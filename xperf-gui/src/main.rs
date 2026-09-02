#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::{Arc, Mutex};
use chrono::{DateTime, Local};
use tauri::{Emitter, Manager, State};
use xperf_core::agent::{self, AgentEvent};
use xperf_core::{MemoryDetails, SampleEvent, ThreadCpuInfo};

/// AgentEvent → 前端 SampleEvent（保持与前端既有协议一致，前端零改动）。
/// 首次见到某 PID 时先补一条 PidDiscovered。
fn map_event(
    ev: AgentEvent,
    known_pids: &mut std::collections::HashSet<u32>,
) -> Vec<SampleEvent> {
    let ts_of = |ts_ms: u64| {
        DateTime::from_timestamp_millis(ts_ms as i64)
            .map(|t| t.with_timezone(&Local))
            .unwrap_or_else(Local::now)
    };
    let mut out = Vec::new();
    let pid = match &ev {
        AgentEvent::Cpu { pid, .. } | AgentEvent::Mem { pid, .. } | AgentEvent::Fps { pid, .. } => Some(*pid),
        _ => None,
    };
    if let Some(p) = pid {
        if known_pids.insert(p) {
            out.push(SampleEvent::PidDiscovered {
                pid: p.to_string(),
                start_time: String::new(), // agent 协议不带启动时间，前端不展示该字段
            });
        }
    }
    match ev {
        AgentEvent::Hello { ncores } => {
            eprintln!("[sampling] agent 已启动（{} 核）", ncores);
        }
        AgentEvent::Cpu { ts, pid, cpu, th } => {
            let t = ts_of(ts);
            out.push(SampleEvent::CpuUpdate {
                pid: pid.to_string(),
                timestamp: t,
                process_cpu: cpu,
                threads: th
                    .into_iter()
                    .map(|(tid, name, usage)| ThreadCpuInfo {
                        tid: tid.to_string(),
                        cpu_usage: usage,
                        name,
                        timestamp: Some(t),
                    })
                    .collect(),
            });
        }
        AgentEvent::Mem { ts, pid, pss, java, native, code, stack, gfx, other, sys, .. } => {
            out.push(SampleEvent::MemoryUpdate {
                pid: pid.to_string(),
                timestamp: ts_of(ts),
                total_pss: pss,
                details: MemoryDetails {
                    java_heap: java,
                    native_heap: native,
                    code,
                    stack,
                    graphics: gfx,
                    private_other: other,
                    system: sys,
                    total_pss: pss,
                },
            });
        }
        AgentEvent::Fps { ts, pid, layer, fps, frames, jank } => {
            out.push(SampleEvent::FpsUpdate {
                pid: pid.to_string(),
                timestamp: ts_of(ts),
                layer,
                fps,
                frame_count: frames,
                jank_count: jank,
            });
        }
        AgentEvent::Exit { pid } => {
            known_pids.remove(&pid);
            out.push(SampleEvent::PidDisappeared { pid: pid.to_string() });
        }
        AgentEvent::Noproc => {
            out.push(SampleEvent::NoProcess { error: "包名下无进程".to_string() });
        }
        AgentEvent::Err { msg } => eprintln!("[sampling] agent: {}", msg),
    }
    out
}

/// 后台采样循环（start_sampling 命令与自动启动共用）。
/// 采样在设备端 agent 进行，本线程只阻塞读事件流并转发给前端。
fn spawn_sampling(app: tauri::AppHandle, package: String, interval: u64, cpu: bool, memory: bool, fps: bool, running: Arc<Mutex<bool>>) {
    eprintln!("[sampling] 启动: package={} interval={} cpu={} memory={} fps={}", package, interval, cpu, memory, fps);
    std::thread::spawn(move || {
        let bin = match agent::ensure_agent_built() {
            Ok(b) => b,
            Err(e) => {
                eprintln!("[sampling] agent 构建失败: {}", e);
                let mut running = running.lock().unwrap();
                *running = false;
                return;
            }
        };
        if let Err(e) = agent::deploy_agent(&bin) {
            eprintln!("[sampling] agent 部署失败: {}", e);
            let mut running = running.lock().unwrap();
            *running = false;
            return;
        }
        let mut stream = match agent::spawn_agent(Some(&package), interval, cpu, memory, fps) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[sampling] agent 启动失败: {}", e);
                let mut running = running.lock().unwrap();
                *running = false;
                return;
            }
        };
        let mut known_pids = std::collections::HashSet::new();
        while *running.lock().unwrap() {
            match stream.next_event() {
                Ok(Some(Ok(ev))) => {
                    for sev in map_event(ev, &mut known_pids) {
                        eprintln!("[sampling] {}", brief_event(&sev));
                        let _ = app.emit("sample", sev);
                    }
                }
                Ok(Some(Err(e))) => eprintln!("[sampling] 协议解析失败: {}", e),
                // EOF/读错误：adb 长连接断开或 agent 退出 → 等待设备恢复并重连（不停止采样）
                Ok(None) | Err(_) => {
                    eprintln!("[sampling] 连接断开，等待设备恢复…");
                    let running2 = running.clone();
                    match agent::reconnect_agent(
                        Some(&package), interval, cpu, memory, fps,
                        &move || *running2.lock().unwrap(),
                    ) {
                        Some(s) => {
                            stream = s;
                            eprintln!("[sampling] 已重连，恢复采样");
                            continue;
                        }
                        None => break, // 用户停止
                    }
                }
            }
        }
    });
}

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

