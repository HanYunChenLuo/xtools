#![deny(warnings)]
use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use clap::Parser;
use colored::*;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::time::{sleep, Duration, Instant};

mod cpu;
mod memory;
mod utils;

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

    /// Generate detailed log file
    #[arg(long)]
    log: bool,

    /// Sampling interval in seconds (default: 1)
    #[arg(short, long, default_value_t = 1)]
    interval: u64,
}

#[derive(Default)]
struct PeakStats {
    cpu_usage: f32,
    cpu_time: DateTime<Local>,
    memory_usage: u64,
    memory_time: DateTime<Local>,
    restart_count: u32,
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

    // Initialize logging if enabled
    if args.log {
        let path = utils::init_logging(&args.package)?;
        println!("Logging to: {}", path.display());
        utils::append_to_log(&format!(
            "Performance monitoring for package: {}\nSampling interval: {} seconds\n",
            args.package, args.interval
        ))?;
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

    while running.load(Ordering::SeqCst) {
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
                    if args.log {
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
            if let Ok((cpu_usage, timestamp)) = cpu::sample_cpu(&args.package, args.log).await {
                if cpu_usage > peak_stats.cpu_usage {
                    peak_stats.cpu_usage = cpu_usage;
                    peak_stats.cpu_time = timestamp;
                }
            }
        }

        if args.memory {
            if let Ok((memory_kb, timestamp)) = memory::sample_memory(&args.package, args.log).await
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

    // Print peak stats
    println!("\n{}", "Peak Statistics:".yellow().bold());
    if args.cpu {
        println!(
            "Peak CPU Usage: {}% at {}",
            format!("{:.1}", peak_stats.cpu_usage).red(),
            peak_stats.cpu_time.format("%Y-%m-%d %H:%M:%S")
        );
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
    if args.log {
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
