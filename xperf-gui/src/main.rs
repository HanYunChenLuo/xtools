#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager, State};
use xperf_core::Sampler;

struct AppState {
    running: Arc<Mutex<bool>>,
}

/// 后台采样循环（start_sampling 命令与自动启动共用）。
/// 用 tauri::async_runtime::spawn：setup 回调不在 tokio runtime 上下文中，
/// 直接 tokio::spawn 会 panic（no reactor running）。
fn spawn_sampling(app: tauri::AppHandle, package: String, interval: u64, cpu: bool, memory: bool, running: Arc<Mutex<bool>>) {
    eprintln!("[sampling] 启动: package={} interval={} cpu={} memory={}", package, interval, cpu, memory);
    tauri::async_runtime::spawn(async move {
        let mut sampler = Sampler::new(&package, interval, cpu, memory, false);
        loop {
            if !*running.lock().unwrap() {
                eprintln!("[sampling] 停止：running=false");
                break;
            }
            eprintln!("[sampling] 开始一轮 sample_once...");
            let events = sampler.sample_once().await;
            eprintln!("[sampling] 本轮产生 {} 个事件", events.len());
            for ev in &events {
                eprintln!("[sampling] 事件: {:?}", ev);
                let _ = app.emit("sample", ev);
            }
            sampler.tick_if_needed().await;
        }
    });
}

#[tauri::command]
async fn start_sampling(
    package: String,
    interval: u64,
    cpu: bool,
    memory: bool,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    {
        let mut running = state.running.lock().map_err(|e| e.to_string())?;
        if *running {
            return Err("已在监控中，请先停止".into());
        }
        *running = true;
    }

    spawn_sampling(app, package, interval, cpu, memory, state.running.clone());

    Ok(())
}

#[tauri::command]
fn stop_sampling(state: State<'_, AppState>) -> Result<(), String> {
    let mut running = state.running.lock().map_err(|e| e.to_string())?;
    *running = false;
    Ok(())
}

/// 诊断命令：前端 JS 执行时调用，把消息写到 /tmp/xperf_gui_diag.log。
/// 用于验证前端是否加载、执行到哪一步（webview 无法直接写文件）。
#[tauri::command]
fn diag_log(message: String) -> Result<(), String> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/xperf_gui_diag.log")
        .map_err(|e| e.to_string())?;
    writeln!(f, "[{}] {}", chrono::Local::now().format("%H:%M:%S%.3f"), message)
        .map_err(|e| e.to_string())
}

fn main() {
    // 支持命令行自动启动：xperf-gui --package <pkg> [--interval 1000] [--cpu] [--memory]
    // （便于脚本化/验证；不传参数则手动在前端操作）
    let args: Vec<String> = std::env::args().collect();
    let get_opt = |name: &str| -> Option<String> {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1).cloned())
    };
    let auto_package = get_opt("--package");
    let auto_interval: u64 = get_opt("--interval").and_then(|v| v.parse().ok()).unwrap_or(1000);
    let auto_cpu = args.iter().any(|a| a == "--cpu");
    let auto_memory = args.iter().any(|a| a == "--memory");

    tauri::Builder::default()
        .manage(AppState {
            running: Arc::new(Mutex::new(false)),
        })
        .setup(move |app| {
            if let Some(package) = auto_package.clone() {
                let state = app.state::<AppState>();
                let mut running = state.running.lock().unwrap();
                if !*running {
                    *running = true;
                    drop(running);
                    spawn_sampling(
                        app.handle().clone(),
                        package,
                        auto_interval,
                        auto_cpu || !auto_memory, // 默认至少 CPU
                        auto_memory || !auto_cpu, // 默认至少 memory
                        state.running.clone(),
                    );
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            start_sampling,
            stop_sampling,
            diag_log
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
