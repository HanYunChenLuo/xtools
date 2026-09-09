//! QNX host 通道（SS3/8295）：GPU 由 QNX host 管理，GVM 内无 kgsl 任何节点。
//! **内嵌极简 telnet client**（纯 TCP，无 busybox 依赖——shell 身份下 /vendor/bin/busybox
//! 被 SELinux 拒执行，故非 root 设备本通道同样可用）登录 QNX（root 免密）→
//! exec 3> 长活连接写 /dev/kgsl-control 开统计 → slog2info -W 流式读 kgsl slog
//! （-W 不回放历史，-w 会先倒 backlog；grep 挡 VHAL 刷屏）。
//! 统计链为驱动全局且不随会话清理：多链锁步产生重复行（按上一行去重），看门狗兜底停走。
//! 读线程独立于节拍循环，不占用采样轮。

use super::{spawn_stream_parser, GpuEvent};
use crate::emit;
use std::collections::HashMap;
use std::io::{BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// QNX host（GPU 所在侧）默认地址：SS3/8295 平台固定 172.31.101.52（virtio_net eth1 对端）
const QNX_TELNET_IP_DEFAULT: &str = "172.31.101.52";

/// QNX 地址（全局一份，由 main() 启动时 set 一次，读线程/主循环共享）
static QNX_IP: std::sync::OnceLock<String> = std::sync::OnceLock::new();

fn qnx_ip() -> &'static str {
    QNX_IP.get().map(|s| s.as_str()).unwrap_or(QNX_TELNET_IP_DEFAULT)
}

/// 设置 QNX 地址（main 启动时调用一次，必须在任何探测/通道启动之前）
pub(super) fn set_qnx_host(ip: &str) {
    let _ = QNX_IP.set(ip.to_string());
}

/// telnet 协议字节（RFC 854 最小子集，只够应答 QNX telnetd 的选项协商）
mod telnet {
    /// SE：子协商结束
    pub(super) const SE: u8 = 240;
    /// SB：子协商开始
    pub(super) const SB: u8 = 250;
    pub(super) const WILL: u8 = 251;
    pub(super) const WONT: u8 = 252;
    pub(super) const DO: u8 = 253;
    pub(super) const DONT: u8 = 254;
    /// IAC：命令转义字节
    pub(super) const IAC: u8 = 255;
}

/// 极简 telnet 会话（QNX telnetd 专用）：IAC 协商全拒（DO→WONT / WILL→DONT，
/// QNX telnetd 接受退化为逐字符 linemode）；登录提示符无换行，必须逐字节读。
/// 读半与写半分离：写半（Arc 共享）供看门狗自愈/teardown 停链写入，
/// 读半移交流式读线程（spawn_stream_parser）。
pub(super) struct QnxTelnet {
    reader: BufReader<TcpStream>,
    writer: Arc<Mutex<TcpStream>>,
    /// 上一次读是否因超时失败（读超时用于登录/观察阶段的 deadline 控制；
    /// 流式阶段清除超时后为永久阻塞读，超时标志恒 false）
    timed_out: bool,
}

