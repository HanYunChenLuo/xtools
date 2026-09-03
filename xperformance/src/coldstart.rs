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
pub fn measure(package: &str, activity: &str) -> Result<ColdStartResult> {
    let component = format!("{}/{}", package, activity);
    let output = Command::new("adb")
        .args(["shell", "am", "start", "-W", &component])
        .output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_am_start_w(&stdout, &component)
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
        else if let Some(v) = line.strip_prefix("ThisTime:") { this_time = v.trim().parse().unwrap_or(0); }
        else if let Some(v) = line.strip_prefix("TotalTime:") { total_time = v.trim().parse().unwrap_or(0); }
        else if let Some(v) = line.strip_prefix("WaitTime:") { wait_time = v.trim().parse().unwrap_or(0); }
    }
    if status.is_empty() {
        anyhow::bail!("am start -W 无输出（组件 {} 可能不存在或无法启动）\n原始输出:\n{}", component, output);
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
}
