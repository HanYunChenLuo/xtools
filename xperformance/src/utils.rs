use crate::cpu::ThreadCpuInfo;
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Local};
use colored::*;
use plotters::prelude::*;
use plotters::style::text_anchor::{HPos, Pos, VPos};
use plotters::style::Color;
use plotters::style::RGBColor;
use std::cmp::Ordering;
use std::collections::VecDeque;
use std::fs::File;
use std::fs::{self, OpenOptions};
use std::io::BufWriter;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::str;
use std::sync::Once;

// 定义我们自己的ORANGE颜色常量
const ORANGE: RGBColor = RGBColor(255, 165, 0);

static INIT_LOG: Once = Once::new();
static mut LOG_FILE_PATH: Option<PathBuf> = None;

pub struct ProcessInfo {
    pub pid: String,
    pub start_time: String,
}

pub fn check_adb_connection() -> bool {
    if let Ok(output) = Command::new("adb").arg("devices").output() {
        if output.status.success() {
            let devices = String::from_utf8_lossy(&output.stdout);
            return devices.lines().skip(1).any(|line| !line.trim().is_empty());
        }
    }
    false
}

pub fn get_process_info(package: &str) -> Result<ProcessInfo> {
    let pid = {
        let output = run_adb_command(&["shell", "pidof", package])?;
        let pid = output.trim();
        if pid.is_empty() {
            anyhow::bail!("Process not found for package: {}", package);
        }
        pid.to_string()
    };

    let start_time = {
        let output = run_adb_command(&[
            "shell",
            "stat",
            "-c",
            "%y",
            format!("/proc/{}/cmdline", pid).as_str(),
        ])?;
        output.trim().to_string()
    };

    Ok(ProcessInfo { pid, start_time })
}

pub fn run_adb_command(args: &[&str]) -> Result<String> {
    let output = Command::new("adb")
        .args(args)
        .env("TERM", "dumb")
        .output()
        .context("Failed to execute adb command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("ADB command failed: {}", stderr);
    }

    let raw_output = String::from_utf8_lossy(&output.stdout).to_string();
    Ok(clean_control_chars(&raw_output))
}

pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

fn clean_control_chars(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\x1B' {
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&next) = chars.peek() {
                    if next.is_ascii_alphabetic() {
                        chars.next();
                        break;
                    }
                    chars.next();
                }
                continue;
            }
        }
        result.push(c);
    }
    result
}

pub fn ensure_log_dir(package: &str) -> Result<PathBuf> {
    let log_dir = PathBuf::from("log").join(package);
    Ok(log_dir)
}

pub fn create_log_dir_if_needed(package: &str) -> Result<PathBuf> {
    let log_dir = PathBuf::from("log").join(package);
    if !log_dir.exists() {
        fs::create_dir_all(&log_dir)?;
        println!("Created log directory: {}", log_dir.display());

        // Try to log the directory creation if logging is already initialized
        // This may fail if this is the first call to ensure_log_dir
        let _ = append_to_log(&format!("Created log directory: {}", log_dir.display()));
    }
    Ok(log_dir)
}

pub fn init_logging(package: &str, cpu: bool, memory: bool) -> Result<PathBuf> {
    let mut result = None;
    INIT_LOG.call_once(|| {
        if let Ok(log_dir) = ensure_log_dir(package) {
            let timestamp = Local::now().format("%Y%m%d_%H%M%S");
            let metrics = match (cpu, memory) {
                (true, true) => "cpu_memory",
                (true, false) => "cpu",
                (false, true) => "memory",
                (false, false) => "none",
            };
            let filename = format!("performance_{}_{}.log", metrics, timestamp);
            let path = log_dir.join(filename);
            unsafe {
                LOG_FILE_PATH = Some(path.clone());
            }
            println!("Created log file: {}", path.display());
            result = Some(path.clone());

            // Initialize the log file with a header
            if let Ok(mut file) = OpenOptions::new().create(true).write(true).open(&path) {
                let header = format!(
                    "Performance monitoring started at {}\nPackage: {}\nMetrics: {}\n",
                    timestamp, package, metrics
                );
                let _ = writeln!(file, "{}", header);
                let _ = file.flush();
            }
        }
    });

    if let Some(path) = result {
        Ok(path)
    } else {
        unsafe {
            if let Some(ref path) = LOG_FILE_PATH {
                Ok(path.clone())
            } else {
                anyhow::bail!("Failed to initialize log file")
            }
        }
    }
}

