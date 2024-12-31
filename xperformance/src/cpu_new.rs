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
    host: f32,
}

pub async fn sample_cpu(package: &str) -> Result<()> {
    let pid = utils::get_pid(package)?;

    // Get system CPU stats first
    let sys_output = utils::run_adb_command(&["shell", "top", "-n", "1", "-b"])?;
    let mut sys_stats = CpuStats::default();
    let mut sys_details = String::new();

    if let Some(first_line) = sys_output.lines().next() {
        if first_line.contains("%cpu") {
            sys_details = first_line.to_string();
            let parts: Vec<&str> = first_line.split_whitespace().collect();

            for part in parts {
                if let Some(value_str) = part.strip_suffix('%') {
                    if let Some(idx) = value_str.find(|c: char| !c.is_ascii_digit() && c != '.') {
                        if let Ok(value) = value_str[..idx].parse::<f32>() {
                            match &value_str[idx..] {
                                "cpu" => sys_stats.total_cpu = value,
                                "user" => sys_stats.user = value,
                                "nice" => sys_stats.nice = value,
                                "sys" => sys_stats.sys = value,
                                "idle" => sys_stats.idle = value,
                                "iow" => sys_stats.iow = value,
                                "irq" => sys_stats.irq = value,
                                "sirq" => sys_stats.sirq = value,
                                "host" => sys_stats.host = value,
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
    }

    // Normalize values to percentages
    let total_cores = sys_stats.total_cpu / 100.0;
    let normalize = |value: f32| -> f32 {
        if total_cores > 0.0 {
            (value / sys_stats.total_cpu) * 100.0
        } else {
            value
        }
    };

    let system_cpu_usage = normalize(
        sys_stats.user
            + sys_stats.nice
            + sys_stats.sys
            + sys_stats.iow
            + sys_stats.irq
            + sys_stats.sirq
            + sys_stats.host,
    );

    let normalized_idle = normalize(sys_stats.idle);

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
    details.push_str(&format!(
        "Total CPU Cores: {:.0}% ({:.1} cores)\n",
        sys_stats.total_cpu, total_cores
    ));
    details.push_str(&format!("Active CPU Usage: {:.1}%\n", system_cpu_usage));
    details.push_str(&format!("User: {:.1}%\n", normalize(sys_stats.user)));
    details.push_str(&format!("Nice: {:.1}%\n", normalize(sys_stats.nice)));
    details.push_str(&format!("System: {:.1}%\n", normalize(sys_stats.sys)));
    details.push_str(&format!("Idle: {:.1}%\n", normalized_idle));
    details.push_str(&format!("I/O Wait: {:.1}%\n", normalize(sys_stats.iow)));
    details.push_str(&format!("IRQ: {:.1}%\n", normalize(sys_stats.irq)));
    details.push_str(&format!("Soft IRQ: {:.1}%\n", normalize(sys_stats.sirq)));
    details.push_str(&format!("Host: {:.1}%\n", normalize(sys_stats.host)));
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
    details.push_str(&format!("System Active CPU: {:.1}%\n", system_cpu_usage));
    details.push_str(&format!("System Idle CPU: {:.1}%\n", normalized_idle));
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
            format!("{:.1}", system_cpu_usage).red(),
            format!("{:.1}", normalized_idle).green(),
            pid.yellow(),
            thread_count
        );
    }

    Ok(())
}
