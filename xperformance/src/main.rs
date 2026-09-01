#![deny(warnings)]
use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use clap::Parser;
use colored::*;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::time::Instant;

mod utils;
use utils as cli_utils;

use xperf_core::ThreadCpuInfo;

#[derive(Parser, Debug)]
#[command(version, about = "XPerformance Monitor - Android process CPU/memory monitor", long_about = None)]
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

    /// Monitor FPS (SurfaceFlinger layer frame timestamps, works for SurfaceView/game direct rendering)
    #[arg(long)]
    fps: bool,

    /// Sampling interval in milliseconds (default: 1000)
    #[arg(short, long, default_value_t = 1000)]
    interval: u64,
}

fn check_adb() -> Result<()> {
    let output = Command::new("adb")
        .arg("devices")
        .output()
        .context("Failed to execute adb command")?;
    if !output.status.success() {
        anyhow::bail!("ADB command failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    let devices = String::from_utf8_lossy(&output.stdout);
    let connected: Vec<&str> = devices
        .lines()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .collect();
    if connected.is_empty() {
        anyhow::bail!("No ADB devices connected. Please connect a device and enable USB debugging.");
    }
    println!("Connected devices: {}", connected.len());
    Ok(())
}

async fn monitor_process(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    println!("{}", "XPerformance Monitor".green().bold());
    println!("Monitoring package: {}", args.package.cyan());
    println!("Sampling interval: {} ms", args.interval);

    check_adb()?;

    if !args.cpu && !args.memory && !args.fps {
        println!("No monitoring options selected. Use --cpu, --memory or --fps");
        return Ok(());
    }

    // 统一走设备端 agent 采样（无 adb 轮询路径）
    monitor_process_agent(args).await
}

/// 统一采样路径：设备端 agent 常驻采样 + exec-out 事件流。
/// 输出策略：interval ≥ 500ms 逐条详细打印（低频率，同旧轮询模式的信息量）；
/// < 500ms 时按 ~1s 聚合打印（逐条会刷屏），全量明细均在退出 CSV 中。
async fn monitor_process_agent(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    use xperf_core::agent::{self, AgentEvent};

    let bin = agent::ensure_agent_built()?;
    agent::deploy_agent(&bin)?;
    let mut stream = agent::spawn_agent(Some(&args.package), args.interval, args.cpu, args.memory, args.fps)?;
    println!("agent 已部署并启动（间隔 {}ms）", args.interval);

    let verbose = args.interval >= 500;

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
        xperf_core::utils::set_interrupt_flag();
        println!("\n程序正在退出...");
    })?;

    let mut pid_stats: std::collections::HashMap<String, xperf_core::PidStats> = Default::default();
    let mut thread_time_series: std::collections::HashMap<
        String,
        std::collections::HashMap<String, Vec<ThreadCpuInfo>>,
    > = Default::default();
    let mut restart_count = 0u32;

    // 终端聚合输出（仅 interval < 500ms 用）：每 PID 累计窗口内样本，每秒打印一行
    #[derive(Default)]
    struct Agg {
        n: u32,
        sum: f32,
        max: f32,
        pss: u64,
        rss: u64,
        has_mem: bool,
        fps: Option<(String, f32, u32)>, // 最新 (图层, fps, jank)
    }
    let mut aggs: std::collections::HashMap<u32, Agg> = Default::default();
    let mut last_print = Instant::now();

    while running.load(Ordering::SeqCst) {
        let Some(ev) = stream.next_event()? else { break }; // EOF：agent 退出/断连
        let ev = match ev {
            Ok(ev) => ev,
            Err(e) => {
                println!("{}", format!("协议解析失败: {}", e).yellow());
                continue;
            }
        };
        match ev {
            AgentEvent::Hello { ncores } => println!("设备 {} 核", ncores),
            AgentEvent::Cpu { ts, pid, cpu, th } => {
                let Some(t) = DateTime::from_timestamp_millis(ts as i64)
                    .map(|t| t.with_timezone(&Local))
                else {
                    continue;
                };
                let threads: Vec<ThreadCpuInfo> = th
                    .iter()
                    .map(|(tid, name, usage)| ThreadCpuInfo {
                        tid: tid.to_string(),
                        cpu_usage: *usage,
                        name: name.clone(),
                        timestamp: Some(t),
                    })
                    .collect();
                if verbose {
                    println!(
                        "[{}] Process CPU: {}% (pid: {})",
                        t.format("%H:%M:%S"),
                        format!("{:.1}", cpu).blue(),
                        pid.to_string().yellow()
                    );
                    let mut top = threads.clone();
                    top.sort_by(|a, b| b.cpu_usage.partial_cmp(&a.cpu_usage).unwrap());
                    top.truncate(5);
                    for thread in top {
                        println!(
                            "  {} ({}): {}%",
                            thread.name.green(),
                            thread.tid.yellow(),
                            format!("{:.1}", thread.cpu_usage).blue()
                        );
                    }
                }
                let s = pid_stats.entry(pid.to_string()).or_default();
                s.active = true;
                s.cpu_data.add_data_point(t, cpu, threads.clone());
                if s.cpu_time.is_none() || cpu > s.cpu_usage {
                    s.cpu_usage = cpu;
                    s.cpu_time = Some(t);
                }
                if args.thread {
                    let per_pid = thread_time_series.entry(pid.to_string()).or_default();
                    for t2 in threads {
                        if t2.cpu_usage > 0.0 {
                            per_pid.entry(t2.name.clone()).or_default().push(t2);
                        }
                    }
                }
                let a = aggs.entry(pid).or_default();
                a.n += 1;
                a.sum += cpu;
                if cpu > a.max {
                    a.max = cpu;
                }
            }
            AgentEvent::Mem { ts, pid, pss, rss, java, native, code, stack, gfx, other, sys } => {
                let Some(t) = DateTime::from_timestamp_millis(ts as i64)
                    .map(|t| t.with_timezone(&Local))
                else {
                    continue;
                };
                if verbose {
                    println!(
                        "[{}] Memory Usage: {} KB (Java: {}, Native: {}, Code: {}, Graphics: {}) [pid {}]",
                        t.format("%H:%M:%S"),
                        pss.to_string().blue(),
                        java,
                        native,
                        code,
                        gfx,
                        pid.to_string().yellow()
                    );
                }
                let s = pid_stats.entry(pid.to_string()).or_default();
                s.active = true;
                // 直接推入时序（绕过轮询模式遗留的 300 点上限，保高频数据完整）
                s.memory_data.timestamps.push_back(t);
                s.memory_data.memory_details.push_back(xperf_core::MemoryDetails {
                    java_heap: java,
                    native_heap: native,
                    code,
                    stack,
                    graphics: gfx,
                    private_other: other,
                    system: sys,
                    total_pss: pss,
                });
                if s.memory_time.is_none() || pss > s.memory_usage {
                    s.memory_usage = pss;
                    s.memory_time = Some(t);
                }
                let a = aggs.entry(pid).or_default();
                a.pss = pss;
                a.rss = rss;
                a.has_mem = true;
            }
            AgentEvent::Fps { ts, pid, layer, fps, frames, jank } => {
                let Some(t) = DateTime::from_timestamp_millis(ts as i64)
                    .map(|t| t.with_timezone(&Local))
                else {
                    continue;
                };
                if verbose {
                    println!(
                        "[{}] FPS: {} (jank: {}, frames: {}, layer: {}) [pid {}]",
                        t.format("%H:%M:%S"),
                        format!("{:.1}", fps).blue(),
                        jank.to_string().red(),
                        frames,
                        layer.green(),
                        pid.to_string().yellow()
                    );
                }
                let s = pid_stats.entry(pid.to_string()).or_default();
                s.active = true;
                s.fps_data.add_data_point(t, fps, jank, &layer);
                let a = aggs.entry(pid).or_default();
                a.fps = Some((layer, fps, jank));
            }
            AgentEvent::Exit { pid } => {
                restart_count += 1;
                if let Some(s) = pid_stats.get_mut(&pid.to_string()) {
                    s.active = false;
                }
                println!("[{}] PID {} 已退出", Local::now().format("%H:%M:%S"), pid.to_string().yellow());
            }
            AgentEvent::Noproc => {} // 无进程期间 agent 每秒报一次，无需逐条打印
            AgentEvent::Err { msg } => println!("{}", format!("agent: {}", msg).yellow()),
        }

        // 低间隔模式：每秒聚合打印一次（逐条会刷屏）
        if !verbose && last_print.elapsed() >= std::time::Duration::from_secs(1) {
            last_print = Instant::now();
            let ts = Local::now().format("%H:%M:%S");
            for (pid, a) in aggs.iter_mut() {
                let mut parts = Vec::new();
                if a.n > 0 {
                    parts.push(format!(
                        "CPU avg {} max {}",
                        format!("{:.1}%", a.sum / a.n as f32).blue(),
                        format!("{:.1}%", a.max).red()
                    ));
                }
                if a.has_mem {
                    parts.push(format!("PSS {} KB (RSS {})", a.pss.to_string().blue(), a.rss));
                }
                if let Some((layer, fps, jank)) = &a.fps {
                    parts.push(format!("FPS {} (jank {}, {})", format!("{:.1}", fps).blue(), jank, layer.green()));
                }
                if !parts.is_empty() {
                    println!("[{}] {} (pid: {})", ts, parts.join(" | "), pid.to_string().yellow());
                }
                a.n = 0;
                a.sum = 0.0;
                a.max = 0.0;
            }
        }
    }

    drop(stream); // 杀掉设备端 agent（Drop 里 kill）
    generate_final_outputs(args, &pid_stats, &thread_time_series)?;
    println!("Process Restarts: {}", restart_count.to_string().red());
    Ok(())
}

