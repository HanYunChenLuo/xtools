#![deny(warnings)]
use anyhow::{Context, Result};
use chrono::{DateTime, Local, Timelike};
use clap::Parser;
use colored::*;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::time::Instant;

mod utils;
use utils as cli_utils;

use xperf_core::{SampleEvent, Sampler};
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

    /// 设备端采样模式（低间隔场景：agent 常驻设备读 /proc，流式回传）。
    /// interval < 500ms 时自动启用。注意：agent 模式内存只有 Pss/Rss（smaps_rollup），
    /// 且不支持 --fps（帧缓冲方案在 1s 轮询下已是帧级分辨率，无需提速）。
    #[arg(long)]
    agent: bool,

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

async fn monitor_adb_connection(running: Arc<AtomicBool>) {
    loop {
        if !running.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        if !xperf_core::utils::check_adb_connection() {
            eprintln!("{}", "ADB device disconnected!".red().bold());
            running.store(false, Ordering::SeqCst);
            return;
        }
    }
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

    // 低间隔场景走设备端 agent（adb 轮询单轮开销就超过间隔本身）
    let use_agent = args.agent || args.interval < xperf_core::agent::AGENT_INTERVAL_THRESHOLD_MS;
    if use_agent {
        if args.fps {
            println!("{}", "⚠️ agent 模式不支持 --fps（帧缓冲方案在 1s 轮询下已是帧级分辨率），FPS 项被忽略".yellow());
        }
        if args.interval >= xperf_core::agent::AGENT_INTERVAL_THRESHOLD_MS {
            println!("已显式启用 --agent 模式");
        } else {
            println!("间隔 {}ms < 500ms，自动切换到 agent 模式（设备端采样）", args.interval);
        }
        return monitor_process_agent(args).await;
    }

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
        xperf_core::utils::set_interrupt_flag();
        println!("\n程序正在退出...");
    })?;

    let adb_monitor = {
        let running = running.clone();
        tokio::spawn(async move {
            monitor_adb_connection(running).await;
        })
    };

    let start_time = Instant::now();
    let mut sample_count: u64 = 0;

    let mut sampler = Sampler::new(&args.package, args.interval, args.cpu, args.memory, args.thread, args.fps);

    // 初始枚举进程
    let initial_processes = xperf_core::get_all_processes(&args.package)?;
    for p in &initial_processes {
        println!("Monitoring PID {} (start: {})", p.pid.yellow(), p.start_time.blue());
    }
    // 把初始进程喂给 sampler（通过首轮 sample_once 自动发现）
    drop(initial_processes);

    let mut last_chart_hour = Local::now().hour() as i32;

    // 内存图表节流
    const MEMORY_CHART_INTERVAL: usize = 30;
    let mut last_memory_chart_sample_count: usize = 0;
    let mut memory_sample_count: usize = 0;

    // 每个线程的时间序列数据，按 pid → thread_name → Vec
    let mut thread_time_series: std::collections::HashMap<
        String,
        std::collections::HashMap<String, Vec<ThreadCpuInfo>>,
    > = std::collections::HashMap::new();

    if args.cpu {
        println!("CPU monitoring enabled, but not creating files until actual thread data is available");
    }

    while running.load(Ordering::SeqCst) {
        let current_time = Instant::now();
        let elapsed = current_time.duration_since(start_time);
        let should_be_at_sample = elapsed.as_millis() as u64 / args.interval;

        if should_be_at_sample > sample_count {
            let samples_to_skip = should_be_at_sample - sample_count - 1;
            if samples_to_skip > 0 {
                println!(
                    "{}",
                    format!("⚠️ System too slow! Skipping {} sample(s) to maintain timing.", samples_to_skip).yellow()
                );
            }
            sample_count = should_be_at_sample;
        }

        let events = sampler.sample_once().await;

        for event in events {
            match event {
                SampleEvent::PidDiscovered { pid, start_time } => {
                    println!("Monitoring PID {} (start: {})", pid.yellow(), start_time.blue());
                }
                SampleEvent::PidDisappeared { pid } => {
                    println!("\n[{}] PID {} disappeared.", Local::now().format("%H:%M:%S"), pid.yellow());
                }
                SampleEvent::CpuUpdate { pid, timestamp, process_cpu, threads } => {
                    println!(
                        "[{}] Process CPU: {}% (pid: {})",
                        timestamp.format("%H:%M:%S"),
                        format!("{:.1}", process_cpu).blue(),
                        pid.yellow()
                    );
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
                    if args.thread {
                        let per_pid = thread_time_series.entry(pid.clone()).or_default();
                        for thread in &threads {
                            if thread.cpu_usage > 0.0 {
                                per_pid.entry(thread.name.clone()).or_default().push(thread.clone());
                            }
                        }
                    }
                }
                SampleEvent::MemoryUpdate { pid, timestamp, total_pss, details } => {
                    println!(
                        "[{}] Memory Usage: {} KB (Java: {}, Native: {}, Code: {}, Graphics: {}) [pid {}]",
                        timestamp.format("%H:%M:%S"),
                        total_pss.to_string().blue(),
                        details.java_heap,
                        details.native_heap,
                        details.code,
                        details.graphics,
                        pid.yellow()
                    );
                    memory_sample_count += 1;
                    // 定期生成内存图表
                    if memory_sample_count >= 5
                        && memory_sample_count - last_memory_chart_sample_count >= MEMORY_CHART_INTERVAL
                    {
                        last_memory_chart_sample_count = memory_sample_count;
                        generate_scheduled_memory_charts(args);
                    }
                }
                SampleEvent::NoProcess { error } => {
                    println!("No process found for package: {}", error);
                }
                SampleEvent::FpsUpdate { pid, timestamp, layer, fps, frame_count, jank_count } => {
                    println!(
                        "[{}] FPS: {} (jank: {}, frames: {}, layer: {}) [pid {}]",
                        timestamp.format("%H:%M:%S"),
                        format!("{:.1}", fps).blue(),
                        jank_count.to_string().red(),
                        frame_count,
                        layer.green(),
                        pid.yellow()
                    );
                }
                SampleEvent::SampleError { pid, stage, error } => {
                    println!("Error sampling {} for pid {:?}: {}", stage, pid, error);
                }
            }
        }

        // 整点 CPU 图表
        let now = Local::now();
        let current_hour = now.hour() as i32;
        if current_hour != last_chart_hour && args.cpu {
            last_chart_hour = current_hour;
            generate_scheduled_cpu_charts(args, &sampler);
        }

        // 节拍：CPU 采样已 sleep；--memory 单跑时需自行维持间隔
        sampler.tick_if_needed().await;
    }

    let _ = adb_monitor.await;

    // 退出时生成最终图表
    generate_final_outputs(args, sampler.pid_stats(), &thread_time_series)?;

    println!("Process Restarts: {}", sampler.restart_count().to_string().red());
    Ok(())
}