impl QnxTelnet {
    /// 建连（含 IAC 协商应答准备）；ip=None 用全局/默认地址
    fn connect(ip: Option<&str>) -> Option<Self> {
        use std::net::ToSocketAddrs;
        let addr = format!("{}:23", ip.unwrap_or_else(|| qnx_ip()))
            .to_socket_addrs()
            .ok()?
            .next()?;
        let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).ok()?;
        let _ = tcp.set_nodelay(true);
        // 登录/观察阶段的读超时（500ms 粒度驱动 deadline 轮询）；流式阶段清除
        let _ = tcp.set_read_timeout(Some(Duration::from_millis(500)));
        let writer = tcp.try_clone().ok()?;
        Some(Self { reader: BufReader::new(tcp), writer: Arc::new(Mutex::new(writer)), timed_out: false })
    }

    /// 读一个数据字节（滤除 IAC 序列并应答协商）。None = EOF/错误/读超时
    /// （超时置 timed_out，其余失败清零——调用方据此区分「暂无数据」与「连接死」；
    /// IAC 序列中途的续读同样按此分类，否则登录期高延迟链路会被误判为连接死）。
    fn read_byte(&mut self) -> Option<u8> {
        // 读一个原始字节（首字节与 IAC 序列续读共用）：超时/失败统一分类
        macro_rules! raw {
            ($buf:expr) => {
                match self.reader.read_exact($buf) {
                    Ok(()) => {}
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        self.timed_out = true;
                        return None;
                    }
                    Err(_) => {
                        self.timed_out = false;
                        return None;
                    }
                }
            };
        }
        loop {
            let mut b = [0u8; 1];
            raw!(&mut b);
            self.timed_out = false; // 本次调用已有数据到达（此前可能的超时作废）
            if b[0] != telnet::IAC {
                return Some(b[0]);
            }
            let mut c = [0u8; 1];
            raw!(&mut c);
            match c[0] {
                telnet::IAC => return Some(telnet::IAC), // 0xFF 字面转义
                telnet::DO | telnet::WILL => {
                    let mut opt = [0u8; 1];
                    raw!(&mut opt);
                    let resp = if c[0] == telnet::DO { telnet::WONT } else { telnet::DONT };
                    self.write_raw(&[telnet::IAC, resp, opt[0]]).ok()?;
                }
                telnet::DONT | telnet::WONT => {
                    let mut opt = [0u8; 1];
                    raw!(&mut opt);
                }
                telnet::SB => {
                    // 子协商：吞到 IAC SE
                    let mut prev = 0u8;
                    loop {
                        let mut x = [0u8; 1];
                        raw!(&mut x);
                        if prev == telnet::IAC && x[0] == telnet::SE {
                            break;
                        }
                        prev = x[0];
                    }
                }
                _ => {} // 其余单字节命令（GA/EOR 等）忽略
            }
        }
    }

    fn write_raw(&self, bytes: &[u8]) -> std::io::Result<()> {
        self.writer.lock().unwrap().write_all(bytes)
    }

    fn write_str(&self, s: &str) -> std::io::Result<()> {
        self.write_raw(s.as_bytes())
    }

    /// 读到 marker 出现为止（逐字节；登录提示符无换行）。
    /// 读超时只计 deadline（连接仍有数据就继续），EOF/真错误立即失败。
    fn read_until(&mut self, marker: &str, deadline: Instant) -> Option<()> {
        let mut buf = Vec::new();
        loop {
            match self.read_byte() {
                Some(b) => {
                    buf.push(b);
                    if buf.ends_with(marker.as_bytes()) {
                        return Some(());
                    }
                    if buf.len() > 4096 {
                        buf.drain(..2048); // 防 banner 刷屏时缓冲无限增长
                    }
                }
                None if self.timed_out && Instant::now() < deadline => continue,
                None => return None,
            }
            if Instant::now() > deadline {
                return None;
            }
        }
    }

    /// 登录 QNX（root 免密）并开 kgsl 统计流（fd3 长活连接写入）。
    /// 成功后清除读超时进入流式阻塞读阶段。
    fn login_and_start_stats(&mut self, period_ms: u64) -> Option<()> {
        let deadline = Instant::now() + Duration::from_secs(8);
        self.read_until("login:", deadline)?;
        self.write_str("root\n").ok()?;
        self.read_until("# ", deadline)?;
        // 写入必须走 exec 3> 的长活连接（写入时连接存活 → 统计链重相位后持续输出）；
        // echo > 式即开即死连接撞上存量链时，链只 flush 一个窗口即停（真机实测）。
        // 注意：管道必须后台执行（&）。前台时 shell 阻塞在管道上，
        // 看门狗自愈写入的命令会滞留 tty 缓冲永不执行。
        let cmds = format!(
            "exec 3>/dev/kgsl-control\n\
             echo gpu_set_log_level 4 >&3\n\
             echo gpubusystats {} >&3\n\
             echo gpu_per_process_busy {} >&3\n\
             slog2info -W | grep kgsl &\n",
            period_ms, period_ms
        );
        self.write_str(&cmds).ok()?;
        let _ = self.reader.get_ref().set_read_timeout(None);
        Some(())
    }

    /// 拆出写半（看门狗/teardown 共享）；（自身剩余部分作行源移交读线程）
    fn writer(&self) -> Arc<Mutex<TcpStream>> {
        self.writer.clone()
    }
}

