//! xperf-gui：xtools 的 Tauri 桌面 GUI——与 CLI 共用 xperf-core 的采样/深挖能力，
//! 前端呈现折线图/实时数值/Top 线程/峰值/Perfetto 分析报告与 simpleperf 函数热点报告
//! （各自独立 tab），支持暗/亮主题。
//! 支持命令行自动启动：`--package <pkg> [--interval N] [--cpu …] [--trace N] [--stack N]`。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::{Arc, Mutex};
use chrono::{DateTime, Local};
use tauri::{Emitter, Manager, State};
use xperf_core::agent::{self, AgentEvent};
use xperf_core::{MemoryDetails, MetricFlags, SampleEvent, ThreadCpuInfo};

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
        AgentEvent::Cpu { pid, .. }
        | AgentEvent::Mem { pid, .. }
        | AgentEvent::Fps { pid, .. }
        | AgentEvent::Io { pid, .. }
        | AgentEvent::GpuMem { pid, .. } => Some(*pid),
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
        AgentEvent::Hello { ncores, maxkhz } => {
            eprintln!("[sampling] agent 已启动（{} 核）", ncores);
            out.push(SampleEvent::AgentHello { ncores, maxkhz });
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
        AgentEvent::Freq { ts, khz } => {
            out.push(SampleEvent::FreqUpdate { timestamp: ts_of(ts), khz });
        }
        AgentEvent::Temp { ts, status, sensors } => {
            out.push(SampleEvent::TempUpdate { timestamp: ts_of(ts), status, sensors });
        }
        AgentEvent::Gpu { ts, busy, util, mhz, maxmhz } => {
            out.push(SampleEvent::GpuUpdate { timestamp: ts_of(ts), busy, util, mhz, maxmhz });
        }
        AgentEvent::GpuProc { ts, pid, busy } => {
            out.push(SampleEvent::GpuProcUpdate { pid: pid.to_string(), timestamp: ts_of(ts), busy });
        }
        AgentEvent::GpuMem { ts, pid, bytes, global } => {
            out.push(SampleEvent::GpuMemUpdate { pid: pid.to_string(), timestamp: ts_of(ts), bytes, global });
        }
        AgentEvent::Io { ts, pid, r, w, dr, dw } => {
            out.push(SampleEvent::IoUpdate { pid: pid.to_string(), timestamp: ts_of(ts), r, w, dr, dw });
        }
        AgentEvent::Net { ts, rx, tx } => {
            out.push(SampleEvent::NetUpdate { timestamp: ts_of(ts), rx, tx });
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
fn spawn_sampling(app: tauri::AppHandle, package: String, interval: u64, flags: MetricFlags, running: Arc<Mutex<bool>>) {
    eprintln!("[sampling] 启动: package={} interval={} flags={:?}", package, interval, flags);
    std::thread::spawn(move || {
        let platform = xperf_core::detect_platform_live();
        eprintln!("[sampling] 平台: {} ({})", platform.name(), platform.description());
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
        let mut stream = match agent::spawn_agent(Some(&package), interval, flags, Some(&*platform)) {
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
                        Some(&package), interval, flags, Some(&*platform),
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
        drop(stream);
        // QNX 统计链清理兜底：agent 可能被 adbd 信号直杀而来不及跑退出钩子
        if flags.gpu {
            agent::qnx_stop_stats(&*platform, interval);
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
        SampleEvent::AgentHello { ncores, .. } => format!("AgentHello ncores={}", ncores),
        SampleEvent::FreqUpdate { khz, .. } => format!("FreqUpdate khz={:?}", khz),
        SampleEvent::TempUpdate { status, sensors, .. } => {
            format!("TempUpdate status={} sensors={}", status, sensors.len())
        }
        SampleEvent::GpuUpdate { busy, mhz, .. } => format!("GpuUpdate busy={:.1}% mhz={}", busy, mhz),
        SampleEvent::GpuProcUpdate { pid, busy, .. } => format!("GpuProcUpdate pid={} busy={:.1}%", pid, busy),
        SampleEvent::GpuMemUpdate { pid, bytes, .. } => {
            format!("GpuMemUpdate pid={} mem={:.1}MB", pid, *bytes as f64 / 1e6)
        }
        SampleEvent::IoUpdate { pid, r, w, .. } => format!("IoUpdate pid={} r={:.1} w={:.1} KB/s", pid, r, w),
        SampleEvent::NetUpdate { rx, tx, .. } => format!("NetUpdate rx={:.1} tx={:.1} KB/s", rx, tx),
        SampleEvent::SampleError { pid, stage, error } => {
            format!("SampleError pid={:?} stage={}: {}", pid, stage, error)
        }
    }
}

struct AppState {
    running: Arc<Mutex<bool>>,
    /// 深挖录制进行中（与采样互不干扰，可并行）
    trace_running: Arc<Mutex<bool>>,
    /// 函数热点录制进行中（与采样/深挖互不干扰，可并行）
    stack_running: Arc<Mutex<bool>>,
}

/// 包名校验：防路径遍历（包名会拼入日志目录路径）
fn validate_package(package: &str) -> Result<(), String> {
    if package.is_empty() || package == "." || package == ".." || package.len() > 256
        || !package.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-') {
        Err(format!("非法包名: {}", package))
    } else {
        Ok(())
    }
}

/// 采集数据根目录 `/tmp/xperf`（与 CLI 的 `cli_utils::data_root` 同一定位；GUI 独立
/// 定义避免跨 crate 公共依赖改动——两处实现一致）
fn gui_data_root() -> std::path::PathBuf {
    std::env::temp_dir().join("xperf")
}

#[tauri::command]
#[allow(clippy::too_many_arguments)] // tauri 命令参数须扁平，指标开关逐一对应前端勾选框
async fn start_sampling(
    package: String,
    interval: u64,
    cpu: bool,
    memory: bool,
    fps: bool,
    freq: bool,
    thermal: bool,
    gpu: bool,
    io: bool,
    net: bool,
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

    // 包名校验：防路径遍历（包名会拼入日志目录路径）
    if let Err(e) = validate_package(&package) {
        *state.running.lock().map_err(|e| e.to_string())? = false;
        return Err(e);
    }

    let flags = MetricFlags { cpu, memory, fps, freq, thermal, gpu, io, net };
    spawn_sampling(app, package, interval, flags, state.running.clone());

    Ok(())
}

#[tauri::command]
fn stop_sampling(state: State<'_, AppState>) -> Result<(), String> {
    let mut running = state.running.lock().map_err(|e| e.to_string())?;
    *running = false;
    Ok(())
}

/// 后台深挖线程（start_trace 命令与 --trace 自动启动共用）：
/// 录制 → 拉回 → trace_processor SQL 分析，全程 emit("trace") 推进度。
/// stage: recording / progress（每秒，message 含已录制秒数）/ recorded（已拉回，
/// 分析中）/ done（message=完整报告文本）/ error；recorded/done 附 trace_path
/// （前端"在浏览器打开 Perfetto UI"按钮用）。
/// 与采样会话互不干扰（可并行；GUI 采样不限时，窗口对照靠报告与图表的时间戳）。
fn spawn_trace(app: tauri::AppHandle, package: String, seconds: u64, running: Arc<Mutex<bool>>) {
    eprintln!("[trace] 启动: package={} seconds={}", package, seconds);
    std::thread::spawn(move || {
        let emit_stage = |stage: &str, message: String, trace_path: Option<String>| {
            let _ = app.emit(
                "trace",
                serde_json::json!({ "stage": stage, "message": message, "trace_path": trace_path }),
            );
        };
        let dir = gui_data_root()
            .join(&package)
            .join(Local::now().format("%Y%m%d_%H%M%S").to_string())
            .join("trace");
        emit_stage("recording", format!("录制 {}s perfetto trace…（窗口内操作被测应用）", seconds), None);
        let progress = |elapsed: u64| {
            eprintln!("[trace] progress: {}s", elapsed);
            let _ = app.emit(
                "trace",
                serde_json::json!({
                    "stage": "progress",
                    "message": format!("perfetto 录制中 {}/{}s", elapsed.min(seconds), seconds),
                    "trace_path": null
                }),
            );
        };
        let rec = match xperf_core::trace::record(seconds, &dir, Some(&progress)) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[trace] 录制失败: {}", e);
                emit_stage("error", format!("录制失败: {}", e), None);
                *running.lock().unwrap() = false;
                return;
            }
        };
        let trace_path = rec.local_path.display().to_string();
        eprintln!("[trace] 已拉回: {}（{:.1} MB）", trace_path, rec.bytes as f64 / 1e6);
        emit_stage(
            "recorded",
            format!("已拉回 {}（{:.1} MB），SQL 分析中…", trace_path, rec.bytes as f64 / 1e6),
            Some(trace_path.clone()),
        );
        match xperf_core::trace::analyze_and_report(&rec, &package) {
            Ok(report) => emit_stage("done", report, Some(trace_path)),
            Err(e) => {
                eprintln!("[trace] 分析失败: {}", e);
                emit_stage("error", format!("{}", e), Some(trace_path));
            }
        }
        *running.lock().unwrap() = false;
    });
}

/// 深挖模式：录制 N 秒 perfetto trace 并 SQL 归因（详见 xperf-core/src/trace.rs）
#[tauri::command]
async fn start_trace(
    package: String,
    seconds: u64,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    {
        let mut t = state.trace_running.lock().map_err(|e| e.to_string())?;
        if *t {
            return Err("Perfetto 分析进行中，请等待完成".into());
        }
        *t = true;
    }
    if let Err(e) = validate_package(&package) {
        *state.trace_running.lock().map_err(|e| e.to_string())? = false;
        return Err(e);
    }
    let seconds = seconds.clamp(1, 600);
    spawn_trace(app, package, seconds, state.trace_running.clone());
    Ok(())
}

/// 后台函数热点线程（start_stack 命令与 --stack 自动启动共用）：
/// 录制 → 设备端三视图报告 → 拉回 → 渲染热点报告，全程 emit("stack") 推进度。
/// stage: recording / progress（每秒，message 含已录制秒数）/ recorded（已拉回，
/// 渲染报告中）/ done（message=完整报告文本）/ error；recorded/done/error 附
/// data_path（`.data` 文件路径，前端"在浏览器打开火焰图"按钮用）。
/// 与采样/trace 会话互不干扰（可并行；GUI 采样不限时，窗口对照靠报告与图表的时间戳）。
fn spawn_stack(app: tauri::AppHandle, package: String, seconds: u64, running: Arc<Mutex<bool>>) {
    eprintln!("[stack] 启动: package={} seconds={}", package, seconds);
    std::thread::spawn(move || {
        let emit_stage = |stage: &str, message: String, data_path: Option<String>| {
            let _ = app.emit(
                "stack",
                serde_json::json!({ "stage": stage, "message": message, "data_path": data_path }),
            );
        };
        let dir = gui_data_root()
            .join(&package)
            .join(Local::now().format("%Y%m%d_%H%M%S").to_string())
            .join("stack");
        emit_stage("recording", format!("录制 {}s 调用栈…（窗口内操作被测应用）", seconds), None);
        let progress = |elapsed: u64| {
            eprintln!("[stack] progress: {}s", elapsed);
            let _ = app.emit(
                "stack",
                serde_json::json!({
                    "stage": "progress",
                    "message": format!("调用栈录制中 {}/{}s", elapsed.min(seconds), seconds),
                    "data_path": null
                }),
            );
        };
        let rec = match xperf_core::simpleperf::record(seconds, &package, &dir, Some(&progress)) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[stack] 录制失败: {}", e);
                emit_stage("error", format!("录制失败: {}", e), None);
                *running.lock().unwrap() = false;
                return;
            }
        };
        let data_path = rec.local_path.display().to_string();
        eprintln!(
            "[stack] 已拉回: {}（{:.1} MB，{} 样本）",
            data_path,
            rec.bytes as f64 / 1e6,
            rec.samples
        );
        emit_stage(
            "recorded",
            format!("已拉回 {}（{:.1} MB，{} 样本），渲染热点报告中…", data_path, rec.bytes as f64 / 1e6, rec.samples),
            Some(data_path.clone()),
        );
        match xperf_core::simpleperf::analyze_and_report(&rec, &package) {
            Ok(report) => emit_stage("done", report, Some(data_path)),
            Err(e) => {
                eprintln!("[stack] 报告生成失败: {}", e);
                emit_stage("error", format!("{}", e), Some(data_path));
            }
        }
        *running.lock().unwrap() = false;
    });
}

