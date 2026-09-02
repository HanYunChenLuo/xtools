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
mod alerts;
mod coldstart;
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

    /// Monitor per-core CPU frequency (scaling_cur_freq)
    #[arg(long)]
    freq: bool,

    /// Monitor temperature and thermal throttling status (dumpsys thermalservice, ≥2s period)
    #[arg(long)]
    thermal: bool,

    /// Monitor GPU busy% (kgsl sysfs; auto-disabled when GPU is behind hypervisor)
    #[arg(long)]
    gpu: bool,

    /// Monitor per-process IO rate (KB/s, /proc/<pid>/io)
    #[arg(long)]
    io: bool,

    /// Monitor system-wide network rate (KB/s, /proc/net/dev physical interfaces)
    #[arg(long)]
    net: bool,

    /// Sampling interval in milliseconds (default: 1000, min: 50)
    #[arg(short, long, default_value_t = 1000, value_parser = clap::value_parser!(u64).range(50..))]
    interval: u64,

    /// 阈值告警（逗号分隔，如 cpu>80,mem>500,fps<30,gpu>90；超阈值实时打印告警，退出时输出达标报告）
    #[arg(long, value_delimiter = ',')]
    threshold: Vec<String>,

    /// 冷启动时间测量：am start -W 指定 Activity（如 .MainActivity），输出 TotalTime/WaitTime/Complete
    #[arg(long)]
    cold_start: Option<String>,
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

    let flags = metric_flags(args);
    if !flags.any() {
        println!("No monitoring options selected. Use --cpu/--memory/--fps/--freq/--thermal/--gpu/--io/--net");
        return Ok(());
    }

    // 统一走设备端 agent 采样（无 adb 轮询路径）
    monitor_process_agent(args, flags).await
}

/// 冷启动测量（独立于采样，先执行再开始监控）
fn run_cold_start(args: &Args) {
    if let Some(ref activity) = args.cold_start {
        println!("{}", "========== 冷启动测量 ==========".green().bold());
        match coldstart::measure(&args.package, activity) {
            Ok(r) => println!("{}", r.summary().cyan()),
            Err(e) => println!("{}", format!("冷启动测量失败: {}", e).yellow()),
        }
        println!("{}", "================================".green().bold());
    }
}

/// 阈值检查：超阈值时实时打印告警并记录统计
fn check_threshold(
    thresholds: &[alerts::Threshold],
    stats: &Arc<std::sync::Mutex<alerts::AlertStats>>,
    metric: &str,
    value: f32,
    t: &DateTime<Local>,
) {
    let triggered = alerts::check_value(thresholds, metric, value);
    if triggered.is_empty() { return; }
    let time_str = t.format("%H:%M:%S").to_string();
    for t in triggered {
        println!("{}", format!("[{}] ⚠️ 告警: {} {:.1} {} {}", time_str, t.metric, value, t.op, t.value).red().bold());
        stats.lock().unwrap().record(&t.raw, value, &time_str);
    }
}

fn metric_flags(args: &Args) -> xperf_core::MetricFlags {
    xperf_core::MetricFlags {
        cpu: args.cpu,
        memory: args.memory,
        fps: args.fps,
        freq: args.freq,
        thermal: args.thermal,
        gpu: args.gpu,
        io: args.io,
        net: args.net,
    }
}

