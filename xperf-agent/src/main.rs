//! xperf-agent：设备端常驻采样器。
//!
//! 低间隔（<500ms）采样时主机侧 adb 轮询不可行（单轮多次 adb 调用的开销就超过
//! 间隔本身），因此把采样循环搬到设备上：直接读 /proc（微秒级），结果以 NDJSON
//! 行流式回传。
//!
//! ## 运行模式
//!
//! **daemon 模式（`--daemon`，CLI/GUI 唯一使用路径）**：agent 常驻设备，监听
//! `localabstract:xperf-agent` 抽象 socket；host 经 `adb forward` + TCP 连接。
//! 连接即收 `hello`（含 `version`，host 校验版本，不符则发 `suicide` + 强杀重推）。
//! host → agent 命令（纯文本行）：`start <与 argv 相同的参数...>`（开采样会话）/
//! `stop`（停本会话）/ `ping`（host 每 5s 一次，30s 无数据断连）/ `suicide`（进程退出）。
//! 多 host 上限 [`MAX_SESSIONS`]，每连接一个独立采样会话（独立节拍线程 + TLS emitter）；
//! 0 会话持续 [`IDLE_EXIT_SECS`] 秒 → daemon 自杀退出（无连接超时自杀）。
//!
//! **stdout 模式（无 `--daemon`，仅手动调试用）**：NDJSON 写 stdout，由 `adb shell`
//! 长连接承载（stdin 须保持打开，EOF 即退出；事件协议与 daemon 相同）。
//!
//! 事件协议（每行一个 JSON 对象）：
//! ```text
//! {"t":"hello","ncores":8,"maxkhz":[...],"version":2}
//! {"t":"cpu","ts":<wall_ms>,"pid":29697,"cpu":15.4,"th":[[29697,"main",5.1],...]}
//! {"t":"mem","ts":<wall_ms>,"pid":29697,"pss":484880,"rss":612000,
//!  "java":..,"native":..,"code":..,"stack":..,"gfx":..,"other":..,"sys":..}
//! （内存分类字段仅 interval≥500ms 的 dumpsys meminfo 路径有值，低间隔 smaps_rollup 路径为 0）
//! {"t":"fps","ts":<wall_ms>,"pid":29697,"layer":"SVM Container#0","fps":30.0,"frames":32,"jank":0}
//! {"t":"freq","ts":<wall_ms>,"khz":[2592000,...]}              // 每核当前频率，下标对应 hello 的 maxkhz
//! {"t":"io","ts":<wall_ms>,"pid":29697,"r":12.3,"w":4.5,"dr":0.0,"dw":1.2}  // KB/s；r/w=rchar/wchar 逻辑读写，dr/dw=read_bytes/write_bytes 磁盘读写
//! {"t":"net","ts":<wall_ms>,"rx":123.4,"tx":56.7}              // KB/s 整机口径（聚合物理口，排除回环/隧道；per-app 无数据源）
//! {"t":"gpu","ts":<wall_ms>,"busy":37.5,"mhz":585}             // kgsl 或 QNX 路径；QNX 路径多 "util"/"maxmhz" 字段
//! {"t":"gpuproc","ts":<wall_ms>,"pid":29697,"busy":14.4}       // QNX 路径：每进程 GPU busy%
//! {"t":"gpumem","ts":<wall_ms>,"pid":29697,"bytes":628928512,"global":2639089664}  // 保底路径：dumpsys gpu 每 PID GPU 显存
//! {"t":"temp","ts":<wall_ms>,"status":0,"sensors":[["名",类型,°C],...]}    // status=Android ThermalStatus（-1=未知）
//! {"t":"exit","pid":29697}
//! {"t":"noproc"}
//! {"t":"err","msg":"..."}
//! （空行）                                                    // 心跳：整轮零输出时探活，host 侧 next_event 跳过
//! ```
//!
//! 用法：`xperf-agent --package <pkg> [--pid N]... --interval 50 [--cpu] [--memory] [--fps]`
//!                   `[--freq] [--io] [--net] [--gpu] [--thermal]`
//!
//! 模块划分：proc（/proc 与 sysfs 读取 + CPU 采样状态）/ mem（内存）/ fps（SurfaceFlinger）/
//! thermal（温度）/ gpu（五通道）。本文件只保留参数解析、节拍主循环与公共输出工具。
//!
//! 与主机侧 adb 模式的差异：
//! - CPU 口径相同（jiffies 差值 ×核数，单核基准），但窗口是相邻两轮之间
//!   （agent 常驻保有上一轮状态，无需主机侧 phase1/phase2 结构）
//! - 内存：interval ≥ 500ms 用本地 dumpsys meminfo（全分类明细，同轮询模式）；
//!   低间隔改读 `/proc/<pid>/smaps_rollup`（Pss/Rss，~1ms）
//! - FPS：设备端本地 dumpsys SurfaceFlinger（无 adb 中转，图层名无需引号转义）；
//!   限频至 ≥500ms 周期（每 fps_every_n_rounds 轮一次），与 CPU/内存节拍解耦——
//!   低间隔下每轮跑 dumpsys SurfaceFlinger 会拖垮节拍（实测 50ms 间隔约半数轮次 overrun）