fn generate_final_outputs(
    args: &Args,
    pid_stats: &std::collections::HashMap<String, xperf_core::PidStats>,
    thread_time_series: &std::collections::HashMap<String, std::collections::HashMap<String, Vec<ThreadCpuInfo>>>,
) -> Result<(), Box<dyn std::error::Error>> {
    // 线程时序图（按 PID）
    if args.thread && args.cpu && !thread_time_series.is_empty() {
        println!("Program ending, generating final thread time series chart...");
        if let Ok(timestamp_dir) = cli_utils::create_timestamp_subdir(&args.package) {
            let thread_dir = timestamp_dir.join("thread");
            if !thread_dir.exists() {
                std::fs::create_dir_all(&thread_dir)?;
                println!("Created thread directory: {}", thread_dir.display());
            }
            for (pid, per_pid_series) in thread_time_series {
                if per_pid_series.is_empty() {
                    continue;
                }
                let threads: Vec<ThreadCpuInfo> =
                    per_pid_series.values().flat_map(|v| v.iter().cloned()).collect();
                match cli_utils::export_thread_data_to_csv(thread_dir.clone(), pid, &threads, false) {
                    Ok(filenames) => println!("✓ Final thread data (pid {}) exported to {} CSV files", pid, filenames.len()),
                    Err(e) => println!("Failed to export final thread data for pid {}: {}", pid, e),
                }
                match cli_utils::generate_thread_time_series_chart(thread_dir.clone(), &args.package, pid, per_pid_series) {
                    Ok(name) if !name.is_empty() => println!("✓ Final thread time series chart (pid {}) generated: {}", pid, name),
                    Ok(_) => {}
                    Err(e) => println!("Failed to generate thread chart for pid {}: {}", pid, e),
                }
            }
        }
    }

    let timestamp_dir = match cli_utils::create_timestamp_subdir(&args.package) {
        Ok(d) => d,
        Err(_) => {
            println!("Warning: Could not create timestamp directory.");
            return Ok(());
        }
    };

    // CPU 图表（每 PID 一张 + 汇总）
    let cpu_dir = timestamp_dir.join("cpu");
    let mut cpu_series_for_summary: Vec<cli_utils::CpuSeriesRef<'_>> = Vec::new();
    if args.cpu {
        let mut has_cpu = false;
        for s in pid_stats.values() {
            if s.cpu_data.timestamps.len() > 1 { has_cpu = true; break; }
        }
        if has_cpu {
            std::fs::create_dir_all(&cpu_dir)?;
            println!("Created CPU directory: {}", cpu_dir.display());
            for (pid, s) in pid_stats {
                if s.cpu_data.timestamps.len() <= 1 { continue; }
                println!(
                    "Peak CPU Usage (pid {}): {} at {}",
                    pid.yellow(),
                    format!("{:.1}%", s.cpu_usage).red(),
                    s.cpu_time.map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()).unwrap_or_else(|| "N/A".to_string())
                );
                match cli_utils::generate_cpu_chart(&s.cpu_data.timestamps, &s.cpu_data.process_cpu, pid) {
                    Ok(chart_path) => {
                        if let Some(filename) = chart_path.file_name() {
                            let target = cpu_dir.join(filename);
                            if let Err(e) = std::fs::copy(&chart_path, &target) {
                                println!("Failed to copy CPU chart for pid {}: {}", pid, e);
                            } else {
                                println!("✓ CPU chart generated (pid {}): {}", pid, target.display());
                            }
                        }
                    }
                    Err(e) => println!("Failed to generate CPU chart for pid {}: {}", pid, e),
                }
                let csv_path = cpu_dir.join(format!("cpu_{}_data.csv", pid));
                match cli_utils::export_cpu_data_to_csv(&csv_path, &s.cpu_data.timestamps, &s.cpu_data.process_cpu) {
                    Ok(_) => println!("✓ CPU data exported to CSV (pid {}): {}", pid, csv_path.display()),
                    Err(e) => println!("Failed to export CPU data for pid {}: {}", pid, e),
                }
                cpu_series_for_summary.push((pid.clone(), &s.cpu_data.timestamps, &s.cpu_data.process_cpu));
            }
            if cpu_series_for_summary.len() >= 2 {
                match cli_utils::generate_cpu_summary_chart(&cpu_dir, &args.package, &cpu_series_for_summary) {
                    Ok(p) => println!("✓ CPU summary chart generated: {}", p.display()),
                    Err(e) => println!("Failed to generate CPU summary chart: {}", e),
                }
            }
        }
    }

    // 内存图表（每 PID 一张 + 汇总）
    if args.memory {
        let memory_dir = timestamp_dir.join("memory");
        let mut has_mem = false;
        for s in pid_stats.values() {
            if s.memory_data.timestamps.len() > 1 { has_mem = true; break; }
        }
        if has_mem {
            std::fs::create_dir_all(&memory_dir)?;
            println!("Created memory directory: {}", memory_dir.display());
            let mut mem_series: Vec<(String, &xperf_core::MemoryTimeSeriesData)> = Vec::new();
            for (pid, s) in pid_stats {
                if s.memory_data.timestamps.len() <= 1 { continue; }
                println!(
                    "Peak Memory Usage (pid {}): {} at {}",
                    pid.yellow(),
                    format!("{} KB", s.memory_usage).red(),
                    s.memory_time.map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()).unwrap_or_else(|| "N/A".to_string())
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
                mem_series.push((pid.clone(), &s.memory_data));
            }
            if mem_series.len() >= 2 {
                match generate_memory_summary_chart(&memory_dir, &args.package, &mem_series) {
                    Ok(p) => println!("✓ Memory summary chart generated: {}", p.display()),
                    Err(e) => println!("Failed to generate memory summary chart: {}", e),
                }
            }
        }
    }
    if args.fps {
        let fps_dir = timestamp_dir.join("fps");
        let mut has_fps = false;
        for s in pid_stats.values() {
            if !s.fps_data.timestamps.is_empty() { has_fps = true; break; }
        }
        if has_fps {
            std::fs::create_dir_all(&fps_dir)?;
            for (pid, s) in pid_stats {
                if s.fps_data.timestamps.is_empty() { continue; }
                let csv_path = fps_dir.join(format!("{}_fps_data_pid{}.csv", args.package, pid));
                match cli_utils::export_fps_data_to_csv(&csv_path, &s.fps_data) {
                    Ok(_) => println!("✓ FPS data exported to CSV (pid {}): {}", pid, csv_path.display()),
                    Err(e) => println!("Failed to export FPS data for pid {}: {}", pid, e),
                }
            }
        }
    }
    Ok(())
}

