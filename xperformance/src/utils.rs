use crate::cpu::ThreadCpuInfo;
use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use plotters::element::PathElement;
use plotters::prelude::*;
use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::{Mutex, OnceLock};

// 全局静态变量，用于跟踪中断状态
static INTERRUPT_FLAG: AtomicBool = AtomicBool::new(false);
static LOG_FILE_PATH: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();

// 存储当前执行期间的timestamp目录路径
static TIMESTAMP_DIR: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();

#[derive(Debug)]
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

/// 获取包名下所有进程（cmdline 等于包名的进程，多进程应用可能返回多个）。
/// pidof 在进程不存在时退出码非零且 stdout 为空，靠 stdout 是否为空判断。
pub fn get_all_processes(package: &str) -> Result<Vec<ProcessInfo>> {
    let output = run_adb_command(&["shell", "pidof", package])?;
    let pids: Vec<&str> = output.stdout.split_whitespace().collect();
    if pids.is_empty() {
        anyhow::bail!("Process not found for package: {}", package);
    }
    let mut processes = Vec::with_capacity(pids.len());
    for pid in pids {
        let start_time = run_adb_command(&[
            "shell",
            "stat",
            "-c",
            "%y",
            format!("/proc/{}/cmdline", pid).as_str(),
        ])?;
        processes.push(ProcessInfo {
            pid: pid.to_string(),
            start_time: start_time.stdout.trim().to_string(),
        });
    }
    Ok(processes)
}

/// 子进程执行结果。
///
/// 注意区分两种"失败"：
/// - **子进程无法启动**：`run_command` 返回 `Err`。
/// - **子进程退出码非零**：`stdout` 仍可能含有效内容。例如 `cat` 部分文件缺失、
///   `pidof` 找不到进程、`grep` 未命中都会返回非零退出码，但 stdout 照常返回。
///   调用方按语义判断：只需 stdout 内容时直接用 `stdout`。
///
/// 后续如需严格判断退出码或诊断 stderr，可在此结构体补充字段：
///   `success: bool`（退出码是否为 0）、`exit_code: i32`、`stderr: String`。
#[derive(Debug, Clone)]
pub struct ProcOutput {
    /// 子进程 stdout（已清洗 ANSI 控制字符）。
    pub stdout: String,
}

/// 执行子进程，返回 stdout。
///
/// 仅当子进程无法启动时返回 `Err`；退出码非零不返回 `Err`，`stdout` 照常返回。
pub fn run_command(program: &str, args: &[&str]) -> Result<ProcOutput> {
    let output = Command::new(program)
        .args(args)
        .env("TERM", "dumb")
        .output()
        .with_context(|| format!("Failed to execute command: {}", program))?;

    Ok(ProcOutput {
        stdout: clean_control_chars(&String::from_utf8_lossy(&output.stdout)),
    })
}

/// 执行 adb 命令。`run_command` 的薄封装。
///
/// 测试可通过 `set_adb_runner_for_test` 注入 mock 实现，避免真实拉起 adb 子进程。
pub fn run_adb_command(args: &[&str]) -> Result<ProcOutput> {
    if let Ok(guard) = ADB_RUNNER_OVERRIDE.lock() {
        if let Some(runner) = *guard {
            return runner(args);
        }
    }
    run_command("adb", args)
}

/// adb 命令执行器的类型（函数指针，不捕获外部状态，按 args 分支返回）。
pub type AdbRunner = fn(&[&str]) -> Result<ProcOutput>;

static ADB_RUNNER_OVERRIDE: Mutex<Option<AdbRunner>> = Mutex::new(None);

/// 注入 mock adb 执行器，仅用于单元测试。
/// 可重复调用（覆盖前一次设置），但测试间共享全局状态，需 `--test-threads=1` 运行。
#[cfg(test)]
pub fn set_adb_runner_for_test(runner: AdbRunner) {
    if let Ok(mut guard) = ADB_RUNNER_OVERRIDE.lock() {
        *guard = Some(runner);
    }
}

