//! SS4 ligfxprofilerd logcat 通道。
//! SS4/8797 平台 GPU 统计由 ligfxprofilerd 服务输出到 logcat，每帧一行系统行 + N 行进程行：
//! ```text
//! [GPU0] Frame N: Frequency: 1000 Hz, ..., Busy=33.75%, ..., Utilization=33.75%
//! [GPU0]   GVM_com.lixiang.eid-8925: Busy=15.38%, ..., Utilization=15.38%
//! ```
//! 业务侧只需关注 Utilization 字段。

use super::{spawn_stream_parser, GpuEvent};
use crate::emit;
use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

/// ligfxprofilerd 可用性：logcat 能拉到 ligfxprofilerd 标签的日志
pub(super) fn available() -> bool {
    // 快速探测：logcat -d -s ligfxprofilerd 取最近日志，有内容即可用
    Command::new("logcat")
        .args(["-d", "-s", "ligfxprofilerd", "-m", "1"])
        .output()
        .ok()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false)
}

/// 启动 ligfxprofilerd logcat 流，返回 (child, reader)
fn spawn() -> Option<(std::process::Child, std::io::BufReader<std::process::ChildStdout>)> {
    use std::io::BufReader;
    let mut child = Command::new("logcat")
        .args(["-s", "ligfxprofilerd"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    Some((child, BufReader::new(stdout)))
}

/// 解析 logcat 行（归一为 GpuEvent）。
/// `"[GPU0] Frame N: Frequency: 1000 Hz, ..., Busy=33.75%, ..., Utilization=33.75%"` → Sys
/// `"[GPU0]   GVM_com.lixiang.eid-8925: Busy=15.38%, ..., Utilization=15.38%"` → Proc
fn parse_line(line: &str) -> Option<GpuEvent> {
    if !line.contains("ligfxprofilerd") {
        return None;
    }
    // 系统行：含 "Frame N:" 和 "Frequency:"
    if line.contains("Frame ") && line.contains("Frequency:") {
        let mhz = line.split("Frequency:").nth(1)?.split_whitespace().next()?.parse::<u32>().ok()?;
        let busy = parse_pct_after(line, "Busy=")?;
        let util = parse_pct_after(line, "Utilization=")?;
        return Some(GpuEvent::Sys { mhz, maxmhz: None, util: Some(util), busy });
    }
    // 进程行：含 "GVM_" 前缀的进程名
    if line.contains("GVM_") {
        // GVM_com.lixiang.eid-8925: Busy=15.38%, ..., Utilization=15.38%
        let gvm_pos = line.find("GVM_")?;
        let after = &line[gvm_pos + 4..]; // 去掉 "GVM_"
        let name_end = after.find(':').unwrap_or(after.len());
        let name = after[..name_end].split('-').next()?.trim().to_string();
        let busy = parse_pct_after(line, "Busy=")?;
        // Utilization 字段缺失的行整体丢弃（保持与原实现一致），值本身不上报
        let _ = parse_pct_after(line, "Utilization=")?;
        return Some(GpuEvent::Proc { name, busy });
    }
    None
}

/// 从 "key=12.34%" 格式中提取 f32
fn parse_pct_after(line: &str, key: &str) -> Option<f32> {
    let pos = line.find(key)? + key.len();
    line[pos..].split('%').next()?.trim().parse().ok()
}

/// 启动通道（start_stream_channels 分发）
pub(super) fn start(pid_names: &Arc<Mutex<HashMap<String, u32>>>) {
    match spawn() {
        Some((child, reader)) => spawn_stream_parser(child, reader, None, None, pid_names, parse_line),
        None => emit("{\"t\":\"err\",\"msg\":\"ligfxprofilerd logcat 启动失败，--gpu 停止\"}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_ligfx_line() {
        let sys = "05-13 16:55:14.656 21047 I ligfxprofilerd: [GPU0] Frame 4579: Frequency: 1000 Hz, Tasks: 3 total, GSL Timestamp: 27217418, Global: Busy=33.75%, Queued=57.43%, Utilization=33.75%";
        match parse_line(sys) {
            Some(GpuEvent::Sys { mhz, maxmhz, util, busy }) => {
                assert_eq!(mhz, 1000);
                assert!(maxmhz.is_none());
                assert!((busy - 33.75).abs() < 0.1);
                assert!((util.unwrap() - 33.75).abs() < 0.1);
            }
            other => panic!("应为 Sys: {:?}", other.map(|_| ())),
        }
        let proc = "05-13 16:55:14.656 21047 I ligfxprofilerd: [GPU0]   GVM_com.lixiang.eid-8925: Busy=15.38%, Queued=46.98%, Utilization=15.38%";
        match parse_line(proc) {
            Some(GpuEvent::Proc { name, busy }) => {
                assert_eq!(name, "com.lixiang.eid");
                assert!((busy - 15.38).abs() < 0.1);
            }
            other => panic!("应为 Proc: {:?}", other.map(|_| ())),
        }
        assert!(parse_line("random logcat line").is_none());
    }
}
