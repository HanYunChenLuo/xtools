//! xperf-agent：设备端常驻采样器。
//!
//! 低间隔（<500ms）采样时主机侧 adb 轮询不可行（单轮多次 adb 调用的开销就超过
//! 间隔本身），因此把采样循环搬到设备上：直接读 /proc（微秒级），结果以 NDJSON
//! 行流式写 stdout，主机用 `adb exec-out` 一条长连接持续读取。
//!
//! 协议（每行一个 JSON 对象）：
//!   {"t":"hello","ncores":8,"version":1}
//!   {"t":"cpu","ts":<wall_ms>,"pid":29697,"cpu":15.4,"th":[[29697,"main",5.1],...]}
//!   {"t":"mem","ts":<wall_ms>,"pid":29697,"pss":484880,"rss":612000,
//!    "java":..,"native":..,"code":..,"stack":..,"gfx":..,"other":..,"sys":..}
//!   （内存分类字段仅 interval≥500ms 的 dumpsys meminfo 路径有值，低间隔 smaps_rollup 路径为 0）
//!   {"t":"fps","ts":<wall_ms>,"pid":29697,"layer":"SVM Container#0","fps":30.0,"frames":32,"jank":0}
//!   {"t":"freq","ts":<wall_ms>,"khz":[2592000,...]}              // 每核当前频率，下标对应 hello 的 maxkhz
//!   {"t":"io","ts":<wall_ms>,"pid":29697,"r":12.3,"w":4.5,"dr":0.0,"dw":1.2}  // KB/s；r/w=rchar/wchar 逻辑读写，dr/dw=read_bytes/write_bytes 磁盘读写
//!   {"t":"net","ts":<wall_ms>,"rx":123.4,"tx":56.7}              // KB/s 整机口径（聚合物理口，排除回环/隧道；per-app 无数据源）
//!   {"t":"gpu","ts":<wall_ms>,"busy":37.5,"mhz":585}             // kgsl 或 QNX 路径；QNX 路径多 "util"/"maxmhz" 字段
//!   {"t":"gpuproc","ts":<wall_ms>,"pid":29697,"busy":14.4}       // QNX 路径：每进程 GPU busy%
//!   {"t":"gpumem","ts":<wall_ms>,"pid":29697,"bytes":628928512,"global":2639089664}  // 保底路径：dumpsys gpu 每 PID GPU 显存
//!   {"t":"temp","ts":<wall_ms>,"status":0,"sensors":[["soc0",4,42.5],...]}    // status=Android ThermalStatus（-1=未知）；sensors=[名称,类型,°C]
//!   {"t":"exit","pid":29697}
//!   {"t":"noproc"}
//!   {"t":"err","msg":"..."}
//!   （空行）                                                    // 心跳：整轮零输出时探活（写失败=主机断连→agent 退出），host 侧 next_event 跳过
//!
//! 用法：xperf-agent --package <pkg> [--pid N]... --interval 50 [--cpu] [--memory] [--fps]
//!                   [--freq] [--io] [--net] [--gpu] [--thermal]
//!
//! 模块划分：proc（/proc 与 sysfs 读取 + CPU 采样状态）/ mem（内存）/ fps（SurfaceFlinger）/
//! thermal（温度）/ gpu（五通道）。本文件只保留参数解析、节拍主循环与公共输出工具。
//!
//! 与主机侧 adb 模式的差异：
//! - CPU 口径相同（jiffies 差值 ×核数，单核基准），但窗口是相邻两轮之间
//!   （agent 常驻保有上一轮状态，无需主机侧 phase1/phase2 结构）
//! - 内存：interval ≥ 500ms 用本地 dumpsys meminfo（全分类明细，同轮询模式）；
//!   低间隔改读 /proc/<pid>/smaps_rollup（Pss/Rss，~1ms）
//! - FPS：设备端本地 dumpsys SurfaceFlinger（无 adb 中转，图层名无需引号转义）；
//!   限频至 ≥500ms 周期（每 fps_every_n_rounds 轮一次），与 CPU/内存节拍解耦——
//!   低间隔下每轮跑 dumpsys SurfaceFlinger 会拖垮节拍（实测 50ms 间隔约半数轮次 overrun）

