// 加载即上报：区分 main.js 是否加载、各依赖是否就绪
document.title = 'XPerformance | boot:main.js';
const _diag = (m) => { try { window.__TAURI__.core.invoke('diag_log', { message: m }); } catch (e) { document.title = 'XPerformance | DIAG-FAIL: ' + m; } };
_diag('main.js loaded');

const { listen } = window.__TAURI__.event;
const { invoke } = window.__TAURI__.core;
_diag('__TAURI__ ok');

// ---------- 调试接口钩子（docs/DESIGN-gui-debug.md；默认开启，XPERF_GUI_DEBUG=0 关闭） ----------
// 后端经 'xperf-debug' 事件下发 {id, op, params}，结果经 debug_respond 命令回传。
// 注册在一切业务代码之前，消除「请求早于 listener」窗口；server 禁用时不发事件、零开销。
const __xperfDebug = {
  // JSON 安全序列化：undefined→null，循环引用/函数/DOM 元素转占位字符串
  stringify(v) {
    const seen = new WeakSet();
    return JSON.parse(JSON.stringify(v === undefined ? null : v, (k, x) => {
      if (typeof x === 'function') return '[Function ' + (x.name || 'anonymous') + ']';
      if (typeof x === 'bigint') return x.toString();
      if (x instanceof Element) return '[Element <' + x.tagName.toLowerCase() + '>]';
      if (typeof x === 'object' && x !== null) {
        if (seen.has(x)) return '[Circular]';
        seen.add(x);
      }
      return x;
    }));
  },
  // selector 未命中 → not_found（后端映射 404）
  _query(selector) {
    const el = document.querySelector(selector);
    if (!el) throw Object.assign(new Error('selector 未命中: ' + selector), { kind: 'not_found' });
    return el;
  },
  // DOM 子树快照：tag/attrs/text/rect/visible，depth 与节点数双上限防爆
  _domSnapshot(p) {
    const root = this._query(p.selector || 'body');
    const depth = Math.min(Math.max(p.depth ?? 4, 0), 12);
    const maxNodes = Math.min(Math.max(p.max_nodes ?? 400, 1), 2000);
    let count = 0, truncated = false;
    const walk = (el, d) => {
      if (count >= maxNodes) { truncated = true; return null; }
      count++;
      const r = el.getBoundingClientRect();
      const attrs = {};
      for (const a of el.attributes) attrs[a.name] = a.value.length > 120 ? a.value.slice(0, 120) + '…' : a.value;
      let text = '';
      for (const n of el.childNodes) if (n.nodeType === 3) text += n.nodeValue;
      text = text.trim().replace(/\s+/g, ' ');
      const cs = getComputedStyle(el);
      const node = {
        tag: el.tagName.toLowerCase(),
        attrs,
        text: text ? (text.length > 80 ? text.slice(0, 80) + '…' : text) : undefined,
        rect: [r.x, r.y, r.width, r.height].map(v => Math.round(v * 10) / 10),
        visible: !!(r.width || r.height) && cs.visibility !== 'hidden' && cs.display !== 'none',
      };
      if (d < depth) {
        const kids = [];
        for (const c of el.children) {
          const k = walk(c, d + 1);
          if (k) kids.push(k); else { truncated = true; break; }
        }
        if (kids.length) node.children = kids;
      } else if (el.children.length) {
        node.childCount = el.children.length;
      }
      return node;
    };
    const tree = walk(root, 0);
    return { tree, nodes: count, truncated };
  },
  // 设备会话取（serial 省略时取当前激活页；不存在 → not_found）
  _session(serial) {
    const s = app.sessions.get(serial || app.active);
    if (!s) throw Object.assign(new Error('设备会话不存在: ' + (serial || '(无激活)')), { kind: 'not_found' });
    return s;
  },
  // 单图表序列摘要（点数/首末时间/末值/全序列 min-max）
  _seriesSummary(pts) {
    if (!pts.length) return { count: 0 };
    let min = Infinity, max = -Infinity;
    for (const pt of pts) { if (pt.v < min) min = pt.v; if (pt.v > max) max = pt.v; }
    return { count: pts.length, first_t: pts[0].t, last: pts[pts.length - 1], min, max };
  },
  // 监控状态快照（无 serial 时只给 app 级概况）
  _stateSnapshot(p) {
    const out = {
      activeSerial: app.active || null,
      devices: [...app.sessions.keys()],
      statusBar: (document.getElementById('status') || {}).textContent || null,
    };
    if (!p.serial && !app.active) return out;
    const s = this._session(p.serial);
    const charts = {};
    for (const [name, ch] of Object.entries(s.charts)) {
      const series = {};
      for (const [k, pts] of Object.entries(ch.series)) series[k] = this._seriesSummary(pts);
      charts[name] = {
        series,
        hover: ch._hoverRows
          ? { t: ch._hoverT, rows: ch._hoverRows.map(r => ({ name: r.name, v: r.v })) }
          : null,
      };
    }
    out.session = {
      serial: s.serial,
      offline: s.offline,
      samplingRunning: s.samplingRunning,
      statusText: s.statusText,
      statusProgress: s.statusProgress,
      activeTab: s.activeTab,
      logcatRunning: s.logcatRunning,
      mirrorRunning: s.mirrorRunning,
      recording: s.recording,
      agentBuilding: s.agentBuilding,
      rooted: s.rooted,
      liveData: s.liveData,
      peaks: s.peaks,
      coldStarts: s.coldStarts,
      charts,
      // logcat_tail=0 须返回 0 行——slice(-0) 等价 slice(0) 会返回全量，须特判
      logcatTail: (() => { const n = Math.min(Math.max(p.logcat_tail ?? 50, 0), 2000); return n === 0 ? [] : s.logcatBuf.slice(-n); })(),
    };
    return out;
  },
  // 单图表全分辨率读数：tail=n 取尾 n 点；at=<epoch_ms> 二分最近点（悬停同口径）
  _seriesData(p) {
    const s = this._session(p.serial);
    // hasOwn 防原型链穿透（metric=constructor 之类会命中 Object 原型属性而非图表）
    if (!p.metric || !Object.hasOwn(s.charts, p.metric)) {
      throw Object.assign(new Error('未知图表: ' + p.metric + '（可选 ' + Object.keys(s.charts).join(',') + '）'), { kind: 'not_found' });
    }
    const ch = s.charts[p.metric];
    const tail = Math.min(Math.max(p.tail ?? 100, 1), 1000);
    const at = p.at != null ? Number(p.at) : null;
    const out = {};
    for (const [name, pts] of Object.entries(ch.series)) {
      if (at != null) {
        let lo = 0, hi = pts.length;
        while (lo < hi) { const mid = (lo + hi) >> 1; if (pts[mid].t < at) lo = mid + 1; else hi = mid; }
        let best = lo < pts.length ? lo : -1;
        if (lo > 0 && (best < 0 || at - pts[lo - 1].t <= pts[lo].t - at)) best = lo - 1;
        out[name] = best >= 0 ? { point: pts[best], distance_ms: Math.abs(pts[best].t - at) } : { point: null };
      } else {
        out[name] = pts.slice(-tail);
      }
    }
    return out;
  },
  // 动作目标定位：selector 优先，否则 x,y 经 elementFromPoint（都没有 → bad_params）
  _target(p) {
    if (p.selector) return this._query(p.selector);
    if (p.x != null && p.y != null) {
      const el = document.elementFromPoint(p.x, p.y);
      if (!el) throw Object.assign(new Error('坐标处无元素: ' + p.x + ',' + p.y), { kind: 'not_found' });
      return el;
    }
    throw Object.assign(new Error('须给 selector 或 x,y'), { kind: 'bad_params' });
  },
  // 事件坐标：显式 x,y（视口坐标）优先，否则元素中心
  _point(p, el) {
    if (p.x != null && p.y != null) return { x: p.x, y: p.y };
    const r = el.getBoundingClientRect();
    return { x: r.x + r.width / 2, y: r.y + r.height / 2 };
  },
  _mouse(el, types, pt) {
    for (const t of types) {
      el.dispatchEvent(new MouseEvent(t, { bubbles: true, cancelable: true, clientX: pt.x, clientY: pt.y, button: 0, view: window }));
    }
  },
  // 元素简述（操作回执，确认作用对象）
  _describe(el) {
    let text = '';
    for (const n of el.childNodes) if (n.nodeType === 3) text += n.nodeValue;
    return { tag: el.tagName.toLowerCase(), id: el.id || undefined, class: el.className || undefined, text: text.trim().replace(/\s+/g, ' ').slice(0, 60) || undefined };
  },
  // 操作注入（真实 DOM 事件序列；全在主线程同步完成）
  _action(p) {
    if (typeof p.op !== 'string' || !p.op) throw Object.assign(new Error('op 必填'), { kind: 'bad_params' });
    const el = this._target(p);
    const pt = this._point(p, el);
    switch (p.op) {
      case 'click':
        this._mouse(el, ['mousedown', 'mouseup', 'click'], pt);
        return { acted: this._describe(el) };
      case 'hover':
        this._mouse(el, ['mouseover', 'mouseenter', 'mousemove'], pt);
        return { acted: this._describe(el), at: pt };
      case 'input': {
        if (!(el instanceof HTMLInputElement) && !(el instanceof HTMLTextAreaElement)) {
          throw Object.assign(new Error('目标不是 input/textarea'), { kind: 'bad_params' });
        }
        el.focus();
        // 原生 setter 写值（绕过潜在的框架 value 拦截），随后显式发 input+change
        // （AX 教训：change 常随 blur 派发、合成事件不触发——这里显式全发）
        const proto = el instanceof HTMLInputElement ? HTMLInputElement.prototype : HTMLTextAreaElement.prototype;
        Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, String(p.value ?? ''));
        el.dispatchEvent(new Event('input', { bubbles: true }));
        el.dispatchEvent(new Event('change', { bubbles: true }));
        return { acted: this._describe(el), value: el.value };
      }
      case 'select': {
        if (!(el instanceof HTMLSelectElement)) throw Object.assign(new Error('目标不是 select'), { kind: 'bad_params' });
        const v = String(p.value ?? '');
        const ok = [...el.options].some(o => o.value === v);
        if (!ok) throw Object.assign(new Error('无此选项: ' + v + '（可选 ' + [...el.options].map(o => o.value).join(',') + '）'), { kind: 'bad_params' });
        el.value = v;
        el.dispatchEvent(new Event('change', { bubbles: true }));
        return { acted: this._describe(el), value: el.value };
      }
      case 'check': {
        if (!(el instanceof HTMLInputElement) || (el.type !== 'checkbox' && el.type !== 'radio')) {
          throw Object.assign(new Error('目标不是 checkbox/radio'), { kind: 'bad_params' });
        }
        el.checked = p.value !== false && p.value !== 'false';
        el.dispatchEvent(new Event('change', { bubbles: true }));
        return { acted: this._describe(el), checked: el.checked };
      }
      case 'scroll':
        // 滚动量用 scrollX/scrollY——与定位坐标 x/y 解耦（x/y 在 _target/_point 是
        // 视口坐标语义，混用会让「selector 缺省 + 给了 x,y」被误当定位坐标）
        if (p.scrollY != null) el.scrollTop = p.scrollY;
        if (p.scrollX != null) el.scrollLeft = p.scrollX;
        if (p.scrollY == null && p.scrollX == null) {
          throw Object.assign(new Error('scroll 须给 scrollX/scrollY（滚动偏移，与定位坐标 x/y 无关）'), { kind: 'bad_params' });
        }
        el.dispatchEvent(new Event('scroll', { bubbles: true }));
        return { acted: this._describe(el), scrollTop: el.scrollTop, scrollLeft: el.scrollLeft };
      case 'key': {
        const target = p.selector ? el : (document.activeElement || el);
        const init = { key: String(p.key || ''), bubbles: true, cancelable: true, ctrlKey: !!p.ctrl, shiftKey: !!p.shift, altKey: !!p.alt, metaKey: !!p.meta };
        if (!init.key) throw Object.assign(new Error('key 必填（如 Enter/Tab/a）'), { kind: 'bad_params' });
        target.dispatchEvent(new KeyboardEvent('keydown', init));
        target.dispatchEvent(new KeyboardEvent('keyup', init));
        return { acted: this._describe(target), key: init.key };
      }
      default:
        throw Object.assign(new Error('未知 op: ' + p.op + '（可选 click/input/select/check/hover/scroll/key）'), { kind: 'bad_params' });
    }
  },
  async exec(op, p) {
    if (op === 'ping') return 'pong';
    if (op === 'action') return this._action(p);
    if (op === 'dom') return this._domSnapshot(p);
    if (op === 'state') return this._stateSnapshot(p);
    if (op === 'series') return this._seriesData(p);
    if (op === 'eval') {
      if (typeof p.expr !== 'string' || !p.expr.trim()) {
        throw Object.assign(new Error('expr 必填'), { kind: 'bad_params' });
      }
      // direct eval：表达式可访问本脚本顶层作用域（app/DeviceSession 等）
      let v = eval(p.expr);
      if (v && typeof v.then === 'function') v = await v; // async 表达式自动 await
      return this.stringify(v);
    }
    throw Object.assign(new Error('未知 op: ' + op), { kind: 'bad_params' });
  },
};
listen('xperf-debug', async (e) => {
  const { id, op, params } = e.payload || {};
  let result;
  try {
    result = { ok: true, data: await __xperfDebug.exec(op, params || {}) };
  } catch (err) {
    result = { ok: false, error: String(err && err.message ? err.message : err), kind: (err && err.kind) || 'js_error' };
  }
  try { await invoke('debug_respond', { id, result }); } catch (_) { /* server 已禁用时丢弃 */ }
});
invoke('debug_frontend_ready').catch(() => {});

// ---------- 轻量 Canvas 折线图（替代 ECharts，无外部依赖） ----------
// 主题色从 CSS 变量动态读取（data-theme 切换后 draw/redraw 自动跟随）。
// 按主题缓存：getComputedStyle 每次 draw 调用太贵（绘制频率可达每秒数百次），
// 主题一天才切几次——applyTheme 换 dataset.theme 后缓存自然失效。
let _colorCache = null;
let _colorTheme = '';
function uiColors() {
  const theme = document.documentElement.dataset.theme || '';
  if (_colorCache && theme === _colorTheme) return _colorCache;
  const cs = getComputedStyle(document.documentElement);
  const v = (n) => cs.getPropertyValue(n).trim();
  const light = theme === 'light';
  _colorCache = {
    bg: v('--bg-card'),
    text: v('--text'),
    dim: v('--text-dim'),
    grid: v('--bg-input'),
    border: v('--border'),
    err: v('--err'),
    // series 色板：暗色 mocha / 亮色 latte
    series: light
      ? ['#1e66f5', '#40a02b', '#df8e1d', '#d20f39', '#8839ef', '#179299', '#fe640b', '#209fb5', '#ea76cb', '#7c7f93']
      : ['#89b4fa', '#a6e3a1', '#f9e2af', '#f38ba8', '#cba6f7', '#94e2d5', '#fab387', '#74c7ec', '#f5c2e7', '#a6adc8'],
  };
  _colorTheme = theme;
  return _colorCache;
}

// ---------- 内存序列抽稀（与 CLI CHART_SERIES_CAP 同口径） ----------
// 超过 2×CAP 时每 2 取 1 原地抽稀：保完整时间范围，分辨率随会话时长降级；
// 内存封顶 ~30k 点/series（全分辨率全量数据在流式 CSV 落盘里，不受影响）。
const SERIES_CAP = 30000;
// logcat 视图行数上限（超出从头部修剪；全量数据在落盘 logcat.log，视图仅供实时查看）
const LOGCAT_VIEW_CAP = 2000;
function pushCapped(arr, item) {
  if (arr.length >= 2 * SERIES_CAP) {
    let w = 0;
    for (let r = 0; r < arr.length; r += 2) arr[w++] = arr[r];
    arr.length = w;
  }
  arr.push(item);
}

// 悬停读数浮层用 innerHTML 拼装；序列名可能来自设备 dumpsys（图层名），须转义
function escHtml(s) { return s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c])); }
// 模态警告对话框（原生 tauri-plugin-dialog）：占用焦点，关闭前阻塞用户其他操作——
// 用于表单校验类警告/失败（替代顶栏状态栏文本——非阻塞且易被后续事件覆盖）
const alertBox = (msg, kind = 'warning') => window.__TAURI__.dialog.message(msg, { title: 'XPerformance', kind });
// 读数格式化：最多两位小数去尾零（值在 push 时已按指标精度取整，这里只兜住 mem/freq 的原始浮点）
function fmtChartVal(v) { return String(Number(v.toFixed(2))); }

// Y 轴自适应上限取「好看」档位（1/1.5/2/2.5/3/4/5/7.5×10^n 向上）：档位吸附让
// 刻度标签在稳态下保持恒定文本，避免每秒产生新字符串（WebKit 唯一文本串驻留）
function niceCeil(v) {
  if (!isFinite(v) || v <= 0) return 1;
  const e = Math.floor(Math.log10(v));
  const f = v / 10 ** e;
  const STEPS = [1, 1.5, 2, 2.5, 3, 4, 5, 7.5, 10];
  const nf = STEPS.find((s) => f <= s) ?? 10;
  return nf * 10 ** e;
}