/// 流式读阶段的行源实现（gpu/mod.rs 的 LineReader）：逐字节过滤 IAC 后组行，
/// 非 UTF-8 字节按 lossy 转换（slog 行为 ASCII，防御性处理）。
impl super::LineReader for QnxTelnet {
    fn read_line(&mut self, buf: &mut String) -> std::io::Result<usize> {
        let mut raw = Vec::new();
        loop {
            match self.read_byte() {
                Some(b) => {
                    raw.push(b);
                    if b == b'\n' {
                        break;
                    }
                    if raw.len() > 8192 {
                        break; // 防异常行无界（slog 行 <200 字节）
                    }
                }
                None => {
                    if self.timed_out {
                        continue; // 流式阶段已清超时，防御分支（不触发）
                    }
                    break; // EOF/错误
                }
            }
        }
        if raw.is_empty() {
            return Ok(0);
        }
        let n = raw.len();
        buf.push_str(&String::from_utf8_lossy(&raw));
        Ok(n)
    }
}

/// QNX 通道可用性：QNX telnet 端口可连（内嵌 client 无 busybox 依赖，非 root 可用）
pub(super) fn available() -> bool {
    QnxTelnet::connect(None).is_some()
}

/// QNX kgsl slog 样本（slog2info 流的两类行）：
/// 进程行: `"For process[PID:1758842997] = 'xiang.car.x.svm' the GPU busy = 14.40% with CtxtID = 244 priority = 1"`
///   （PID 是 QNX 侧编号，无意义；按进程名匹配 Android comm）
/// 系统行: `"frame 435653: freq = 506.975174MHz/635Mhz, elapsed time = 5001.13ms, busy time = 840.57ms, busy = 16.81%, utilization = 13.42%"`
fn parse_line(line: &str) -> Option<GpuEvent> {
    if let Some(pos) = line.find("For process[PID:") {
        // 进程名在 = '...' 内；busy 在 "the GPU busy = " 后
        let name_start = line[pos..].find("= '")? + pos + 3;
        let name_end = line[name_start..].find('\'')? + name_start;
        let name = line[name_start..name_end].to_string();
        let busy_pos = line.find("the GPU busy = ")? + "the GPU busy = ".len();
        let busy: f32 = line[busy_pos..].split('%').next()?.trim().parse().ok()?;
        return Some(GpuEvent::Proc { name, busy });
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
        return Some(GpuEvent::Sys { mhz, maxmhz: Some(maxmhz), util: Some(util), busy });
    }
    None
}

/// 启动 QNX GPU 统计流：内嵌 telnet 登录（root 免密）→ 开 kgsl 统计 → slog2info -W 持续跟踪。
/// 返回已开流的 telnet 会话（读半作行源移交读线程；写半 Arc 共享给看门狗/teardown）。
/// agent 退出时 TCP 断开，QNX 侧 shell 会话结束自行清理（同旧 busybox 语义）。
/// period_ms：kgsl 统计周期（实测 50ms 稳定；QNX 侧逐上下文打点，telnet 带宽无压力）
fn spawn_qnx_gpu(period_ms: u64) -> Option<QnxTelnet> {
    let mut t = QnxTelnet::connect(None)?;
    t.login_and_start_stats(period_ms)?;
    Some(t)
}