/// agent 模式（低间隔采样）：设备端常驻采样器 + exec-out 事件流。
/// 与轮询模式的差异：内存只有 Pss/Rss（smaps_rollup）；不支持 --fps；
/// 终端按 ~1s 聚合打印（50ms 逐条会刷屏），全量明细在退出 CSV 中。
async fn monitor_process_agent(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    use xperf_core::agent::{self, AgentEvent};

    let bin = agent::ensure_agent_built()?;
    agent::deploy_agent(&bin)?;
    let mut stream = agent::spawn_agent(Some(&args.package), args.interval, args.cpu, args.memory)?;
    println!("agent 已部署并启动（间隔 {}ms）", args.interval);

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

    // 终端聚合输出：每 PID 累计窗口内样本，每秒打印一行
    #[derive(Default)]
    struct Agg {
        n: u32,
        sum: f32,
        max: f32,
        pss: u64,
        rss: u64,
        has_mem: bool,
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
            AgentEvent::Mem { ts, pid, pss, rss } => {
                let Some(t) = DateTime::from_timestamp_millis(ts as i64)
                    .map(|t| t.with_timezone(&Local))
                else {
                    continue;
                };
                let s = pid_stats.entry(pid.to_string()).or_default();
                s.active = true;
                // agent 模式高频率数据：直接推入时序（绕过轮询模式的 300 点上限，保 CSV 完整）
                s.memory_data.timestamps.push_back(t);
                s.memory_data.memory_details.push_back(xperf_core::MemoryDetails {
                    total_pss: pss,
                    ..Default::default()
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
            AgentEvent::Exit { pid } => {
                restart_count += 1;
                if let Some(s) = pid_stats.get_mut(&pid.to_string()) {
                    s.active = false;
                }
                println!("[{}] PID {} 已退出", Local::now().format("%H:%M:%S"), pid.to_string().yellow());
            }
            AgentEvent::Noproc => {} // 无进程期间 agent 每秒报一次，聚合行自然体现为无数据
            AgentEvent::Err { msg } => println!("{}", format!("agent: {}", msg).yellow()),
        }

        // 每秒聚合打印一次（高频率逐条打印会刷屏）
        if last_print.elapsed() >= std::time::Duration::from_secs(1) {
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

fn generate_scheduled_memory_charts(args: &Args) {
    if let Ok(timestamp_dir) = cli_utils::create_timestamp_subdir(&args.package) {
        let memory_dir = timestamp_dir.join("memory");
        if !memory_dir.exists() {
            if let Err(e) = std::fs::create_dir_all(&memory_dir) {
                println!("Failed to create memory directory: {}", e);
                return;
            }
        }
        // 注：此处无法访问 sampler 的 pid_stats（sampler 在 monitor_process 作用域），
        // 调度图表改为在退出时统一生成；运行中调度内存图表暂略。
        let _ = memory_dir;
    }
}

fn generate_scheduled_cpu_charts(args: &Args, _sampler: &Sampler) {
    let now = Local::now();
    println!(
        "{} Generating scheduled CPU chart at {}...",
        now.format("%H:%M:%S").to_string().blue(),
        format!("{}:00", now.hour()).green()
    );
    // 运行中 CPU 图表：写 /tmp，仅打印路径（与原行为一致）
    // 实际每 PID 图表在退出时生成；此处仅保留提示
    let _ = args;
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
