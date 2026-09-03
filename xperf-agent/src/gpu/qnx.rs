//! QNX host 通道（SS3/8295）：GPU 由 QNX host 管理，GVM 内无 kgsl 任何节点。
//! busybox telnet 登录 QNX（root 免密）→ exec 3> 长活连接写 /dev/kgsl-control 开统计 →
//! slog2info -W 流式读 kgsl slog（-W 不回放历史，-w 会先倒 backlog；grep 挡 VHAL 刷屏）。
//! 统计链为驱动全局且不随会话清理：多链锁步产生重复行（按上一行去重），看门狗兜底停走。
//! 读线程独立于节拍循环，不占用采样轮。

use super::{spawn_stream_parser, GpuEvent};
use crate::emit;
use std::collections::HashMap;
use std::fs;
use std::io::Write;
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

fn qnx_addr() -> String {
    format!("{}:23", qnx_ip())
}

/// 设置 QNX 地址（main 启动时调用一次，必须在任何探测/通道启动之前）
pub(super) fn set_qnx_host(ip: &str) {
    let _ = QNX_IP.set(ip.to_string());
}

/// QNX 通道可用性：busybox 存在 + QNX telnet 端口可连
pub(super) fn available() -> bool {
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
    // 登录/初始化失败时 kill 子进程，避免设备上残留 busybox telnet
    let result = (|| {
        read_until(&mut reader, "login:", deadline)?;
        stdin.write_all(b"root\n").ok()?;
        read_until(&mut reader, "# ", deadline)?;
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
        stdin.write_all(cmds.as_bytes()).ok()?;
        Some(())
    })();
    if result.is_none() {
        let _ = child.kill();
        return None;
    }
    Some((child, stdin, reader))
}

/// 启动通道（start_stream_channels 分发）。
/// kgsl 统计周期跟随采样间隔（clamp [100, 1000]ms：50ms 实测稳定，过短 busy% 窗口噪声大）。
pub(super) fn start(interval_ms: u64, pid_names: &Arc<Mutex<HashMap<String, u32>>>) {
    let period = interval_ms.clamp(100, 1000);
    match spawn_qnx_gpu(period) {
        Some((child, stdin, reader)) => {
            // stdin 共享：读线程持有保活，看门狗锁定自愈重写
            let stdin = Arc::new(Mutex::new(stdin));
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
            spawn_stream_parser(
                child,
                reader,
                Some(stdin.clone()),
                Some("QNX GPU 流断开，--gpu 停止"),
                pid_names,
                parse,
            );
            spawn_watchdog(period, stdin, sys_count);
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
fn spawn_watchdog(period_ms: u64, stdin: Arc<Mutex<std::process::ChildStdin>>, sys_count: Arc<AtomicU64>) {
    std::thread::spawn(move || {
        let period = Duration::from_millis(period_ms);
        // 启动宽限：telnet 登录 + slog2info 起流 + 首个窗口完成需 2~3s
        let start = Instant::now();
        let grace = period * 2 + Duration::from_secs(3);
        let mut last = 0u64;
        let mut misses = 0u32; // 连续无新 frame 的检查次数
        let mut heals = 0u32; // 自愈次数（恢复后归零，长会话可反复自愈）
        loop {
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
                        let Ok(mut w) = stdin.lock() else { return };
                        if w.write_all(cmd.as_bytes()).is_err() {
                            return; // 通道已断（读线程已 kill 子进程），静默退出
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
