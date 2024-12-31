use crate::utils;
use anyhow::Result;
use chrono::{DateTime, Local};
use colored::*;

pub async fn sample_memory(package: &str, enable_logging: bool) -> Result<(u64, DateTime<Local>)> {
    let timestamp = Local::now();
    let process_info = utils::get_process_info(package)?;
    let pid = &process_info.pid;
    let output = utils::run_adb_command(&["shell", "dumpsys", "meminfo", pid])?;

    let mut total_pss = 0;

    // Parse TOTAL PSS first
    for line in output.lines() {
        let line = line.trim();
        if line.starts_with("TOTAL PSS:") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 {
                if let Ok(kb) = parts[2].parse::<u64>() {
                    total_pss = kb;
                    break;
                }
            }
        }
    }

    if enable_logging {
        let mut details = String::new();
        let mut current_section = String::new();

        // Add section header
        details.push_str("Memory Usage Details\n");
        details.push_str(&"=".repeat(80));
        details.push_str("\n\n");

        // Add process information
        details.push_str(&format!("Process ID: {}\n", pid));
        details.push_str(&format!("Package Name: {}\n", package));
        details.push_str(&format!("Start Time: {}\n", process_info.start_time));
        details.push_str("\n");

        // Parse memory info
        for line in output.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            // Start of a new section
            if line.ends_with(':') || line.contains("TOTAL") {
                if !current_section.is_empty() {
                    details.push_str(&current_section);
                    details.push_str("\n");
                }
                current_section = format!("\n{}\n{}\n", line, "-".repeat(80));
                continue;
            }

            // Parse memory values and format them
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                // Try to parse the second column as a number (memory value)
                if let Ok(kb) = parts[1].parse::<u64>() {
                    let formatted_size = utils::format_bytes(kb * 1024);
                    // Left-align name (40 chars), right-align memory value (15 chars)
                    current_section.push_str(&format!("{:<40} {:>15}\n", parts[0], formatted_size));
                } else {
                    // If not a memory line, just add it with indentation
                    current_section.push_str(&format!("    {}\n", line));
                }
            } else {
                // Lines that don't match the pattern
                current_section.push_str(&format!("    {}\n", line));
            }
        }

        // Add the last section if any
        if !current_section.is_empty() {
            details.push_str(&current_section);
        }

        // Add summary section
        details.push_str("\nMemory Summary\n");
        details.push_str(&"=".repeat(80));
        details.push_str("\n");
        details.push_str(&format!(
            "{:<40} {:>15}\n",
            "Total PSS",
            utils::format_bytes(total_pss * 1024)
        ));
        details.push_str(&"=".repeat(80));
        details.push_str("\n");

        // Write to log file
        utils::append_to_log(&details)?;
    }

    // Print summary to console
    println!(
        "[{}] Memory Usage: {}",
        timestamp.format("%H:%M:%S"),
        utils::format_bytes(total_pss * 1024).blue()
    );

    Ok((total_pss, timestamp))
}