fn generate_memory_charts(
    output_dir: &Path,
    package: &str,
    pid: &str,
    memory_data: &xperf_core::MemoryTimeSeriesData,
) -> Result<Vec<PathBuf>> {
    use plotters::prelude::*;

    let mut chart_paths = Vec::new();
    let file_name = format!("memory_{}_chart.png", pid);
    let path = output_dir.join(file_name);

    if memory_data.timestamps.is_empty() || memory_data.memory_details.is_empty() {
        return Err(anyhow::format_err!("No memory data to chart"));
    }

    let path_clone = path.clone();
    let root = BitMapBackend::new(&path, (1920, 1080)).into_drawing_area();
    root.fill(&WHITE)?;

    let title = format!("Memory Usage - {} (PID: {})", package, pid);
    let (title_area, rest_area) = root.split_vertically(50);
    title_area.titled(&title, ("sans-serif", 20))?;

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
    max_memory *= 1.1;

    let min_time = *memory_data.timestamps.front().unwrap();
    let max_time = *memory_data.timestamps.back().unwrap();

    let memory_types = ["Total PSS", "Java Heap", "Native Heap", "Code", "Stack", "Graphics", "Private Other", "System"];
    let colors = [&RED, &BLUE, &GREEN, &YELLOW, &MAGENTA, &CYAN, &RGBColor(128, 0, 0), &RGBColor(0, 128, 0)];

    let mut chart = ChartBuilder::on(&rest_area)
        .margin(10)
        .margin_right(35)
        .x_label_area_size(40)
        .y_label_area_size(60)
        .build_cartesian_2d(min_time..max_time, 0f32..max_memory)?;

    chart.configure_mesh()
        .x_labels(8)
        .x_label_formatter(&|x| x.format("%H:%M:%S").to_string())
        .y_desc("Memory Usage (KB)")
        .x_desc("Time")
        .draw()?;

    for (i, &memory_type) in memory_types.iter().enumerate() {
        let color = colors[i];
        let values: Vec<(chrono::DateTime<Local>, f32)> = memory_data
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
                (*t, value)
            })
            .collect();
        chart.draw_series(LineSeries::new(values, *color))?
            .label(memory_type.to_string())
            .legend(move |(x, y)| PathElement::new(vec![(x, y), (x + 20, y)], color));
    }
    chart.configure_series_labels()
        .background_style(WHITE.mix(0.8))
        .border_style(BLACK)
        .position(SeriesLabelPosition::UpperRight)
        .margin(10)
        .legend_area_size(35)
        .label_font(("sans-serif", 15))
        .draw()?;

    root.present()?;
    chart_paths.push(path_clone);

    let csv_path = output_dir.join(format!("memory_{}_data.csv", pid));
    if let Ok(file) = std::fs::File::create(&csv_path) {
        let mut writer = std::io::BufWriter::new(file);
        use std::io::Write;
        let _ = writeln!(writer, "Timestamp,Total PSS,Java Heap,Native Heap,Code,Stack,Graphics,Private Other,System");
        for (t, d) in memory_data.timestamps.iter().zip(memory_data.memory_details.iter()) {
            let _ = writeln!(
                writer,
                "{},{},{},{},{},{},{},{},{}",
                t.format("%Y-%m-%d %H:%M:%S%.3f"),
                d.total_pss, d.java_heap, d.native_heap, d.code, d.stack, d.graphics, d.private_other, d.system
            );
        }
        let _ = writer.flush();
        chart_paths.push(csv_path);
    }

    Ok(chart_paths)
}

