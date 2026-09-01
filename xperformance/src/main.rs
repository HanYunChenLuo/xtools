#![deny(warnings)]
use anyhow::{Context, Result};
use chrono::{DateTime, Local, Timelike};
use clap::Parser;
use colored::*;
use std::collections::VecDeque;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::time::{sleep, Duration, Instant};

mod cpu;
mod memory;
mod utils;

use cpu::ThreadCpuInfo;
use memory::MemoryTimeSeriesData;

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

    /// Monitor thread activity
    #[arg(long)]
    thread: bool,

    /// Sampling interval in milliseconds (default: 1000)
    #[arg(short, long, default_value_t = 1000)]
    interval: u64,
}

#[derive(Default)]
struct CpuTimeSeriesData {
    timestamps: VecDeque<DateTime<Local>>,
    process_cpu: VecDeque<f32>,
    top_threads: VecDeque<Vec<ThreadCpuInfo>>,
}

impl CpuTimeSeriesData {
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

/// 单个 PID 的监控状态。多进程应用会有多个 PidStats，按 pid 索引在 HashMap 中。
#[derive(Default)]
struct PidStats {
    cpu_data: CpuTimeSeriesData,
    memory_data: MemoryTimeSeriesData,
    cpu_usage: f32, // 该 PID 的峰值 CPU %
    cpu_time: Option<DateTime<Local>>, // 该 PID 达到峰值 CPU 的时间（None = 尚无采样）
    memory_usage: u64, // 该 PID 的峰值内存 KB
    memory_time: Option<DateTime<Local>>, // 该 PID 达到峰值内存的时间（None = 尚无采样）
    start_time: String, // 该 PID 的启动时间
    active: bool, // 是否仍在运行（动态跟随：消失的 PID 置 false 但保留数据）
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
    println!("{}", "XPerformance Monitor".green().bold());
    println!("Monitoring package: {}", args.package.cyan());
    println!("Sampling interval: {} ms", args.interval);

    check_adb()?;

    if !args.cpu && !args.memory {
        println!("No monitoring options selected. Use --cpu or --memory");
        return Ok(());
    }

