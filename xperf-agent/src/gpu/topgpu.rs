//! SS2MAX topgpu 工具通道。
//! topgpu 是 SS2/8155 平台的 GPU 负载工具（push 到 /data/），读 sysfs gpu_busy_percentage
//! 或 adreno_cmdbatch ftrace 事件，输出系统 GPU 使用率 + 各进程使用率。
//! 格式（每采样周期一行）：
//!   sys gpu: 20.0%
//!   pid 1234 'com.app' gpu: 16.0% (80.0% of sys)

use super::{spawn_stream_parser, GpuEvent};
use crate::emit;
use std::collections::HashMap;
use std::fs;
use std::sync::{Arc, Mutex};

/// topgpu 工具可用性：/data/local/tmp/topgpu 或 /data/topgpu 存在且可执行
pub(super) fn available() -> bool {
    for p in ["/data/local/tmp/topgpu", "/data/topgpu"] {
        if fs::metadata(p).is_ok() {
            return true;
        }
    }
    false
}

/// 启动 topgpu 子进程（持续输出流），返回 (child, reader)
/// period_s: 采样周期（秒），topgpu 接受整数秒
fn spawn(period_s: u64) -> Option<(std::process::Child, std::io::BufReader<std::process::ChildStdout>)> {
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

/// 解析输出行（归一为 GpuEvent；sys 行无频率信息，mhz 补 0）
/// "sys gpu: 20.0%" → Sys
/// "pid 1234 'com.app' gpu: 16.0% (80.0% of sys)" → Proc
fn parse_line(line: &str) -> Option<GpuEvent> {
    if let Some(rest) = line.strip_prefix("sys gpu:") {
        let v: f32 = rest.trim().trim_end_matches('%').trim().parse().ok()?;
        return Some(GpuEvent::Sys { mhz: 0, maxmhz: None, util: None, busy: v });
    }
    if line.starts_with("pid ") && line.contains("gpu:") {
        // pid 1234 'com.app' gpu: 16.0% (80.0% of sys)
        let name_start = line.find('\'')? + 1;
        let name_end = line[name_start..].find('\'')? + name_start;
        let name = line[name_start..name_end].to_string();
        let gpu_pos = line.find("gpu:")? + 4;
        let busy: f32 = line[gpu_pos..].split('%').next()?.trim().parse().ok()?;
        return Some(GpuEvent::Proc { name, busy });
    }
    None
}

/// 启动通道（start_stream_channels 分发）
pub(super) fn start(interval_ms: u64, pid_names: &Arc<Mutex<HashMap<String, u32>>>) {
    let period_s = (interval_ms / 1000).max(1);
    match spawn(period_s) {
        Some((child, reader)) => spawn_stream_parser(child, reader, None, None, pid_names, parse_line),
        None => emit("{\"t\":\"err\",\"msg\":\"topgpu 启动失败，--gpu 停止\"}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_topgpu_line() {
        match parse_line("sys gpu: 20.0%") {
            Some(GpuEvent::Sys { mhz, maxmhz, util, busy }) => {
                assert_eq!(mhz, 0);
                assert!(maxmhz.is_none() && util.is_none());
                assert!((busy - 20.0).abs() < 0.1);
            }
            other => panic!("应为 Sys: {:?}", other.map(|_| ())),
        }
        match parse_line("pid 1234 'com.app' gpu: 16.0% (80.0% of sys)") {
            Some(GpuEvent::Proc { name, busy }) => {
                assert_eq!(name, "com.app");
                assert!((busy - 16.0).abs() < 0.1);
            }
            other => panic!("应为 Proc: {:?}", other.map(|_| ())),
        }
        assert!(parse_line("garbage").is_none());
    }
}