/// 导出前端持有的完整会话历史为 CSV（GUI 不流式落盘，数据在前端内存中）。
/// 写到 log/<pkg>/<导出时刻>/ 下的 cpu/memory/fps 子目录，返回目录路径。
/// cpu/mem: pid -> [[ms, value]...]；fps: 图层短名 -> [[ms, fps, jank]...]
#[tauri::command]
fn export_csv(
    package: String,
    cpu: std::collections::HashMap<String, Vec<(f64, f64)>>,
    mem: std::collections::HashMap<String, Vec<(f64, f64)>>,
    fps: std::collections::HashMap<String, Vec<(f64, f64, u32)>>,
) -> Result<String, String> {
    use std::io::Write;
    let dir = std::path::PathBuf::from("log")
        .join(&package)
        .join(Local::now().format("%Y%m%d_%H%M%S").to_string());
    let fmt_ts = |ms: f64| {
        DateTime::from_timestamp_millis(ms as i64)
            .map(|t| t.with_timezone(&Local).format("%Y-%m-%d %H:%M:%S%.3f").to_string())
            .unwrap_or_default()
    };
    let mut wrote = false;
    if !cpu.is_empty() {
        let d = dir.join("cpu");
        std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
        for (pid, pts) in &cpu {
            let mut f = std::fs::File::create(d.join(format!("cpu_{}_data.csv", pid))).map_err(|e| e.to_string())?;
            writeln!(f, "Timestamp,Process CPU (%)").map_err(|e| e.to_string())?;
            for (t, v) in pts {
                writeln!(f, "{},{:.2}", fmt_ts(*t), v).map_err(|e| e.to_string())?;
            }
        }
        wrote = true;
    }
    if !mem.is_empty() {
        let d = dir.join("memory");
        std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
        for (pid, pts) in &mem {
            let mut f = std::fs::File::create(d.join(format!("memory_{}_data.csv", pid))).map_err(|e| e.to_string())?;
            writeln!(f, "Timestamp,Total PSS").map_err(|e| e.to_string())?;
            for (t, v) in pts {
                writeln!(f, "{},{}", fmt_ts(*t), *v as u64).map_err(|e| e.to_string())?;
            }
        }
        wrote = true;
    }
    if !fps.is_empty() {
        let d = dir.join("fps");
        std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
        for (layer, pts) in &fps {
            let safe = layer.replace(['/', '#'], "_");
            let mut f = std::fs::File::create(d.join(format!("fps_data_{}.csv", safe))).map_err(|e| e.to_string())?;
            writeln!(f, "Timestamp,FPS,Jank").map_err(|e| e.to_string())?;
            for (t, v, jank) in pts {
                writeln!(f, "{},{:.2},{}", fmt_ts(*t), v, jank).map_err(|e| e.to_string())?;
            }
        }
        wrote = true;
    }
    if !wrote {
        return Err("暂无可导出的数据".into());
    }
    Ok(dir.to_string_lossy().into_owned())
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
            is_running,
            export_csv
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_export_csv_writes_files() {
        let pkg = format!("test_export_{}", std::process::id());
        let mut cpu = std::collections::HashMap::new();
        cpu.insert("1234".to_string(), vec![(1700000000000.0, 12.5), (1700000001000.0, 30.0)]);
        let mut fps = std::collections::HashMap::new();
        fps.insert("SVM Container_0".to_string(), vec![(1700000000000.0, 30.0, 1u32)]);
        let dir = export_csv(pkg.clone(), cpu, Default::default(), fps).unwrap();
        let cpu_csv = std::fs::read_to_string(format!("{}/cpu/cpu_1234_data.csv", dir)).unwrap();
        assert!(cpu_csv.starts_with("Timestamp,Process CPU (%)\n"));
        assert!(cpu_csv.contains(",12.50\n"));
        let fps_csv = std::fs::read_to_string(format!("{}/fps/fps_data_SVM Container_0.csv", dir)).unwrap();
        assert!(fps_csv.starts_with("Timestamp,FPS,Jank\n"));
        assert!(fps_csv.contains(",30.00,1\n"));
        std::fs::remove_dir_all(std::path::PathBuf::from("log").join(&pkg)).ok();
    }

    #[test]
    fn test_export_csv_empty_errors() {
        let r = export_csv("x".into(), Default::default(), Default::default(), Default::default());
        assert!(r.is_err());
    }
}

