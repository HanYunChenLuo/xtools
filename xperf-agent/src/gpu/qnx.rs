//! QNX host 通道（SS3/8295）：GPU 由 QNX host 管理，GVM 内无 kgsl 任何节点。
//! busybox telnet 登录 QNX（root 免密）→ 写 /dev/kgsl-control 开统计 →
//! slog2info -W 流式读 kgsl slog（-W 不回放历史，-w 会先倒 backlog；grep 挡 VHAL 刷屏）。
//! 读线程独立于节拍循环，不占用采样轮。

use super::{spawn_stream_parser, GpuEvent};
use crate::emit;
use std::collections::HashMap;
use std::fs;
use std::io::Write;
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
        let cmds = format!(
            "echo gpu_set_log_level 4 > /dev/kgsl-control\n\
             echo gpubusystats {} > /dev/kgsl-control\n\
             echo gpu_per_process_busy {} > /dev/kgsl-control\n\
             slog2info -W | grep kgsl\n",
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
        Some((child, stdin, reader)) => spawn_stream_parser(
            child,
            reader,
            Some(stdin),
            Some("QNX GPU 流断开，--gpu 停止"),
            pid_names,
            parse_line,
        ),
        None => emit("{\"t\":\"err\",\"msg\":\"QNX 通道启动失败（telnet 登录或 kgsl 统计开启失败），--gpu 停止\"}"),
    }
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
}
