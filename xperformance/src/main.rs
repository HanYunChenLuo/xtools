#![deny(warnings)]
use anyhow::{Context, Result};
use chrono::{DateTime, Local, Timelike};
use clap::Parser;
use colored::*;
use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;
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
    memory_data: MemoryTimeSeriesData,
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
                format!("{} KB", self.memory_usage).red(),
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
                    if peak_stats.cpu_data.timestamps.back().is_some()
                        && peak_stats.cpu_data.top_threads.back().is_some()
                    {
                        println!("Thread data collection available");
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
            if let Ok((cpu_usage, timestamp, top_threads)) = cpu::sample_cpu(&args.package).await {
                if cpu_usage > peak_stats.cpu_usage {
                    peak_stats.cpu_usage = cpu_usage;
                    peak_stats.cpu_time = timestamp;
                }
                peak_stats
                    .cpu_data
                    .add_data_point(timestamp, cpu_usage, top_threads.clone());

                // 将线程数据添加到时间序列跟踪
                if args.thread {
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
            if let Ok((memory_kb, timestamp, memory_details)) =
                memory::sample_memory(&args.package, args.verbose).await
            {
                if memory_kb > peak_stats.memory_usage {
                    peak_stats.memory_usage = memory_kb;
                    peak_stats.memory_time = timestamp;
                }

                // 添加内存数据点到时间序列
                peak_stats
                    .memory_data
                    .add_data_point(timestamp, memory_details);

                // 如果开启了详细模式并且已收集了足够的数据点，生成内存图表
                if args.verbose && peak_stats.memory_data.timestamps.len() >= 5 {
                    if let Ok(timestamp_dir) = utils::create_timestamp_subdir(&args.package) {
                        // 创建memory子目录
                        let memory_dir = timestamp_dir.join("memory");
                        if !memory_dir.exists() {
                            if let Err(e) = std::fs::create_dir_all(&memory_dir) {
                                println!("Failed to create memory directory: {}", e);
                                continue;
                            }
                            println!("Created memory directory: {}", memory_dir.display());
                        }

                        // 生成内存图表
                        let memory_charts = generate_memory_charts(
                            &memory_dir,
                            &args.package,
                            &peak_stats.memory_data,
                        );
                        if let Ok(chart_paths) = memory_charts {
                            for path in chart_paths {
                                if path.to_string_lossy().ends_with(".png") {
                                    println!("✓ Memory chart generated: {}", path.display());
                                } else if path.to_string_lossy().ends_with(".csv") {
                                    println!("✓ Memory data exported to CSV: {}", path.display());
                                }
                            }
                        } else {
                            println!("Failed to generate memory charts");
                        }
                    }
                }
            }
        }
    }

    // Wait for ADB monitor to finish
    let _ = adb_monitor.await;

    // 在结束前生成最终的线程时间序列图表
    if args.thread && args.cpu && !thread_time_series.is_empty() {
        println!("Program ending, generating final thread time series chart...");
        if let Ok(timestamp_dir) = utils::create_timestamp_subdir(&args.package) {
            // 创建thread子目录
            let thread_dir = timestamp_dir.join("thread");
            if !thread_dir.exists() {
                if let Err(e) = std::fs::create_dir_all(&thread_dir) {
                    println!("Failed to create thread directory: {}", e);
                    return Ok(());
                }
                println!("Created thread directory: {}", thread_dir.display());
            }

            // 导出最终的线程数据
            match utils::export_thread_data_to_csv(
                thread_dir.clone(),
                &last_process_info.pid,
                &thread_time_series
                    .values()
                    .flat_map(|v| v.iter().cloned())
                    .collect::<Vec<_>>(),
                false,
            ) {
                Ok(filenames) => {
                    println!(
                        "✓ Final thread data exported to {} CSV files",
                        filenames.len()
                    );
                }
                Err(e) => {
                    println!("Failed to export final thread data to CSV: {}", e);
                }
            }

            // 生成最终的线程时间序列图表
            match utils::generate_thread_time_series_chart(
                thread_dir,
                &args.package,
                &last_process_info.pid,
                &thread_time_series,
            ) {
                Ok(chart_filename) => {
                    if !chart_filename.is_empty() {
                        println!(
                            "✓ Final thread time series chart generated: {}",
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

    // 创建时间戳目录
    let timestamp_dir = if let Ok(dir) = utils::create_timestamp_subdir(&args.package) {
        dir
    } else {
        println!("Warning: Could not create timestamp directory.");
        return Ok(());
    };

    // 程序结束时生成CPU图表
    if args.cpu && peak_stats.cpu_data.timestamps.len() > 1 {
        // 创建CPU子目录
        let cpu_dir = timestamp_dir.join("cpu");
        if !cpu_dir.exists() {
            if let Err(e) = std::fs::create_dir_all(&cpu_dir) {
                println!("Failed to create CPU directory: {}", e);
                return Ok(());
            }
            println!("Created CPU directory: {}", cpu_dir.display());
        }

        println!(
            "Peak CPU Usage: {} at {}",
            format!("{:.1}%", peak_stats.cpu_usage).red(),
            peak_stats.cpu_time.format("%Y-%m-%d %H:%M:%S")
        );

        // 生成CPU图表
        let chart_path = match utils::generate_cpu_chart(
            &args.package,
            &peak_stats.cpu_data.timestamps,
            &peak_stats.cpu_data.process_cpu,
            &last_process_info.pid,
        ) {
            Ok(path) => path,
            Err(e) => {
                println!("Failed to generate CPU chart: {}", e);
                return Ok(());
            }
        };

        // 复制CPU图表到输出目录
        let target_path = cpu_dir.join(chart_path.file_name().unwrap());
        if let Err(e) = std::fs::copy(&chart_path, &target_path) {
            println!("Failed to copy CPU chart to output directory: {}", e);
        } else {
            println!("✓ CPU chart generated: {}", target_path.display());
        }

        // 导出CPU数据到CSV
        let csv_path = cpu_dir.join(format!("{}_cpu_data.csv", args.package));
        if let Ok(_) = utils::export_cpu_data_to_csv(
            &csv_path,
            &peak_stats.cpu_data.timestamps,
            &peak_stats.cpu_data.process_cpu,
        ) {
            println!("✓ CPU data exported to CSV: {}", csv_path.display());
        }
    }

    if args.memory {
        println!(
            "Peak Memory Usage: {} at {}",
            format!("{} KB", peak_stats.memory_usage).red(),
            peak_stats.memory_time.format("%Y-%m-%d %H:%M:%S")
        );

        // 如果收集了足够的内存数据点，生成内存图表
        if peak_stats.memory_data.timestamps.len() > 1 {
            // 在时间戳目录下创建memory子目录
            let memory_dir = timestamp_dir.join("memory");
            if !memory_dir.exists() {
                if let Err(e) = std::fs::create_dir_all(&memory_dir) {
                    println!("Failed to create memory directory: {}", e);
                    return Ok(());
                }
                println!("Created memory directory: {}", memory_dir.display());
            }

            // 生成内存图表
            let memory_charts =
                generate_memory_charts(&memory_dir, &args.package, &peak_stats.memory_data);
            if let Ok(chart_paths) = memory_charts {
                for path in chart_paths {
                    if path.to_string_lossy().ends_with(".png") {
                        println!("✓ Memory chart generated: {}", path.display());
                    } else if path.to_string_lossy().ends_with(".csv") {
                        println!("✓ Memory data exported to CSV: {}", path.display());
                    }
                }
            } else {
                println!("Failed to generate memory charts");
            }
        }
    }
    println!(
        "Process Restarts: {}",
        peak_stats.restart_count.to_string().red()
    );

    Ok(())
}

// 生成内存图表的函数
fn generate_memory_charts(
    output_dir: &PathBuf,
    package: &str,
    memory_data: &MemoryTimeSeriesData,
) -> Result<Vec<PathBuf>> {
    use plotters::prelude::*;

    // 创建一个单一的内存图表文件
    let mut chart_paths = Vec::new();
    let file_name = format!("{}_memory_chart.png", package);
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
    max_memory = max_memory * 1.1;

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
            .draw_series(LineSeries::new(values, color.clone()))?
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
    let csv_path = output_dir.join(format!("{}_memory_data.csv", package));
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

// 保留原始的单个内存指标图表函数，但它不会被直接调用
#[allow(dead_code)]
fn generate_single_memory_chart(
    output_dir: &PathBuf,
    package: &str,
    metric_name: &str,
    timestamps: &VecDeque<DateTime<Local>>,
    values: &Vec<f32>,
) -> Result<PathBuf> {
    use plotters::prelude::*;

    // 创建文件名，用下划线替换空格
    let file_name = format!("{}_{}.png", package, metric_name.replace(" ", "_"));
    let path = output_dir.join(file_name);
    let path_copy = path.clone();

    // 创建图表
    let root = BitMapBackend::new(&path, (1920, 1080)).into_drawing_area();
    root.fill(&WHITE)?;

    // 找到最大值
    let max_value = values.iter().fold(0.0f32, |a, &b| a.max(b)) * 1.1;

    // 获取开始和结束时间
    let first_timestamp = timestamps.front().unwrap();
    let last_timestamp = timestamps.back().unwrap();

    // 定义图表区域
    let mut chart = ChartBuilder::on(&root)
        .caption(
            format!("{} - {}", package, metric_name),
            ("sans-serif", 22).into_font(),
        )
        .margin(10)
        .x_label_area_size(40)
        .y_label_area_size(60)
        .build_cartesian_2d(
            first_timestamp.to_owned()..last_timestamp.to_owned(),
            0.0..max_value,
        )?;

    // 配置网格和标签
    chart
        .configure_mesh()
        .x_labels(10)
        .x_label_formatter(&|x| x.format("%H:%M:%S").to_string())
        .y_desc(format!("{} (KB)", metric_name))
        .draw()?;

    // 绘制折线
    chart.draw_series(LineSeries::new(
        timestamps
            .iter()
            .zip(values.iter())
            .map(|(t, &v)| (t.to_owned(), v)),
        &RED,
    ))?;

    // 保存图表
    root.present()?;

    Ok(path_copy)
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