// X 轴时间刻度步长取时间档位（1s/2s/5s/10s/15s/30s/1/2/5/10/15/30min 向上）：
// 与 tick 绝对对齐配合，窗口滚动或增长阶段标签文本都只在跨档时变化
function niceTimeStep(ms) {
  const STEPS = [1000, 2000, 5000, 10000, 15000, 30000,
    60000, 120000, 300000, 600000, 900000, 1800000];
  return STEPS.find((s) => ms <= s) ?? 3600000;
}

class LineChart {
  // canvas 直接传元素引用（多设备页并存，不再用全局 id 查找）
  constructor(canvas, title, unit, maxValue) {
    this.canvas = canvas;
    this.ctx = canvas.getContext('2d');
    this.title = title;
    this.unit = unit;
    this.maxValue = maxValue; // Y 轴下限（如 CPU=100），实际值超出时自动扩展；undefined = 自适应
    this.series = {}; // pid -> [{t, v}]，完整会话历史（回看用），绘制时按窗口裁剪+抽稀
    this.seriesColor = {}; // pid -> 色板序号（创建时分配，终生不变；绘色时 % 色板长度解析，主题切换自动跟随）
    this._colorSeq = 0;
    this.windowMode = 'follow'; // follow=最近 followMs；all=全部历史
    this.followMs = 10 * 60 * 1000;
    this.dirty = false; // 有待绘制数据（requestDraw 置位，flushCharts 统一绘制）
    this.plot = null;   // 最近一次 draw 的绘图区几何（悬停读数用；无数据时为 null）
    this.hoverX = null; // 悬停中的 canvas 内 x（CSS px）；draw 完成后按新几何刷新读数
    this._hoverPX = 0;  // rAF 合帧的待处理 x
    this._hoverRaf = 0; // 已预约的 rAF id（0=无）
    this._tipHtml = ''; // 浮层内容签名：不变则跳过 innerHTML 重写（避免强制同步布局）
    // 悬停读数 overlay（DOM 实现：采样中 canvas 每 150ms 重绘，画在 canvas 上的游标会被抹掉）
    this.hoverLine = document.createElement('div');
    this.hoverLine.className = 'chart-hover-line';
    this.hoverTip = document.createElement('div');
    this.hoverTip.className = 'chart-hover-tip';
    const box = canvas.parentElement; // .chart 容器（position:relative）
    box.appendChild(this.hoverLine);
    box.appendChild(this.hoverTip);
    canvas.addEventListener('mousemove', (e) => this.onHover(e));
    canvas.addEventListener('mouseleave', () => this.hideHover());
    this.resize();
  }
  // 数据到达不立即绘制：标脏即可，由全局 150ms 合帧器统一绘制（仅激活设备页）。
  // 非激活页持续累积数据但不绘制——切回时 resize() 会即时补画。
  requestDraw() { this.dirty = true; }
  resize() {
    // 高分屏（devicePixelRatio>1）下需放大 canvas 缓冲区，否则字体和线条模糊。
    // ctx 用 setTransform(dpr,0,0,dpr,0,0)，后续 draw 坐标按 CSS 像素书写即可。
    const r = this.canvas.parentElement.getBoundingClientRect();
    const dpr = window.devicePixelRatio || 1;
    this.cssW = r.width;
    this.cssH = r.height;
    this.canvas.width = Math.max(1, Math.round(r.width * dpr));
    this.canvas.height = Math.max(1, Math.round(r.height * dpr));
    this.ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    this.dirty = false;
    this.draw();
  }
  push(pid, t, v) {
    if (!this.series[pid]) { this.series[pid] = []; this.seriesColor[pid] = this._colorSeq++; }
    pushCapped(this.series[pid], { t, v });
  }
  draw() {
    const { ctx } = this;
    const C = uiColors();
    const W = this.cssW, H = this.cssH;
    const L = 70, R = 20, T = 30, B = 30;
    ctx.clearRect(0, 0, W, H);
    // 背景
    ctx.fillStyle = C.bg;
    ctx.fillRect(0, 0, W, H);
    // 标题
    ctx.fillStyle = C.text;
    ctx.font = `500 ${14 * fontScale()}px system-ui, sans-serif`;
    ctx.fillText(this.title, 12, 20);
    // 计算范围（tMax 永远取全量最新；tMin 按窗口模式：follow=最新往前 followMs，all=全量最早）
    let tMinAll = Infinity, tMax = -Infinity;
    const pids = Object.keys(this.series);
    for (const pid of pids) {
      const pts = this.series[pid];
      if (pts.length === 0) continue;
      if (pts[0].t < tMinAll) tMinAll = pts[0].t;
      const lastT = pts[pts.length - 1].t;
      if (lastT > tMax) tMax = lastT;
    }
    if (pids.length === 0 || !isFinite(tMax)) { this.plot = null; this.hideHover(); this.drawAxes(L, T, W - R, H - B); return; }
    let tMin = this.windowMode === 'all' ? tMinAll : Math.max(tMinAll, tMax - this.followMs);
    if (tMax - tMin < 1000) tMax = tMin + 1000;
    // 每 series 预计算窗口起点（时间有序二分）——vMax 扫描与绘制共用，
    // 窗口外前缀不再重复扫（长会话 'all' 模式下避免每次绘制 O(全量) 前缀比较）
    const rows = [];
    for (const pid of pids) {
      const all = this.series[pid];
      if (all.length === 0) continue;
      let lo = 0, hi = all.length;
      while (lo < hi) { const mid = (lo + hi) >> 1; if (all[mid].t < tMin) lo = mid + 1; else hi = mid; }
      if (lo < all.length) rows.push({ pid, all, lo });
    }
    // 可见窗口内的 vMax（Y 轴）
    let vMax = 0;
    for (const { all, lo } of rows) {
      for (let j = lo; j < all.length; j++) {
        const v = all[j].v;
        if (v > vMax) vMax = v;
      }
    }
    // Y 轴范围：固定下限（CPU=100，超出自动扩展）或自适应（内存，基于实际最大值）。
    // 自适应值吸附 1-2-2.5-5 档位：连续浮点 yMax 每秒微变会让刻度标签每秒产生
    // 新文本串——WebKit 对渲染/测量过的唯一字符串有无界驻留（2026-09-22 压测
    // 定位：fillText ~74KB/串、measureText ~6KB/串），吸附后稳态标签恒定
    let yMax;
    if (this.maxValue !== undefined) {
      yMax = Math.max(this.maxValue, niceCeil(vMax * 1.05));
    } else {
      yMax = niceCeil(Math.max(1, vMax) * 1.05);
    }
    // 刻度标签精度：按 yMax 大小选位数，避免 toFixed(0) 四舍五入导致重复
    const labelPrec = yMax >= 100 ? 0 : (yMax >= 10 ? 1 : 2);
    // 网格 + Y 轴刻度
    ctx.strokeStyle = C.grid;
    ctx.fillStyle = C.dim;
    ctx.font = `${12 * fontScale()}px system-ui, sans-serif`;
    for (let i = 0; i <= 4; i++) {
      const y = T + (H - B - T) * i / 4;
      ctx.beginPath(); ctx.moveTo(L, y); ctx.lineTo(W - R, y); ctx.stroke();
      ctx.fillText((yMax * (1 - i / 4)).toFixed(labelPrec), 8, y + 4);
    }
    // X 轴时间刻度：tick 落在 step 的绝对时间整数倍上（step 取 1s/2s/5s/…/30min
    // 时间档位）——窗口滚动/增长时标签串只在 step 跳档或跨整数倍时变化，而非每帧
    // 新串（WebKit 对渲染/测量过的唯一字符串有无界驻留，2026-09-22 压测定位；
    // 此前只对齐 5s 的做法在窗口未满 10min 时 span 每秒增长、tickStep 随动，
    // 标签仍逐秒 churn——对齐 tick 值本身才完整覆盖两阶段）
    const span = tMax - tMin;
    const tickStep = niceTimeStep(span / 4);
    const t0 = Math.ceil(tMin / tickStep) * tickStep;
    for (let tv = t0; tv <= tMax; tv += tickStep) {
      const x = L + (W - R - L) * (tv - tMin) / span;
      const ts = new Date(tv);
      ctx.fillText(ts.toTimeString().slice(0, 8), x - 22, H - 10);
    }
    this.drawAxes(L, T, W - R, H - B);
    // 折线：窗口外点跳过（起点二分已预算）；可见点超过 2×像素宽时按 stride 抽稀
    let legendX = W - R;
    const plotW = W - R - L;
    rows.forEach(({ pid, all, lo }) => {
      const visLen = all.length - lo;
      const stride = Math.max(1, Math.ceil(visLen / (plotW * 2)));
      // 颜色按序列创建时分配的稳定序号取（而非可见行下标）——序列滑出窗口后其余折线不再整体变色，
      // 悬停 tooltip 色点也查同一映射，与折线恒一致
      const color = C.series[this.seriesColor[pid] % C.series.length];
      ctx.strokeStyle = color;
      ctx.lineWidth = 2;
      ctx.beginPath();
      // 从窗口前一个点开始画，保证折线在左边界处连续（不截断出缺口）
      if (lo > 0) {
        ctx.moveTo(L, T + (H - B - T) * (1 - all[lo - 1].v / yMax));
      } else {
        ctx.moveTo(L + plotW * (all[0].t - tMin) / span, T + (H - B - T) * (1 - all[0].v / yMax));
      }
      for (let j = lo; j < all.length; j += stride) {
        const p = all[j];
        const x = L + plotW * (p.t - tMin) / span;
        const y = T + (H - B - T) * (1 - p.v / yMax);
        ctx.lineTo(x, y);
      }
      // 末点必画（stride 抽稀可能跳过最新点）
      const lp = all[all.length - 1];
      ctx.lineTo(L + plotW * (lp.t - tMin) / span, T + (H - B - T) * (1 - lp.v / yMax));
      ctx.stroke();
      // 图例（key 即展示名：CPU/内存用 "PID xxx"，FPS 用图层短名）；
      // 从右往左按文字实际宽度排布，长图层名不截断不重叠
      ctx.fillStyle = color;
      legendX -= ctx.measureText(pid).width;
      ctx.fillText(pid, legendX, 20);
      legendX -= 14;
      ctx.fillRect(legendX, 11, 10, 10);
      legendX -= 10;
    });
    // 记录绘图区几何供悬停读数；悬停中则用新几何刷新（follow 模式 tMax 随采样滚动）
    this.plot = { L, T, R, B, tMin, tMax, span };
    if (this.hoverX !== null) this.showHover(this.hoverX);
  }
  drawAxes(l, t, r, b) {
    const { ctx } = this;
    ctx.strokeStyle = uiColors().border;
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.moveTo(l, t); ctx.lineTo(l, b); ctx.lineTo(r, b);
    ctx.stroke();
  }
  // ---- 悬停准确读数：竖线 + 浮层列出该时刻各序列的值 ----
  // 取数与绘制无关（不受绘制 stride 抽稀影响）：按 hovered 时间在完整序列上二分最近点
  onHover(e) {
    const r = this.canvas.getBoundingClientRect();
    this._hoverPX = e.clientX - r.left;
    // rAF 合帧：高轮询率鼠标下每帧最多一次取数与 DOM 更新（原实现每事件重建 innerHTML + 读布局尺寸）
    if (!this._hoverRaf) {
      this._hoverRaf = requestAnimationFrame(() => {
        this._hoverRaf = 0;
        this.showHover(this._hoverPX);
      });
    }
  }
  showHover(x) {
    const p = this.plot;
    // 图表尚未完成首帧绘制（plot 未建立，如约 150ms 合帧前/首批数据未到）：
    // 记住悬停位置只隐视觉——draw() 建立 plot 后会用 hoverX 重刷补上读数；
    // 若直接 hideHover 清 hoverX，这次悬停就永久丢失（自动化单次 hover 必踩）
    if (!p) {
      this.hoverX = x;
      this._hoverRows = null;
      if (this.hoverLine) this.hoverLine.style.display = 'none';
      if (this.hoverTip) this.hoverTip.style.display = 'none';
      return;
    }
    const W = this.cssW, H = this.cssH;
    const plotW = W - p.R - p.L;
    if (x < p.L || x > W - p.R || plotW <= 0) return this.hideHover();
    this.hoverX = x;
    const t = p.tMin + (x - p.L) / plotW * p.span;
    const C = uiColors();
    const rows = [];
    const keys = Object.keys(this.series);
    for (let i = 0; i < keys.length; i++) {
      const all = this.series[keys[i]];
      if (!all || all.length === 0) continue;
      // 可见范围与 draw 同口径：窗口起点前一点也参与取数（draw 用它保持折线左边界连续，
      // 图上有线就应有读数，左缘不再缺行）；整条序列在窗口前结束（已停止的 PID）图上无线，不读数
      let lo0 = 0, hi0 = all.length;
      while (lo0 < hi0) { const mid = (lo0 + hi0) >> 1; if (all[mid].t < p.tMin) lo0 = mid + 1; else hi0 = mid; }
      if (lo0 >= all.length) continue;
      // 可见范围 [start, len) 内二分 hovered t 的最近点
      const start = lo0 > 0 ? lo0 - 1 : 0;
      let lo = start, hi = all.length;
      while (lo < hi) { const mid = (lo + hi) >> 1; if (all[mid].t < t) lo = mid + 1; else hi = mid; }
      let bestIdx = lo < all.length ? lo : -1;
      if (lo > start && (bestIdx < 0 || t - all[lo - 1].t <= all[lo].t - t)) bestIdx = lo - 1;
      if (bestIdx < 0) continue;
      rows.push({ name: keys[i], color: C.series[this.seriesColor[keys[i]] % C.series.length], v: all[bestIdx].v });
    }
    if (rows.length === 0) return this.hideHover();
    // 悬停内容结构化留存：调试接口（__xperfDebug state）直读，免解析 hoverTip HTML
    this._hoverRows = rows;
    this._hoverT = t;
    // 竖线
    this.hoverLine.style.display = 'block';
    this.hoverLine.style.left = x + 'px';
    this.hoverLine.style.top = p.T + 'px';
    this.hoverLine.style.height = (H - p.B - p.T) + 'px';
    // 浮层（毫秒精度时间头 + 逐序列色点/名称/值）
    const d = new Date(t);
    const hh = d.toTimeString().slice(0, 8) + '.' + String(d.getMilliseconds()).padStart(3, '0');
    let html = '<div class="ht-time">' + hh + '</div><div class="ht-rows">';
    for (const row of rows) {
      html += '<div class="ht-row"><span class="ht-chip" style="background:' + row.color + '"></span><span class="ht-name">'
        + escHtml(row.name) + '</span><span class="ht-val">' + fmtChartVal(row.v) + ' ' + escHtml(this.unit) + '</span></div>';
    }
    html += '</div>';
    // 内容签名不变则不重写 innerHTML、不动布局类（采样中 draw 每 150ms 刷新本浮层，
    // 值多数时候未变；重写会让紧随其后的 offsetWidth/offsetHeight 读取强制同步布局）
    if (html !== this._tipHtml) {
      this._tipHtml = html;
      this.hoverTip.innerHTML = html;
      // 列数分档（行高约 18px，图框 min-height 120px）：>5 序列（如 8 核频率图）两列、
      // >12（真实设备温度传感器可达 10~30 个）三列，避免浮层高出图框被 overflow 裁剪
      this.hoverTip.classList.toggle('cols2', rows.length > 5 && rows.length <= 12);
      this.hoverTip.classList.toggle('cols3', rows.length > 12);
    }
    this.hoverTip.style.display = 'block';
    // 定位：右侧放不下则翻到光标左边；垂直钳在图框内
    const tipW = this.hoverTip.offsetWidth, tipH = this.hoverTip.offsetHeight;
    this.hoverTip.style.left = (x + 14 + tipW <= W - 4 ? x + 14 : Math.max(4, x - 14 - tipW)) + 'px';
    this.hoverTip.style.top = Math.max(4, Math.min(p.T + 4, H - tipH - 4)) + 'px';
  }
  hideHover() {
    this.hoverX = null;
    this._hoverRows = null;
    this._hoverT = null;
    // 取消未消费的合帧回调，防止 mouseleave 后 rAF 又把浮层显示出来
    if (this._hoverRaf) { cancelAnimationFrame(this._hoverRaf); this._hoverRaf = 0; }
    if (this.hoverLine) this.hoverLine.style.display = 'none';
    if (this.hoverTip) this.hoverTip.style.display = 'none';
  }
}

