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
    system_cpu: VecDeque<f32>,
    idle_cpu: VecDeque<f32>,
    top_threads: VecDeque<Vec<ThreadCpuInfo>>,
}

impl CpuTimeSeriesData {
    fn new() -> Self {
        Self {
            timestamps: VecDeque::new(),
            process_cpu: VecDeque::new(),
            system_cpu: VecDeque::new(),
            idle_cpu: VecDeque::new(),
            top_threads: VecDeque::new(),
        }
    }

    fn add_data_point(
        &mut self,
        timestamp: DateTime<Local>,
        process_cpu: f32,
        system_cpu: f32,
        idle_cpu: f32,
        top_threads: Vec<ThreadCpuInfo>,
    ) {
        self.timestamps.push_back(timestamp);
        self.process_cpu.push_back(process_cpu);
        self.system_cpu.push_back(system_cpu);
        self.idle_cpu.push_back(idle_cpu);
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

    // 如果不是verbose模式，才初始化日志记录
    let mut logging_enabled = false;
    if args.verbose && !args.cpu {
        logging_enabled = true;
        let path = utils::init_logging(&args.package, args.cpu, args.memory)?;
        println!("Logging to: {}", path.display());
        utils::append_to_log(&format!(
            "Performance monitoring for package: {}\nSampling interval: {} seconds\n",
            args.package, args.interval
        ))?;
    } else if args.verbose && args.cpu {
        println!("Verbose mode with CPU monitoring: Top thread information will be collected instead of detailed logs");
    }

    // Set up signal handling
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })?;

    // Start ADB connection monitoring
    let adb_monitor = {
        let running = running.clone();
        tokio::spawn(async move {
            monitor_adb_connection(running).await;
        })
    };

    let interval = Duration::from_secs(args.interval);
    let mut next_sample = Instant::now();
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

                match utils::generate_cpu_chart(
                    &args.package,
                    &peak_stats.cpu_data.timestamps,
                    &peak_stats.cpu_data.process_cpu,
                    &peak_stats.cpu_data.system_cpu,
                    &peak_stats.cpu_data.idle_cpu,
                    &last_process_info.pid,
                ) {
                    Ok(chart_path) => {
                        println!("Scheduled CPU chart generated: {}", chart_path.display());

                        // 如果是verbose模式且开启了CPU监控，导出top线程信息
                        if args.verbose && args.cpu {
                            // 为最新时间点的top线程创建CSV
                            if let Some(last_timestamp) = peak_stats.cpu_data.timestamps.back() {
                                if let Some(top_threads) = peak_stats.cpu_data.top_threads.back() {
                                    let timestamp_str =
                                        last_timestamp.format("%Y%m%d_%H%M%S").to_string();

                                    // 导出CSV
                                    match utils::export_top_threads_to_csv(
                                        chart_path
                                            .parent()
                                            .unwrap_or(&PathBuf::from("."))
                                            .to_path_buf(),
                                        &timestamp_str,
                                        &last_process_info.pid,
                                        top_threads,
                                    ) {
                                        Ok(csv_filename) => {
                                            if !csv_filename.is_empty() {
                                                let full_path = chart_path
                                                    .parent()
                                                    .unwrap_or(&PathBuf::from("."))
                                                    .join(&csv_filename);
                                                println!(
                                                    "Peak thread data exported to CSV: {}",
                                                    full_path.display()
                                                );
                                            } else {
                                                println!("Skipped exporting peak thread data to CSV (no threads)");
                                            }
                                        }
                                        Err(e) => {
                                            println!("Failed to export top threads to CSV: {}", e);
                                        }
                                    }

                                    // 生成线程图表
                                    match utils::generate_top_threads_chart(
                                        chart_path
                                            .parent()
                                            .unwrap_or(&PathBuf::from("."))
                                            .to_path_buf(),
                                        &timestamp_str,
                                        &args.package,
                                        &last_process_info.pid,
                                        top_threads,
                                    ) {
                                        Ok(chart_filename) => {
                                            let full_path = chart_path
                                                .parent()
                                                .unwrap_or(&PathBuf::from("."))
                                                .join(&chart_filename);
                                            println!(
                                                "Top threads chart generated: {}",
                                                full_path.display()
                                            );
                                        }
                                        Err(e) => {
                                            println!("Failed to generate top threads chart: {}", e);
                                        }
                                    }
                                }
                            }
                        } else if logging_enabled {
                            utils::append_to_log(&format!(
                                "Scheduled CPU chart generated at {}: {}\n",
                                hour_mark,
                                chart_path.display()
                            ))?;

                            // 添加CSV数据文件的日志记录
                            let csv_path = chart_path.with_extension("csv");
                            if csv_path.exists() {
                                utils::append_to_log(&format!(
                                    "Scheduled CPU data exported to CSV at {}: {}\n",
                                    hour_mark,
                                    csv_path.display()
                                ))?;
                            }
                        }
                    }
                    Err(e) => {
                        println!("Failed to generate scheduled CPU chart: {}", e);
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
                    let peaks = peak_stats.format_current_peaks();
                    let restart_msg = format!(
                        "[{}] Process restarted! New PID: {} (previous: {}), Start time: {}",
                        timestamp.blue(),
                        current_info.pid.yellow(),
                        last_process_info.pid.red(),
                        current_info.start_time
                    );
                    if !peaks.is_empty() {
                        println!("{}\n\n{}", peaks, restart_msg);
                    } else {
                        println!("\n{}", restart_msg);
                    }
                    if logging_enabled {
                        utils::append_to_log(&format!("{}\n\n{}\n", peaks, restart_msg))?;
                    }
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
            if let Ok((cpu_usage, system_cpu, idle_cpu, timestamp, top_threads)) =
                cpu::sample_cpu(&args.package, args.verbose).await
            {
                if cpu_usage > peak_stats.cpu_usage {
                    peak_stats.cpu_usage = cpu_usage;
                    peak_stats.cpu_time = timestamp;
                }
                peak_stats.cpu_data.add_data_point(
                    timestamp,
                    cpu_usage,
                    system_cpu,
                    idle_cpu,
                    top_threads.clone(),
                );

                // 将线程数据添加到时间序列跟踪
                if args.verbose {
                    for thread in &top_threads {
                        let entry = thread_time_series
                            .entry(thread.tid.clone())
                            .or_insert_with(Vec::new);
                        entry.push(thread.clone());
                    }

                    // 每10个样本导出一次线程数据
                    if peak_stats.cpu_data.timestamps.len() % 10 == 0 {
                        if let Ok(subdir) = utils::create_timestamp_subdir(&args.package) {
                            match utils::export_thread_data_to_csv(
                                subdir.clone(),
                                &last_process_info.pid,
                                &top_threads,
                                true,
                            ) {
                                Ok(filenames) => {
                                    if !filenames.is_empty() {
                                        println!(
                                            "Thread data exported to {} CSV files",
                                            filenames.len()
                                        );
                                    }
                                }
                                Err(e) => {
                                    println!("Failed to export thread data to CSV: {}", e);
                                }
                            }

                            // 生成线程时间序列图表
                            match utils::generate_thread_time_series_chart(
                                subdir,
                                &args.package,
                                &last_process_info.pid,
                                &thread_time_series,
                            ) {
                                Ok(chart_filename) => {
                                    if !chart_filename.is_empty() {
                                        println!(
                                            "Thread time series chart generated: {}",
                                            chart_filename
                                        );
                                    }
                                }
                                Err(e) => {
                                    println!("Failed to generate thread time series chart: {}", e);
                                }
                            }
                        }
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

        next_sample += interval;
        let now = Instant::now();
        if next_sample > now {
            sleep(next_sample - now).await;
        } else {
            next_sample = now + interval;
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
                match utils::generate_cpu_chart(
                    &args.package,
                    &peak_stats.cpu_data.timestamps,
                    &peak_stats.cpu_data.process_cpu,
                    &peak_stats.cpu_data.system_cpu,
                    &peak_stats.cpu_data.idle_cpu,
                    &last_process_info.pid,
                ) {
                    Ok(chart_path) => {
                        println!("Final CPU chart generated: {}", chart_path.display());
                    }
                    Err(e) => {
                        eprintln!("Failed to generate CPU chart: {}", e);
                    }
                }

                // Generate thread data chart if thread data is available
                if !peak_stats.cpu_data.top_threads.is_empty() {
                    let timestamp = Local::now().format("%Y%m%d_%H%M%S").to_string();

                    // Export top threads to CSV
                    if let Ok(csv_filename) = utils::export_top_threads_to_csv(
                        timestamp_dir.clone(),
                        &timestamp,
                        &last_process_info.pid,
                        &peak_stats
                            .cpu_data
                            .top_threads
                            .back()
                            .unwrap_or(&Vec::new()),
                    ) {
                        println!("Final thread data exported to CSV: {}", csv_filename);
                    }

                    // Generate thread chart
                    if let Ok(chart_filename) = utils::generate_top_threads_chart(
                        timestamp_dir,
                        &timestamp,
                        &args.package,
                        &last_process_info.pid,
                        &peak_stats
                            .cpu_data
                            .top_threads
                            .back()
                            .unwrap_or(&Vec::new()),
                    ) {
                        println!("Final thread chart generated: {}", chart_filename);
                    }
                } else {
                    println!("No thread data available for chart generation");
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

    // Write peak stats to log if enabled
    if logging_enabled {
        utils::append_to_log(&format!("\nPeak Statistics:\n{}\n", "-".repeat(80)))?;
        if args.cpu {
            utils::append_to_log(&format!(
                "Peak CPU Usage: {:.1}% at {}\n",
                peak_stats.cpu_usage,
                peak_stats.cpu_time.format("%Y-%m-%d %H:%M:%S")
            ))?;
        }
        if args.memory {
            utils::append_to_log(&format!(
                "Peak Memory Usage: {} at {}\n",
                utils::format_bytes(peak_stats.memory_usage * 1024),
                peak_stats.memory_time.format("%Y-%m-%d %H:%M:%S")
            ))?;
        }
        utils::append_to_log(&format!("Process Restarts: {}\n", peak_stats.restart_count))?;
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let mut peak_stats = PeakStats::default();

    println!("{}", "XPerformance Monitor".green().bold());
    println!("Monitoring package: {}", args.package.cyan());
    println!("Sampling interval: {} seconds", args.interval);

    check_adb()?;

    if !args.cpu && !args.memory {
        println!("No monitoring options selected. Use --cpu or --memory");
        return Ok(());
    }

    // 如果不是verbose模式，才初始化日志记录
    let mut logging_enabled = false;
    if args.verbose && !args.cpu {
        logging_enabled = true;
        let path = utils::init_logging(&args.package, args.cpu, args.memory)?;
        println!("Logging to: {}", path.display());
        utils::append_to_log(&format!(
            "Performance monitoring for package: {}\nSampling interval: {} seconds\n",
            args.package, args.interval
        ))?;
    } else if args.verbose && args.cpu {
        println!("Verbose mode with CPU monitoring: Top thread information will be collected instead of detailed logs");
    }

    // Set up signal handling
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })?;

    // Start ADB connection monitoring
    let adb_monitor = {
        let running = running.clone();
        tokio::spawn(async move {
            monitor_adb_connection(running).await;
        })
    };

    let interval = Duration::from_secs(args.interval);
    let mut next_sample = Instant::now();
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

                match utils::generate_cpu_chart(
                    &args.package,
                    &peak_stats.cpu_data.timestamps,
                    &peak_stats.cpu_data.process_cpu,
                    &peak_stats.cpu_data.system_cpu,
                    &peak_stats.cpu_data.idle_cpu,
                    &last_process_info.pid,
                ) {
                    Ok(chart_path) => {
                        println!("Scheduled CPU chart generated: {}", chart_path.display());

                        // 如果是verbose模式且开启了CPU监控，导出top线程信息
                        if args.verbose && args.cpu {
                            // 为最新时间点的top线程创建CSV
                            if let Some(last_timestamp) = peak_stats.cpu_data.timestamps.back() {
                                if let Some(top_threads) = peak_stats.cpu_data.top_threads.back() {
                                    let timestamp_str =
                                        last_timestamp.format("%Y%m%d_%H%M%S").to_string();

                                    // 导出CSV
                                    match utils::export_top_threads_to_csv(
                                        chart_path
                                            .parent()
                                            .unwrap_or(&PathBuf::from("."))
                                            .to_path_buf(),
                                        &timestamp_str,
                                        &last_process_info.pid,
                                        top_threads,
                                    ) {
                                        Ok(csv_filename) => {
                                            if !csv_filename.is_empty() {
                                                let full_path = chart_path
                                                    .parent()
                                                    .unwrap_or(&PathBuf::from("."))
                                                    .join(&csv_filename);
                                                println!(
                                                    "Peak thread data exported to CSV: {}",
                                                    full_path.display()
                                                );
                                            } else {
                                                println!("Skipped exporting peak thread data to CSV (no threads)");
                                            }
                                        }
                                        Err(e) => {
                                            println!("Failed to export top threads to CSV: {}", e);
                                        }
                                    }

                                    // 生成线程图表
                                    match utils::generate_top_threads_chart(
                                        chart_path
                                            .parent()
                                            .unwrap_or(&PathBuf::from("."))
                                            .to_path_buf(),
                                        &timestamp_str,
                                        &args.package,
                                        &last_process_info.pid,
                                        top_threads,
                                    ) {
                                        Ok(chart_filename) => {
                                            let full_path = chart_path
                                                .parent()
                                                .unwrap_or(&PathBuf::from("."))
                                                .join(&chart_filename);
                                            println!(
                                                "Top threads chart generated: {}",
                                                full_path.display()
                                            );
                                        }
                                        Err(e) => {
                                            println!("Failed to generate top threads chart: {}", e);
                                        }
                                    }
                                }
                            }
                        } else if logging_enabled {
                            utils::append_to_log(&format!(
                                "Scheduled CPU chart generated at {}: {}\n",
                                hour_mark,
                                chart_path.display()
                            ))?;

                            // 添加CSV数据文件的日志记录
                            let csv_path = chart_path.with_extension("csv");
                            if csv_path.exists() {
                                utils::append_to_log(&format!(
                                    "Scheduled CPU data exported to CSV at {}: {}\n",
                                    hour_mark,
                                    csv_path.display()
                                ))?;
                            }
                        }
                    }
                    Err(e) => {
                        println!("Failed to generate scheduled CPU chart: {}", e);
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
                    let peaks = peak_stats.format_current_peaks();
                    let restart_msg = format!(
                        "[{}] Process restarted! New PID: {} (previous: {}), Start time: {}",
                        timestamp.blue(),
                        current_info.pid.yellow(),
                        last_process_info.pid.red(),
                        current_info.start_time
                    );
                    if !peaks.is_empty() {
                        println!("{}\n\n{}", peaks, restart_msg);
                    } else {
                        println!("\n{}", restart_msg);
                    }
                    if logging_enabled {
                        utils::append_to_log(&format!("{}\n\n{}\n", peaks, restart_msg))?;
                    }
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
            if let Ok((cpu_usage, system_cpu, idle_cpu, timestamp, top_threads)) =
                cpu::sample_cpu(&args.package, args.verbose).await
            {
                if cpu_usage > peak_stats.cpu_usage {
                    peak_stats.cpu_usage = cpu_usage;
                    peak_stats.cpu_time = timestamp;
                }
                peak_stats.cpu_data.add_data_point(
                    timestamp,
                    cpu_usage,
                    system_cpu,
                    idle_cpu,
                    top_threads.clone(),
                );

                // 将线程数据添加到时间序列跟踪
                if args.verbose {
                    for thread in &top_threads {
                        let entry = thread_time_series
                            .entry(thread.tid.clone())
                            .or_insert_with(Vec::new);
                        entry.push(thread.clone());
                    }

                    // 每10个样本导出一次线程数据
                    if peak_stats.cpu_data.timestamps.len() % 10 == 0 {
                        if let Ok(subdir) = utils::create_timestamp_subdir(&args.package) {
                            match utils::export_thread_data_to_csv(
                                subdir.clone(),
                                &last_process_info.pid,
                                &top_threads,
                                true,
                            ) {
                                Ok(filenames) => {
                                    if !filenames.is_empty() {
                                        println!(
                                            "Thread data exported to {} CSV files",
                                            filenames.len()
                                        );
                                    }
                                }
                                Err(e) => {
                                    println!("Failed to export thread data to CSV: {}", e);
                                }
                            }

                            // 生成线程时间序列图表
                            match utils::generate_thread_time_series_chart(
                                subdir,
                                &args.package,
                                &last_process_info.pid,
                                &thread_time_series,
                            ) {
                                Ok(chart_filename) => {
                                    if !chart_filename.is_empty() {
                                        println!(
                                            "Thread time series chart generated: {}",
                                            chart_filename
                                        );
                                    }
                                }
                                Err(e) => {
                                    println!("Failed to generate thread time series chart: {}", e);
                                }
                            }
                        }
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

        next_sample += interval;
        let now = Instant::now();
        if next_sample > now {
            sleep(next_sample - now).await;
        } else {
            next_sample = now + interval;
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
                match utils::generate_cpu_chart(
                    &args.package,
                    &peak_stats.cpu_data.timestamps,
                    &peak_stats.cpu_data.process_cpu,
                    &peak_stats.cpu_data.system_cpu,
                    &peak_stats.cpu_data.idle_cpu,
                    &last_process_info.pid,
                ) {
                    Ok(chart_path) => {
                        println!("Final CPU chart generated: {}", chart_path.display());
                    }
                    Err(e) => {
                        eprintln!("Failed to generate CPU chart: {}", e);
                    }
                }

                // Generate thread data chart if thread data is available
                if !peak_stats.cpu_data.top_threads.is_empty() {
                    let timestamp = Local::now().format("%Y%m%d_%H%M%S").to_string();

                    // Export top threads to CSV
                    if let Ok(csv_filename) = utils::export_top_threads_to_csv(
                        timestamp_dir.clone(),
                        &timestamp,
                        &last_process_info.pid,
                        &peak_stats
                            .cpu_data
                            .top_threads
                            .back()
                            .unwrap_or(&Vec::new()),
                    ) {
                        println!("Final thread data exported to CSV: {}", csv_filename);
                    }

                    // Generate thread chart
                    if let Ok(chart_filename) = utils::generate_top_threads_chart(
                        timestamp_dir,
                        &timestamp,
                        &args.package,
                        &last_process_info.pid,
                        &peak_stats
                            .cpu_data
                            .top_threads
                            .back()
                            .unwrap_or(&Vec::new()),
                    ) {
                        println!("Final thread chart generated: {}", chart_filename);
                    }
                } else {
                    println!("No thread data available for chart generation");
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

    // Write peak stats to log if enabled
    if logging_enabled {
        utils::append_to_log(&format!("\nPeak Statistics:\n{}\n", "-".repeat(80)))?;
        if args.cpu {
            utils::append_to_log(&format!(
                "Peak CPU Usage: {:.1}% at {}\n",
                peak_stats.cpu_usage,
                peak_stats.cpu_time.format("%Y-%m-%d %H:%M:%S")
            ))?;
        }
        if args.memory {
            utils::append_to_log(&format!(
                "Peak Memory Usage: {} at {}\n",
                utils::format_bytes(peak_stats.memory_usage * 1024),
                peak_stats.memory_time.format("%Y-%m-%d %H:%M:%S")
            ))?;
        }
        utils::append_to_log(&format!("Process Restarts: {}\n", peak_stats.restart_count))?;
    }

    Ok(())
}
