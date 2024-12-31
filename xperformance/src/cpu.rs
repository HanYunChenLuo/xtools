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

pub async fn sample_cpu(package: &str, enable_logging: bool) -> Result<(f32, DateTime<Local>)> {
    let timestamp = Local::now();
    let process_info = utils::get_process_info(package)?;
    let pid = &process_info.pid;

    // Get system stats using top
    let sys_output = utils::run_adb_command(&["shell", "top", "-n", "1", "-b"])?;

    let mut sys_stats = CpuStats::default();
    let mut sys_details = String::new();
    let mut tasks_info = String::new();

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
        }
    }

    // Get detailed thread information
    let output = utils::run_adb_command(&["shell", "top", "-H", "-b", "-n", "1", "-p", pid])?;

    let mut total_cpu = 0.0;
    let mut thread_count = 0;

    // Parse CPU usage from top output
    for line in output.lines() {
        if line.contains(pid) || line.contains(package) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() >= 9 {
                if let (Some(_tid), Some(cpu_str)) = (fields.get(0), fields.get(8)) {
                    if let Ok(cpu_usage) = cpu_str.trim_matches(&['[', ']', '%'][..]).parse::<f32>()
                    {
                        total_cpu += cpu_usage;
                        thread_count += 1;
                    }
                }
            }
        }
    }

    if enable_logging {
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
                if fields.len() >= 9 {
                    if let (Some(tid), Some(cpu_str)) = (fields.get(0), fields.get(8)) {
                        if let Ok(cpu_usage) =
                            cpu_str.trim_matches(&['[', ']', '%'][..]).parse::<f32>()
                        {
                            let thread_name = fields.get(11).unwrap_or(&"<unknown>");
                            details.push_str(&format!(
                                "{:<8} {:>6.1}% {}\n",
                                tid, cpu_usage, thread_name
                            ));
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
        details.push_str(&format!(
            "System CPU Usage: {:.1}%\n",
            sys_stats.user + sys_stats.sys
        ));
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
            format!("{:.1}", sys_stats.user + sys_stats.sys).red(),
            format!("{:.1}", sys_stats.idle).green(),
            pid.yellow(),
            thread_count
        );
    }

    Ok((total_cpu, timestamp))
}
