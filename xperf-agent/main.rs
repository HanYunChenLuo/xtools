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
//!
//! 用法：xperf-agent --package <pkg> [--pid N]... --interval 50 [--cpu] [--memory] [--fps]
//!                   [--freq] [--io] [--net] [--gpu] [--thermal]
//!
//! 与主机侧 adb 模式的差异：
//! - CPU 口径相同（jiffies 差值 ×核数，单核基准），但窗口是相邻两轮之间
//!   （agent 常驻保有上一轮状态，无需主机侧 phase1/phase2 结构）
//! - 内存：interval ≥ 500ms 用本地 dumpsys meminfo（全分类明细，同轮询模式）；
//!   低间隔改读 /proc/<pid>/smaps_rollup（Pss/Rss，~1ms）
//! - FPS：设备端本地 dumpsys SurfaceFlinger（无 adb 中转，图层名无需引号转义）；
//!   限频至 ≥500ms 周期（每 fps_every_n_rounds 轮一次），与 CPU/内存节拍解耦——
//!   低间隔下每轮跑 dumpsys SurfaceFlinger 会拖垮节拍（实测 50ms 间隔约半数轮次 overrun）

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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

// ---------- /proc 读取 ----------

/// 读 /proc/stat：返回 (所有核总 jiffies, 核数)
fn read_total_jiffies() -> Option<(u64, u32)> {
    let content = fs::read_to_string("/proc/stat").ok()?;
    let mut total = None;
    let mut ncores = 0u32;
    for line in content.lines() {
        if line.starts_with("cpu ") {
            total = Some(
                line.split_whitespace()
                    .skip(1)
                    .map(|v| v.parse::<u64>().unwrap_or(0))
                    .sum(),
            );
        } else if line.starts_with("cpu")
            && line.as_bytes().get(3).is_some_and(|b| b.is_ascii_digit())
        {
            ncores += 1;
        }
    }
    Some((total?, ncores.max(1)))
}

/// 读 /proc/<pid>/stat 或 task/<tid>/stat 的 utime+stime jiffies
fn read_stat_jiffies(path: &str) -> Option<u64> {
    let content = fs::read_to_string(path).ok()?;
    parse_stat_jiffies(&content)
}

fn parse_stat_jiffies(stat: &str) -> Option<u64> {
    let paren_end = stat.trim().rfind(')')?;
    let fields: Vec<&str> = stat.trim()[paren_end + 1..].split_whitespace().collect();
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some(utime + stime)
}

/// 读 /proc/<pid>/smaps_rollup：返回 (Pss KB, Rss KB)
fn read_smaps_rollup(pid: u32) -> Option<(u64, u64)> {
    let content = fs::read_to_string(format!("/proc/{}/smaps_rollup", pid)).ok()?;
    parse_smaps_rollup(&content)
}

fn parse_smaps_rollup(content: &str) -> Option<(u64, u64)> {
    let mut pss = None;
    let mut rss = None;
    for line in content.lines() {
        if let Some(v) = line.strip_prefix("Pss:") {
            pss = v.split_whitespace().next().and_then(|s| s.parse().ok());
        } else if let Some(v) = line.strip_prefix("Rss:") {
            rss = v.split_whitespace().next().and_then(|s| s.parse().ok());
        }
        if pss.is_some() && rss.is_some() {
            break;
        }
    }
    Some((pss?, rss?))
}

/// 按包名解析 PID（扫 /proc/*/cmdline）
fn resolve_pids(package: &str) -> Vec<u32> {
    let mut pids = Vec::new();
    if let Ok(entries) = fs::read_dir("/proc") {
        for e in entries.flatten() {
            let name = e.file_name();
            let Some(name) = name.to_str() else { continue };
            let Ok(pid) = name.parse::<u32>() else { continue };
            if let Ok(cmd) = fs::read_to_string(format!("/proc/{}/cmdline", pid)) {
                if cmd.trim_end_matches('\0') == package {
                    pids.push(pid);
                }
            }
        }
    }
    pids.sort_unstable();
    pids
}

// ---------- B 类指标：CPU 频率 / IO / 网络 / GPU / 温度 ----------

fn read_u64_file(path: &str) -> Option<u64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// 读 cpu<N> 的某个 cpufreq 节点（KHz）；核离线/节点缺失返回 None
fn read_cpufreq(core: u32, node: &str) -> Option<u64> {
    read_u64_file(&format!("/sys/devices/system/cpu/cpu{}/cpufreq/{}", core, node))
}

/// 读全部核 scaling_cur_freq（KHz）；读不到的核补 0，保持下标与核号对齐
fn read_cpu_freqs(ncores: u32) -> Vec<u64> {
    (0..ncores).map(|i| read_cpufreq(i, "scaling_cur_freq").unwrap_or(0)).collect()
}

/// 读 /proc/<pid>/io：(rchar, wchar, read_bytes, write_bytes)，单位字节
/// rchar/wchar 是逻辑读写（含 page cache），read_bytes/write_bytes 是真实磁盘 IO
fn read_pid_io(pid: u32) -> Option<(u64, u64, u64, u64)> {
    let content = fs::read_to_string(format!("/proc/{}/io", pid)).ok()?;
    parse_pid_io(&content)
}

fn parse_pid_io(content: &str) -> Option<(u64, u64, u64, u64)> {
    let (mut r, mut w, mut dr, mut dw) = (None, None, None, None);
    for line in content.lines() {
        if let Some(v) = line.strip_prefix("rchar:") {
            r = v.trim().parse().ok();
        } else if let Some(v) = line.strip_prefix("wchar:") {
            w = v.trim().parse().ok();
        } else if let Some(v) = line.strip_prefix("read_bytes:") {
            dr = v.trim().parse().ok();
        } else if let Some(v) = line.strip_prefix("write_bytes:") {
            dw = v.trim().parse().ok();
        }
    }
    Some((r?, w?, dr?, dw?))
}

/// 读 /proc/net/dev 聚合物理口收发字节数：(rx_bytes, tx_bytes)。
/// 排除回环与隧道/虚拟口（lo/sit/tun/gre/dummy/vti/ip6*），只统计真实网络活动。
/// 注意是整机口径：Android 应用共享 netns，/proc/<pid>/net/dev 与整机内容一致，
/// per-app 流量需 qtaguid/eBPF（此车机均不可用）。
fn read_net_dev() -> Option<(u64, u64)> {
    let content = fs::read_to_string("/proc/net/dev").ok()?;
    Some(parse_net_dev(&content))
}

fn parse_net_dev(content: &str) -> (u64, u64) {
    let (mut rx, mut tx) = (0u64, 0u64);
    for line in content.lines().skip(2) {
        let Some((iface, rest)) = line.split_once(':') else { continue };
        let iface = iface.trim();
        if iface == "lo"
            || iface.starts_with("sit")
            || iface.starts_with("tun")
            || iface.starts_with("gre")
            || iface.starts_with("dummy")
            || iface.contains("vti")
            || iface.starts_with("ip6")
        {
            continue;
        }
        let fields: Vec<&str> = rest.split_whitespace().collect();
        rx += fields.first().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
        tx += fields.get(8).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
    }
    (rx, tx)
}

/// kgsl sysfs 路径（GPU 使用率 + 时钟）。GPU 在 hypervisor 后的车机平台无此节点。
struct Kgsl {
    busy_path: &'static str,
    clk_path: Option<&'static str>,
}

fn detect_kgsl() -> Option<Kgsl> {
    const BUSY: &str = "/sys/class/kgsl/kgsl-3d0/gpubusy";
    fs::metadata(BUSY).ok()?;
    let clk_path = ["/sys/class/kgsl/kgsl-3d0/gpuclk", "/sys/class/kgsl/kgsl-3d0/devfreq/cur_freq"]
        .into_iter()
        .find(|p| fs::metadata(p).is_ok());
    Some(Kgsl { busy_path: BUSY, clk_path })
}

/// 读 gpubusy："busy_time total_time"（µs 计数器），差值算窗口占比
fn read_gpu_busy(path: &str) -> Option<(u64, u64)> {
    let content = fs::read_to_string(path).ok()?;
    parse_gpu_busy(&content)
}

fn parse_gpu_busy(content: &str) -> Option<(u64, u64)> {
    let mut it = content.split_whitespace();
    let busy = it.next()?.parse().ok()?;
    let total = it.next()?.parse().ok()?;
    Some((busy, total))
}

/// GPU 采样路径，按平台/探测结果选择：
/// - Kgsl：标准 Android / SS2，sysfs gpubusy 直通（GVM 内有 kgsl）
/// - Qnx：SS3/8295，QNX host 侧 kgsl slog（GVM 内无 kgsl，telnet 读 QNX）
/// - TopGpu：SS2MAX/8155，topgpu 工具（push 到 /data/，读 sysfs 或 ftrace）
/// - Ligfx：SS4/8797，PVM 侧 logcat -s ligfxprofilerd（每帧 Utilization + 每进程 busy）
/// - DumpMem：保底，dumpsys gpu 每 PID 显存
enum GpuPath {
    Kgsl(Kgsl),
    Qnx,
    TopGpu,
    Ligfx,
    DumpMem,
}