// ---------- 单设备会话页：侧栏控制 + 三子 tab + 全部指标数据 ----------
// 每台在线设备一个实例，事件按 payload.serial 分发到这里；
// 数据（图表 series/峰值/线程/时序 hist）全部设备内隔离。
class DeviceSession {
  constructor(serial, info) {
    this.serial = serial;
    this.info = info || { model: '', version: '' };
    this.samplingRunning = false;
    this.agentBuilding = false; // 开发运行 agent 构建中（agent-build 事件/agent_building 回查驱动）
    this.offline = false;
    this.eventCount = 0;
    this.statusText = '未开始';
    this.statusProgress = null; // null=普通文案；数字=进度百分比
    this.currentTracePath = null;
    this.currentStackPath = null;
    this.restartTimer = null;
    this.mirrorRunning = false;
    this.recording = false;
    // logcat 抓取（「日志」tab）：running=抓取中；paused=视图暂停滚动（不落盘）；
    // buf=行数据 ring buffer（LOGCAT_VIEW_CAP 上限）；dirty=非可见期间有未渲染行
    //（设备页/日志 tab 非激活时不碰 DOM——WebKit 不可见 DOM 变更仍要维护渲染/AX 树，
    // 实测全机洪泛 ~190 行/s 时白耗 ~13% 单核；切回时一次性重放）
    this.logcatRunning = false;
    this.logcatPaused = false;
    this.logcatBuf = [];
    this.logcatDirty = false;
    this.activeTab = 'perf'; // 当前激活子 tab（switchTab 更新；渲染节流判定用）
    // 同型号多机并存时 tab 标签附 serial 尾 4 位消歧（App.refreshTabLabels 维护）
    this.dupModel = false;

    // DOM：克隆设备页模板并挂载
    const tpl = document.getElementById('devicePageTpl');
    this.root = tpl.content.querySelector('.device-page').cloneNode(true);
    this.root.dataset.serial = serial;
    document.getElementById('content').appendChild(this.root);
    // datalist 用 id 关联 input——多实例并存须每设备唯一 id
    const dlId = 'pkgList-' + serial;
    this.root.querySelector('.package-input').setAttribute('list', dlId);
    this.root.querySelector('.pkg-list').id = dlId;
    // 按平台禁用无数据源的指标项（info.platform 由 devices payload 提供）：
    // SS2MAX 无 GPU 显存源（dumpsys gpu 无 Memory snapshot，2026-09-07 root 确证）
    this.applyPlatformCaps();

    // 顶栏设备 tab
    this.tabBtn = document.createElement('button');
    this.tabBtn.className = 'device-tab';
    this.tabBtn.textContent = this.tabLabel();
    this.tabBtn.title = this.tabTitle();
    this.tabBtn.addEventListener('click', () => app.switchDevice(serial));
    document.getElementById('deviceTabs').appendChild(this.tabBtn);

    // 9 张折线图
    const chart = (cls, title, unit, maxValue) =>
      new LineChart(this.root.querySelector('canvas.' + cls), title, unit, maxValue);
    this.charts = {
      cpu: chart('chart-cpu', 'Process CPU (%)', '%', 100),
      mem: chart('chart-mem', 'Memory Total PSS (MB)', 'MB'),
      fps: chart('chart-fps', 'FPS', 'fps'), // 自适应纵轴（30/60/120 档各异）
      freq: chart('chart-freq', 'CPU Frequency (MHz)', 'MHz'), // 每核一条线
      temp: chart('chart-temp', 'Temperature (°C)', '°C'), // 每传感器一条线；thermal status 写入标题
      gpu: chart('chart-gpu', 'GPU Busy (%)', '%', 100),
      gpumem: chart('chart-gpumem', 'GPU Memory (MB)', 'MB'), // --gpu 降级路径（hypervisor 平台）：每 PID 一条
      io: chart('chart-io', 'IO (KB/s)', 'KB/s'), // 每 PID 读/写各一条
      net: chart('chart-net', 'Network 整机 (KB/s)', 'KB/s'),
    };
    this.allCharts = Object.values(this.charts);

    // 会话数据容器（全部 per-device）
    this.pidData = {};      // pid -> { cpu, mem, new, stopped }
    this.peaks = {};        // pid -> { cpu: {v,t}, mem: {v,t} }，仅新峰值时更新 DOM
    this.latestThreads = {}; // pid -> 最新一轮线程数组（500ms 节流渲染）
    this.fpsHist = {};      // 图层短名 -> [{t, fps, jank}]
    this.tempHist = {};     // 传感器名 -> [{t, v, status}]
    this.ioHist = {};       // pid -> [{t, r, w, dr, dw}]
    this.gpuHist = [];      // [{t, busy, util, mhz}]
    this.gpumemHist = {};   // pid -> [{t, mb, gmb}]
    this.gpuprocHist = {};  // pid -> [{t, busy}]
    this.maxkhz = [];       // AgentHello 带的每核最大频率（KHz）
    this.rooted = null;     // AgentHello 带的权限身份：true=root / false=shell / null=未知（未握手）
    this._lastAgentErr = ''; // agent err 状态栏去重（防同文案反复刷）
    this.liveData = {};     // 实时数值面板数据
    this.coldStarts = [];   // 冷启动记录 [{time, action, total, wait}]（最近 5 次）
    this.liveDirty = false;    // liveData 有未渲染变更（500ms 面板渲染器据此跳过）
    this.threadsDirty = false; // latestThreads 有未渲染变更

    this.bindEvents();
    this.toggleCharts();
    this.updateEffectiveRates();
    this.renderColdStarts();
  }

  // ---- DOM 查找（设备页 root 内） ----
  el(cls) { return this.root.querySelector('.' + cls); }
  els(cls) { return this.root.querySelectorAll('.' + cls); }
  // 平台能力：禁用对应指标 checkbox（强制不勾选 + 提示原因）。
  // info 更新（断开重连 onDevicesChanged / addDevice）后重跑以覆盖热插拔场景
  applyPlatformCaps() {
    const box = this.metricBox('gpumem');
    const noGpuMem = (this.info && this.info.platform === 'ss2max');
    box.disabled = noGpuMem;
    box.parentElement.title = noGpuMem ? 'SS2MAX 平台无 GPU 显存数据源' : '';
    if (noGpuMem) box.checked = false;
  }
  metricBox(id) { return this.el('metric-' + id); }
  package() { return this.el('package-input').value.trim(); }

  tabLabel() {
    let name = this.info.model || this.serial;
    // 同型号多机并存（如两台 SS3）：附 serial 尾 4 位消歧，serial 本体在 title 悬浮
    if (this.dupModel && this.info.model) name += '·' + this.serial.slice(-4);
    return this.offline ? name + '（已断开）' : name;
  }
  tabTitle() {
    const extra = [this.info.model, this.info.version ? 'Android ' + this.info.version : '']
      .filter(Boolean).join('，');
    return (extra ? this.serial + '（' + extra + '）' : this.serial)
      + (this.offline ? ' — 已断开，插回自动恢复采样' : '');
  }

  // ---- 设备权限徽章 + IO 灰显（AgentHello.root / 获取 root 按钮成功后更新） ----
  updatePermBadge() {
    const badge = this.el('perm-badge');
    const btn = this.el('root-btn');
    const ioBox = this.metricBox('io');
    if (this.rooted === true) {
      badge.textContent = 'root';
      badge.className = 'perm-badge perm-ok';
      btn.disabled = true;
      ioBox.disabled = false;
      ioBox.closest('label').title = '';
    } else if (this.rooted === false) {
      badge.textContent = 'shell（非 root）';
      badge.className = 'perm-badge perm-warn';
      btn.disabled = false;
      // IO 数据源 /proc/<pid>/io 仅 owner/root 可读（非 root 实测拒读，无兜底）——
      // 灰显防误勾（agent 侧也会 err 拒绝，双保险）；已勾选则取消（disabled 的
      // checked 仍会进 start 参数）
      if (ioBox.checked) { ioBox.checked = false; this.toggleCharts(); }
      ioBox.disabled = true;
      ioBox.closest('label').title = '需 root 权限（/proc/<pid>/io 仅 owner/root 可读）';
    } else {
      badge.textContent = '未知';
      badge.className = 'perm-badge';
      btn.disabled = false;
      ioBox.disabled = false;
      ioBox.closest('label').title = '';
    }
  }

  // ---- 获取 root（adb root 重启 adbd；采样中会话走重连恢复，新 hello 更新徽章） ----
  async acquireRoot() {
    const btn = this.el('root-btn');
    btn.disabled = true;
    this.setStatus('正在获取 root（adb root 重启 adbd，设备短暂离线）…');
    _diag('[' + this.serial + '] rootBtn: acquire_root');
    try {
      const msg = await invoke('acquire_root', { serial: this.serial });
      this.rooted = true; // adbd 已确认 uid=0；daemon 重建后 hello 会再次确认
      this.updatePermBadge();
      this.setStatus(msg + '（下次采样/重连后全指标生效）');
      _diag('[' + this.serial + '] rootBtn: OK');
    } catch (e) {
      await alertBox('获取 root 失败: ' + (e && e.message ? e.message : e), 'error');
      _diag('[' + this.serial + '] rootBtn ERROR: ' + (e && e.message ? e.message : JSON.stringify(e)));
      this.updatePermBadge(); // 失败回恢复按 rooted 状态决定按钮可用性
    }
  }

  // ---- 屏幕镜像（scrcpy 外部窗口；后端托管进程与 SSH 隧道映射） ----
  async toggleMirror() {
    const btn = this.el('mirror-btn');
    btn.disabled = true;
    try {
      if (!this.mirrorRunning) {
        const msg = await invoke('start_mirror', { serial: this.serial });
        this.mirrorRunning = true;
        btn.textContent = '停止镜像';
        this.setStatus(msg);
        _diag('[' + this.serial + '] mirrorBtn: started');
      } else {
        const msg = await invoke('stop_mirror', { serial: this.serial });
        this.setStatus(msg);
        _diag('[' + this.serial + '] mirrorBtn: stopped');
        // 按钮复位由监护线程的 mirror exited 事件完成（进程真正退出后）
      }
    } catch (e) {
      await alertBox('屏幕镜像失败: ' + (e && e.message ? e.message : e), 'error');
      _diag('[' + this.serial + '] mirrorBtn ERROR: ' + (e && e.message ? e.message : JSON.stringify(e)));
      this.mirrorRunning = false;
      btn.textContent = '屏幕镜像';
    }
    btn.disabled = false;
  }

  // 镜像进程退出事件（stopped=用户停止 / closed=关窗正常退出 / failed=异常退出）：
  // 三态统一复位按钮；注意不能凭 scrcpy stderr 非空判异常（正常运行也有启动日志）
  handleMirrorEvent(stage, message) {
    const btn = this.el('mirror-btn');
    this.mirrorRunning = false;
    btn.textContent = '屏幕镜像';
    btn.disabled = false;
    if (stage === 'stopped') {
      this.setStatus('镜像已停止');
    } else if (stage === 'closed') {
      this.setStatus('镜像窗口已关闭');
    } else if (stage === 'failed') {
      const last = (message || '').split('\n').filter(Boolean).pop() || '未知原因';
      this.setStatus('镜像异常退出: ' + last);
    } else {
      return;
    }
    _diag('[' + this.serial + '] mirror ' + stage + (message ? ': ' + message : ''));
  }

  // ---- 截屏与录屏（core capture/mirror；产物落 /tmp/xperf 会话目录 capture/） ----
  async takeScreenshot() {
    const btn = this.el('shot-btn');
    btn.disabled = true;
    try {
      const pkg = this.el('package-input').value.trim();
      const path = await invoke('take_screenshot', { serial: this.serial, package: pkg });
      this.setStatus('截屏已保存: ' + path);
      _diag('[' + this.serial + '] shot: ' + path);
    } catch (e) {
      await alertBox('截屏失败: ' + (e && e.message ? e.message : e), 'error');
      _diag('[' + this.serial + '] shot ERROR: ' + (e && e.message ? e.message : JSON.stringify(e)));
    }
    btn.disabled = false;
  }

  async toggleRecord() {
    const btn = this.el('record-btn');
    btn.disabled = true;
    try {
      if (!this.recording) {
        const pkg = this.el('package-input').value.trim();
        const msg = await invoke('start_recording', { serial: this.serial, package: pkg });
        this.recording = true;
        btn.textContent = '停止录屏';
        this.setStatus(msg + '（停止时封盘）');
        _diag('[' + this.serial + '] record: started');
      } else {
        const msg = await invoke('stop_recording', { serial: this.serial });
        this.setStatus(msg);
        _diag('[' + this.serial + '] record: stopping');
        // 按钮复位由监护线程的 record 事件完成（进程封盘退出后）
      }
    } catch (e) {
      await alertBox('录屏失败: ' + (e && e.message ? e.message : e), 'error');
      _diag('[' + this.serial + '] record ERROR: ' + (e && e.message ? e.message : JSON.stringify(e)));
      this.recording = false;
      btn.textContent = '录屏';
    }
    btn.disabled = false;
  }

  // 录屏进程退出事件（stopped=用户停止封盘 / closed=进程正常退出 / failed=异常 /
  // retrying=设备端 server 启动失败自动重试中）：retrying 只更新状态栏不复位按钮；
  // 其余统一复位按钮；stopped 附产物路径
  handleRecordEvent(stage, message, path) {
    if (stage === 'retrying') {
      this.setStatus('录屏视频流未建立，自动重试一次…');
      _diag('[' + this.serial + '] record retrying');
      return;
    }
    if (stage !== 'stopped' && stage !== 'closed' && stage !== 'failed') return;
    const btn = this.el('record-btn');
    this.recording = false;
    btn.textContent = '录屏';
    btn.disabled = false;
    if (stage === 'stopped') {
      this.setStatus('录屏已保存: ' + (path || '未知路径'));
    } else if (stage === 'closed') {
      this.setStatus('录屏进程已退出' + (path ? '（产物: ' + path + '）' : ''));
    } else {
      const last = (message || '').split('\n').filter(Boolean).pop() || '未知原因';
      this.setStatus('录屏异常退出: ' + last);
    }
    _diag('[' + this.serial + '] record ' + stage + (path ? ': ' + path : ''));
  }

  // ---- logcat 抓取（「日志」tab；core logcat 模块流式落盘 + 批量行事件） ----
  async toggleLogcat() {
    const btn = this.el('logcat-btn');
    btn.disabled = true;
    try {
      if (!this.logcatRunning) {
        const pkg = this.el('package-input').value.trim();
        const msg = await invoke('start_logcat', {
          serial: this.serial,
          package: pkg,
          filterPackage: this.el('logcat-bypkg').checked,
          level: this.el('logcat-level').value,
          text: this.el('logcat-text').value.trim(),
        });
        this.logcatRunning = true;
        btn.textContent = '停止抓取';
        // 新会话隔离：清上一会话的视图/缓冲（落盘文件按会话分目录，旧日志在那）
        this.logcatBuf = [];
        this.logcatDirty = false;
        this.el('logcat-view').innerHTML = '';
        this.el('logcat-path').textContent = msg.replace(/^logcat 抓取中: /, '');
        this.setStatus(msg);
        this.syncLogcatEvents(); // 可见性同步（启动必在日志页，正常 resume/no-op）
        _diag('[' + this.serial + '] logcat: started');
      } else {
        const msg = await invoke('stop_logcat', { serial: this.serial });
        this.logcatRunning = false;
        btn.textContent = '开始抓取';
        this.setStatus(msg);
        _diag('[' + this.serial + '] logcat: stopped');
      }
    } catch (e) {
      await alertBox('logcat 失败: ' + (e && e.message ? e.message : e), 'error');
      _diag('[' + this.serial + '] logcat ERROR: ' + (e && e.message ? e.message : JSON.stringify(e)));
      this.logcatRunning = false;
      btn.textContent = '开始抓取';
    }
    btn.disabled = false;
  }

  // logcat 过滤口径热切换（级别下拉 / 按包过滤勾选 / 包名 / 文本过滤输入变更）：
  // 抓取中变更即调 restart_logcat（同文件续写、不打断会话）；未在抓取时不动作
  // （下次开始按当前值读取）。包名变更仅在按包过滤勾选时有意义（全机抓取与包名无关）
  async restartLogcatIfRunning() {
    if (!this.logcatRunning) return;
    const byPkg = this.el('logcat-bypkg').checked;
    const pkg = this.el('package-input').value.trim();
    const text = this.el('logcat-text').value.trim();
    try {
      const msg = await invoke('restart_logcat', {
        serial: this.serial,
        package: pkg,
        filterPackage: byPkg,
        level: this.el('logcat-level').value,
        text: text,
      });
      this.setStatus(msg);
      _diag('[' + this.serial + '] logcat: restarted (' + (byPkg ? 'pkg=' + pkg : 'all') + ', level=' + this.el('logcat-level').value + ', text=' + (text || '(none)') + ')');
    } catch (e) {
      // 解析失败（如 A11 按包过滤但应用未运行）：旧口径继续抓，如实提示
      await alertBox('logcat 切换失败（旧口径继续）: ' + (e && e.message ? e.message : e), 'error');
      _diag('[' + this.serial + '] logcat restart ERROR: ' + (e && e.message ? e.message : JSON.stringify(e)));
    }
  }

