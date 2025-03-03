// #![deny(warnings)]
use anyhow::{Context, Result};
use chrono::{DateTime, Local, Timelike};
use clap::Parser;
use colored::*;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::time::{sleep, Duration, Instant};

mod cpu;
mod memory;
mod utils;

use cpu::ThreadCpuInfo;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Package name to monitor
    #[arg(short, long)]
    package: String,

    /// Monitor CPU usage
    #[arg(long)]
    cpu: bool,

    /// Monitor memory usage
    #[arg(long)]
    memory: bool,

    /// Enable verbose output with detailed metrics
    #[arg(short, long)]
    verbose: bool,

    /// Sampling interval in seconds (default: 1)
    #[arg(short, long, default_value_t = 1)]
    interval: u64,
}

#[derive(Default)]
struct CpuTimeSeriesData {
    timestamps: VecDeque<DateTime<Local>>,
    process_cpu: VecDeque<f32>,
    top_threads: VecDeque<Vec<ThreadCpuInfo>>,
}

impl CpuTimeSeriesData {
    fn new() -> Self {
        Self {
            timestamps: VecDeque::new(),
            process_cpu: VecDeque::new(),
            top_threads: VecDeque::new(),
        }
    }

    fn add_data_point(
        &mut self,
        timestamp: DateTime<Local>,
        process_cpu: f32,
        top_threads: Vec<ThreadCpuInfo>,
    ) {
        self.timestamps.push_back(timestamp);
        self.process_cpu.push_back(process_cpu);
        self.top_threads.push_back(top_threads);
    }
}

#[derive(Default)]
struct PeakStats {
    cpu_usage: f32,
    cpu_time: DateTime<Local>,
    memory_usage: u64,
    memory_time: DateTime<Local>,
    restart_count: u32,
    cpu_data: CpuTimeSeriesData,
}

impl PeakStats {
    fn format_current_peaks(&self) -> String {
        let timestamp = Local::now().format("%H:%M:%S").to_string();
        let mut peaks = Vec::new();
        if self.cpu_usage > 0.0 {
            peaks.push(format!(
                "[{}] Peak CPU: {}% at {}",
                timestamp.blue(),
                format!("{:.1}", self.cpu_usage).red(),
                self.cpu_time.format("%H:%M:%S").to_string().blue()
            ));
        }
        if self.memory_usage > 0 {
            peaks.push(format!(
                "[{}] Peak Memory: {} at {}",
                timestamp.blue(),
                utils::format_bytes(self.memory_usage * 1024).red(),
                self.memory_time.format("%H:%M:%S").to_string().blue()
            ));
        }
        peaks.join("\n")
    }
}

fn check_adb() -> Result<()> {
    let output = Command::new("adb")
        .arg("devices")
        .output()
        .context("Failed to execute adb command")?;

    if !output.status.success() {
        anyhow::bail!("ADB command failed");
    }

    let devices = String::from_utf8_lossy(&output.stdout);
    if !devices.lines().skip(1).any(|line| !line.trim().is_empty()) {
        anyhow::bail!("No Android devices connected");
    }

    Ok(())
}

async fn monitor_adb_connection(running: Arc<AtomicBool>) {
    let check_interval = Duration::from_secs(1);
    while running.load(Ordering::SeqCst) {
        if !utils::check_adb_connection() {
            println!("\n{}", "ADB connection lost. Stopping...".red());
            running.store(false, Ordering::SeqCst);
            break;
        }
        sleep(check_interval).await;
    }
}