/// 一次性停链（agent `--qnx-stop` 模式，host 侧 `qnx_stop_stats` 兜底路径的设备端
/// 载体——替代旧 busybox telnet 方案，shell 身份可执行，非 root 可用）：
/// 登录 QNX → 纯观察 ~4s（slog2info -W | grep frame，只读不写 kgsl-control）→
/// frame 流在跑（≥2 行）才发 echo>（死写入者）停止——对已停链写入会把链全部复活
/// （toggle 语义真机实测），不可无条件执行。结果打印一行 stdout（host 记录诊断）。
/// best-effort：任何失败只打印不非零退出（host 在 daemon 异常死亡后才走这里）。
/// `ip`：None 用默认/全局 QNX 地址；`period_ms`：停链写入的周期参数（语义同启动）。
pub(super) fn stop_once(ip: Option<&str>, period_ms: u64) {
    let Some(mut t) = QnxTelnet::connect(ip) else {
        println!("qnx-stop: telnet 连接失败（{}）", ip.unwrap_or(QNX_TELNET_IP_DEFAULT));
        return;
    };
    let deadline = Instant::now() + Duration::from_secs(8);
    let ok = t.read_until("login:", deadline).is_some()
        && t.write_str("root\n").is_ok()
        && t.read_until("# ", deadline).is_some();
    if !ok {
        println!("qnx-stop: telnet 登录失败");
        return;
    }
    if t.write_str("slog2info -W | grep frame &\n").is_err() {
        println!("qnx-stop: 观察命令写入失败");
        return;
    }
    // 纯观察 ~4s：统计 frame 行数（命令回显含一处 "frame"，阈值 ≥2 排除之）
    let observe_until = Instant::now() + Duration::from_secs(4);
    let mut text = String::new();
    while Instant::now() < observe_until {
        match t.read_byte() {
            Some(b) => text.push(b as char),
            None if t.timed_out => continue,
            None => break,
        }
        if text.len() > 16384 {
            text.drain(..8192);
        }
    }
    let flowing = text.matches("frame ").count();
    if flowing < 2 {
        println!("qnx-stop: frame 流未在跑（{} 行命中），不动链", flowing);
        return;
    }
    let cmd = format!("echo gpubusystats {} > /dev/kgsl-control\n", period_ms);
    let stopped = t.write_str(&cmd).is_ok();
    // 给 QNX shell 执行命令的时间（进程退出后连接随之消亡，命令须先落）
    std::thread::sleep(Duration::from_millis(300));
    println!("qnx-stop: frame 流在跑（{} 行命中），停链写入{}", flowing, if stopped { "成功" } else { "失败" });
}

/// 启动通道（start_stream_channels 分发）。io/stop 见 spawn_stream_parser——
/// 会话结束 stop 置位，读线程与看门狗随收；停链 teardown 注册进 crate 的
/// GPU_TEARDOWN 槽（持有会话结束或进程退出时执行一次）。
/// kgsl 统计周期跟随采样间隔（clamp [100, 1000]ms：50ms 实测稳定，过短 busy% 窗口噪声大）。
pub(super) fn start(
    interval_ms: u64,
    pid_names: &Arc<Mutex<HashMap<String, u32>>>,
    io: Option<crate::SessionIo>,
    stop: Arc<std::sync::atomic::AtomicBool>,
) {
    let period = interval_ms.clamp(100, 1000);
    match spawn_qnx_gpu(period) {
        Some(telnet) => {
            // 写半共享：读线程持有保活（Arc 计数），看门狗锁定自愈重写，teardown 停链写入
            let writer = telnet.writer();
            // Sys（frame）样本计数：看门狗据此检测 busy 窗口流停走
            let sys_count = Arc::new(AtomicU64::new(0));
            // kgsl 统计链是驱动全局的，且每次写入会叠加/重相位一条链（会话死亡不清理，
            // 直到整机重启）。多条链锁步时同一行会重复出现 N 份——按"与上一行完全相同"
            // 去重（Sys 与 Proc 各记上一条；链锁步时重复行总是相邻）。
            let dedupe = Arc::new(Mutex::new((String::new(), String::new())));
            let (cnt, ded) = (sys_count.clone(), dedupe.clone());
            let parse = move |line: &str| {
                let ev = parse_line(line);
                match &ev {
                    Some(GpuEvent::Sys { .. }) => {
                        let mut d = ded.lock().unwrap();
                        if d.0 == line {
                            return None; // 锁步链重复行
                        }
                        d.0 = line.to_string();
                        cnt.fetch_add(1, Ordering::Relaxed);
                    }
                    Some(GpuEvent::Proc { .. }) => {
                        let mut d = ded.lock().unwrap();
                        if d.1 == line {
                            return None;
                        }
                        d.1 = line.to_string();
                    }
                    None => {}
                }
                ev
            };
            let cleanup_conn = writer.clone();
            spawn_stream_parser(
                telnet,
                move || {
                    // TCP 断开 → QNX 侧 shell 会话结束（同旧 busybox 子进程被 kill 的语义）
                    let _ = cleanup_conn.lock().unwrap().shutdown(std::net::Shutdown::Both);
                },
                Some(writer.clone()),
                Some("QNX GPU 流断开，--gpu 停止"),
                pid_names,
                io.clone(),
                stop.clone(),
                parse,
            );
            // 停链 teardown：经 telnet 下发 echo>（死写入者）式 gpubusystats 停止全部
            // 统计链（fd3 活连接存在时同样有效，真机实测）。不停链则链泄漏到整机重启，
            // 且下一会话 fd3 写入撞活链会停走、走 ~8s 看门狗自愈路径；停链后下一会话
            // 锁步即起。由持有会话结束或进程退出时执行（crate::run_gpu_teardown）。
            let teardown = writer.clone();
            crate::register_gpu_teardown(Box::new(move || {
                if let Ok(mut w) = teardown.lock() {
                    let _ = w.write_all(
                        format!("echo gpubusystats {} > /dev/kgsl-control\n", period).as_bytes(),
                    );
                }
                // 给 QNX shell 执行命令的时间（agent 退出后连接随之消亡，命令须先落）
                std::thread::sleep(Duration::from_millis(200));
            }));
            spawn_watchdog(period, writer, sys_count, io, stop);
        }
        None => emit("{\"t\":\"err\",\"msg\":\"QNX 通道启动失败（telnet 登录或 kgsl 统计开启失败），--gpu 停止\"}"),
    }
}