mod fps;
mod gpu;
mod mem;
mod proc;
mod thermal;

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fps::FpsState;
use gpu::GpuPath;
use proc::PidState;

/// 协议版本：host（xperf-core 的 `AGENT_PROTOCOL_VERSION` 常量）校验，
/// 不一致则 suicide + 重推二进制。改动 wire 协议/命令时两侧同步 bump。
const PROTOCOL_VERSION: u32 = 2;

/// daemon 模式的最大并发会话（host）数
const MAX_SESSIONS: usize = 10;

/// daemon 模式：最后一个会话结束后多少秒自杀（无连接超时自杀）
const IDLE_EXIT_SECS: u64 = 60;

/// daemon 模式的抽象 socket 名（`localabstract:xperf-agent`）
const ABSTRACT_SOCK: &str = "xperf-agent";

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

/// 解析会话参数。argv 不含程序名（`argv[0]` 由调用方语义决定：
/// stdout 模式来自 `std::env::args().skip(1)`，daemon 的 start 命令来自 socket 行分词）。
fn parse_args(argv: &[String]) -> Result<Args, String> {
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
    let mut i = 0;
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

/// stdout 模式：本轮（节拍循环或任一 GPU 读线程）是否已有输出。
/// 用于心跳：整轮零输出时发一个空行探活——host 的 next_event 跳过空行（零协议影响），
/// 而 stdout 写失败（主机断连）会在 emit 内退出进程，agent 不再残留设备。
static ROUND_EMITTED: AtomicBool = AtomicBool::new(false);

/// 会话输出通道：daemon 模式下每个会话一个（socket writer + 会话级「本轮已输出」标志），
/// 经 thread_local 挂到会话节拍线程与该会话的 GPU 读线程/看门狗——
/// 各子模块（mem/fps/thermal/gpu）的 `crate::emit` 调用点因此零改动。
/// sink 返回 false = 对端断开，closure 内置会话 stop 置位（节拍循环下一轮即收）。
#[derive(Clone)]
struct SessionIo {
    sink: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    emitted: Arc<AtomicBool>,
}

thread_local! {
    /// 当前线程的会话输出通道（daemon 会话线程/GPU 线程设置；stdout 模式为 None）
    static SESSION_IO: RefCell<Option<SessionIo>> = const { RefCell::new(None) };
}

/// 挂接当前线程的会话输出通道（会话节拍线程/GPU 读线程/看门狗启动时调用）
fn set_session_io(io: SessionIo) {
    SESSION_IO.with(|c| *c.borrow_mut() = Some(io));
}

/// GPU 流式通道（QNX/TopGpu/Ligfx）全局独占与 teardown 槽：
/// daemon 多会话共享一个进程，而 QNX 统计链是驱动全局资源——多会话各开一条会
/// 叠加锁步洪泛（实测 ~20 条链挤死 frame 链）。故流式通道同时只允许一个会话持有；
/// teardown（如 QNX 停链）由持有会话结束或进程退出时执行一次。
static GPU_STREAM_BUSY: AtomicBool = AtomicBool::new(false);
static GPU_TEARDOWN: Mutex<Option<Box<dyn Fn() + Send>>> = Mutex::new(None);

/// QNX 通道注册停链 teardown（启动时调用一次）
fn register_gpu_teardown(f: Box<dyn Fn() + Send>) {
    *GPU_TEARDOWN.lock().unwrap() = Some(f);
}

/// 执行 GPU 流式通道 teardown（并发触发只执行一次）。会话结束/进程退出路径均调用。
fn run_gpu_teardown() {
    if let Some(f) = GPU_TEARDOWN.lock().unwrap().take() {
        f();
    }
    GPU_STREAM_BUSY.store(false, Ordering::Relaxed);
}

/// 带 teardown 的进程退出（std::process::exit 不跑 Drop，清理动作必须显式执行）。
/// 触发路径：stdout 模式 emit 写失败（EPIPE）/ stdin EOF / 停滞看门狗；
/// daemon 模式 suicide 命令 / 空载超时。
fn exit_with_hooks() -> ! {
    run_gpu_teardown();
    std::process::exit(0);
}

/// stdout 模式：最近一次成功写出 stdout 的时间（ms 墙钟）。停滞看门狗据此检测
/// 「host 失联但传输半开」场景：写阻塞时 emit 的失败自检不触发，须靠停滞超时兜底。
static LAST_EMIT_OK: AtomicU64 = AtomicU64::new(0);

fn emit(line: &str) {
    // daemon 会话线程：走 TLS sink（失败由 closure 内置 stop 置位，节拍循环下轮收）
    let has_sink = SESSION_IO.with(|c| {
        let io = c.borrow();
        if let Some(io) = io.as_ref() {
            io.emitted.store(true, Ordering::Relaxed);
            (io.sink)(line);
            true
        } else {
            false
        }
    });
    if has_sink {
        return;
    }
    // stdout 模式：对端断开（adb 连接关闭）时写失败，带钩子退出
    ROUND_EMITTED.store(true, Ordering::Relaxed);
    let mut out = std::io::stdout().lock();
    if writeln!(out, "{}", line).is_err() || out.flush().is_err() {
        exit_with_hooks();
    }
    LAST_EMIT_OK.store(now_ms(), Ordering::Relaxed);
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

/// FPS 兜底匹配需要的包名：--package 直接用；--pid 模式从 `/proc/<pid>/cmdline` 反查（一次性缓存）
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
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.iter().any(|a| a == "--daemon") {
        run_daemon();
    }
    let args = match parse_args(&argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(2);
        }
    };
    run_stdio_session(args);
}

