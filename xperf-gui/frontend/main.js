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
  constructor(canvasId, title, unit) {
    this.canvas = document.getElementById(canvasId);
    this.ctx = this.canvas.getContext('2d');
    this.title = title;
    this.unit = unit;
    this.series = {}; // pid -> [{t, v}]
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
    if (this.series[pid].length > 600) this.series[pid].shift();
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
    // 计算范围
    let tMin = Infinity, tMax = -Infinity, vMax = 0;
    const pids = Object.keys(this.series);
    for (const pid of pids) {
      for (const p of this.series[pid]) {
        if (p.t < tMin) tMin = p.t;
        if (p.t > tMax) tMax = p.t;
        if (p.v > vMax) vMax = p.v;
      }
    }
    if (pids.length === 0 || !isFinite(tMin)) { this.drawAxes(L, T, W - R, H - B); return; }
    if (tMax - tMin < 1000) tMax = tMin + 1000;
    vMax = vMax * 1.1 || 1;
    // 网格 + Y 轴刻度
    ctx.strokeStyle = '#313244';
    ctx.fillStyle = '#6c7086';
    ctx.font = '12px system-ui, sans-serif';
    for (let i = 0; i <= 4; i++) {
      const y = T + (H - B - T) * i / 4;
      ctx.beginPath(); ctx.moveTo(L, y); ctx.lineTo(W - R, y); ctx.stroke();
      ctx.fillText((vMax * (1 - i / 4)).toFixed(0), 8, y + 4);
    }
    // X 轴时间刻度
    const span = tMax - tMin;
    for (let i = 0; i <= 4; i++) {
      const x = L + (W - R - L) * i / 4;
      const ts = new Date(tMin + span * i / 4);
      ctx.fillText(ts.toTimeString().slice(0, 8), x - 22, H - 10);
    }
    this.drawAxes(L, T, W - R, H - B);
    // 折线
    pids.forEach((pid, i) => {
      const pts = this.series[pid];
      if (pts.length < 1) return;
      ctx.strokeStyle = COLORS[i % COLORS.length];
      ctx.lineWidth = 2;
      ctx.beginPath();
      pts.forEach((p, j) => {
        const x = L + (W - R - L) * (p.t - tMin) / span;
        const y = T + (H - B - T) * (1 - p.v / vMax);
        j === 0 ? ctx.moveTo(x, y) : ctx.lineTo(x, y);
      });
      ctx.stroke();
      // 图例
      ctx.fillStyle = COLORS[i % COLORS.length];
      ctx.fillText('PID ' + pid, W - R - 70 - i * 70, 20);
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

const cpuChart = new LineChart('cpuChart', 'Process CPU (%)', '%');
const memChart = new LineChart('memChart', 'Memory Total PSS (KB)', 'KB');
_diag('charts initialized');

// pid -> { cpu, mem, new }
const pidData = {};

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
  } else if (ev.CpuUpdate) {
    const { pid, timestamp, process_cpu } = ev.CpuUpdate;
    if (!pidData[pid]) { pidData[pid] = { cpu: [], mem: [], new: true }; renderPidList(); }
    cpuChart.push(pid, new Date(timestamp).getTime(), +process_cpu.toFixed(2));
    try { cpuChart.draw(); } catch (err) { _diag('cpuChart.draw ERROR: ' + err.message); }
  } else if (ev.MemoryUpdate) {
    const { pid, timestamp, total_pss } = ev.MemoryUpdate;
    if (!pidData[pid]) { pidData[pid] = { cpu: [], mem: [], new: true }; renderPidList(); }
    memChart.push(pid, new Date(timestamp).getTime(), total_pss);
    try { memChart.draw(); } catch (err) { _diag('memChart.draw ERROR: ' + err.message); }
  } else if (ev.NoProcess) {
    document.getElementById('status').textContent = '无进程: ' + ev.NoProcess.error;
  }
  updateTitle();
});
_diag('listen(sample) registered');

document.getElementById('startBtn').addEventListener('click', async () => {
  _diag('startBtn CLICKED');
  const package = document.getElementById('package').value;
  const interval = parseInt(document.getElementById('interval').value, 10);
  const cpu = document.getElementById('cpu').checked;
  const memory = document.getElementById('memory').checked;
  for (const k of Object.keys(pidData)) delete pidData[k];
  renderPidList();
  cpuChart.series = {}; memChart.series = {};
  try {
    _diag('startBtn invoking start_sampling: pkg=' + package + ' interval=' + interval);
    await invoke('start_sampling', { package, interval, cpu, memory });
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

// ---- 图表随 CPU/Memory 勾选状态显示/隐藏 ----
function toggleCharts() {
  const cpuOn = document.getElementById('cpu').checked;
  const memOn = document.getElementById('memory').checked;
  document.getElementById('cpuChartBox').classList.toggle('hidden', !cpuOn);
  document.getElementById('memChartBox').classList.toggle('hidden', !memOn);
  // 容器显隐变化后 chart 尺寸需刷新
  setTimeout(() => { cpuChart.resize(); memChart.resize(); }, 50);
}
document.getElementById('cpu').addEventListener('change', toggleCharts);
document.getElementById('memory').addEventListener('change', toggleCharts);
toggleCharts();

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
