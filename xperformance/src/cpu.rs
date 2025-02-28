use crate::utils;
use anyhow::Result;
use chrono::{DateTime, Local};
use colored::*;

#[derive(Default)]
struct CpuStats {
    total_cpu: f32,
    user: f32,
    nice: f32,
    sys: f32,
    idle: f32,
    iow: f32,
    irq: f32,
    sirq: f32,
    host: f32,
}

pub async fn sample_cpu(package: &str, verbose: bool) -> Result<(f32, f32, f32, DateTime<Local>)> {
    let timestamp = Local::now();
    let process_info = utils::get_process_info(package)?;
    let pid = &process_info.pid;

    // Get system stats using top
    let sys_output = utils::run_adb_command(&["shell", "top", "-n", "1", "-b"])?;

    let mut sys_stats = CpuStats::default();
    let mut sys_details = String::new();
    let mut tasks_info = String::new();
    let mut process_top_line = String::new(); // 存储进程的top行
    let mut process_cpu_from_top = 0.0; // 从top直接获取的进程CPU使用率

    // Parse top output
    for line in sys_output.lines() {
        if line.contains("%cpu") {
            sys_details = line.to_string();
            // Format: "800%cpu 123%user 0%nice 171%sys 481%idle 3%iow 13%irq 10%sirq 0%host"
            let parts: Vec<&str> = line.split_whitespace().collect();

            for part in parts {
                let value_str = part.trim();
                if let Some(percent_idx) = value_str.find('%') {
                    if let Ok(value) = value_str[..percent_idx].parse::<f32>() {
                        if value_str[percent_idx..].starts_with("%cpu") {
                            sys_stats.total_cpu = value;
                        } else if value_str[percent_idx..].starts_with("%user") {
                            sys_stats.user = value;
                        } else if value_str[percent_idx..].starts_with("%nice") {
                            sys_stats.nice = value;
                        } else if value_str[percent_idx..].starts_with("%sys") {
                            sys_stats.sys = value;
                        } else if value_str[percent_idx..].starts_with("%idle") {
                            sys_stats.idle = value;
                        } else if value_str[percent_idx..].starts_with("%iow") {
                            sys_stats.iow = value;
                        } else if value_str[percent_idx..].starts_with("%irq") {
                            sys_stats.irq = value;
                        } else if value_str[percent_idx..].starts_with("%sirq") {
                            sys_stats.sirq = value;
                        } else if value_str[percent_idx..].starts_with("%host") {
                            sys_stats.host = value;
                        }
                    }
                }
            }
        } else if line.starts_with("Tasks:") {
            tasks_info = line.to_string();
        } else if line.trim().starts_with(pid)
            || (line.contains(package) && !line.contains("top -p"))
        {
            // 更精确地匹配进程行：以PID开头或包含包名但不是top命令行
            let words: Vec<&str> = line.split_whitespace().collect();
            if !words.is_empty() && words[0] == pid {
                process_top_line = line.to_string();
                // 从进程行提取CPU使用率，通常是第9列（索引8）
                if words.len() > 8 {
                    if let Ok(cpu) = words[8].trim_end_matches('%').parse::<f32>() {
                        process_cpu_from_top = cpu;
                    }
                }
            } else if line.contains(package) && !line.contains("top -p") {
                process_top_line = line.to_string();
                // 同样尝试提取CPU使用率
                for (i, word) in words.iter().enumerate() {
                    if i > 0 && i < words.len() - 1 {
                        if let Ok(cpu) = word.trim_end_matches('%').parse::<f32>() {
                            // 验证这是一个合理的CPU值 (0-100%)
                            if cpu >= 0.0 && cpu <= 100.0 {
                                process_cpu_from_top = cpu;
                                break;
                            }
                        }
                    }
                }
            }
        }
    }

    // Get detailed thread information
    let output = utils::run_adb_command(&["shell", "top", "-H", "-b", "-n", "1", "-p", pid])?;

    let mut total_thread_cpu = 0.0; // 从线程计算的CPU总和
    let mut thread_count = 0;
    let mut cpu_column_index = 8; // Default index, will be updated if header is found

    // Find the CPU column index from the header line
    for line in output.lines() {
        if line.contains("PID") && line.contains("[%CPU]") {
            let headers: Vec<&str> = line.split_whitespace().collect();
            for (i, header) in headers.iter().enumerate() {
                if *header == "[%CPU]" {
                    cpu_column_index = i;
                    break;
                }
            }
            break;
        }
    }

    // Parse CPU usage from top output
    for line in output.lines() {
        if line.contains(pid) || line.contains(package) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() > cpu_column_index {
                if let (Some(_tid), Some(cpu_str)) = (fields.get(0), fields.get(cpu_column_index)) {
                    if let Ok(cpu_usage) = cpu_str.trim_matches(&['[', ']', '%'][..]).parse::<f32>()
                    {
                        total_thread_cpu += cpu_usage;
                        thread_count += 1;
                    }
                }
            }
        }
    }

    // Calculate active CPU usage (excluding idle and iowait)
    let _active_cpu = sys_stats.total_cpu - sys_stats.idle - sys_stats.iow;

    // 使用从top获取的进程CPU使用率，如果没有获取到，则使用线程CPU总和
    let total_cpu = if process_cpu_from_top > 0.0 {
        process_cpu_from_top
    } else {
        total_thread_cpu
    };

    if verbose {
        let mut details = String::new();

        // Add section header
        details.push_str("CPU Usage Details\n");
        details.push_str(&"=".repeat(80));
        details.push_str("\n\n");

        // Add process information
        details.push_str(&format!("Process ID: {}\n", pid));
        details.push_str(&format!("Package Name: {}\n", package));
        details.push_str(&format!("Start Time: {}\n", process_info.start_time));
        details.push_str("\n");

        // Add system stats
        details.push_str("System Stats:\n");
        details.push_str(&"-".repeat(80));
        details.push_str("\n");
        if !tasks_info.is_empty() {
            details.push_str(&tasks_info);
            details.push_str("\n");
        }
        details.push_str(&sys_details);
        details.push_str("\n\n");

        // Add CPU breakdown
        details.push_str("CPU Breakdown:\n");
        details.push_str(&"-".repeat(80));
        details.push_str("\n");
        details.push_str(&format!(
            "Total CPU Cores: {:.0}% ({:.1} cores)\n",
            sys_stats.total_cpu,
            sys_stats.total_cpu / 100.0
        ));
        details.push_str(&format!("User CPU: {:.1}%\n", sys_stats.user));
        details.push_str(&format!("Nice CPU: {:.1}%\n", sys_stats.nice));
        details.push_str(&format!("System CPU: {:.1}%\n", sys_stats.sys));
        details.push_str(&format!("Idle CPU: {:.1}%\n", sys_stats.idle));
        details.push_str(&format!("I/O Wait: {:.1}%\n", sys_stats.iow));
        details.push_str(&format!("IRQ: {:.1}%\n", sys_stats.irq));
        details.push_str(&format!("Soft IRQ: {:.1}%\n", sys_stats.sirq));
        details.push_str(&format!("Host: {:.1}%\n", sys_stats.host));
        details.push_str("\n");

        // 添加进程CPU使用率的信息
        details.push_str(&format!(
            "Process CPU (from top): {:.1}%\n",
            process_cpu_from_top
        ));
        details.push_str(&format!(
            "Process CPU (from threads): {:.1}%\n",
            total_thread_cpu
        ));
        details.push_str("\n");

        details.push_str("Thread Details:\n");
        details.push_str(&"-".repeat(80));
        details.push_str("\n");
        details.push_str(&format!("{:<8} {:>7} {:<}\n", "TID", "CPU%", "Name"));
        details.push_str(&"-".repeat(80));
        details.push_str("\n");

        // Add thread details
        for line in output.lines() {
            if line.contains(pid) || line.contains(package) {
                let fields: Vec<&str> = line.split_whitespace().collect();
                if fields.len() > cpu_column_index {
                    if let (Some(tid), Some(cpu_str)) =
                        (fields.get(0), fields.get(cpu_column_index))
                    {
                        if let Ok(cpu_usage) =
                            cpu_str.trim_matches(&['[', ']', '%'][..]).parse::<f32>()
                        {
                            // Only log threads with CPU usage > 0
                            if cpu_usage > 0.0 {
                                // Get thread name - it's typically after the TIME+ column
                                let thread_name = if fields.len() > cpu_column_index + 3 {
                                    fields[cpu_column_index + 3..].join(" ")
                                } else {
                                    "<unknown>".to_string()
                                };

                                details.push_str(&format!(
                                    "{:<8} {:>6.1}% {}\n",
                                    tid, cpu_usage, thread_name
                                ));
                            }
                        }
                    }
                }
            }
        }

        // Add summary
        details.push_str("\nSummary:\n");
        details.push_str(&"-".repeat(80));
        details.push_str("\n");
        details.push_str(&format!("Process ID: {}\n", pid));
        details.push_str(&format!("Total Process CPU: {:.1}%\n", total_cpu));
        details.push_str(&format!("System CPU: {:.1}%\n", sys_stats.sys));
        details.push_str(&format!("System Idle: {:.1}%\n", sys_stats.idle));
        details.push_str(&format!("Thread Count: {}\n", thread_count));
        details.push_str(&"=".repeat(80));
        details.push_str("\n");

        // Write to log file
        utils::append_to_log(&details)?;
    }

    // Print summary to console with both process and system CPU usage
    if thread_count > 0 {
        println!(
            "[{}] Process: {}%, System: {}% (idle: {}%, pid: {}, threads: {})",
            timestamp.format("%H:%M:%S"),
            format!("{:.1}", total_cpu).blue(),
            format!("{:.1}", sys_stats.sys).red(),
            format!("{:.1}", sys_stats.idle).green(),
            pid.yellow(),
            thread_count
        );
    }

    // Return process CPU usage, system CPU usage, idle CPU usage, and timestamp
    Ok((total_cpu, sys_stats.sys, sys_stats.idle, timestamp))
}