/// stdout 模式（手动调试）：hello + liveness 看门狗 + 会话循环（永不返回）。
fn run_stdio_session(args: Args) -> ! {
    let Some((_, ncores)) = proc::read_total_jiffies() else {
        eprintln!("无法读取 /proc/stat");
        std::process::exit(1);
    };
    let maxkhz: Vec<u64> = (0..ncores).map(|i| proc::read_cpufreq(i, "cpuinfo_max_freq").unwrap_or(0)).collect();
    emit(&hello_line(ncores, &maxkhz));

    // 停滞看门狗：host 失联但传输半开（写阻塞，emit 失败自检不触发）时兜底退出。
    // 下限 30s 覆盖正常节拍（心跳保证每轮至少一次写出）；interval 极大时取 3 倍间隔。
    let stall_limit = (args.interval_ms * 3).max(30_000);
    LAST_EMIT_OK.store(now_ms(), Ordering::Relaxed);
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(5));
        if now_ms().saturating_sub(LAST_EMIT_OK.load(Ordering::Relaxed)) > stall_limit {
            exit_with_hooks();
        }
    });

    // stdin 存活性监测：host（xperf-core，shell 传输）会话期间持有 agent stdin 管道；
    // host 死亡 → adb stdin EOF → adbd 关设备端 stdin → EOF → 带钩子退出。
    // 手动调试时 stdin 须保持打开（重定向 /dev/null 会立即退出）。
    std::thread::spawn(|| {
        use std::io::Read;
        let mut stdin = std::io::stdin().lock();
        let mut buf = [0u8; 64];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => exit_with_hooks(),
                Ok(_) => {}
            }
        }
    });

    run_session(args, None, Arc::new(AtomicBool::new(false)));
    exit_with_hooks();
}