/// QNX host（GPU 所在侧）默认地址：SS3/8295 平台固定 172.31.101.52（virtio_net eth1 对端）
const QNX_TELNET_IP_DEFAULT: &str = "172.31.101.52";

/// 当前 QNX 地址（由 --qnx-host 参数覆盖，或用默认值）
fn qnx_ip() -> &'static str {
    use std::sync::OnceLock;
    static IP: OnceLock<String> = OnceLock::new();
    IP.get_or_init(|| QNX_TELNET_IP_DEFAULT.to_string()).as_str()
}

fn qnx_addr() -> String {
    format!("{}:23", qnx_ip())
}

/// 设置 QNX 地址（main 启动时调用一次）
fn set_qnx_host(ip: &str) {
    use std::sync::OnceLock;
    static IP: OnceLock<String> = OnceLock::new();
    let _ = IP.set(ip.to_string());
}

/// GPU 路径探测（带平台提示）
/// platform: "ss3" → QNX | "ss2max" → TopGpu | "ss4" → Ligfx | "android" → Kgsl | None → 自动探测
fn detect_gpu_path_ex(platform: Option<&str>) -> Option<GpuPath> {
    match platform {
        Some("ss3") => {
            // SS3：跳过 kgsl，QNX 优先，失败则 dumpsys 保底
            if qnx_gpu_available() {
                Some(GpuPath::Qnx)
            } else {
                dumpsys(&["gpu"]).and_then(|s| parse_gpu_mem_snapshot(&s).map(|_| GpuPath::DumpMem))
            }
        }
        Some("ss2max") | Some("ss2pro") => {
            // SS2 系列：kgsl sysfs 优先（直通），失败则 topgpu 工具，再失败 dumpsys 保底
            if let Some(k) = detect_kgsl() {
                Some(GpuPath::Kgsl(k))
            } else if topgpu_available() {
                Some(GpuPath::TopGpu)
            } else {
                dumpsys(&["gpu"]).and_then(|s| parse_gpu_mem_snapshot(&s).map(|_| GpuPath::DumpMem))
            }
        }
        Some("ss4") => {
            // SS4：ligfxprofilerd logcat 优先，失败则 kgsl（可能有），再失败 dumpsys 保底
            if ligfx_available() {
                Some(GpuPath::Ligfx)
            } else if let Some(k) = detect_kgsl() {
                Some(GpuPath::Kgsl(k))
            } else {
                dumpsys(&["gpu"]).and_then(|s| parse_gpu_mem_snapshot(&s).map(|_| GpuPath::DumpMem))
            }
        }
        _ => {
            // 自动探测 / android：kgsl 优先，QNX 次之，dumpsys 保底
            if let Some(k) = detect_kgsl() {
                Some(GpuPath::Kgsl(k))
            } else if qnx_gpu_available() {
                Some(GpuPath::Qnx)
            } else {
                dumpsys(&["gpu"]).and_then(|s| parse_gpu_mem_snapshot(&s).map(|_| GpuPath::DumpMem))
            }
        }
    }
}

#[allow(dead_code)]
fn detect_gpu_path() -> Option<GpuPath> {
    detect_gpu_path_ex(None)
}

/// QNX 通道可用性：busybox 存在 + QNX telnet 端口可连
fn qnx_gpu_available() -> bool {
    if fs::metadata("/vendor/bin/busybox").is_err() && fs::metadata("/system/bin/busybox").is_err() {
        return false;
    }
    use std::net::TcpStream;
    use std::net::ToSocketAddrs;
    let addr = match qnx_addr().to_socket_addrs() {
        Ok(mut it) => match it.next() {
            Some(a) => a,
            None => return false,
        },
        Err(_) => return false,
    };
    TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_ok()
}

/// QNX kgsl slog 样本（slog2info 流的两类行）：
/// 进程行: "For process[PID:1758842997] = 'xiang.car.x.svm' the GPU busy = 14.40% with CtxtID = 244 priority = 1"
///   （PID 是 QNX 侧编号，无意义；按进程名匹配 Android comm）
/// 系统行: "frame 435653: freq = 506.975174MHz/635Mhz, elapsed time = 5001.13ms, busy time = 840.57ms, busy = 16.81%, utilization = 13.42%"
enum QnxGpuSample {
    Proc { name: String, busy: f32 },
    Sys { mhz: u32, maxmhz: u32, busy: f32, util: f32 },
}

fn parse_qnx_gpu_line(line: &str) -> Option<QnxGpuSample> {
    if let Some(pos) = line.find("For process[PID:") {
        // 进程名在 = '...' 内；busy 在 "the GPU busy = " 后
        let name_start = line[pos..].find("= '")? + pos + 3;
        let name_end = line[name_start..].find('\'')? + name_start;
        let name = line[name_start..name_end].to_string();
        let busy_pos = line.find("the GPU busy = ")? + "the GPU busy = ".len();
        let busy: f32 = line[busy_pos..].split('%').next()?.trim().parse().ok()?;
        return Some(QnxGpuSample::Proc { name, busy });
    }
    if line.contains("utilization = ") && line.contains("frame ") {
        // freq = 506.975174MHz/635Mhz
        let fpos = line.find("freq = ")? + 7;
        let fend = line[fpos..].find("MHz")? + fpos;
        let mhz = line[fpos..fend].trim().parse::<f32>().ok()? as u32;
        let maxpos = line[fend..].find('/')? + fend + 1;
        let maxend = line[maxpos..].find("Mhz")? + maxpos;
        let maxmhz = line[maxpos..maxend].trim().parse::<f32>().ok()? as u32;
        let bpos = line.find("busy = ")? + 7;
        let busy: f32 = line[bpos..].split('%').next()?.trim().parse().ok()?;
        let upos = line.find("utilization = ")? + "utilization = ".len();
        let util: f32 = line[upos..].split('%').next()?.trim().parse().ok()?;
        return Some(QnxGpuSample::Sys { mhz, maxmhz, busy, util });
    }
    None
}