    // Set up signal handling
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
        // 设置中断标志
        utils::set_interrupt_flag();
        println!("\n程序正在退出...");
    })?;

    // 设置ADB连接监控
    let adb_monitor = {
        let running = running.clone();
        tokio::spawn(async move {
            monitor_adb_connection(running).await;
        })
    };

    // 记录程序起始时间点，用于计算绝对采样时间
    let start_time = Instant::now();
    let mut sample_count: u64 = 0;

    // 按 PID 索引的监控状态：多进程应用会有多个条目，动态跟随
    let mut pid_stats: std::collections::HashMap<String, PidStats> = std::collections::HashMap::new();
    let mut restart_count: u32 = 0;

    // 初始枚举进程
    let initial_processes = utils::get_all_processes(&args.package)?;
    for p in &initial_processes {
        println!(
            "Monitoring PID {} (start: {})",
            p.pid.yellow(),
            p.start_time.blue()
        );
        pid_stats.insert(
            p.pid.clone(),
            PidStats {
                start_time: p.start_time.clone(),
                active: true,
                ..Default::default()
            },
        );
    }

    // 添加变量以跟踪上次生成图表的小时（初始化为当前小时，避免启动时立即触发）
    let mut last_chart_hour = Local::now().hour() as i32;

    // 内存图表节流：记录上次生成图表时的累计采样数，每 MEMORY_CHART_INTERVAL 个新采样生成一次。
    const MEMORY_CHART_INTERVAL: usize = 30;
    let mut last_memory_chart_sample_count: usize = 0;
    let mut memory_sample_count: usize = 0;

    // 每个线程的时间序列数据，按 pid → thread_name → Vec<ThreadCpuInfo> 组织
    let mut thread_time_series: std::collections::HashMap<
        String,
        std::collections::HashMap<String, Vec<ThreadCpuInfo>>,
    > = std::collections::HashMap::new();

    // 如果开启了CPU监控，立即尝试导出一个初始线程数据文件
    // 确保文件被创建但不预先创建空目录
    if args.cpu {
        println!(
            "CPU monitoring enabled, but not creating files until actual thread data is available"
        );
    }

    while running.load(Ordering::SeqCst) {
        let current_time = Instant::now();
        let elapsed = current_time.duration_since(start_time);

        // Calculate what sample we should be at based on elapsed time
        let should_be_at_sample = elapsed.as_millis() as u64 / args.interval;

        // Handle skipping samples if we're behind
        if should_be_at_sample > sample_count {
            let samples_to_skip = should_be_at_sample - sample_count - 1;
            if samples_to_skip > 0 {
                println!(
                    "{}",
                    format!(
                        "⚠️ System too slow! Skipping {} sample(s) to maintain timing.",
                        samples_to_skip
                    )
                    .yellow()
                );
            }
            sample_count = should_be_at_sample;
        }

        // 动态跟随：每轮重新枚举包名下的所有进程
        let current_processes = match utils::get_all_processes(&args.package) {
            Ok(ps) => ps,
            Err(e) => {
                // 包名下无任何进程（Process not found）：所有已知 PID 标记失活
                for s in pid_stats.values_mut() {
                    if s.active {
                        s.active = false;
                        restart_count += 1;
                    }
                }
                println!("No process found for package: {}", e);
                if args.cpu {
                    tokio::time::sleep(tokio::time::Duration::from_millis(args.interval)).await;
                }
                continue;
            }
        };
        // 新出现的 PID 加入监控；已有的标记 active=true
        for p in &current_processes {
            pid_stats
                .entry(p.pid.clone())
                .or_insert_with(|| PidStats {
                    start_time: p.start_time.clone(),
                    active: true,
                    ..Default::default()
                });
            if let Some(s) = pid_stats.get_mut(&p.pid) {
                s.active = true;
                if s.start_time.is_empty() {
                    s.start_time = p.start_time.clone();
                }
            }
        }

        // 收集本轮活跃的 PID（保持稳定顺序）
        let active_pids: Vec<String> = pid_stats
            .iter()
            .filter(|(_, s)| s.active)
            .map(|(pid, _)| pid.clone())
            .collect();

        // CPU 采样：两阶段，所有 PID 共享一个 sleep 窗口
        if args.cpu && !active_pids.is_empty() {
            // phase1：对每个活跃 PID 读取第一次 jiffies
            let mut phase1_results: std::collections::HashMap<String, cpu::CpuSample1> =
                std::collections::HashMap::new();
            for pid in &active_pids {
                match cpu::sample_cpu_phase1(pid).await {
                    Ok(p1) => {
                        phase1_results.insert(pid.clone(), p1);
                    }
                    Err(e) => {
                        if e.to_string().contains("Process not found") {
                            if let Some(s) = pid_stats.get_mut(pid) {
                                s.active = false;
                                restart_count += 1;
                            }
                            println!("\n[{}] PID {} disappeared.", Local::now().format("%H:%M:%S"), pid.yellow());
                        } else {
                            println!("Error sampling CPU (phase1) for pid {}: {}", pid, e);
                        }
                    }
                }
            }

            // sleep 窗口：所有 PID 共享这一个 interval
            tokio::time::sleep(tokio::time::Duration::from_millis(args.interval)).await;

            // phase2：对每个有 phase1 结果的 PID 读取第二次 jiffies 并计算
            for (pid, p1) in &phase1_results {
                match cpu::sample_cpu_phase2(p1).await {
                    Ok((process_cpu, timestamp, threads)) => {
                        let s = pid_stats.get_mut(pid).expect("pid present in phase1_results implies pid_stats has it");
                        s.cpu_data.add_data_point(timestamp, process_cpu, threads.clone());
                        if s.cpu_time.is_none() || process_cpu > s.cpu_usage {
                            s.cpu_usage = process_cpu;
                            s.cpu_time = Some(timestamp);
                        }

                        // 打印 top 线程
                        let mut top_threads = threads.clone();
                        top_threads.sort_by(|a, b| b.cpu_usage.partial_cmp(&a.cpu_usage).unwrap());
                        let top_threads = top_threads.into_iter().take(5).collect::<Vec<_>>();
                        if !top_threads.is_empty() {
                            println!("Top Threads (pid {}):", pid.yellow());
                            for thread in top_threads {
                                println!(
                                    "  {} ({}): {}%",
                                    thread.name.green(),
                                    thread.tid.yellow(),
                                    format!("{:.1}", thread.cpu_usage).blue()
                                );
                            }
                        }

                        // 保存线程时间序列数据（按 pid → name 组织）
                        if args.thread {
                            let per_pid = thread_time_series.entry(pid.clone()).or_default();
                            for thread in &threads {
                                if thread.cpu_usage > 0.0 {
                                    let thread_data = per_pid.entry(thread.name.clone()).or_default();
                                    thread_data.push(thread.clone());
                                }
                            }
                        }
                    }
                    Err(e) => {
                        if e.to_string().contains("Process not found") {
                            if let Some(s) = pid_stats.get_mut(pid) {
                                s.active = false;
                                restart_count += 1;
                            }
                            println!("\n[{}] PID {} disappeared during sampling.", Local::now().format("%H:%M:%S"), pid.yellow());
                        } else {
                            println!("Error sampling CPU (phase2) for pid {}: {}", pid, e);
                        }
                    }
                }
            }
        }

        // Memory 采样：对每个活跃 PID 各采一次（无内部 sleep）
        if args.memory && !active_pids.is_empty() {
            for pid in &active_pids {
                match memory::sample_memory(pid).await {
                    Ok((total_pss, timestamp, memory_details)) => {
                        let s = pid_stats.get_mut(pid).expect("active pid must be in pid_stats");
                        s.memory_data.add_data_point(timestamp, memory_details);
                        memory_sample_count += 1;
                        if s.memory_time.is_none() || total_pss > s.memory_usage {
                            s.memory_usage = total_pss;
                            s.memory_time = Some(timestamp);
                        }
                    }
                    Err(e) => {
                        if e.to_string().contains("Process not found") {
                            if let Some(s) = pid_stats.get_mut(pid) {
                                s.active = false;
                                restart_count += 1;
                            }
                            println!("\n[{}] PID {} disappeared.", Local::now().format("%H:%M:%S"), pid.yellow());
                        } else {
                            println!("Error sampling memory for pid {}: {}", pid, e);
                        }
                    }
                }
            }

            // 定期生成内存图表：达到 5 个采样后，每 MEMORY_CHART_INTERVAL 个新采样生成一次
            if memory_sample_count >= 5
                && memory_sample_count - last_memory_chart_sample_count
                    >= MEMORY_CHART_INTERVAL
            {
                last_memory_chart_sample_count = memory_sample_count;
                if let Ok(timestamp_dir) = utils::create_timestamp_subdir(&args.package) {
                    let memory_dir = timestamp_dir.join("memory");
                    if !memory_dir.exists() {
                        if let Err(e) = std::fs::create_dir_all(&memory_dir) {
                            println!("Failed to create memory directory: {}", e);
                            continue;
                        }
                        println!("Created memory directory: {}", memory_dir.display());
                    }

                    // 为每个有数据的活跃 PID 生成内存图表
                    for (pid, s) in &pid_stats {
                        if s.memory_data.timestamps.len() < 2 {
                            continue;
                        }
                        let memory_charts =
                            generate_memory_charts(&memory_dir, &args.package, pid, &s.memory_data);
                        match memory_charts {
                            Ok(paths) => {
                                for path in paths {
                                    if path.to_string_lossy().ends_with(".png") {
                                        println!("✓ Scheduled Memory chart generated: {}", path.display());
                                    } else if path.to_string_lossy().ends_with(".csv") {
                                        println!("✓ Memory data exported to CSV: {}", path.display());
                                    }
                                }
                            }
                            Err(e) => println!("Failed to generate memory chart for pid {}: {}", pid, e),
                        }
                    }
                }
            }
        }

        // 检查当前是否为整小时，如果是则生成图表和CSV
        let now = Local::now();
        let current_hour = now.hour() as i32;

        // 如果进入了新的整小时且有足够的CPU数据，生成图表
        if current_hour != last_chart_hour && args.cpu {
            last_chart_hour = current_hour;
            let hour_mark = format!("{}:00", now.hour());
            println!(
                "{} Generating scheduled CPU chart at {}...",
                now.format("%H:%M:%S").to_string().blue(),
                hour_mark.green()
            );
            for (pid, s) in &pid_stats {
                if s.cpu_data.timestamps.len() <= 1 {
                    continue;
                }
                match utils::generate_cpu_chart(
                    &s.cpu_data.timestamps,
                    &s.cpu_data.process_cpu,
                    pid,
                ) {
                    Ok(chart_path) => {
                        println!("Scheduled CPU chart generated (pid {}): {}", pid.yellow(), chart_path.display());
                        let csv_path = chart_path.with_extension("csv");
                        if csv_path.exists() {
                            println!("Scheduled CPU data exported to CSV: {}", csv_path.display());
                        }
                    }
                    Err(e) => eprintln!("Error generating CPU chart for pid {}: {}", pid, e),
                }
            }
        }

        // 节拍：CPU 采样已在上面 sleep(interval)；
        // --memory 单跑（无 --cpu）时需自行维持采样间隔，否则空转狂采样。
        if !args.cpu {
            tokio::time::sleep(tokio::time::Duration::from_millis(args.interval)).await;
        }
    }

    // Wait for ADB monitor to finish
    let _ = adb_monitor.await;

    // 在结束前生成最终的线程时间序列图表（按 PID 分别生成）
    if args.thread && args.cpu && !thread_time_series.is_empty() {
        println!("Program ending, generating final thread time series chart...");
        if let Ok(timestamp_dir) = utils::create_timestamp_subdir(&args.package) {
            let thread_dir = timestamp_dir.join("thread");
            if !thread_dir.exists() {
                if let Err(e) = std::fs::create_dir_all(&thread_dir) {
                    println!("Failed to create thread directory: {}", e);
                    return Ok(());
                }
                println!("Created thread directory: {}", thread_dir.display());
            }

            for (pid, per_pid_series) in &thread_time_series {
                if per_pid_series.is_empty() {
                    continue;
                }
                let threads: Vec<ThreadCpuInfo> =
                    per_pid_series.values().flat_map(|v| v.iter().cloned()).collect();
                match utils::export_thread_data_to_csv(thread_dir.clone(), pid, &threads, false) {
                    Ok(filenames) => {
                        println!("✓ Final thread data (pid {}) exported to {} CSV files", pid, filenames.len());
                    }
                    Err(e) => println!("Failed to export final thread data for pid {}: {}", pid, e),
                }
                match utils::generate_thread_time_series_chart(thread_dir.clone(), &args.package, pid, per_pid_series) {
                    Ok(chart_filename) if !chart_filename.is_empty() => {
                        println!("✓ Final thread time series chart (pid {}) generated: {}", pid, chart_filename);
                    }
                    Ok(_) => {}
                    Err(e) => println!("Failed to generate thread chart for pid {}: {}", pid, e),
                }
            }
        }
    }

    // 创建时间戳目录
    let timestamp_dir = if let Ok(dir) = utils::create_timestamp_subdir(&args.package) {
        dir
    } else {
        println!("Warning: Could not create timestamp directory.");
        return Ok(());
    };

    // 程序结束时生成CPU图表（每 PID 一张 + 汇总）
    let cpu_dir = timestamp_dir.join("cpu");
    let mut cpu_series_for_summary: Vec<utils::CpuSeriesRef<'_>> = Vec::new();
    if args.cpu {
        let mut has_cpu_data = false;
        for s in pid_stats.values() {
            if s.cpu_data.timestamps.len() > 1 {
                has_cpu_data = true;
                break;
            }
        }
        if has_cpu_data {
            if !cpu_dir.exists() {
                if let Err(e) = std::fs::create_dir_all(&cpu_dir) {
                    println!("Failed to create CPU directory: {}", e);
                    return Ok(());
                }
                println!("Created CPU directory: {}", cpu_dir.display());
            }

            for (pid, s) in &pid_stats {
                if s.cpu_data.timestamps.len() <= 1 {
                    continue;
                }
                println!(
                    "Peak CPU Usage (pid {}): {} at {}",
                    pid.yellow(),
                    format!("{:.1}%", s.cpu_usage).red(),
                    s.cpu_time
                        .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
                        .unwrap_or_else(|| "N/A".to_string())
                );
                let chart_path = match utils::generate_cpu_chart(
                    &s.cpu_data.timestamps,
                    &s.cpu_data.process_cpu,
                    pid,
                ) {
                    Ok(path) => path,
                    Err(e) => {
                        println!("Failed to generate CPU chart for pid {}: {}", pid, e);
                        continue;
                    }
                };
                if let Some(filename) = chart_path.file_name() {
                    let target_path = cpu_dir.join(filename);
                    if let Err(e) = std::fs::copy(&chart_path, &target_path) {
                        println!("Failed to copy CPU chart for pid {}: {}", pid, e);
                    } else {
                        println!("✓ CPU chart generated (pid {}): {}", pid, target_path.display());
                    }
                }
                let csv_path = cpu_dir.join(format!("cpu_{}_data.csv", pid));
                match utils::export_cpu_data_to_csv(&csv_path, &s.cpu_data.timestamps, &s.cpu_data.process_cpu) {
                    Ok(_) => println!("✓ CPU data exported to CSV (pid {}): {}", pid, csv_path.display()),
                    Err(e) => println!("Failed to export CPU data for pid {}: {}", pid, e),
                }
                cpu_series_for_summary.push((pid.clone(), &s.cpu_data.timestamps, &s.cpu_data.process_cpu));
            }

            // 汇总 CPU 图表（多 PID 同图）
            if cpu_series_for_summary.len() >= 2 {
                match utils::generate_cpu_summary_chart(&cpu_dir, &args.package, &cpu_series_for_summary) {
                    Ok(p) => println!("✓ CPU summary chart generated: {}", p.display()),
                    Err(e) => println!("Failed to generate CPU summary chart: {}", e),
                }
            }
        }
    }

    // 程序结束时生成内存图表（每 PID 一张 + 汇总）
    if args.memory {
        let memory_dir = timestamp_dir.join("memory");
        let mut has_mem_data = false;
        for s in pid_stats.values() {
            if s.memory_data.timestamps.len() > 1 {
                has_mem_data = true;
                break;
            }
        }
        if has_mem_data {
            if !memory_dir.exists() {
                if let Err(e) = std::fs::create_dir_all(&memory_dir) {
                    println!("Failed to create memory directory: {}", e);
                    return Ok(());
                }
                println!("Created memory directory: {}", memory_dir.display());
            }
            let mut mem_series_for_summary: Vec<(String, &MemoryTimeSeriesData)> = Vec::new();
            for (pid, s) in &pid_stats {
                if s.memory_data.timestamps.len() <= 1 {
                    continue;
                }
                println!(
                    "Peak Memory Usage (pid {}): {} at {}",
                    pid.yellow(),
                    format!("{} KB", s.memory_usage).red(),
                    s.memory_time
                        .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
                        .unwrap_or_else(|| "N/A".to_string())
                );
                match generate_memory_charts(&memory_dir, &args.package, pid, &s.memory_data) {
                    Ok(paths) => {
                        for path in paths {
                            if path.to_string_lossy().ends_with(".png") {
                                println!("✓ Memory chart generated (pid {}): {}", pid, path.display());
                            } else if path.to_string_lossy().ends_with(".csv") {
                                println!("✓ Memory data exported to CSV (pid {}): {}", pid, path.display());
                            }
                        }
                    }
                    Err(e) => println!("Failed to generate memory charts for pid {}: {}", pid, e),
                }
                mem_series_for_summary.push((pid.clone(), &s.memory_data));
            }
            if mem_series_for_summary.len() >= 2 {
                match generate_memory_summary_chart(&memory_dir, &args.package, &mem_series_for_summary) {
                    Ok(p) => println!("✓ Memory summary chart generated: {}", p.display()),
                    Err(e) => println!("Failed to generate memory summary chart: {}", e),
                }
            }
        }
    }
    println!("Process Restarts: {}", restart_count.to_string().red());

    Ok(())
}