  // logcat 事件（lines=批量日志行 / error=连续秒死放弃）：lines 追加到视图
  // （ring buffer 修剪 + 级别着色 + 自动滚动）；error 复位按钮并提示
  handleLogcatEvent(stage, lines, message) {
    if (stage === 'lines') {
      this.appendLogcatLines(lines || []);
    } else if (stage === 'error') {
      this.logcatRunning = false;
      const btn = this.el('logcat-btn');
      btn.textContent = '开始抓取';
      btn.disabled = false;
      this.setStatus('logcat 抓取中止: ' + (message || '未知原因'));
      _diag('[' + this.serial + '] logcat error: ' + message);
      // 兜底清后端槽位（stop 幂等：写线程已退出，kill/join 立即返回）——
      // 治本在后端 start 的 is_done 探测（本事件可能晚到/丢失）
      invoke('stop_logcat', { serial: this.serial }).catch(() => {});
    }
  }

  // 日志页可见性 → 后端事件推送开关（不可见暂停 IPC，可见恢复并补发 pending；
  // dirty 重放负责 DOM，两机制独立）。抓取未运行时 no-op
  syncLogcatEvents() {
    if (!this.logcatRunning) return;
    const visible = app.active === this.serial && this.activeTab === 'logcat';
    invoke('set_logcat_events', { serial: this.serial, paused: !visible }).catch((e) => {
      // 极端情况（IPC 掉线）：后端暂停状态可能与前端认知失同步（视图静默停滞），
      // 打点供诊断；恢复靠下一次任意切换的 sync（幂等重发）
      _diag('[' + this.serial + '] logcat syncEvents ERROR: ' + JSON.stringify(e));
    });
  }

  // 追加日志行：数据恒入 buf（ring buffer 修剪）；仅当设备页+日志 tab 都激活时
  // 才碰 DOM（否则置 dirty，切回重放）——非可见 DOM 变更是 WebKit 无谓开销的大头
  appendLogcatLines(lines) {
    if (!lines.length) return;
    // 大批发送（≥500 行）= 暂停恢复后的 pending 补发，打点供验证
    if (lines.length >= 500) _diag('[' + this.serial + '] logcat bigbatch ' + lines.length);
    for (const l of lines) this.logcatBuf.push(l);
    while (this.logcatBuf.length > LOGCAT_VIEW_CAP) this.logcatBuf.shift();
    if (app.active === this.serial && this.activeTab === 'logcat') {
      this.renderLogcatLines(lines);
    } else {
      this.logcatDirty = true;
    }
  }

  // 渲染行到 DOM（调用前提：日志页可见）：级别着色 + 修剪 + 未暂停时滚到底部
  renderLogcatLines(lines) {
    const view = this.el('logcat-view');
    const frag = document.createDocumentFragment();
    for (const line of lines) {
      const div = document.createElement('div');
      const m = line.match(/^(?:\+\d{4}\s+)?\d{4}-\d{2}-\d{2}\s+\d{2}:\d{2}:\d{2}\.\d{3}\s+\d+\s+\d+\s+([VDIWEF])\s/);
      div.className = 'log-line' + (m ? ' lv-' + m[1] : '');
      div.textContent = line;
      frag.appendChild(div);
    }
    view.appendChild(frag);
    while (view.childElementCount > LOGCAT_VIEW_CAP) view.removeChild(view.firstChild);
    if (!this.logcatPaused) view.scrollTop = view.scrollHeight;
  }

  // 切回日志页/设备页时重放非可见期间积累的行（一次性 DocumentFragment 重建）。
  // 不可见时跳过（设备页激活但日志 tab 隐藏的中间态不重建 DOM，切到日志页再放）
  renderLogcatIfDirty() {
    if (!this.logcatDirty) return;
    if (!(app.active === this.serial && this.activeTab === 'logcat')) return;
    this.logcatDirty = false;
    const view = this.el('logcat-view');
    view.innerHTML = '';
    if (this.logcatBuf.length) this.renderLogcatLines(this.logcatBuf);
  }

  // ---- 状态栏（顶栏显示当前激活设备的状态；非激活设备暂存自己的状态） ----
  setStatus(text) {
    this.statusText = text;
    this.statusProgress = null;
    app.renderStatus();
  }
  setStatusProgress(text, pct) {
    this.statusText = text;
    this.statusProgress = Math.max(0, Math.min(100, pct));
    app.renderStatus();
  }

  // ---- 指标勾选 → 图表显隐 ----
  toggleCharts() {
    const pairs = [
      ['cpu', 'chart-cpu-box'], ['memory', 'chart-mem-box'], ['fps', 'chart-fps-box'],
      ['freq', 'chart-freq-box'], ['thermal', 'chart-temp-box'], ['gpu', 'chart-gpu-box'],
      ['gpumem', 'chart-gpumem-box'], // GPU 显存独立开关（SS2MAX 等无数据源平台禁用）
      ['io', 'chart-io-box'], ['net', 'chart-net-box'],
    ];
    for (const [id, boxCls] of pairs) {
      this.el(boxCls).classList.toggle('hidden', !this.metricBox(id).checked);
    }
    // 线程数据来自 CpuUpdate：CPU 关闭时线程面板同步隐藏
    this.el('panel-thread').classList.toggle('hidden', !this.metricBox('cpu').checked);
    this.refreshChartSizes();
  }
  // rAF 刷新图表尺寸：容器显隐后布局完成时机不确定（webkit2gtk 布局异步），
  // 用双 rAF 保证在下一帧布局完成后取到正确 clientWidth/Height
  refreshChartSizes() {
    requestAnimationFrame(() => requestAnimationFrame(() => { for (const c of this.allCharts) c.resize(); }));
  }
  autoCheck(id) {
    const box = this.metricBox(id);
    if (!box.checked) { box.checked = true; this.toggleCharts(); }
  }

  // ---- 实际采样周期展示：各指标限频不同，动态计算并显示 ----
  fmtPeriod(ms) { return ms >= 1000 ? (ms / 1000) + 's' : ms + 'ms'; }
  updateEffectiveRates() {
    const interval = parseInt(this.el('interval-select').value, 10);
    // 与 agent 端限频逻辑一致（见 xperf-agent/main.rs）
    const metrics = [
      { id: 'cpu',      name: 'CPU',   period: interval },
      { id: 'memory',   name: '内存',   period: interval },
      { id: 'fps',      name: 'FPS',   period: Math.ceil(500 / interval) * interval },
      { id: 'freq',     name: '频率',   period: interval },
      { id: 'thermal',  name: '温度',   period: Math.ceil(2000 / interval) * interval },
      { id: 'gpu',      name: 'GPU',   period: Math.max(100, Math.min(interval, 1000)) },
      { id: 'gpumem',  name: 'GPU 显存', period: Math.max(100, Math.min(interval, 1000)) },
      { id: 'io',       name: 'IO',    period: interval },
      { id: 'net',      name: '网络',   period: interval },
    ];
    const enabled = metrics.filter(m => this.metricBox(m.id).checked);
    if (enabled.length === 0) {
      this.el('effective-rates').innerHTML = '';
      return;
    }
    const parts = enabled.map(m => {
      const same = m.period === interval;
      return `<span class="${same ? 'rate-same' : 'rate-limited'}">${m.name}: ${this.fmtPeriod(m.period)}</span>`;
    });
    this.el('effective-rates').innerHTML =
      '<span class="rate-label">实际周期</span> ' + parts.join(' · ');
  }

  // ---- 子 tab 切换（性能指标 / Perfetto / Simpleperf / 日志；侧栏公共模块常驻） ----
  switchTab(which) {
    const perf = which === 'perf';
    this.activeTab = which;
    this.el('perf-content').classList.toggle('hidden', !perf);
    this.el('trace-content').classList.toggle('hidden', which !== 'trace');
    this.el('stack-content').classList.toggle('hidden', which !== 'stack');
    this.el('logcat-content').classList.toggle('hidden', which !== 'logcat');
    for (const t of this.els('subtab')) t.classList.toggle('active', t.dataset.tab === which);
    // 图表容器显隐变化后尺寸需刷新
    if (perf) this.refreshChartSizes();
    // 日志页重新可见：重放非可见期间积累的行 + 恢复后端事件推送
    if (which === 'logcat') this.renderLogcatIfDirty();
    this.syncLogcatEvents();
  }

  // ---- 面板渲染 ----
  fmtTime(t) { return new Date(t).toTimeString().slice(0, 8); }
  setLive(key, label, value, unit, color) {
    this.liveData[key] = { label, value, unit, color: color || 'text' };
    this.liveDirty = true;
  }
  renderLive() {
    const rows = Object.entries(this.liveData).map(([key, d]) =>
      `<tr><td>${d.label}</td><td style="color:var(--${d.color})">${d.value}${d.unit}</td></tr>`);
    this.el('live-table').innerHTML =
      '<tr><th>指标</th><th>当前值</th></tr>' + rows.join('');
  }
  renderPeaks() {
    const rows = Object.entries(this.peaks).map(([pid, p]) =>
      `<tr><td>${pid}</td>` +
      `<td>${p.cpu ? p.cpu.v.toFixed(1) : '-'}</td><td>${p.cpu ? this.fmtTime(p.cpu.t) : '-'}</td>` +
      `<td>${p.mem ? p.mem.v.toFixed(1) : '-'}</td><td>${p.mem ? this.fmtTime(p.mem.t) : '-'}</td></tr>`);
    this.el('peak-table').innerHTML =
      '<tr><th>PID</th><th>峰值 CPU%</th><th>时间</th><th>峰值 PSS(MB)</th><th>时间</th></tr>' + rows.join('');
  }
  trackPeak(pid, kind, v, t) {
    if (!this.peaks[pid]) this.peaks[pid] = {};
    if (!this.peaks[pid][kind] || v > this.peaks[pid][kind].v) {
      this.peaks[pid][kind] = { v, t };
      this.renderPeaks();
    }
  }
  renderThreads() {
    const all = [];
    for (const [pid, threads] of Object.entries(this.latestThreads)) {
      for (const t of threads) all.push({ pid, tid: t.tid, name: t.name, cpu: t.cpu_usage });
    }
    all.sort((a, b) => b.cpu - a.cpu);
    const rows = all.slice(0, 8).map(t =>
      `<tr><td>${t.name}</td><td>${t.tid}</td><td>${t.pid}</td><td>${t.cpu.toFixed(1)}</td></tr>`);
    this.el('thread-table').innerHTML =
      '<tr><th>线程</th><th>TID</th><th>PID</th><th>CPU%</th></tr>' + rows.join('');
  }

  // ---- 冷启动面板：最近 5 次「打开/重启」的 am start -W 测量 ----
  renderColdStarts() {
    const rows = this.coldStarts.map(c =>
      `<tr><td>${c.time}</td><td>${c.action}</td><td>${c.total}</td><td>${c.wait}</td></tr>`);
    this.el('coldstart-table').innerHTML =
      '<tr><th>时间</th><th>操作</th><th>TotalTime(ms)</th><th>WaitTime(ms)</th></tr>' +
      (rows.length ? rows.join('')
        : '<tr><td colspan="4" style="color:var(--text-dim)">「打开应用」/「重启应用」时自动测量（am start -W）</td></tr>');
  }
  recordColdStart(action, r) {
    this.coldStarts.unshift({ time: new Date().toTimeString().slice(0, 8), action, total: r.total_time_ms, wait: r.wait_time_ms });
    if (this.coldStarts.length > 5) this.coldStarts.pop();
    this.renderColdStarts();
    // 重定向检测：实际启动的 Activity 不属于目标包（如车机熄屏/锁定时被系统
    // 引导页接管）——测量值无效，须提示用户而非给出误导性的 0ms。
    // Activity 两种形态都算本包：`pkg/.MainActivity`（短类名）与 `pkg.MainActivity`（完整类名）
    const pkg = this.package();
    const inPkg = r.activity.startsWith(pkg + '/') || r.activity.startsWith(pkg + '.');
    const redirected = pkg && !inPkg;
    this.setStatus(redirected
      ? `${action}应用被重定向: 实际启动 ${r.activity}（设备可能熄屏/锁定，唤醒后重试）`
      : `${action}应用完成 — 冷启动 TotalTime ${r.total_time_ms}ms（WaitTime ${r.wait_time_ms}ms，${r.activity}）`);
  }

  // ---- 打开/重启应用（顺带测冷启动；activity 留空自动解析主入口） ----
  async launchOrRestart(kind) { // kind: '打开' | '重启'
    const pkg = this.package();
    if (!pkg) { await alertBox('请先填写包名', 'warning'); return; }
    const activity = this.el('activity-input').value.trim();
    const btn = this.el(kind === '打开' ? 'launch-btn' : 'restart-btn');
    btn.disabled = true;
    this.setStatus(`${kind}应用中: ${pkg}${activity ? '（' + activity + '）' : '（自动解析主入口）'}…`);
    try {
      const r = await invoke(kind === '打开' ? 'launch_app' : 'restart_app',
        { serial: this.serial, package: pkg, activity });
      this.recordColdStart(kind, r);
      // r 是 serde 序列化的数据对象（无 Rust 方法）；摘要前端自拼
      _diag(`[${this.serial}] ${kind}应用: ${r.activity} TotalTime=${r.total_time_ms}ms WaitTime=${r.wait_time_ms}ms`);
    } catch (e) {
      await alertBox(`${kind}应用失败: ${e && e.message ? e.message : e}`, 'error');
      _diag(`[${this.serial}] ${kind}应用 ERROR: ` + (e && e.message ? e.message : JSON.stringify(e)));
    } finally {
      btn.disabled = false;
    }
  }

  // ---- 停止应用（am force-stop 清场） ----
  async stopApp() {
    const pkg = this.package();
    if (!pkg) { await alertBox('请先填写包名', 'warning'); return; }
    const btn = this.el('stop-app-btn');
    btn.disabled = true;
    this.setStatus(`停止应用中: ${pkg}…`);
    try {
      const msg = await invoke('stop_app', { serial: this.serial, package: pkg });
      this.setStatus(msg);
      _diag(`[${this.serial}] 停止应用: ${msg}`);
    } catch (e) {
      await alertBox(`停止应用失败: ${e && e.message ? e.message : e}`, 'error');
      _diag(`[${this.serial}] 停止应用 ERROR: ` + (e && e.message ? e.message : JSON.stringify(e)));
    } finally {
      btn.disabled = false;
    }
  }

  // PID 变化时刷新状态栏：PIDs 融入「监控中」文案（如「监控中: pkg（PID 4428）」）。
  // 已退出（stopped）的 PID 不展示——防止列表无限增长。
  renderPidList() {
    if (!this.samplingRunning) return;
    const pkg = this.package() || '';
    const pids = Object.keys(this.pidData).filter(pid => !this.pidData[pid].stopped);
    this.setStatus(pids.length ? `监控中: ${pkg}（PID ${pids.join(', ')}）` : `监控中: ${pkg}`);
  }