pub fn append_to_log(content: &str) -> Result<()> {
    let path = unsafe {
        if let Some(ref path) = LOG_FILE_PATH {
            path
        } else {
            anyhow::bail!("Log file not initialized")
        }
    };

    let mut file = OpenOptions::new().create(true).append(true).open(path)?;

    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");
    writeln!(file, "\n[{}]", timestamp)?;
    writeln!(file, "{}", content)?;
    file.flush()?;

    Ok(())
}

pub fn generate_cpu_chart(
    package: &str,
    timestamps: &VecDeque<DateTime<Local>>,
    process_cpu: &VecDeque<f32>,
    system_cpu: &VecDeque<f32>,
    idle_cpu: &VecDeque<f32>,
    pid: &str,
) -> Result<PathBuf> {
    // Ensure we have data
    if timestamps.is_empty() || process_cpu.is_empty() {
        anyhow::bail!("No CPU data available to generate chart");
    }

    // Create timestamp-based subdirectory
    let timestamp_dir = create_timestamp_subdir(package)?;
    let timestamp = Local::now().format("%Y%m%d_%H%M%S");

    // Export CSV data to timestamp subdirectory, filename includes PID
    let csv_filename = format!("cpu_data_{}_pid{}.csv", timestamp, pid);
    let csv_path = timestamp_dir.join(&csv_filename);
    export_cpu_data_to_csv(&csv_path, timestamps, process_cpu, system_cpu, idle_cpu)?;
    let csv_message = format!("CPU data exported to CSV: {}", csv_path.display());
    println!("{}", csv_message);
    // Log CSV creation
    let _ = append_to_log(&csv_message);

    // Chart-related code, also stored in timestamp subdirectory
    let filename = format!("cpu_chart_{}_pid{}.png", timestamp, pid);
    let chart_path = timestamp_dir.join(filename);

    // Clone chart_path for later use
    let chart_path_display = chart_path.clone();

    // Set up chart
    let root = BitMapBackend::new(&chart_path, (1024, 900)).into_drawing_area();
    root.fill(&WHITE)?;

    // 为标题预留空间
    let (title_area, chart_area) = root.split_vertically(60);

    // 在标题区域绘制标题
    let title_style = TextStyle::from(("sans-serif", 30).into_font())
        .color(&BLACK)
        .pos(Pos::new(HPos::Center, VPos::Center));

    // 获取区域中心点坐标
    let center = (
        title_area.dim_in_pixel().0 as i32 / 2,
        title_area.dim_in_pixel().1 as i32 / 2,
    );

    // 绘制居中标题
    title_area.draw_text(&format!("CPU Usage for {}", package), &title_style, center)?;

    // 将绘图区域分成三份
    let areas = chart_area.split_evenly((3, 1));

    // 设置共享X轴范围 - 所有图表使用相同的时间范围
    let x_range = timestamps.front().unwrap().clone()..timestamps.back().unwrap().clone();

    // 计算系统和空闲CPU的最大值以设置合适的Y轴范围
    let max_system_cpu = system_cpu.iter().cloned().fold(0.0, f32::max) * 1.1; // 增加10%的余量
    let max_system_cpu = f32::max(max_system_cpu, 100.0); // 最小保持100%的量程

    let max_idle_cpu = idle_cpu.iter().cloned().fold(0.0, f32::max) * 1.1; // 增加10%的余量
    let max_idle_cpu = f32::max(max_idle_cpu, 100.0); // 最小保持100%的量程

    // 创建三个子图表
    // 1. Process CPU (Top)
    let mut process_chart = ChartBuilder::on(&areas[0])
        .margin(15)
        .x_label_area_size(0) // 顶部图表不显示X轴标签
        .y_label_area_size(60)
        .build_cartesian_2d(x_range.clone(), 0f32..100f32)?;

    process_chart
        .configure_mesh()
        .y_desc("Process CPU")
        .y_label_formatter(&|v| format!("{:.1}", v))
        .disable_x_mesh() // 不显示X轴网格线
        .draw()?;

    // 2. System CPU (Middle)
    let mut system_chart = ChartBuilder::on(&areas[1])
        .margin(15)
        .x_label_area_size(0) // 中间图表不显示X轴标签
        .y_label_area_size(60)
        .build_cartesian_2d(x_range.clone(), 0f32..max_system_cpu)?;

    system_chart
        .configure_mesh()
        .y_desc("System CPU")
        .y_label_formatter(&|v| format!("{:.1}", v))
        .disable_x_mesh() // 不显示X轴网格线
        .draw()?;

    // 3. Idle CPU (Bottom)
    let mut idle_chart = ChartBuilder::on(&areas[2])
        .margin(15)
        .x_label_area_size(40) // 底部图表显示X轴标签
        .y_label_area_size(60)
        .build_cartesian_2d(x_range, 0f32..max_idle_cpu)?;

    idle_chart
        .configure_mesh()
        .y_desc("Idle CPU")
        .y_label_formatter(&|v| format!("{:.1}", v))
        .x_desc("Time")
        .x_labels(10)
        .x_label_formatter(&|x| x.format("%H:%M:%S").to_string())
        .draw()?;

    // 转换数据为可绘制格式
    let process_data: Vec<(DateTime<Local>, f32)> = timestamps
        .iter()
        .zip(process_cpu.iter())
        .map(|(t, &cpu)| (t.clone(), cpu))
        .collect();

    let system_data: Vec<(DateTime<Local>, f32)> = timestamps
        .iter()
        .zip(system_cpu.iter())
        .map(|(t, &cpu)| (t.clone(), cpu))
        .collect();

    let idle_data: Vec<(DateTime<Local>, f32)> = timestamps
        .iter()
        .zip(idle_cpu.iter())
        .map(|(t, &cpu)| (t.clone(), cpu))
        .collect();

    // 绘制每个图表的线
    process_chart.draw_series(LineSeries::new(process_data, &BLUE))?;
    system_chart.draw_series(LineSeries::new(system_data, &RED))?;
    idle_chart.draw_series(LineSeries::new(idle_data, &GREEN))?;

    root.present()?;

    let chart_message = format!("CPU chart saved to: {}", chart_path_display.display());
    println!("{}", chart_message);
    // Log chart creation
    let _ = append_to_log(&chart_message);

    Ok(chart_path_display)
}