mod fps;
mod gpu;
mod mem;
mod proc;
mod thermal;

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fps::FpsState;
use gpu::GpuPath;
use proc::PidState;

struct Args {
    package: Option<String>,
    pids: Vec<u32>,
    interval_ms: u64,
    cpu: bool,
    memory: bool,
    fps: bool,
    freq: bool,
    io: bool,
    net: bool,
    gpu: bool,
    thermal: bool,
    /// 平台提示（ss2max/ss2pro/ss3/ss4/android），跳过运行时探测
    platform: Option<String>,
    /// QNX host telnet IP（覆盖默认 172.31.101.52，由 host 侧平台检测传入）
    qnx_host: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut package = None;
    let mut pids = Vec::new();
    let mut interval_ms = 50u64;
    let mut cpu = false;
    let mut memory = false;
    let mut fps = false;
    let mut freq = false;
    let mut io = false;
    let mut net = false;
    let mut gpu = false;
    let mut thermal = false;
    let mut platform = None;
    let mut qnx_host = None;
    let argv: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < argv.len() {
        match argv[i].as_str() {
            "--package" => {
                i += 1;
                package = Some(argv.get(i).ok_or("--package 缺参数")?.clone());
            }
            "--pid" => {
                i += 1;
                pids.push(
                    argv.get(i)
                        .and_then(|s| s.parse().ok())
                        .ok_or("--pid 参数非法")?,
                );
            }
            "--interval" => {
                i += 1;
                interval_ms = argv
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .ok_or("--interval 参数非法")?;
            }
            "--cpu" => cpu = true,
            "--memory" => memory = true,
            "--fps" => fps = true,
            "--freq" => freq = true,
            "--io" => io = true,
            "--net" => net = true,
            "--gpu" => gpu = true,
            "--thermal" => thermal = true,
            "--platform" => {
                i += 1;
                platform = Some(argv.get(i).ok_or("--platform 缺参数")?.clone());
            }
            "--qnx-host" => {
                i += 1;
                qnx_host = Some(argv.get(i).ok_or("--qnx-host 缺参数")?.clone());
            }
            other => return Err(format!("未知参数: {}", other)),
        }
        i += 1;
    }
    if package.is_none() && pids.is_empty() {
        return Err("需要 --package 或 --pid".into());
    }
    if !(cpu || memory || fps || freq || io || net || gpu || thermal) {
        return Err("需要至少一个采样开关（--cpu/--memory/--fps/--freq/--io/--net/--gpu/--thermal）".into());
    }
    if interval_ms < 50 {
        return Err("--interval 最小 50ms（更低会撞上 jiffies 粒度（10ms）且采样开销占比过高）".into());
    }
    Ok(Args { package, pids, interval_ms, cpu, memory, fps, freq, io, net, gpu, thermal, platform, qnx_host })
}

// ---------- 输出与公共工具（crate 根私有项对所有子模块可见）----------

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// JSON 字符串转义：处理引号、反斜杠和控制字符（\n \r \t 等会破坏 NDJSON 行帧结构）
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < '\x20' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// 本轮（节拍循环或任一 GPU 读线程）是否已有输出。
/// 用于心跳：整轮零输出时发一个空行探活——host 的 next_event 跳过空行（零协议影响），
/// 而 stdout 写失败（主机断连）会在 emit 内 exit(0)，agent 不再残留设备。
static ROUND_EMITTED: AtomicBool = AtomicBool::new(false);

fn emit(line: &str) {
    ROUND_EMITTED.store(true, Ordering::Relaxed);
    let mut out = std::io::stdout().lock();
    // 对端断开（adb 连接关闭）时写失败，直接退出
    if writeln!(out, "{}", line).is_err() || out.flush().is_err() {
        std::process::exit(0);
    }
}