/// 函数热点模式：simpleperf 录制 N 秒调用栈并生成热点报告（详见 xperf-core/src/simpleperf.rs）
#[tauri::command]
async fn start_stack(
    package: String,
    seconds: u64,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    {
        let mut t = state.stack_running.lock().map_err(|e| e.to_string())?;
        if *t {
            return Err("函数热点录制进行中，请等待完成".into());
        }
        *t = true;
    }
    if let Err(e) = validate_package(&package) {
        *state.stack_running.lock().map_err(|e| e.to_string())? = false;
        return Err(e);
    }
    let seconds = seconds.clamp(1, 600);
    spawn_stack(app, package, seconds, state.stack_running.clone());
    Ok(())
}

/// 在浏览器查看 simpleperf 调用栈火焰图。
/// 用 AOSP 官方 report_html.py 把 `.data` 渲染成单文件 HTML（含火焰图/Chart/Sample
/// Table）后打开；首次使用自动从 AOSP 下载脚本集（~10MB，缓存后离线），需要 python3。
/// 阻塞数秒（渲染），async 命令不卡 UI 主线程。
#[tauri::command]
async fn open_stack_html(data_path: String) -> Result<String, String> {
    let path = std::path::PathBuf::from(&data_path);
    if !path.is_file() {
        return Err(format!("数据文件不存在: {}", data_path));
    }
    xperf_core::simpleperf::open_stack_in_browser(&path).map_err(|e| e.to_string())
}

