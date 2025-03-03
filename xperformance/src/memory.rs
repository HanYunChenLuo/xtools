use crate::utils;
use anyhow::Result;
use chrono::{DateTime, Local};
use colored::*;
use std::collections::VecDeque;

// 定义内存详细类别结构
#[derive(Debug, Clone, Default)]
pub struct MemoryDetails {
    pub java_heap: u64,
    pub native_heap: u64,
    pub code: u64,
    pub stack: u64,
    pub graphics: u64,
    pub private_other: u64,
    pub system: u64,
    pub total_pss: u64,
}

#[derive(Debug, Clone, Default)]
pub struct MemoryTimeSeriesData {
    pub timestamps: VecDeque<DateTime<Local>>,
    pub memory_details: VecDeque<MemoryDetails>,
}

impl MemoryTimeSeriesData {
    pub fn add_data_point(&mut self, timestamp: DateTime<Local>, details: MemoryDetails) {
        // 添加新数据点
        self.timestamps.push_back(timestamp);
        self.memory_details.push_back(details);

        // 保持最多300个数据点
        while self.timestamps.len() > 300 {
            self.timestamps.pop_front();
            self.memory_details.pop_front();
        }
    }
}

pub async fn sample_memory(
    package: &str,
    verbose: bool,
) -> Result<(u64, DateTime<Local>, MemoryDetails)> {
    let timestamp = Local::now();
    let process_info = utils::get_process_info(package)?;
    let pid = &process_info.pid;
    let output = utils::run_adb_command(&["shell", "dumpsys", "meminfo", pid])?;

    let mut total_pss = 0;
    let mut memory_details = MemoryDetails::default();
    let mut in_app_summary = false;
    let mut header_passed = false; // 用于跳过标题行

    // 解析App Summary部分
    for line in output.lines() {
        let line = line.trim();

        // 检测App Summary部分开始
        if line.contains("App Summary") {
            in_app_summary = true;
            continue;
        }

        // 跳过PSS/RSS标题行
        if in_app_summary && (line.contains("Pss(KB)") || line.contains("------")) {
            header_passed = line.contains("------");
            continue;
        }

        // 如果已经过了App Summary部分，则退出解析
        if in_app_summary && line.is_empty() {
            in_app_summary = false;
            continue;
        }

        // 解析App Summary部分的内存信息
        if in_app_summary && header_passed {
            let parts: Vec<&str> = line.split(':').collect();
            if parts.len() >= 2 {
                let category = parts[0].trim();
                let values: Vec<&str> = parts[1].trim().split_whitespace().collect();

                if !values.is_empty() {
                    if let Ok(kb) = values[0].parse::<u64>() {
                        match category {
                            "Java Heap" => memory_details.java_heap = kb,
                            "Native Heap" => memory_details.native_heap = kb,
                            "Code" => memory_details.code = kb,
                            "Stack" => memory_details.stack = kb,
                            "Graphics" => memory_details.graphics = kb,
                            "Private Other" => memory_details.private_other = kb,
                            "System" => memory_details.system = kb,
                            "TOTAL" | "TOTAL PSS" => {
                                memory_details.total_pss = kb;
                                total_pss = kb;
                            }
                            _ => {} // 忽略其他类别
                        }
                    }
                }
            }
        }

        // 如果不在App Summary中，仍然需要查找TOTAL PSS作为备用
        if !in_app_summary && line.starts_with("TOTAL PSS:") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 {
                if let Ok(kb) = parts[2].parse::<u64>() {
                    total_pss = kb;
                    if memory_details.total_pss == 0 {
                        memory_details.total_pss = kb;
                    }
                }
            }
        }
    }

    if verbose {
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

        // 添加App Summary详细信息
        details.push_str("App Summary\n");
        details.push_str(&"-".repeat(80));
        details.push_str("\n");
        details.push_str(&format!(
            "{:<25} {:>15}\n",
            "Java Heap:",
            format!("{} KB", memory_details.java_heap)
        ));
        details.push_str(&format!(
            "{:<25} {:>15}\n",
            "Native Heap:",
            format!("{} KB", memory_details.native_heap)
        ));
        details.push_str(&format!(
            "{:<25} {:>15}\n",
            "Code:",
            format!("{} KB", memory_details.code)
        ));
        details.push_str(&format!(
            "{:<25} {:>15}\n",
            "Stack:",
            format!("{} KB", memory_details.stack)
        ));
        details.push_str(&format!(
            "{:<25} {:>15}\n",
            "Graphics:",
            format!("{} KB", memory_details.graphics)
        ));
        details.push_str(&format!(
            "{:<25} {:>15}\n",
            "Private Other:",
            format!("{} KB", memory_details.private_other)
        ));
        details.push_str(&format!(
            "{:<25} {:>15}\n",
            "System:",
            format!("{} KB", memory_details.system)
        ));
        details.push_str(&format!(
            "{:<25} {:>15}\n",
            "TOTAL PSS:",
            format!("{} KB", memory_details.total_pss)
        ));
        details.push_str("\n\n");

        // Parse memory info for full details
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
                    // 直接显示KB单位，不转换为字节
                    let formatted_size = format!("{} KB", kb);
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
            format!("{} KB", total_pss)
        ));
        details.push_str(&"=".repeat(80));
        details.push_str("\n");

        // Write to log file
        utils::append_to_log(&details)?;
    }

    // Print detailed summary to console
    println!(
        "[{}] Memory Usage: {} KB (Java: {}, Native: {}, Code: {}, Graphics: {})",
        timestamp.format("%H:%M:%S"),
        memory_details.total_pss.to_string().blue(),
        memory_details.java_heap.to_string().green(),
        memory_details.native_heap.to_string().yellow(),
        memory_details.code.to_string().cyan(),
        memory_details.graphics.to_string().magenta()
    );

    Ok((total_pss, timestamp, memory_details))
}