fn generate_memory_summary_chart(
    output_dir: &Path,
    package: &str,
    series: &[(String, &xperf_core::MemoryTimeSeriesData)],
) -> Result<PathBuf> {
    use plotters::prelude::*;

    let path = output_dir.join("memory_summary.png");
    let path_clone = path.clone();
    let root = BitMapBackend::new(&path, (1920, 1080)).into_drawing_area();
    root.fill(&WHITE)?;

    let (title_area, rest_area) = root.split_vertically(50);
    title_area.titled(&format!("Memory Summary - {} ({} PIDs)", package, series.len()), ("sans-serif", 20))?;

    let mut min_time = None;
    let mut max_time = None;
    let mut max_mem = 1.0f32;
    for (_, md) in series {
        if let (Some(&t0), Some(&tn)) = (md.timestamps.front(), md.timestamps.back()) {
            min_time = Some(min_time.map_or(t0, |m: chrono::DateTime<Local>| m.min(t0)));
            max_time = Some(max_time.map_or(tn, |m: chrono::DateTime<Local>| m.max(tn)));
        }
        for d in &md.memory_details {
            if d.total_pss as f32 > max_mem { max_mem = d.total_pss as f32; }
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
        let pts: Vec<(chrono::DateTime<Local>, f32)> = md.timestamps.iter()
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
    if let Err(e) = monitor_process(&args).await {
        eprintln!("Monitor error: {}", e);
    }
    Ok(())
}
