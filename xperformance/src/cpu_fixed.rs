use crate::utils;
use anyhow::Result;
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
}

pub async fn sample_cpu(package: &str) -> Result<()> {
    let pid = utils::get_pid(package)?;

    // Get system CPU stats from /proc/stat
    let sys_output = utils::run_adb_command(&["shell", "cat", "/proc/stat"])?;
    let mut sys_stats = CpuStats::default();
    let mut sys_details = String::new();

    // Parse CPU stats from /proc/stat
    // Format: cpu  user nice system idle iowait irq softirq steal guest guest_nice
    if let Some(cpu_line) = sys_output.lines().find(|line| line.starts_with("cpu ")) {
        let values: Vec<f32> = cpu_line
            .split_whitespace()
            .skip(1) // Skip "cpu" prefix
            .filter_map(|s| s.parse::<f32>().ok())
            .collect();

        if values.len() >= 8 {
            sys_stats.user = values[0];
            sys_stats.nice = values[1];
            sys_stats.sys = values[2];
            sys_stats.idle = values[3];
            sys_stats.iow = values[4];
            sys_stats.irq = values[5];
            sys_stats.sirq = values[6];

            // Calculate total CPU time
            sys_stats.total_cpu = values.iter().take(8).sum();

            // Format stats for display
            sys_details = format!(
                "CPU: total={:.0} user={:.0} nice={:.0} sys={:.0} idle={:.0} iow={:.0} irq={:.0} sirq={:.0}",
                sys_stats.total_cpu, values[0], values[1], values[2], values[3], values[4], values[5], values[6]
            );
        }
    }

    // Calculate percentages
    let calculate_percentage = |value: f32| -> f32 {
        if sys_stats.total_cpu > 0.0 {
            (value / sys_stats.total_cpu) * 100.0
        } else {
            0.0
        }
    };

    let user_pct = calculate_percentage(sys_stats.user);
    let nice_pct = calculate_percentage(sys_stats.nice);
    let sys_pct = calculate_percentage(sys_stats.sys);
    let idle_pct = calculate_percentage(sys_stats.idle);
    let iow_pct = calculate_percentage(sys_stats.iow);
    let irq_pct = calculate_percentage(sys_stats.irq);
    let sirq_pct = calculate_percentage(sys_stats.sirq);

    // Calculate active CPU usage
    let active_pct = 100.0 - idle_pct - iow_pct;

    // Get detailed thread information
    let output = utils::run_adb_command(&["shell", "top", "-H", "-b", "-n", "1", "-p", &pid])?;

    let mut total_cpu = 0.0;
    let mut thread_count = 0;
    let mut details = String::new();

    // Add section header
    details.push_str("CPU Usage Details\n");
    details.push_str(&"=".repeat(80));
    details.push_str("\n\n");

    // Add process information
    details.push_str(&format!("Process ID: {}\n", pid));
    details.push_str(&format!("Package Name: {}\n", package));
    details.push_str("\n");

    // Add system CPU stats with detailed breakdown
    details.push_str("System CPU Stats:\n");
    details.push_str(&"-".repeat(80));
    details.push_str("\n");
    details.push_str(&format!("Active CPU Usage: {:.1}%\n", active_pct));
    details.push_str(&format!("User: {:.1}%\n", user_pct));
    details.push_str(&format!("Nice: {:.1}%\n", nice_pct));
    details.push_str(&format!("System: {:.1}%\n", sys_pct));
    details.push_str(&format!("Idle: {:.1}%\n", idle_pct));
    details.push_str(&format!("I/O Wait: {:.1}%\n", iow_pct));
    details.push_str(&format!("IRQ: {:.1}%\n", irq_pct));
    details.push_str(&format!("Soft IRQ: {:.1}%\n", sirq_pct));
    details.push_str("\nRaw Stats: ");
    details.push_str(&sys_details);
    details.push_str("\n\n");

    details.push_str("Thread Details:\n");
    details.push_str(&"-".repeat(80));
    details.push_str("\n");
    details.push_str(&format!("{:<8} {:>7} {:<}\n", "TID", "CPU%", "Name"));
    details.push_str(&"-".repeat(80));
    details.push_str("\n");

    // Parse CPU usage from top output
    for line in output.lines() {
        if line.contains(&pid) || line.contains(package) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() >= 9 {
                if let (Some(tid), Some(cpu_str)) = (fields.get(0), fields.get(8)) {
                    if let Ok(cpu_usage) = cpu_str.trim_end_matches('%').parse::<f32>() {
                        total_cpu += cpu_usage;
                        thread_count += 1;

                        // Get thread name (usually the last field)
                        let thread_name = fields.get(11).unwrap_or(&"<unknown>");
                        details
                            .push_str(&format!("{:<8} {:>6.1}% {}\n", tid, cpu_usage, thread_name));
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
    details.push_str(&format!("System Active CPU: {:.1}%\n", active_pct));
    details.push_str(&format!("System Idle CPU: {:.1}%\n", idle_pct));
    details.push_str(&format!("Thread Count: {}\n", thread_count));
    details.push_str(&"=".repeat(80));
    details.push_str("\n");

    // Write to log file
    utils::append_to_log(&details)?;

    // Print summary to console with both process and system CPU usage
    if thread_count > 0 {
        println!(
            "{} Process CPU: {}%, System Active: {}% (idle: {}%, pid: {}, threads: {})",
            chrono::Local::now().format("%H:%M:%S"),
            format!("{:.1}", total_cpu).blue(),
            format!("{:.1}", active_pct).red(),
            format!("{:.1}", idle_pct).green(),
            pid.yellow(),
            thread_count
        );
    }

    Ok(())
}
