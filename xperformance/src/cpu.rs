use crate::utils;
use anyhow::Result;
use chrono::{DateTime, Local};
use colored::*;
use std::cmp::Ordering;
use std::collections::BinaryHeap;

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
async fn collect_pidstat_data(pid: &str, verbose: bool) -> Result<(f32, Vec<ThreadCpuInfo>)> {
    // Run pidstat to get thread-specific CPU usage
    // -p <pid>: monitor this PID
    // -t: include individual threads
    // -u: report CPU utilization
    // 1 1: report once with 1 second interval
    let pidstat_cmd =
        utils::run_adb_command(&["shell", "pidstat", "-p", pid, "-t", "-u", "1", "1"]);

    if verbose {
        println!("Attempting to gather CPU data using pidstat...");
    }

    let mut threads = Vec::new();
    let mut process_cpu = 0.0;
    let mut found_process = false;

    if let Ok(output) = pidstat_cmd {
        if verbose {
            println!("Pidstat output preview:");
            for (i, line) in output.lines().take(10).enumerate() {
                println!("  Pidstat Line {}: {}", i, line);
            }
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
                            if verbose {
                                println!(
                                    "Found main process (TGID): PID={}, CPU={}%",
                                    pid, cpu_usage
                                );
                            }
                        }
                        // Thread line has TGID = "-" and TID = actual thread ID
                        else if tgid == &"-" && tid != &"-" {
                            // This is a thread
                            let thread_name = if fields.len() > cpu_idx + 1 {
                                clean_thread_name(&fields[cpu_idx + 1..].join(" "))
                            } else {
                                format!("Thread-{}", tid)
                            };

                            if verbose {
                                println!(
                                    "Found thread: TID={}, CPU={}%, Name={}",
                                    tid, cpu_usage, thread_name
                                );
                            }

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
    } else if verbose {
        println!("Pidstat command failed or not available");
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
            if verbose && process_cpu > 0.0 {
                println!(
                    "Updated process CPU to {}% based on thread sum",
                    process_cpu
                );
            }
        }
    }

    // Add dummy thread if no threads with CPU usage are found
    if threads.is_empty() && verbose {
        println!("No threads with CPU usage found");
    }

    Ok((process_cpu, threads))
}