fn hello_line(ncores: u32, maxkhz: &[u64]) -> String {
    format!(
        "{{\"t\":\"hello\",\"ncores\":{},\"maxkhz\":[{}],\"version\":{}}}",
        ncores,
        maxkhz.iter().map(u64::to_string).collect::<Vec<_>>().join(","),
        PROTOCOL_VERSION
    )
}

/// daemon 模式：监听抽象 socket，每连接一个会话（上限 MAX_SESSIONS），
/// 0 会话持续 IDLE_EXIT_SECS 秒自杀。永不返回（bind 失败除外——已有 daemon 在跑）。
fn run_daemon() -> ! {
    // 抽象 socket 扩展 trait：Android/Linux 各在 os::android/os::linux 下（std 同 API）
    #[cfg(target_os = "android")]
    use std::os::android::net::SocketAddrExt;
    #[cfg(target_os = "linux")]
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::UnixListener;

    let Some((_, ncores)) = proc::read_total_jiffies() else {
        eprintln!("无法读取 /proc/stat");
        std::process::exit(1);
    };
    let maxkhz: Vec<u64> = (0..ncores).map(|i| proc::read_cpufreq(i, "cpuinfo_max_freq").unwrap_or(0)).collect();

    let addr = std::os::unix::net::SocketAddr::from_abstract_name(ABSTRACT_SOCK).expect("抽象地址构造失败");
    let listener = match UnixListener::bind_addr(&addr) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("绑定 {} 失败（已有 daemon？）: {}", ABSTRACT_SOCK, e);
            std::process::exit(1);
        }
    };
    eprintln!("xperf-agent daemon v{} 监听 localabstract:{}", PROTOCOL_VERSION, ABSTRACT_SOCK);

    // 会话计数 + 空载计时：0 会话持续 IDLE_EXIT_SECS 秒自杀
    let sessions = Arc::new(AtomicUsize::new(0));
    let idle_since = Arc::new(AtomicU64::new(now_ms()));
    {
        let (sessions, idle_since) = (sessions.clone(), idle_since.clone());
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(5));
            if sessions.load(Ordering::Relaxed) == 0 {
                let t = idle_since.load(Ordering::Relaxed);
                if now_ms().saturating_sub(t) > IDLE_EXIT_SECS * 1000 {
                    eprintln!("空载 {}s，daemon 退出", IDLE_EXIT_SECS);
                    exit_with_hooks();
                }
            }
        });
    }

    for conn in listener.incoming() {
        let Ok(stream) = conn else { continue };
        let hello = hello_line(ncores, &maxkhz);
        if sessions.load(Ordering::Relaxed) >= MAX_SESSIONS {
            // 满员也必须先发 hello 再发 err：probe 探活只认 hello——若首行是 err，
            // host 会误判「无 daemon」而强杀重启 daemon，把 10 个活会话全端掉（review 修复）
            use std::io::Write as _;
            let mut s = &stream;
            let _ = writeln!(s, "{}", hello);
            let _ = writeln!(s, "{{\"t\":\"err\",\"msg\":\"会话数已满（{}）\"}}", MAX_SESSIONS);
            continue;
        }
        sessions.fetch_add(1, Ordering::Relaxed);
        let (sessions, idle_since) = (sessions.clone(), idle_since.clone());
        std::thread::spawn(move || {
            handle_conn(stream, hello);
            if sessions.fetch_sub(1, Ordering::Relaxed) == 1 {
                idle_since.store(now_ms(), Ordering::Relaxed); // 归零时刻：空载计时起点
            }
        });
    }
    exit_with_hooks();
}

