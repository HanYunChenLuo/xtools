//! 冷启动时间测量：通过 `adb shell am start -W <pkg>/<activity>` 获取启动耗时。
//!
//! am start -W 输出示例：
//!   Starting: Intent { ... }
//!   Status: ok
//!   Activity: com.lixiang.car.x.svm/.MainActivity
//!   ThisTime: 1234
//!   TotalTime: 1567
//!   WaitTime: 1580
//!   Complete
//!
//! ThisTime = 目标 Activity 启动耗时；TotalTime = 整条启动链耗时；
//! WaitTime = am start 命令等待总时间（含 IPC）。

use anyhow::Result;
use std::process::Command;

#[derive(Debug)]
pub struct ColdStartResult {
    pub activity: String,
    pub this_time_ms: u64,
    pub total_time_ms: u64,
    pub wait_time_ms: u64,
    pub status: String,
}

/// 执行 am start -W 并解析结果。activity 格式：".MainActivity" 或完整类名
/// 超时：macOS 无 `timeout` 命令，用 spawn+wait_timeout 手动实现
pub fn measure(package: &str, activity: &str) -> Result<ColdStartResult> {
    let component = format!("{}/{}", package, activity);
    let mut child = Command::new("adb")
        .args(["shell", "am", "start", "-W", &component])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    // 手动超时：15s 无输出则 kill
    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(15);
    let output = loop {
        match child.try_wait()? {
            Some(_) => {
                let mut out = String::new();
                use std::io::Read;
                child.stdout.take().unwrap().read_to_string(&mut out).ok();
                break out;
            }
            None => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    anyhow::bail!("am start -W 超时（15s）");
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    };
    parse_am_start_w(&output, &component)
}

fn parse_am_start_w(output: &str, component: &str) -> Result<ColdStartResult> {
    let mut activity = String::new();
    let mut this_time = 0u64;
    let mut total_time = 0u64;
    let mut wait_time = 0u64;
    let mut status = String::new();
    for line in output.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("Status:") { status = v.trim().into(); }
        else if let Some(v) = line.strip_prefix("Activity:") { activity = v.trim().into(); }
        else if let Some(v) = line.strip_prefix("ThisTime:") { this_time = v.trim().parse().map_err(|_| anyhow::anyhow!("ThisTime 解析失败: {}", v))?; }
        else if let Some(v) = line.strip_prefix("TotalTime:") { total_time = v.trim().parse().map_err(|_| anyhow::anyhow!("TotalTime 解析失败: {}", v))?; }
        else if let Some(v) = line.strip_prefix("WaitTime:") { wait_time = v.trim().parse().map_err(|_| anyhow::anyhow!("WaitTime 解析失败: {}", v))?; }
    }
    if status.is_empty() {
        anyhow::bail!("am start -W 无输出（组件 {} 可能不存在或无法启动）\n原始输出:\n{}", component, output);
    }
    if status != "ok" {
        anyhow::bail!("am start -W 失败（Status: {}）\n原始输出:\n{}", status, output);
    }
    Ok(ColdStartResult { activity, this_time_ms: this_time, total_time_ms: total_time, wait_time_ms: wait_time, status })
}

impl ColdStartResult {
    pub fn summary(&self) -> String {
        format!(
            "冷启动: {} | ThisTime: {}ms | TotalTime: {}ms | WaitTime: {}ms | Status: {}",
            self.activity, self.this_time_ms, self.total_time_ms, self.wait_time_ms, self.status
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_am_start_w() {
        let out = "Starting: Intent { act=android.intent.action.MAIN cat=[android.intent.category.LAUNCHER] cmp=com.lixiang.car.x.svm/.MainActivity }\n\
                   Status: ok\n\
                   Activity: com.lixiang.car.x.svm/.MainActivity\n\
                   ThisTime: 1234\n\
                   TotalTime: 1567\n\
                   WaitTime: 1580\n\
                   Complete\n";
        let r = parse_am_start_w(out, "com.lixiang.car.x.svm/.MainActivity").unwrap();
        assert_eq!(r.status, "ok");
        assert_eq!(r.this_time_ms, 1234);
        assert_eq!(r.total_time_ms, 1567);
        assert_eq!(r.wait_time_ms, 1580);
        assert!(r.summary().contains("1234ms"));
    }

    #[test]
    fn test_parse_empty() {
        assert!(parse_am_start_w("no output", "x/.y").is_err());
    }

    #[test]
    fn test_parse_error_status() {
        let out = "Starting: Intent { cmp=x/.y }\nStatus: error\nActivity: x/.y\n";
        assert!(parse_am_start_w(out, "x/.y").is_err());
    }
}