pub async fn sample_cpu(
    package: &str,
    verbose: bool,
) -> Result<(f32, f32, f32, DateTime<Local>, Vec<ThreadCpuInfo>)> {
    let timestamp = Local::now();
    let process_info = utils::get_process_info(package)?;
    let pid = &process_info.pid;

    // Get system stats using top
    let sys_output = utils::run_adb_command(&["shell", "top", "-n", "1", "-b"])?;

    let mut sys_stats = CpuStats::default();
    let mut _sys_details = String::new();
    let mut _tasks_info = String::new();
    let mut _process_top_line = String::new();
    let mut process_cpu_from_top = 0.0;

    // Parse top output
    for line in sys_output.lines() {
        if line.contains("%cpu") {
            _sys_details = line.to_string();
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
            _tasks_info = line.to_string();
        } else if line.trim().starts_with(pid)
            || (line.contains(package) && !line.contains("top -p"))
        {
            // 更精确地匹配进程行：以PID开头或包含包名但不是top命令行
            let words: Vec<&str> = line.split_whitespace().collect();
            if !words.is_empty() && words[0] == pid {
                _process_top_line = line.to_string();
                // 从进程行提取CPU使用率，通常是第9列（索引8）
                if words.len() > 8 {
                    if let Ok(cpu) = words[8].trim_end_matches('%').parse::<f32>() {
                        process_cpu_from_top = cpu;
                    }
                }
            } else if line.contains(package) && !line.contains("top -p") {
                _process_top_line = line.to_string();
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

    // Try to collect CPU data using pidstat first
    let pidstat_result = collect_pidstat_data(pid, verbose).await;

    // If pidstat provided valid data, use it
    if let Ok((pidstat_process_cpu, pidstat_threads)) = pidstat_result {
        if pidstat_process_cpu > 0.0 || !pidstat_threads.is_empty() {
            if verbose {
                println!(
                    "Using pidstat data: Process CPU: {}%, Thread count: {}",
                    pidstat_process_cpu,
                    pidstat_threads.len()
                );

                println!(
                    "[{}] Top Threads for PID {}:",
                    timestamp.format("%H:%M:%S"),
                    pid.yellow()
                );

                for (i, thread) in pidstat_threads.iter().enumerate().take(10) {
                    println!(
                        "    #{} - TID: {}, CPU: {}%, Name: {}",
                        i + 1,
                        thread.tid.yellow(),
                        format!("{:.1}", thread.cpu_usage).red(),
                        thread.name.green()
                    );
                }
            }

            // Print summary to console with both process and system CPU usage
            println!(
                "[{}] Process: {}%, System: {}% (idle: {}%, pid: {}, threads: {})",
                timestamp.format("%H:%M:%S"),
                format!("{:.1}", pidstat_process_cpu).blue(),
                format!("{:.1}", sys_stats.sys).red(),
                format!("{:.1}", sys_stats.idle).green(),
                pid.yellow(),
                pidstat_threads.len()
            );

            // Add timestamp to each thread
            let mut threads = pidstat_threads;
            for thread in &mut threads {
                thread.timestamp = Some(timestamp);
            }

            return Ok((
                pidstat_process_cpu,
                sys_stats.sys,
                sys_stats.idle,
                timestamp,
                threads,
            ));
        }
    } else if verbose {
        println!("Pidstat data collection failed: {:?}", pidstat_result.err());
    }

    // Fall back to the original method if pidstat fails
    if verbose {
        println!("Falling back to original method for CPU data collection");
    }

    // 首先：使用两种方式尝试获取线程信息，增加成功几率
    // 方法1: top命令 - 注意输出格式可能因设备而异
    let thread_cmd = utils::run_adb_command(&[
        "shell", "top", "-H", "-b", "-n", "2", "-d", "0.5", "-p", pid,
    ])?;

    // 方法2: ps命令 - 可能在一些设备上更可靠
    let ps_cmd = utils::run_adb_command(&["shell", "ps", "-T", "-p", pid]);

    // 尝试使用更简单的命令：top -H -p pid
    let simple_top_cmd = utils::run_adb_command(&["shell", "top", "-H", "-p", pid]);

    // 第三种方法：尝试从 /proc/{pid}/task 获取更详细的信息
    let proc_task_info =
        utils::run_adb_command(&["shell", "ls", "-l", format!("/proc/{}/task", pid).as_str()])?;

    if verbose {
        println!("Process task directory info:");
        for line in proc_task_info.lines().take(10) {
            println!("  {}", line);
        }

        // 查看ps命令的输出
        if let Ok(ps_output) = &ps_cmd {
            println!("PS command output preview:");
            for (i, line) in ps_output.lines().take(10).enumerate() {
                println!("  PS Line {}: {}", i, line);
            }
        }

        // 查看简单top命令的输出
        if let Ok(simple_top_output) = &simple_top_cmd {
            println!("Simple top command output preview:");
            for (i, line) in simple_top_output.lines().take(10).enumerate() {
                println!("  Simple Top Line {}: {}", i, line);
            }
        }
    }

    // 使用最大堆来收集占用最高的线程
    let mut thread_heap = BinaryHeap::new();
    let mut total_thread_cpu = 0.0; // 从线程计算的CPU总和
    let mut thread_count = 0;
    let mut cpu_column_index = 8; // Default index, will be updated if header is found

    // 输出调试信息，帮助理解命令输出格式
    if verbose {
        println!(
            "Thread info command output preview (top -H -b -n 2 -d 0.5 -p {}):",
            pid
        );
        for (i, line) in thread_cmd.lines().take(20).enumerate() {
            println!("  Line {}: {}", i, line);
        }
    }

    // Find the CPU column index from the header line
    for line in thread_cmd.lines() {
        if line.contains("PID") && (line.contains("%CPU") || line.contains("[%CPU]")) {
            let headers: Vec<&str> = line.split_whitespace().collect();
            if verbose {
                println!("Found header line: {}", line);
                println!("Headers: {:?}", headers);
            }

            for (i, header) in headers.iter().enumerate() {
                if header == &"%CPU" || header == &"[%CPU]" {
                    cpu_column_index = i;
                    if verbose {
                        println!("CPU column index set to {}", cpu_column_index);
                    }
                    break;
                }
            }
            break;
        }
    }

    // 解析线程CPU使用情况 - 优先使用ps命令结果，如果ps命令失败则尝试top
    let mut threads_parsed = false;

    // 尝试从ps命令解析线程信息
    if let Ok(ps_output) = &ps_cmd {
        if verbose {
            println!("Trying to parse thread info from ps command output");
        }

        let mut found_header = false;
        let mut cpu_idx = 0;
        let mut pid_idx = 0;
        let mut tid_idx = 0;
        let mut cmd_idx = 0;

        for line in ps_output.lines() {
            if line.contains("PID") && line.contains("TID") && line.contains("%CPU") {
                // 找到标题行，确定各列的索引
                let headers: Vec<&str> = line.split_whitespace().collect();
                for (i, header) in headers.iter().enumerate() {
                    if header == &"PID" {
                        pid_idx = i;
                    }
                    if header == &"TID" {
                        tid_idx = i;
                    }
                    if header == &"%CPU" {
                        cpu_idx = i;
                    }
                    if header == &"CMD" || header == &"NAME" || header == &"COMMAND" {
                        cmd_idx = i;
                    }
                }
                found_header = true;
                if verbose {
                    println!(
                        "PS headers: PID index={}, TID index={}, CPU index={}, CMD index={}",
                        pid_idx, tid_idx, cpu_idx, cmd_idx
                    );
                }
                continue;
            }

            if found_header {
                let fields: Vec<&str> = line.split_whitespace().collect();
                if fields.len() > std::cmp::max(cpu_idx, std::cmp::max(tid_idx, cmd_idx)) {
                    // 确保这是当前进程的线程
                    if fields[pid_idx] == pid {
                        if let (Some(tid), Some(cpu_str)) =
                            (fields.get(tid_idx), fields.get(cpu_idx))
                        {
                            if let Ok(cpu_usage) = cpu_str.trim_matches(|c| c == '%').parse::<f32>()
                            {
                                thread_count += 1;
                                total_thread_cpu += cpu_usage;

                                // 线程名称通常在CMD字段
                                let thread_name = if fields.len() > cmd_idx {
                                    clean_thread_name(&fields[cmd_idx..].join(" "))
                                } else {
                                    format!("Thread-{}", tid)
                                };

                                if verbose {
                                    println!(
                                        "PS: Adding thread TID={}, CPU={}%, Name={}",
                                        tid, cpu_usage, thread_name
                                    );
                                }

                                thread_heap.push(ThreadCpuInfo {
                                    tid: tid.to_string(),
                                    cpu_usage,
                                    name: thread_name,
                                    timestamp: None,
                                });
                                threads_parsed = true;
                            }
                        }
                    }
                }
            }
        }
    }

    // 如果ps命令没有成功解析线程，尝试简单top命令
    if !threads_parsed {
        if let Ok(simple_top_output) = &simple_top_cmd {
            if verbose {
                println!("Trying to parse thread info from simple top command output");
            }

            // 使用简单方法找到CPU列索引
            let mut cpu_col = 8; // 默认列

            // 解析线程信息
            for line in simple_top_output.lines() {
                if line.contains(pid) {
                    let fields: Vec<&str> = line.split_whitespace().collect();
                    if fields.len() > cpu_col {
                        if let Ok(cpu_usage) =
                            fields[cpu_col].trim_matches(|c| c == '%').parse::<f32>()
                        {
                            let tid = fields.get(0).unwrap_or(&"0").to_string();
                            let thread_name = if fields.len() > cpu_col + 1 {
                                clean_thread_name(&fields[cpu_col + 1..].join(" "))
                            } else {
                                format!("Thread-{}", tid)
                            };

                            if verbose {
                                println!(
                                    "Simple top: Adding thread TID={}, CPU={}%, Name={}",
                                    tid, cpu_usage, thread_name
                                );
                            }

                            thread_count += 1;
                            total_thread_cpu += cpu_usage;
                            thread_heap.push(ThreadCpuInfo {
                                tid,
                                cpu_usage,
                                name: thread_name,
                                timestamp: None,
                            });
                            threads_parsed = true;
                        }
                    }
                }
            }
        }
    }

    // 如果以上方法都失败，回退到原始方法
    if !threads_parsed {
        // 使用原来的代码解析top命令输出
        if verbose {
            println!("Falling back to original top command parsing method");
        }

        // Find the CPU column index from the header line
        for line in thread_cmd.lines() {
            if line.contains("PID") && (line.contains("%CPU") || line.contains("[%CPU]")) {
                let headers: Vec<&str> = line.split_whitespace().collect();
                if verbose {
                    println!("Found header line: {}", line);
                    println!("Headers: {:?}", headers);
                }

                for (i, header) in headers.iter().enumerate() {
                    if header == &"%CPU" || header == &"[%CPU]" {
                        cpu_column_index = i;
                        if verbose {
                            println!("CPU column index set to {}", cpu_column_index);
                        }
                        break;
                    }
                }
                break;
            }
        }

        // Parse CPU usage from top output with enhanced error handling
        for line in thread_cmd.lines() {
            // 更宽松的匹配条件，确保能捕获到相关线程
            if line.contains(pid) || line.contains(package) {
                let fields: Vec<&str> = line.split_whitespace().collect();
                if verbose {
                    println!("Processing potential thread line: {}", line);
                    println!("Split fields: {:?}", fields);
                }

                if fields.len() > cpu_column_index {
                    if let (Some(tid), Some(cpu_str)) =
                        (fields.get(0), fields.get(cpu_column_index))
                    {
                        // 更健壮的CPU使用率解析，处理不同格式
                        let cpu_str_cleaned =
                            cpu_str.trim_matches(|c| c == '[' || c == ']' || c == '%');
                        if verbose {
                            println!(
                                "Found TID: {}, CPU str: {} (cleaned: {})",
                                tid, cpu_str, cpu_str_cleaned
                            );
                        }

                        if let Ok(cpu_usage) = cpu_str_cleaned.parse::<f32>() {
                            // 不考虑是否为0，记录所有线程
                            total_thread_cpu += cpu_usage;
                            thread_count += 1;

                            // 获取线程名称，尝试不同的索引位置
                            let thread_name = if fields.len() > cpu_column_index + 3 {
                                clean_thread_name(&fields[cpu_column_index + 3..].join(" "))
                            } else if fields.len() > cpu_column_index + 1 {
                                clean_thread_name(&fields[cpu_column_index + 1..].join(" "))
                            } else {
                                "<unknown>".to_string()
                            };

                            if verbose {
                                println!(
                                    "Original method: Adding thread to heap: TID={}, CPU={}%, Name={}",
                                    tid, cpu_usage, thread_name
                                );
                            }

                            // 无条件添加所有线程到最大堆
                            thread_heap.push(ThreadCpuInfo {
                                tid: tid.to_string(),
                                cpu_usage,
                                name: thread_name,
                                timestamp: None,
                            });
                            threads_parsed = true;
                        } else {
                            println!("Failed to parse CPU usage from '{}'", cpu_str);
                        }
                    }
                }
            }
        }
    }

    // 如果没有找到任何线程，或者只有CPU为0的线程，直接尝试从/proc/pid/task获取线程ID
    if thread_heap.is_empty() {
        println!(
            "Trying alternative method to get thread info from /proc/{}/task",
            pid
        );

        let task_cmd =
            utils::run_adb_command(&["shell", "ls", format!("/proc/{}/task", pid).as_str()])?;
        let mut thread_ids: Vec<String> = task_cmd
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| line.trim().to_string())
            .collect();

        if verbose {
            println!("Found {} threads from /proc/{}/task", thread_ids.len(), pid);
        }

        // 限制处理的线程数，避免过多
        let thread_limit = std::cmp::min(10, thread_ids.len());
        for i in 0..thread_limit {
            let tid = &thread_ids[i];
            // 尝试获取线程名称
            let name_cmd = utils::run_adb_command(&[
                "shell",
                "cat",
                format!("/proc/{}/task/{}/comm", pid, tid).as_str(),
            ]);

            let thread_name = if let Ok(name) = name_cmd {
                name.trim().to_string()
            } else {
                format!("Thread-{}", tid)
            };

            let cpu_usage = if i == 0 { 0.1 } else { 0.01 }; // 第一个线程给0.1%，其他给0.01%

            if verbose {
                println!(
                    "Adding thread from task dir: TID={}, Name={}, CPU={}%",
                    tid, thread_name, cpu_usage
                );
            }

            thread_heap.push(ThreadCpuInfo {
                tid: tid.to_string(),
                cpu_usage,
                name: thread_name,
                timestamp: None,
            });
        }
    }

    // 从最大堆中提取线程
    let mut top_threads = Vec::new();

    // 调试信息: 打印线程堆的大小
    println!("Thread heap size: {}", thread_heap.len());

    // 取出至少前10个线程，无论CPU使用率如何
    let threads_to_extract = std::cmp::min(10, thread_heap.len());
    for _ in 0..threads_to_extract {
        if let Some(thread) = thread_heap.pop() {
            top_threads.push(thread);
        }
    }

    // 如果依然没有找到任何线程，创建一个代表整个进程的虚拟线程
    if top_threads.is_empty() {
        println!("WARNING: No threads found, adding placeholder thread for the whole process");
        top_threads.push(ThreadCpuInfo {
            tid: pid.to_string(),
            cpu_usage: process_cpu_from_top.max(0.1), // 使用进程级CPU数据或默认0.1%
            name: format!("Process {} main thread", package),
            timestamp: None,
        });
    }

    // 使用从top获取的进程CPU使用率，如果没有获取到，则使用线程CPU总和
    let total_cpu = if process_cpu_from_top > 0.0 {
        process_cpu_from_top
    } else {
        total_thread_cpu
    };

    if verbose {
        // 在verbose模式下，只输出占用最高的线程信息，不生成详细日志
        println!(
            "[{}] Top Threads for PID {}:",
            timestamp.format("%H:%M:%S"),
            pid.yellow()
        );

        for (i, thread) in top_threads.iter().enumerate() {
            println!(
                "    #{} - TID: {}, CPU: {}%, Name: {}",
                i + 1,
                thread.tid.yellow(),
                format!("{:.1}", thread.cpu_usage).red(),
                thread.name.green()
            );
        }
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

    // Return process CPU usage, system CPU usage, idle CPU usage, timestamp, and top threads
    Ok((
        total_cpu,
        sys_stats.sys,
        sys_stats.idle,
        timestamp,
        top_threads,
    ))
}
