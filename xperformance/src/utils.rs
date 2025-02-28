use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use plotters::prelude::*;
use plotters::style::text_anchor::{HPos, Pos, VPos};
use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Once;

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
    fs::create_dir_all(&log_dir)?;
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
            result = Some(path);
        }
    });

    if let Some(path) = result {
        Ok(path)
    } else {
        unsafe {
            if let Some(ref path) = LOG_FILE_PATH {
                Ok(path.clone())
            } else {
                anyhow::bail!("Failed to initialize logging")
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
) -> Result<PathBuf> {
    // Ensure we have data
    if timestamps.is_empty() || process_cpu.is_empty() {
        anyhow::bail!("No CPU data available to generate chart");
    }

    // Create chart directory
    let chart_dir = ensure_log_dir(package)?;
    let timestamp = Local::now().format("%Y%m%d_%H%M%S");
    let filename = format!("cpu_chart_{}.png", timestamp);
    let chart_path = chart_dir.join(filename);

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

    println!("CPU chart saved to: {}", chart_path_display.display());

    Ok(chart_path_display)
}
