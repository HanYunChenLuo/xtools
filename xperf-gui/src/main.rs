#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager, State};
use xperf_core::{SampleEvent, Sampler};

/// 事件的单行摘要（替代 {:?} 全量 Debug——CpuUpdate 含全部线程列表，每轮数千字符）
fn brief_event(ev: &SampleEvent) -> String {
    match ev {
        SampleEvent::PidDiscovered { pid, start_time } => {
            format!("PidDiscovered pid={} start={}", pid, start_time)
        }
        SampleEvent::PidDisappeared { pid } => format!("PidDisappeared pid={}", pid),
        SampleEvent::CpuUpdate { pid, process_cpu, threads, .. } => {
            format!("CpuUpdate pid={} cpu={:.1}% threads={}", pid, process_cpu, threads.len())
        }
        SampleEvent::MemoryUpdate { pid, total_pss, .. } => {
            format!("MemoryUpdate pid={} total_pss={}KB", pid, total_pss)
        }
        SampleEvent::FpsUpdate { pid, fps, jank_count, layer, .. } => {
            format!("FpsUpdate pid={} fps={:.1} jank={} layer={}", pid, fps, jank_count, layer)
        }
        SampleEvent::NoProcess { error } => format!("NoProcess: {}", error),
        SampleEvent::SampleError { pid, stage, error } => {
            format!("SampleError pid={:?} stage={}: {}", pid, stage, error)
        }
    }
}

struct AppState {
    running: Arc<Mutex<bool>>,
}

/// 后台采样循环（start_sampling 命令与自动启动共用）。
/// 用 tauri::async_runtime::spawn：setup 回调不在 tokio runtime 上下文中，
/// 直接 tokio::spawn 会 panic（no reactor running）。
fn spawn_sampling(app: tauri::AppHandle, package: String, interval: u64, cpu: bool, memory: bool, fps: bool, running: Arc<Mutex<bool>>) {
    eprintln!("[sampling] 启动: package={} interval={} cpu={} memory={} fps={}", package, interval, cpu, memory, fps);
    tauri::async_runtime::spawn(async move {
        let mut sampler = Sampler::new(&package, interval, cpu, memory, false, fps);
        loop {
            if !*running.lock().unwrap() {
                eprintln!("[sampling] 停止：running=false");
                break;
            }
            eprintln!("[sampling] 开始一轮 sample_once...");
            let events = sampler.sample_once().await;
            eprintln!("[sampling] 本轮产生 {} 个事件", events.len());
            for ev in &events {
                eprintln!("[sampling] {}", brief_event(ev));
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
    fps: bool,
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

    spawn_sampling(app, package, interval, cpu, memory, fps, state.running.clone());

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

/// 列出设备上已安装的全部应用包名（含系统应用，共几百个），供前端搜索选择。
/// 用 `pm list packages`（不带 -3，-3 只列第三方会遗漏系统应用）。
#[tauri::command]
fn list_packages() -> Result<Vec<String>, String> {
    let out = xperf_core::run_adb_command(&["shell", "pm", "list", "packages"])
        .map_err(|e| e.to_string())?;
    let mut pkgs: Vec<String> = out
        .stdout
        .lines()
        .filter_map(|l| l.trim().strip_prefix("package:").map(|s| s.to_string()))
        .collect();
    // 去重 + 排序（pm 输出无序）
    pkgs.sort();
    pkgs.dedup();
    Ok(pkgs)
}

/// 查询后端采样是否正在运行（前端据此设置开始/停止按钮状态）。
#[tauri::command]
fn is_running(state: State<'_, AppState>) -> Result<bool, String> {
    Ok(*state.running.lock().map_err(|e| e.to_string())?)
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
    let auto_fps = args.iter().any(|a| a == "--fps");

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
                        auto_fps,
                        state.running.clone(),
                    );
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            start_sampling,
            stop_sampling,
            diag_log,
            list_packages,
            is_running
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
