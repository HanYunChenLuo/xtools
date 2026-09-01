use anyhow::Result;
use chrono::{DateTime, Local};
use plotters::element::PathElement;
use plotters::prelude::*;
use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use xperf_core::ThreadCpuInfo;

// 存储当前执行期间的timestamp目录路径
static TIMESTAMP_DIR: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();
static LOG_FILE_PATH: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();

pub fn create_log_dir_if_needed(package: &str) -> Result<PathBuf> {
    let log_dir = PathBuf::from("log").join(package);
    if !log_dir.exists() {
        fs::create_dir_all(&log_dir)?;
        println!("Created log directory: {}", log_dir.display());
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

    let temp_dir = std::env::temp_dir();
    let output_file = temp_dir.join(format!("cpu_{}_chart.png", pid));
    let output_file_clone = output_file.clone();

    let x_range = (*timestamps.front().unwrap())..(*timestamps.back().unwrap());
    let root = BitMapBackend::new(&output_file, (1920, 1080)).into_drawing_area();
    root.fill(&WHITE)?;

    let areas = root.split_evenly((1, 1));
    // 单核口径下 CPU% 可超过 100%（多线程）：纵轴下限固定 100，超出自动扩展
    let max_cpu = process_cpu.iter().cloned().fold(0f32, f32::max);
    let y_max = if max_cpu > 100.0 { max_cpu * 1.1 } else { 100.0 };
    let mut process_chart = ChartBuilder::on(&areas[0])
        .margin(15)
        .x_label_area_size(40)
        .y_label_area_size(60)
        .build_cartesian_2d(x_range.clone(), 0f32..y_max)?;

    let mut mesh_config = process_chart.configure_mesh();
    mesh_config
        .y_desc("Process CPU")
        .y_label_formatter(&|v| format!("{:.1}", v))
        .x_desc("Time")
        .x_labels(10)
        .x_label_formatter(&|x| x.format("%H:%M:%S").to_string());
    mesh_config.draw()?;

    let series = process_cpu
        .iter()
        .zip(timestamps.iter())
        .map(|(y, x)| (*x, *y));

    process_chart
        .draw_series(LineSeries::new(series, BLUE.stroke_width(2)))?
        .label(format!("Process CPU (PID: {})", pid))
        .legend(|(x, y)| PathElement::new(vec![(x, y), (x + 20, y)], BLUE.stroke_width(2)));

    process_chart
        .configure_series_labels()
        .background_style(WHITE.mix(0.8))
        .border_style(BLACK)
        .draw()?;

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
    let path = output_dir.join("cpu_summary.png");
    let path_clone = path.clone();
    let root = BitMapBackend::new(&path, (1920, 1080)).into_drawing_area();
    root.fill(&WHITE)?;

    let mut min_time = None;
    let mut max_time = None;
    let mut max_cpu = 1.0f32;
    for (_, ts, cpu) in series {
        if let (Some(&t0), Some(&tn)) = (ts.front(), ts.back()) {
            min_time = Some(min_time.map_or(t0, |m: DateTime<Local>| m.min(t0)));
            max_time = Some(max_time.map_or(tn, |m: DateTime<Local>| m.max(tn)));
        }
        for &c in cpu.iter() {
            if c > max_cpu { max_cpu = c; }
        }
    }
    let min_time = min_time.ok_or_else(|| anyhow::format_err!("No CPU data for summary"))?;
    let max_time = max_time.unwrap_or(min_time);
    max_cpu *= 1.1;
    if max_cpu < 1.0 { max_cpu = 1.0; }

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

pub fn export_cpu_data_to_csv(
    path: &PathBuf,
    timestamps: &VecDeque<DateTime<Local>>,
    process_cpu: &VecDeque<f32>,
) -> Result<()> {
    let mut file = fs::File::create(path)?;
    writeln!(file, "Timestamp,Process CPU (%)")?;
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

/// 导出 FPS 时序数据到 CSV
pub fn export_fps_data_to_csv(path: &PathBuf, data: &xperf_core::FpsTimeSeriesData) -> Result<()> {
    let mut file = fs::File::create(path)?;
    writeln!(file, "Timestamp,FPS,Jank")?;
    for i in 0..data.timestamps.len() {
        writeln!(
            file,
            "{},{:.2},{}",
            data.timestamps[i].format("%Y-%m-%d %H:%M:%S"),
            data.fps[i],
            data.jank_counts[i]
        )?;
    }
    file.flush()?;
    Ok(())
}

pub fn create_timestamp_subdir(package: &str) -> Result<PathBuf> {
    let cell = TIMESTAMP_DIR.get_or_init(|| Mutex::new(None));
    let mut guard = cell.lock().unwrap();

    if let Some(ref dir) = *guard {
        return Ok(dir.clone());
    }

    let log_dir = create_log_dir_if_needed(package)?;
    let timestamp_str = Local::now().format("%Y%m%d_%H%M%S").to_string();
    let timestamp_dir = log_dir.join(&timestamp_str);

    if !timestamp_dir.exists() {
        std::fs::create_dir_all(&timestamp_dir)?;
        let msg = format!("Created timestamp directory: {}", timestamp_dir.display());
        println!("{}", msg);
        let _ = append_to_log(&msg);
    }

    *guard = Some(timestamp_dir.clone());
    Ok(timestamp_dir)
}

pub fn export_thread_data_to_csv(
    path: PathBuf,
    pid: &str,
    threads: &[ThreadCpuInfo],
    append: bool,
) -> Result<Vec<String>> {
    let mut created_files = Vec::new();
    let active_threads: Vec<&ThreadCpuInfo> = threads.iter().filter(|t| t.cpu_usage > 0.0).collect();

    if active_threads.is_empty() {
        return Ok(created_files);
    }

    let mut thread_map: std::collections::HashMap<String, Vec<&ThreadCpuInfo>> =
        std::collections::HashMap::new();
    for thread in active_threads {
        if thread.timestamp.is_some() {
            thread_map.entry(thread.tid.clone()).or_default().push(thread);
        }
    }

    for (tid, thread_data) in thread_map {
        if thread_data.is_empty() {
            continue;
        }
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
        if !append || !file_exists {
            writeln!(writer, "Timestamp,CPUUsage")?;
        }

        let mut sorted_data = thread_data.clone();
        sorted_data.sort_by(|a, b| a.timestamp.unwrap().cmp(&b.timestamp.unwrap()));
        for thread in sorted_data {
            if let Some(timestamp) = thread.timestamp {
                writeln!(writer, "{},{}", timestamp.format("%Y-%m-%d %H:%M:%S"), thread.cpu_usage)?;
            }
        }
        writer.flush()?;
        created_files.push(filename);
    }

    Ok(created_files)
}

pub fn generate_thread_time_series_chart(
    path: PathBuf,
    package: &str,
    pid: &str,
    thread_data: &std::collections::HashMap<String, Vec<ThreadCpuInfo>>,
) -> Result<String> {
    if thread_data.is_empty() {
        return Ok(String::new());
    }

    let active_threads: std::collections::HashMap<String, Vec<ThreadCpuInfo>> = thread_data
        .iter()
        .filter_map(|(tid, threads)| {
            let active_points: Vec<ThreadCpuInfo> =
                threads.iter().filter(|t| t.cpu_usage > 0.0).cloned().collect();
            if !active_points.is_empty() {
                Some((tid.clone(), active_points))
            } else {
                None
            }
        })
        .collect();

    if active_threads.is_empty() {
        return Ok(String::new());
    }

    let timestamp_str = Local::now().format("%Y%m%d_%H%M%S").to_string();
    let chart_filename = format!("thread_time_series_{}_pid{}.png", timestamp_str, pid);
    let filepath = path.join(&chart_filename);

    let root = BitMapBackend::new(&filepath, (1920, 1080)).into_drawing_area();
    root.fill(&WHITE)?;

    let title = format!("Thread CPU Time Series - {} (PID: {})", package, pid);
    let colors = [
        &RED, &BLUE, &GREEN, &YELLOW, &MAGENTA, &CYAN,
        &RGBColor(128, 0, 0), &RGBColor(0, 128, 0), &RGBColor(0, 0, 128),
        &RGBColor(128, 128, 0), &RGBColor(128, 0, 128), &RGBColor(0, 128, 128),
    ];

    let (title_area, chart_area) = root.split_vertically(50);
    title_area.titled(&title, ("sans-serif", 20))?;

    let mut min_time = chrono::Local::now();
    let mut max_time = chrono::Local::now() - chrono::Duration::hours(1);
    let mut max_cpu = 0.1f32;
    for thread_points in active_threads.values() {
        for point in thread_points {
            if let Some(timestamp) = point.timestamp {
                if timestamp < min_time { min_time = timestamp; }
                if timestamp > max_time { max_time = timestamp; }
                if point.cpu_usage > max_cpu { max_cpu = point.cpu_usage; }
            }
        }
    }
    if max_time <= min_time {
        max_time = min_time + chrono::Duration::minutes(5);
    }
    max_cpu *= 1.1;
    if max_cpu < 1.0 { max_cpu = 1.0; }

    let mut chart = ChartBuilder::on(&chart_area)
        .margin(10)
        .x_label_area_size(40)
        .y_label_area_size(60)
        .build_cartesian_2d(min_time..max_time, 0f32..max_cpu)?;

    chart.configure_mesh()
        .x_labels(8)
        .x_label_formatter(&|x| x.format("%H:%M:%S").to_string())
        .y_desc("CPU Usage (%)")
        .x_desc("Time")
        .draw()?;

    let mut legend_entries = Vec::new();
    for (idx, (tid, thread_points)) in active_threads.iter().enumerate().take(12) {
        if thread_points.is_empty() || thread_points[0].timestamp.is_none() {
            continue;
        }
        let thread_name = thread_points[0].name.clone();
        let legend_name = format!("{} ({})", thread_name, tid);
        let color = *colors[idx % colors.len()];
        let line_data: Vec<(DateTime<Local>, f32)> = thread_points
            .iter()
            .filter_map(|p| p.timestamp.map(|ts| (ts, p.cpu_usage)))
            .collect();
        chart.draw_series(LineSeries::new(line_data, color))?
            .label(legend_name.clone())
            .legend(move |(x, y)| PathElement::new(vec![(x, y), (x + 20, y)], color));
        legend_entries.push((legend_name, color));
    }

    if !legend_entries.is_empty() {
        chart.configure_series_labels()
            .background_style(WHITE.mix(0.8))
            .border_style(BLACK)
            .position(SeriesLabelPosition::UpperRight)
            .margin(10)
            .draw()?;
    }

    root.present()?;
    Ok(chart_filename)
}