  // ---- sample 事件处理（payload.event = 后端 SampleEvent） ----
  handleSample(ev) {
    this.eventCount++;
    if (this.eventCount === 1) _diag('[' + this.serial + '] first sample event: ' + JSON.stringify(ev).slice(0, 150));
    if (ev.PidDiscovered) {
      if (!this.pidData[ev.PidDiscovered.pid]) {
        this.pidData[ev.PidDiscovered.pid] = { cpu: [], mem: [], new: true };
        this.renderPidList();
      }
    } else if (ev.PidDisappeared) {
      const pid = ev.PidDisappeared.pid;
      if (this.pidData[pid]) { this.pidData[pid].stopped = true; this.renderPidList(); }
      delete this.latestThreads[pid]; // 进程已死，Top 线程表不再展示其残留线程
      this.threadsDirty = true;
    } else if (ev.CpuUpdate) {
      const { pid, timestamp, process_cpu, threads } = ev.CpuUpdate;
      if (!this.pidData[pid]) { this.pidData[pid] = { cpu: [], mem: [], new: true }; this.renderPidList(); }
      const t = new Date(timestamp).getTime();
      this.charts.cpu.push('PID ' + pid, t, +process_cpu.toFixed(2));
      this.trackPeak(pid, 'cpu', process_cpu, t);
      this.latestThreads[pid] = threads;
      this.threadsDirty = true;
      this.setLive('cpu', 'CPU (pid ' + pid + ')', process_cpu.toFixed(1), '%', 'accent');
      // 线程 top1
      if (threads.length > 0) {
        const top1 = threads.reduce((a, b) => a.cpu_usage > b.cpu_usage ? a : b);
        this.setLive('cpu_top', '  └ ' + top1.name, top1.cpu_usage.toFixed(1), '%', 'dim');
      }
      this.charts.cpu.requestDraw();
    } else if (ev.MemoryUpdate) {
      const { pid, timestamp, total_pss, details } = ev.MemoryUpdate;
      if (!this.pidData[pid]) { this.pidData[pid] = { cpu: [], mem: [], new: true }; this.renderPidList(); }
      const t = new Date(timestamp).getTime();
      // 单位统一：内存展示/图表/峰值/导出全 MB（协议与基线 JSON 存储仍 KB）
      this.charts.mem.push('PID ' + pid, t, total_pss / 1024);
      this.trackPeak(pid, 'mem', total_pss / 1024, t);
      this.setLive('mem', '内存 PSS (pid ' + pid + ')', (total_pss / 1024).toFixed(1), ' MB', 'ok');
      if (details) {
        // App Summary 7 分类 + 协议 v8 拆出的 DMA-BUF（合计恒等于 PSS；DMA-BUF 是
        // gralloc/dma-heap 显存的 CPU mmap，常是直渲染应用大头——已从 Other 扣减单列；
        // 低间隔/非 root 路径 dmabuf 为 0，Other 退化为原 Private Other 口径）
        this.setLive('mem_native', '  └ Native', (details.native_heap / 1024).toFixed(1), ' MB', 'dim');
        this.setLive('mem_java', '  └ Java', (details.java_heap / 1024).toFixed(1), ' MB', 'dim');
        this.setLive('mem_gfx', '  └ Graphics', (details.graphics / 1024).toFixed(1), ' MB', 'dim');
        this.setLive('mem_code', '  └ Code', (details.code / 1024).toFixed(1), ' MB', 'dim');
        this.setLive('mem_stack', '  └ Stack', (details.stack / 1024).toFixed(1), ' MB', 'dim');
        this.setLive('mem_dmabuf', '  └ DMA-BUF', ((details.dmabuf || 0) / 1024).toFixed(1), ' MB', 'dim');
        this.setLive('mem_other', '  └ 其他', (details.private_other / 1024).toFixed(1), ' MB', 'dim');
        this.setLive('mem_sys', '  └ System', (details.system / 1024).toFixed(1), ' MB', 'dim');
      }
      this.charts.mem.requestDraw();
    } else if (ev.FpsUpdate) {
      const { pid, timestamp, layer, fps, jank_count } = ev.FpsUpdate;
      if (!this.pidData[pid]) { this.pidData[pid] = { cpu: [], mem: [], new: true }; this.renderPidList(); }
      // 自动启动带 --fps 时前端勾选框未同步，收到首个 FPS 事件自动展开图表
      this.autoCheck('fps');
      // 多渲染面并存时逐图层一条折线（如游戏主 Surface + 相机预览），
      // key 用图层短名（截到最后一段，长名图例放不下）
      const shortLayer = layer.split('/').pop();
      const t = new Date(timestamp).getTime();
      this.charts.fps.push(shortLayer, t, +fps.toFixed(1));
      if (!this.fpsHist[shortLayer]) this.fpsHist[shortLayer] = [];
      pushCapped(this.fpsHist[shortLayer], { t, fps, jank: jank_count });
      this.setLive('fps', 'FPS (' + shortLayer + ')', fps.toFixed(1), '', 'err');
      this.setLive('fps_jank', '  └ Jank', jank_count, '', 'dim');
      this.charts.fps.requestDraw();
    } else if (ev.NoProcess) {
      // 被测进程死亡重扫期间与录制并行时，不要抹掉录制进度条（等下一秒 progress 事件会恢复，
      // 但期间空白更差）；录制态只更新右侧 PID 列表相关状态，status 保持
      if (this.statusProgress === null) this.setStatus('无进程: ' + ev.NoProcess.error);
    } else if (ev.AgentHello) {
      this.maxkhz = ev.AgentHello.maxkhz || [];
      // 权限身份（v3 协议）：更新徽章 + IO 灰显；root 状态变化（获取 root 后重连）实时生效
      const rooted = !!ev.AgentHello.root;
      if (rooted !== this.rooted) {
        this.rooted = rooted;
        this.updatePermBadge();
        if (!rooted && this.statusProgress === null) {
          this.setStatus('设备非 root（shell）：IO 不可用，低间隔内存降级为 ≥500ms dumpsys 明细');
        }
      }
    } else if (ev.SampleError) {
      // agent 能力/降级类提示（overrun 类高频警告后端已过滤）：去重防刷状态栏；
      // 录制进度态不打断（等 progress 事件恢复）
      const m = 'agent: ' + ev.SampleError.error;
      if (m !== this._lastAgentErr) {
        this._lastAgentErr = m;
        if (this.statusProgress === null) this.setStatus(m);
        _diag('[' + this.serial + '] agent err: ' + ev.SampleError.error);
      }
    } else if (ev.FreqUpdate) {
      const { timestamp, khz } = ev.FreqUpdate;
      // 勾选框未同步（自动启动带 --freq）时自动展开
      this.autoCheck('freq');
      const t = new Date(timestamp).getTime();
      khz.forEach((k, i) => this.charts.freq.push('cpu' + i, t, k / 1000)); // KHz → MHz
      // 频率展示：平均 + 最高核
      const mhz = khz.map(k => k / 1000);
      const avg = mhz.reduce((a, b) => a + b, 0) / mhz.length;
      this.setLive('freq', 'CPU 频率', avg.toFixed(0), ' MHz', 'warn');
      this.setLive('freq_max', '  └ 最高核', Math.max(...mhz).toFixed(0), ' MHz', 'dim');
      this.charts.freq.requestDraw();
    } else if (ev.TempUpdate) {
      const { timestamp, status, sensors } = ev.TempUpdate;
      this.autoCheck('thermal');
      const t = new Date(timestamp).getTime();
      this.charts.temp.title = `Temperature (°C) — thermal status ${status}`;
      for (const [name, , value] of sensors) {
        this.charts.temp.push(name, t, value);
        if (!this.tempHist[name]) this.tempHist[name] = [];
        pushCapped(this.tempHist[name], { t, v: value, status });
        this.setLive('temp_' + name, '温度 ' + name, value.toFixed(1), ' °C', 'orange');
      }
      this.setLive('temp_status', '  └ 热状态', status >= 0 ? status : '?', '', 'dim');
      this.charts.temp.requestDraw();
    } else if (ev.GpuUpdate) {
      const { timestamp, busy, util, mhz, maxmhz } = ev.GpuUpdate;
      this.autoCheck('gpu');
      const t = new Date(timestamp).getTime();
      // QNX 路径带 util/maxmhz（util = busy 按频率折算的利用率）；kgsl 路径 util=0
      this.charts.gpu.title = maxmhz > 0 ? `GPU (%) — ${mhz}/${maxmhz} MHz` : `GPU Busy (%) — @ ${mhz} MHz`;
      this.charts.gpu.push('busy', t, +busy.toFixed(2));
      if (util > 0 || maxmhz > 0) this.charts.gpu.push('util', t, +util.toFixed(2));
      pushCapped(this.gpuHist, { t, busy, util, mhz });
      this.setLive('gpu_busy', 'GPU busy', busy.toFixed(1), '%', 'purple');
      if (maxmhz > 0) this.setLive('gpu_freq', '  └ 频率', mhz + '/' + maxmhz, ' MHz', 'dim');
      if (util > 0) this.setLive('gpu_util', '  └ util', util.toFixed(1), '%', 'dim');
      this.charts.gpu.requestDraw();
    } else if (ev.GpuProcUpdate) {
      // QNX 路径：每进程 GPU busy%
      const { pid, timestamp, busy } = ev.GpuProcUpdate;
      if (!this.pidData[pid]) { this.pidData[pid] = { cpu: [], mem: [], new: true }; this.renderPidList(); }
      this.autoCheck('gpu');
      const t = new Date(timestamp).getTime();
      this.charts.gpu.push('PID ' + pid, t, +busy.toFixed(2));
      if (!this.gpuprocHist[pid]) this.gpuprocHist[pid] = [];
      pushCapped(this.gpuprocHist[pid], { t, busy });
      this.setLive('gpu_proc_' + pid, 'GPU busy (pid ' + pid + ')', busy.toFixed(1), '%', 'purple');
      this.charts.gpu.requestDraw();
    } else if (ev.GpuMemUpdate) {
      // --gpu 降级路径（hypervisor 平台）：每 PID GPU 显存
      const { pid, timestamp, bytes, global } = ev.GpuMemUpdate;
      if (!this.pidData[pid]) { this.pidData[pid] = { cpu: [], mem: [], new: true }; this.renderPidList(); }
      this.autoCheck('gpu');
      const t = new Date(timestamp).getTime();
      const mb = bytes / 1e6;
      this.charts.gpumem.push('PID ' + pid, t, +mb.toFixed(1));
      this.charts.gpumem.push('global', t, Math.round(global / 1e6));
      if (!this.gpumemHist[pid]) this.gpumemHist[pid] = [];
      pushCapped(this.gpumemHist[pid], { t, mb, gmb: global / 1e6 });
      this.setLive('gpumem_' + pid, 'GPU 显存 (pid ' + pid + ')', mb.toFixed(0), ' MB', 'teal');
      this.setLive('gpumem_global', '  └ 整机', (global / 1e6).toFixed(0), ' MB', 'dim');
      this.charts.gpumem.requestDraw();
    } else if (ev.IoUpdate) {
      const { pid, timestamp, r, w, dr, dw } = ev.IoUpdate;
      if (!this.pidData[pid]) { this.pidData[pid] = { cpu: [], mem: [], new: true }; this.renderPidList(); }
      this.autoCheck('io');
      const t = new Date(timestamp).getTime();
      this.charts.io.push(`PID ${pid} R`, t, +r.toFixed(2));
      this.charts.io.push(`PID ${pid} W`, t, +w.toFixed(2));
      if (!this.ioHist[pid]) this.ioHist[pid] = [];
      pushCapped(this.ioHist[pid], { t, r, w, dr, dw });
      this.setLive('io_' + pid, 'IO 读/写 (pid ' + pid + ')', r.toFixed(1) + ' / ' + w.toFixed(1), ' KB/s', 'orange');
      this.charts.io.requestDraw();
    } else if (ev.NetUpdate) {
      const { timestamp, rx, tx } = ev.NetUpdate;
      this.autoCheck('net');
      const t = new Date(timestamp).getTime();
      this.charts.net.push('RX', t, +rx.toFixed(2));
      this.charts.net.push('TX', t, +tx.toFixed(2));
      this.setLive('net', '网络 RX/TX', rx.toFixed(1) + ' / ' + tx.toFixed(1), ' KB/s', 'sky');
      this.charts.net.requestDraw();
    }
    // updateTitle 不再随事件调用（O 全设备累计点数/事件，长会话平方级）——5s 定时器更新
  }

  // ---- trace / stack 事件处理（payload 带 serial 已由 App 分发） ----
  // 深挖「录制并分析」按钮（分析页 toolbar 内）状态同步
  setTraceButtons(disabled) {
    this.el('trace-start-btn').disabled = disabled;
  }
  setStackButtons(disabled) {
    this.el('stack-start-btn').disabled = disabled;
  }

  // 启动 perfetto 录制+分析（分析页 toolbar 入口；不切换当前页，
  // 进度在状态栏与报告区可见，done/error 时自动切到分析页）
  async startTraceRecording(seconds) {
    const pkg = this.package();
    if (!pkg) { await alertBox('请先填写包名', 'warning'); return; }
    try {
      await invoke('start_trace', { serial: this.serial, package: pkg, seconds });
      this.setTraceButtons(true);
      this.setStatus('Perfetto 分析录制中: ' + pkg);
      _diag('[' + this.serial + '] traceBtn: ' + pkg + ' ' + seconds + 's');
    } catch (err) {
      await alertBox('Perfetto 分析错误: ' + err, 'error');
      _diag('[' + this.serial + '] traceBtn invoke ERROR: ' + JSON.stringify(err));
    }
  }

  // 启动 simpleperf 录制+报告（分析页 toolbar 入口，同上）
  async startStackRecording(seconds) {
    const pkg = this.package();
    if (!pkg) { await alertBox('请先填写包名', 'warning'); return; }
    try {
      await invoke('start_stack', { serial: this.serial, package: pkg, seconds });
      this.setStackButtons(true);
      this.setStatus('Simpleperf 分析录制中: ' + pkg);
      _diag('[' + this.serial + '] stackBtn: ' + pkg + ' ' + seconds + 's');
    } catch (err) {
      await alertBox('Simpleperf 错误: ' + err, 'error');
      _diag('[' + this.serial + '] stackBtn invoke ERROR: ' + JSON.stringify(err));
    }
  }

  handleTrace(p) {
    const { stage, message, trace_path } = p;
    if (stage === 'progress') {
      // 进度态：绿色填充按已录制秒数比例铺开（message 如 "perfetto 录制中 3/8s"）
      const m = message.match(/(\d+)\/(\d+)s/);
      if (m) this.setStatusProgress(message, (parseInt(m[1], 10) / parseInt(m[2], 10)) * 100);
      _diag('[' + this.serial + '] trace: ' + message);
      return;
    }
    if (trace_path) {
      this.currentTracePath = trace_path;
      this.el('open-perf-btn').disabled = false;
      this.el('trace-content').querySelector('.trace-file-label').textContent = trace_path;
    }
    this.el('trace-report').textContent = message;
    if (stage === 'recording') {
      // 命令行自动启动（--trace）时无 click handler 禁用，收到 recording 事件统一禁用
      this.setTraceButtons(true);
      this.setStatus(message);
    } else if (stage === 'recorded') {
      // 录制完成进入分析阶段：退出进度态，避免进度条冻结在 100%"录制中"
      this.setStatus('Perfetto trace 已拉回，分析中…');
    } else if (stage === 'done') {
      this.setTraceButtons(false);
      this.setStatus('Perfetto 分析完成');
      if (app.active === this.serial) this.switchTab('trace');
    } else if (stage === 'error') {
      this.setTraceButtons(false);
      this.setStatus('Perfetto 分析失败');
      if (app.active === this.serial) this.switchTab('trace');
    }
    _diag('[' + this.serial + '] trace: ' + stage);
  }

  handleStack(p) {
    const { stage, message, data_path } = p;
    if (stage === 'progress') {
      const m = message.match(/(\d+)\/(\d+)s/);
      if (m) this.setStatusProgress(message, (parseInt(m[1], 10) / parseInt(m[2], 10)) * 100);
      _diag('[' + this.serial + '] stack: ' + message);
      return;
    }
    if (data_path) {
      this.currentStackPath = data_path;
      this.el('open-stack-btn').disabled = false;
      this.el('stack-content').querySelector('.trace-file-label').textContent = data_path;
    }
    this.el('stack-report').textContent = message;
    if (stage === 'recording') {
      this.setStackButtons(true);
      this.setStatus(message);
    } else if (stage === 'recorded') {
      this.setStatus('调用栈已拉回，生成报告中…');
    } else if (stage === 'done') {
      this.setStackButtons(false);
      this.setStatus('Simpleperf 分析完成');
      if (app.active === this.serial) this.switchTab('stack');
    } else if (stage === 'error') {
      this.setStackButtons(false);
      this.setStatus('Simpleperf 分析失败');
      if (app.active === this.serial) this.switchTab('stack');
    }
    _diag('[' + this.serial + '] stack: ' + stage);
  }

  handleSamplingError(message) {
    this.agentBuilding = false;
    this.setStatus('错误: ' + message);
    this.el('start-btn').disabled = false;
    this.el('stop-btn').disabled = true;
    this.samplingRunning = false;
  }

  // 开发运行的 agent 构建进度（首次/源码变更后交叉构建约 1-2 分钟，
  // 期间采样线程阻塞——状态栏提示防「点了开始没反应」；失败走 sampling-error）。
  // agentBuilding 标志让各路径乐观的「监控中」setStatus 在构建期间让位：
  // 事件先到则守卫跳过「监控中」，事件后到则直接覆盖——两种时序都安全。
  handleAgentBuild(stage) {
    if (stage === 'building') {
      this.agentBuilding = true;
      this.setStatus('首次构建 Android agent 中（约 1-2 分钟，仅开发运行）…');
    } else if (stage === 'done') {
      this.agentBuilding = false;
      // 构建期间用户可能已点停止（samplingRunning=false）——那时没有采样在启动，不覆盖「已停止」
      if (this.samplingRunning) this.setStatus('agent 构建完成，正在启动采样…');
    }
  }