async fn monitor_process(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let mut peak_stats = PeakStats::default();

    println!("{}", "XPerformance Monitor".green().bold());
    println!("Monitoring package: {}", args.package.cyan());
    println!("Sampling interval: {} seconds", args.interval);

    check_adb()?;

    if !args.cpu && !args.memory {
        println!("No monitoring options selected. Use --cpu or --memory");
        return Ok(());
    }

    // 不再初始化日志，禁用所有日志记录功能
    let logging_enabled = false; // 设为false确保不会记录任何日志

    // Set up signal handling
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
        // 设置中断标志
        utils::set_interrupt_flag();
        println!("\n程序正在退出...");
    })?;

    // Start ADB connection monitoring
    let adb_monitor = {
        let running = running.clone();
        tokio::spawn(async move {
            monitor_adb_connection(running).await;
        })
    };

    let interval = Duration::from_secs(args.interval);

    // 记录程序起始时间点，用于计算绝对采样时间
    let start_time = Instant::now();
    let mut sample_count: u64 = 0;

    let mut last_process_info = utils::get_process_info(&args.package)?;
    println!(
        "Process started with PID {} at {}",
        last_process_info.pid.yellow(),
        last_process_info.start_time.blue()
    );

    // 添加变量以跟踪上次生成图表的小时
    let mut last_chart_hour = -1i32;

    // 添加变量用于跟踪每个线程的时间序列数据
    let mut thread_time_series: std::collections::HashMap<String, Vec<ThreadCpuInfo>> =
        std::collections::HashMap::new();

    // 如果是verbose模式且开启了CPU监控，立即尝试导出一个初始线程数据文件
    // 确保文件被创建但不预先创建空目录
    if args.verbose && args.cpu {
        println!(
            "CPU monitoring enabled, but not creating files until actual thread data is available"
        );
    }

    while running.load(Ordering::SeqCst) {
        // 计算当前应该在的绝对采样点
        sample_count += 1;
        // 使用Duration::from_secs代替直接乘法
        let target_duration = Duration::from_secs(args.interval * sample_count);
        let target_sample_time = start_time + target_duration;
        let now = Instant::now();

        // 如果当前时间已经超过了下一个采样点，需要跳过一些采样点以赶上
        if now > target_sample_time {
            // 计算应该跳过多少个采样点
            let time_behind = now.duration_since(start_time);
            let should_be_at_sample =
                (time_behind.as_secs_f64() / interval.as_secs_f64()).ceil() as u64;

            if should_be_at_sample > sample_count && args.verbose {
                println!(
                    "Warning: Sampling is taking longer than the interval. Skipped {} samples to catch up.",
                    should_be_at_sample - sample_count
                );
            }

            // 直接跳到当前应该在的采样点
            sample_count = should_be_at_sample;
            // 重新计算目标时间点
            let target_duration = Duration::from_secs(args.interval * sample_count);
            let target_sample_time = start_time + target_duration;

            // 如果新目标时间仍然在过去，进行下一次循环并重新计算
            if target_sample_time < now {
                continue;
            }
        }

        // 等待到达计划的采样时间点
        if target_sample_time > now {
            sleep(target_sample_time - now).await;
        }

        // 检查当前是否为整小时，如果是则生成图表和CSV
        let now = Local::now();
        let current_hour = now.hour() as i32;

        // 如果进入了新的整小时且有足够的CPU数据，生成图表
        if current_hour != last_chart_hour && !peak_stats.cpu_data.timestamps.is_empty() && args.cpu
        {
            last_chart_hour = current_hour;

            // 只有在收集了数据后才生成图表
            if peak_stats.cpu_data.timestamps.len() > 1 {
                // 计算整小时标记（格式如 14:00）
                let hour_mark = format!("{}:00", now.hour());

                println!(
                    "{} Generating scheduled CPU chart at {}...",
                    now.format("%H:%M:%S").to_string().blue(),
                    hour_mark.green()
                );

                // 使用预定义chart_hourly_intervals的时间执行图表生成
                let chart_path = match utils::generate_cpu_chart(
                    &args.package,
                    &peak_stats.cpu_data.timestamps,
                    &peak_stats.cpu_data.process_cpu,
                    &last_process_info.pid,
                ) {
                    Ok(path) => path,
                    Err(e) => {
                        eprintln!("Error generating CPU chart: {}", e);
                        continue;
                    }
                };

                if args.verbose && args.cpu {
                    // 为最新时间点的top线程创建CSV
                    if let Some(last_timestamp) = peak_stats.cpu_data.timestamps.back() {
                        if let Some(top_threads) = peak_stats.cpu_data.top_threads.back() {
                            println!("Thread data collection available");
                        }
                    }

                    // 仅打印图表生成信息，不写入日志
                    println!("Scheduled CPU chart generated: {}", chart_path.display());

                    // 添加CSV数据文件的信息
                    let csv_path = chart_path.with_extension("csv");
                    if csv_path.exists() {
                        println!("Scheduled CPU data exported to CSV: {}", csv_path.display());
                    }
                }
            }
        }

        // Check for process restart
        match utils::get_process_info(&args.package) {
            Ok(current_info) => {
                if current_info.pid != last_process_info.pid {
                    peak_stats.restart_count += 1;
                    let timestamp = Local::now().format("%H:%M:%S").to_string();
                    let restart_msg = format!(
                        "[{}] Process restarted! New PID: {} (previous: {}), Start time: {}",
                        timestamp.blue(),
                        current_info.pid.yellow(),
                        last_process_info.pid.red(),
                        current_info.start_time
                    );

                    let peaks = peak_stats.format_current_peaks();
                    if !peaks.is_empty() {
                        println!("{}\n\n{}", peaks, restart_msg);
                    } else {
                        println!("\n{}", restart_msg);
                    }

                    // 移除进程重启时的日志记录，只在整小时和退出时记录
                    last_process_info = current_info;
                }
            }
            Err(e) => {
                println!("\n{}: {}", "Process not found".red(), e);
                running.store(false, Ordering::SeqCst);
                break;
            }
        }

        if args.cpu {
            if let Ok((cpu_usage, timestamp, top_threads)) =
                cpu::sample_cpu(&args.package, args.verbose).await
            {
                if cpu_usage > peak_stats.cpu_usage {
                    peak_stats.cpu_usage = cpu_usage;
                    peak_stats.cpu_time = timestamp;
                }
                peak_stats
                    .cpu_data
                    .add_data_point(timestamp, cpu_usage, top_threads.clone());

                // 将线程数据添加到时间序列跟踪
                if args.verbose {
                    // 打印CPU占用最高的线程信息
                    println!("Top CPU threads:");

                    // 只显示最多5个线程，避免输出过多
                    let display_count = std::cmp::min(5, top_threads.len());
                    for (i, thread) in top_threads.iter().take(display_count).enumerate() {
                        println!(
                            "  {}: {} (TID: {}) - {:.1}%",
                            i + 1,
                            thread.name.cyan(),
                            thread.tid.yellow(),
                            thread.cpu_usage
                        );
                    }

                    // 如果有更多线程，显示总数
                    if top_threads.len() > display_count {
                        println!(
                            "  ... and {} more threads",
                            top_threads.len() - display_count
                        );
                    }
                    println!(); // 空行分隔

                    for thread in &top_threads {
                        let entry = thread_time_series
                            .entry(thread.tid.clone())
                            .or_insert_with(Vec::new);
                        entry.push(thread.clone());
                    }
                }
            }
        }

        if args.memory {
            if let Ok((memory_kb, timestamp)) =
                memory::sample_memory(&args.package, args.verbose).await
            {
                if memory_kb > peak_stats.memory_usage {
                    peak_stats.memory_usage = memory_kb;
                    peak_stats.memory_time = timestamp;
                }
            }
        }
    }

    // Wait for ADB monitor to finish
    let _ = adb_monitor.await;

    // 在结束前生成最终的线程时间序列图表
    if args.verbose && args.cpu && !thread_time_series.is_empty() {
        println!("Program ending, generating final thread time series chart...");
        if let Ok(subdir) = utils::create_timestamp_subdir(&args.package) {
            // 导出最终的线程数据
            match utils::export_thread_data_to_csv(
                subdir.clone(),
                &last_process_info.pid,
                &thread_time_series
                    .values()
                    .flat_map(|v| v.iter().cloned())
                    .collect::<Vec<_>>(),
                false,
            ) {
                Ok(filenames) => {
                    println!(
                        "Final thread data exported to {} CSV files",
                        filenames.len()
                    );
                }
                Err(e) => {
                    println!("Failed to export final thread data to CSV: {}", e);
                }
            }

            // 生成最终的线程时间序列图表
            match utils::generate_thread_time_series_chart(
                subdir,
                &args.package,
                &last_process_info.pid,
                &thread_time_series,
            ) {
                Ok(chart_filename) => {
                    if !chart_filename.is_empty() {
                        println!(
                            "Final thread time series chart generated: {}",
                            chart_filename
                        );
                    }
                }
                Err(e) => {
                    println!("Failed to generate final thread time series chart: {}", e);
                }
            }
        }
    }

    // Print peak stats
    println!("\n{}", "Peak Statistics:".yellow().bold());
    if args.cpu {
        println!(
            "Peak CPU Usage: {}% at {}",
            format!("{:.1}", peak_stats.cpu_usage).red(),
            peak_stats.cpu_time.format("%Y-%m-%d %H:%M:%S")
        );

        // Generate CPU chart if we have collected data
        if args.cpu && peak_stats.cpu_data.timestamps.len() > 1 {
            // Create timestamp-based subdirectory for final charts
            if let Ok(timestamp_dir) = utils::create_timestamp_subdir(&args.package) {
                // Generate CPU usage chart
                if let Ok(chart_path) = utils::generate_cpu_chart(
                    &args.package,
                    &peak_stats.cpu_data.timestamps,
                    &peak_stats.cpu_data.process_cpu,
                    &last_process_info.pid,
                ) {
                    println!("✓ CPU chart generated: {}", chart_path.display());
                }

                // Generate thread data chart if thread data is available
                if !peak_stats.cpu_data.top_threads.is_empty() {
                    println!("Thread data available in final report");
                }
            }
        }
    }
    if args.memory {
        println!(
            "Peak Memory Usage: {} at {}",
            utils::format_bytes(peak_stats.memory_usage * 1024).red(),
            peak_stats.memory_time.format("%Y-%m-%d %H:%M:%S")
        );
    }
    println!(
        "Process Restarts: {}",
        peak_stats.restart_count.to_string().red()
    );

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // 不再调用init_logging初始化日志文件
    // if args.verbose {
    //     utils::init_logging(&args.package, args.cpu, args.memory)?;
    // }

    // 直接调用monitor_process函数
    if let Err(e) = monitor_process(&args).await {
        eprintln!("Monitor error: {}", e);
    }

    Ok(())
}