/// 清理缓存与采集数据（与 CLI `--clean-cache` 同一实现）：
/// `~/.cache/xperf`（perfetto UI 镜像 + simpleperf 脚本集，首次使用重新下载）
/// + `/tmp/xperf`（全部采集数据）。采样/录制进行中会丢当前会话产物——前端
/// 弹确认框后调用。返回人类可读结果（清理文件数与体积）。
#[tauri::command]
async fn clean_cache() -> Result<String, String> {
    let r = xperf_core::simpleperf::clean_all_caches().map_err(|e| e.to_string())?;
    Ok(format!(
        "缓存已清理: {} 个文件，{:.1} MB（~/.cache/xperf + /tmp/xperf）",
        r.files,
        r.bytes as f64 / 1e6
    ))
}

/// 在浏览器打开 Perfetto UI 并自动加载 trace。
/// 优先本地镜像 UI + 同源加载（全自动，首次使用需联网镜像约 20 个资源）；
/// 失败（离线/无 Chrome/无法访问 ui.perfetto.dev）自动回退拖拽方式。
#[tauri::command]
fn open_perfetto_ui(trace_path: String) -> Result<String, String> {
    let path = std::path::PathBuf::from(&trace_path);
    if !path.is_file() {
        return Err(format!("trace 文件不存在: {}", trace_path));
    }
    match xperf_core::trace::open_trace_in_local_ui(&path) {
        Ok(msg) => Ok(msg),
        Err(e) => {
            eprintln!("[trace] 自动加载不可用，回退拖拽: {}", e);
            let msg = xperf_core::trace::reveal_trace_and_open_ui(&path).map_err(|e| e.to_string())?;
            Ok(format!("自动加载不可用（{}）。{}", e, msg))
        }
    }
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
async fn list_packages() -> Result<Vec<String>, String> {
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

/// IO 导出行：(ms, r, w, dr, dw) KB/s
type IoExportPoints = Vec<(f64, f64, f64, f64, f64)>;
/// GPU 显存导出行：(ms, 进程 MB, 整机 MB)
type GpuMemExportPoints = Vec<(f64, f64, f64)>;
/// GPU 系统导出行：(ms, busy%, util%, mhz)
type GpuExportPoints = Vec<(f64, f64, f64, u32)>;

/// 打点：前端按钮调用，追加到 markers 列表 + 打印到 stderr（调试用）
#[tauri::command]
fn add_marker(label: String, app: tauri::AppHandle) -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let ts_str = chrono::DateTime::from_timestamp_millis(ts as i64)
        .map(|t| t.with_timezone(&chrono::Local).format("%H:%M:%S%.3f").to_string())
        .unwrap_or_default();
    eprintln!("[marker] {} {}", ts_str, label);
    // 发事件给前端让图表画竖线
    let _ = app.emit("marker", serde_json::json!({"label": label, "timestamp": ts}));
    format!("[{}] 📍 {}", ts_str, label)
}

/// 导出前端持有的完整会话历史为 CSV（GUI 不流式落盘，数据在前端内存中）。
/// 写到 `log/<pkg>/<导出时刻>/` 下的各指标子目录，返回目录路径。
/// cpu/mem: pid -> [[ms, value]...]；fps: 图层短名 -> [[ms, fps, jank]...]
/// freq: 核名 -> [[ms, MHz]...]；temp: 传感器 -> [[ms, °C, status]...]；
/// gpu: [[ms, busy%, mhz]...]；io: pid -> [[ms, r, w, dr, dw]...]；net: [[ms, rx, tx]...]
#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn export_csv(
    package: String,
    cpu: std::collections::HashMap<String, Vec<(f64, f64)>>,
    mem: std::collections::HashMap<String, Vec<(f64, f64)>>,
    fps: std::collections::HashMap<String, Vec<(f64, f64, u32)>>,
    freq: std::collections::HashMap<String, Vec<(f64, f64)>>,
    temp: std::collections::HashMap<String, Vec<(f64, f64, i32)>>,
    gpu: GpuExportPoints,
    io: std::collections::HashMap<String, IoExportPoints>,
    net: Vec<(f64, f64, f64)>,
    gpumem: std::collections::HashMap<String, GpuMemExportPoints>,
    gpuproc: std::collections::HashMap<String, Vec<(f64, f64)>>,
) -> Result<String, String> {
    use std::io::Write;
    let dir = gui_data_root()
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
    if !freq.is_empty() {
        let d = dir.join("freq");
        std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
        let mut f = std::fs::File::create(d.join("freq_data.csv")).map_err(|e| e.to_string())?;
        // 核名按 cpuN 数值排序，列序稳定
        let mut cores: Vec<&String> = freq.keys().collect();
        cores.sort_by_key(|n| n.trim_start_matches("cpu").parse::<u32>().unwrap_or(u32::MAX));
        writeln!(f, "Timestamp,{}", cores.iter().map(|c| format!("{} (MHz)", c)).collect::<Vec<_>>().join(","))
            .map_err(|e| e.to_string())?;
        let rows = cores.iter().map(|c| &freq[*c]).collect::<Vec<_>>();
        let n = rows.iter().map(|r| r.len()).max().unwrap_or(0);
        for i in 0..n {
            let t = rows.iter().find_map(|r| r.get(i).map(|(t, _)| *t)).unwrap_or(0.0);
            let cells: Vec<String> = rows.iter().map(|r| r.get(i).map(|(_, v)| format!("{:.0}", v)).unwrap_or_default()).collect();
            writeln!(f, "{},{}", fmt_ts(t), cells.join(",")).map_err(|e| e.to_string())?;
        }
        wrote = true;
    }
    if !temp.is_empty() {
        let d = dir.join("thermal");
        std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
        let mut f = std::fs::File::create(d.join("thermal_data.csv")).map_err(|e| e.to_string())?;
        writeln!(f, "Timestamp,Status,Sensor,TempC").map_err(|e| e.to_string())?;
        for (sensor, pts) in &temp {
            for (t, v, status) in pts {
                writeln!(f, "{},{},{},{:.1}", fmt_ts(*t), status, sensor, v).map_err(|e| e.to_string())?;
            }
        }
        wrote = true;
    }
    if !gpu.is_empty() {
        let d = dir.join("gpu");
        std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
        let mut f = std::fs::File::create(d.join("gpu_data.csv")).map_err(|e| e.to_string())?;
        writeln!(f, "Timestamp,Busy (%),Util (%),Clock (MHz)").map_err(|e| e.to_string())?;
        for (t, busy, util, mhz) in &gpu {
            writeln!(f, "{},{:.2},{:.2},{}", fmt_ts(*t), busy, util, mhz).map_err(|e| e.to_string())?;
        }
        wrote = true;
    }
    if !gpuproc.is_empty() {
        let d = dir.join("gpu");
        std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
        for (pid, pts) in &gpuproc {
            let mut f = std::fs::File::create(d.join(format!("gpu_proc_{}_data.csv", pid))).map_err(|e| e.to_string())?;
            writeln!(f, "Timestamp,Busy (%)").map_err(|e| e.to_string())?;
            for (t, busy) in pts {
                writeln!(f, "{},{:.2}", fmt_ts(*t), busy).map_err(|e| e.to_string())?;
            }
        }
        wrote = true;
    }
    if !io.is_empty() {
        let d = dir.join("io");
        std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
        for (pid, pts) in &io {
            let mut f = std::fs::File::create(d.join(format!("io_{}_data.csv", pid))).map_err(|e| e.to_string())?;
            writeln!(f, "Timestamp,Read (KB/s),Write (KB/s),Disk Read (KB/s),Disk Write (KB/s)").map_err(|e| e.to_string())?;
            for (t, r, w, dr, dw) in pts {
                writeln!(f, "{},{:.2},{:.2},{:.2},{:.2}", fmt_ts(*t), r, w, dr, dw).map_err(|e| e.to_string())?;
            }
        }
        wrote = true;
    }
    if !net.is_empty() {
        let d = dir.join("net");
        std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
        let mut f = std::fs::File::create(d.join("net_data.csv")).map_err(|e| e.to_string())?;
        writeln!(f, "Timestamp,RX (KB/s),TX (KB/s)").map_err(|e| e.to_string())?;
        for (t, rx, tx) in &net {
            writeln!(f, "{},{:.2},{:.2}", fmt_ts(*t), rx, tx).map_err(|e| e.to_string())?;
        }
        wrote = true;
    }
    if !gpumem.is_empty() {
        let d = dir.join("gpumem");
        std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
        for (pid, pts) in &gpumem {
            let mut f = std::fs::File::create(d.join(format!("gpumem_{}_data.csv", pid))).map_err(|e| e.to_string())?;
            writeln!(f, "Timestamp,Process GPU Mem (MB),Global GPU Mem (MB)").map_err(|e| e.to_string())?;
            for (t, mb, gmb) in pts {
                writeln!(f, "{},{:.1},{:.0}", fmt_ts(*t), mb, gmb).map_err(|e| e.to_string())?;
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
    // 支持命令行自动启动：xperf-gui --package <pkg> [--interval 1000] [--cpu] [--memory] [--fps] [--freq] [--io] [--net] [--gpu] [--thermal] [--trace N] [--stack N]
    // （便于脚本化/验证；不传参数则手动在前端操作）
    let args: Vec<String> = std::env::args().collect();
    let get_opt = |name: &str| -> Option<String> {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1).cloned())
    };
    let has_flag = |name: &str| args.iter().any(|a| a == name);
    let auto_package = get_opt("--package");
    let auto_interval: u64 = get_opt("--interval").and_then(|v| v.parse().ok()).unwrap_or(1000).max(50);
    let auto_trace: Option<u64> = get_opt("--trace").and_then(|v| v.parse().ok());
    let auto_stack: Option<u64> = get_opt("--stack").and_then(|v| v.parse().ok());
    let auto_flags = MetricFlags {
        cpu: has_flag("--cpu"),
        memory: has_flag("--memory"),
        fps: has_flag("--fps"),
        freq: has_flag("--freq"),
        thermal: has_flag("--thermal"),
        gpu: has_flag("--gpu"),
        io: has_flag("--io"),
        net: has_flag("--net"),
    };

    tauri::Builder::default()
        .manage(AppState {
            running: Arc::new(Mutex::new(false)),
            trace_running: Arc::new(Mutex::new(false)),
            stack_running: Arc::new(Mutex::new(false)),
        })
        .setup(move |app| {
            if let Some(package) = auto_package.clone() {
                let state = app.state::<AppState>();
                let mut running = state.running.lock().unwrap();
                if !*running {
                    *running = true;
                    drop(running);
                    // 一个指标都没传时默认 CPU+Memory（保持旧行为）
                    let flags = if auto_flags.any() {
                        auto_flags
                    } else {
                        MetricFlags { cpu: true, memory: true, ..auto_flags }
                    };
                    spawn_sampling(
                        app.handle().clone(),
                        package.clone(),
                        auto_interval,
                        flags,
                        state.running.clone(),
                    );
                }
                // 深挖自动启动（--trace N，可与采样并行）
                if let Some(n) = auto_trace {
                    let n = n.clamp(1, 600);
                    let state = app.state::<AppState>();
                    let mut t = state.trace_running.lock().unwrap();
                    if !*t {
                        *t = true;
                        drop(t);
                        spawn_trace(app.handle().clone(), package.clone(), n, state.trace_running.clone());
                    }
                }
                // 函数热点自动启动（--stack N，可与采样/深挖并行）
                if let Some(n) = auto_stack {
                    let n = n.clamp(1, 600);
                    let state = app.state::<AppState>();
                    let mut t = state.stack_running.lock().unwrap();
                    if !*t {
                        *t = true;
                        drop(t);
                        spawn_stack(app.handle().clone(), package, n, state.stack_running.clone());
                    }
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            start_sampling,
            stop_sampling,
            start_trace,
            start_stack,
            open_perfetto_ui,
            open_stack_html,
            clean_cache,
            diag_log,
            list_packages,
            is_running,
            add_marker,
            export_csv
        ])
        .on_window_event(|window, event| {
            // 关窗时停止采样：置 running=false，采样线程在下一轮循环检测到后退出，
            // exec-out 管道断开 → 设备端 agent 因 stdout 写失败自行退出
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                // 深挖录制线程不等待（最长 600s），置中断标志让其尽快退出；
                // 未及退出时设备端 perfetto 由 traced TTL 兜底停止（残留文件无害）
                xperf_core::utils::set_interrupt_flag();
                let state = window.state::<AppState>();
                let mut running = state.running.lock().unwrap();
                if *running {
                    *running = false;
                    drop(running);
                    // 等采样线程检测到 running=false 并退出（最长一个 interval 周期）
                    std::thread::sleep(std::time::Duration::from_millis(1200));
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_export_csv_writes_files() {
        let pkg = format!("test_export_{}", std::process::id());
        let mut cpu = std::collections::HashMap::new();
        cpu.insert("1234".to_string(), vec![(1700000000000.0, 12.5), (1700000001000.0, 30.0)]);
        let mut fps = std::collections::HashMap::new();
        fps.insert("SVM Container_0".to_string(), vec![(1700000000000.0, 30.0, 1u32)]);
        let mut freq = std::collections::HashMap::new();
        freq.insert("cpu0".to_string(), vec![(1700000000000.0, 2592.0)]);
        freq.insert("cpu1".to_string(), vec![(1700000000000.0, 2246.0)]);
        let mut temp = std::collections::HashMap::new();
        temp.insert("soc0".to_string(), vec![(1700000000000.0, 42.5, 0i32)]);
        let gpu = vec![(1700000000000.0, 37.5, 33.8, 585u32)];
        let mut io = std::collections::HashMap::new();
        io.insert("1234".to_string(), vec![(1700000000000.0, 12.0, 3.0, 0.0, 1.0)]);
        let net = vec![(1700000000000.0, 123.0, 45.0)];
        let mut gpumem = std::collections::HashMap::new();
        gpumem.insert("1234".to_string(), vec![(1700000000000.0, 154.4, 2639.1)]);
        let mut gpuproc = std::collections::HashMap::new();
        gpuproc.insert("1234".to_string(), vec![(1700000000000.0, 14.4)]);
        let dir = export_csv(pkg.clone(), cpu, Default::default(), fps, freq, temp, gpu, io, net, gpumem, gpuproc).await.unwrap();
        let cpu_csv = std::fs::read_to_string(format!("{}/cpu/cpu_1234_data.csv", dir)).unwrap();
        assert!(cpu_csv.starts_with("Timestamp,Process CPU (%)\n"));
        assert!(cpu_csv.contains(",12.50\n"));
        let fps_csv = std::fs::read_to_string(format!("{}/fps/fps_data_SVM Container_0.csv", dir)).unwrap();
        assert!(fps_csv.starts_with("Timestamp,FPS,Jank\n"));
        assert!(fps_csv.contains(",30.00,1\n"));
        let freq_csv = std::fs::read_to_string(format!("{}/freq/freq_data.csv", dir)).unwrap();
        assert!(freq_csv.starts_with("Timestamp,cpu0 (MHz),cpu1 (MHz)\n"));
        assert!(freq_csv.contains(",2592,2246\n"));
        let temp_csv = std::fs::read_to_string(format!("{}/thermal/thermal_data.csv", dir)).unwrap();
        assert!(temp_csv.starts_with("Timestamp,Status,Sensor,TempC\n"));
        assert!(temp_csv.contains(",0,soc0,42.5\n"));
        let gpu_csv = std::fs::read_to_string(format!("{}/gpu/gpu_data.csv", dir)).unwrap();
        assert!(gpu_csv.starts_with("Timestamp,Busy (%),Util (%),Clock (MHz)\n"));
        assert!(gpu_csv.contains(",37.50,33.80,585\n"));
        let gpuproc_csv = std::fs::read_to_string(format!("{}/gpu/gpu_proc_1234_data.csv", dir)).unwrap();
        assert!(gpuproc_csv.starts_with("Timestamp,Busy (%)\n"));
        assert!(gpuproc_csv.contains(",14.40\n"));
        let io_csv = std::fs::read_to_string(format!("{}/io/io_1234_data.csv", dir)).unwrap();
        assert!(io_csv.starts_with("Timestamp,Read (KB/s),Write (KB/s),Disk Read (KB/s),Disk Write (KB/s)\n"));
        assert!(io_csv.contains(",12.00,3.00,0.00,1.00\n"));
        let net_csv = std::fs::read_to_string(format!("{}/net/net_data.csv", dir)).unwrap();
        assert!(net_csv.starts_with("Timestamp,RX (KB/s),TX (KB/s)\n"));
        assert!(net_csv.contains(",123.00,45.00\n"));
        let gpumem_csv = std::fs::read_to_string(format!("{}/gpumem/gpumem_1234_data.csv", dir)).unwrap();
        assert!(gpumem_csv.starts_with("Timestamp,Process GPU Mem (MB),Global GPU Mem (MB)\n"));
        assert!(gpumem_csv.contains(",154.4,2639\n"));
        std::fs::remove_dir_all(gui_data_root().join(&pkg)).ok();
    }

    #[tokio::test]
    async fn test_export_csv_empty_errors() {
        let r = export_csv(
            "x".into(),
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
        ).await;
        assert!(r.is_err());
    }
}

