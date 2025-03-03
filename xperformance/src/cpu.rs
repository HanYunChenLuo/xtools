use crate::utils;
use anyhow::Result;
use chrono::{DateTime, Local};
use colored::*;
use std::cmp::Ordering;

// 定义线程CPU使用信息结构体
#[derive(Debug, Clone)]
pub struct ThreadCpuInfo {
    pub tid: String,
    pub cpu_usage: f32,
    pub name: String,
    pub timestamp: Option<DateTime<Local>>,
}

// 实现比较特性以便在最大堆中使用
impl PartialEq for ThreadCpuInfo {
    fn eq(&self, other: &Self) -> bool {
        self.cpu_usage == other.cpu_usage
    }
}

impl Eq for ThreadCpuInfo {}

impl PartialOrd for ThreadCpuInfo {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ThreadCpuInfo {
    fn cmp(&self, other: &Self) -> Ordering {
        // 按CPU使用率降序排列
        self.cpu_usage
            .partial_cmp(&other.cpu_usage)
            .unwrap_or(Ordering::Equal)
            .reverse()
    }
}

// Helper function to clean thread names
fn clean_thread_name(name: &str) -> String {
    // Remove common prefixes like "1 |__", "2 |__", etc.
    let cleaned = if let Some(pos) = name.find("|__") {
        // Skip the pipe and underscores
        let after_prefix = &name[(pos + 3)..];
        after_prefix.trim().to_string()
    } else {
        name.trim().to_string()
    };

    // Further clean up - remove any non-thread-related info
    cleaned
}

// Add a new function to collect CPU statistics using pidstat
async fn collect_pidstat_data(pid: &str) -> Result<(f32, Vec<ThreadCpuInfo>)> {
    // Run pidstat to get thread-specific CPU usage
    // -p <pid>: monitor this PID
    // -t: include individual threads
    // -u: report CPU utilization
    // 1 1: report once with 1 second interval
    let pidstat_cmd_result =
        utils::run_adb_command(&["shell", "pidstat", "-p", pid, "-t", "-u", "1", "1"]);

    let mut threads = Vec::new();
    let mut process_cpu = 0.0;
    let mut found_process = false;

    if let Ok(output) = pidstat_cmd_result {
        // 检查输出是否表明pidstat命令不存在
        if output.contains("not found") || output.contains("No such file or directory") {
            return Err(anyhow::format_err!("pidstat命令在设备上不可用"));
        }

        // 检查输出是否为空或非预期格式
        if output.trim().is_empty() {
            return Err(anyhow::format_err!("pidstat返回空输出"));
        }

        if !output.contains("CPU") && !output.contains("%") && !output.contains("PID") {
            return Err(anyhow::format_err!("pidstat输出格式不正确: {}", output));
        }

        // Parse pidstat output
        for line in output.lines() {
            // Skip header lines and empty lines
            if line.trim().is_empty()
                || line.contains("Average")
                || line.contains("Linux")
                || line.contains("UID")
            {
                continue;
            }

            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 8 {
                continue;
            }

            // Properly identify TGID (main process) vs TID (thread) lines
            // The TGID line format: "UID      TGID       TID    %usr %system  %guest   %wait    %CPU   CPU  Command"
            // For main process: TGID = PID, TID = "-"
            // For threads: TGID = "-", TID = actual thread ID

            // Check if this is a process or thread line by examining TGID and TID columns
            let tgid_idx = 2; // TGID column index
            let tid_idx = 3; // TID column index
            let cpu_idx = 8; // %CPU column index (should be column 8 in standard pidstat output)

            if fields.len() > cpu_idx {
                // Get TGID and TID values
                let tgid = fields.get(tgid_idx).unwrap_or(&"");
                let tid = fields.get(tid_idx).unwrap_or(&"");

                if let Some(cpu_str) = fields.get(cpu_idx) {
                    if let Ok(cpu_usage) = cpu_str.parse::<f32>() {
                        // Main process line has TGID = pid and TID = "-"
                        if tgid == &pid && tid == &"-" {
                            // This is the main process (TGID line)
                            process_cpu = cpu_usage;
                            found_process = true;
                        }
                        // Thread line has TGID = "-" and TID = actual thread ID
                        else if tgid == &"-" && tid != &"-" {
                            // This is a thread
                            let thread_name = if fields.len() > cpu_idx + 1 {
                                clean_thread_name(&fields[cpu_idx + 1..].join(" "))
                            } else {
                                format!("Thread-{}", tid)
                            };

                            threads.push(ThreadCpuInfo {
                                tid: tid.to_string(),
                                cpu_usage,
                                name: thread_name,
                                timestamp: None,
                            });
                        }
                    }
                }
            }
        }
    } else {
        // 如果命令执行失败，返回详细错误
        let error_msg = match pidstat_cmd_result {
            Err(e) => e.to_string(),
            _ => "未知错误".to_string(),
        };
        return Err(anyhow::format_err!("无法执行pidstat命令: {}", error_msg));
    }

    // 如果没有找到进程或任何线程，返回错误
    if !found_process && threads.is_empty() {
        return Err(anyhow::format_err!(
            "未能在进程 {} 中找到任何CPU使用数据",
            pid
        ));
    }

    // Sort threads by CPU usage (highest first)
    threads.sort_by(|a, b| {
        b.cpu_usage
            .partial_cmp(&a.cpu_usage)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // If we didn't find process CPU but have threads, sum them up
    if (!found_process || process_cpu == 0.0) && !threads.is_empty() {
        // Calculate process CPU as the sum of all thread CPU usage
        let thread_cpu_sum = threads.iter().map(|t| t.cpu_usage).sum();

        // Use the thread sum if it's greater than the reported process CPU
        // This ensures we don't show 0% process CPU when threads are active
        if thread_cpu_sum > process_cpu {
            process_cpu = thread_cpu_sum;
        }
    }

    Ok((process_cpu, threads))
}

pub async fn sample_cpu(package: &str) -> Result<(f32, DateTime<Local>, Vec<ThreadCpuInfo>)> {
    let timestamp = Local::now();
    let process_info = utils::get_process_info(package)?;
    let pid = &process_info.pid;

    // 尝试使用pidstat命令获取进程CPU使用率
    let pidstat_result = collect_pidstat_data(pid).await;

    match pidstat_result {
        Ok((pidstat_process_cpu, pidstat_threads)) => {
            // 添加时间戳到每个线程
            let mut threads = pidstat_threads;
            for thread in &mut threads {
                thread.timestamp = Some(timestamp);
            }

            // 打印进程CPU使用情况
            println!(
                "[{}] Process CPU: {}% (pid: {})",
                timestamp.format("%H:%M:%S"),
                format!("{:.1}", pidstat_process_cpu).blue(),
                pid.yellow()
            );

            Ok((pidstat_process_cpu, timestamp, threads))
        }
        Err(e) => {
            // 检查是否为中断信号
            let error_string = e.to_string();
            let is_interrupt = error_string.contains("interrupt")
                || error_string.contains("signal")
                || error_string.contains("terminated")
                || error_string.contains("ADB command failed") && utils::is_being_interrupted();

            // 只在非中断情况下打印详细错误
            if !is_interrupt {
                eprintln!("pidstat数据收集失败: {}", e);
                eprintln!("可能原因:");
                eprintln!("1. 设备上未安装pidstat工具");
                eprintln!("2. 设备权限不足");
                eprintln!("3. ADB连接不稳定");
                eprintln!("4. 目标进程已终止");
            }

            // 返回错误
            Err(anyhow::format_err!("无法获取CPU数据: {}", e))
        }
    }
}