/// daemon 单连接处理：发 hello → 命令循环（start/stop/ping/suicide）→ 会话线程。
/// 30s 收不到任何数据（ping 超时）或连接断开即结束会话；suicide 整个进程退出。
fn handle_conn(stream: std::os::unix::net::UnixStream, hello: String) {
    use std::io::{BufRead, BufReader};

    let writer = match stream.try_clone() {
        Ok(w) => Arc::new(Mutex::new(w)),
        Err(_) => return,
    };
    // 连接即 hello（含版本，host 据此校验）
    {
        use std::io::Write as _;
        if writeln!(writer.lock().unwrap(), "{}", hello).is_err() {
            return;
        }
    }
    let mut reader = BufReader::new(stream);
    // 心跳超时：host 每 5s 一次 ping；30s 无任何数据视为失联（防半开连接挂死会话）
    let _ = reader.get_ref().set_read_timeout(Some(Duration::from_secs(30)));
    let mut session_stop: Option<Arc<AtomicBool>> = None;
    let mut session_handle: Option<std::thread::JoinHandle<()>> = None;
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => {
                eprintln!("[conn] EOF（host 断开）");
                break;
            }
            Err(e) => {
                eprintln!("[conn] 读失败: {}", e);
                break;
            }
            Ok(_) => {
                let cmd = line.trim();
                if cmd == "ping" {
                    continue;
                }
                if cmd == "suicide" {
                    exit_with_hooks();
                }
                if cmd == "stop" {
                    if let Some(s) = session_stop.take() {
                        s.store(true, Ordering::Relaxed);
                        // 等会话线程收尾（节拍 ≤interval + dumpsys 有界），
                        // 收完才允许重新 start（保证 GPU 独占/teardown 已复位）。
                        // 无 ack：stop 是 fire-and-forget，host 当前也不发它
                        if let Some(h) = session_handle.take() {
                            let _ = h.join();
                        }
                    }
                    continue;
                }
                if let Some(rest) = cmd.strip_prefix("start ") {
                    if session_stop.is_some() {
                        write_line(&writer, "{\"t\":\"err\",\"msg\":\"本会话已在采样（先 stop）\"}");
                        continue;
                    }
                    let argv: Vec<String> = rest.split_whitespace().map(|s| s.to_string()).collect();
                    match parse_args(&argv) {
                        Ok(args) => {
                            let stop = Arc::new(AtomicBool::new(false));
                            let emitted = Arc::new(AtomicBool::new(false));
                            let (stop_sink, w_sink) = (stop.clone(), writer.clone());
                            // sink：写失败（host 断开）→ 置 stop，节拍循环下轮收
                            let sink: Arc<dyn Fn(&str) -> bool + Send + Sync> = Arc::new(move |s: &str| {
                                use std::io::Write as _;
                                let ok = w_sink.lock().map(|mut g| writeln!(g, "{}", s).and_then(|_| g.flush()).is_ok()).unwrap_or(false);
                                if !ok {
                                    stop_sink.store(true, Ordering::Relaxed);
                                }
                                ok
                            });
                            let io = SessionIo { sink, emitted };
                            session_stop = Some(stop.clone());
                            session_handle = Some(std::thread::spawn(move || run_session(args, Some(io), stop)));
                        }
                        Err(e) => write_line(&writer, &format!("{{\"t\":\"err\",\"msg\":\"start 参数错误：{}\"}}", json_escape(&e))),
                    }
                    continue;
                }
                write_line(&writer, &format!("{{\"t\":\"err\",\"msg\":\"未知命令：{}\"}}", json_escape(cmd)));
            }
        }
    }
    // 收尾：停会话 → 等线程收（节拍 ≤interval + dumpsys 有界）
    // （GPU 流式通道的 teardown 由会话线程退出前自行执行）
    if let Some(s) = &session_stop {
        s.store(true, Ordering::Relaxed);
    }
    if let Some(h) = session_handle {
        let _ = h.join();
    }
}

fn write_line(w: &Arc<Mutex<std::os::unix::net::UnixStream>>, line: &str) {
    use std::io::Write as _;
    if let Ok(mut g) = w.lock() {
        let _ = writeln!(g, "{}", line);
        let _ = g.flush();
    }
}

