//! 设备端采样器（xperf-agent）的主机侧传输层。
//!
//! 背景：低间隔（<500ms）采样时 adb 轮询的开销就超过间隔本身（单轮 CPU 采样
//! 固定 6 次 adb 调用，每次 ~13ms 起）。agent 模式把采样循环搬到设备上
//! （直接读 /proc），主机通过 `adb exec-out` 长连接读取 NDJSON 事件流。
//! 协议见 xperf-agent/main.rs 头注释。
//!
//! 当前覆盖：CPU（含线程）、内存（smaps_rollup 的 Pss/Rss）。
//! FPS 不在此列——SurfaceFlinger 的 127 帧缓冲在 1s 轮询下已是帧级分辨率。

use anyhow::Result;
use serde::Deserialize;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

const DEVICE_AGENT_PATH: &str = "/data/local/tmp/xperf-agent";

/// 采样间隔低于该值时 adb 轮询模式不可用，建议走 agent 模式
pub const AGENT_INTERVAL_THRESHOLD_MS: u64 = 500;

#[derive(Debug, Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum AgentEvent {
    Hello { ncores: u32 },
    /// ts: 墙钟毫秒；cpu: 单核口径 %；th: [tid, 线程名, cpu%]（仅 >0.05% 的线程）
    Cpu { ts: u64, pid: u32, cpu: f32, th: Vec<(u32, String, f32)> },
    /// pss/rss 单位 KB
    Mem { ts: u64, pid: u32, pss: u64, rss: u64 },
    Exit { pid: u32 },
    Noproc,
    Err { msg: String },
}

pub struct AgentStream {
    child: Child,
    reader: BufReader<std::process::ChildStdout>,
}

impl AgentStream {
    /// 阻塞读取下一行事件；流结束（agent 退出/断开）返回 Ok(None)。
    /// 解析失败的行跳过（返回 Some(Err) 由调用方决定）。
    pub fn next_event(&mut self) -> Result<Option<std::result::Result<AgentEvent, String>>> {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line)?;
        if n == 0 {
            return Ok(None); // EOF
        }
        let line = line.trim();
        if line.is_empty() {
            return self.next_event();
        }
        Ok(Some(
            serde_json::from_str(line).map_err(|e| format!("{} (行: {})", e, line)),
        ))
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for AgentStream {
    fn drop(&mut self) {
        self.kill();
    }
}

/// 本机 agent 二进制路径（交叉编译产物）
pub fn agent_binary_path() -> PathBuf {
    // xperf-core/Cargo.toml 所在目录的上级 = workspace 根
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("target/aarch64-linux-android/release/xperf-agent")
}

/// 若本机尚未交叉编译 agent 二进制则自动构建（链接器见 .cargo/config.toml）
pub fn ensure_agent_built() -> Result<PathBuf> {
    let bin = agent_binary_path();
    if !bin.exists() {
        eprintln!("agent 二进制不存在，正在交叉编译（aarch64-linux-android）...");
        let status = Command::new("cargo")
            .args(["build", "-p", "xperf-agent", "--target", "aarch64-linux-android", "--release"])
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap())
            .status()?;
        if !status.success() {
            anyhow::bail!("交叉编译 xperf-agent 失败（需要 Android NDK，见 .cargo/config.toml）");
        }
    }
    Ok(bin)
}

/// 推送 agent 到设备（设备上不存在或大小不一致时）
pub fn deploy_agent(local: &Path) -> Result<()> {
    let local_size = std::fs::metadata(local)?.len();
    let remote_size = crate::run_adb_command(&["shell", "stat", "-c", "%s", DEVICE_AGENT_PATH])
        .ok()
        .and_then(|o| o.stdout.trim().parse::<u64>().ok());
    if remote_size == Some(local_size) {
        return Ok(()); // 已是最新（大小一致）
    }
    crate::run_adb_command(&[
        "push",
        &local.to_string_lossy(),
        DEVICE_AGENT_PATH,
    ])?;
    crate::run_adb_command(&["shell", "chmod", "755", DEVICE_AGENT_PATH])?;
    Ok(())
}

/// 启动设备端采样器，返回事件流。Ctrl-C/断连时 agent 因 stdout 写失败自行退出。
pub fn spawn_agent(
    package: Option<&str>,
    interval_ms: u64,
    cpu: bool,
    memory: bool,
) -> Result<AgentStream> {
    let mut cmd_args = vec!["exec-out".to_string(), DEVICE_AGENT_PATH.to_string()];
    if let Some(pkg) = package {
        cmd_args.extend(["--package".to_string(), pkg.to_string()]);
    }
    cmd_args.extend(["--interval".to_string(), interval_ms.to_string()]);
    if cpu {
        cmd_args.push("--cpu".to_string());
    }
    if memory {
        cmd_args.push("--memory".to_string());
    }
    let mut child = Command::new("adb")
        .args(&cmd_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdout = child.stdout.take().expect("stdout piped");
    Ok(AgentStream {
        child,
        reader: BufReader::new(stdout),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hello() {
        let ev: AgentEvent = serde_json::from_str(r#"{"t":"hello","ncores":8,"version":1}"#).unwrap();
        assert!(matches!(ev, AgentEvent::Hello { ncores: 8 }));
    }

    #[test]
    fn test_parse_cpu_with_threads() {
        // 真机协议：th 为 [tid, 线程名, cpu%] 三元组
        let ev: AgentEvent = serde_json::from_str(
            r#"{"t":"cpu","ts":1788258836663,"pid":29697,"cpu":27.59,"th":[[9871,"AdrenoOsLib",20.0],[29797,"XFW:Main",20.0]]}"#,
        )
        .unwrap();
        match ev {
            AgentEvent::Cpu { pid, cpu, th, .. } => {
                assert_eq!(pid, 29697);
                assert!((cpu - 27.59).abs() < 0.01);
                assert_eq!(th.len(), 2);
                assert_eq!(th[0].1, "AdrenoOsLib");
            }
            _ => panic!("应为 Cpu 事件"),
        }
    }

    #[test]
    fn test_parse_mem_exit_noproc_err() {
        let ev: AgentEvent = serde_json::from_str(r#"{"t":"mem","ts":1,"pid":2,"pss":483713,"rss":638728}"#).unwrap();
        assert!(matches!(ev, AgentEvent::Mem { pss: 483713, rss: 638728, .. }));
        let ev: AgentEvent = serde_json::from_str(r#"{"t":"exit","pid":29697}"#).unwrap();
        assert!(matches!(ev, AgentEvent::Exit { pid: 29697 }));
        let ev: AgentEvent = serde_json::from_str(r#"{"t":"noproc"}"#).unwrap();
        assert!(matches!(ev, AgentEvent::Noproc));
        let ev: AgentEvent = serde_json::from_str(r#"{"t":"err","msg":"round overrun"}"#).unwrap();
        assert!(matches!(ev, AgentEvent::Err { .. }));
    }
}
