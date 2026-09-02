// 加载即上报：区分 main.js 是否加载、各依赖是否就绪
document.title = 'XPerformance | boot:main.js';
const _diag = (m) => { try { window.__TAURI__.core.invoke('diag_log', { message: m }); } catch (e) { document.title = 'XPerformance | DIAG-FAIL: ' + m; } };
_diag('main.js loaded');

const { listen } = window.__TAURI__.event;
const { invoke } = window.__TAURI__.core;
_diag('__TAURI__ ok');

// ---------- 轻量 Canvas 折线图（替代 ECharts，无外部依赖） ----------
const COLORS = ['#89b4fa','#a6e3a1','#f9e2af','#f38ba8','#cba6f7','#94e2d5','#fab387','#74c7ec','#f5c2e7','#a6adc8'];

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
    const W = this.cssW, H = this.cssH;
    const L = 70, R = 20, T = 30, B = 30;
    ctx.clearRect(0, 0, W, H);
    // 背景
    ctx.fillStyle = '#1e1e2e';
    ctx.fillRect(0, 0, W, H);
    // 标题
    ctx.fillStyle = '#cdd6f4';
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
    ctx.strokeStyle = '#313244';
    ctx.fillStyle = '#6c7086';
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
      ctx.strokeStyle = COLORS[i % COLORS.length];
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
      ctx.fillStyle = COLORS[i % COLORS.length];
      legendX -= ctx.measureText(pid).width;
      ctx.fillText(pid, legendX, 20);
      legendX -= 14;
      ctx.fillRect(legendX, 11, 10, 10);
      legendX -= 10;
    });
  }
  drawAxes(l, t, r, b) {
    const { ctx } = this;
    ctx.strokeStyle = '#45475a';
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
  // 统计图表真实数据源（cpuChart.series / memChart.series）
  let cpuPoints = 0, memPoints = 0;
  for (const s of Object.values(cpuChart.series)) cpuPoints += s.length;
  for (const s of Object.values(memChart.series)) memPoints += s.length;
  const pidCount = new Set([...Object.keys(cpuChart.series), ...Object.keys(memChart.series)]).size;
  document.title = `XPerformance | ok | events:${eventCount} | pids:${pidCount} | cpu:${cpuPoints},mem:${memPoints}`;
  // 每 20 个事件上报一次图表数据积压（证明图表已累积数据点）
  if (eventCount % 20 === 0) {
    _diag(`frontend state: events=${eventCount} pids=${pidCount} cpuPoints=${cpuPoints} memPoints=${memPoints}`);
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
    try { cpuChart.draw(); } catch (err) { _diag('cpuChart.draw ERROR: ' + err.message); }
  } else if (ev.MemoryUpdate) {
    const { pid, timestamp, total_pss } = ev.MemoryUpdate;
    if (!pidData[pid]) { pidData[pid] = { cpu: [], mem: [], new: true }; renderPidList(); }
    const t = new Date(timestamp).getTime();
    memChart.push('PID ' + pid, t, total_pss);
    trackPeak(pid, 'mem', total_pss, t);
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
    try { fpsChart.draw(); } catch (err) { _diag('fpsChart.draw ERROR: ' + err.message); }
  } else if (ev.NoProcess) {
    document.getElementById('status').textContent = '无进程: ' + ev.NoProcess.error;
  } else if (ev.AgentHello) {
    maxkhz = ev.AgentHello.maxkhz || [];
  } else if (ev.FreqUpdate) {
    const { timestamp, khz } = ev.FreqUpdate;
    // 勾选框未同步（自动启动带 --freq）时自动展开
    autoCheck('freq');
    const t = new Date(timestamp).getTime();
    khz.forEach((k, i) => freqChart.push('cpu' + i, t, k / 1000)); // KHz → MHz
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
    }
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
    try { ioChart.draw(); } catch (err) { _diag('ioChart.draw ERROR: ' + err.message); }
  } else if (ev.NetUpdate) {
    const { timestamp, rx, tx } = ev.NetUpdate;
    autoCheck('net');
    const t = new Date(timestamp).getTime();
    netChart.push('RX', t, +rx.toFixed(2));
    netChart.push('TX', t, +tx.toFixed(2));
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
  gpuHist.length = 0;
  renderPeaks();
  renderPidList();
  for (const c of allCharts) c.series = {};
  try {
    _diag('startBtn invoking start_sampling: pkg=' + package + ' interval=' + interval);
    await invoke('start_sampling', { package, interval, cpu, memory, fps, freq, thermal, gpu, io, net });
    _diag('start_sampling RETURNED OK');
    document.getElementById('startBtn').disabled = true;
    document.getElementById('stopBtn').disabled = false;
    document.getElementById('status').textContent = '监控中: ' + package;
  } catch (e) {
    document.getElementById('status').textContent = '错误: ' + e;
    _diag('startBtn invoke ERROR: ' + JSON.stringify(e));
  }
});

document.getElementById('stopBtn').addEventListener('click', async () => {
  _diag('stopBtn CLICKED');
  await invoke('stop_sampling');
  _diag('stop_sampling returned');
  document.getElementById('startBtn').disabled = false;
  document.getElementById('stopBtn').disabled = true;
  document.getElementById('status').textContent = '已停止';
});
_diag('UI handlers bound');

// ---- 图表随勾选状态显示/隐藏 ----
function toggleCharts() {
  const pairs = [
    ['cpu', 'cpuChartBox'], ['memory', 'memChartBox'], ['fps', 'fpsChartBox'],
    ['freq', 'freqChartBox'], ['thermal', 'tempChartBox'], ['gpu', 'gpuChartBox'],
    ['gpu', 'gpumemChartBox'], // --gpu 降级路径（hypervisor 平台）的显存图与 busy 图同开关
    ['io', 'ioChartBox'], ['net', 'netChartBox'],
  ];
  for (const [id, boxId] of pairs) {
    document.getElementById(boxId).classList.toggle('hidden', !document.getElementById(id).checked);
  }
  // 线程数据来自 CpuUpdate：CPU 关闭时线程面板同步隐藏
  document.getElementById('threadPanel').classList.toggle('hidden', !document.getElementById('cpu').checked);
  // 容器显隐变化后 chart 尺寸需刷新
  setTimeout(() => { for (const c of allCharts) c.resize(); }, 50);
}
for (const id of ['cpu', 'memory', 'fps', 'freq', 'thermal', 'gpu', 'io', 'net']) {
  document.getElementById(id).addEventListener('change', toggleCharts);
}
toggleCharts();

// ---- 时间窗口：跟随最新 / 全部历史 ----
document.getElementById('timeWindow').addEventListener('change', (e) => {
  const mode = e.target.value;
  for (const c of allCharts) { c.windowMode = mode; c.draw(); }
});

// ---- 导出 CSV：把前端持有的完整会话历史发给后端写盘 ----
document.getElementById('exportBtn').addEventListener('click', async () => {
  const pkg = document.getElementById('package').value || 'unknown';
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
  try {
    const dir = await invoke('export_csv', { package: pkg, cpu, mem, fps, freq, temp, gpu, io, net, gpumem, gpuproc });
    document.getElementById('status').textContent = '已导出: ' + dir;
  } catch (e) {
    document.getElementById('status').textContent = '导出失败: ' + e;
  }
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
loadPackages();

// ---- 初始化同步按钮状态（自动启动时开始按钮应禁用）----
invoke('is_running').then((running) => {
  if (running) {
    document.getElementById('startBtn').disabled = true;
    document.getElementById('stopBtn').disabled = false;
    document.getElementById('status').textContent = '监控中';
  }
}).catch(() => {});
