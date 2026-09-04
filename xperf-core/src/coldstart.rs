//! 冷启动时间测量与应用操作：`am start -W` 解析、主 Activity 自动解析、force-stop。
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
//!
//! CLI（`--cold-start`）与 GUI（「打开应用」/「重启应用」按钮）共用本模块；
//! `serial` 参数路由到目标设备（多设备并行会话用，`None` 回退全局选择）。

use anyhow::{bail, Result};
use serde::Serialize;

/// 一次 `am start -W` 的测量结果（单位全为毫秒）。
#[derive(Debug, Clone, Serialize)]
pub struct ColdStartResult {
    /// 实际启动的 Activity（am start -W 输出的 `Activity:` 行，完整组件名）
    pub activity: String,
    /// 目标 Activity 启动耗时（`ThisTime`）
    pub this_time_ms: u64,
    /// 整条启动链耗时（`TotalTime`；基线对比的冷启动口径）
    pub total_time_ms: u64,
    /// am start 命令等待总时间（`WaitTime`，含 IPC）
    pub wait_time_ms: u64,
    /// 启动状态（`Status:` 行；非 `ok` 时 [`measure`] 已报错，恒为 `ok`）
    pub status: String,
}

impl ColdStartResult {
    /// 单行摘要（CLI 打印 / GUI 状态栏共用）
    pub fn summary(&self) -> String {
        format!(
            "冷启动: {} | ThisTime: {}ms | TotalTime: {}ms | WaitTime: {}ms | Status: {}",
            self.activity, self.this_time_ms, self.total_time_ms, self.wait_time_ms, self.status
        )
    }
}

/// 解析包的主启动 Activity（`cmd package resolve-activity --brief`）。
///
/// 输出两行：包名 + 完整组件（如 `com.example/.MainActivity`），
/// 返回组件的 activity 段（`.MainActivity`，相对写法可直接拼回包名）。
/// 应用无 launcher 入口（纯服务包）时报错。
pub fn resolve_activity(package: &str, serial: Option<&str>) -> Result<String> {
    let out = crate::utils::adb_for(serial)
        .args(["shell", "cmd", "package", "resolve-activity", "--brief", package])
        .output()?;
    if !out.status.success() {
        bail!(
            "resolve-activity 失败: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    // 取最后一个非空行（部分系统首行回显包名，末行是完整组件）
    let component = stdout
        .lines()
        .map(|l| l.trim())
        .rfind(|l| !l.is_empty())
        .unwrap_or("");
    let activity = component.split('/').next_back().unwrap_or("");
    if activity.is_empty() {
        bail!("包 {} 无可启动的 launcher Activity（可手填完整类名）", package);
    }
    Ok(activity.to_string())
}

/// 强制停止应用（`am force-stop`；「重启应用」/ 冷启动前置）。
/// force-stop 为异步杀进程，调用方须自行等待进程死透再启动。
pub fn force_stop(package: &str, serial: Option<&str>) -> Result<()> {
    let out = crate::utils::adb_for(serial)
        .args(["shell", "am", "force-stop", package])
        .output()?;
    if !out.status.success() {
        bail!(
            "am force-stop 失败: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// 执行 `am start -W` 并解析结果。activity 格式：".MainActivity" 或完整类名；
/// 空串时自动 [`resolve_activity`] 解析主入口。
/// 手动超时 15s（无 `timeout` 命令依赖，macOS/Linux 通用）。
pub fn measure(package: &str, activity: &str, serial: Option<&str>) -> Result<ColdStartResult> {
    let activity = if activity.is_empty() {
        resolve_activity(package, serial)?
    } else {
        activity.to_string()
    };
    let component = format!("{}/{}", package, activity);
    let mut child = crate::utils::adb_for(serial)
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
                    bail!("am start -W 超时（15s）");
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    };
    parse_am_start_w(&output, &component)
}

/// 解析 `am start -W` 输出（纯函数；`Status` 非 ok / 无输出报错带原始输出）
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
        bail!("am start -W 无输出（组件 {} 可能不存在或无法启动）\n原始输出:\n{}", component, output);
    }
    if status != "ok" {
        bail!("am start -W 失败（Status: {}）\n原始输出:\n{}", status, output);
    }
    Ok(ColdStartResult { activity, this_time_ms: this_time, total_time_ms: total_time, wait_time_ms: wait_time, status })
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
