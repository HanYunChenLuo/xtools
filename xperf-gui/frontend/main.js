// 加载即上报：区分 main.js 是否加载、各依赖是否就绪
document.title = 'XPerformance | boot:main.js';
const _diag = (m) => { try { window.__TAURI__.core.invoke('diag_log', { message: m }); } catch (e) { document.title = 'XPerformance | DIAG-FAIL: ' + m; } };
_diag('main.js loaded');

const { listen } = window.__TAURI__.event;
const { invoke } = window.__TAURI__.core;
_diag('__TAURI__ ok');

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
function pushCapped(arr, item) {
  if (arr.length >= 2 * SERIES_CAP) {
    let w = 0;
    for (let r = 0; r < arr.length; r += 2) arr[w++] = arr[r];
    arr.length = w;
  }
  arr.push(item);
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
    this.windowMode = 'follow'; // follow=最近 followMs；all=全部历史
    this.followMs = 10 * 60 * 1000;
    this.dirty = false; // 有待绘制数据（requestDraw 置位，flushCharts 统一绘制）
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
    if (!this.series[pid]) this.series[pid] = [];
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
    ctx.font = '500 14px system-ui, sans-serif';
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
    if (pids.length === 0 || !isFinite(tMax)) { this.drawAxes(L, T, W - R, H - B); return; }
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
    // Y 轴范围：固定下限（CPU=100，超出自动扩展）或自适应（内存，基于实际最大值）
    let yMax;
    if (this.maxValue !== undefined) {
      yMax = Math.max(this.maxValue, vMax * 1.1);
    } else {
      yMax = Math.max(1, vMax) * 1.1;
    }
    // 刻度标签精度：按 yMax 大小选位数，避免 toFixed(0) 四舍五入导致重复
    const labelPrec = yMax >= 100 ? 0 : (yMax >= 10 ? 1 : 2);
    // 网格 + Y 轴刻度
    ctx.strokeStyle = C.grid;
    ctx.fillStyle = C.dim;
    ctx.font = '12px system-ui, sans-serif';
    for (let i = 0; i <= 4; i++) {
      const y = T + (H - B - T) * i / 4;
      ctx.beginPath(); ctx.moveTo(L, y); ctx.lineTo(W - R, y); ctx.stroke();
      ctx.fillText((yMax * (1 - i / 4)).toFixed(labelPrec), 8, y + 4);
    }
    // X 轴时间刻度
    const span = tMax - tMin;
    for (let i = 0; i <= 4; i++) {
      const x = L + (W - R - L) * i / 4;
      const ts = new Date(tMin + span * i / 4);
      ctx.fillText(ts.toTimeString().slice(0, 8), x - 22, H - 10);
    }
    this.drawAxes(L, T, W - R, H - B);
    // 折线：窗口外点跳过（起点二分已预算）；可见点超过 2×像素宽时按 stride 抽稀
    let legendX = W - R;
    const plotW = W - R - L;
    rows.forEach(({ pid, all, lo }, i) => {
      const visLen = all.length - lo;
      const stride = Math.max(1, Math.ceil(visLen / (plotW * 2)));
      ctx.strokeStyle = C.series[i % C.series.length];
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
      ctx.fillStyle = C.series[i % C.series.length];
      legendX -= ctx.measureText(pid).width;
      ctx.fillText(pid, legendX, 20);
      legendX -= 14;
      ctx.fillRect(legendX, 11, 10, 10);
      legendX -= 10;
    });
  }
  drawAxes(l, t, r, b) {
    const { ctx } = this;
    ctx.strokeStyle = uiColors().border;
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.moveTo(l, t); ctx.lineTo(l, b); ctx.lineTo(r, b);
    ctx.stroke();
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
    this.offline = false;
    this.eventCount = 0;
    this.statusText = '未开始';
    this.statusProgress = null; // null=普通文案；数字=进度百分比
    this.currentTracePath = null;
    this.currentStackPath = null;
    this.restartTimer = null;
    this.mirrorRunning = false;

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
    const name = this.info.model || this.serial;
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
      this.setStatus('获取 root 失败: ' + (e && e.message ? e.message : e));
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
      this.setStatus('屏幕镜像失败: ' + (e && e.message ? e.message : e));
      _diag('[' + this.serial + '] mirrorBtn ERROR: ' + (e && e.message ? e.message : JSON.stringify(e)));
      this.mirrorRunning = false;
      btn.textContent = '屏幕镜像';
    }
    btn.disabled = false;
  }

  // 镜像进程退出（用户关窗/连接断开/stop 生效）：复位按钮
  handleMirrorEvent(stage, message) {
    if (stage !== 'exited') return;
    this.mirrorRunning = false;
    const btn = this.el('mirror-btn');
    btn.textContent = '屏幕镜像';
    btn.disabled = false;
    // message 为 scrcpy stderr 尾行（正常关窗为空；异常退出带诊断）
    this.setStatus(message ? '镜像异常退出: ' + message.split('\n').pop() : '镜像窗口已关闭');
    _diag('[' + this.serial + '] mirror exited: ' + (message || '(clean)'));
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

  // ---- 子 tab 切换（性能指标 / Perfetto / Simpleperf；侧栏公共模块三页常驻） ----
  switchTab(which) {
    const perf = which === 'perf';
    this.el('perf-content').classList.toggle('hidden', !perf);
    this.el('trace-content').classList.toggle('hidden', which !== 'trace');
    this.el('stack-content').classList.toggle('hidden', which !== 'stack');
    for (const t of this.els('subtab')) t.classList.toggle('active', t.dataset.tab === which);
    // 图表容器显隐变化后尺寸需刷新
    if (perf) this.refreshChartSizes();
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
    if (!pkg) { this.setStatus('请先填写包名'); return; }
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
      this.setStatus(`${kind}应用失败: ${e && e.message ? e.message : e}`);
      _diag(`[${this.serial}] ${kind}应用 ERROR: ` + (e && e.message ? e.message : JSON.stringify(e)));
    } finally {
      btn.disabled = false;
    }
  }

  // ---- 停止应用（am force-stop 清场） ----
  async stopApp() {
    const pkg = this.package();
    if (!pkg) { this.setStatus('请先填写包名'); return; }
    const btn = this.el('stop-app-btn');
    btn.disabled = true;
    this.setStatus(`停止应用中: ${pkg}…`);
    try {
      const msg = await invoke('stop_app', { serial: this.serial, package: pkg });
      this.setStatus(msg);
      _diag(`[${this.serial}] 停止应用: ${msg}`);
    } catch (e) {
      this.setStatus(`停止应用失败: ${e && e.message ? e.message : e}`);
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
    if (!pkg) { this.setStatus('请先填写包名'); return; }
    try {
      await invoke('start_trace', { serial: this.serial, package: pkg, seconds });
      this.setTraceButtons(true);
      this.setStatus('Perfetto 分析录制中: ' + pkg);
      _diag('[' + this.serial + '] traceBtn: ' + pkg + ' ' + seconds + 's');
    } catch (err) {
      this.setStatus('Perfetto 分析错误: ' + err);
      _diag('[' + this.serial + '] traceBtn invoke ERROR: ' + JSON.stringify(err));
    }
  }

  // 启动 simpleperf 录制+报告（分析页 toolbar 入口，同上）
  async startStackRecording(seconds) {
    const pkg = this.package();
    if (!pkg) { this.setStatus('请先填写包名'); return; }
    try {
      await invoke('start_stack', { serial: this.serial, package: pkg, seconds });
      this.setStackButtons(true);
      this.setStatus('Simpleperf 分析录制中: ' + pkg);
      _diag('[' + this.serial + '] stackBtn: ' + pkg + ' ' + seconds + 's');
    } catch (err) {
      this.setStatus('Simpleperf 错误: ' + err);
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
    this.setStatus('错误: ' + message);
    this.el('start-btn').disabled = false;
    this.el('stop-btn').disabled = true;
    this.samplingRunning = false;
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
    for (const c of this.allCharts) c.series = {};
    // 新会话：旧基线对比报告不再适用，隐藏清空
    this.el('panel-baseline').classList.add('hidden');
    this.el('baseline-report').textContent = '';
  }
  async start() {
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
      this.setStatus('监控中: ' + f.package);
    } catch (e) {
      this.setStatus('错误: ' + e);
      _diag('[' + this.serial + '] startBtn invoke ERROR: ' + JSON.stringify(e));
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
    this.setStatus('监控中: ' + f.package);
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
  applyStartupArgs(args) {
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
    this.setStatus('监控中: ' + args.package);
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
    // 离线/镜像失败自动回退拖拽方式（后端 open_perfetto_ui 处理，msg 含结果说明）
    this.el('open-perf-btn').addEventListener('click', async () => {
      if (!this.currentTracePath) return;
      try {
        const msg = await invoke('open_perfetto_ui', { tracePath: this.currentTracePath });
        this.setStatus(msg);
        _diag('[' + this.serial + '] openPerfBtn: ' + msg);
      } catch (err) {
        this.setStatus('打开 Perfetto UI 失败: ' + err);
        _diag('[' + this.serial + '] openPerfBtn ERROR: ' + JSON.stringify(err));
      }
    });

    // ---- Simpleperf 函数热点（分析页 toolbar 内录制） ----
    this.el('stack-start-btn').addEventListener('click', () => {
      this.startStackRecording(parseInt(this.el('stack-seconds').value, 10) || 10);
    });
    // 打开浏览器火焰图：report_html.py 渲染 .data 为单文件 HTML（首次自动下载 AOSP
    // 脚本 ~10MB，需 python3）；HTML 新于 .data 时复用不重渲染
    this.el('open-stack-btn').addEventListener('click', async () => {
      if (!this.currentStackPath) return;
      try {
        const msg = await invoke('open_stack_html', { dataPath: this.currentStackPath });
        this.setStatus(msg);
        _diag('[' + this.serial + '] openStackBtn: ' + msg);
      } catch (err) {
        this.setStatus('打开火焰图失败: ' + err);
        _diag('[' + this.serial + '] openStackBtn ERROR: ' + JSON.stringify(err));
      }
    });

    // ---- 数据管理（导出/基线/清理；基线与 CLI --save-baseline 同一文件互通） ----
    // 导出 = 复制后端流式落盘的会话 CSV 目录快照（全量数据在磁盘，无需前端回传）
    this.el('export-btn').addEventListener('click', async () => {
      try {
        const dir = await invoke('export_csv', { serial: this.serial });
        this.setStatus('已导出: ' + dir);
      } catch (e) {
        this.setStatus('导出失败: ' + e);
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
        this.setStatus('基线保存失败: ' + e);
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
        this.setStatus('基线对比失败: ' + e);
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
        this.setStatus('更新火焰图脚本失败: ' + err);
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
        this.setStatus('清理失败: ' + err);
        _diag('cleanBtn ERROR: ' + JSON.stringify(err));
      }
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
      s.tabBtn.textContent = s.tabLabel();
      s.tabBtn.title = s.tabTitle();
      return s;
    }
    const s = new DeviceSession(info.serial, info);
    this.sessions.set(info.serial, s);
    s.loadPackages();
    if (this.active === null) this.switchDevice(info.serial);
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

  // 设备热插拔：新增建页；移除灰显（数据/采样线程保留，插回自动恢复重连）
  onDevicesChanged({ devices, added, removed }) {
    for (const d of devices) {
      if (this.sessions.has(d.serial)) {
        const s = this.sessions.get(d.serial);
        s.info = d;
        s.applyPlatformCaps();
        s.setOnline();
        s.tabBtn.textContent = s.tabLabel();
        s.tabBtn.title = s.tabTitle();
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
// 镜像进程退出事件（每设备；payload {serial, stage, message}）
listen('mirror', (e) => {
  const s = app.sessions.get(e.payload.serial);
  if (s) s.handleMirrorEvent(e.payload.stage, e.payload.message);
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

// ---------- 主题切换（暗/亮，localStorage 持久化；全部设备页同步） ----------
function applyTheme(theme) {
  document.documentElement.dataset.theme = theme === 'light' ? 'light' : '';
  if (theme !== 'light') delete document.documentElement.dataset.theme;
  document.getElementById('themeBtn').textContent = theme === 'light' ? '☀ 暗色' : '☾ 亮色';
  localStorage.setItem('xperf-theme', theme);
  // 图表与实时面板取色跟随主题（所有设备页）
  for (const s of app.sessions.values()) {
    for (const c of s.allCharts) c.draw();
    s.renderLive();
  }
}
document.getElementById('themeBtn').addEventListener('click', () => {
  applyTheme(document.documentElement.dataset.theme === 'light' ? 'dark' : 'light');
});
applyTheme(localStorage.getItem('xperf-theme') || 'dark');

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
    let remotes = [], sshHosts = [];
    try { remotes = await invoke('list_remotes'); } catch (e) { _diag('list_remotes ERROR: ' + JSON.stringify(e)); }
    try { sshHosts = await invoke('list_ssh_hosts'); } catch (e) { _diag('list_ssh_hosts ERROR: ' + JSON.stringify(e)); }
    // 已保存配置优先；ssh_config 主机补充（跳过与已保存条目同 host/name 的）
    const savedKeys = new Set(remotes.flatMap(r => [r.host, r.name]));
    const extra = sshHosts.filter(h => !savedKeys.has(h));
    sel.innerHTML = '<option value="">本机</option>' +
      remotes.map(r => `<option value="${r.host}">${r.name}</option>`).join('') +
      extra.map(h => `<option value="${h}">${h}（ssh）</option>`).join('');
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

  async switchTo(host) {
    const sel = document.getElementById('remoteSelect');
    sel.disabled = true;
    this.setGlobalStatus(host ? '正在连接 ' + host + '…' : '正在切回本机…');
    try {
      const r = await invoke('connect_remote', { host: host || null });
      this.rebuildDevices(r.devices || []);
      this.setGlobalStatus(host ? '已连接远程: ' + host : '本机');
    } catch (e) {
      const msg = (e && e.toString()) || '连接失败';
      this.setGlobalStatus('连接失败: ' + msg);
      // 连接失败：后端已回落本机，下拉复位并重建本机设备 tab
      sel.value = '';
      try {
        const r = await invoke('connect_remote', { host: null });
        this.rebuildDevices(r.devices || []);
      } catch (e2) { _diag('fallback local ERROR: ' + JSON.stringify(e2)); }
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
    await this.populate(currentHost);

    sel.addEventListener('change', () => this.switchTo(sel.value || null));

    const form = document.getElementById('remoteForm');
    document.getElementById('remoteAddBtn').addEventListener('click', () => form.classList.toggle('hidden'));
    document.getElementById('rfSave').addEventListener('click', async () => {
      const name = document.getElementById('rfName').value.trim();
      const host = document.getElementById('rfHost').value.trim();
      const adb = document.getElementById('rfAdb').value.trim() || 'adb';
      const port = parseInt(document.getElementById('rfPort').value, 10) || 5037;
      if (!name || !host) { this.setGlobalStatus('远程配置：名称与 ssh 目标不能为空'); return; }
      try {
        await invoke('save_remote', { cfg: { name, host, adb_path: adb, remote_port: port } });
        form.classList.add('hidden');
        await this.populate(null);
        this.setGlobalStatus('已保存远程: ' + name);
      } catch (e) {
        this.setGlobalStatus('保存失败: ' + e);
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

// ---------- 初始化：列设备建页 → 回填自动启动会话 → 动态窗口尺寸 ----------
(async function init() {
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
      app.sessions.get(serial).applyStartupArgs(args);
    }
  } catch (e) {
    _diag('startup_sessions ERROR: ' + JSON.stringify(e));
  }
  // 默认窗口大小按屏幕动态设置（后端 resize_default：setup 阶段 webview 未就绪
  // 直接 set_size 会渲染空白，故等前端加载完成后调用）
  invoke('resize_default').catch((e) => _diag('resize_default ERROR: ' + e));
  _diag('init done');
})();