  // ---- 采样启动/停止/重启 ----
  currentFlags() {
    const g = (id) => this.metricBox(id).checked;
    return {
      serial: this.serial,
      package: this.package(),
      interval: parseInt(this.el('interval-select').value, 10),
      cpu: g('cpu'), memory: g('memory'), fps: g('fps'), freq: g('freq'),
      // 注意：tauri 命令参数 snake_case → camelCase（gpu_mem → gpuMem），键名写错报 missing required key
      thermal: g('thermal'), gpu: g('gpu'), gpuMem: g('gpumem'), io: g('io'), net: g('net'),
    };
  }
  resetSessionData() {
    for (const k of Object.keys(this.pidData)) delete this.pidData[k];
    for (const k of Object.keys(this.peaks)) delete this.peaks[k];
    for (const k of Object.keys(this.latestThreads)) delete this.latestThreads[k];
    for (const k of Object.keys(this.fpsHist)) delete this.fpsHist[k];
    for (const k of Object.keys(this.tempHist)) delete this.tempHist[k];
    for (const k of Object.keys(this.ioHist)) delete this.ioHist[k];
    for (const k of Object.keys(this.gpumemHist)) delete this.gpumemHist[k];
    for (const k of Object.keys(this.gpuprocHist)) delete this.gpuprocHist[k];
    for (const k of Object.keys(this.liveData)) delete this.liveData[k];
    this.gpuHist.length = 0;
    this.coldStarts = [];
    this.renderPeaks();
    this.renderColdStarts();
    // 重置图表：序列与配色序号清零（新会话从色板头重新配色），并摘掉悬停 overlay 残留
    for (const c of this.allCharts) { c.series = {}; c.seriesColor = {}; c._colorSeq = 0; c.hideHover(); }
    // 新会话：旧基线对比报告不再适用，隐藏清空
    this.el('panel-baseline').classList.add('hidden');
    this.el('baseline-report').textContent = '';
  }
  async start() {
    // 防重入：双击的第二次落在首次 invoke pending 窗口内（disabled 要等 invoke
    // 返回才置位拦不住）——双发 start_sampling 会报「已在监控中」弹错误框，且
    // 第二次的 resetSessionData 已把首次的图表数据清掉
    if (this._startPending || this.samplingRunning) return;
    this._startPending = true;
    try {
      _diag('[' + this.serial + '] startBtn CLICKED');
      const f = this.currentFlags();
      this.resetSessionData();
      this.renderPidList();
      try {
        _diag('[' + this.serial + '] invoking start_sampling: pkg=' + f.package + ' interval=' + f.interval);
        await invoke('start_sampling', { ...f, fresh: true });
        _diag('[' + this.serial + '] start_sampling RETURNED OK');
        this.samplingRunning = true;
        this.el('start-btn').disabled = true;
        this.el('stop-btn').disabled = false;
        if (!this.agentBuilding) this.setStatus('监控中: ' + f.package);
      } catch (e) {
        this.agentBuilding = false;
        await alertBox('开始监控失败: ' + e, 'error');
        _diag('[' + this.serial + '] startBtn invoke ERROR: ' + JSON.stringify(e));
      }
    } finally {
      this._startPending = false;
    }
  }
  async stop() {
    _diag('[' + this.serial + '] stopBtn CLICKED');
    await invoke('stop_sampling', { serial: this.serial });
    this.samplingRunning = false;
    _diag('[' + this.serial + '] stop_sampling returned');
    this.el('start-btn').disabled = false;
    this.el('stop-btn').disabled = true;
    this.setStatus('已停止');
  }
  async restartSampling() {
    await invoke('stop_sampling', { serial: this.serial });
    const f = this.currentFlags();
    _diag('[' + this.serial + '] restart sampling with flags: ' + JSON.stringify(f));
    await invoke('start_sampling', { ...f, fresh: false });
    if (!this.agentBuilding) this.setStatus('监控中: ' + f.package);
  }
  onMetricToggle() {
    this.toggleCharts();
    this.updateEffectiveRates();
    if (!this.samplingRunning) return;
    // 连续切换去抖 500ms，避免一次改多项时反复重启 agent
    if (this.restartTimer) clearTimeout(this.restartTimer);
    this.restartTimer = setTimeout(() => {
      this.restartTimer = null;
      this.restartSampling().catch((e) => _diag('[' + this.serial + '] restart ERROR: ' + e));
    }, 500);
  }

  // ---- 会话数据收集（基线保存/对比用：前端持有的会话序列，长会话已抽稀；
  //      全分辨率全量数据在后端流式落盘的 CSV 里，导出走 export_csv 目录快照） ----
  collectSessionData() {
    const cpu = {}, mem = {}, fps = {}, freq = {}, temp = {}, io = {}, gpumem = {}, gpuproc = {};
    for (const [k, pts] of Object.entries(this.charts.cpu.series)) cpu[k.replace('PID ', '')] = pts.map(p => [p.t, p.v]);
    for (const [k, pts] of Object.entries(this.charts.mem.series)) mem[k.replace('PID ', '')] = pts.map(p => [p.t, p.v]);
    for (const [layer, pts] of Object.entries(this.fpsHist)) fps[layer] = pts.map(p => [p.t, p.fps, p.jank]);
    for (const [core, pts] of Object.entries(this.charts.freq.series)) freq[core] = pts.map(p => [p.t, p.v]);
    for (const [sensor, pts] of Object.entries(this.tempHist)) temp[sensor] = pts.map(p => [p.t, p.v, p.status]);
    for (const [pid, pts] of Object.entries(this.ioHist)) io[pid] = pts.map(p => [p.t, p.r, p.w, p.dr, p.dw]);
    for (const [pid, pts] of Object.entries(this.gpumemHist)) gpumem[pid] = pts.map(p => [p.t, p.mb, p.gmb]);
    for (const [pid, pts] of Object.entries(this.gpuprocHist)) gpuproc[pid] = pts.map(p => [p.t, p.busy]);
    const gpu = this.gpuHist.map(p => [p.t, p.busy, p.util, p.mhz]);
    const net = (this.charts.net.series['RX'] || []).map((p, i) => [p.t, p.v, (this.charts.net.series['TX'] || [])[i]?.v ?? 0]);
    return { cpu, mem, fps, freq, temp, gpu, io, net, gpumem, gpuproc };
  }

  // ---- 包名列表（该设备独立；list_packages 带 serial 路由） ----
  async loadPackages() {
    try {
      const list = await invoke('list_packages', { serial: this.serial });
      const dl = this.root.querySelector('.pkg-list');
      dl.innerHTML = '';
      for (const p of list) {
        const opt = document.createElement('option');
        opt.value = p;
        dl.appendChild(opt);
      }
      _diag('[' + this.serial + '] packages loaded: ' + list.length);
    } catch (e) {
      _diag('[' + this.serial + '] list_packages ERROR: ' + JSON.stringify(e));
    }
  }

  // --package --device 自动启动回填（--package：与手动开始同流程同效果）
  async applyStartupArgs(args) {
    this.samplingRunning = true;
    this.el('package-input').value = args.package;
    if (args.interval) this.el('interval-select').value = String(args.interval);
    if (args.flags) {
      const flagToMetric = {
        cpu: 'cpu', memory: 'memory', fps: 'fps', freq: 'freq',
        thermal: 'thermal', gpu: 'gpu', gpumem: 'gpumem', io: 'io', net: 'net',
      };
      for (const [f, m] of Object.entries(flagToMetric)) {
        if (args.flags[f]) this.metricBox(m).checked = true;
      }
      this.toggleCharts();
      this.updateEffectiveRates();
    }
    this.el('start-btn').disabled = true;
    this.el('stop-btn').disabled = false;
    // 构建可能早于本页加载开始（building 事件已丢）——回查后端状态补偿
    this.agentBuilding = await invoke('agent_building', { serial: this.serial });
    if (this.agentBuilding) {
      this.setStatus('首次构建 Android agent 中（约 1-2 分钟，仅开发运行）…');
    } else {
      this.setStatus('监控中: ' + args.package);
    }
    // 自动启动会话的设备页置为激活页（否则停留在默认第一台的 idle 页）
    app.switchDevice(this.serial);
  }

  // ---- 设备热插拔（tab 灰显/恢复；数据与采样线程保留，插回自动重连） ----
  setOffline() {
    this.offline = true;
    this.tabBtn.classList.add('offline');
    this.tabBtn.textContent = this.tabLabel();
    this.tabBtn.title = this.tabTitle();
    if (this.samplingRunning) this.setStatus('设备已断开: ' + this.serial + '（采样等待重连，插回自动恢复）');
    else this.setStatus('设备已断开: ' + this.serial);
  }
  setOnline() {
    this.offline = false;
    this.tabBtn.classList.remove('offline');
    this.tabBtn.textContent = this.tabLabel();
    this.tabBtn.title = this.tabTitle();
  }

  // ---- 事件绑定 ----
  bindEvents() {
    this.el('start-btn').addEventListener('click', () => this.start());
    this.el('stop-btn').addEventListener('click', () => this.stop());
    this.el('refresh-pkgs').addEventListener('click', () => this.loadPackages());
    this.el('root-btn').addEventListener('click', () => this.acquireRoot());
    this.el('mirror-btn').addEventListener('click', () => this.toggleMirror());
    this.el('shot-btn').addEventListener('click', () => this.takeScreenshot());
    this.el('record-btn').addEventListener('click', () => this.toggleRecord());
    this.el('logcat-btn').addEventListener('click', () => this.toggleLogcat());
    this.el('logcat-clear-btn').addEventListener('click', () => {
      this.el('logcat-view').innerHTML = '';
      this.logcatBuf = [];
      this.logcatDirty = false;
    });
    // 级别/按包过滤/文本过滤变更热切换（抓取中即时生效，同文件续写）；包名变更仅在勾选时有意义
    this.el('logcat-level').addEventListener('change', () => this.restartLogcatIfRunning());
    this.el('logcat-bypkg').addEventListener('change', () => this.restartLogcatIfRunning());
    this.el('logcat-text').addEventListener('change', () => this.restartLogcatIfRunning());
    this.el('package-input').addEventListener('change', () => {
      if (this.el('logcat-bypkg').checked) this.restartLogcatIfRunning();
    });
    this.el('logcat-pause-btn').addEventListener('click', (e) => {
      this.logcatPaused = !this.logcatPaused;
      e.target.textContent = this.logcatPaused ? '恢复滚动' : '暂停滚动';
      if (!this.logcatPaused) {
        const v = this.el('logcat-view');
        v.scrollTop = v.scrollHeight;
      }
    });
    this.el('launch-btn').addEventListener('click', () => this.launchOrRestart('打开'));
    this.el('restart-btn').addEventListener('click', () => this.launchOrRestart('重启'));
    this.el('stop-app-btn').addEventListener('click', () => this.stopApp());
    this.el('interval-select').addEventListener('change', () => this.updateEffectiveRates());
    for (const id of ['cpu', 'memory', 'fps', 'freq', 'thermal', 'gpu', 'gpumem', 'io', 'net']) {
      this.metricBox(id).addEventListener('change', () => this.onMetricToggle());
    }
    this.el('time-window').addEventListener('change', (e) => {
      const mode = e.target.value;
      for (const c of this.allCharts) { c.windowMode = mode; c.draw(); }
    });
    for (const t of this.els('subtab')) {
      t.addEventListener('click', () => this.switchTab(t.dataset.tab));
    }

    // ---- Perfetto 深挖（分析页 toolbar 内录制；进度见状态栏与报告区） ----
    this.el('trace-start-btn').addEventListener('click', () => {
      this.startTraceRecording(parseInt(this.el('trace-seconds').value, 10) || 10);
    });
    // 打开浏览器 Perfetto UI 并自动加载 trace：本地镜像 UI + 同源深链（全自动）；
    // 离线/镜像失败自动回退拖拽方式（后端 open_perfetto_ui 处理，msg 含结果说明）。
    // 防连点开多个重复标签：进行中 disabled（首次镜像下载可达数秒）+ 完成后短冷却
    this.el('open-perf-btn').addEventListener('click', async () => {
      const btn = this.el('open-perf-btn');
      if (!this.currentTracePath || btn.disabled) return;
      if (Date.now() - (this._lastPerfOpen || 0) < 5000) {
        this.setStatus('Perfetto UI 已在浏览器打开（5s 内不重复开页）');
        return;
      }
      btn.disabled = true;
      try {
        const msg = await invoke('open_perfetto_ui', { tracePath: this.currentTracePath });
        this.setStatus(msg);
        _diag('[' + this.serial + '] openPerfBtn: ' + msg);
      } catch (err) {
        await alertBox('打开 Perfetto UI 失败: ' + err, 'error');
        _diag('[' + this.serial + '] openPerfBtn ERROR: ' + JSON.stringify(err));
      } finally {
        // 冷却锚定在完成时刻（而非点击时刻）+ 5s 窗口：invoke ~0.6s，600ms 点击锚
        // 冷却在 finally 解禁时已过期（连点穿透），扩到 5s 才覆盖真实连点 burst
        // （2026-09-22 压测 s6 实测 600ms burst 仍开 3 页）；进行中窗口由 disabled 覆盖
        this._lastPerfOpen = Date.now();
        btn.disabled = false;
      }
    });

    // ---- Simpleperf 函数热点（分析页 toolbar 内录制） ----
    this.el('stack-start-btn').addEventListener('click', () => {
      this.startStackRecording(parseInt(this.el('stack-seconds').value, 10) || 10);
    });
    // 打开浏览器火焰图：report_html.py 渲染 .data 为单文件 HTML（首次自动下载 AOSP
    // 脚本 ~10MB，需 python3）；HTML 新于 .data 时复用不重渲染。防连点同上
    this.el('open-stack-btn').addEventListener('click', async () => {
      const btn = this.el('open-stack-btn');
      if (!this.currentStackPath || btn.disabled) return;
      if (Date.now() - (this._lastStackOpen || 0) < 5000) {
        this.setStatus('火焰图已在浏览器打开（5s 内不重复开页）');
        return;
      }
      btn.disabled = true;
      try {
        const msg = await invoke('open_stack_html', { dataPath: this.currentStackPath });
        this.setStatus(msg);
        _diag('[' + this.serial + '] openStackBtn: ' + msg);
      } catch (err) {
        await alertBox('打开火焰图失败: ' + err, 'error');
        _diag('[' + this.serial + '] openStackBtn ERROR: ' + JSON.stringify(err));
      } finally {
        // 冷却锚定完成时刻 + 5s 窗口（同 open-perf 的连点穿透修复）
        this._lastStackOpen = Date.now();
        btn.disabled = false;
      }
    });

    // ---- 数据管理（导出/基线/清理；基线与 CLI --save-baseline 同一文件互通） ----
    // 导出 = 复制后端流式落盘的会话 CSV 目录快照（全量数据在磁盘，无需前端回传）
    this.el('export-btn').addEventListener('click', async () => {
      try {
        const dir = await invoke('export_csv', { serial: this.serial });
        this.setStatus('已导出: ' + dir);
      } catch (e) {
        await alertBox('导出失败: ' + e, 'error');
      }
    });
    this.el('save-baseline-btn').addEventListener('click', async () => {
      const pkg = this.package() || 'unknown';
      const d = this.collectSessionData();
      const intervalMs = parseInt(this.el('interval-select').value, 10) || 1000;
      try {
        const path = await invoke('save_baseline', { package: pkg, intervalMs, cpu: d.cpu, mem: d.mem, fps: d.fps, gpu: d.gpu, io: d.io, net: d.net });
        this.setStatus('基线已保存（覆盖旧基线）: ' + path);
      } catch (e) {
        await alertBox('基线保存失败: ' + e, 'error');
      }
    });
    this.el('compare-baseline-btn').addEventListener('click', async () => {
      const pkg = this.package() || 'unknown';
      const d = this.collectSessionData();
      const intervalMs = parseInt(this.el('interval-select').value, 10) || 1000;
      try {
        const report = await invoke('compare_baseline', { package: pkg, intervalMs, cpu: d.cpu, mem: d.mem, fps: d.fps, gpu: d.gpu, io: d.io, net: d.net });
        this.el('baseline-report').textContent = report;
        this.el('panel-baseline').classList.remove('hidden');
        this.setStatus('基线对比完成');
      } catch (e) {
        await alertBox('基线对比失败: ' + e, 'error');
      }
    });
    // 更新火焰图脚本：从 AOSP 强制重新拉取 vendor 文件（覆盖后 git 提交同步到其他机器）
    this.el('update-scripts-btn').addEventListener('click', async () => {
      this.setStatus('更新火焰图脚本中（AOSP，双平台 ~30MB）…');
      try {
        const msg = await invoke('update_simpleperf_scripts');
        this.setStatus(msg);
        _diag('[' + this.serial + '] updateScripts: OK');
      } catch (err) {
        await alertBox('更新火焰图脚本失败: ' + err, 'error');
        _diag('[' + this.serial + '] updateScripts ERROR: ' + JSON.stringify(err));
      }
    });
    // 清理缓存与采集数据：~/.cache/xperf（UI 镜像；simpleperf 脚本集 vendor 在仓库不受影响）+
    // /tmp/xperf（全部采集数据）。采样/录制进行中会丢当前会话产物——原生对话框确认
    // （confirm() 在 webkit2gtk 下标题为 "Javascript-taurixxx"，不专业）
    this.el('clean-btn').addEventListener('click', async () => {
      const ok = await window.__TAURI__.dialog.confirm(
        '将清理：\n- ~/.cache/xperf（UI 镜像）\n- /tmp/xperf（全部采集数据：CSV/图表/trace/调用栈，含全部设备）\n- xperf-core/simpleperf_scripts/（火焰图脚本下载缓存，下次使用重新下载）\n\n正在采样/录制时当前会话产物会丢失，确认清理？',
        { title: 'XPerformance - 清理缓存与数据', kind: 'warning' }
      );
      if (!ok) return;
      try {
        const msg = await invoke('clean_cache');
        this.setStatus(msg);
        _diag('cleanBtn: ' + msg);
      } catch (err) {
        await alertBox('清理失败: ' + err, 'error');
        _diag('cleanBtn ERROR: ' + JSON.stringify(err));
      }
    });
    // 远程主机 adb 设置：写入当前连接主机的已保存配置（下次连接生效）。
    // 区块仅 ssh 模式可见（remoteUI.updateSidebars 控制），全局状态每设备页同份。
    this.el('remote-adb-save').addEventListener('click', async () => {
      const host = remoteUI.connectedHost;
      if (!host) { this.setStatus('未连接远程主机'); return; }
      const r = (remoteUI.remotes || []).find(x => x.host === host || x.name === host);
      if (!r) { await alertBox('该远程是临时目标（未保存配置），先在「＋」表单保存后才能改 adb 设置', 'warning'); return; }
      const adb = this.el('remote-adb-input').value.trim() || 'adb';
      const port = parseInt(this.el('remote-port-input').value, 10) || 5037;
      try {
        await invoke('save_remote', { cfg: { ...r, adb_path: adb, remote_port: port } });
        remoteUI.remotes = await invoke('list_remotes');
        this.setStatus('adb 设置已保存，重连 ' + host + ' 后生效');
      } catch (e) { await alertBox('保存失败: ' + e, 'error'); }
    });
  }
}