/// 采样会话主循环（stdout 模式与 daemon 会话线程共用）。
/// `io`：daemon 会话的输出通道（None = stdout 模式直写）；
/// `stop`：会话停止标志（host 断开/stop 命令/写失败置位），每轮检查。
fn run_session(args: Args, io: Option<SessionIo>, stop: Arc<AtomicBool>) {
    // 会话线程挂 TLS 输出通道（daemon 模式）；GPU 读线程/看门狗在通道启动时同样挂接
    let emitted = io.as_ref().map(|i| i.emitted.clone());
    if let Some(io) = io {
        set_session_io(io);
    }

    let Some((mut prev_total, ncores)) = proc::read_total_jiffies() else {
        emit("{\"t\":\"err\",\"msg\":\"read /proc/stat failed\"}");
        return;
    };
    // 每核最大频率（KHz），与 freq 事件的 khz 数组下标对应
    let maxkhz: Vec<u64> = (0..ncores).map(|i| proc::read_cpufreq(i, "cpuinfo_max_freq").unwrap_or(0)).collect();
    // QNX 地址：--qnx-host 参数覆盖默认值
    if let Some(ref host) = args.qnx_host {
        gpu::set_qnx_host(host);
    }

    // B 类指标启动探测（探测失败发 err 并禁用，不影响其余指标）
    let mut freq_enabled = args.freq;
    if freq_enabled && maxkhz.iter().all(|&f| f == 0) && proc::read_cpu_freqs(ncores).iter().all(|&f| f == 0) {
        emit("{\"t\":\"err\",\"msg\":\"cpufreq sysfs 不可用，--freq 已禁用\"}");
        freq_enabled = false;
    }
    let gpu_path = if args.gpu {
        let mut g = gpu::detect_gpu_path_ex(args.platform.as_deref());
        // 流式通道（QNX/TopGpu/Ligfx）全局独占：QNX 统计链是驱动全局资源，
        // 多会话各开一条会叠加锁步洪泛（实测挤死 frame 链）。被占用时本会话禁用 GPU。
        let streaming = matches!(g, Some(GpuPath::Qnx) | Some(GpuPath::TopGpu) | Some(GpuPath::Ligfx));
        if streaming
            && GPU_STREAM_BUSY
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
        {
            emit("{\"t\":\"err\",\"msg\":\"GPU 流式通道被另一会话占用，本会话 --gpu 已禁用\"}");
            g = None;
        }
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
    // 本会话是否持有 GPU 流式通道（结束时收 teardown + 释放独占）
    let holds_gpu_stream = matches!(gpu_path, Some(GpuPath::Qnx) | Some(GpuPath::TopGpu) | Some(GpuPath::Ligfx));

    // 流式 GPU 通道（QNX/TopGpu/Ligfx）的进程行按 comm 名归因到 Android PID，
    // 映射表读线程/主循环共享（重扫时更新）。
    let pid_names: Arc<Mutex<HashMap<String, u32>>> = Arc::new(Mutex::new(HashMap::new()));

    let mut states: HashMap<u32, PidState> = HashMap::new();
    let mut fps_states: HashMap<u32, FpsState> = HashMap::new();
    // 速率类指标的上一轮基线：(上轮时间戳 ms, 计数器...)
    let mut io_states: HashMap<u32, (u64, u64, u64, u64, u64)> = HashMap::new(); // pid → (ts, rchar, wchar, read_bytes, write_bytes)
    let mut prev_net: Option<(u64, u64, u64)> = None; // (ts, rx_bytes, tx_bytes)
    let mut gpu_busy_calc = gpu::GpuBusyCalc::new(); // kgsl gpubusy 占比（累计/窗口语义自适应）
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
    // daemon 会话：读线程/看门狗挂本会话的 TLS 输出通道；其停止标志用 gpu_stop——
    // 会话结束必须先跑 teardown（停链写 telnet）再放读线程杀子进程，否则竞态下
    // 读线程先杀 telnet、teardown 写入落空链残留（真机实测）。
    let gpu_stop = Arc::new(AtomicBool::new(false));
    if let Some(g) = &gpu_path {
        if matches!(g, GpuPath::Qnx | GpuPath::TopGpu | GpuPath::Ligfx) {
            fill_pid_names(&pid_names, &active_pids);
            let io = SESSION_IO.with(|c| c.borrow().clone());
            gpu::start_stream_channels(g, args.interval_ms, &pid_names, io, gpu_stop.clone());
        }
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
        if stop.load(Ordering::Relaxed) {
            break; // host 断开 / stop 命令 / 写失败
        }
        round += 1;
        // 心跳：上一轮（含 GPU 读线程）完全零输出时发一个空行探活。emit("") 会置位
        // 标志故随即复位；读线程在此间隙的输出至多让下一轮多一个空行，host 跳过空行无害。
        // daemon 会话用会话级标志，stdout 模式用全局静态（两者互斥）
        let round_had_output = match &emitted {
            Some(e) => e.swap(false, Ordering::Relaxed),
            None => ROUND_EMITTED.swap(false, Ordering::Relaxed),
        };
        if round >= 2 && !round_had_output {
            emit("");
            match &emitted {
                Some(e) => e.store(false, Ordering::Relaxed),
                None => ROUND_EMITTED.store(false, Ordering::Relaxed),
            }
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

        // GPU：kgsl gpubusy 读数 → busy%（GpuBusyCalc 自适应累计/窗口两种内核语义，
        // 累计语义首轮建基线不出数）；QNX/TopGpu/Ligfx 路径的 busy% 由独立读线程
        // 异步发（不占节拍），四条非 kgsl 路径都在这里补采 dumpsys gpu 每 PID 显存（限频 ≥1s）
        match &gpu_path {
            Some(GpuPath::Kgsl(g)) => {
                if let Some((busy, total)) = gpu::read_gpu_busy(g.busy_path) {
                    if let Some(pct) = gpu_busy_calc.sample(busy, total) {
                        let mhz = g.clk_path.and_then(proc::read_u64_file).map(|hz| hz / 1_000_000).unwrap_or(0);
                        emit(&format!("{{\"t\":\"gpu\",\"ts\":{},\"busy\":{:.2},\"mhz\":{}}}", ts, pct, mhz));
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

    // 会话结束：先跑 GPU teardown（QNX 停链须趁 telnet 子进程还活着——读线程的
    // stop 尚未置位，其 kill 未发生），再放读线程收（杀 telnet），顺序确定无竞态。
    if holds_gpu_stream {
        run_gpu_teardown();
    }
    gpu_stop.store(true, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_json_escape() {
        assert_eq!(json_escape("a\"b\\c"), "a\\\"b\\\\c");
    }

    /// start 命令行 → parse_args（daemon 会话的参数解析路径）
    #[test]
    fn test_parse_args() {
        // 完整参数（与 host spawn_agent 组装的 start 行同构）
        let argv: Vec<String> = ["--package", "com.x", "--interval", "500", "--cpu", "--memory", "--fps",
            "--freq", "--io", "--net", "--gpu", "--thermal", "--platform", "ss3", "--qnx-host", "1.2.3.4"]
            .iter().map(|s| s.to_string()).collect();
        let a = parse_args(&argv).unwrap();
        assert_eq!(a.package.as_deref(), Some("com.x"));
        assert_eq!(a.interval_ms, 500);
        assert!(a.cpu && a.memory && a.fps && a.freq && a.io && a.net && a.gpu && a.thermal);
        assert_eq!(a.platform.as_deref(), Some("ss3"));
        assert_eq!(a.qnx_host.as_deref(), Some("1.2.3.4"));
        // 最小参数：默认间隔 50ms、其余指标关
        let a = parse_args(&["--package".to_string(), "com.x".to_string(), "--cpu".to_string()]).unwrap();
        assert_eq!(a.interval_ms, 50);
        assert!(!a.memory && !a.fps);
        // 无采样开关报错
        assert!(parse_args(&["--package".to_string(), "com.x".to_string()]).is_err());
        // 无 --package/--pid 报错（host 侧能拿到 err 行而不是会话静默失败）
        assert!(parse_args(&[]).is_err());
        // 间隔下限 50ms
        assert!(parse_args(&["--package".to_string(), "com.x".to_string(), "--interval".to_string(), "10".to_string()]).is_err());
        // 未知参数报错
        assert!(parse_args(&["--package".to_string(), "com.x".to_string(), "--bogus".to_string()]).is_err());
        // 缺值报错
        assert!(parse_args(&["--package".to_string()]).is_err());
    }

    /// hello 行含版本号（host 探活协议契约）
    #[test]
    fn test_hello_line() {
        let h = hello_line(8, &[1785600, 2841600]);
        assert!(h.contains("\"t\":\"hello\""));
        assert!(h.contains(&format!("\"version\":{}", PROTOCOL_VERSION)));
        assert!(h.contains("\"maxkhz\":[1785600,2841600]"));
    }
}
