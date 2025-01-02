use anyhow::{Context, Result};
use chrono::Local;
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