/// 看门狗单步决策（纯函数，语义由单测锁定——阈值回退会重新引入长测误伤）。
#[derive(Debug, PartialEq, Eq)]
enum WatchdogAction {
    /// 有新样本：misses/heals 归零
    Progress,
    /// 宽限期内：不计数不动作
    Wait,
    /// 记一次缺失：未达阈值，继续等
    Count,
    /// 连续第 3 次缺失且自愈次数未用尽：重写 gpubusystats
    Heal,
    /// 达阈值但自愈次数用尽：等主机侧重连重建通道
    GiveUp,
}

fn watchdog_step(have_new: bool, past_grace: bool, misses: u32, heals: u32) -> WatchdogAction {
    if have_new {
        return WatchdogAction::Progress;
    }
    if !past_grace {
        return WatchdogAction::Wait;
    }
    if misses + 1 < 3 {
        return WatchdogAction::Count;
    }
    if heals >= 3 {
        return WatchdogAction::GiveUp;
    }
    WatchdogAction::Heal
}

/// QNX kgsl 统计链看门狗（2026-09-03 真机实测的行为兜底）：
/// - 统计链为驱动全局，会话/fd 关闭都不清理；echo> 式死写入者撞存量链只 flush 一窗即停
/// - 长活连接（exec 3>）写入时连接存活 → 存量链全部重相位（计数归零）后持续输出
///
/// 正常路径下 fd3 活写入后 frame 流持续，看门狗不动作；若未知状态导致 frame 流静默，
/// 通过同一会话的 fd3 重写 gpubusystats 自愈（重相位走活写入者路径）。
/// 判定须连续 3 个周期无新样本：实测窗口周期 ~1001.5ms 略长于检查周期 1000ms，
/// 相位每周期漂移 ~1.5ms，长会话中单次缺失是正常漂移（约每 11 分钟必现一次），
/// 3 连续缺失（3 秒级无任何窗口完成）才构成真停走。
/// 先尝试写入、成功才报自愈（通道已断时静默退出，不产生误导 err）；恢复后计数归零。
/// 会话结束（stop 置位）看门狗随收。
fn spawn_watchdog(
    period_ms: u64,
    writer: Arc<Mutex<TcpStream>>,
    sys_count: Arc<AtomicU64>,
    io: Option<crate::SessionIo>,
    stop: Arc<std::sync::atomic::AtomicBool>,
) {
    std::thread::spawn(move || {
        if let Some(io) = io {
            crate::set_session_io(io);
        }
        let period = Duration::from_millis(period_ms);
        // 启动宽限：telnet 登录 + slog2info 起流 + 首个窗口完成需 2~3s
        let start = Instant::now();
        let grace = period * 2 + Duration::from_secs(3);
        let mut last = 0u64;
        let mut misses = 0u32; // 连续无新 frame 的检查次数
        let mut heals = 0u32; // 自愈次数（恢复后归零，长会话可反复自愈）
        loop {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(period);
            let c = sys_count.load(Ordering::Relaxed);
            match watchdog_step(c > last, Instant::now() - start >= grace, misses, heals) {
                WatchdogAction::Progress => {
                    last = c;
                    misses = 0;
                    heals = 0;
                }
                WatchdogAction::Wait => {}
                WatchdogAction::Count => misses += 1,
                WatchdogAction::GiveUp => return,
                WatchdogAction::Heal => {
                    heals += 1;
                    misses = 0;
                    let cmd = format!("echo gpubusystats {} >&3\n", period_ms);
                    {
                        let Ok(mut w) = writer.lock() else { return };
                        if w.write_all(cmd.as_bytes()).is_err() {
                            return; // 通道已断（读线程已断开 TCP），静默退出
                        }
                    }
                    emit(&format!(
                        "{{\"t\":\"err\",\"msg\":\"QNX GPU frame 流停走，经 fd3 重写 gpubusystats 自愈（第 {} 次）\"}}",
                        heals
                    ));
                    // 重相位后首个窗口完成需 ~1-2s，期间不重复检查
                    std::thread::sleep(period * 3);
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_qnx_gpu_line() {
        // 真机 slog2info 行（QNX SS3/8295）
        let proc_line = "Sep 02 14:20:14.513  kgsl.94250  slog  100  For process[PID:1758842997] = 'xiang.car.x.svm' the GPU busy = 14.40% with CtxtID = 244 priority = 1";
        match parse_line(proc_line) {
            Some(GpuEvent::Proc { name, busy }) => {
                assert_eq!(name, "xiang.car.x.svm");
                assert!((busy - 14.40).abs() < 0.01);
            }
            other => panic!("应为 Proc 样本: {:?}", other.map(|_| ())),
        }
        let sys_line = "Sep 02 14:20:15.498  kgsl.94250  slog  100  frame 435653: freq = 506.975174MHz/635Mhz, elapsed time = 5001.131108ms, busy time = 840.570286ms, busy = 16.807603%, utilization = 13.418957%";
        match parse_line(sys_line) {
            Some(GpuEvent::Sys { mhz, maxmhz, util, busy }) => {
                assert_eq!(mhz, 506);
                assert_eq!(maxmhz, Some(635));
                assert!((busy - 16.81).abs() < 0.01);
                assert!((util.unwrap() - 13.42).abs() < 0.01);
            }
            other => panic!("应为 Sys 样本: {:?}", other.map(|_| ())),
        }
        // 无关行
        assert!(parse_line("random log line").is_none());
        assert!(parse_line("frame 1: something without utilization").is_none());
    }

    /// 看门狗决策语义锁定：单次缺失是窗口相位漂移（实测窗口 ~1001.5ms > 检查周期
    /// 1000ms，长会话约每 11 分钟必现一次），3 连续缺失才自愈——阈值回退到 1 会
    /// 重新引入长测误伤（重相位 1-2s 数据缺口 + 误告警行），真机短测无法发现。
    #[test]
    fn test_watchdog_step() {
        use WatchdogAction::*;
        // 有新样本：任何状态下都归零继续
        assert_eq!(watchdog_step(true, false, 0, 0), Progress);
        assert_eq!(watchdog_step(true, true, 2, 2), Progress);
        // 宽限期内：不计数不动作（即使已连续缺失）
        assert_eq!(watchdog_step(false, false, 2, 0), Wait);
        // 单次/两次缺失：只计数不动作
        assert_eq!(watchdog_step(false, true, 0, 0), Count);
        assert_eq!(watchdog_step(false, true, 1, 0), Count);
        // 第 3 次连续缺失 → 自愈
        assert_eq!(watchdog_step(false, true, 2, 0), Heal);
        // 恢复后 heals 归零（Progress 分支），再次 3 缺失仍可自愈
        assert_eq!(watchdog_step(false, true, 2, 1), Heal);
        // 自愈次数用尽 → 放弃
        assert_eq!(watchdog_step(false, true, 2, 3), GiveUp);
    }
}