fn dumpsys(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("dumpsys").args(args).output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// QNX/topgpu/ligfx GPU 通道的进程行按 comm 名归因到 Android PID：
/// 重建 name(comm) → pid 映射（启动/重扫时调用）。
fn fill_pid_names(map: &Mutex<HashMap<String, u32>>, pids: &[u32]) {
    let mut m = map.lock().unwrap();
    m.clear();
    for &pid in pids {
        if let Ok(c) = fs::read_to_string(format!("/proc/{}/comm", pid)) {
            m.insert(c.trim().to_string(), pid);
        }
    }
}

/// FPS 兜底匹配需要的包名：--package 直接用；--pid 模式从 /proc/<pid>/cmdline 反查（一次性缓存）
fn package_of(args: &Args, pkg_cache: &mut HashMap<u32, String>, pid: u32) -> String {
    match &args.package {
        Some(p) => p.clone(),
        None => pkg_cache
            .entry(pid)
            .or_insert_with(|| {
                fs::read_to_string(format!("/proc/{}/cmdline", pid))
                    .map(|s| s.trim_end_matches('\0').to_string())
                    .unwrap_or_default()
            })
            .clone(),
    }
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(2);
        }
    };

    let Some((mut prev_total, ncores)) = proc::read_total_jiffies() else {
        eprintln!("无法读取 /proc/stat");
        std::process::exit(1);
    };
    // 每核最大频率（KHz），与 freq 事件的 khz 数组下标对应
    let maxkhz: Vec<u64> = (0..ncores).map(|i| proc::read_cpufreq(i, "cpuinfo_max_freq").unwrap_or(0)).collect();
    // QNX 地址：--qnx-host 参数覆盖默认值
    if let Some(ref host) = args.qnx_host {
        gpu::set_qnx_host(host);
    }
    emit(&format!(
        "{{\"t\":\"hello\",\"ncores\":{},\"maxkhz\":[{}],\"version\":1}}",
        ncores,
        maxkhz.iter().map(u64::to_string).collect::<Vec<_>>().join(",")
    ));

    // B 类指标启动探测（探测失败发 err 并禁用，不影响其余指标）
    let mut freq_enabled = args.freq;
    if freq_enabled && maxkhz.iter().all(|&f| f == 0) && proc::read_cpu_freqs(ncores).iter().all(|&f| f == 0) {
        emit("{\"t\":\"err\",\"msg\":\"cpufreq sysfs 不可用，--freq 已禁用\"}");
        freq_enabled = false;
    }
    let gpu_path = if args.gpu {
        let g = gpu::detect_gpu_path_ex(args.platform.as_deref());
        match &g {
            None => emit("{\"t\":\"err\",\"msg\":\"GPU 数据源均不可用，--gpu 已禁用\"}"),
            Some(GpuPath::Qnx) => emit("{\"t\":\"err\",\"msg\":\"--gpu 走 QNX host 通道（真利用率 + 每进程 busy + 频率）\"}"),
            Some(GpuPath::TopGpu) => emit("{\"t\":\"err\",\"msg\":\"--gpu 走 topgpu 工具通道（SS2 平台）\"}"),
            Some(GpuPath::Ligfx) => emit("{\"t\":\"err\",\"msg\":\"--gpu 走 ligfxprofilerd logcat 通道（SS4 平台）\"}"),
            Some(GpuPath::DumpMem) => emit("{\"t\":\"err\",\"msg\":\"--gpu 降级为每 PID GPU 显存（dumpsys gpu）\"}"),
            Some(GpuPath::Kgsl(_)) => {}
        }
        g
    } else {
        None
    };

    // 流式 GPU 通道（QNX/TopGpu/Ligfx）的进程行按 comm 名归因到 Android PID，
    // 映射表读线程/主循环共享（重扫时更新）。
    let pid_names: Arc<Mutex<HashMap<String, u32>>> = Arc::new(Mutex::new(HashMap::new()));

    let mut states: HashMap<u32, PidState> = HashMap::new();
    let mut fps_states: HashMap<u32, FpsState> = HashMap::new();
    // 速率类指标的上一轮基线：(上轮时间戳 ms, 计数器...)
    let mut io_states: HashMap<u32, (u64, u64, u64, u64, u64)> = HashMap::new(); // pid → (ts, rchar, wchar, read_bytes, write_bytes)
    let mut prev_net: Option<(u64, u64, u64)> = None; // (ts, rx_bytes, tx_bytes)
    let mut prev_gpu: Option<(u64, u64)> = None; // (busy_time, total_time)
    let mut active_pids: Vec<u32> = args.pids.clone();

    // 内存分类明细仅在间隔 ≥500ms 时启用（dumpsys meminfo ~100ms，低间隔下太重）
    let full_meminfo = args.memory && args.interval_ms >= 500;
    // --pid 模式下 FPS 兜底匹配需要包名：从 /proc/<pid>/cmdline 反查（一次性缓存）
    let mut pkg_cache: HashMap<u32, String> = HashMap::new();

    // FPS 预热：图层发现的全量 dumpsys SurfaceFlinger 在此车机 ~1.5s，
    // 放在节拍时钟开始前执行，避免首轮 backlog、后续追帧期 CPU 窗口不齐。
    // （进程尚未启动时此处无 PID，发现会推迟到循环内首次 FPS 轮，代价同上但仅一次）
    if args.fps {
        if let Some(pkg) = &args.package {
            for pid in proc::resolve_pids(pkg) {
                if !active_pids.contains(&pid) {
                    active_pids.push(pid);
                }
            }
        }
        for &pid in &active_pids.clone() {
            let pkg = package_of(&args, &mut pkg_cache, pid);
            // 首轮仅建图层列表 + 帧时间戳基线，不出数
            fps_states.entry(pid).or_default().sample_round(pid, &pkg, now_ms());
        }
    }

    // 绝对节拍：按起始时间推算每轮时刻，避免 sleep 累积漂移
    let interval = Duration::from_millis(args.interval_ms);
    // 流式 GPU 通道读线程启动（QNX/TopGpu/Ligfx；kgsl/DumpMem 由主循环轮询）。
    // 先填进程名归因映射，保证读线程首批进程行即可归因。
    if let Some(g) = &gpu_path {
        if matches!(g, GpuPath::Qnx | GpuPath::TopGpu | GpuPath::Ligfx) {
            fill_pid_names(&pid_names, &active_pids);
        }
        gpu::start_stream_channels(g, args.interval_ms, &pid_names);
    }
    let start = Instant::now();
    let mut round: u64 = 0;
    // 进程列表重扫间隔：约 1s 一次（低间隔下每轮扫 /proc 太贵）
    let rescan_rounds = (1000 / args.interval_ms).max(1);
    // FPS 限频：与 CPU/内存节拍解耦，有效周期 ≥500ms（50ms 间隔 → 每 10 轮一次）
    let fps_every = fps::fps_every_n_rounds(args.interval_ms);
    // 温度限频：dumpsys thermalservice ~50ms 级，≥2s 一轮（温度变化慢；低间隔下避免频繁拖长节拍轮）
    let thermal_every = 2000u64.div_ceil(args.interval_ms).max(1);
    let mut thermal_warned = false;
    let mut io_warned = false;
    // GPU 显存降级路径限频：dumpsys gpu ~11ms，≥1s 一轮
    let gpumem_every = 1000u64.div_ceil(args.interval_ms).max(1);

    loop {
        round += 1;
        // 心跳：上一轮（含 GPU 读线程）完全零输出时发一个空行探活。emit("") 会置位
        // 标志故随即复位；读线程在此间隙的输出至多让下一轮多一个空行，host 跳过空行无害。
        if round >= 2 && !ROUND_EMITTED.swap(false, Ordering::Relaxed) {
            emit("");
            ROUND_EMITTED.store(false, Ordering::Relaxed);
        }
        // 绝对节拍 sleep：本轮目标时刻 = start + round * interval
        let target = start + interval * round as u32;
        let now = Instant::now();
        if target > now {
            std::thread::sleep(target - now);
        } else {
            // 本轮处理已超间隔（设备太忙），打印告警行
            emit(&format!("{{\"t\":\"err\",\"msg\":\"round {} overrun by {}ms\"}}", round, (now - target).as_millis()));
        }

        // 周期性按包名重扫进程（动态跟随新 PID）
        if args.package.is_some() && (active_pids.is_empty() || round.is_multiple_of(rescan_rounds)) {
            let pkg = args.package.as_deref().unwrap();
            let found = proc::resolve_pids(pkg);
            for pid in &found {
                if !active_pids.contains(pid) {
                    active_pids.push(*pid); // 新 PID 首轮建基线（states 中无记录）
                }
            }
            // 消失的 PID：重扫剔除前先发 exit 事件（避免 per-pid 循环检测窗口竞态丢失）
            let exited: Vec<u32> = active_pids.iter().filter(|p| !found.contains(p)).copied().collect();
            for pid in &exited {
                emit(&format!("{{\"t\":\"exit\",\"pid\":{}}}", pid));
            }
            active_pids.retain(|p| found.contains(p));
            if found.is_empty() {
                // 每轮重扫都报：让主机读循环在无进程期间也能定期收到行（保持 Ctrl-C 响应）
                emit("{\"t\":\"noproc\"}");
            }
            // 流式 GPU 通道：进程行按 comm 名归因，重扫时同步映射表
            if matches!(gpu_path, Some(GpuPath::Qnx) | Some(GpuPath::TopGpu) | Some(GpuPath::Ligfx)) {
                fill_pid_names(&pid_names, &active_pids);
            }
        }

        let Some((total, _)) = proc::read_total_jiffies() else {
            emit("{\"t\":\"err\",\"msg\":\"read /proc/stat failed\"}");
            continue;
        };
        let total_delta = total.saturating_sub(prev_total);
        prev_total = total;
        if total_delta == 0 {
            continue; // 间隔过短导致 jiffies 无变化，跳过本轮
        }
        let ts = now_ms();

        // CPU 频率：每核一次 sysfs 读（µs 级），每轮都采
        if freq_enabled {
            let khz = proc::read_cpu_freqs(ncores);
            emit(&format!(
                "{{\"t\":\"freq\",\"ts\":{},\"khz\":[{}]}}",
                ts,
                khz.iter().map(u64::to_string).collect::<Vec<_>>().join(",")
            ));
        }

        // 网络：整机口径计数器差值 → KB/s（首轮建基线不出数）
        if args.net {
            if let Some((rx, tx)) = proc::read_net_dev() {
                if let Some((pts, prx, ptx)) = prev_net.replace((ts, rx, tx)) {
                    let dt = ts.saturating_sub(pts) as f32 / 1000.0;
                    if dt > 0.0 {
                        let rx_kbs = rx.saturating_sub(prx) as f32 / 1024.0 / dt;
                        let tx_kbs = tx.saturating_sub(ptx) as f32 / 1024.0 / dt;
                        emit(&format!("{{\"t\":\"net\",\"ts\":{},\"rx\":{:.2},\"tx\":{:.2}}}", ts, rx_kbs, tx_kbs));
                    }
                }
            }
        }

        // GPU：kgsl gpubusy 计数器差值 → 窗口占比 %（首轮建基线不出数）；
        // QNX/TopGpu/Ligfx 路径的 busy% 由独立读线程异步发（不占节拍），
        // 四条非 kgsl 路径都在这里补采 dumpsys gpu 每 PID 显存（限频 ≥1s）
        match &gpu_path {
            Some(GpuPath::Kgsl(g)) => {
                if let Some((busy, total)) = gpu::read_gpu_busy(g.busy_path) {
                    if let Some((pb, pt)) = prev_gpu.replace((busy, total)) {
                        let dtotal = total.saturating_sub(pt);
                        if dtotal > 0 {
                            let pct = busy.saturating_sub(pb) as f32 / dtotal as f32 * 100.0;
                            let mhz = g.clk_path.and_then(proc::read_u64_file).map(|hz| hz / 1_000_000).unwrap_or(0);
                            emit(&format!("{{\"t\":\"gpu\",\"ts\":{},\"busy\":{:.2},\"mhz\":{}}}", ts, pct, mhz));
                        }
                    }
                }
            }
            Some(GpuPath::Qnx) | Some(GpuPath::TopGpu) | Some(GpuPath::Ligfx) | Some(GpuPath::DumpMem) => {
                if round.is_multiple_of(gpumem_every) && !active_pids.is_empty() {
                    gpu::emit_gpumem(&active_pids, ts);
                }
            }
            None => {}
        }

        // 温度/热降频：限频 ≥2s 一轮（dumpsys ~50ms 会拖长低间隔节拍轮）
        if args.thermal && round.is_multiple_of(thermal_every) && !thermal::sample(ts) && !thermal_warned {
            emit("{\"t\":\"err\",\"msg\":\"thermalservice 与 sysfs thermal zones 均无温度数据\"}");
            thermal_warned = true;
        }

        let mut exited: Vec<u32> = Vec::new();
        for &pid in &active_pids {
            let proc_path = format!("/proc/{}/stat", pid);
            let Some(proc_jiffies) = proc::read_stat_jiffies(&proc_path) else {
                exited.push(pid);
                continue;
            };

            // CPU：与上轮取差（首轮建基线不出数）
            if args.cpu {
                if let Some(st) = states.get_mut(&pid) {
                    let (cpu, th_json) = st.sample_cpu(pid, proc_jiffies, total_delta, ncores);
                    emit(&format!(
                        "{{\"t\":\"cpu\",\"ts\":{},\"pid\":{},\"cpu\":{:.2},\"th\":[{}]}}",
                        ts, pid, cpu, th_json.trim_start_matches(',')
                    ));
                } else {
                    states.insert(pid, PidState::new(proc_jiffies));
                }
            }

            // 内存：≥500ms 用 dumpsys meminfo（全分类明细）；低间隔用 smaps_rollup（Pss/Rss）
            if args.memory {
                mem::sample_memory(pid, ts, full_meminfo);
            }

            // IO：/proc/<pid>/io 计数器差值 → KB/s（首轮建基线不出数）
            if args.io {
                match proc::read_pid_io(pid) {
                    Some((r, w, dr, dw)) => {
                        if let Some((pts, pr, pw, pdr, pdw)) = io_states.insert(pid, (ts, r, w, dr, dw)) {
                            let dt = ts.saturating_sub(pts) as f32 / 1000.0;
                            if dt > 0.0 {
                                let kbs = |cur: u64, prev: u64| cur.saturating_sub(prev) as f32 / 1024.0 / dt;
                                emit(&format!(
                                    "{{\"t\":\"io\",\"ts\":{},\"pid\":{},\"r\":{:.2},\"w\":{:.2},\"dr\":{:.2},\"dw\":{:.2}}}",
                                    ts, pid, kbs(r, pr), kbs(w, pw), kbs(dr, pdr), kbs(dw, pdw)
                                ));
                            }
                        }
                    }
                    None => {
                        if !io_warned {
                            emit(&format!(
                                "{{\"t\":\"err\",\"msg\":\"pid {} 的 /proc/PID/io 不可读（非 root 或 SELinux），--io 无数据\"}}",
                                pid
                            ));
                            io_warned = true;
                        }
                    }
                }
            }

            // FPS：设备端本地 dumpsys SurfaceFlinger（图层发现 + 帧时间戳差值）。
            // 限频执行（每 fps_every 轮一次）；启动时已预热建基线，
            // 首个 FPS 轮（round == fps_every）即覆盖一个完整周期。
            if args.fps && round.is_multiple_of(fps_every) {
                let pkg = package_of(&args, &mut pkg_cache, pid);
                fps_states.entry(pid).or_default().sample_round(pid, &pkg, ts);
            }
        }

        for pid in exited {
            // 重扫已发 exit 事件（避免窗口竞态丢失），这里只做状态清理
            active_pids.retain(|&p| p != pid);
            states.remove(&pid);
            fps_states.remove(&pid);
            io_states.remove(&pid);
            pkg_cache.remove(&pid);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_json_escape() {
        assert_eq!(json_escape("a\"b\\c"), "a\\\"b\\\\c");
    }
}
