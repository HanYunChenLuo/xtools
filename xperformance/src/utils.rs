use crate::cpu::ThreadCpuInfo;
use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use plotters::element::PathElement;
use plotters::prelude::*;
use plotters::style::Color;
use plotters::style::RGBColor;
use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::str;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::Mutex;

// 全局静态变量，用于跟踪中断状态
static INTERRUPT_FLAG: AtomicBool = AtomicBool::new(false);
static mut LOG_FILE_PATH: Option<PathBuf> = None;

// 存储当前执行期间的timestamp目录路径
static mut TIMESTAMP_DIR: Option<PathBuf> = None;
static TIMESTAMP_DIR_MUTEX: Mutex<()> = Mutex::new(());

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
    pid: &str,
) -> Result<PathBuf> {
    if timestamps.is_empty() || process_cpu.is_empty() {
        return Err(anyhow::format_err!("No CPU data to chart"));
    }

    // 直接创建输出文件路径，不创建目录
    let temp_dir = std::env::temp_dir();
    let output_file = temp_dir.join(format!("{}_cpu_chart.png", package));
    // 创建一个克隆用于返回
    let output_file_clone = output_file.clone();

    // Create X-axis range (timestamps)
    let x_range = (*timestamps.front().unwrap())..(*timestamps.back().unwrap());

    // Create root drawing area
    let root = BitMapBackend::new(&output_file, (1920, 1080)).into_drawing_area();
    root.fill(&WHITE)?;

    // Only one chart for process CPU
    let chart_count = 1;

    // Split the drawing area into subplots
    let areas = root.split_evenly((chart_count, 1));
    let area_index = 0;

    // Process CPU (always shown)
    let mut process_chart = ChartBuilder::on(&areas[area_index])
        .margin(15)
        .x_label_area_size(40) // Always show X-axis labels
        .y_label_area_size(60)
        .build_cartesian_2d(x_range.clone(), 0f32..100f32)?;

    // 创建持久的mesh配置
    let mut mesh_config = process_chart.configure_mesh();
    mesh_config
        .y_desc("Process CPU")
        .y_label_formatter(&|v| format!("{:.1}", v))
        .x_desc("Time")
        .x_labels(10)
        .x_label_formatter(&|x| x.format("%H:%M:%S").to_string());

    mesh_config.draw()?;

    // 转换数据为可绘制格式
    let series = process_cpu
        .iter()
        .zip(timestamps.iter())
        .map(|(y, x)| (*x, *y));

    // 绘制进程CPU线
    process_chart
        .draw_series(LineSeries::new(series, BLUE.stroke_width(2)))?
        .label(&format!("Process CPU (PID: {})", pid))
        .legend(|(x, y)| PathElement::new(vec![(x, y), (x + 20, y)], BLUE.stroke_width(2)));

    // 添加图例
    process_chart
        .configure_series_labels()
        .background_style(WHITE.mix(0.8))
        .border_style(BLACK)
        .draw()?;

    // 导出数据到CSV (保留这个功能)
    let csv_path = output_file.with_extension("csv");
    export_cpu_data_to_csv(&csv_path, timestamps, process_cpu)?;

    Ok(output_file_clone)
}

// 添加一个新函数用于导出CSV数据
pub fn export_cpu_data_to_csv(
    path: &PathBuf,
    timestamps: &VecDeque<DateTime<Local>>,
    process_cpu: &VecDeque<f32>,
) -> Result<()> {
    let mut file = fs::File::create(path)?;

    // 写入CSV头
    writeln!(file, "Timestamp,Process CPU (%)")?;

    // 写入数据行
    for i in 0..timestamps.len() {
        writeln!(
            file,
            "{},{:.2}",
            timestamps[i].format("%Y-%m-%d %H:%M:%S"),
            process_cpu[i]
        )?;
    }

    file.flush()?;
    Ok(())
}

// Function to create timestamp subdirectory within the log directory
pub fn create_timestamp_subdir(package: &str) -> Result<PathBuf> {
    // 使用互斥锁保护静态变量的访问
    let _lock = TIMESTAMP_DIR_MUTEX.lock().unwrap();

    // 检查缓存中是否已存在timestamp目录
    unsafe {
        if let Some(ref dir) = TIMESTAMP_DIR {
            return Ok(dir.clone());
        }
    }

    // 如果没有，创建新的timestamp目录
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

    // 缓存目录路径
    unsafe {
        TIMESTAMP_DIR = Some(timestamp_dir.clone());
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
    let root = BitMapBackend::new(&filepath, (1920, 1080)).into_drawing_area();
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
        let color = colors[idx % colors.len()].clone();

        // Convert data to the format expected by the chart
        let line_data: Vec<(DateTime<Local>, f32)> = thread_points
            .iter()
            .filter_map(|point| point.timestamp.map(|ts| (ts, point.cpu_usage)))
            .collect();

        // Plot the data for this thread with label
        chart
            .draw_series(LineSeries::new(line_data, color))?
            .label(legend_name.clone())
            .legend(move |(x, y)| PathElement::new(vec![(x, y), (x + 20, y)], color));

        legend_entries.push((legend_name, color));
    }

    // Add a legend with better positioning and size
    if !legend_entries.is_empty() {
        chart
            .configure_series_labels()
            .background_style(&WHITE.mix(0.8))
            .border_style(&BLACK)
            .position(SeriesLabelPosition::UpperRight)
            .margin(10)
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

// 设置中断标志
pub fn set_interrupt_flag() {
    INTERRUPT_FLAG.store(true, AtomicOrdering::SeqCst);
}

// 检查程序是否正在被中断
pub fn is_being_interrupted() -> bool {
    INTERRUPT_FLAG.load(AtomicOrdering::SeqCst)
}