/// 清除 mock adb 执行器，恢复真实 adb 调用，仅用于单元测试。
#[cfg(test)]
pub fn clear_adb_runner_for_test() {
    if let Ok(mut guard) = ADB_RUNNER_OVERRIDE.lock() {
        *guard = None;
    }
}
fn clean_control_chars(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\x1B' && chars.peek() == Some(&'[') {
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
    let cell = LOG_FILE_PATH.get_or_init(|| Mutex::new(None));
    let guard = cell.lock().unwrap();
    let path = match guard.as_ref() {
        Some(p) => p.clone(),
        None => anyhow::bail!("Log file not initialized"),
    };
    drop(guard);

    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;

    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");
    writeln!(file, "\n[{}]", timestamp)?;
    writeln!(file, "{}", content)?;
    file.flush()?;

    Ok(())
}

pub fn generate_cpu_chart(
    timestamps: &VecDeque<DateTime<Local>>,
    process_cpu: &VecDeque<f32>,
    pid: &str,
) -> Result<PathBuf> {
    if timestamps.is_empty() || process_cpu.is_empty() {
        return Err(anyhow::format_err!("No CPU data to chart"));
    }

    // 直接创建输出文件路径，不创建目录。文件名带 pid 以区分多进程。
    let temp_dir = std::env::temp_dir();
    let output_file = temp_dir.join(format!("cpu_{}_chart.png", pid));
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
        .label(format!("Process CPU (PID: {})", pid))
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

/// 单个 PID 的 CPU 时序引用（pid, timestamps, process_cpu），用于汇总图。
pub type CpuSeriesRef<'a> = (String, &'a VecDeque<DateTime<Local>>, &'a VecDeque<f32>);

/// 生成多 PID CPU 汇总对比图：每个 PID 一条折线（不同颜色）。
pub fn generate_cpu_summary_chart(
    output_dir: &std::path::Path,
    package: &str,
    series: &[CpuSeriesRef<'_>],
) -> Result<PathBuf> {
    use plotters::prelude::*;

    let path = output_dir.join("cpu_summary.png");
    let path_clone = path.clone();
    let root = BitMapBackend::new(&path, (1920, 1080)).into_drawing_area();
    root.fill(&WHITE)?;

    // 计算 X/Y 范围（跨所有 PID）
    let mut min_time = None;
    let mut max_time = None;
    let mut max_cpu = 1.0f32;
    for (_, ts, cpu) in series {
        if let (Some(&t0), Some(&tn)) = (ts.front(), ts.back()) {
            min_time = Some(min_time.map_or(t0, |m: DateTime<Local>| m.min(t0)));
            max_time = Some(max_time.map_or(tn, |m: DateTime<Local>| m.max(tn)));
        }
        for &c in cpu.iter() {
            if c > max_cpu {
                max_cpu = c;
            }
        }
    }
    let min_time = min_time.ok_or_else(|| anyhow::format_err!("No CPU data for summary"))?;
    let max_time = max_time.unwrap_or(min_time);
    max_cpu *= 1.1;
    if max_cpu < 1.0 {
        max_cpu = 1.0;
    }

    let mut chart = ChartBuilder::on(&root)
        .caption(format!("{} - CPU Summary ({} PIDs)", package, series.len()), ("sans-serif", 22).into_font())
        .margin(15)
        .x_label_area_size(40)
        .y_label_area_size(60)
        .build_cartesian_2d(min_time..max_time, 0f32..max_cpu)?;

    chart.configure_mesh()
        .y_desc("Process CPU (%)")
        .x_desc("Time")
        .x_labels(10)
        .x_label_formatter(&|x| x.format("%H:%M:%S").to_string())
        .draw()?;

    let colors = [
        RGBColor(31, 119, 180), RGBColor(255, 127, 14), RGBColor(44, 160, 44),
        RGBColor(214, 39, 40), RGBColor(148, 103, 189), RGBColor(140, 86, 75),
        RGBColor(227, 119, 194), RGBColor(127, 127, 127), RGBColor(188, 189, 34),
        RGBColor(23, 190, 207),
    ];

    for (i, (pid, ts, cpu)) in series.iter().enumerate() {
        let color = colors[i % colors.len()];
        let pts: Vec<(DateTime<Local>, f32)> = ts.iter().zip(cpu.iter()).map(|(t, &c)| (*t, c)).collect();
        chart.draw_series(LineSeries::new(pts, color.stroke_width(2)))?
            .label(format!("PID {}", pid))
            .legend(move |(x, y)| PathElement::new(vec![(x, y), (x + 20, y)], color.stroke_width(2)));
    }
    chart.configure_series_labels()
        .background_style(WHITE.mix(0.8))
        .border_style(BLACK)
        .draw()?;

    root.present()?;
    Ok(path_clone)
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
    let cell = TIMESTAMP_DIR.get_or_init(|| Mutex::new(None));
    let mut guard = cell.lock().unwrap();

    // 检查缓存中是否已存在timestamp目录
    if let Some(ref dir) = *guard {
        return Ok(dir.clone());
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
    *guard = Some(timestamp_dir.clone());

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
                .or_default()
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

    for thread_points in active_threads.values() {
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
    max_cpu *= 1.1;
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
        let color = *colors[idx % colors.len()];

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
            .background_style(WHITE.mix(0.8))
            .border_style(BLACK)
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

#[cfg(test)]
mod tests {
    use super::*;

    // ---- clean_control_chars：ANSI CSI 转义序列清洗 ----

    #[test]
    fn test_clean_control_chars_strips_color_codes() {
        // \x1B[31m = 红色，\x1B[0m = 重置
        let input = "\x1B[31mred text\x1B[0m";
        assert_eq!(clean_control_chars(input), "red text");
    }

    #[test]
    fn test_clean_control_chars_strips_multiple_codes() {
        let input = "\x1B[1;32mbold green\x1B[0m and \x1B[33myellow\x1B[0m";
        assert_eq!(clean_control_chars(input), "bold green and yellow");
    }

    #[test]
    fn test_clean_control_chars_no_escape_passes_through() {
        assert_eq!(clean_control_chars("plain text"), "plain text");
        assert_eq!(clean_control_chars(""), "");
    }

    #[test]
    fn test_clean_control_chars_preserves_other_control_chars() {
        // 非 CSI 的控制字符（如 \n、\t）原样保留
        assert_eq!(clean_control_chars("line1\nline2\ttab"), "line1\nline2\ttab");
    }

    #[test]
    fn test_clean_control_chars_strips_cursor_movement() {
        // \x1B[2K = 清行，\x1B[H = 光标归位
        let input = "\x1B[2K\x1B[Hhello";
        assert_eq!(clean_control_chars(input), "hello");
    }

    // ---- get_all_processes 的 pidof 输出解析（通过 run_adb_command 间接，这里测 split 逻辑）----
    // 注：get_all_processes 本身依赖 adb，不单测；但 pidof 多 PID 的 split 行为
    // 已在真机验证（浏览器 2 PID 场景）。

    #[test]
    fn test_pidof_multi_pid_split() {
        // 验证 pidof 返回空格分隔多 PID 时 split_whitespace 的行为
        let stdout = "1119 16071\n";
        let pids: Vec<&str> = stdout.split_whitespace().collect();
        assert_eq!(pids, vec!["1119", "16071"]);
    }

    #[test]
    fn test_pidof_single_pid_split() {
        let stdout = "15803\n";
        let pids: Vec<&str> = stdout.split_whitespace().collect();
        assert_eq!(pids, vec!["15803"]);
    }

    #[test]
    fn test_pidof_empty_split() {
        let stdout = "\n";
        let pids: Vec<&str> = stdout.split_whitespace().collect();
        assert!(pids.is_empty());
    }

    // ---- get_all_processes（注入 mock adb runner）----

    fn mock_runner_for_get_all_processes(args: &[&str]) -> Result<ProcOutput> {
        // 匹配 pidof 调用
        if args.len() >= 3 && args[0] == "shell" && args[1] == "pidof" {
            return Ok(ProcOutput {
                stdout: "1119 16071\n".to_string(),
            });
        }
        // 匹配 stat -c %y /proc/<pid>/cmdline 调用
        if args.len() >= 5 && args[1] == "stat" {
            // 从 args[4] 提取 pid
            let path = args[4];
            if let Some(pid_start) = path.find("/proc/") {
                let rest = &path[pid_start + 6..];
                if let Some(pid_end) = rest.find('/') {
                    let pid = &rest[..pid_end];
                    return Ok(ProcOutput {
                        stdout: format!("2026-09-01 10:00:00.000000000 +0800 pid={}\n", pid),
                    });
                }
            }
        }
        Ok(ProcOutput { stdout: String::new() })
    }

    #[test]
    fn test_get_all_processes_multi_pid() {
        set_adb_runner_for_test(mock_runner_for_get_all_processes);
        let procs = get_all_processes("com.lixiang.car.browser").unwrap();
        assert_eq!(procs.len(), 2);
        assert_eq!(procs[0].pid, "1119");
        assert_eq!(procs[1].pid, "16071");
        assert!(procs[0].start_time.contains("pid=1119"));
        assert!(procs[1].start_time.contains("pid=16071"));
        clear_adb_runner_for_test();
    }

    #[test]
    fn test_get_all_processes_single_pid() {
        fn single(args: &[&str]) -> Result<ProcOutput> {
            if args[1] == "pidof" {
                return Ok(ProcOutput { stdout: "15803\n".to_string() });
            }
            Ok(ProcOutput { stdout: "2026-09-01 10:00:00 +0800\n".to_string() })
        }
        set_adb_runner_for_test(single);
        let procs = get_all_processes("com.x").unwrap();
        assert_eq!(procs.len(), 1);
        assert_eq!(procs[0].pid, "15803");
        clear_adb_runner_for_test();
    }

    #[test]
    fn test_get_all_processes_not_found() {
        fn empty(_args: &[&str]) -> Result<ProcOutput> {
            Ok(ProcOutput { stdout: "\n".to_string() })
        }
        set_adb_runner_for_test(empty);
        let err = get_all_processes("com.nonexistent").unwrap_err();
        assert!(
            err.to_string().contains("Process not found"),
            "应报 Process not found，实际: {}",
            err
        );
        clear_adb_runner_for_test();
    }
}