// 生成内存图表的函数
fn generate_memory_charts(
    output_dir: &Path,
    package: &str,
    pid: &str,
    memory_data: &MemoryTimeSeriesData,
) -> Result<Vec<PathBuf>> {
    use plotters::prelude::*;

    // 创建一个单一的内存图表文件。文件名带 pid 以区分多进程。
    let mut chart_paths = Vec::new();
    let file_name = format!("memory_{}_chart.png", pid);
    let path = output_dir.join(file_name);

    // 检查数据是否足够
    if memory_data.timestamps.is_empty() || memory_data.memory_details.is_empty() {
        return Err(anyhow::format_err!("No memory data to chart"));
    }

    // 创建图表
    let root = BitMapBackend::new(&path, (1920, 1080)).into_drawing_area();
    root.fill(&WHITE)?;

    // 创建图表标题
    let title = format!("Memory Usage - {}", package);

    // 分割绘图区域为标题、图表和图例
    let (title_area, rest_area) = root.split_vertically(50);

    // 绘制标题
    title_area.titled(&title, ("sans-serif", 20))?;

    // 查找最大内存使用量以设置Y轴范围
    let mut max_memory = 0.1f32;
    for detail in &memory_data.memory_details {
        max_memory = max_memory.max(detail.total_pss as f32);
        max_memory = max_memory.max(detail.java_heap as f32);
        max_memory = max_memory.max(detail.native_heap as f32);
        max_memory = max_memory.max(detail.code as f32);
        max_memory = max_memory.max(detail.stack as f32);
        max_memory = max_memory.max(detail.graphics as f32);
        max_memory = max_memory.max(detail.private_other as f32);
        max_memory = max_memory.max(detail.system as f32);
    }

    // 添加一些填充到最大内存使用量
    max_memory *= 1.1;

    // 获取时间范围
    let min_time = *memory_data.timestamps.front().unwrap();
    let max_time = *memory_data.timestamps.back().unwrap();

    // 定义内存类型和对应的名称
    let memory_types = [
        "Total PSS",
        "Java Heap",
        "Native Heap",
        "Code",
        "Stack",
        "Graphics",
        "Private Other",
        "System",
    ];

    // 定义颜色
    let colors = [
        &RED,
        &BLUE,
        &GREEN,
        &YELLOW,
        &MAGENTA,
        &CYAN,
        &RGBColor(128, 0, 0),
        &RGBColor(0, 128, 0),
    ];

    // 创建图表上下文
    let mut chart = ChartBuilder::on(&rest_area)
        .margin(10)
        .margin_right(35) // 增加右侧边距为图例留出空间
        .x_label_area_size(40)
        .y_label_area_size(60)
        .build_cartesian_2d(min_time..max_time, 0f32..max_memory)?;

    // 配置网格
    chart
        .configure_mesh()
        .x_labels(8)
        .x_label_formatter(&|x| x.format("%H:%M:%S").to_string())
        .y_desc("Memory Usage (KB)")
        .x_desc("Time")
        .draw()?;

    // 为每种内存类型绘制数据线
    for (i, &memory_type) in memory_types.iter().enumerate() {
        let color = colors[i];

        // 根据内存类型获取对应的数据
        let values: Vec<(DateTime<Local>, f32)> = memory_data
            .timestamps
            .iter()
            .zip(memory_data.memory_details.iter())
            .map(|(t, d)| {
                let value = match i {
                    0 => d.total_pss as f32,
                    1 => d.java_heap as f32,
                    2 => d.native_heap as f32,
                    3 => d.code as f32,
                    4 => d.stack as f32,
                    5 => d.graphics as f32,
                    6 => d.private_other as f32,
                    7 => d.system as f32,
                    _ => 0.0,
                };
                (t.to_owned(), value)
            })
            .collect();

        // 绘制数据线
        chart
            .draw_series(LineSeries::new(values, *color))?
            .label(memory_type.to_string())
            .legend(move |(x, y)| PathElement::new(vec![(x, y), (x + 20, y)], color));
    }

    // 添加图例配置
    chart
        .configure_series_labels()
        .background_style(WHITE.mix(0.8))
        .border_style(BLACK)
        .position(SeriesLabelPosition::UpperRight)
        .margin(10)
        .legend_area_size(35) // 增加图例区域大小
        .label_font(("sans-serif", 15)) // 增加字体大小
        .draw()?;

    // 保存图表
    root.present()?;

    chart_paths.push(path.clone());
    // 移除输出，由调用者处理输出
    // println!("✓ Memory chart generated: {}", path.display());

    // 导出内存数据到CSV
    let csv_path = output_dir.join(format!("memory_{}_data.csv", pid));
    if let Ok(file) = std::fs::File::create(&csv_path) {
        let mut writer = std::io::BufWriter::new(file);

        // 写入CSV头
        writeln!(
            &mut writer,
            "Timestamp,Total PSS,Java Heap,Native Heap,Code,Stack,Graphics,Private Other,System"
        )?;

        // 写入每个数据点
        for i in 0..memory_data.timestamps.len() {
            let timestamp = &memory_data.timestamps[i];
            let details = &memory_data.memory_details[i];

            writeln!(
                &mut writer,
                "{},{},{},{},{},{},{},{},{}",
                timestamp.format("%Y-%m-%d %H:%M:%S"),
                details.total_pss,
                details.java_heap,
                details.native_heap,
                details.code,
                details.stack,
                details.graphics,
                details.private_other,
                details.system
            )?;
        }

        // 添加CSV文件路径到返回结果
        chart_paths.push(csv_path.clone());
        // 移除输出，由调用者处理输出
        // println!("✓ Memory data exported to CSV: {}", csv_path.display());
    }

    Ok(chart_paths)
}