/// 启动 QNX GPU 统计流：busybox telnet 登录（root 免密）→ 开 kgsl 统计 → slog2info -W 持续跟踪。
/// 返回 (子进程, stdin 保持管道存活——drop 即 EOF 会让 telnet 退出, 行读取器)。
/// agent 退出时 stdin/stdout 管道断开，telnet 随 QNX 侧会话结束自行清理。
/// period_ms：kgsl 统计周期（实测 50ms 稳定；QNX 侧逐上下文打点，telnet 带宽无压力）
fn spawn_qnx_gpu(period_ms: u64) -> Option<(std::process::Child, std::process::ChildStdin, std::io::BufReader<std::process::ChildStdout>)> {
    use std::io::BufReader;
    use std::process::{Command, Stdio};
    let mut child = Command::new("busybox")
        .args(["telnet", qnx_ip()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    let mut reader = BufReader::new(child.stdout.take()?);
    // 登录阶段必须逐字节读：QNX 的 "login: " 提示符不带换行，read_line 会永远阻塞
    fn read_until(reader: &mut std::io::BufReader<std::process::ChildStdout>, marker: &str, deadline: Instant) -> Option<()> {
        use std::io::Read;
        let mut buf = String::new();
        loop {
            let mut b = [0u8; 1];
            match reader.read_exact(&mut b) {
                Ok(()) => {}
                Err(_) => return None,
            }
            buf.push(b[0] as char);
            if buf.ends_with(marker) {
                return Some(());
            }
            if buf.len() > 4096 {
                buf.drain(..2048); // 防 banner 刷屏时缓冲无限增长
            }
            if Instant::now() > deadline {
                return None;
            }
        }
    }
    let deadline = Instant::now() + Duration::from_secs(8);
    read_until(&mut reader, "login:", deadline)?;
    stdin.write_all(b"root\n").ok()?;
    // 等 shell prompt：QNX 登录成功后的提示符是 "# "（同样无换行）
    read_until(&mut reader, "# ", deadline)?;
    // 开 kgsl 统计并启动持续流（-W 只跟新日志不回放历史；grep 过滤 VHAL 等海量无关日志）
    let cmds = format!(
        "echo gpu_set_log_level 4 > /dev/kgsl-control\n\
         echo gpubusystats {} > /dev/kgsl-control\n\
         echo gpu_per_process_busy {} > /dev/kgsl-control\n\
         slog2info -W | grep kgsl\n",
        period_ms, period_ms
    );
    stdin.write_all(cmds.as_bytes()).ok()?;
    Some((child, stdin, reader))
}

// ---------- SS2MAX topgpu 工具通道 ----------
// topgpu 是 SS2/8155 平台的 GPU 负载工具（push 到 /data/），读 sysfs gpu_busy_percentage
// 或 adreno_cmdbatch ftrace 事件，输出系统 GPU 使用率 + 各进程使用率。
// 格式（每采样周期一行）：
//   sys gpu: 20.0%
//   pid 1234 'com.app' gpu: 16.0% (80.0% of sys)

/// topgpu 工具可用性：/data/local/tmp/topgpu 或 /data/topgpu 存在且可执行
fn topgpu_available() -> bool {
    for p in ["/data/local/tmp/topgpu", "/data/topgpu"] {
        if fs::metadata(p).is_ok() {
            return true;
        }
    }
    false
}

/// 启动 topgpu 子进程（持续输出流），返回 (child, reader)
/// period_s: 采样周期（秒），topgpu 接受整数秒
fn spawn_topgpu(period_s: u64) -> Option<(std::process::Child, std::io::BufReader<std::process::ChildStdout>)> {
    use std::io::BufReader;
    use std::process::{Command, Stdio};
    let path = ["/data/local/tmp/topgpu", "/data/topgpu"]
        .into_iter()
        .find(|p| fs::metadata(p).is_ok())?;
    let mut child = Command::new(path)
        .arg(period_s.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    Some((child, BufReader::new(stdout)))
}

/// 解析 topgpu 输出行
/// "sys gpu: 20.0%" → Sys(20.0)
/// "pid 1234 'com.app' gpu: 16.0% (80.0% of sys)" → Proc("com.app", 16.0)
enum TopGpuSample {
    Sys(f32),
    Proc(String, f32),
}

fn parse_topgpu_line(line: &str) -> Option<TopGpuSample> {
    if let Some(rest) = line.strip_prefix("sys gpu:") {
        let v: f32 = rest.trim().trim_end_matches('%').trim().parse().ok()?;
        return Some(TopGpuSample::Sys(v));
    }
    if line.starts_with("pid ") && line.contains("gpu:") {
        // pid 1234 'com.app' gpu: 16.0% (80.0% of sys)
        let name_start = line.find('\'')? + 1;
        let name_end = line[name_start..].find('\'')? + name_start;
        let name = line[name_start..name_end].to_string();
        let gpu_pos = line.find("gpu:")? + 4;
        let busy: f32 = line[gpu_pos..].split('%').next()?.trim().parse().ok()?;
        return Some(TopGpuSample::Proc(name, busy));
    }
    None
}

// ---------- SS4 ligfxprofilerd logcat 通道 ----------
// SS4/8797 平台 GPU 统计由 ligfxprofilerd 服务输出到 logcat，每帧一行系统行 + N 行进程行：
//   [GPU0] Frame N: Frequency: 1000 Hz, ..., Busy=33.75%, ..., Utilization=33.75%
//   [GPU0]   GVM_com.lixiang.eid-8925: Busy=15.38%, ..., Utilization=15.38%
// 业务侧只需关注 Utilization 字段。

/// ligfxprofilerd 可用性：logcat 能拉到 ligfxprofilerd 标签的日志
fn ligfx_available() -> bool {
    // 快速探测：logcat -d -s ligfxprofilerd 取最近日志，有内容即可用
    std::process::Command::new("logcat")
        .args(["-d", "-s", "ligfxprofilerd", "-m", "1"])
        .output()
        .ok()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false)
}

/// 启动 ligfxprofilerd logcat 流，返回 (child, reader)
fn spawn_ligfx() -> Option<(std::process::Child, std::io::BufReader<std::process::ChildStdout>)> {
    use std::io::BufReader;
    use std::process::{Command, Stdio};
    let mut child = Command::new("logcat")
        .args(["-s", "ligfxprofilerd"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    Some((child, BufReader::new(stdout)))
}

/// 解析 ligfxprofilerd logcat 行
/// "[GPU0] Frame N: Frequency: 1000 Hz, ..., Busy=33.75%, ..., Utilization=33.75%"
///   → LigfxSys { mhz: 1000, busy: 33.75, util: 33.75 }
/// "[GPU0]   GVM_com.lixiang.eid-8925: Busy=15.38%, ..., Utilization=15.38%"
///   → LigfxProc { name: "com.lixiang.eid", busy: 15.38, util: 15.38 }
enum LigfxSample {
    Sys { mhz: u32, busy: f32, util: f32 },
    Proc { name: String, busy: f32, #[allow(dead_code)] util: f32 },
}

fn parse_ligfx_line(line: &str) -> Option<LigfxSample> {
    if !line.contains("ligfxprofilerd") {
        return None;
    }
    // 系统行：含 "Frame N:" 和 "Frequency:"
    if line.contains("Frame ") && line.contains("Frequency:") {
        let mhz = line.split("Frequency:").nth(1)?.split_whitespace().next()?.parse::<u32>().ok()?;
        let busy = parse_pct_after(line, "Busy=")?;
        let util = parse_pct_after(line, "Utilization=")?;
        return Some(LigfxSample::Sys { mhz, busy, util });
    }
    // 进程行：含 "GVM_" 前缀的进程名
    if line.contains("GVM_") {
        // GVM_com.lixiang.eid-8925: Busy=15.38%, ..., Utilization=15.38%
        let gvm_pos = line.find("GVM_")?;
        let after = &line[gvm_pos + 4..]; // 去掉 "GVM_"
        let name_end = after.find(':').unwrap_or(after.len());
        let name = after[..name_end].split('-').next()?.trim().to_string();
        let busy = parse_pct_after(line, "Busy=")?;
        let util = parse_pct_after(line, "Utilization=")?;
        return Some(LigfxSample::Proc { name, busy, util });
    }
    None
}

/// 从 "key=12.34%" 格式中提取 f32
fn parse_pct_after(line: &str, key: &str) -> Option<f32> {
    let pos = line.find(key)? + key.len();
    line[pos..].split('%').next()?.trim().parse().ok()
}


/// 返回 None 表示无 Memory snapshot 段（该设备不支持）。
fn parse_gpu_mem_snapshot(out: &str) -> Option<(u64, Vec<(u32, u64)>)> {
    if !out.contains("Memory snapshot") {
        return None;
    }
    let mut global = None;
    let mut procs = Vec::new();
    for line in out.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Global total:") {
            global = rest.trim().parse().ok();
        } else if let Some(rest) = line.strip_prefix("Proc ") {
            // "778 total: 628928512"
            if let Some((pid_s, rest2)) = rest.split_once(' ') {
                if let (Ok(pid), Some(bytes)) = (
                    pid_s.parse::<u32>(),
                    rest2.trim().strip_prefix("total:").and_then(|s| s.trim().parse::<u64>().ok()),
                ) {
                    procs.push((pid, bytes));
                }
            }
        }
    }
    Some((global?, procs))
}

/// 解析 dumpsys thermalservice：
/// - "Thermal Status: N" → 热降频状态（Android ThermalStatus 0-6）
/// - "Temperature{mValue=30.8, mType=3, mName=..., mStatus=0}" → 各传感器
///
/// 输出含 Cached/Current HAL 两个温度区块（HAL 在后且更准），后者覆盖前者。
fn parse_thermalservice(out: &str) -> (Option<i32>, Vec<(String, i32, f32)>) {
    let mut status = None;
    let mut sensors: Vec<(String, i32, f32)> = Vec::new();
    for line in out.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Thermal Status:") {
            status = rest.trim().parse().ok();
        } else if line.contains("temperatures") {
            sensors.clear(); // 新温度区块（Cached → Current HAL），保留最后一个
        } else if let Some(start) = line.find("Temperature{") {
            if let Some(s) = parse_temperature_entry(&line[start..]) {
                sensors.push(s);
            }
        }
    }
    (status, sensors)
}

/// 解析 "Temperature{mValue=30.8, mType=3, mName=test temperature sensor, mStatus=0}"
/// 名字可含空格/逗号，以 ", mStatus=" 为右边界。
fn parse_temperature_entry(s: &str) -> Option<(String, i32, f32)> {
    let value = s.split("mValue=").nth(1)?.split(',').next()?.trim().parse().ok()?;
    let type_ = s.split("mType=").nth(1)?.split(',').next()?.trim().parse().ok()?;
    let name_start = s.find("mName=")? + "mName=".len();
    let name_end = s.rfind(", mStatus=").unwrap_or(s.len());
    let name = s.get(name_start..name_end.min(s.len()))?.trim().to_string();
    Some((name, type_, value))
}

/// 读取 sysfs thermal zones（兜底：thermalservice 无数据时用，如 SS2MAX）。
/// /sys/class/thermal/thermal_zoneN/{type,temp}：type=传感器名，temp=millidegree Celsius。
/// 走 shell cat（SELinux 下 agent 可能无法直接 open sysfs，但 shell 命令可以）。
fn read_sysfs_thermal_zones() -> (Option<i32>, Vec<(String, i32, f32)>) {
    let cmd = "for z in /sys/class/thermal/thermal_zone*; do echo \"$(cat $z/type 2>/dev/null) $(cat $z/temp 2>/dev/null)\"; done";
    let out = match std::process::Command::new("sh")
        .args(["-c", cmd])
        .output() {
        Ok(o) => o,
        Err(_) => return (None, Vec::new()),
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut sensors = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() { continue; }
        // "aoss0-usr 39000" → ("aoss0-usr", 39.0)
        let mut parts = line.rsplitn(2, ' ');
        let temp_str = match parts.next() { Some(s) => s, None => continue };
        let name = match parts.next() { Some(s) => s.to_string(), None => continue };
        if let Ok(milli) = temp_str.parse::<i64>() {
            sensors.push((name, 0, milli as f32 / 1000.0));
        }
    }
    if sensors.is_empty() { return (None, sensors); }
    (Some(0), sensors)
}

// ---------- 输出 ----------

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn emit(line: &str) {
    let mut out = std::io::stdout().lock();
    // 对端断开（adb 连接关闭）时写失败，直接退出
    if writeln!(out, "{}", line).is_err() || out.flush().is_err() {
        std::process::exit(0);
    }
}

// ---------- 采样状态 ----------

struct PidState {
    prev_jiffies: u64,
    prev_threads: HashMap<u32, u64>,
    comm_cache: HashMap<u32, String>,
}

// ---------- FPS（SurfaceFlinger 图层帧时间戳，设备端本地 dumpsys）----------
// 与 xperf-core/fps.rs 同源的解析逻辑（agent 零依赖独立发布，有意复制而非共享）。
// 设备端用 Command 直接 exec dumpsys，无 adb shell 拼接，图层名无需引号。

/// 连续零帧达到该采样轮数后重新发现图层（Surface 重建会换名，如 #0 → #1）。
/// 计的是 FPS 采样轮（限频后每轮 ≥500ms，即 ≥5s 持续零帧才重发现）。
const FPS_REDISCOVER_ZERO_ROUNDS: u32 = 10;

/// FPS 采样限频：每多少主循环轮采一次，保证 FPS 有效周期 ≥500ms。
/// dumpsys SurfaceFlinger 单轮开销 ~100ms 级，低间隔下每轮执行会拖垮节拍。
fn fps_every_n_rounds(interval_ms: u64) -> u64 {
    500u64.div_ceil(interval_ms).max(1)
}

struct FpsLayerState {
    name: String,
    /// 上轮时刻 + 上轮缓冲末尾时间戳；None = 未建基线
    last: Option<(Instant, Option<u64>)>,
}

#[derive(Default)]
struct FpsState {
    layers: Vec<FpsLayerState>,
    attempted: bool,
    zero_rounds: u32,
}

/// 解析 --latency 输出：首行刷新周期（跳过），随后每行三列取 actualPresent。
/// 过滤 0（空槽）和 i64::MAX（已入队未上屏哨兵）。
fn parse_latency_output(output: &str) -> Vec<u64> {
    output
        .lines()
        .skip(1)
        .filter_map(|line| {
            let mut cols = line.split_whitespace();
            cols.next()?;
            let actual: u64 = cols.next()?.parse().ok()?;
            (actual > 0 && actual < i64::MAX as u64).then_some(actual)
        })
        .collect()
}

/// jank：相邻帧间隔 > 2×窗口中位间隔（<3 间隔不计；含跨窗口边界帧）。
/// 不按 vsync 阈值：30fps 相机流在 60Hz 屏上间隔 33ms 会被误判全卡。
fn count_jank(prev: Option<u64>, presents: &[u64]) -> u32 {
    let mut intervals: Vec<u64> = Vec::with_capacity(presents.len());
    let mut last = prev;
    for &p in presents {
        if let Some(l) = last {
            intervals.push(p.saturating_sub(l));
        }
        last = Some(p);
    }
    if intervals.len() < 3 {
        return 0;
    }
    let mid = intervals.len() / 2;
    let median = *intervals.select_nth_unstable(mid).1;
    let threshold = median * 2;
    intervals.iter().filter(|&&d| d > threshold).count() as u32
}

/// 全量 dumpsys 解析：BufferStateLayer 块 metadata 的 ownerPID 归属匹配。
/// （直渲染应用的 SurfaceView 图层名常不含包名，如 "SVM Container"，只能靠归属识别）
fn parse_owned_buffer_layers(dump: &str, pid: u32) -> Vec<String> {
    let owner_marker = format!("ownerPID:{}", pid);
    let mut layers = Vec::new();
    let mut current: Option<String> = None;
    for line in dump.lines() {
        if let Some(pos) = line.find("BufferStateLayer (") {
            let start = pos + "BufferStateLayer (".len();
            current = line[start..].find(')').map(|end| line[start..start + end].to_string());
        } else if line.contains("Layer (") {
            current = None;
        } else if let Some(name) = &current {
            if line.contains(&owner_marker) {
                layers.push(name.clone());
                current = None;
            }
        }
    }
    layers
}

/// --list 解析：保留包名匹配的行，去 "<hex> " 别名前缀，去重。
fn parse_list_layers(list: &str, package: &str) -> Vec<String> {
    let mut layers = Vec::new();
    for line in list.lines() {
        let line = line.trim();
        if !line.contains(package) {
            continue;
        }
        let name = match line.split_once(' ') {
            Some((head, rest)) if !head.is_empty() && head.chars().all(|c| c.is_ascii_hexdigit()) => rest.trim(),
            _ => line,
        };
        if !layers.contains(&name.to_string()) {
            layers.push(name.to_string());
        }
    }
    layers
}

fn dumpsys(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("dumpsys").args(args).output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn sf_discover_layers(pid: u32, package: &str) -> Vec<String> {
    let by_owner = dumpsys(&["SurfaceFlinger"])
        .map(|s| parse_owned_buffer_layers(&s, pid))
        .unwrap_or_default();
    if !by_owner.is_empty() {
        return by_owner;
    }
    dumpsys(&["SurfaceFlinger", "--list"])
        .map(|s| parse_list_layers(&s, package))
        .unwrap_or_default()
}

/// 某 PID 一轮 FPS 采样：有帧图层各发一行；全零时发一条零帧行（界面静止是真实状态）。
/// 图层发现：首轮 + 连续零帧达阈值后重做。包名用于兜底匹配（ownerPID 是首选）。
/// 发现为空（进程刚重启 Surface 未建，或应用本无界面）也记零帧轮，
/// 靠阈值节流重试——全量 dump 在此车机 ~1.5s，不能每轮试。
fn fps_sample_round(st: &mut FpsState, pid: u32, package: &str, ts: u64) {
    if !st.attempted || st.zero_rounds >= FPS_REDISCOVER_ZERO_ROUNDS {
        st.layers = sf_discover_layers(pid, package)
            .into_iter()
            .map(|name| FpsLayerState { name, last: None })
            .collect();
        st.attempted = true;
        st.zero_rounds = 0;
    }
    if st.layers.is_empty() {
        st.zero_rounds += 1;
        return;
    }

    let now = Instant::now();
    let mut samples: Vec<(String, f32, u32, u32)> = Vec::new();
    for layer in &mut st.layers {
        let presents = dumpsys(&["SurfaceFlinger", "--latency", &layer.name])
            .map(|s| parse_latency_output(&s))
            .unwrap_or_default();
        if presents.is_empty() {
            match layer.last {
                // 已有基线但本轮读空（dumpsys 失败/缓冲被清）：记零帧样本（zero_rounds
                // 记账与重发现照常）但**保留基线不动**——否则基线被 None 覆盖后，
                // 下轮会把整个 127 帧缓冲误计为新帧，FPS 瞬时虚高数倍。
                Some((_, Some(_))) => {
                    samples.push((layer.name.clone(), 0.0, 0, 0));
                }
                // 无基线（新图层）：建基线
                _ => layer.last = Some((now, None)),
            }
            continue;
        }
        let latest = presents.last().copied();
        let Some((last_t, last_p)) = layer.last.replace((now, latest)) else {
            continue; // 首轮建基线
        };
        let elapsed = (now - last_t).as_secs_f32();
        if elapsed <= 0.0 {
            continue;
        }
        let new_frames: Vec<u64> = match last_p {
            Some(lp) => presents.into_iter().filter(|&t| t > lp).collect(),
            None => presents,
        };
        let fps = new_frames.len() as f32 / elapsed;
        let jank = count_jank(last_p, &new_frames);
        samples.push((layer.name.clone(), fps, new_frames.len() as u32, jank));
    }
    if samples.is_empty() {
        return;
    }

    if samples.iter().any(|s| s.2 > 0) {
        st.zero_rounds = 0;
        samples.retain(|s| s.2 > 0); // 静止图层是噪声，不上报
    } else {
        st.zero_rounds += 1;
        samples.truncate(1); // 全零：一条静止样本即可
    }
    for (layer, fps, frames, jank) in samples {
        emit(&format!(
            "{{\"t\":\"fps\",\"ts\":{},\"pid\":{},\"layer\":\"{}\",\"fps\":{:.2},\"frames\":{},\"jank\":{}}}",
            ts, pid, json_escape(&layer), fps, frames, jank
        ));
    }
}

// ---------- 内存分类明细（dumpsys meminfo，interval ≥ 500ms 时启用）----------
// 低间隔下 dumpsys meminfo 太重（~100ms），退化到 smaps_rollup（只有 Pss/Rss）。

#[derive(Default)]
struct MemBreakdown {
    java: u64,
    native_: u64,
    code: u64,
    stack: u64,
    gfx: u64,
    other: u64,
    sys: u64,
    total: u64,
}

/// 解析 dumpsys meminfo 的 App Summary（与 xperf-core/memory.rs 同逻辑）。
/// 注意真机格式：分类行之后有一个空行，然后才是 "TOTAL PSS: ... TOTAL RSS: ..." 行——
/// 空行会结束 App Summary 区块，所以 TOTAL 必须在区块外用兜底逻辑找。
fn parse_meminfo_summary(output: &str) -> Option<MemBreakdown> {
    let mut bd = MemBreakdown::default();
    let mut in_summary = false;
    let mut header_passed = false;
    for line in output.lines() {
        let line = line.trim();
        if line.contains("App Summary") {
            in_summary = true;
            continue;
        }
        if in_summary && (line.contains("Pss(KB)") || line.contains("------")) {
            header_passed |= line.contains("------");
            continue;
        }
        if in_summary && line.is_empty() {
            in_summary = false;
            continue;
        }
        if in_summary && header_passed {
            if let Some((cat, rest)) = line.split_once(':') {
                if let Some(Ok(kb)) = rest.split_whitespace().next().map(|s| s.parse::<u64>()) {
                    match cat.trim() {
                        "Java Heap" => bd.java = kb,
                        "Native Heap" => bd.native_ = kb,
                        "Code" => bd.code = kb,
                        "Stack" => bd.stack = kb,
                        "Graphics" => bd.gfx = kb,
                        "Private Other" => bd.other = kb,
                        "System" => bd.sys = kb,
                        "TOTAL" | "TOTAL PSS" => bd.total = kb,
                        _ => {}
                    }
                }
            }
        }
        // 兜底：TOTAL PSS 在 App Summary 空行之后（区块外）
        if !in_summary {
            if let Some(rest) = line.strip_prefix("TOTAL PSS:") {
                if let Some(Ok(kb)) = rest.split_whitespace().next().map(|s| s.parse::<u64>()) {
                    bd.total = kb;
                }
            }
        }
    }
    (bd.total > 0).then_some(bd)
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(2);
        }
    };

    let Some((mut prev_total, ncores)) = read_total_jiffies() else {
        eprintln!("无法读取 /proc/stat");
        std::process::exit(1);
    };
    // 每核最大频率（KHz），与 freq 事件的 khz 数组下标对应
    let maxkhz: Vec<u64> = (0..ncores).map(|i| read_cpufreq(i, "cpuinfo_max_freq").unwrap_or(0)).collect();
    // QNX 地址：--qnx-host 参数覆盖默认值
    if let Some(ref host) = args.qnx_host {
        set_qnx_host(host);
    }
    emit(&format!(
        "{{\"t\":\"hello\",\"ncores\":{},\"maxkhz\":[{}],\"version\":1}}",
        ncores,
        maxkhz.iter().map(u64::to_string).collect::<Vec<_>>().join(",")
    ));

    // B 类指标启动探测（探测失败发 err 并禁用，不影响其余指标）
    let mut freq_enabled = args.freq;
    if freq_enabled && maxkhz.iter().all(|&f| f == 0) && read_cpu_freqs(ncores).iter().all(|&f| f == 0) {
        emit("{\"t\":\"err\",\"msg\":\"cpufreq sysfs 不可用，--freq 已禁用\"}");
        freq_enabled = false;
    }
    let gpu_path = if args.gpu {
        let g = detect_gpu_path_ex(args.platform.as_deref());
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

    // QNX GPU 通道：独立读线程（slog2info -w 流式输出，不占节拍循环）。
    // pid_names：QNX 进程行按进程名归因到 Android PID（QNX 显示名与 /proc/<pid>/comm 一致）。
    let pid_names: Arc<Mutex<HashMap<String, u32>>> = Arc::new(Mutex::new(HashMap::new()));
    if matches!(gpu_path, Some(GpuPath::Qnx)) {
        // kgsl 统计周期跟随采样间隔（clamp [100, 1000]ms：50ms 实测稳定，过短 busy% 窗口噪声大）
        let qnx_period = args.interval_ms.clamp(100, 1000);
        match spawn_qnx_gpu(qnx_period) {
            Some((mut child, stdin, mut reader)) => {
                let pid_names2 = pid_names.clone();
                std::thread::spawn(move || {
                    use std::io::BufRead;
                    let _stdin = stdin; // 持有保持管道存活（drop 即 EOF，telnet 会退出）
                    let mut line = String::new();
                    loop {
                        line.clear();
                        match reader.read_line(&mut line) {
                            Ok(0) | Err(_) => {
                                let _ = child.kill();
                                emit("{\"t\":\"err\",\"msg\":\"QNX GPU 流断开，--gpu 停止\"}");
                                return;
                            }
                            Ok(_) => {
                                match parse_qnx_gpu_line(&line) {
                                    Some(QnxGpuSample::Sys { mhz, maxmhz, busy, util }) => emit(&format!(
                                        "{{\"t\":\"gpu\",\"ts\":{},\"busy\":{:.2},\"util\":{:.2},\"mhz\":{},\"maxmhz\":{}}}",
                                        now_ms(), busy, util, mhz, maxmhz
                                    )),
                                    Some(QnxGpuSample::Proc { name, busy }) => {
                                        let pid = pid_names2.lock().unwrap().get(&name).copied();
                                        if let Some(pid) = pid {
                                            emit(&format!(
                                                "{{\"t\":\"gpuproc\",\"ts\":{},\"pid\":{},\"busy\":{:.2}}}",
                                                now_ms(), pid, busy
                                            ));
                                        }
                                    }
                                    None => {}
                                }
                            }
                        }
                    }
                });
            }
            None => emit("{\"t\":\"err\",\"msg\":\"QNX 通道启动失败（telnet 登录或 kgsl 统计开启失败），--gpu 停止\"}"),
        }
    }

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
            for pid in resolve_pids(pkg) {
                if !active_pids.contains(&pid) {
                    active_pids.push(pid);
                }
            }
        }
        for &pid in &active_pids.clone() {
            let pkg = match &args.package {
                Some(p) => p.clone(),
                None => pkg_cache
                    .entry(pid)
                    .or_insert_with(|| {
                        fs::read_to_string(format!("/proc/{}/cmdline", pid))
                            .map(|s| s.trim_end_matches('\0').to_string())
                            .unwrap_or_default()
                    })
                    .clone(),
            };
            // 首轮仅建图层列表 + 帧时间戳基线，不出数
            fps_sample_round(fps_states.entry(pid).or_default(), pid, &pkg, now_ms());
        }
    }

    // 绝对节拍：按起始时间推算每轮时刻，避免 sleep 累积漂移
    let interval = Duration::from_millis(args.interval_ms);
    // QNX GPU 路径：进程行归因映射初始填充（后续随重扫每 ~1s 更新）
    if matches!(gpu_path, Some(GpuPath::Qnx)) {
        let mut m = pid_names.lock().unwrap();
        for &pid in &active_pids {
            if let Ok(c) = fs::read_to_string(format!("/proc/{}/comm", pid)) {
                m.insert(c.trim().to_string(), pid);
            }
        }
    }
    // TopGpu 路径（SS2MAX）：独立读线程解析 topgpu 输出流
    if matches!(gpu_path, Some(GpuPath::TopGpu)) {
        let period_s = (args.interval_ms / 1000).max(1);
        match spawn_topgpu(period_s) {
            Some((mut child, mut reader)) => {
                let pid_names2 = pid_names.clone();
                std::thread::spawn(move || {
                    use std::io::BufRead;
                    let mut line = String::new();
                    loop {
                        line.clear();
                        match reader.read_line(&mut line) {
                            Ok(0) | Err(_) => { let _ = child.kill(); return; }
                            Ok(_) => match parse_topgpu_line(&line) {
                                Some(TopGpuSample::Sys(busy)) => emit(&format!(
                                    "{{\"t\":\"gpu\",\"ts\":{},\"busy\":{:.2},\"mhz\":0}}", now_ms(), busy
                                )),
                                Some(TopGpuSample::Proc(name, busy)) => {
                                    let pid = pid_names2.lock().unwrap().get(&name).copied();
                                    if let Some(pid) = pid {
                                        emit(&format!(
                                            "{{\"t\":\"gpuproc\",\"ts\":{},\"pid\":{},\"busy\":{:.2}}}",
                                            now_ms(), pid, busy
                                        ));
                                    }
                                }
                                None => {}
                            },
                        }
                    }
                });
            }
            None => emit("{\"t\":\"err\",\"msg\":\"topgpu 启动失败，--gpu 停止\"}"),
        }
    }
    // Ligfx 路径（SS4）：独立读线程解析 logcat ligfxprofilerd 流
    if matches!(gpu_path, Some(GpuPath::Ligfx)) {
        match spawn_ligfx() {
            Some((mut child, mut reader)) => {
                let pid_names2 = pid_names.clone();
                std::thread::spawn(move || {
                    use std::io::BufRead;
                    let mut line = String::new();
                    loop {
                        line.clear();
                        match reader.read_line(&mut line) {
                            Ok(0) | Err(_) => { let _ = child.kill(); return; }
                            Ok(_) => match parse_ligfx_line(&line) {
                                Some(LigfxSample::Sys { mhz, busy, util }) => emit(&format!(
                                    "{{\"t\":\"gpu\",\"ts\":{},\"busy\":{:.2},\"util\":{:.2},\"mhz\":{}}}",
                                    now_ms(), busy, util, mhz
                                )),
                                Some(LigfxSample::Proc { name, busy, util: _ }) => {
                                    let pid = pid_names2.lock().unwrap().get(&name).copied();
                                    if let Some(pid) = pid {
                                        emit(&format!(
                                            "{{\"t\":\"gpuproc\",\"ts\":{},\"pid\":{},\"busy\":{:.2}}}",
                                            now_ms(), pid, busy
                                        ));
                                    }
                                }
                                None => {}
                            },
                        }
                    }
                });
            }
            None => emit("{\"t\":\"err\",\"msg\":\"ligfxprofilerd logcat 启动失败，--gpu 停止\"}"),
        }
    }
    let start = Instant::now();
    let mut round: u64 = 0;
    // 进程列表重扫间隔：约 1s 一次（低间隔下每轮扫 /proc 太贵）
    let rescan_rounds = (1000 / args.interval_ms).max(1);
    // FPS 限频：与 CPU/内存节拍解耦，有效周期 ≥500ms（50ms 间隔 → 每 10 轮一次）
    let fps_every = fps_every_n_rounds(args.interval_ms);
    // 温度限频：dumpsys thermalservice ~50ms 级，≥2s 一轮（温度变化慢；低间隔下避免频繁拖长节拍轮）
    let thermal_every = 2000u64.div_ceil(args.interval_ms).max(1);
    let mut thermal_warned = false;
    // GPU 显存降级路径限频：dumpsys gpu ~11ms，≥1s 一轮
    let gpumem_every = 1000u64.div_ceil(args.interval_ms).max(1);

    loop {
        round += 1;
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
            let found = resolve_pids(pkg);
            for pid in &found {
                if !active_pids.contains(pid) {
                    active_pids.push(*pid); // 新 PID 首轮建基线（states 中无记录）
                }
            }
            active_pids.retain(|p| found.contains(p));
            if found.is_empty() {
                // 每轮重扫都报：让主机读循环在无进程期间也能定期收到行（保持 Ctrl-C 响应）
                emit("{\"t\":\"noproc\"}");
            }
            // QNX/TopGpu/Ligfx GPU 路径：进程行按 comm 名归因，重扫时同步映射表
            if matches!(gpu_path, Some(GpuPath::Qnx) | Some(GpuPath::TopGpu) | Some(GpuPath::Ligfx)) {
                let mut m = pid_names.lock().unwrap();
                m.clear();
                for &pid in &active_pids {
                    if let Ok(c) = fs::read_to_string(format!("/proc/{}/comm", pid)) {
                        m.insert(c.trim().to_string(), pid);
                    }
                }
            }
        }

        let Some((total, _)) = read_total_jiffies() else {
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
            let khz = read_cpu_freqs(ncores);
            emit(&format!(
                "{{\"t\":\"freq\",\"ts\":{},\"khz\":[{}]}}",
                ts,
                khz.iter().map(u64::to_string).collect::<Vec<_>>().join(",")
            ));
        }

        // 网络：整机口径计数器差值 → KB/s（首轮建基线不出数）
        if args.net {
            if let Some((rx, tx)) = read_net_dev() {
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
        // QNX 路径由独立读线程发事件（不占节拍）；
        // 保底路径：dumpsys gpu 每 PID 显存（限频 ≥1s）
        match &gpu_path {
            Some(GpuPath::Kgsl(g)) => {
                if let Some((busy, total)) = read_gpu_busy(g.busy_path) {
                    if let Some((pb, pt)) = prev_gpu.replace((busy, total)) {
                        let dtotal = total.saturating_sub(pt);
                        if dtotal > 0 {
                            let pct = busy.saturating_sub(pb) as f32 / dtotal as f32 * 100.0;
                            let mhz = g.clk_path.and_then(read_u64_file).map(|hz| hz / 1_000_000).unwrap_or(0);
                            emit(&format!("{{\"t\":\"gpu\",\"ts\":{},\"busy\":{:.2},\"mhz\":{}}}", ts, pct, mhz));
                        }
                    }
                }
            }
            Some(GpuPath::Qnx) => {
                // QNX 路径的 busy%/util%/频率由读线程异步发；这里补采 dumpsys gpu 显存（限频 ≥1s）
                if round.is_multiple_of(gpumem_every) && !active_pids.is_empty() {
                    if let Some((global, procs)) = dumpsys(&["gpu"]).as_deref().and_then(parse_gpu_mem_snapshot) {
                        for &pid in &active_pids {
                            let bytes = procs.iter().find(|(p, _)| *p == pid).map(|(_, b)| *b).unwrap_or(0);
                            emit(&format!(
                                "{{\"t\":\"gpumem\",\"ts\":{},\"pid\":{},\"bytes\":{},\"global\":{}}}",
                                ts, pid, bytes, global
                            ));
                        }
                    }
                }
            }
            Some(GpuPath::DumpMem) => {
                if round.is_multiple_of(gpumem_every) && !active_pids.is_empty() {
                    if let Some((global, procs)) = dumpsys(&["gpu"]).as_deref().and_then(parse_gpu_mem_snapshot) {
                        for &pid in &active_pids {
                            let bytes = procs.iter().find(|(p, _)| *p == pid).map(|(_, b)| *b).unwrap_or(0);
                            emit(&format!(
                                "{{\"t\":\"gpumem\",\"ts\":{},\"pid\":{},\"bytes\":{},\"global\":{}}}",
                                ts, pid, bytes, global
                            ));
                        }
                    }
                }
            }
            Some(GpuPath::TopGpu) => {
                // TopGpu 路径：由独立读线程异步发事件；补采 dumpsys gpu 显存
                if round.is_multiple_of(gpumem_every) && !active_pids.is_empty() {
                    if let Some((global, procs)) = dumpsys(&["gpu"]).as_deref().and_then(parse_gpu_mem_snapshot) {
                        for &pid in &active_pids {
                            let bytes = procs.iter().find(|(p, _)| *p == pid).map(|(_, b)| *b).unwrap_or(0);
                            emit(&format!(
                                "{{\"t\":\"gpumem\",\"ts\":{},\"pid\":{},\"bytes\":{},\"global\":{}}}",
                                ts, pid, bytes, global
                            ));
                        }
                    }
                }
            }
            Some(GpuPath::Ligfx) => {
                // Ligfx 路径：由独立读线程异步发事件；补采 dumpsys gpu 显存
                if round.is_multiple_of(gpumem_every) && !active_pids.is_empty() {
                    if let Some((global, procs)) = dumpsys(&["gpu"]).as_deref().and_then(parse_gpu_mem_snapshot) {
                        for &pid in &active_pids {
                            let bytes = procs.iter().find(|(p, _)| *p == pid).map(|(_, b)| *b).unwrap_or(0);
                            emit(&format!(
                                "{{\"t\":\"gpumem\",\"ts\":{},\"pid\":{},\"bytes\":{},\"global\":{}}}",
                                ts, pid, bytes, global
                            ));
                        }
                    }
                }
            }
            None => {}
        }

        // 温度/热降频：dumpsys thermalservice 优先，失败则 sysfs thermal zones 兜底
        // 限频 ≥2s 一轮（dumpsys ~50ms 会拖长低间隔节拍轮）
        if args.thermal && round.is_multiple_of(thermal_every) {
            let from_thermal = dumpsys(&["thermalservice"]).as_deref().map(parse_thermalservice);
            let (status, sensors) = match from_thermal {
                // thermalservice 有数据才走（sensors 非空），否则走 sysfs 兜底
                Some((st, s)) if !s.is_empty() => (st, s),
                _ => read_sysfs_thermal_zones(), // 兜底：sysfs（shell cat，绕过 SELinux）
            };
            if !sensors.is_empty() {
                let sensors_json: Vec<String> = sensors
                    .iter()
                    .map(|(name, type_, value)| format!("[\"{}\",{},{:.1}]", json_escape(name), type_, value))
                    .collect();
                emit(&format!(
                    "{{\"t\":\"temp\",\"ts\":{},\"status\":{},\"sensors\":[{}]}}",
                    ts,
                    status.unwrap_or(-1),
                    sensors_json.join(",")
                ));
            } else if !thermal_warned {
                emit("{\"t\":\"err\",\"msg\":\"thermalservice 与 sysfs thermal zones 均无温度数据\"}");
                thermal_warned = true;
            }
        }

        let mut exited: Vec<u32> = Vec::new();
        for &pid in &active_pids {
            let proc_path = format!("/proc/{}/stat", pid);
            let Some(proc_jiffies) = read_stat_jiffies(&proc_path) else {
                exited.push(pid);
                continue;
            };

            // CPU：与上轮取差（首轮建基线不出数）
            if args.cpu {
                if let Some(st) = states.get_mut(&pid) {
                    let cpu = proc_jiffies.saturating_sub(st.prev_jiffies) as f32
                        / total_delta as f32
                        * 100.0
                        * ncores as f32;
                    st.prev_jiffies = proc_jiffies;

                    // 线程级：读 task/ 下所有 tid
                    let mut th_json = String::new();
                    if let Ok(tids) = fs::read_dir(format!("/proc/{}/task", pid)) {
                        let mut threads: Vec<u32> = tids
                            .flatten()
                            .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
                            .collect();
                        threads.sort_unstable();
                        for tid in threads {
                            let tj = read_stat_jiffies(&format!("/proc/{}/task/{}/stat", pid, tid));
                            let Some(tj) = tj else { continue };
                            let prev = st.prev_threads.insert(tid, tj);
                            if let Some(prev) = prev {
                                let tcpu = tj.saturating_sub(prev) as f32
                                    / total_delta as f32
                                    * 100.0
                                    * ncores as f32;
                                if tcpu > 0.05 {
                                    let name = st.comm_cache.entry(tid).or_insert_with(|| {
                                        fs::read_to_string(format!("/proc/{}/task/{}/comm", pid, tid))
                                            .map(|s| s.trim().to_string())
                                            .unwrap_or_else(|_| "?".into())
                                    });
                                    th_json.push_str(&format!(
                                        ",[{},\"{}\",{:.2}]",
                                        tid,
                                        json_escape(name),
                                        tcpu
                                    ));
                                }
                            }
                        }
                        // 清掉已退出线程的状态
                        st.prev_threads.retain(|tid, _| {
                            fs::metadata(format!("/proc/{}/task/{}", pid, tid)).is_ok()
                        });
                    }
                    emit(&format!(
                        "{{\"t\":\"cpu\",\"ts\":{},\"pid\":{},\"cpu\":{:.2},\"th\":[{}]}}",
                        ts, pid, cpu, th_json.trim_start_matches(',')
                    ));
                } else {
                    states.insert(
                        pid,
                        PidState {
                            prev_jiffies: proc_jiffies,
                            prev_threads: HashMap::new(),
                            comm_cache: HashMap::new(),
                        },
                    );
                }
            }

            // 内存：≥500ms 用 dumpsys meminfo（全分类明细）；低间隔用 smaps_rollup（Pss/Rss）
            if args.memory {
                if full_meminfo {
                    if let Some(bd) = dumpsys(&["meminfo", &pid.to_string()])
                        .and_then(|s| parse_meminfo_summary(&s))
                    {
                        // rss 不在 App Summary 里，从 smaps_rollup 补（~1ms）
                        let rss = read_smaps_rollup(pid).map(|(_, r)| r).unwrap_or(0);
                        emit(&format!(
                            "{{\"t\":\"mem\",\"ts\":{},\"pid\":{},\"pss\":{},\"rss\":{},\"java\":{},\"native\":{},\"code\":{},\"stack\":{},\"gfx\":{},\"other\":{},\"sys\":{}}}",
                            ts, pid, bd.total, rss, bd.java, bd.native_, bd.code, bd.stack, bd.gfx, bd.other, bd.sys
                        ));
                    }
                } else if let Some((pss, rss)) = read_smaps_rollup(pid) {
                    emit(&format!(
                        "{{\"t\":\"mem\",\"ts\":{},\"pid\":{},\"pss\":{},\"rss\":{},\"java\":0,\"native\":0,\"code\":0,\"stack\":0,\"gfx\":0,\"other\":0,\"sys\":0}}",
                        ts, pid, pss, rss
                    ));
                }
            }

            // IO：/proc/<pid>/io 计数器差值 → KB/s（首轮建基线不出数）
            if args.io {
                if let Some((r, w, dr, dw)) = read_pid_io(pid) {
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
            }

            // FPS：设备端本地 dumpsys SurfaceFlinger（图层发现 + 帧时间戳差值）。
            // 限频执行（每 fps_every 轮一次）；启动时已预热建基线，
            // 首个 FPS 轮（round == fps_every）即覆盖一个完整周期。
            if args.fps && round.is_multiple_of(fps_every) {
                let pkg = match &args.package {
                    Some(p) => p.clone(),
                    None => pkg_cache.entry(pid).or_insert_with(|| {
                        fs::read_to_string(format!("/proc/{}/cmdline", pid))
                            .map(|s| s.trim_end_matches('\0').to_string())
                            .unwrap_or_default()
                    }).clone(),
                };
                let st = fps_states.entry(pid).or_default();
                fps_sample_round(st, pid, &pkg, ts);
            }
        }

        for pid in exited {
            active_pids.retain(|&p| p != pid);
            states.remove(&pid);
            fps_states.remove(&pid);
            io_states.remove(&pid);
            pkg_cache.remove(&pid);
            emit(&format!("{{\"t\":\"exit\",\"pid\":{}}}", pid));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_stat_jiffies_basic() {
        let stat = "29697 (xiang.car.x.svm) S 1 2 3 0 -1 0 0 0 0 0 24130 38458 0 0 20 0 1 0";
        assert_eq!(parse_stat_jiffies(stat), Some(24130 + 38458));
    }

    #[test]
    fn test_parse_stat_jiffies_comm_with_spaces() {
        let stat = "123 (Signal Catcher) S 1 2 3 0 -1 0 0 0 0 0 100 200 0 0 20 0 1 0";
        assert_eq!(parse_stat_jiffies(stat), Some(300));
    }

    #[test]
    fn test_parse_stat_jiffies_truncated() {
        assert_eq!(parse_stat_jiffies("123 (comm) S 1 2 3"), None);
    }

    #[test]
    fn test_parse_smaps_rollup_basic() {
        let content = "55f7000000-7g0000000 ---p 00000000 00:00 0 [rollup]\n\
                       Rss:             612000 kB\n\
                       Pss:             484880 kB\n\
                       Shared_Clean:      100 kB\n";
        assert_eq!(parse_smaps_rollup(content), Some((484880, 612000)));
    }

    #[test]
    fn test_parse_smaps_rollup_missing_fields() {
        assert_eq!(parse_smaps_rollup("Rss: 100 kB\n"), None);
    }

    #[test]
    fn test_json_escape() {
        assert_eq!(json_escape("a\"b\\c"), "a\\\"b\\\\c");
    }

    #[test]
    fn test_fps_every_n_rounds() {
        // FPS 有效周期 ≥500ms：低间隔限频，≥500ms 每轮都采
        assert_eq!(fps_every_n_rounds(50), 10); // 50ms → 每 10 轮（500ms）
        assert_eq!(fps_every_n_rounds(300), 2); // 300ms → 每 2 轮（600ms）
        assert_eq!(fps_every_n_rounds(500), 1);
        assert_eq!(fps_every_n_rounds(1000), 1);
    }

    // ---- FPS / meminfo 解析（与 xperf-core 同源逻辑的设备端副本）----

    #[test]
    fn test_parse_latency_filters_zero_and_sentinel() {
        // 真机实测：空槽为 0；已入队未上屏的行 actualPresent = i64::MAX
        let out = "16666666\n2102436563608017\t2102436592973938\t2102436567588086\n0\t0\t0\n2102436613000155\t9223372036854775807\t2102436617210308\n";
        assert_eq!(parse_latency_output(out), vec![2102436592973938]);
    }

    #[test]
    fn test_count_jank_30fps_on_60hz_not_janky() {
        // 30fps 相机流在 60Hz 屏上帧间隔 33ms：中位数自适应，不能误判全卡
        let p0 = 1_000_000_000u64;
        let presents: Vec<u64> = (0..10).map(|i| p0 + i * 33_333_333).collect();
        assert_eq!(count_jank(None, &presents), 0);
    }

    #[test]
    fn test_count_jank_detects_stall() {
        let p0 = 1_000_000_000u64;
        let presents = vec![p0, p0 + 16_666_666, p0 + 33_333_332, p0 + 33_333_332 + 80_000_000];
        assert_eq!(count_jank(None, &presents), 1);
    }

    #[test]
    fn test_parse_owned_buffer_layers() {
        let dump = "+ BufferStateLayer (SVM Container#0) uid=1000\n\
                    \x20     metadata={dequeueTime:123, ownerPID:29697, ownerUID:1000}\n\
                    + BufferStateLayer (com.other/Main#0) uid=10123\n\
                    \x20     metadata={ownerPID:9999}\n";
        assert_eq!(parse_owned_buffer_layers(dump, 29697), vec!["SVM Container#0".to_string()]);
    }

    #[test]
    fn test_parse_list_layers_strips_hex_alias() {
        let list = "147955a com.pkg/com.pkg.MainActivity#0\ncom.pkg/com.pkg.MainActivity#0\ncom.other/Main#0\n";
        assert_eq!(parse_list_layers(list, "com.pkg"), vec!["com.pkg/com.pkg.MainActivity#0".to_string()]);
    }

    #[test]
    fn test_parse_meminfo_summary() {
        // 真机格式：双列（Pss/Rss），分类行与 TOTAL PSS 之间隔一个空行
        let out = " App Summary\n\
                   \x20                       Pss(KB)                        Rss(KB)\n\
                   \x20                        ------                         ------\n\
                   \x20           Java Heap:     8296                          31012\n\
                   \x20         Native Heap:   117492                         122204\n\
                   \x20                Code:    36100                         147340\n\
                   \x20               Stack:     2192                           2204\n\
                   \x20            Graphics:        0                              0\n\
                   \x20       Private Other:   314880\n\
                   \x20              System:     5053\n\
                   \x20             Unknown:                                  336624\n\
                   \x20\n\
                   \x20           TOTAL PSS:   484013            TOTAL RSS:   639384      TOTAL SWAP (KB):        0\n\
                   \x20\n\
                   \x20Objects\n";
        let bd = parse_meminfo_summary(out).unwrap();
        assert_eq!(bd.total, 484013);
        assert_eq!(bd.java, 8296);
        assert_eq!(bd.native_, 117492);
        assert_eq!(bd.code, 36100);
    }

    #[test]
    fn test_parse_meminfo_summary_no_total() {
        assert!(parse_meminfo_summary("garbage\n").is_none());
    }

    // ---- B 类指标解析 ----

    #[test]
    fn test_parse_pid_io() {
        let content = "rchar: 78340951\nwchar: 1099734450\nsyscr: 8814340\nsyscw: 29172547\nread_bytes: 16384\nwrite_bytes: 167936\ncancelled_write_bytes: 0\n";
        assert_eq!(parse_pid_io(content), Some((78340951, 1099734450, 16384, 167936)));
        assert_eq!(parse_pid_io("rchar: 1\n"), None); // 字段不全
    }

    #[test]
    fn test_parse_net_dev_skips_virtual_ifaces() {
        // 真机格式：eth* 物理口统计，lo/sit0/tunl0/gre0/dummy0/ip6_vti0 等虚拟口跳过
        let content = "Inter-|   Receive\n face |bytes\n\
                       eth0.4: 2017964286 23774549 0 0 0 0 0 1155 80722878944 59977192 0 0 0 0 0 0\n\
                       eth1: 9355968477 69004472 0 0 0 0 0 0 16194561619 118076019 0 0 0 0 0 0\n\
                       lo: 999 10 0 0 0 0 0 0 888 10 0 0 0 0 0 0\n\
                       sit0: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n\
                       tunl0: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n\
                       gre0: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n\
                       gretap0: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n\
                       dummy0: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n\
                       ip6_vti0: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n\
                       ip6gre0: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n";
        let (rx, tx) = parse_net_dev(content);
        assert_eq!(rx, 2017964286 + 9355968477);
        assert_eq!(tx, 80722878944 + 16194561619); // 不含 lo 的 888
    }

    #[test]
    fn test_parse_gpu_busy() {
        assert_eq!(parse_gpu_busy("12345 67890\n"), Some((12345, 67890)));
        assert_eq!(parse_gpu_busy("12345\n"), None);
    }

    #[test]
    fn test_parse_gpu_mem_snapshot() {
        // 真机 dumpsys gpu 输出（hypervisor 平台）
        let out = "Stable Game Driver: unsupported\n\
                   Pre-release Game Driver: unsupported\n\n\
                   Memory snapshot for GPU 0:\n\
                   Global total: 2639089664\n\
                   Proc 778 total: 628928512\n\
                   Proc 29697 total: 154370048\n";
        let (global, procs) = parse_gpu_mem_snapshot(out).unwrap();
        assert_eq!(global, 2639089664);
        assert_eq!(procs, vec![(778, 628928512), (29697, 154370048)]);
        // 无 Memory snapshot 段 → None（该设备不支持降级路径）
        assert!(parse_gpu_mem_snapshot("garbage\n").is_none());
    }

    #[test]
    fn test_parse_topgpu_line() {
        assert!(matches!(parse_topgpu_line("sys gpu: 20.0%"), Some(TopGpuSample::Sys(v)) if (v - 20.0).abs() < 0.1));
        match parse_topgpu_line("pid 1234 'com.app' gpu: 16.0% (80.0% of sys)") {
            Some(TopGpuSample::Proc(name, busy)) => {
                assert_eq!(name, "com.app");
                assert!((busy - 16.0).abs() < 0.1);
            }
            other => panic!("应为 Proc: {:?}", other.map(|_| ())),
        }
        assert!(parse_topgpu_line("garbage").is_none());
    }

    #[test]
    fn test_parse_ligfx_line() {
        let sys = "05-13 16:55:14.656 21047 I ligfxprofilerd: [GPU0] Frame 4579: Frequency: 1000 Hz, Tasks: 3 total, GSL Timestamp: 27217418, Global: Busy=33.75%, Queued=57.43%, Utilization=33.75%";
        match parse_ligfx_line(sys) {
            Some(LigfxSample::Sys { mhz, busy, util }) => {
                assert_eq!(mhz, 1000);
                assert!((busy - 33.75).abs() < 0.1);
                assert!((util - 33.75).abs() < 0.1);
            }
            other => panic!("应为 Sys: {:?}", other.map(|_| ())),
        }
        let proc = "05-13 16:55:14.656 21047 I ligfxprofilerd: [GPU0]   GVM_com.lixiang.eid-8925: Busy=15.38%, Queued=46.98%, Utilization=15.38%";
        match parse_ligfx_line(proc) {
            Some(LigfxSample::Proc { name, busy, util }) => {
                assert_eq!(name, "com.lixiang.eid");
                assert!((busy - 15.38).abs() < 0.1);
                assert!((util - 15.38).abs() < 0.1);
            }
            other => panic!("应为 Proc: {:?}", other.map(|_| ())),
        }
        assert!(parse_ligfx_line("random logcat line").is_none());
    }

    #[test]
    fn test_parse_qnx_gpu_line() {
        // 真机 slog2info 行（QNX SS3/8295）
        let proc_line = "Sep 02 14:20:14.513  kgsl.94250  slog  100  For process[PID:1758842997] = 'xiang.car.x.svm' the GPU busy = 14.40% with CtxtID = 244 priority = 1";
        match parse_qnx_gpu_line(proc_line) {
            Some(QnxGpuSample::Proc { name, busy }) => {
                assert_eq!(name, "xiang.car.x.svm");
                assert!((busy - 14.40).abs() < 0.01);
            }
            other => panic!("应为 Proc 样本: {:?}", other.map(|_| ())),
        }
        let sys_line = "Sep 02 14:20:15.498  kgsl.94250  slog  100  frame 435653: freq = 506.975174MHz/635Mhz, elapsed time = 5001.131108ms, busy time = 840.570286ms, busy = 16.807603%, utilization = 13.418957%";
        match parse_qnx_gpu_line(sys_line) {
            Some(QnxGpuSample::Sys { mhz, maxmhz, busy, util }) => {
                assert_eq!((mhz, maxmhz), (506, 635));
                assert!((busy - 16.81).abs() < 0.01);
                assert!((util - 13.42).abs() < 0.01);
            }
            other => panic!("应为 Sys 样本: {:?}", other.map(|_| ())),
        }
        // 无关行
        assert!(parse_qnx_gpu_line("random log line").is_none());
        assert!(parse_qnx_gpu_line("frame 1: something without utilization").is_none());
    }

    #[test]
    fn test_parse_thermalservice() {
        // 真机格式：Cached 区块在前，HAL 区块在后（后者覆盖前者）
        let out = "IsStatusOverride: false\n\
                   Thermal Status: 1\n\
                   Cached temperatures:\n\
                   \tTemperature{mValue=30.8, mType=3, mName=test temperature sensor, mStatus=0}\n\
                   HAL Ready: true\n\
                   Current temperatures from HAL:\n\
                   \tTemperature{mValue=42.5, mType=0, mName=soc0, mStatus=1}\n\
                   \tTemperature{mValue=41.0, mType=3, mName=skin, mStatus=0}\n\
                   Current cooling devices from HAL:\n\
                   \tCoolingDevice{mValue=100, mType=0, mName=test cooling device}\n\
                   Temperature static thresholds from HAL:\n\
                   \t{.type = SKIN}\n";
        let (status, sensors) = parse_thermalservice(out);
        assert_eq!(status, Some(1));
        assert_eq!(sensors.len(), 2);
        assert_eq!(sensors[0], ("soc0".to_string(), 0, 42.5));
        assert_eq!(sensors[1], ("skin".to_string(), 3, 41.0));
    }

    #[test]
    fn test_parse_thermalservice_empty() {
        assert_eq!(parse_thermalservice("garbage\n"), (None, Vec::new()));
    }
}