// 添加一个新函数用于导出CSV数据
pub fn export_cpu_data_to_csv(
    path: &PathBuf,
    timestamps: &VecDeque<DateTime<Local>>,
    process_cpu: &VecDeque<f32>,
    system_cpu: &VecDeque<f32>,
    idle_cpu: &VecDeque<f32>,
) -> Result<()> {
    let mut file = fs::File::create(path)?;

    // 写入CSV头
    writeln!(
        file,
        "Timestamp,Process CPU (%),System CPU (%),Idle CPU (%)"
    )?;

    // 写入数据行
    for i in 0..timestamps.len() {
        writeln!(
            file,
            "{},{:.2},{:.2},{:.2}",
            timestamps[i].format("%Y-%m-%d %H:%M:%S"),
            process_cpu[i],
            system_cpu[i],
            idle_cpu[i]
        )?;
    }

    file.flush()?;
    Ok(())
}

// 添加一个新函数用于导出占用CPU最高的线程信息到CSV
pub fn export_top_threads_to_csv(
    path: PathBuf,
    timestamp: &str,
    pid: &str,
    top_threads: &[ThreadCpuInfo],
) -> Result<String> {
    // 筛选出CPU使用率大于0的线程
    let active_threads: Vec<&ThreadCpuInfo> = top_threads
        .iter()
        .filter(|thread| thread.cpu_usage > 0.0)
        .collect();

    // 如果没有活跃线程，不创建文件
    if active_threads.is_empty() {
        println!("No active threads found with CPU usage > 0%, skipping file creation");
        return Ok(String::new());
    }

    // Use the provided path directly (which should be a timestamp subdirectory)
    let filename = format!("top_threads_{}_pid{}.csv", timestamp, pid);
    let filepath = path.join(&filename);

    let mut file = fs::File::create(&filepath)?;

    // 写入CSV头部
    writeln!(file, "ThreadID,CPUUsage,ThreadName")?;

    // 写入每个活跃线程数据
    for thread in active_threads {
        writeln!(
            file,
            "{},{:.2},{}",
            thread.tid, thread.cpu_usage, thread.name
        )?;
    }

    file.flush()?;
    let message = format!("Top threads exported to CSV: {}", filepath.display());
    println!("{}", message);

    // Log file creation
    let _ = append_to_log(&message);

    Ok(filename)
}