/// 生成多 PID 内存汇总对比图：每个 PID 的 Total PSS 一条折线。
fn generate_memory_summary_chart(
    output_dir: &Path,
    package: &str,
    series: &[(String, &MemoryTimeSeriesData)],
) -> Result<PathBuf> {
    use plotters::prelude::*;

    let path = output_dir.join("memory_summary.png");
    let path_clone = path.clone();
    let root = BitMapBackend::new(&path, (1920, 1080)).into_drawing_area();
    root.fill(&WHITE)?;

    let (title_area, rest_area) = root.split_vertically(50);
    title_area.titled(
        &format!("Memory Summary - {} ({} PIDs)", package, series.len()),
        ("sans-serif", 20),
    )?;

    // 跨所有 PID 计算 X/Y 范围（仅用 Total PSS）
    let mut min_time = None;
    let mut max_time = None;
    let mut max_mem = 1.0f32;
    for (_, md) in series {
        if let (Some(&t0), Some(&tn)) = (md.timestamps.front(), md.timestamps.back()) {
            min_time = Some(min_time.map_or(t0, |m: DateTime<Local>| m.min(t0)));
            max_time = Some(max_time.map_or(tn, |m: DateTime<Local>| m.max(tn)));
        }
        for d in &md.memory_details {
            if d.total_pss as f32 > max_mem {
                max_mem = d.total_pss as f32;
            }
        }
    }
    let min_time = min_time.ok_or_else(|| anyhow::format_err!("No memory data for summary"))?;
    let max_time = max_time.unwrap_or(min_time);
    max_mem *= 1.1;

    let mut chart = ChartBuilder::on(&rest_area)
        .margin(15)
        .x_label_area_size(40)
        .y_label_area_size(60)
        .build_cartesian_2d(min_time..max_time, 0f32..max_mem)?;

    chart.configure_mesh()
        .y_desc("Total PSS (KB)")
        .x_desc("Time")
        .x_labels(8)
        .x_label_formatter(&|x| x.format("%H:%M:%S").to_string())
        .draw()?;

    let colors = [
        RGBColor(31, 119, 180), RGBColor(255, 127, 14), RGBColor(44, 160, 44),
        RGBColor(214, 39, 40), RGBColor(148, 103, 189), RGBColor(140, 86, 75),
        RGBColor(227, 119, 194), RGBColor(127, 127, 127), RGBColor(188, 189, 34),
        RGBColor(23, 190, 207),
    ];

    for (i, (pid, md)) in series.iter().enumerate() {
        let color = colors[i % colors.len()];
        let pts: Vec<(DateTime<Local>, f32)> = md.timestamps.iter()
            .zip(md.memory_details.iter())
            .map(|(t, d)| (*t, d.total_pss as f32))
            .collect();
        chart.draw_series(LineSeries::new(pts, color.stroke_width(2)))?
            .label(format!("PID {}", pid))
            .legend(move |(x, y)| PathElement::new(vec![(x, y), (x + 20, y)], color.stroke_width(2)));
    }
    chart.configure_series_labels()
        .background_style(WHITE.mix(0.8))
        .border_style(BLACK)
        .position(SeriesLabelPosition::UpperRight)
        .draw()?;

    root.present()?;
    Ok(path_clone)
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
