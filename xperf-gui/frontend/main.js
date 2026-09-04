// 加载即上报：区分 main.js 是否加载、各依赖是否就绪
document.title = 'XPerformance | boot:main.js';
const _diag = (m) => { try { window.__TAURI__.core.invoke('diag_log', { message: m }); } catch (e) { document.title = 'XPerformance | DIAG-FAIL: ' + m; } };
_diag('main.js loaded');

const { listen } = window.__TAURI__.event;
const { invoke } = window.__TAURI__.core;
_diag('__TAURI__ ok');

// ---------- 状态栏三态：普通文本 / 录制进度（绿色填充按百分比铺开） ----------
function setStatus(text) {
  const el = document.getElementById('status');
  el.classList.remove('progress');
  el.style.setProperty('--progress', '0%');
  el.textContent = text;
}
function setStatusProgress(text, pct) {
  const el = document.getElementById('status');
  el.classList.add('progress');
  el.style.setProperty('--progress', Math.max(0, Math.min(100, pct)) + '%');
  el.textContent = text;
}

// ---------- 轻量 Canvas 折线图（替代 ECharts，无外部依赖） ----------
// 主题色从 CSS 变量动态读取（data-theme 切换后 draw/redraw 自动跟随）
function uiColors() {
  const cs = getComputedStyle(document.documentElement);
  const v = (n) => cs.getPropertyValue(n).trim();
  const light = document.documentElement.dataset.theme === 'light';
  return {
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
}

class LineChart {
  constructor(canvasId, title, unit, maxValue) {
    this.canvas = document.getElementById(canvasId);
    this.ctx = this.canvas.getContext('2d');
    this.title = title;
    this.unit = unit;
    this.maxValue = maxValue; // Y 轴下限（如 CPU=100），实际值超出时自动扩展；undefined = 自适应
    this.series = {}; // pid -> [{t, v}]，完整会话历史（回看用），绘制时按窗口裁剪+抽稀
    this.windowMode = 'follow'; // follow=最近 followMs；all=全部历史
    this.followMs = 10 * 60 * 1000;
    this.resize();
    window.addEventListener('resize', () => this.resize());
  }
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
    this.draw();
  }
  push(pid, t, v) {
    if (!this.series[pid]) this.series[pid] = [];
    this.series[pid].push({ t, v });
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
    // 可见窗口内的 vMax（Y 轴）与绘制点数统计
    let vMax = 0;
    for (const pid of pids) {
      for (const p of this.series[pid]) {
        if (p.t >= tMin && p.v > vMax) vMax = p.v;
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
    // 折线：窗口外点跳过（时间有序，二分找起点）；可见点超过 2×像素宽时按 stride 抽稀
    let legendX = W - R;
    const plotW = W - R - L;
    pids.forEach((pid, i) => {
      const all = this.series[pid];
      if (all.length < 1) return;
      // 二分：第一个 t >= tMin 的下标
      let lo = 0, hi = all.length;
      while (lo < hi) { const mid = (lo + hi) >> 1; if (all[mid].t < tMin) lo = mid + 1; else hi = mid; }
      if (lo >= all.length) return;
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
    // 打点竖线（全局 markers 数组）
    if (window.markers && window.markers.length > 0) {
      const plotH = H - B - T;
      for (const m of window.markers) {
        const mt = m.timestamp;
        if (mt < tMin || mt > tMax) continue;
        const x = L + plotW * (mt - tMin) / span;
        ctx.strokeStyle = C.err;
        ctx.lineWidth = 1.5;
        ctx.setLineDash([4, 4]);
        ctx.beginPath();
        ctx.moveTo(x, T);
        ctx.lineTo(x, T + plotH);
        ctx.stroke();
        ctx.setLineDash([]);
        // 标签（右对齐，避免越界）
        ctx.fillStyle = C.err;
        ctx.font = '11px system-ui, sans-serif';
        const tw = ctx.measureText(m.label).width;
        ctx.fillText(m.label, Math.min(x + 4, W - R - tw), T - 4);
      }
    }
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

const cpuChart = new LineChart('cpuChart', 'Process CPU (%)', '%', 100);
const memChart = new LineChart('memChart', 'Memory Total PSS (KB)', 'KB');
const fpsChart = new LineChart('fpsChart', 'FPS', 'fps'); // 自适应纵轴（30/60/120 档各异）
const freqChart = new LineChart('freqChart', 'CPU Frequency (MHz)', 'MHz'); // 每核一条线
const tempChart = new LineChart('tempChart', 'Temperature (°C)', '°C'); // 每传感器一条线；thermal status 写入标题
const gpuChart = new LineChart('gpuChart', 'GPU Busy (%)', '%', 100);
const gpumemChart = new LineChart('gpumemChart', 'GPU Memory (MB)', 'MB'); // --gpu 降级路径（hypervisor 平台）：每 PID 一条
const ioChart = new LineChart('ioChart', 'IO (KB/s)', 'KB/s'); // 每 PID 读/写各一条
const netChart = new LineChart('netChart', 'Network 整机 (KB/s)', 'KB/s');
const allCharts = [cpuChart, memChart, fpsChart, freqChart, tempChart, gpuChart, gpumemChart, ioChart, netChart];
_diag('charts initialized');

// pid -> { cpu, mem, new }
const pidData = {};
// 峰值面板：pid -> { cpu: {v, t}, mem: {v, t} }，仅在新峰值出现时更新 DOM
const peaks = {};
// Top 线程面板：pid -> 最新一轮的线程数组（CpuUpdate 自带），500ms 节流渲染
const latestThreads = {};
// FPS 导出用 jank 记录：图层短名 -> [{t, fps, jank}]（图表 series 只存 fps 值）
const fpsHist = {};
// B 类指标导出用：temp 需要 status（图表 series 只存 °C）；io 需要 dr/dw；gpu 需要 mhz
const tempHist = {}; // 传感器名 -> [{t, v, status}]
const ioHist = {};   // pid -> [{t, r, w, dr, dw}]
const gpuHist = [];  // [{t, busy, mhz}]
const gpumemHist = {}; // pid -> [{t, mb, gmb}]（--gpu 保底路径）
const gpuprocHist = {}; // pid -> [{t, busy}]（--gpu QNX 路径每进程 busy%）
let maxkhz = [];     // AgentHello 带的每核最大频率（KHz）
window.markers = []; // 打点事件（{label, timestamp}），LineChart.draw() 画竖线

// ---- 实时数值面板：各指标最新值，500ms 节流渲染 ----
const liveData = {}; // key -> { label, value, unit, color（语义名，映射 CSS 变量） }
function setLive(key, label, value, unit, color) {
  liveData[key] = { label, value, unit, color: color || 'text' };
}
function renderLive() {
  const rows = Object.entries(liveData).map(([key, d]) =>
    `<tr><td>${d.label}</td><td style="color:var(--${d.color})">${d.value}${d.unit}</td></tr>`);
  document.getElementById('liveTable').innerHTML =
    '<tr><th>指标</th><th>当前值</th></tr>' + rows.join('');
}
setInterval(renderLive, 500);

function fmtTime(t) { return new Date(t).toTimeString().slice(0, 8); }

function renderPeaks() {
  const rows = Object.entries(peaks).map(([pid, p]) =>
    `<tr><td>${pid}</td>` +
    `<td>${p.cpu ? p.cpu.v.toFixed(1) : '-'}</td><td>${p.cpu ? fmtTime(p.cpu.t) : '-'}</td>` +
    `<td>${p.mem ? p.mem.v : '-'}</td><td>${p.mem ? fmtTime(p.mem.t) : '-'}</td></tr>`);
  document.getElementById('peakTable').innerHTML =
    '<tr><th>PID</th><th>峰值 CPU%</th><th>时间</th><th>峰值 PSS(KB)</th><th>时间</th></tr>' + rows.join('');
}

function trackPeak(pid, kind, v, t) {
  if (!peaks[pid]) peaks[pid] = {};
  if (!peaks[pid][kind] || v > peaks[pid][kind].v) {
    peaks[pid][kind] = { v, t };
    renderPeaks();
  }
}

function renderThreads() {
  const all = [];
  for (const [pid, threads] of Object.entries(latestThreads)) {
    for (const t of threads) all.push({ pid, tid: t.tid, name: t.name, cpu: t.cpu_usage });
  }
  all.sort((a, b) => b.cpu - a.cpu);
  const rows = all.slice(0, 8).map(t =>
    `<tr><td>${t.name}</td><td>${t.tid}</td><td>${t.pid}</td><td>${t.cpu.toFixed(1)}</td></tr>`);
  document.getElementById('threadTable').innerHTML =
    '<tr><th>线程</th><th>TID</th><th>PID</th><th>CPU%</th></tr>' + rows.join('');
}
setInterval(renderThreads, 500);

let eventCount = 0;
function updateTitle() {
  // 统计图表真实数据源
  let cpuPoints = 0, memPoints = 0, fpsPoints = 0;
  for (const s of Object.values(cpuChart.series)) cpuPoints += s.length;
  for (const s of Object.values(memChart.series)) memPoints += s.length;
  for (const s of Object.values(fpsChart.series)) fpsPoints += s.length;
  const pidCount = new Set([...Object.keys(cpuChart.series), ...Object.keys(memChart.series)]).size;
  document.title = `XPerformance | ok | events:${eventCount} | pids:${pidCount} | cpu:${cpuPoints},mem:${memPoints},fps:${fpsPoints}`;
  // 每 20 个事件上报一次图表数据积压（证明图表已累积数据点）
  if (eventCount % 20 === 0) {
    _diag(`frontend state: events=${eventCount} pids=${pidCount} cpuPoints=${cpuPoints} memPoints=${memPoints} fpsPoints=${fpsPoints} fpsBox=${document.getElementById('fps').checked} fpsHidden=${document.getElementById('fpsChartBox').classList.contains('hidden')}`);
  }
}
window.addEventListener('error', (e) => {
  document.title = 'XPerformance | ERROR: ' + (e.message || 'unknown');
});

function renderPidList() {
  const ul = document.getElementById('pidList');
  ul.innerHTML = '';
  for (const pid of Object.keys(pidData)) {
    const li = document.createElement('li');
    li.textContent = 'PID ' + pid + (pidData[pid].stopped ? ' (已停止)' : '');
    if (pidData[pid].new) { li.classList.add('new'); pidData[pid].new = false; }
    ul.appendChild(li);
  }
}

listen('sample', (e) => {
  const ev = e.payload;
  eventCount++;
  if (eventCount === 1) _diag('first sample event: ' + JSON.stringify(ev).slice(0, 150));
  if (ev.PidDiscovered) {
    if (!pidData[ev.PidDiscovered.pid]) {
      pidData[ev.PidDiscovered.pid] = { cpu: [], mem: [], new: true };
      renderPidList();
    }
  } else if (ev.PidDisappeared) {
    const pid = ev.PidDisappeared.pid;
    if (pidData[pid]) { pidData[pid].stopped = true; renderPidList(); }
    delete latestThreads[pid]; // 进程已死，Top 线程表不再展示其残留线程
  } else if (ev.CpuUpdate) {
    const { pid, timestamp, process_cpu, threads } = ev.CpuUpdate;
    if (!pidData[pid]) { pidData[pid] = { cpu: [], mem: [], new: true }; renderPidList(); }
    const t = new Date(timestamp).getTime();
    cpuChart.push('PID ' + pid, t, +process_cpu.toFixed(2));
    trackPeak(pid, 'cpu', process_cpu, t);
    latestThreads[pid] = threads;
    setLive('cpu', 'CPU (pid ' + pid + ')', process_cpu.toFixed(1), '%', 'accent');
    // 线程 top1
    if (threads.length > 0) {
      const top1 = threads.reduce((a, b) => a.cpu_usage > b.cpu_usage ? a : b);
      setLive('cpu_top', '  └ ' + top1.name, top1.cpu_usage.toFixed(1), '%', 'dim');
    }
    try { cpuChart.draw(); } catch (err) { _diag('cpuChart.draw ERROR: ' + err.message); }
  } else if (ev.MemoryUpdate) {
    const { pid, timestamp, total_pss, details } = ev.MemoryUpdate;
    if (!pidData[pid]) { pidData[pid] = { cpu: [], mem: [], new: true }; renderPidList(); }
    const t = new Date(timestamp).getTime();
    memChart.push('PID ' + pid, t, total_pss);
    trackPeak(pid, 'mem', total_pss, t);
    setLive('mem', '内存 PSS (pid ' + pid + ')', (total_pss / 1024).toFixed(1), ' MB', 'ok');
    if (details) {
      setLive('mem_native', '  └ Native', (details.native_heap / 1024).toFixed(1), ' MB', 'dim');
      setLive('mem_java', '  └ Java', (details.java_heap / 1024).toFixed(1), ' MB', 'dim');
      setLive('mem_code', '  └ Code', (details.code / 1024).toFixed(1), ' MB', 'dim');
    }
    try { memChart.draw(); } catch (err) { _diag('memChart.draw ERROR: ' + err.message); }
  } else if (ev.FpsUpdate) {
    const { pid, timestamp, layer, fps, jank_count } = ev.FpsUpdate;
    if (!pidData[pid]) { pidData[pid] = { cpu: [], mem: [], new: true }; renderPidList(); }
    // 自动启动带 --fps 时前端勾选框未同步，收到首个 FPS 事件自动展开图表
    const fpsBox = document.getElementById('fps');
    if (!fpsBox.checked) { fpsBox.checked = true; toggleCharts(); }
    // 多渲染面并存时逐图层一条折线（如游戏主 Surface + 相机预览），
    // key 用图层短名（截到最后一段，长名图例放不下）
    const shortLayer = layer.split('/').pop();
    const t = new Date(timestamp).getTime();
    fpsChart.push(shortLayer, t, +fps.toFixed(1));
    if (!fpsHist[shortLayer]) fpsHist[shortLayer] = [];
    fpsHist[shortLayer].push({ t, fps, jank: jank_count });
    setLive('fps', 'FPS (' + shortLayer + ')', fps.toFixed(1), '', 'err');
    setLive('fps_jank', '  └ Jank', jank_count, '', 'dim');
    try { fpsChart.draw(); } catch (err) { _diag('fpsChart.draw ERROR: ' + err.message); }
  } else if (ev.NoProcess) {
    // 被测进程死亡重扫期间与录制并行时，不要抹掉录制进度条（等下一秒 progress 事件会恢复，
    // 但期间空白更差）；录制态只更新右侧 PID 列表相关状态，status 保持
    const st = document.getElementById('status');
    if (!st.classList.contains('progress')) setStatus('无进程: ' + ev.NoProcess.error);
  } else if (ev.AgentHello) {
    maxkhz = ev.AgentHello.maxkhz || [];
  } else if (ev.FreqUpdate) {
    const { timestamp, khz } = ev.FreqUpdate;
    // 勾选框未同步（自动启动带 --freq）时自动展开
    autoCheck('freq');
    const t = new Date(timestamp).getTime();
    khz.forEach((k, i) => freqChart.push('cpu' + i, t, k / 1000)); // KHz → MHz
    // 频率展示：平均 + 最高核
    const mhz = khz.map(k => k / 1000);
    const avg = mhz.reduce((a, b) => a + b, 0) / mhz.length;
    setLive('freq', 'CPU 频率', avg.toFixed(0), ' MHz', 'warn');
    setLive('freq_max', '  └ 最高核', Math.max(...mhz).toFixed(0), ' MHz', 'dim');
    try { freqChart.draw(); } catch (err) { _diag('freqChart.draw ERROR: ' + err.message); }
  } else if (ev.TempUpdate) {
    const { timestamp, status, sensors } = ev.TempUpdate;
    autoCheck('thermal');
    const t = new Date(timestamp).getTime();
    tempChart.title = `Temperature (°C) — thermal status ${status}`;
    for (const [name, , value] of sensors) {
      tempChart.push(name, t, value);
      if (!tempHist[name]) tempHist[name] = [];
      tempHist[name].push({ t, v: value, status });
      setLive('temp_' + name, '温度 ' + name, value.toFixed(1), ' °C', 'orange');
    }
    setLive('temp_status', '  └ 热状态', status >= 0 ? status : '?', '', 'dim');
    try { tempChart.draw(); } catch (err) { _diag('tempChart.draw ERROR: ' + err.message); }
  } else if (ev.GpuUpdate) {
    const { timestamp, busy, util, mhz, maxmhz } = ev.GpuUpdate;
    autoCheck('gpu');
    const t = new Date(timestamp).getTime();
    // QNX 路径带 util/maxmhz（util = busy 按频率折算的利用率）；kgsl 路径 util=0
    gpuChart.title = maxmhz > 0 ? `GPU (%) — ${mhz}/${maxmhz} MHz` : `GPU Busy (%) — @ ${mhz} MHz`;
    gpuChart.push('busy', t, +busy.toFixed(2));
    if (util > 0 || maxmhz > 0) gpuChart.push('util', t, +util.toFixed(2));
    gpuHist.push({ t, busy, util, mhz });
    setLive('gpu_busy', 'GPU busy', busy.toFixed(1), '%', 'purple');
    if (maxmhz > 0) setLive('gpu_freq', '  └ 频率', mhz + '/' + maxmhz, ' MHz', 'dim');
    if (util > 0) setLive('gpu_util', '  └ util', util.toFixed(1), '%', 'dim');
    try { gpuChart.draw(); } catch (err) { _diag('gpuChart.draw ERROR: ' + err.message); }
  } else if (ev.GpuProcUpdate) {
    // QNX 路径：每进程 GPU busy%
    const { pid, timestamp, busy } = ev.GpuProcUpdate;
    if (!pidData[pid]) { pidData[pid] = { cpu: [], mem: [], new: true }; renderPidList(); }
    autoCheck('gpu');
    const t = new Date(timestamp).getTime();
    gpuChart.push('PID ' + pid, t, +busy.toFixed(2));
    if (!gpuprocHist[pid]) gpuprocHist[pid] = [];
    gpuprocHist[pid].push({ t, busy });
    setLive('gpu_proc_' + pid, 'GPU busy (pid ' + pid + ')', busy.toFixed(1), '%', 'purple');
    try { gpuChart.draw(); } catch (err) { _diag('gpuChart.draw ERROR: ' + err.message); }
  } else if (ev.GpuMemUpdate) {
    // --gpu 降级路径（hypervisor 平台）：每 PID GPU 显存
    const { pid, timestamp, bytes, global } = ev.GpuMemUpdate;
    if (!pidData[pid]) { pidData[pid] = { cpu: [], mem: [], new: true }; renderPidList(); }
    autoCheck('gpu');
    const t = new Date(timestamp).getTime();
    const mb = bytes / 1e6;
    gpumemChart.push('PID ' + pid, t, +mb.toFixed(1));
    gpumemChart.push('global', t, Math.round(global / 1e6));
    if (!gpumemHist[pid]) gpumemHist[pid] = [];
    gpumemHist[pid].push({ t, mb, gmb: global / 1e6 });
    setLive('gpumem_' + pid, 'GPU 显存 (pid ' + pid + ')', mb.toFixed(0), ' MB', 'teal');
    setLive('gpumem_global', '  └ 整机', (global / 1e6).toFixed(0), ' MB', 'dim');
    try { gpumemChart.draw(); } catch (err) { _diag('gpumemChart.draw ERROR: ' + err.message); }
  } else if (ev.IoUpdate) {
    const { pid, timestamp, r, w, dr, dw } = ev.IoUpdate;
    if (!pidData[pid]) { pidData[pid] = { cpu: [], mem: [], new: true }; renderPidList(); }
    autoCheck('io');
    const t = new Date(timestamp).getTime();
    ioChart.push(`PID ${pid} R`, t, +r.toFixed(2));
    ioChart.push(`PID ${pid} W`, t, +w.toFixed(2));
    if (!ioHist[pid]) ioHist[pid] = [];
    ioHist[pid].push({ t, r, w, dr, dw });
    setLive('io_' + pid, 'IO 读/写 (pid ' + pid + ')', r.toFixed(1) + ' / ' + w.toFixed(1), ' KB/s', 'orange');
    try { ioChart.draw(); } catch (err) { _diag('ioChart.draw ERROR: ' + err.message); }
  } else if (ev.NetUpdate) {
    const { timestamp, rx, tx } = ev.NetUpdate;
    autoCheck('net');
    const t = new Date(timestamp).getTime();
    netChart.push('RX', t, +rx.toFixed(2));
    netChart.push('TX', t, +tx.toFixed(2));
    setLive('net', '网络 RX/TX', rx.toFixed(1) + ' / ' + tx.toFixed(1), ' KB/s', 'sky');
    try { netChart.draw(); } catch (err) { _diag('netChart.draw ERROR: ' + err.message); }
  }
  updateTitle();
});
// 自动启动带的指标勾选框未同步：收到首个对应事件时自动勾上并展开图表
function autoCheck(id) {
  const box = document.getElementById(id);
  if (!box.checked) { box.checked = true; toggleCharts(); }
}
_diag('listen(sample) registered');

// ---- 打点：marker 事件（后端 add_marker 发射）→ 加入全局数组并重绘所有图表 ----
listen('marker', (e) => {
  const { label, timestamp } = e.payload;
  window.markers.push({ label, timestamp });
  for (const c of allCharts) c.draw();
  _diag('marker: ' + label);
});

// ---- 打点按钮：输入标签 → 调后端 add_marker → 追加 markers + 重绘 ----
document.getElementById('markerBtn').addEventListener('click', async () => {
  const input = document.getElementById('markerLabel');
  const label = input.value.trim() || '打点';
  input.value = '';
  await invoke('add_marker', { label });
  _diag('markerBtn: ' + label);
});

// ---- perfetto 分析（--trace）：录制 N 秒 → trace_processor SQL 归因，独立 tab 展示 ----
// trace 事件 stage: recording / recorded / done（message=完整报告）/ error；recorded/done/error 附 trace_path
let currentTracePath = null;
function switchTab(which) {
  // which: 'perf' / 'trace' / 'stack'（两个分析页均隐藏侧栏，报告占满全宽）
  const perf = which === 'perf';
  document.getElementById('perfContent').classList.toggle('hidden', !perf);
  document.getElementById('traceContent').classList.toggle('hidden', which !== 'trace');
  document.getElementById('stackContent').classList.toggle('hidden', which !== 'stack');
  document.getElementById('tabPerfBtn').classList.toggle('active', perf);
  document.getElementById('tabTraceBtn').classList.toggle('active', which === 'trace');
  document.getElementById('tabStackBtn').classList.toggle('active', which === 'stack');
  // 分析页隐藏左侧采样控制栏（报告占满全宽）；性能指标页恢复
  document.getElementById('sidebar').classList.toggle('hidden', !perf);
  // 图表容器显隐变化后尺寸需刷新
  if (perf) refreshChartSizes();
}
document.getElementById('tabPerfBtn').addEventListener('click', () => switchTab('perf'));
document.getElementById('tabTraceBtn').addEventListener('click', () => switchTab('trace'));
document.getElementById('tabStackBtn').addEventListener('click', () => switchTab('stack'));

// ---- 主题切换（暗/亮，localStorage 持久化） ----
function applyTheme(theme) {
  document.documentElement.dataset.theme = theme === 'light' ? 'light' : '';
  if (theme !== 'light') delete document.documentElement.dataset.theme;
  document.getElementById('themeBtn').textContent = theme === 'light' ? '☀ 暗色' : '☾ 亮色';
  localStorage.setItem('xperf-theme', theme);
  // 图表与实时面板取色跟随主题
  for (const c of allCharts) c.draw();
  renderLive();
}
document.getElementById('themeBtn').addEventListener('click', () => {
  applyTheme(document.documentElement.dataset.theme === 'light' ? 'dark' : 'light');
});
applyTheme(localStorage.getItem('xperf-theme') || 'dark');

listen('trace', (e) => {
  const { stage, message, trace_path } = e.payload;
  if (stage === 'progress') {
    // 进度态：绿色填充按已录制秒数比例铺开（message 如 "perfetto 录制中 3/8s"）
    const m = message.match(/(\d+)\/(\d+)s/);
    if (m) setStatusProgress(message, (parseInt(m[1], 10) / parseInt(m[2], 10)) * 100);
    _diag('trace: ' + message);
    return;
  }
  if (trace_path) {
    currentTracePath = trace_path;
    document.getElementById('openPerfBtn').disabled = false;
    document.getElementById('traceFileLabel').textContent = trace_path;
  }
  document.getElementById('traceReport').textContent = message;
  if (stage === 'recording') {
    // 命令行自动启动（--trace）时无 click handler 禁用，收到 recording 事件统一禁用
    document.getElementById('traceBtn').disabled = true;
    setStatus(message);
  } else if (stage === 'recorded') {
    // 录制完成进入分析阶段：退出进度态，避免进度条冻结在 100%"录制中"
    setStatus('Perfetto trace 已拉回，分析中…');
  } else if (stage === 'done') {
    document.getElementById('traceBtn').disabled = false;
    setStatus('Perfetto 分析完成');
    switchTab('trace');
  } else if (stage === 'error') {
    document.getElementById('traceBtn').disabled = false;
    setStatus('Perfetto 分析失败');
    switchTab('trace');
  }
  _diag('trace: ' + stage);
});

document.getElementById('traceBtn').addEventListener('click', async () => {
  const package = document.getElementById('package').value.trim();
  if (!package) {
    setStatus('请先填写包名');
    return;
  }
  const seconds = parseInt(document.getElementById('recordSeconds').value, 10) || 10;
  try {
    await invoke('start_trace', { package, seconds });
    document.getElementById('traceBtn').disabled = true;
    // 录制期间留在指标页观察实时曲线（采样与录制并行），完成/失败时自动切到分析页
    setStatus('Perfetto 分析录制中: ' + package);
    _diag('traceBtn: ' + package + ' ' + seconds + 's');
  } catch (err) {
    setStatus('Perfetto 分析错误: ' + err);
    _diag('traceBtn invoke ERROR: ' + JSON.stringify(err));
  }
});

// 打开浏览器 Perfetto UI 并自动加载 trace：本地镜像 UI + 同源深链（全自动）；
// 离线/镜像失败自动回退拖拽方式（后端 open_perfetto_ui 处理，msg 含结果说明）
document.getElementById('openPerfBtn').addEventListener('click', async () => {
  if (!currentTracePath) return;
  try {
    const msg = await invoke('open_perfetto_ui', { tracePath: currentTracePath });
    setStatus(msg);
    _diag('openPerfBtn: ' + msg);
  } catch (err) {
    setStatus('打开 Perfetto UI 失败: ' + err);
    _diag('openPerfBtn ERROR: ' + JSON.stringify(err));
  }
});

// ---- simpleperf 函数热点（--stack）：录制 N 秒调用栈 → 线程 CPU 分布 + 函数热点报告，独立 tab 展示 ----
// stack 事件 stage: recording / progress（每秒进度，只更新 status）/ recorded / done（message=完整报告）/
// error；recorded/done/error 附 data_path
let currentStackPath = null;
listen('stack', (e) => {
  const { stage, message, data_path } = e.payload;
  if (stage === 'progress') {
    // 进度态：绿色填充按已录制秒数比例铺开（message 如 "调用栈录制中 3/8s"）
    const m = message.match(/(\d+)\/(\d+)s/);
    if (m) setStatusProgress(message, (parseInt(m[1], 10) / parseInt(m[2], 10)) * 100);
    _diag('stack: ' + message);
    return;
  }
  if (data_path) {
    currentStackPath = data_path;
    document.getElementById('openStackBtn').disabled = false;
    document.getElementById('stackFileLabel').textContent = data_path;
  }
  document.getElementById('stackReport').textContent = message;
  if (stage === 'recording') {
    // 命令行自动启动（--stack）时无 click handler 禁用，收到 recording 事件统一禁用
    document.getElementById('stackBtn').disabled = true;
    setStatus(message);
  } else if (stage === 'recorded') {
    // 录制完成进入报告生成阶段：退出进度态，避免进度条冻结在 100%"录制中"
    setStatus('调用栈已拉回，生成报告中…');
  } else if (stage === 'done') {
    document.getElementById('stackBtn').disabled = false;
    setStatus('函数热点分析完成');
    switchTab('stack');
  } else if (stage === 'error') {
    document.getElementById('stackBtn').disabled = false;
    setStatus('函数热点分析失败');
    switchTab('stack');
  }
  _diag('stack: ' + stage);
});

document.getElementById('stackBtn').addEventListener('click', async () => {
  const package = document.getElementById('package').value.trim();
  if (!package) {
    setStatus('请先填写包名');
    return;
  }
  const seconds = parseInt(document.getElementById('recordSeconds').value, 10) || 10;
  try {
    await invoke('start_stack', { package, seconds });
    document.getElementById('stackBtn').disabled = true;
    // 录制期间留在指标页观察实时曲线（采样与录制并行），完成/失败时自动切到函数热点页
    setStatus('函数热点录制中: ' + package);
    _diag('stackBtn: ' + package + ' ' + seconds + 's');
  } catch (err) {
    setStatus('函数热点错误: ' + err);
    _diag('stackBtn invoke ERROR: ' + JSON.stringify(err));
  }
});

// 打开浏览器火焰图：report_html.py 渲染 .data 为单文件 HTML（首次自动下载 AOSP 脚本 ~10MB，
// 需 python3）；HTML 新于 .data 时复用不重渲染
document.getElementById('openStackBtn').addEventListener('click', async () => {
  if (!currentStackPath) return;
  try {
    const msg = await invoke('open_stack_html', { dataPath: currentStackPath });
    setStatus(msg);
    _diag('openStackBtn: ' + msg);
  } catch (err) {
    setStatus('打开火焰图失败: ' + err);
    _diag('openStackBtn ERROR: ' + JSON.stringify(err));
  }
});

// 清理缓存与采集数据：~/.cache/xperf（UI 镜像/脚本集，首次使用重新下载）+ /tmp/xperf
// （全部采集数据）。采样/录制进行中会丢当前会话产物——原生对话框确认
// （confirm() 在 webkit2gtk 下标题为 "Javascript-taurixxx"，不专业）
document.getElementById('cleanBtn').addEventListener('click', async () => {
  const ok = await window.__TAURI__.dialog.confirm(
    '将清理：\n- ~/.cache/xperf（UI 镜像/脚本集，首次使用会重新下载）\n- /tmp/xperf（全部采集数据：CSV/图表/trace/调用栈）\n\n正在采样/录制时当前会话产物会丢失，确认清理？',
    { title: 'XPerformance - 清理缓存与数据', kind: 'warning' }
  );
  if (!ok) return;
  try {
    const msg = await invoke('clean_cache');
    setStatus(msg);
    _diag('cleanBtn: ' + msg);
  } catch (err) {
    setStatus('清理失败: ' + err);
    _diag('cleanBtn ERROR: ' + JSON.stringify(err));
  }
});

document.getElementById('startBtn').addEventListener('click', async () => {
  _diag('startBtn CLICKED');
  const package = document.getElementById('package').value;
  const interval = parseInt(document.getElementById('interval').value, 10);
  const cpu = document.getElementById('cpu').checked;
  const memory = document.getElementById('memory').checked;
  const fps = document.getElementById('fps').checked;
  const freq = document.getElementById('freq').checked;
  const thermal = document.getElementById('thermal').checked;
  const gpu = document.getElementById('gpu').checked;
  const io = document.getElementById('io').checked;
  const net = document.getElementById('net').checked;
  for (const k of Object.keys(pidData)) delete pidData[k];
  for (const k of Object.keys(peaks)) delete peaks[k];
  for (const k of Object.keys(latestThreads)) delete latestThreads[k];
  for (const k of Object.keys(fpsHist)) delete fpsHist[k];
  for (const k of Object.keys(tempHist)) delete tempHist[k];
  for (const k of Object.keys(ioHist)) delete ioHist[k];
  for (const k of Object.keys(gpumemHist)) delete gpumemHist[k];
  for (const k of Object.keys(gpuprocHist)) delete gpuprocHist[k];
  for (const k of Object.keys(liveData)) delete liveData[k];
  window.markers = [];
  gpuHist.length = 0;
  renderPeaks();
  renderPidList();
  for (const c of allCharts) c.series = {};
  // 新会话：旧基线对比报告不再适用，隐藏清空
  document.getElementById('baselinePanel').classList.add('hidden');
  document.getElementById('baselineReport').textContent = '';
  try {
    _diag('startBtn invoking start_sampling: pkg=' + package + ' interval=' + interval);
    await invoke('start_sampling', { package, interval, cpu, memory, fps, freq, thermal, gpu, io, net });
    _diag('start_sampling RETURNED OK');
    samplingRunning = true;
    document.getElementById('startBtn').disabled = true;
    document.getElementById('stopBtn').disabled = false;
    document.getElementById('idleHint').classList.add('hidden');
    setStatus('监控中: ' + package);
  } catch (e) {
    setStatus('错误: ' + e);
    _diag('startBtn invoke ERROR: ' + JSON.stringify(e));
  }
});

document.getElementById('stopBtn').addEventListener('click', async () => {
  _diag('stopBtn CLICKED');
  await invoke('stop_sampling');
  samplingRunning = false;
  _diag('stop_sampling returned');
  document.getElementById('startBtn').disabled = false;
  document.getElementById('stopBtn').disabled = true;
  setStatus('已停止');
});
_diag('UI handlers bound');

// ---- 图表随勾选状态显示/隐藏 ----
// rAF 刷新图表尺寸：容器显隐后布局完成时机不确定（webkit2gtk 布局异步），
// 用双 rAF 保证在下一帧布局完成后取到正确 clientWidth/Height
function refreshChartSizes() {
  requestAnimationFrame(() => { for (const c of allCharts) c.resize(); });
}
function toggleCharts() {
  const pairs = [
    ['cpu', 'cpuChartBox'], ['memory', 'memChartBox'], ['fps', 'fpsChartBox'],
    ['freq', 'freqChartBox'], ['thermal', 'tempChartBox'], ['gpu', 'gpuChartBox'],
    ['gpu', 'gpumemChartBox'], // --gpu 保底路径（hypervisor 平台）的显存图与 busy 图同开关
    ['io', 'ioChartBox'], ['net', 'netChartBox'],
  ];
  for (const [id, boxId] of pairs) {
    document.getElementById(boxId).classList.toggle('hidden', !document.getElementById(id).checked);
  }
  // 线程数据来自 CpuUpdate：CPU 关闭时线程面板同步隐藏
  document.getElementById('threadPanel').classList.toggle('hidden', !document.getElementById('cpu').checked);
  // 容器显隐变化后 chart 尺寸需刷新
  refreshChartSizes();
}

// ---- 勾选即生效：监控运行中改指标勾选 → 重启采样会话应用新 flag 集 ----
let samplingRunning = false;
let restartTimer = null;
function currentFlags() {
  const g = (id) => document.getElementById(id).checked;
  return {
    package: document.getElementById('package').value,
    interval: parseInt(document.getElementById('interval').value, 10),
    cpu: g('cpu'), memory: g('memory'), fps: g('fps'), freq: g('freq'),
    thermal: g('thermal'), gpu: g('gpu'), io: g('io'), net: g('net'),
  };
}
async function restartSampling() {
  await invoke('stop_sampling');
  const f = currentFlags();
  _diag('restart sampling with flags: ' + JSON.stringify(f));
  await invoke('start_sampling', f);
  setStatus('监控中: ' + f.package);
}
function onMetricToggle() {
  toggleCharts();
  updateEffectiveRates();
  if (!samplingRunning) return;
  // 连续切换去抖 500ms，避免一次改多项时反复重启 agent
  if (restartTimer) clearTimeout(restartTimer);
  restartTimer = setTimeout(() => { restartTimer = null; restartSampling().catch((e) => _diag('restart ERROR: ' + e)); }, 500);
}
for (const id of ['cpu', 'memory', 'fps', 'freq', 'thermal', 'gpu', 'io', 'net']) {
  document.getElementById(id).addEventListener('change', onMetricToggle);
}
toggleCharts();

// ---- 实际采样周期展示：各指标限频不同，动态计算并显示 ----
function fmtPeriod(ms) {
  if (ms >= 1000) return (ms / 1000) + 's';
  return ms + 'ms';
}
function updateEffectiveRates() {
  const interval = parseInt(document.getElementById('interval').value, 10);
  // 与 agent 端限频逻辑一致（见 xperf-agent/main.rs）
  const metrics = [
    { id: 'cpu',      name: 'CPU',   period: interval },
    { id: 'memory',   name: '内存',   period: interval },
    { id: 'fps',      name: 'FPS',   period: Math.ceil(500 / interval) * interval },
    { id: 'freq',     name: '频率',   period: interval },
    { id: 'thermal',  name: '温度',   period: Math.ceil(2000 / interval) * interval },
    { id: 'gpu',      name: 'GPU',   period: Math.max(100, Math.min(interval, 1000)) },
    { id: 'io',       name: 'IO',    period: interval },
    { id: 'net',      name: '网络',   period: interval },
  ];
  const enabled = metrics.filter(m => document.getElementById(m.id).checked);
  if (enabled.length === 0) {
    document.getElementById('effectiveRates').innerHTML = '';
    return;
  }
  const parts = enabled.map(m => {
    const same = m.period === interval;
    return `<span class="${same ? 'rate-same' : 'rate-limited'}">${m.name}: ${fmtPeriod(m.period)}</span>`;
  });
  document.getElementById('effectiveRates').innerHTML =
    '<span class="rate-label">实际周期</span> ' + parts.join(' · ');
}
document.getElementById('interval').addEventListener('change', updateEffectiveRates);
updateEffectiveRates();

// ---- 时间窗口：跟随最新 / 全部历史 ----
document.getElementById('timeWindow').addEventListener('change', (e) => {
  const mode = e.target.value;
  for (const c of allCharts) { c.windowMode = mode; c.draw(); }
});

// ---- 会话数据收集（导出 CSV / 基线保存与对比共用：前端持有的完整会话历史）----
function collectSessionData() {
  const cpu = {}, mem = {}, fps = {}, freq = {}, temp = {}, io = {}, gpumem = {}, gpuproc = {};
  for (const [k, pts] of Object.entries(cpuChart.series)) cpu[k.replace('PID ', '')] = pts.map(p => [p.t, p.v]);
  for (const [k, pts] of Object.entries(memChart.series)) mem[k.replace('PID ', '')] = pts.map(p => [p.t, p.v]);
  for (const [layer, pts] of Object.entries(fpsHist)) fps[layer] = pts.map(p => [p.t, p.fps, p.jank]);
  for (const [core, pts] of Object.entries(freqChart.series)) freq[core] = pts.map(p => [p.t, p.v]);
  for (const [sensor, pts] of Object.entries(tempHist)) temp[sensor] = pts.map(p => [p.t, p.v, p.status]);
  for (const [pid, pts] of Object.entries(ioHist)) io[pid] = pts.map(p => [p.t, p.r, p.w, p.dr, p.dw]);
  for (const [pid, pts] of Object.entries(gpumemHist)) gpumem[pid] = pts.map(p => [p.t, p.mb, p.gmb]);
  for (const [pid, pts] of Object.entries(gpuprocHist)) gpuproc[pid] = pts.map(p => [p.t, p.busy]);
  const gpu = gpuHist.map(p => [p.t, p.busy, p.util, p.mhz]);
  const net = (netChart.series['RX'] || []).map((p, i) => [p.t, p.v, (netChart.series['TX'] || [])[i]?.v ?? 0]);
  return { cpu, mem, fps, freq, temp, gpu, io, net, gpumem, gpuproc };
}

// ---- 导出 CSV：发给后端写盘 ----
document.getElementById('exportBtn').addEventListener('click', async () => {
  const pkg = document.getElementById('package').value || 'unknown';
  const d = collectSessionData();
  try {
    const dir = await invoke('export_csv', { package: pkg, cpu: d.cpu, mem: d.mem, fps: d.fps, freq: d.freq, temp: d.temp, gpu: d.gpu, io: d.io, net: d.net, gpumem: d.gpumem, gpuproc: d.gpuproc });
    setStatus('已导出: ' + dir);
  } catch (e) {
    setStatus('导出失败: ' + e);
  }
});

// ---- 基线：保存 / 对比（与 CLI --save-baseline / --compare-baseline 同一基线文件，两侧互通）----
document.getElementById('saveBaselineBtn').addEventListener('click', async () => {
  const pkg = document.getElementById('package').value || 'unknown';
  const d = collectSessionData();
  const intervalMs = parseInt(document.getElementById('interval').value, 10) || 1000;
  try {
    const path = await invoke('save_baseline', { package: pkg, intervalMs, cpu: d.cpu, mem: d.mem, fps: d.fps, gpu: d.gpu, io: d.io, net: d.net });
    setStatus('基线已保存（覆盖旧基线）: ' + path);
  } catch (e) {
    setStatus('基线保存失败: ' + e);
  }
});
document.getElementById('compareBaselineBtn').addEventListener('click', async () => {
  const pkg = document.getElementById('package').value || 'unknown';
  const d = collectSessionData();
  const intervalMs = parseInt(document.getElementById('interval').value, 10) || 1000;
  try {
    const report = await invoke('compare_baseline', { package: pkg, intervalMs, cpu: d.cpu, mem: d.mem, fps: d.fps, gpu: d.gpu, io: d.io, net: d.net });
    document.getElementById('baselineReport').textContent = report;
    document.getElementById('baselinePanel').classList.remove('hidden');
    setStatus('基线对比完成');
  } catch (e) {
    setStatus('基线对比失败: ' + e);
  }
});

// ---- 设备选择（多台同连时所有 adb 命令带 -s 路由到选中设备）----
// 已有选择（--device 自动启动/切换后的 selected）不覆盖；无选择时默认第一台
// （多台时用户自行切换，切换即重选并刷新包列表）
async function loadDevices() {
  try {
    const r = await invoke('list_devices');
    const sel = document.getElementById('deviceSelect');
    sel.innerHTML = '';
    for (const d of r.devices) {
      const opt = document.createElement('option');
      opt.value = d.serial;
      opt.textContent = d.serial + (d.model ? '（' + d.model + '）' : '');
      sel.appendChild(opt);
    }
    const target = r.selected && r.devices.some(d => d.serial === r.selected)
      ? r.selected
      : (r.devices[0]?.serial || '');
    if (target) {
      sel.value = target;
      if (target !== r.selected) await invoke('select_device', { serial: target });
      _diag('device selected: ' + target);
    }
    return true;
  } catch (e) {
    _diag('list_devices ERROR: ' + JSON.stringify(e));
    return false;
  }
}
document.getElementById('deviceSelect').addEventListener('change', async (e) => {
  try {
    await invoke('select_device', { serial: e.target.value });
    _diag('device switched: ' + e.target.value);
    loadPackages();
  } catch (err) {
    setStatus('设备选择失败: ' + err);
  }
});

// ---- 采样启动失败（设备未选定多台同连/agent 构建部署启动失败）→ 前端可见 ----
listen('sampling-error', (e) => {
  setStatus('错误: ' + e.payload.message);
  document.getElementById('startBtn').disabled = false;
  document.getElementById('stopBtn').disabled = true;
  samplingRunning = false;
});

// ---- 包名列表（可搜索下拉）----
async function loadPackages() {
  try {
    const list = await invoke('list_packages');
    const dl = document.getElementById('pkgList');
    dl.innerHTML = '';
    for (const p of list) {
      const opt = document.createElement('option');
      opt.value = p;
      dl.appendChild(opt);
    }
    _diag('packages loaded: ' + list.length);
  } catch (e) {
    _diag('list_packages ERROR: ' + JSON.stringify(e));
  }
}
document.getElementById('refreshPkgs').addEventListener('click', loadPackages);
// 启动顺序：先选设备（list_devices 带出已生效的 --device 选择/默认第一台），
// 再加载包列表（list_packages 依赖目标设备已选定——多台同连不带 -s 会失败）
loadDevices().then(() => loadPackages());

// ---- 初始化同步按钮状态（自动启动时开始按钮应禁用）----
// --package 自动启动与手动「开始监控」走同一流程：回填包名/间隔/勾选、
// 隐藏 idle 引导、按钮状态与状态文案一致
invoke('startup_args').then((args) => {
  if (args) {
    samplingRunning = true;
    document.getElementById('package').value = args.package;
    if (args.interval) document.getElementById('interval').value = String(args.interval);
    if (args.flags) {
      const flagToBox = {
        cpu: 'cpu', memory: 'memory', fps: 'fps', freq: 'freq',
        thermal: 'thermal', gpu: 'gpu', io: 'io', net: 'net',
      };
      for (const [f, id] of Object.entries(flagToBox)) {
        if (args.flags[f]) document.getElementById(id).checked = true;
      }
      toggleCharts();
      updateEffectiveRates();
    }
    document.getElementById('idleHint').classList.add('hidden');
    document.getElementById('startBtn').disabled = true;
    document.getElementById('stopBtn').disabled = false;
    setStatus('监控中: ' + args.package);
  }
}).catch(() => {});