// 修改生成线程图表的函数
pub fn generate_top_threads_chart(
    path: PathBuf,
    timestamp: &str,
    package: &str,
    pid: &str,
    top_threads: &[ThreadCpuInfo],
) -> Result<String> {
    // If no threads at all, don't create any files
    if top_threads.is_empty() {
        println!("WARNING: Empty thread list, skipping chart generation");
        return Ok(String::new());
    }

    // Filter for threads with non-zero CPU usage
    let active_threads: Vec<&ThreadCpuInfo> = top_threads
        .iter()
        .filter(|thread| thread.cpu_usage > 0.0)
        .collect();

    // If no active threads, don't create any files
    if active_threads.is_empty() {
        println!("WARNING: No threads with CPU > 0 found, skipping chart generation");
        return Ok(String::new());
    }

    // Continue with normal chart generation for active threads
    println!(
        "Generating chart with {} active threads",
        active_threads.len()
    );

    let chart_filename = format!("top_threads_{}_pid{}.png", timestamp, pid);
    let filepath = path.join(&chart_filename);

    // Sort threads by CPU usage (highest first)
    let mut sorted_threads: Vec<ThreadCpuInfo> = active_threads.into_iter().cloned().collect();
    sorted_threads.sort_by(|a, b| {
        b.cpu_usage
            .partial_cmp(&a.cpu_usage)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Limit to top 10 threads for readability
    let threads_to_display = if sorted_threads.len() > 10 {
        sorted_threads[0..10].to_vec()
    } else {
        sorted_threads
    };

    // Create canvas - adjust size for horizontal bar chart
    let root = BitMapBackend::new(&filepath, (1000, 600)).into_drawing_area();
    root.fill(&WHITE)?;

    // Split canvas area
    let (title_area, chart_area) = root.split_vertically(60);

    // Add title
    title_area.titled(
        &format!("Thread CPU Usage - {} (PID: {})", package, pid),
        ("sans-serif", 20),
    )?;

    // Prepare data
    let max_cpu = threads_to_display
        .iter()
        .map(|t| t.cpu_usage)
        .fold(0.0f32, f32::max)
        .max(1.0); // Ensure minimum value is 1.0

    // Create a chart with 3 rows: process CPU, system CPU, and thread CPU
    let mut chart = ChartBuilder::on(&chart_area)
        .margin(5)
        .x_label_area_size(40)
        .y_label_area_size(60)
        .build_cartesian_2d(0.0f32..threads_to_display.len() as f32, 0.0f32..max_cpu)?;

    // Configure the mesh
    chart
        .configure_mesh()
        .x_labels(threads_to_display.len())
        .x_label_formatter(&|x| {
            let idx = *x as usize;
            if idx < threads_to_display.len() {
                format!(
                    "{} ({})",
                    threads_to_display[idx].name, threads_to_display[idx].tid
                )
            } else {
                "".to_string()
            }
        })
        .x_desc("Thread")
        .y_desc("CPU Usage (%)")
        .draw()?;

    // Draw a line series for each thread
    let mut legend_entries = Vec::new();

    for (idx, thread) in threads_to_display.iter().enumerate() {
        // Use thread name and tid for legend
        let legend_name = format!("{} ({})", thread.name, thread.tid);
        legend_entries.push((legend_name, thread.cpu_usage));

        // Plot the data for this thread
        chart.draw_series(LineSeries::new(
            vec![(idx as f32, 0.0), (idx as f32, thread.cpu_usage)],
            &cpu_usage_to_color(thread.cpu_usage),
        ))?;
    }

    // Add a legend
    if !legend_entries.is_empty() {
        chart
            .configure_series_labels()
            .background_style(&WHITE.mix(0.8))
            .border_style(&BLACK)
            .draw()?;
    }

    // Present the chart
    root.present()?;
    let message = format!("Thread time series chart saved to: {}", filepath.display());
    println!("{}", message);
    // Log chart creation
    let _ = append_to_log(&message);

    Ok(chart_filename)
}

// Add this helper function to convert CPU usage to a color
fn cpu_usage_to_color(cpu_usage: f32) -> RGBColor {
    // Color gradient from green (low CPU) to red (high CPU)
    if cpu_usage < 10.0 {
        // Green for low usage
        RGBColor(0, 255, 0)
    } else if cpu_usage < 30.0 {
        // Yellow-green
        RGBColor(128, 255, 0)
    } else if cpu_usage < 50.0 {
        // Yellow
        RGBColor(255, 255, 0)
    } else if cpu_usage < 70.0 {
        // Orange
        RGBColor(255, 165, 0)
    } else {
        // Red for high usage
        RGBColor(255, 0, 0)
    }
}

// Function to create timestamp subdirectory within the log directory
pub fn create_timestamp_subdir(package: &str) -> Result<PathBuf> {
    let log_dir = create_log_dir_if_needed(package)?;
    let timestamp_str = Local::now().format("%Y%m%d_%H%M%S").to_string();
    let timestamp_dir = log_dir.join(&timestamp_str);

    if !timestamp_dir.exists() {
        std::fs::create_dir_all(&timestamp_dir)?;
        let msg = format!("Created timestamp directory: {}", timestamp_dir.display());
        println!("{}", msg);

        // Log directory creation
        let _ = append_to_log(&msg);
    }

    Ok(timestamp_dir)
}

// Function to export thread data to individual CSV files by thread ID
pub fn export_thread_data_to_csv(
    path: PathBuf,
    pid: &str,
    threads: &[ThreadCpuInfo],
    append: bool,
) -> Result<Vec<String>> {
    let mut created_files = Vec::new();

    // Filter out threads with zero CPU usage
    let active_threads: Vec<&ThreadCpuInfo> = threads
        .iter()
        .filter(|thread| thread.cpu_usage > 0.0)
        .collect();

    if active_threads.is_empty() {
        println!("No threads with CPU usage > 0 found, skipping thread data export");
        return Ok(created_files);
    }

    // Group threads by TID
    let mut thread_map: std::collections::HashMap<String, Vec<&ThreadCpuInfo>> =
        std::collections::HashMap::new();
    for thread in active_threads {
        if let Some(_timestamp) = thread.timestamp {
            thread_map
                .entry(thread.tid.clone())
                .or_insert_with(Vec::new)
                .push(thread);
        }
    }

    // Create/update a CSV file for each thread
    for (tid, thread_data) in thread_map {
        if thread_data.is_empty() {
            continue;
        }

        // Use the thread name from the latest data point
        let thread_name = thread_data.last().unwrap().name.clone();
        let sanitized_name = thread_name.replace(" ", "_").replace("/", "-");
        let filename = format!("thread_{}_{}_{}.csv", sanitized_name, tid, pid);
        let filepath = path.join(&filename);

        let file_exists = filepath.exists();
        let file = if append && file_exists {
            std::fs::OpenOptions::new().append(true).open(&filepath)?
        } else {
            std::fs::File::create(&filepath)?
        };

        let mut writer = std::io::BufWriter::new(file);

        // Write header if new file
        if !append || !file_exists {
            writeln!(writer, "Timestamp,CPUUsage")?;
        }

        // Write data, ordered by timestamp
        let mut sorted_data = thread_data.clone();
        sorted_data.sort_by(|a, b| a.timestamp.unwrap().cmp(&b.timestamp.unwrap()));

        for thread in sorted_data {
            if let Some(timestamp) = thread.timestamp {
                writeln!(
                    writer,
                    "{},{}",
                    timestamp.format("%Y-%m-%d %H:%M:%S"),
                    thread.cpu_usage
                )?;
            }
        }

        writer.flush()?;
        created_files.push(filename.clone());

        // Log CSV file creation or update
        let action = if append && file_exists {
            "Updated"
        } else {
            "Created"
        };
        let message = format!("{} thread data CSV: {}", action, filepath.display());
        println!("{}", message);
        let _ = append_to_log(&message);
    }

    Ok(created_files)
}

// Function to generate a time-series chart for thread data
pub fn generate_thread_time_series_chart(
    path: PathBuf,
    package: &str,
    pid: &str,
    thread_data: &std::collections::HashMap<String, Vec<ThreadCpuInfo>>,
) -> Result<String> {
    // If there's no thread data, return early
    if thread_data.is_empty() {
        let message = "No thread data available for chart generation";
        println!("{}", message);
        return Ok(String::new());
    }

    // Filter for active threads
    let active_threads: std::collections::HashMap<String, Vec<ThreadCpuInfo>> = thread_data
        .iter()
        .filter_map(|(tid, threads)| {
            // Check if this thread has any readings with CPU > 0
            let active_points: Vec<ThreadCpuInfo> = threads
                .iter()
                .filter(|thread| thread.cpu_usage > 0.0)
                .cloned()
                .collect();

            if !active_points.is_empty() {
                Some((tid.clone(), active_points))
            } else {
                None
            }
        })
        .collect();

    if active_threads.is_empty() {
        let message = "No active threads (CPU > 0) found for chart generation";
        println!("{}", message);
        return Ok(String::new());
    }

    // Create a timestamp for the chart filename
    let timestamp_str = Local::now().format("%Y%m%d_%H%M%S").to_string();
    let chart_filename = format!("thread_time_series_{}_pid{}.png", timestamp_str, pid);
    let filepath = path.join(&chart_filename);

    // Create a chart with 3 rows: process CPU, system CPU, and thread CPU
    let root = BitMapBackend::new(&filepath, (1000, 600)).into_drawing_area();
    root.fill(&WHITE)?;

    // Create chart title with process name and PID
    let title = format!("Thread CPU Time Series - {} (PID: {})", package, pid);

    // Map of colors for different threads
    let colors = [
        &RED,
        &BLUE,
        &GREEN,
        &YELLOW,
        &MAGENTA,
        &CYAN,
        &RGBColor(128, 0, 0),   // Dark Red
        &RGBColor(0, 128, 0),   // Dark Green
        &RGBColor(0, 0, 128),   // Dark Blue
        &RGBColor(128, 128, 0), // Olive
        &RGBColor(128, 0, 128), // Purple
        &RGBColor(0, 128, 128), // Teal
    ];

    // Split the drawing area for title and chart
    let (title_area, chart_area) = root.split_vertically(50);

    // Draw the title
    title_area.titled(&title, ("sans-serif", 20))?;

    // Find the min and max timestamps from all thread data
    let mut min_time = chrono::Local::now();
    let mut max_time = chrono::Local::now() - chrono::Duration::hours(1);
    let mut max_cpu = 0.1f32;

    for (_, thread_points) in &active_threads {
        for point in thread_points {
            if let Some(timestamp) = point.timestamp {
                if timestamp < min_time {
                    min_time = timestamp;
                }
                if timestamp > max_time {
                    max_time = timestamp;
                }
                if point.cpu_usage > max_cpu {
                    max_cpu = point.cpu_usage;
                }
            }
        }
    }

    // Ensure we have a reasonable range
    if max_time <= min_time {
        max_time = min_time + chrono::Duration::minutes(5);
    }

    // Add some padding to the max CPU usage
    max_cpu = max_cpu * 1.1;
    if max_cpu < 1.0 {
        max_cpu = 1.0;
    }

    // Create the chart context
    let mut chart = ChartBuilder::on(&chart_area)
        .margin(10)
        .x_label_area_size(40)
        .y_label_area_size(60)
        .build_cartesian_2d(min_time..max_time, 0f32..max_cpu)?;

    // Configure the mesh
    chart
        .configure_mesh()
        .x_labels(8)
        .x_label_formatter(&|x| x.format("%H:%M:%S").to_string())
        .y_desc("CPU Usage (%)")
        .x_desc("Time")
        .draw()?;

    // Draw a line series for each thread
    let mut legend_entries = Vec::new();

    for (idx, (tid, thread_points)) in active_threads.iter().enumerate().take(12) {
        // Skip if no points with timestamps
        if thread_points.is_empty() || thread_points[0].timestamp.is_none() {
            continue;
        }

        // Get the thread name from first data point
        let thread_name = if !thread_points.is_empty() {
            thread_points[0].name.clone()
        } else {
            format!("Thread-{}", tid)
        };

        // Use thread name and tid for legend
        let legend_name = format!("{} ({})", thread_name, tid);
        legend_entries.push((legend_name, colors[idx % colors.len()].clone()));

        // Convert data to the format expected by the chart
        let line_data: Vec<(DateTime<Local>, f32)> = thread_points
            .iter()
            .filter_map(|point| point.timestamp.map(|ts| (ts, point.cpu_usage)))
            .collect();

        // Plot the data for this thread
        chart.draw_series(LineSeries::new(
            line_data,
            colors[idx % colors.len()].clone(),
        ))?;
    }

    // Add a legend
    if !legend_entries.is_empty() {
        chart
            .configure_series_labels()
            .background_style(&WHITE.mix(0.8))
            .border_style(&BLACK)
            .draw()?;
    }

    // Present the chart
    root.present()?;
    let message = format!("Thread time series chart saved to: {}", filepath.display());
    println!("{}", message);
    // Log chart creation
    let _ = append_to_log(&message);

    Ok(chart_filename)
}