// ---------- App：设备会话管理 + 全局事件分发 ----------
const app = {
  sessions: new Map(), // serial -> DeviceSession
  active: null,

  addDevice(info) {
    if (this.sessions.has(info.serial)) {
      // 已有会话（断开重连）：更新信息并恢复
      const s = this.sessions.get(info.serial);
      s.info = info;
      s.applyPlatformCaps();
      s.setOnline();
      this.refreshTabLabels();
      return s;
    }
    const s = new DeviceSession(info.serial, info);
    this.sessions.set(info.serial, s);
    s.loadPackages();
    // 新设备页的侧栏「远程主机」区块默认隐藏，按当前全局连接状态幂等补齐
    // （热插拔在 ssh 模式下新建页时也正确显示）
    remoteUI.updateSidebars();
    if (this.active === null) this.switchDevice(info.serial);
    this.refreshTabLabels();
    return s;
  },

  switchDevice(serial) {
    const s = this.sessions.get(serial);
    if (!s) return;
    this.active = serial;
    for (const [ser, sess] of this.sessions) {
      sess.root.classList.toggle('active', ser === serial);
      sess.tabBtn.classList.toggle('active', ser === serial);
    }
    document.getElementById('noDeviceHint').classList.toggle('hidden', true);
    // 隐藏期间 canvas 尺寸为 0，切回后须刷新图表
    s.refreshChartSizes();
    this.renderStatus();
    s.renderLive();
    s.renderThreads();
    // 日志页若激活：重放设备页隐藏期间积累的行
    s.renderLogcatIfDirty();
    // 全部设备同步事件推送开关（旧设备暂停，新激活设备恢复）
    for (const sess of this.sessions.values()) sess.syncLogcatEvents();
  },

  // 顶栏状态栏：显示当前激活设备的状态（普通文本 / 录制进度绿色填充）
  renderStatus() {
    const el = document.getElementById('status');
    const s = this.sessions.get(this.active);
    if (!s) {
      el.classList.remove('progress');
      el.style.setProperty('--progress', '0%');
      el.textContent = this.sessions.size === 0 ? '未开始' : '未选择设备';
      return;
    }
    el.classList.toggle('progress', s.statusProgress !== null);
    el.style.setProperty('--progress', (s.statusProgress ?? 0) + '%');
    el.textContent = s.statusText;
  },

  updateTitle() {
    // 统计全部设备图表真实数据源（诊断用）
    let events = 0, cpuPoints = 0, memPoints = 0, fpsPoints = 0;
    let pidCount = new Set();
    for (const s of this.sessions.values()) {
      events += s.eventCount;
      for (const [k, pts] of Object.entries(s.charts.cpu.series)) { cpuPoints += pts.length; pidCount.add(s.serial + ':' + k); }
      for (const pts of Object.values(s.charts.mem.series)) memPoints += pts.length;
      for (const pts of Object.values(s.charts.fps.series)) fpsPoints += pts.length;
    }
    document.title = `XPerformance | ok | devices:${this.sessions.size} | events:${events} | pids:${pidCount.size} | cpu:${cpuPoints},mem:${memPoints},fps:${fpsPoints}`;
  },

  // 同型号多机并存时 tab 附 serial 尾 4 位消歧（型号仅显示用途，serial 才是键）；
  // 设备增删/型号信息变化后重算并刷新全部 tab 标签
  refreshTabLabels() {
    const models = new Map();
    for (const s of this.sessions.values()) {
      if (s.info.model) models.set(s.info.model, (models.get(s.info.model) || 0) + 1);
    }
    for (const s of this.sessions.values()) {
      s.dupModel = s.info.model ? (models.get(s.info.model) || 0) > 1 : false;
      s.tabBtn.textContent = s.tabLabel();
      s.tabBtn.title = s.tabTitle();
    }
  },

  // 设备热插拔：新增建页；移除灰显（数据/采样线程保留，插回自动恢复重连）
  onDevicesChanged({ devices, added, removed }) {
    for (const d of devices) {
      if (this.sessions.has(d.serial)) {
        const s = this.sessions.get(d.serial);
        s.info = d;
        s.applyPlatformCaps();
        s.setOnline();
      }
    }
    for (const serial of added) {
      const info = devices.find(d => d.serial === serial) || { serial, model: '', version: '' };
      this.addDevice(info);
      _diag('devices added: ' + serial);
    }
    for (const serial of removed) {
      const s = this.sessions.get(serial);
      if (s) {
        s.setOffline();
        _diag('devices removed: ' + serial);
      }
    }
    this.refreshTabLabels();
    // 全部设备移除：回到无设备提示页
    const online = this.sessions.size > 0 && [...this.sessions.values()].some(s => !s.offline);
    document.getElementById('noDeviceHint').classList.toggle('hidden', online);
    if (added.length > 0) {
      const s = this.sessions.get(this.active);
      if (s && s.statusProgress === null) s.setStatus('设备接入: ' + added.join(', ') + '（顶栏切换设备页）');
    }
  },
};

// ---------- 全局事件（payload 带 serial 分发到设备会话） ----------
// sample 为批量事件（一轮节拍的多条合并一次 IPC）：payload.events 数组逐条分发
listen('sample', (e) => {
  const s = app.sessions.get(e.payload.serial);
  if (!s) return;
  for (const ev of e.payload.events || []) s.handleSample(ev);
});
listen('trace', (e) => {
  const s = app.sessions.get(e.payload.serial);
  if (s) s.handleTrace(e.payload);
});
listen('stack', (e) => {
  const s = app.sessions.get(e.payload.serial);
  if (s) s.handleStack(e.payload);
});
listen('sampling-error', (e) => {
  const s = app.sessions.get(e.payload.serial);
  if (s) s.handleSamplingError(e.payload.message);
});
// agent 构建进度事件（每设备；payload {serial, stage}——开发运行首次构建提示）
listen('agent-build', (e) => {
  const s = app.sessions.get(e.payload.serial);
  if (s) s.handleAgentBuild(e.payload.stage);
});
// 镜像进程退出事件（每设备；payload {serial, stage, message}）
listen('mirror', (e) => {
  const s = app.sessions.get(e.payload.serial);
  if (s) s.handleMirrorEvent(e.payload.stage, e.payload.message);
});
// 录屏进程退出事件（每设备；payload {serial, stage, message, path}）
listen('record', (e) => {
  const s = app.sessions.get(e.payload.serial);
  if (s) s.handleRecordEvent(e.payload.stage, e.payload.message, e.payload.path);
});
// logcat 行批量/错误事件（每设备；payload {serial, stage, lines|message}）
listen('logcat', (e) => {
  const s = app.sessions.get(e.payload.serial);
  if (s) s.handleLogcatEvent(e.payload.stage, e.payload.lines, e.payload.message);
});
listen('devices-changed', (e) => app.onDevicesChanged(e.payload));
// 火焰图脚本更新进度（全局操作，路由到激活设备的状态栏；进度条按完成文件数铺开）
listen('scripts-update', (e) => {
  const s = app.sessions.get(app.active);
  if (!s) return;
  const { stage, message } = e.payload;
  if (stage === 'progress') {
    // 消息形如 "3/7 report_html.js: 0.1 MB"（core 每完成一个文件回调一次）
    const m = message.match(/^(\d+)\/(\d+)\b/);
    if (m) {
      s.setStatusProgress(message, (parseInt(m[1], 10) / parseInt(m[2], 10)) * 100);
      return;
    }
  }
  s.setStatus(message); // done / 无进度格式：普通样式
});
_diag('event listeners registered');

// ---------- 主题（跟随系统[默认]/暗色/亮色） ----------
// 持久化：gui-settings.json 是权威来源（后端 save_gui_settings）；localStorage
// 仅存镜像供启动同步应用防闪烁。旧版 key `xperf-theme`（light/dark）一次性迁移。
const themeMedia = window.matchMedia('(prefers-color-scheme: light)');
let themeMode = (() => {
  const m = localStorage.getItem('xperf-theme-mode');
  if (m === 'system' || m === 'dark' || m === 'light') return m;
  const old = localStorage.getItem('xperf-theme'); // 旧版两态 key 迁移
  if (old === 'light' || old === 'dark') {
    localStorage.setItem('xperf-theme-mode', old);
    localStorage.removeItem('xperf-theme');
    return old;
  }
  return 'system';
})();
function effectiveTheme() {
  return themeMode === 'system' ? (themeMedia.matches ? 'light' : 'dark') : themeMode;
}
function applyTheme(mode) {
  themeMode = (mode === 'system' || mode === 'dark' || mode === 'light') ? mode : 'system';
  const eff = effectiveTheme();
  document.documentElement.dataset.theme = eff === 'light' ? 'light' : '';
  if (eff !== 'light') delete document.documentElement.dataset.theme;
  localStorage.setItem('xperf-theme-mode', themeMode);
  // 图表与实时面板取色跟随主题（所有设备页）
  for (const s of app.sessions.values()) {
    for (const c of s.allCharts) c.draw();
    s.renderLive();
  }
}
// system 模式下跟随系统亮暗实时切换
themeMedia.addEventListener('change', () => { if (themeMode === 'system') applyTheme('system'); });
// 启动先用 localStorage 镜像同步应用（防等待设置的闪烁）；随后 updateUI.init
// 读 gui-settings.json 对账（设置文件存在则以其为准）
applyTheme(themeMode);

// ---------- 字体档位（小/中[默认]/大，--font-scale 全局缩放 + canvas 图表同比例） ----------
function fontScale() {
  const v = parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--font-scale'));
  return Number.isFinite(v) ? v : 1;
}
function applyFontSize(size) {
  const mode = (size === 'small' || size === 'large') ? size : 'medium';
  document.documentElement.dataset.fontsize = mode;
  localStorage.setItem('xperf-font-size', mode);
  // canvas 图表文字不随 CSS，重绘时经 fontScale() 缩放
  for (const s of app.sessions.values()) {
    for (const c of s.allCharts) c.draw();
    s.renderLive();
  }
}
applyFontSize(localStorage.getItem('xperf-font-size') || 'medium');

// ---------- 周期渲染（仅激活页——隐藏页无渲染意义；全部按 dirty 标志跳过无变化工作） ----------
// 图表合帧：数据事件只标脏（requestDraw），150ms 统一绘制——极端配置（50ms×多 PID）
// 下每图最多 ~7 次/秒绘制（原实现每事件一次全量重绘，可达数百次/秒/图）
setInterval(() => {
  const s = app.sessions.get(app.active);
  if (!s) return;
  for (const c of s.allCharts) {
    if (c.dirty) { c.dirty = false; c.draw(); }
  }
}, 150);
// 面板（实时数值/Top 线程）：数据变了才重建 DOM
setInterval(() => {
  const s = app.sessions.get(app.active);
  if (!s) return;
  if (s.liveDirty) { s.liveDirty = false; s.renderLive(); }
  if (s.threadsDirty) { s.threadsDirty = false; s.renderThreads(); }
}, 500);
// 标题诊断统计：原每事件一次（O 全设备累计点数，长会话平方级），降为 5s 定时
setInterval(() => app.updateTitle(), 5000);

window.addEventListener('error', (e) => {
  document.title = 'XPerformance | ERROR: ' + (e.message || 'unknown');
});
window.addEventListener('resize', () => {
  const s = app.sessions.get(app.active);
  if (s) s.refreshChartSizes();
});