/// 统一采样路径：设备端 agent 常驻采样 + exec-out 事件流。
/// 输出策略：interval ≥ 500ms 逐条详细打印（低频率，同旧轮询模式的信息量）；
/// < 500ms 时按 ~1s 聚合打印（逐条会刷屏），全量明细均在退出 CSV 中。
async fn monitor_process_agent(args: &Args, flags: xperf_core::MetricFlags) -> Result<(), Box<dyn std::error::Error>> {
    use xperf_core::agent::{self, AgentEvent};

    let bin = agent::ensure_agent_built()?;
    agent::deploy_agent(&bin)?;
    let platform = xperf_core::detect_platform_live();
    println!("平台: {} ({})", platform.name(), platform.description());
    let mut stream = agent::spawn_agent(Some(&args.package), args.interval, flags, Some(&*platform))?;
    println!("agent 已部署并启动（间隔 {}ms）", args.interval);

    let verbose = args.interval >= 500;
    let thresholds = alerts::parse_thresholds(&args.threshold);
    let alert_stats = Arc::new(std::sync::Mutex::new(alerts::AlertStats::default()));
    if !thresholds.is_empty() {
        println!("阈值规则: {}", thresholds.iter().map(|t| &t.raw).cloned().collect::<Vec<_>>().join(", "));
    }

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
        io: Option<(f32, f32)>,          // 最新 (读 KB/s, 写 KB/s)
        gpumem: Option<(f32, f32)>,      // 最新 (进程 MB, 整机 MB)
        gpuproc: Option<f32>,            // 最新 GPU busy%（QNX 路径每进程）
    }
    let mut aggs: std::collections::HashMap<u32, Agg> = Default::default();
    // 设备级指标的最新值（聚合模式每秒打印一次）
    let mut latest_freq: Vec<u64> = Vec::new(); // KHz
    let mut latest_net: Option<(f32, f32)> = None;
    let mut latest_gpu: Option<(f32, u32)> = None;
    let mut latest_temp: Option<(i32, Vec<cli_utils::SensorReading>)> = None;
    let mut last_print = Instant::now();
    // 边采边落盘：样本到达即追加 CSV，崩溃只丢未 flush 尾部
    let mut csv = cli_utils::CsvStream::default();
    // B 类指标时序（退出图表用；超 2×CHART_SERIES_CAP 每 2 取 1 抽稀）
    let mut extra = cli_utils::ExtraSeries::default();

    while running.load(Ordering::SeqCst) {
        let ev = match stream.next_event() {
            Ok(Some(ev)) => ev,
            // EOF/读错误：adb 长连接断开或 agent 退出 → 等待设备恢复并重连，采样状态保留
            Ok(None) | Err(_) => {
                println!("{}", "连接断开，等待设备恢复…（Ctrl-C 退出）".yellow());
                let r = running.clone();
                match agent::reconnect_agent(
                    Some(&args.package), args.interval, flags, Some(&*platform),
                    &move || r.load(Ordering::SeqCst),
                ) {
                    Some(s) => {
                        stream = s;
                        println!("{}", "已重连，恢复采样".green());
                        continue;
                    }
                    None => break, // Ctrl-C
                }
            }
        };
        let ev = match ev {
            Ok(ev) => ev,
            Err(e) => {
                println!("{}", format!("协议解析失败: {}", e).yellow());
                continue;
            }
        };
        match ev {
            AgentEvent::Hello { ncores, maxkhz } => {
                let maxghz: Vec<String> = maxkhz.iter().map(|k| format!("{:.2}", *k as f32 / 1e6)).collect();
                println!("设备 {} 核（最大频率 GHz: [{}]）", ncores, maxghz.join(", "));
            }
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
                // top_threads 无读者（线程数据走下方 thread_time_series + 流式 CSV），不存
                s.cpu_data.add_data_point(t, cpu, Vec::new());
                csv.cpu_row(&args.package, pid, t, cpu);
                if s.cpu_time.is_none() || cpu > s.cpu_usage {
                    s.cpu_usage = cpu;
                    s.cpu_time = Some(t);
                }
                check_threshold(&thresholds, &alert_stats, "cpu", cpu, &t);
                if args.thread {
                    let per_pid = thread_time_series.entry(pid.to_string()).or_default();
                    for t2 in threads {
                        if t2.cpu_usage > 0.0 {
                            if let Ok(tid) = t2.tid.parse::<u32>() {
                                csv.thread_row(&args.package, pid, tid, &t2.name, t, t2.cpu_usage);
                            }
                            let series = per_pid.entry(t2.name.clone()).or_default();
                            // 与 xperf-core 时序同策略：超 2×CAP 每 2 取 1 抽稀，长测内存有界
                            if series.len() >= 2 * xperf_core::CHART_SERIES_CAP {
                                let mut i = 0;
                                series.retain(|_| {
                                    let keep = i % 2 == 0;
                                    i += 1;
                                    keep
                                });
                            }
                            series.push(t2);
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
                let details = xperf_core::MemoryDetails {
                    java_heap: java,
                    native_heap: native,
                    code,
                    stack,
                    graphics: gfx,
                    private_other: other,
                    system: sys,
                    total_pss: pss,
                };
                s.memory_data.add_data_point(t, details.clone());
                csv.mem_row(&args.package, pid, t, &details);
                if s.memory_time.is_none() || pss > s.memory_usage {
                    s.memory_usage = pss;
                    s.memory_time = Some(t);
                }
                check_threshold(&thresholds, &alert_stats, "mem", pss as f32 / 1024.0, &t); // MB
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
                csv.fps_row(&args.package, pid, t, &layer, fps, jank);
                check_threshold(&thresholds, &alert_stats, "fps", fps, &t);
                let a = aggs.entry(pid).or_default();
                a.fps = Some((layer, fps, jank));
            }
            AgentEvent::Freq { ts, khz } => {
                let Some(t) = DateTime::from_timestamp_millis(ts as i64)
                    .map(|t| t.with_timezone(&Local))
                else {
                    continue;
                };
                let mhz: Vec<f32> = khz.iter().map(|k| *k as f32 / 1000.0).collect();
                if verbose {
                    let cells: Vec<String> = mhz.iter().map(|m| format!("{:.0}", m)).collect();
                    println!("[{}] CPU Freq MHz: [{}]", t.format("%H:%M:%S"), cells.join(", ").blue());
                }
                csv.freq_row(&args.package, t, &mhz);
                extra.push_freq(t, mhz.clone());
                latest_freq = khz;
            }
            AgentEvent::Io { ts, pid, r, w, dr, dw } => {
                let Some(t) = DateTime::from_timestamp_millis(ts as i64)
                    .map(|t| t.with_timezone(&Local))
                else {
                    continue;
                };
                if verbose {
                    println!(
                        "[{}] IO: read {} KB/s (disk {}), write {} KB/s (disk {}) [pid {}]",
                        t.format("%H:%M:%S"),
                        format!("{:.1}", r).blue(),
                        format!("{:.1}", dr).blue(),
                        format!("{:.1}", w).yellow(),
                        format!("{:.1}", dw).yellow(),
                        pid.to_string().yellow()
                    );
                }
                csv.io_row(&args.package, pid, t, r, w, dr, dw);
                extra.push_io(pid, t, r, w, dr, dw);
                aggs.entry(pid).or_default().io = Some((r, w));
            }
            AgentEvent::Net { ts, rx, tx } => {
                let Some(t) = DateTime::from_timestamp_millis(ts as i64)
                    .map(|t| t.with_timezone(&Local))
                else {
                    continue;
                };
                if verbose {
                    println!(
                        "[{}] Net（整机）: RX {} KB/s, TX {} KB/s",
                        t.format("%H:%M:%S"),
                        format!("{:.1}", rx).blue(),
                        format!("{:.1}", tx).yellow()
                    );
                }
                csv.net_row(&args.package, t, rx, tx);
                extra.push_net(t, rx, tx);
                latest_net = Some((rx, tx));
            }
            AgentEvent::Gpu { ts, busy, util, mhz, maxmhz } => {
                let Some(t) = DateTime::from_timestamp_millis(ts as i64)
                    .map(|t| t.with_timezone(&Local))
                else {
                    continue;
                };
                if verbose {
                    // QNX 路径带 util/maxmhz；kgsl 路径 util=0
                    let extra_info = if maxmhz > 0 {
                        format!("util {}%, {} / {} MHz", format!("{:.1}", util).blue(), mhz, maxmhz)
                    } else {
                        format!("@ {} MHz", mhz)
                    };
                    println!(
                        "[{}] GPU: busy {}% ({})",
                        t.format("%H:%M:%S"),
                        format!("{:.1}", busy).blue(),
                        extra_info
                    );
                }
                csv.gpu_row(&args.package, t, busy, util, mhz, maxmhz);
                extra.push_gpu(t, busy, util, mhz);
                latest_gpu = Some((busy, mhz));
                check_threshold(&thresholds, &alert_stats, "gpu", busy, &t);
            }
            AgentEvent::GpuProc { ts, pid, busy } => {
                let Some(t) = DateTime::from_timestamp_millis(ts as i64)
                    .map(|t| t.with_timezone(&Local))
                else {
                    continue;
                };
                if verbose {
                    println!(
                        "[{}] GPU busy: {}% [pid {}]",
                        t.format("%H:%M:%S"),
                        format!("{:.1}", busy).blue(),
                        pid.to_string().yellow()
                    );
                }
                csv.gpuproc_row(&args.package, pid, t, busy);
                extra.push_gpuproc(pid, t, busy);
                aggs.entry(pid).or_default().gpuproc = Some(busy);
            }
            AgentEvent::GpuMem { ts, pid, bytes, global } => {
                let Some(t) = DateTime::from_timestamp_millis(ts as i64)
                    .map(|t| t.with_timezone(&Local))
                else {
                    continue;
                };
                let mb = bytes as f32 / 1e6;
                let global_mb = global as f32 / 1e6;
                if verbose {
                    println!(
                        "[{}] GPU Mem: {} MB（整机 {} MB）[pid {}]",
                        t.format("%H:%M:%S"),
                        format!("{:.1}", mb).blue(),
                        format!("{:.0}", global_mb).blue(),
                        pid.to_string().yellow()
                    );
                }
                csv.gpumem_row(&args.package, pid, t, mb, global_mb);
                extra.push_gpumem(pid, t, mb, global_mb);
                aggs.entry(pid).or_default().gpumem = Some((mb, global_mb));
            }
            AgentEvent::Temp { ts, status, sensors } => {
                let Some(t) = DateTime::from_timestamp_millis(ts as i64)
                    .map(|t| t.with_timezone(&Local))
                else {
                    continue;
                };
                if verbose {
                    let cells: Vec<String> =
                        sensors.iter().map(|(name, _, v)| format!("{}: {:.1}°C", name, v)).collect();
                    println!(
                        "[{}] Temp: {} (thermal status: {})",
                        t.format("%H:%M:%S"),
                        cells.join(", ").blue(),
                        if status >= 0 { status.to_string().red() } else { "未知".into() }
                    );
                }
                csv.temp_row(&args.package, t, status, &sensors);
                extra.push_temp(t, status, &sensors);
                latest_temp = Some((status, sensors));
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
                if let Some((r, w)) = a.io {
                    parts.push(format!("IO R {} / W {} KB/s", format!("{:.1}", r).blue(), format!("{:.1}", w).yellow()));
                }
                if let Some((mb, gmb)) = a.gpumem {
                    parts.push(format!("GPU Mem {} MB（整机 {:.0}）", format!("{:.1}", mb).blue(), gmb));
                }
                if let Some(busy) = a.gpuproc {
                    parts.push(format!("GPU busy {}%", format!("{:.1}", busy).blue()));
                }
                if !parts.is_empty() {
                    println!("[{}] {} (pid: {})", ts, parts.join(" | "), pid.to_string().yellow());
                }
                a.n = 0;
                a.sum = 0.0;
                a.max = 0.0;
            }
            // 设备级指标（不区分 PID）：一行汇总
            let mut dev_parts = Vec::new();
            if !latest_freq.is_empty() {
                let cells: Vec<String> = latest_freq.iter().map(|k| format!("{:.0}", *k as f32 / 1000.0)).collect();
                dev_parts.push(format!("Freq MHz [{}]", cells.join(",").blue()));
            }
            if let Some((rx, tx)) = latest_net {
                dev_parts.push(format!("Net RX {} / TX {} KB/s", format!("{:.1}", rx).blue(), format!("{:.1}", tx).yellow()));
            }
            if let Some((busy, mhz)) = latest_gpu {
                dev_parts.push(format!("GPU {}% @ {}MHz", format!("{:.1}", busy).blue(), mhz));
            }
            if let Some((status, sensors)) = &latest_temp {
                let cells: Vec<String> = sensors.iter().map(|(n, _, v)| format!("{}: {:.1}°C", n, v)).collect();
                dev_parts.push(format!("Temp {} (status {})", cells.join(", ").blue(), status));
            }
            if !dev_parts.is_empty() {
                println!("[{}] {}", ts, dev_parts.join(" | "));
            }
        }
    }

    drop(stream); // 杀掉设备端 agent（Drop 里 kill）
    generate_final_outputs(args, &pid_stats, &thread_time_series, &extra)?;
    println!("Process Restarts: {}", restart_count.to_string().red());
    // 验证报告
    if !thresholds.is_empty() {
        let stats = alert_stats.lock().unwrap();
        println!("{}", alerts::generate_report(&thresholds, &stats));
    }
    Ok(())
}

/// B 类指标退出图表：每指标一张多序列折线（freq 每核一条 / temp 每传感器一条 / io 每 PID 读写两条）。
fn generate_extra_charts(timestamp_dir: &Path, package: &str, extra: &cli_utils::ExtraSeries) {
    if extra.freq.len() > 1 {
        let dir = timestamp_dir.join("freq");
        let _ = std::fs::create_dir_all(&dir);
        let ncores = extra.freq.back().map(|(_, v)| v.len()).unwrap_or(0);
        let series: Vec<cli_utils::NamedSeries> = (0..ncores)
            .map(|c| {
                (
                    format!("cpu{}", c),
                    extra.freq.iter().map(|(t, v)| (*t, v.get(c).copied().unwrap_or(0.0))).collect(),
                )
            })
            .collect();
        match cli_utils::generate_multi_line_chart(&dir.join("freq_chart.png"), &format!("CPU Frequency - {}", package), "MHz", &series) {
            Ok(p) => println!("✓ Freq chart generated: {}", p.display()),
            Err(e) => println!("Failed to generate freq chart: {}", e),
        }
    }
    if extra.temp.len() > 1 {
        let dir = timestamp_dir.join("thermal");
        let _ = std::fs::create_dir_all(&dir);
        // 传感器集合以出现过的并集为准（HAL 传感器列表运行中可能变化）
        let mut names: Vec<String> = Vec::new();
        for (_, _, sensors) in &extra.temp {
            for (n, _) in sensors {
                if !names.contains(n) {
                    names.push(n.clone());
                }
            }
        }
        let series: Vec<cli_utils::NamedSeries> = names
            .iter()
            .map(|n| {
                let pts = extra
                    .temp
                    .iter()
                    .filter_map(|(t, _, sensors)| sensors.iter().find(|(sn, _)| sn == n).map(|(_, v)| (*t, *v)))
                    .collect();
                (n.clone(), pts)
            })
            .collect();
        match cli_utils::generate_multi_line_chart(&dir.join("thermal_chart.png"), &format!("Temperature - {}", package), "°C", &series) {
            Ok(p) => println!("✓ Thermal chart generated: {}", p.display()),
            Err(e) => println!("Failed to generate thermal chart: {}", e),
        }
    }
    if extra.gpu.len() > 1 {
        let dir = timestamp_dir.join("gpu");
        let _ = std::fs::create_dir_all(&dir);
        let busy: Vec<(DateTime<Local>, f32)> = extra.gpu.iter().map(|(t, b, _, _)| (*t, *b)).collect();
        let mut series = vec![("busy%".to_string(), busy)];
        // QNX 路径带 util（busy 按频率折算）；kgsl 路径全 0，不画
        if extra.gpu.iter().any(|(_, _, u, _)| *u > 0.0) {
            series.push(("util%".to_string(), extra.gpu.iter().map(|(t, _, u, _)| (*t, *u)).collect()));
        }
        match cli_utils::generate_multi_line_chart(&dir.join("gpu_chart.png"), &format!("GPU Busy - {}", package), "%", &series) {
            Ok(p) => println!("✓ GPU chart generated: {}", p.display()),
            Err(e) => println!("Failed to generate gpu chart: {}", e),
        }
    }
    // QNX 路径每进程 GPU busy
    if extra.gpuproc.values().any(|v| v.len() > 1) {
        let dir = timestamp_dir.join("gpu");
        let _ = std::fs::create_dir_all(&dir);
        let mut pids: Vec<u32> = extra.gpuproc.keys().copied().collect();
        pids.sort_unstable();
        let series: Vec<cli_utils::NamedSeries> = pids
            .iter()
            .map(|pid| (format!("PID {}", pid), extra.gpuproc[pid].iter().map(|(t, b)| (*t, *b)).collect()))
            .collect();
        match cli_utils::generate_multi_line_chart(&dir.join("gpu_proc_chart.png"), &format!("GPU Busy per PID - {}", package), "%", &series) {
            Ok(p) => println!("✓ GPU proc chart generated: {}", p.display()),
            Err(e) => println!("Failed to generate gpu proc chart: {}", e),
        }
    }
    // GPU 显存（--gpu 降级路径）：每 PID 一条线 + 整机一条
    if extra.gpumem.values().any(|v| v.len() > 1) {
        let dir = timestamp_dir.join("gpumem");
        let _ = std::fs::create_dir_all(&dir);
        let mut series: Vec<cli_utils::NamedSeries> = Vec::new();
        let mut pids: Vec<u32> = extra.gpumem.keys().copied().collect();
        pids.sort_unstable();
        let mut global: Option<cli_utils::SeriesPoints> = None;
        for pid in pids {
            let samples = &extra.gpumem[&pid];
            series.push((format!("PID {}", pid), samples.iter().map(|(t, mb, _)| (*t, *mb)).collect()));
            if global.is_none() {
                global = Some(samples.iter().map(|(t, _, g)| (*t, *g)).collect());
            }
        }
        if let Some(g) = global {
            series.push(("global".to_string(), g));
        }
        match cli_utils::generate_multi_line_chart(&dir.join("gpumem_chart.png"), &format!("GPU Memory - {}", package), "MB", &series) {
            Ok(p) => println!("✓ GPU mem chart generated: {}", p.display()),
            Err(e) => println!("Failed to generate gpu mem chart: {}", e),
        }
    }
    if extra.net.len() > 1 {
        let dir = timestamp_dir.join("net");
        let _ = std::fs::create_dir_all(&dir);
        let rx: Vec<(DateTime<Local>, f32)> = extra.net.iter().map(|(t, r, _)| (*t, *r)).collect();
        let tx: Vec<(DateTime<Local>, f32)> = extra.net.iter().map(|(t, _, x)| (*t, *x)).collect();
        let series = vec![("RX KB/s".to_string(), rx), ("TX KB/s".to_string(), tx)];
        match cli_utils::generate_multi_line_chart(&dir.join("net_chart.png"), &format!("Network（整机）- {}", package), "KB/s", &series) {
            Ok(p) => println!("✓ Net chart generated: {}", p.display()),
            Err(e) => println!("Failed to generate net chart: {}", e),
        }
    }
    for (pid, samples) in &extra.io {
        if samples.len() <= 1 {
            continue;
        }
        let dir = timestamp_dir.join("io");
        let _ = std::fs::create_dir_all(&dir);
        let r: Vec<(DateTime<Local>, f32)> = samples.iter().map(|(t, r, _, _, _)| (*t, *r)).collect();
        let w: Vec<(DateTime<Local>, f32)> = samples.iter().map(|(t, _, w, _, _)| (*t, *w)).collect();
        let series = vec![("read KB/s".to_string(), r), ("write KB/s".to_string(), w)];
        match cli_utils::generate_multi_line_chart(
            &dir.join(format!("io_{}_chart.png", pid)),
            &format!("IO - {} (PID: {})", package, pid),
            "KB/s",
            &series,
        ) {
            Ok(p) => println!("✓ IO chart generated (pid {}): {}", pid, p.display()),
            Err(e) => println!("Failed to generate io chart for pid {}: {}", pid, e),
        }
    }
}

fn generate_final_outputs(
    args: &Args,
    pid_stats: &std::collections::HashMap<String, xperf_core::PidStats>,
    thread_time_series: &std::collections::HashMap<String, std::collections::HashMap<String, Vec<ThreadCpuInfo>>>,
    extra: &cli_utils::ExtraSeries,
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
                // 线程 CSV 已在采样时流式落盘，这里只出时序图
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
                // CPU CSV 已在采样时流式落盘（cpu/cpu_<pid>_data.csv）
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
                            println!("✓ Memory chart generated (pid {}): {}", pid, path.display());
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
    // FPS CSV 已在采样时流式落盘（fps/<pkg>_fps_data_pid<pid>.csv），CLI 不出 FPS 图表
    // B 类指标退出图表（CSV 同样已流式落盘在各自子目录）
    generate_extra_charts(&timestamp_dir, &args.package, extra);
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

    // 内存 CSV 已在采样时流式落盘（memory/memory_<pid>_data.csv）
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
    run_cold_start(&args);
    if let Err(e) = monitor_process(&args).await {
        eprintln!("Monitor error: {}", e);
    }
    Ok(())
}