// ---------- SSH 远程后端：连接切换（顶栏 remoteBox，设计 §6.2） ----------
const remoteUI = {
  // 重建设备 tab：清空全部设备页（采样线程已被后端停止）→ 按新侧设备重建
  rebuildDevices(devices) {
    for (const s of app.sessions.values()) {
      s.root.remove();
      s.tabBtn.remove();
    }
    app.sessions.clear();
    app.active = null;
    for (const d of devices) app.addDevice(d);
    document.getElementById('noDeviceHint').classList.toggle('hidden', devices.length > 0);
    app.renderStatus();
  },

  async populate(currentHost) {
    const sel = document.getElementById('remoteSelect');
    let remotes = [];
    try { remotes = await invoke('list_remotes'); } catch (e) { _diag('list_remotes ERROR: ' + JSON.stringify(e)); }
    this.remotes = remotes; // 供 reopenForPassword / refreshImportList 查回填
    // 下拉只列已保存配置（remotes.json）——ssh config 主机不自动进入：其中混有
    // Gerrit/GitLab 等服务别名（如 29418 端口的 gerrit），无可靠判据区分「真机
    // 后端」，自动列出只会制造误选（2026-09-20 用户实测踩坑）。ssh config 主机
    // 的入口改为「＋」表单的「从 ssh config 导入」选择器（refreshImportList）。
    sel.innerHTML = '<option value="">本机</option>' +
      remotes.map(r => `<option value="${escHtml(r.host)}">${escHtml(r.name)}</option>`).join('');
    sel.value = currentHost || '';
    if (currentHost && sel.value !== currentHost) {
      // 未保存的临时目标（--remote 启动）：补一个选项显示
      const o = document.createElement('option');
      o.value = currentHost;
      o.textContent = currentHost;
      sel.appendChild(o);
      sel.value = currentHost;
    }
  },

  // 「＋」表单的「从 ssh config 导入」选择器：列出未保存过的 ssh config 主机
  // （alias → hostname），选中即填充全部字段（端口/用户名取 ssh -G 生效配置，
  // 含 Include/通配段合并——手动抄 config 拿不到这些）。
  async refreshImportList() {
    const sel = document.getElementById('rfImport');
    let details = [];
    try { details = await invoke('list_ssh_host_details'); } catch (e) { _diag('list_ssh_host_details ERROR: ' + JSON.stringify(e)); }
    this.sshDetails = details; // change 事件取结构化数据
    const saved = new Set((this.remotes || []).flatMap(r => [r.name, r.host]));
    const opts = details.filter(d => !saved.has(d.alias) && !saved.has(d.hostname));
    sel.innerHTML = '<option value="">' +
      (opts.length ? `从 ssh config 导入…（${opts.length} 台可选）` : '从 ssh config 导入…（无可用主机）') +
      '</option>' +
      opts.map(d => `<option value="${escHtml(d.alias)}">${escHtml(d.alias)} → ${escHtml(d.hostname)}</option>`).join('');
  },

  // 认证失败重试：预填表单（只欠输密码）。target 是 remotes.json 的
  // name/host（可全量预填）或临时目标（--remote 启动，无保存配置，
  // 仅预填目标本身）
  reopenForPassword(target) {
    const form = document.getElementById('remoteForm');
    const r = (this.remotes || []).find(r => r.name === target || r.host === target);
    if (r) {
      document.getElementById('rfName').value = r.name;
      document.getElementById('rfHost').value = r.host;
      document.getElementById('rfSshPort').value = r.ssh_port || 22;
    } else {
      document.getElementById('rfName').value = target;
      document.getElementById('rfHost').value = target;
      document.getElementById('rfSshPort').value = 22;
    }
    form.classList.remove('hidden');
    this.refreshImportList();
    document.getElementById('rfPassword').focus();
  },

  // 设备页侧栏「远程主机」区块同步：仅 SSH 远程模式显示，填充当前连接主机与
  // 已保存的 adb 设置（连接状态是全局单例，遍历所有设备页统一刷新——含
  // 热插拔新建页，addDevice 后也会调本方法幂等补齐）
  updateSidebars() {
    const host = this.connectedHost || null;
    document.querySelectorAll('.sidebar-remote').forEach(box => {
      box.classList.toggle('hidden', !host);
      if (!host) return;
      box.querySelector('.remote-host-label').textContent = host;
      const r = (this.remotes || []).find(x => x.host === host || x.name === host);
      box.querySelector('.remote-adb-input').value = r ? r.adb_path : '';
      box.querySelector('.remote-port-input').value = r ? r.remote_port : '';
    });
  },

  // 切换远程目标：host 为 remotes 条目（name/host）、ssh 别名或 `user@host` 临时目标。
  // sshPort 仅临时目标生效（无 remotes 配置时的非 22 端口；已保存配置以配置为准）
  async switchTo(host, password, sshPort) {
    const sel = document.getElementById('remoteSelect');
    sel.disabled = true;
    this.setGlobalStatus(host ? '正在连接 ' + host + '…' : '正在切回本机…');
    try {
      const r = await invoke('connect_remote', { host: host || null, password: password || null, sshPort: sshPort || null });
      this.rebuildDevices(r.devices || []);
      this.setGlobalStatus(host ? '已连接远程: ' + host : '本机');
      this.connectedHost = host || null;
      // 刷新下拉选中态（临时目标 populate 会补选项显示）；侧栏「远程主机」区块同步
      await this.populate(host || null);
      this.updateSidebars();
    } catch (e) {
      const msg = (e && e.toString()) || '连接失败';
      // 密码主机重连（密码不落盘 → 每个新会话都要重输一次）：表单预填重开，只欠输密码
      if (host && msg.includes('密码认证失败')) this.reopenForPassword(host);
      // init_remote 失败时 core 已保持本机传输；不要再次调用 connect_remote(null)，
      // 否则真实错误会被“已切回本机”覆盖，DMG/Finder 启动时尤其难以诊断。
      sel.value = '';
      this.connectedHost = null;
      this.updateSidebars();
      try {
        const r = await invoke('list_devices');
        this.rebuildDevices(r.devices || []);
      } catch (e2) { _diag('list local devices ERROR: ' + JSON.stringify(e2)); }
      this.setGlobalStatus('远程连接失败（当前仍为本机）: ' + msg);
    } finally {
      sel.disabled = false;
    }
  },

  setGlobalStatus(text) {
    const el = document.getElementById('status');
    el.classList.remove('progress');
    el.textContent = text;
  },

  async init() {
    const sel = document.getElementById('remoteSelect');
    // 当前状态（--remote 启动时后端已是远程）：同步选中项
    let currentHost = null;
    try {
      const st = await invoke('remote_status');
      if (st.mode === 'ssh') currentHost = st.host;
    } catch (e) { _diag('remote_status ERROR: ' + JSON.stringify(e)); }
    this.connectedHost = currentHost;
    await this.populate(currentHost);
    this.updateSidebars();

    sel.addEventListener('change', () => this.switchTo(sel.value || null));

    const form = document.getElementById('remoteForm');
    const addBtn = document.getElementById('remoteAddBtn');
    addBtn.addEventListener('click', () => {
      const opening = form.classList.contains('hidden');
      form.classList.toggle('hidden');
      // 每次打开刷新导入列表（已保存过滤 + ssh config 变更都实时反映）
      if (opening) this.refreshImportList();
    });
    document.getElementById('rfImport').addEventListener('change', (e) => {
      const d = (this.sshDetails || []).find(x => x.alias === e.target.value);
      if (!d) return;
      document.getElementById('rfName').value = d.alias;
      // 主机栏填别名（用户 ssh config 已有 Host 条目——连接自动继承其
      // HostName/User/Port/密钥/跳板配置，比展开成 user@IP 更完整）
      document.getElementById('rfHost').value = d.alias;
      document.getElementById('rfSshPort').value = d.port || 22;
    });
    // 双按钮：「连接」= 临时零保存；「保存并连接」= 写 remotes.json（名称必填）
    // 后连接。不写用户 ssh config（remotes.json 已含全部连接信息，工具不碰
    // 用户配置文件；ssh config 导入的主机以别名保存、连接时继承其配置）
    const nameInput = document.getElementById('rfName');
    document.getElementById('rfConnect').addEventListener('click', async () => {
      // 临时连接：不写 remotes.json，user@host 原样交给 connect_remote
      const hostRaw = document.getElementById('rfHost').value.trim();
      const sshPort = parseInt(document.getElementById('rfSshPort').value, 10) || 22;
      const password = document.getElementById('rfPassword').value;
      if (!hostRaw) { await alertBox('主机不能为空', 'warning'); return; }
      document.getElementById('remoteForm').classList.add('hidden');
      document.getElementById('rfPassword').value = '';
      await this.switchTo(hostRaw, password || null, sshPort);
    });
    document.getElementById('rfSave').addEventListener('click', async () => {
      let name = nameInput.value.trim();
      // 主机栏支持 user@host 合并格式（ssh 惯例）；无 @ 时用户名留空=当前用户
      const hostRaw = document.getElementById('rfHost').value.trim();
      const at = hostRaw.lastIndexOf('@');
      const user = at > 0 ? hostRaw.slice(0, at) : '';
      const host = at > 0 ? hostRaw.slice(at + 1) : hostRaw;
      const sshPort = parseInt(document.getElementById('rfSshPort').value, 10) || 22;
      const password = document.getElementById('rfPassword').value; // 不落盘，仅随连接走内存
      if (!name) { await alertBox('保存需要名称（remotes 配置名）', 'warning'); return; }
      // adb 设置不在本表单（仅 SSH 连接信息）：同名已有条目沿用，新增默认
      // adb/5037（预检自动探测远端 SDK 位置）；调整入口在设备页侧栏「远程主机」区块。
      // 已存条目的 host 是展开形式（user@host 或别名），按展开后形态比对防误覆盖
      const effectiveLookup = user ? `${user}@${host}` : host;
      const prev = (this.remotes || []).find(r => r.name === name || r.host === effectiveLookup);
      const adb = prev ? prev.adb_path : 'adb';
      const port = prev ? prev.remote_port : 5037;
      if (!host) { await alertBox('主机不能为空', 'warning'); return; }
      try {
        // 保存（remotes.json，不写用户 ssh config）→ 立即连接（密码随行，仅内存驻留）
        const effective = await invoke('add_ssh_host', {
          cfg: { name, host, user: user || null, ssh_port: sshPort,
                 adb_path: adb, remote_port: port },
        });
        form.classList.add('hidden');
        document.getElementById('rfPassword').value = '';
        await this.populate(null);
        await this.switchTo(effective, password || null);
      } catch (e) {
        await alertBox('保存失败: ' + e, 'error');
      }
    });

    // 隧道/连接状态事件 → 状态栏（切换中的 error 由 switchTo 自己呈现，不覆盖）
    listen('remote-status', (e) => {
      const { state, message } = e.payload;
      if (state === 'connecting' || state === 'error') return;
      this.setGlobalStatus(message || state);
    });
  },
};

// ---------- 问题反馈：全局浮层（侧栏「问题反馈」按钮唤起，收集+打包+上传 GitLab issue） ----------
const feedbackUI = {
  show() {
    document.getElementById('feedbackMask').classList.remove('hidden');
    document.getElementById('fbDesc').focus();
    this.refreshAuth();
  },
  hide() {
    document.getElementById('feedbackMask').classList.add('hidden');
  },
  // 身份行：PAT（env/文件）> OAuth 登录态 > 未配置（红字提示 + 登录按钮）
  async refreshAuth() {
    const identity = document.getElementById('fbIdentity');
    const loginBtn = document.getElementById('fbLoginBtn');
    const logoutBtn = document.getElementById('fbLogoutBtn');
    const authRow = document.querySelector('#feedbackForm .fb-auth');
    try {
      const st = await invoke('gitlab_auth_status');
      if (st.kind === 'pat') {
        identity.textContent = '将以 PAT（' + st.source + '）属主身份创建 issue（优先级高于 OAuth）';
        authRow.classList.remove('fb-warn');
        loginBtn.classList.add('hidden');
        logoutBtn.classList.add('hidden');
      } else if (st.kind === 'oauth') {
        identity.textContent = '将以 ' + st.name + '（@' + st.username + '）身份创建 issue';
        authRow.classList.remove('fb-warn');
        loginBtn.classList.add('hidden');
        logoutBtn.classList.remove('hidden');
      } else {
        identity.textContent = '未配置 GitLab 凭证——提交会失败；点击右侧「登录 GitLab」（浏览器授权一次）';
        authRow.classList.add('fb-warn');
        loginBtn.classList.remove('hidden');
        logoutBtn.classList.add('hidden');
      }
    } catch (e) {
      identity.textContent = '凭证状态查询失败: ' + e;
      _diag('gitlab_auth_status ERROR: ' + JSON.stringify(e));
    }
  },
  init() {
    const submitBtn = document.getElementById('fbSubmit');
    const loginBtn = document.getElementById('fbLoginBtn');
    // 顶栏「问题反馈」按钮（全局入口，收集范围不随设备页变化）
    document.getElementById('feedbackTopBtn').addEventListener('click', () => this.show());
    document.getElementById('fbCancel').addEventListener('click', () => this.hide());
    // 点击遮罩空白处关闭（点在表单内不关）
    document.getElementById('feedbackMask').addEventListener('click', (e) => {
      if (e.target.id === 'feedbackMask') this.hide();
    });
    // OAuth 登录：后端起回环监听并打开浏览器，promise 挂起至回调/超时
    loginBtn.addEventListener('click', async () => {
      loginBtn.disabled = true;
      document.getElementById('fbIdentity').textContent = '请在浏览器完成授权（10 分钟内）…';
      try {
        const who = await invoke('gitlab_login');
        _diag('gitlab_login OK: ' + who);
      } catch (e) {
        await alertBox('GitLab 登录失败: ' + (e && e.toString()), 'error');
        _diag('gitlab_login ERROR: ' + JSON.stringify(e));
      } finally {
        loginBtn.disabled = false;
        this.refreshAuth();
      }
    });
    document.getElementById('fbLogoutBtn').addEventListener('click', async () => {
      try {
        await invoke('gitlab_logout');
      } catch (e) {
        await alertBox('退出登录失败: ' + (e && e.toString()), 'error');
      }
      this.refreshAuth();
    });
    submitBtn.addEventListener('click', async () => {
      const desc = document.getElementById('fbDesc').value.trim();
      submitBtn.disabled = true;
      remoteUI.setGlobalStatus('问题反馈：正在收集并上传（最近 1 小时数据）…');
      _diag('feedback submit: desc=' + (desc || '(empty)'));
      try {
        const url = await invoke('submit_feedback', { description: desc });
        this.hide();
        document.getElementById('fbDesc').value = '';
        remoteUI.setGlobalStatus('反馈已提交: ' + url);
        _diag('feedback OK: ' + url);
      } catch (e) {
        const msg = (e && e.toString()) || '未知错误';
        await alertBox('反馈提交失败: ' + msg, 'error');
        _diag('feedback ERROR: ' + msg);
      } finally {
        submitBtn.disabled = false;
      }
    });
  },
};

// ---------- 版本更新检测（设置子菜单 + 顶栏徽标 + 模态） ----------
const updateUI = {
  release: null,  // 最近一次检测到的 ReleaseInfo（徽标点击后模态的数据源）

  show(rel) {
    this.release = rel;
    document.getElementById('updTag').textContent = rel.tag;
    document.getElementById('updMeta').textContent = '发布于 ' + (rel.released_at || '未知');
    document.getElementById('updDesc').textContent = rel.description || '（无 Release 说明）';
    const box = document.getElementById('updAssets');
    box.textContent = '';
    const assets = rel.assets || [];
    if (assets.length === 0) {
      box.textContent = '（无资产）';
    } else {
      for (const a of assets) {
        const div = document.createElement('div');
        div.textContent = a.name;
        div.title = a.url;
        box.appendChild(div);
      }
    }
    document.getElementById('updateMask').classList.remove('hidden');
  },
  hide() {
    document.getElementById('updateMask').classList.add('hidden');
  },
  showBadge(tag) {
    const b = document.getElementById('updateBadge');
    b.textContent = '🆕 ' + tag;
    b.classList.remove('hidden');
  },

  // 检测一次。manual=true（设置菜单手动触发）：状态栏给结论含失败提示；
  // manual=false（启动自动检测）：失败静默仅记 diag，有新版照常出徽标
  async run(manual) {
    try {
      const r = await invoke('check_update');
      if (!r.ok) {
        _diag('check_update failed: ' + r.error);
        if (manual) remoteUI.setGlobalStatus('检查更新失败: ' + r.error);
        return;
      }
      if (r.has_update === true) {
        this.release = r.release;
        this.showBadge(r.release.tag);
        remoteUI.setGlobalStatus('发现新版本 ' + r.release.tag + '（当前 ' + r.current + '），点顶栏徽标查看');
      } else if (manual) {
        remoteUI.setGlobalStatus(r.has_update === false
          ? '已是最新版本（' + r.current + '）'
          : '版本格式无法比较（最新 tag: ' + r.release.tag + '）');
      }
      _diag('check_update ok: latest=' + r.release.tag + ' has_update=' + r.has_update + (manual ? ' (manual)' : ''));
    } catch (e) {
      _diag('check_update ERROR: ' + JSON.stringify(e));
      if (manual) remoteUI.setGlobalStatus('检查更新失败: ' + e);
    }
  },

  async init() {
    // 设置子菜单开合（点菜单外任意处收起）
    const menu = document.getElementById('settingsMenu');
    document.getElementById('settingsBtn').addEventListener('click', (e) => {
      e.stopPropagation();
      menu.classList.toggle('hidden');
    });
    document.addEventListener('click', (e) => {
      if (!menu.classList.contains('hidden') && !document.getElementById('settingsBox').contains(e.target)) {
        menu.classList.add('hidden');
      }
    });
    // 设置项：主题（三态）+ 字体档位（小/中/大）+ 启动时自动检查更新（持久化
    // gui-settings.json）。对账：设置文件存在则以其为准（同步 localStorage
    // 镜像）；不存在则把当前生效值（含旧版 localStorage 迁移结果）写回设置文件。
    const autoCheck = document.getElementById('stAutoCheck');
    const themeSel = document.getElementById('stTheme');
    const fontSel = document.getElementById('stFontSize');
    const save = async () => {
      try {
        await invoke('save_gui_settings', { autoCheckUpdate: autoCheck.checked, theme: themeSel.value, fontSize: fontSel.value });
        _diag('gui settings saved: auto_check_update=' + autoCheck.checked + ' theme=' + themeSel.value + ' font_size=' + fontSel.value);
      } catch (e) {
        await alertBox('保存设置失败: ' + (e && e.toString()), 'error');
      }
    };
    try {
      const st = await invoke('get_gui_settings');
      autoCheck.checked = st.auto_check_update !== false;
      if (st.existed) {
        themeSel.value = st.theme;
        if (st.theme !== themeMode) applyTheme(st.theme);
        fontSel.value = st.font_size || 'medium';
        applyFontSize(fontSel.value);
      } else {
        themeSel.value = themeMode;
        fontSel.value = document.documentElement.dataset.fontsize || 'medium';
        save(); // 一次性落盘（含旧版 localStorage 迁移值）
      }
    } catch (e) {
      _diag('get_gui_settings ERROR: ' + JSON.stringify(e));
      themeSel.value = themeMode;
      fontSel.value = document.documentElement.dataset.fontsize || 'medium';
    }
    themeSel.addEventListener('change', () => {
      applyTheme(themeSel.value);
      save();
    });
    fontSel.addEventListener('change', () => {
      applyFontSize(fontSel.value);
      save();
    });
    autoCheck.addEventListener('change', save);
    document.getElementById('stCheckNow').addEventListener('click', () => {
      menu.classList.add('hidden');
      this.run(true);
    });
    // 徽标 → 模态；打开 Release 页；遮罩空白/关闭按钮收起
    document.getElementById('updateBadge').addEventListener('click', () => {
      if (this.release) this.show(this.release);
    });
    document.getElementById('updClose').addEventListener('click', () => this.hide());
    document.getElementById('updateMask').addEventListener('click', (e) => {
      if (e.target.id === 'updateMask') this.hide();
    });
    document.getElementById('updOpen').addEventListener('click', async () => {
      if (!this.release) return;
      try {
        await invoke('open_url', { url: this.release.url });
      } catch (e) {
        await alertBox('打开浏览器失败: ' + (e && e.toString()), 'error');
      }
    });
    // 启动自动检测（设置开启时；失败静默不打扰）
    if (autoCheck.checked) this.run(false);
  },
};

// ---------- 初始化：列设备建页 → 回填自动启动会话 → 动态窗口尺寸 ----------
(async function init() {
  feedbackUI.init();
  await updateUI.init();
  await remoteUI.init();
  try {
    const r = await invoke('list_devices');
    for (const d of r.devices) app.addDevice(d);
    document.getElementById('noDeviceHint').classList.toggle('hidden', r.devices.length > 0);
    _diag('init devices: ' + r.devices.map(d => d.serial).join(','));
  } catch (e) {
    _diag('list_devices ERROR: ' + JSON.stringify(e));
  }
  // --package --device 自动启动回填：startup_sessions 返回全部运行中会话
  try {
    const sessions = await invoke('startup_sessions');
    for (const [serial, args] of Object.entries(sessions || {})) {
      if (!app.sessions.has(serial)) {
        // 启动会话的设备可能已断开（快照晚于启动）：也建页，采样线程等设备回来
        app.addDevice({ serial, model: '', version: '' });
      }
      app.sessions.get(serial).applyStartupArgs(args)
        .catch((e) => _diag('[' + serial + '] applyStartupArgs ERROR: ' + (e && e.message ? e.message : JSON.stringify(e))));
    }
  } catch (e) {
    _diag('startup_sessions ERROR: ' + JSON.stringify(e));
  }
  // 默认窗口大小按屏幕动态设置（后端 resize_default：setup 阶段 webview 未就绪
  // 直接 set_size 会渲染空白，故等前端加载完成后调用）
  invoke('resize_default').catch((e) => _diag('resize_default ERROR: ' + e));
  _diag('init done');
})();

