//! xperf-gui：xtools 的 Tauri 桌面 GUI——与 CLI 共用 xperf-core 的采样/深挖能力。
//! 多设备并行：每台在线设备一个独立会话（顶栏设备 tab），各自持有采样/Perfetto/
//! simpleperf 三路控制与状态，事件 payload 均带 `serial` 供前端分发。
//! 支持命令行自动启动：`--package <pkg> [--device <serial>] [--interval N] [--cpu …] [--trace N] [--stack N]`。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::HashMap;
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
/// 采样在设备端 agent 进行，本线程只阻塞读事件流并转发给前端；
/// 全部 adb 调用带 `-s <serial>` 路由到该设备（多设备并行互不干扰）。
/// `sample` 事件 payload：`{serial, event}`（前端按 serial 分发到对应设备页）。
fn spawn_sampling(app: tauri::AppHandle, serial: String, package: String, interval: u64, flags: MetricFlags, running: Arc<Mutex<bool>>) {
    eprintln!("[sampling] 启动: device={} package={} interval={} flags={:?}", serial, package, interval, flags);
    // 记录当前采样包名与启动参数（startup_sessions 回查给前端回填输入框/勾选）
    if let Some(state) = app.try_state::<AppState>() {
        state.record_startup(&serial, &package, interval, flags);
    }
    let emit_error = {
        let app = app.clone();
        let serial = serial.clone();
        move |message: String| {
            let _ = app.emit("sampling-error", serde_json::json!({ "serial": serial, "message": message }));
        }
    };
    std::thread::spawn(move || {
        let platform = xperf_core::detect_platform_live(Some(&serial));
        eprintln!("[sampling] 平台: {} ({})", platform.name(), platform.description());
        let bin = match agent::ensure_agent_built() {
            Ok(b) => b,
            Err(e) => {
                eprintln!("[sampling] agent 构建失败: {}", e);
                emit_error(format!("agent 构建失败: {}", e));
                let mut running = running.lock().unwrap();
                *running = false;
                return;
            }
        };
        if let Err(e) = agent::deploy_agent(&bin, Some(&serial)) {
            eprintln!("[sampling] agent 部署失败: {}", e);
            emit_error(format!("agent 部署失败: {}", e));
            let mut running = running.lock().unwrap();
            *running = false;
            return;
        }
        let mut stream = match agent::spawn_agent(Some(&package), interval, flags, Some(&*platform), Some(&serial)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[sampling] agent 启动失败: {}", e);
                emit_error(format!("agent 启动失败: {}", e));
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
                        let _ = app.emit("sample", serde_json::json!({ "serial": serial, "event": sev }));
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
                        Some(&serial),
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
            agent::qnx_stop_stats(&*platform, interval, Some(&serial));
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

/// 单台设备的会话状态（多设备并行：每台在线设备一个独立会话，
/// 采样/Perfetto/simpleperf 三路控制互不干扰）。
#[derive(Clone)]
struct DeviceSession {
    /// 采样进行中
    running: Arc<Mutex<bool>>,
    /// 深挖录制进行中（与采样互不干扰，可并行）
    trace_running: Arc<Mutex<bool>>,
    /// 函数热点录制进行中（与采样/深挖互不干扰，可并行）
    stack_running: Arc<Mutex<bool>>,
    /// 当前采样包名（`--package` 自动启动与手动开始均写入；`startup_sessions` 回查用）
    package: String,
    /// 采样启动参数（间隔 + 指标 flags，与 `package` 同一写入点；None = 非本进程启动）
    startup_extra: Option<(u64, MetricFlags)>,
}

impl DeviceSession {
    fn new() -> Self {
        Self {
            running: Arc::new(Mutex::new(false)),
            trace_running: Arc::new(Mutex::new(false)),
            stack_running: Arc::new(Mutex::new(false)),
            package: String::new(),
            startup_extra: None,
        }
    }
}

/// 全部设备会话（serial → 会话）。命令按 serial 定位会话；
/// 首次触达的 serial 自动建会话（设备在线性由命令前置校验保证）。
struct AppState {
    sessions: Mutex<HashMap<String, DeviceSession>>,
}

impl AppState {
    /// 取（无则建）指定设备的会话，返回其克隆（Arc/字段均克隆，锁内不持有）
    fn session(&self, serial: &str) -> DeviceSession {
        let mut map = self.sessions.lock().unwrap();
        map.entry(serial.to_string()).or_insert_with(DeviceSession::new).clone()
    }

    /// 更新指定设备会话的启动记录（包名 + 间隔 + flags；无则先建会话）。
    /// `--package` 自动启动与手动「开始监控」均经此写入，`startup_sessions` 回查。
    fn record_startup(&self, serial: &str, package: &str, interval: u64, flags: MetricFlags) {
        if let Ok(mut map) = self.sessions.lock() {
            let s = map.entry(serial.to_string()).or_insert_with(DeviceSession::new);
            s.package = package.to_string();
            s.startup_extra = Some((interval, flags));
        }
    }
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

/// 设备在线性校验（命令入口用）：serial 须在当前在线列表中，
/// 不在线返回带设备清单的错误（前端展示给用户）。
fn ensure_device_online(serial: &str) -> Result<(), String> {
    let devices = xperf_core::list_adb_devices().map_err(|e| e.to_string())?;
    if devices.iter().any(|d| d.serial == serial) {
        Ok(())
    } else {
        Err(format!(
            "设备 {} 不在线（当前在线：{}）",
            serial,
            if devices.is_empty() { "无".to_string() } else { devices.iter().map(|d| d.serial.as_str()).collect::<Vec<_>>().join(", ") }
        ))
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
    serial: String,
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
    let session = state.session(&serial);
    {
        let mut running = session.running.lock().map_err(|e| e.to_string())?;
        if *running {
            return Err("已在监控中，请先停止".into());
        }
        *running = true;
    }
    // 前置校验失败时回滚 running
    let bail = |session: &DeviceSession, e: String| -> String {
        *session.running.lock().unwrap() = false;
        e
    };
    if let Err(e) = validate_package(&package) {
        return Err(bail(&session, e));
    }
    if let Err(e) = ensure_device_online(&serial) {
        return Err(bail(&session, e));
    }

    let flags = MetricFlags { cpu, memory, fps, freq, thermal, gpu, io, net };
    spawn_sampling(app, serial, package, interval, flags, session.running.clone());

    Ok(())
}

/// 停止指定设备的采样（`running` 置 false，采样线程下一轮检测到后退出并清理）
#[tauri::command]
fn stop_sampling(serial: String, state: State<'_, AppState>) -> Result<(), String> {
    let session = state.session(&serial);
    let mut running = session.running.lock().map_err(|e| e.to_string())?;
    *running = false;
    Ok(())
}

/// 后台深挖线程（start_trace 命令与 --trace 自动启动共用）：
/// 录制 → 拉回 → trace_processor SQL 分析，全程 emit("trace") 推进度（payload 带
/// serial，前端分发到对应设备页）。stage: recording / progress（每秒，message 含已
/// 录制秒数）/ recorded（已拉回，分析中）/ done（message=完整报告文本）/ error；
/// recorded/done 附 trace_path（前端"在浏览器打开 Perfetto UI"按钮用）。
/// 与采样会话互不干扰（可并行；GUI 采样不限时，窗口对照靠报告与图表的时间戳）。
/// 落盘目录 `<pkg>/<ts>-<serial>`（serial 后缀防双设备同秒录制撞目录）。
fn spawn_trace(app: tauri::AppHandle, serial: String, package: String, seconds: u64, running: Arc<Mutex<bool>>) {
    eprintln!("[trace] 启动: device={} package={} seconds={}", serial, package, seconds);
    std::thread::spawn(move || {
        let emit_stage = |stage: &str, message: String, trace_path: Option<String>| {
            let _ = app.emit(
                "trace",
                serde_json::json!({ "serial": serial, "stage": stage, "message": message, "trace_path": trace_path }),
            );
        };
        let dir = gui_data_root()
            .join(&package)
            .join(format!("{}-{}", Local::now().format("%Y%m%d_%H%M%S"), serial))
            .join("trace");
        emit_stage("recording", format!("录制 {}s perfetto trace…（窗口内操作被测应用）", seconds), None);
        let progress = |elapsed: u64| {
            eprintln!("[trace] progress: {}s", elapsed);
            let _ = app.emit(
                "trace",
                serde_json::json!({
                    "serial": serial,
                    "stage": "progress",
                    "message": format!("perfetto 录制中 {}/{}s", elapsed.min(seconds), seconds),
                    "trace_path": null
                }),
            );
        };
        let rec = match xperf_core::trace::record(seconds, &dir, Some(&progress), Some(&serial)) {
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
    serial: String,
    package: String,
    seconds: u64,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let session = state.session(&serial);
    {
        let mut t = session.trace_running.lock().map_err(|e| e.to_string())?;
        if *t {
            return Err("Perfetto 分析进行中，请等待完成".into());
        }
        *t = true;
    }
    let bail = |session: &DeviceSession, e: String| -> String {
        *session.trace_running.lock().unwrap() = false;
        e
    };
    if let Err(e) = validate_package(&package) {
        return Err(bail(&session, e));
    }
    if let Err(e) = ensure_device_online(&serial) {
        return Err(bail(&session, e));
    }
    let seconds = seconds.clamp(1, 600);
    spawn_trace(app, serial, package, seconds, session.trace_running.clone());
    Ok(())
}

/// 后台函数热点线程（start_stack 命令与 --stack 自动启动共用）：
/// 录制 → 设备端三视图报告 → 拉回 → 渲染热点报告，全程 emit("stack") 推进度
/// （payload 带 serial，前端分发到对应设备页）。stage: recording / progress（每秒，
/// message 含已录制秒数）/ recorded（已拉回，渲染报告中）/ done（message=完整报告
/// 文本）/ error；recorded/done/error 附 data_path（`.data` 文件路径，前端
/// "在浏览器打开火焰图"按钮用）。
/// 与采样/trace 会话互不干扰（可并行；GUI 采样不限时，窗口对照靠报告与图表的时间戳）。
/// 落盘目录 `<pkg>/<ts>-<serial>`（serial 后缀防双设备同秒录制撞目录）。
fn spawn_stack(app: tauri::AppHandle, serial: String, package: String, seconds: u64, running: Arc<Mutex<bool>>) {
    eprintln!("[stack] 启动: device={} package={} seconds={}", serial, package, seconds);
    std::thread::spawn(move || {
        let emit_stage = |stage: &str, message: String, data_path: Option<String>| {
            let _ = app.emit(
                "stack",
                serde_json::json!({ "serial": serial, "stage": stage, "message": message, "data_path": data_path }),
            );
        };
        let dir = gui_data_root()
            .join(&package)
            .join(format!("{}-{}", Local::now().format("%Y%m%d_%H%M%S"), serial))
            .join("stack");
        emit_stage("recording", format!("录制 {}s 调用栈…（窗口内操作被测应用）", seconds), None);
        let progress = |elapsed: u64| {
            eprintln!("[stack] progress: {}s", elapsed);
            let _ = app.emit(
                "stack",
                serde_json::json!({
                    "serial": serial,
                    "stage": "progress",
                    "message": format!("调用栈录制中 {}/{}s", elapsed.min(seconds), seconds),
                    "data_path": null
                }),
            );
        };
        let rec = match xperf_core::simpleperf::record(seconds, &package, &dir, Some(&progress), Some(&serial)) {
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
    serial: String,
    package: String,
    seconds: u64,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let session = state.session(&serial);
    {
        let mut t = session.stack_running.lock().map_err(|e| e.to_string())?;
        if *t {
            return Err("函数热点录制进行中，请等待完成".into());
        }
        *t = true;
    }
    let bail = |session: &DeviceSession, e: String| -> String {
        *session.stack_running.lock().unwrap() = false;
        e
    };
    if let Err(e) = validate_package(&package) {
        return Err(bail(&session, e));
    }
    if let Err(e) = ensure_device_online(&serial) {
        return Err(bail(&session, e));
    }
    let seconds = seconds.clamp(1, 600);
    spawn_stack(app, serial, package, seconds, session.stack_running.clone());
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

/// 列出指定设备上已安装的全部应用包名（含系统应用，共几百个），供前端搜索选择。
/// 用 `pm list packages`（不带 -3，-3 只列第三方会遗漏系统应用）。
#[tauri::command]
async fn list_packages(serial: String) -> Result<Vec<String>, String> {
    let out = xperf_core::run_adb_command_for(Some(&serial), &["shell", "pm", "list", "packages"])
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

/// 查询指定设备采样是否正在运行（前端据此设置开始/停止按钮状态）。
#[tauri::command]
fn is_running(serial: String, state: State<'_, AppState>) -> Result<bool, String> {
    let session = state.session(&serial);
    let running = session.running.lock().map_err(|e| e.to_string())?;
    Ok(*running)
}

/// 打开指定设备上的应用并测量冷启动（`am start -W`）。
///
/// `activity` 留空时自动解析包的主入口（`cmd package resolve-activity --brief`）；
/// 支持 `.MainActivity` 相对写法或完整类名。阻塞数秒（应用启动耗时），async 不卡 UI。
/// 返回 [`xperf_core::coldstart::ColdStartResult`]（TotalTime/WaitTime 等，前端展示）。
#[tauri::command]
async fn launch_app(serial: String, package: String, activity: String) -> Result<xperf_core::coldstart::ColdStartResult, String> {
    validate_package(&package)?;
    ensure_device_online(&serial)?;
    xperf_core::coldstart::measure(&package, &activity, Some(&serial)).map_err(|e| e.to_string())
}

/// 重启指定设备上的应用并测量冷启动：force-stop → 等进程死透（800ms）→
/// `am start -W`。应用已在采样监控中时，重启后 agent 端自动重扫包名进程
/// （exit 事件 + 新 PID 发现），前端时序/峰值保留。
#[tauri::command]
async fn restart_app(serial: String, package: String, activity: String) -> Result<xperf_core::coldstart::ColdStartResult, String> {
    validate_package(&package)?;
    ensure_device_online(&serial)?;
    xperf_core::coldstart::force_stop(&package, Some(&serial)).map_err(|e| e.to_string())?;
    // force-stop 异步杀进程，立即 start 会测到残留路径；800ms 缓冲进程死透
    std::thread::sleep(std::time::Duration::from_millis(800));
    xperf_core::coldstart::measure(&package, &activity, Some(&serial)).map_err(|e| e.to_string())
}

/// 在线设备清单（顶栏设备 tab 用）：`{devices: [{serial, model, version}]}`
#[tauri::command]
fn list_devices() -> Result<serde_json::Value, String> {
    let devices = xperf_core::list_adb_devices().map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "devices": devices
            .into_iter()
            .map(|d| serde_json::json!({ "serial": d.serial, "model": d.model, "version": d.android_version }))
            .collect::<Vec<_>>(),
    }))
}

/// 设备热插拔监视线程：每 3s 轮询 `adb devices -l`，与上次快照 diff，有变化时
/// emit `devices-changed` 事件：`{devices: [{serial, model, version}], added, removed}`。
/// 首轮只建立快照不通知（首屏由前端 loadDevices 填充，避免重复提示）。
/// adb 暂不可用（如 server 重启中）跳过本轮；线程随进程存活。已移除设备的会话
/// 数据保留在前端（设备页不销毁，插回后采样线程自动重连恢复）。
fn spawn_device_monitor(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        let mut last: Vec<xperf_core::AdbDevice> = Vec::new();
        let mut first_round = true;
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3));
            let devices = match xperf_core::list_adb_devices() {
                Ok(d) => d,
                Err(_) => continue, // adb 暂不可用，下轮重试
            };
            let (added, removed) = xperf_core::diff_devices(&last, &devices);
            if first_round || (added.is_empty() && removed.is_empty()) {
                last = devices;
                first_round = false;
                continue;
            }
            eprintln!(
                "[devices] 热插拔: 接入 [{}]，移除 [{}]",
                added.join(","),
                removed.join(",")
            );
            let _ = app.emit(
                "devices-changed",
                serde_json::json!({
                    "devices": devices.iter().map(|d| serde_json::json!({
                        "serial": d.serial, "model": d.model, "version": d.android_version,
                    })).collect::<Vec<_>>(),
                    "added": added,
                    "removed": removed,
                }),
            );
            last = devices;
        }
    });
}

/// 启动参数回查（`--package` 等命令行自动启动时前端回填输入框用）：
/// 返回 `{serial: {package, interval, flags}}`——全部运行中的采样会话（多设备
/// 并行时可有多台）。前端据此把对应设备页的 UI 状态（包名/间隔/勾选/idleHint/
/// 按钮）同步成与手动「开始监控」一致的效果。
#[tauri::command]
fn startup_sessions(
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let map = state.sessions.lock().map_err(|e| e.to_string())?;
    let mut out = serde_json::Map::new();
    for (serial, s) in map.iter() {
        let Some((interval, flags)) = s.startup_extra else { continue };
        // MetricFlags 未派生 Serialize（core 协议类型，避免 serde 依赖），手动展开
        out.insert(
            serial.clone(),
            serde_json::json!({
                "package": s.package,
                "interval": interval,
                "flags": {
                    "cpu": flags.cpu, "memory": flags.memory, "fps": flags.fps,
                    "freq": flags.freq, "thermal": flags.thermal, "gpu": flags.gpu,
                    "io": flags.io, "net": flags.net,
                }
            }),
        );
    }
    Ok(serde_json::Value::Object(out))
}

/// IO 导出行：(ms, r, w, dr, dw) KB/s
type IoExportPoints = Vec<(f64, f64, f64, f64, f64)>;
/// GPU 显存导出行：(ms, 进程 MB, 整机 MB)
type GpuMemExportPoints = Vec<(f64, f64, f64)>;
/// GPU 系统导出行：(ms, busy%, util%, mhz)
type GpuExportPoints = Vec<(f64, f64, f64, u32)>;

/// 导出前端持有的完整会话历史为 CSV（GUI 不流式落盘，数据在前端内存中）。
/// 写到 `log/<pkg>/<导出时刻>/` 下的各指标子目录，返回目录路径。
/// cpu/mem: pid -> [[ms, value]...]（mem 为 MB）；fps: 图层短名 -> [[ms, fps, jank]...]
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
    // 包名拼入数据目录路径（/tmp/xperf/<pkg>/<ts>），须与 start_sampling 等同一校验
    validate_package(&package)?;
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
            // 前端 memChart 序列已按 MB 传入（单位统一，与实时面板/峰值一致）
            let mut f = std::fs::File::create(d.join(format!("memory_{}_data.csv", pid))).map_err(|e| e.to_string())?;
            writeln!(f, "Timestamp,Total PSS (MB)").map_err(|e| e.to_string())?;
            for (t, v) in pts {
                writeln!(f, "{},{:.1}", fmt_ts(*t), v).map_err(|e| e.to_string())?;
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

/// 前端持有的会话序列 → 基线汇总（`export_csv` 同源数据结构，多 PID 合并，
/// 口径见 `xperf_core::baseline::SessionSummary` 文档）。
/// 时长取全序列首/末样本的全局跨度；无任何数据时报错（与 export_csv 空数据行为一致）。
#[allow(clippy::too_many_arguments)] // tauri 命令参数须扁平（export_csv 同源），此处为其共用实现
fn build_summary_from_series(
    package: &str,
    interval_ms: u64,
    cpu: &std::collections::HashMap<String, Vec<(f64, f64)>>,
    mem: &std::collections::HashMap<String, Vec<(f64, f64)>>,
    fps: &std::collections::HashMap<String, Vec<(f64, f64, u32)>>,
    gpu: &GpuExportPoints,
    io: &std::collections::HashMap<String, IoExportPoints>,
    net: &[(f64, f64, f64)],
) -> Result<xperf_core::baseline::SessionSummary, String> {
    validate_package(package)?;
    // 会话时长（秒）：全序列首/末样本的全局跨度
    let mut first = f64::MAX;
    let mut last = f64::MIN;
    let mut note = |t0: f64, tn: f64| {
        first = first.min(t0);
        last = last.max(tn);
    };
    for pts in cpu.values().chain(mem.values()) {
        if let (Some((t0, _)), Some((tn, _))) = (pts.first(), pts.last()) {
            note(*t0, *tn);
        }
    }
    for pts in fps.values() {
        if let (Some((t0, _, _)), Some((tn, _, _))) = (pts.first(), pts.last()) {
            note(*t0, *tn);
        }
    }
    if let (Some(t0), Some(tn)) = (gpu.first(), gpu.last()) {
        note(t0.0, tn.0);
    }
    for pts in io.values() {
        if let (Some((t0, _, _, _, _)), Some((tn, _, _, _, _))) = (pts.first(), pts.last()) {
            note(*t0, *tn);
        }
    }
    if let (Some(t0), Some(tn)) = (net.first(), net.last()) {
        note(t0.0, tn.0);
    }

    let total_points: usize = cpu.values().map(|p| p.len()).sum::<usize>()
        + mem.values().map(|p| p.len()).sum::<usize>()
        + fps.values().map(|p| p.len()).sum::<usize>()
        + gpu.len()
        + io.values().map(|p| p.len()).sum::<usize>()
        + net.len();
    if total_points == 0 {
        return Err("暂无采样数据（先开始监控）".into());
    }
    let duration_s = if first <= last { (last - first) / 1000.0 } else { 0.0 };

    // PID 列表：CPU/内存序列的键取并集排序
    let mut pids: Vec<String> = cpu.keys().chain(mem.keys()).cloned().collect();
    pids.sort();
    pids.dedup();

    let mut b = xperf_core::baseline::SummaryBuilder::new(package, interval_ms, duration_s);
    b.pids(pids);
    for pts in cpu.values() {
        for (_, v) in pts {
            b.push_cpu(*v);
        }
    }
    for pts in mem.values() {
        for (_, v) in pts {
            // 前端传入 MB（单位统一）；基线 JSON 的 mem_pss_kb 字段语义为 KB，转回存储
            b.push_mem(v * 1024.0);
        }
    }
    for pts in fps.values() {
        for (_, v, jank) in pts {
            b.push_fps(*v, *jank);
        }
    }
    for (_, busy, _, _) in gpu {
        b.push_gpu(*busy);
    }
    for pts in io.values() {
        for (_, r, w, _, _) in pts {
            b.push_io(*r, *w);
        }
    }
    for (_, rx, tx) in net {
        b.push_net(*rx, *tx);
    }
    Ok(b.finish())
}

/// 保存基线：把前端当前会话序列的汇总统计存为该包的基线
/// （`~/.local/share/xperf/baselines/<pkg>.json`，覆盖旧基线；与 CLI `--save-baseline` 互通）。
/// 返回基线文件路径。
#[tauri::command]
#[allow(clippy::too_many_arguments)] // tauri 命令参数须扁平，与 export_csv 同源结构
async fn save_baseline(
    package: String,
    interval_ms: u64,
    cpu: std::collections::HashMap<String, Vec<(f64, f64)>>,
    mem: std::collections::HashMap<String, Vec<(f64, f64)>>,
    fps: std::collections::HashMap<String, Vec<(f64, f64, u32)>>,
    gpu: GpuExportPoints,
    io: std::collections::HashMap<String, IoExportPoints>,
    net: Vec<(f64, f64, f64)>,
) -> Result<String, String> {
    let summary = build_summary_from_series(&package, interval_ms, &cpu, &mem, &fps, &gpu, &io, &net)?;
    let path = xperf_core::baseline::save(&package, &summary).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

/// 对比基线：当前会话汇总与已保存基线逐指标 diff，返回报告文本
/// （基线可来自本按钮保存或 CLI `--save-baseline`，两侧互通）。
#[tauri::command]
#[allow(clippy::too_many_arguments)] // tauri 命令参数须扁平，与 export_csv 同源结构
async fn compare_baseline(
    package: String,
    interval_ms: u64,
    cpu: std::collections::HashMap<String, Vec<(f64, f64)>>,
    mem: std::collections::HashMap<String, Vec<(f64, f64)>>,
    fps: std::collections::HashMap<String, Vec<(f64, f64, u32)>>,
    gpu: GpuExportPoints,
    io: std::collections::HashMap<String, IoExportPoints>,
    net: Vec<(f64, f64, f64)>,
) -> Result<String, String> {
    let cur = build_summary_from_series(&package, interval_ms, &cpu, &mem, &fps, &gpu, &io, &net)?;
    let base = xperf_core::baseline::load(&package)
        .map_err(|e| format!("未找到基线（先点「保存基线」或 CLI --save-baseline 保存一次）: {}", e))?;
    Ok(xperf_core::baseline::compare(&base, &cur))
}

/// 按屏幕尺寸设置默认窗口大小（前端加载完成后调用一次）。
///
/// 策略：宽 72%（侧栏 280 + 图表区，≥1080 才完整；≤1600 防大屏过大），
/// 高 88%（≥880 侧栏全量免滚动；≤1280），并各留屏幕边距防超出。
/// 放在命令里而非 setup：setup 阶段 webview 未就绪直接 `set_size` 会导致
/// webkit2gtk 渲染空白（真机实测）；conf 里的固定尺寸作为检测失败兜底。
#[tauri::command]
async fn resize_default(app: tauri::AppHandle) -> Result<(), String> {
    let win = app.get_webview_window("main").ok_or("无主窗口")?;
    let monitor = win.current_monitor().ok().flatten().ok_or("无显示器信息")?;
    let scale = monitor.scale_factor();
    let sw = monitor.size().width as f64 / scale;
    let sh = monitor.size().height as f64 / scale;
    // 宽高各自取「比例值」与「屏幕减边距」的较小者，再 clamp 到内容需求下限
    let w = (sw * 0.72).min(sw - 80.0).max(960.0);
    let h = (sh * 0.88).min(sh - 120.0).max(600.0);
    win.set_size(tauri::LogicalSize::new(w, h))
        .map_err(|e| e.to_string())
}

fn main() {
    // webkit2gtk 的 DMABUF 渲染路径在本机间歇性空白（窗口仅标题栏、内容全灰，
    // 浏览器等 GPU 重负载应用占用时触发，2026-09-04 实测连续复现）；
    // 进程内禁用 DMABUF renderer 实测恢复。须在任何 webview 初始化前设置，
    // 故放在 main 最开头（此时单线程，set_var 安全）。
    std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
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
    // --device <serial>：命令行自动启动指定目标设备（多台同连时必须给）；
    // 显式指定但无效时直接跳过自动启动（不静默回落到自动选台——用户指定了设备）
    let mut auto_serial: Option<String> = None;
    let mut auto_start = true;
    if let Some(serial) = get_opt("--device") {
        match xperf_core::pick_device(Some(&serial), &xperf_core::list_adb_devices().unwrap_or_default()) {
            Ok(d) => auto_serial = Some(d.serial),
            Err(e) => {
                eprintln!("[startup] --device 无效，跳过自动启动: {}", e);
                auto_start = false;
            }
        }
    }
    // 未给 --device 时的设备前置解析：单台自动；多台/无设备则放弃自动启动
    // （确定性跳过，脚本化多设备场景须显式 --device；交互场景在各设备页手动开始）
    if auto_start && auto_package.is_some() && auto_serial.is_none() {
        match xperf_core::list_adb_devices()
            .ok()
            .and_then(|ds| xperf_core::pick_device(None, &ds).ok())
        {
            Some(d) => {
                auto_serial = Some(d.serial);
            }
            None => {
                eprintln!(
                    "[startup] --package 自动启动跳过：多台/无在线设备且未指定 --device（须在设备页选定后手动开始，或启动参数加 --device）"
                );
                auto_start = false;
            }
        }
    }
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
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            sessions: Mutex::new(HashMap::new()),
        })
        .setup(move |app| {
            // 默认窗口大小：前端加载完成后经 resize_default 命令按屏幕动态设置
            // （setup 阶段 webview 未就绪直接 set_size 会导致渲染空白，真机实测）
            // 设备热插拔监视线程（devices-changed 事件 → 前端设备 tab 动态更新）
            spawn_device_monitor(app.handle().clone());
            // auto_start：设备前置解析通过（--device 或单台自动）才自动启动采样；
            // 多台未指定时为 false（eprintln 已提示），前端保持空闲态等用户在设备页开始
            if auto_start {
                if let Some(package) = auto_package.clone() {
                    if let Some(serial) = auto_serial.clone() {
                        let state = app.state::<AppState>();
                        let session = state.session(&serial);
                        let mut running = session.running.lock().unwrap();
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
                            serial.clone(),
                            package.clone(),
                            auto_interval,
                            flags,
                            session.running.clone(),
                        );
                        // 深挖自动启动（--trace N，可与采样并行）
                        if let Some(n) = auto_trace {
                            let n = n.clamp(1, 600);
                            let mut t = session.trace_running.lock().unwrap();
                            if !*t {
                                *t = true;
                            drop(t);
                            spawn_trace(app.handle().clone(), serial.clone(), package.clone(), n, session.trace_running.clone());
                            }
                        }
                        // 函数热点自动启动（--stack N，可与采样/深挖并行）
                        if let Some(n) = auto_stack {
                            let n = n.clamp(1, 600);
                            let mut t = session.stack_running.lock().unwrap();
                            if !*t {
                                *t = true;
                            drop(t);
                            spawn_stack(app.handle().clone(), serial, package, n, session.stack_running.clone());
                            }
                        }
                        }
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
            startup_sessions,
            is_running,
            launch_app,
            restart_app,
            export_csv,
            save_baseline,
            compare_baseline,
            list_devices,
            resize_default
        ])
        .on_window_event(|window, event| {
            // 关窗时停止全部设备采样：各会话 running 置 false，采样线程在下一轮
            // 循环检测到后退出，exec-out 管道断开 → 设备端 agent 因 stdout 写失败自行退出
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                // 深挖录制线程不等待（最长 600s），置中断标志让其尽快退出；
                // 未及退出时设备端 perfetto 由 traced TTL 兜底停止（残留文件无害）
                xperf_core::utils::set_interrupt_flag();
                let state = window.state::<AppState>();
                let mut any_running = false;
                if let Ok(map) = state.sessions.lock() {
                    for s in map.values() {
                        let mut running = s.running.lock().unwrap();
                        if *running {
                            *running = false;
                            any_running = true;
                        }
                    }
                }
                if any_running {
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

    // ---- 多设备会话状态隔离 ----

    #[test]
    fn test_app_state_sessions_per_device() {
        let state = AppState { sessions: Mutex::new(HashMap::new()) };
        let a1 = state.session("devA");
        let b1 = state.session("devB");
        assert!(!*a1.running.lock().unwrap());
        // 同设备两次取会话共享状态（Arc 同一）；不同设备各自独立
        let a2 = state.session("devA");
        assert!(Arc::ptr_eq(&a1.running, &a2.running));
        assert!(!Arc::ptr_eq(&a1.running, &b1.running));
        // record_startup 只写对应设备；对未启动设备无副作用
        state.record_startup(
            "devA",
            "com.example.app",
            500,
            MetricFlags { cpu: true, memory: true, ..MetricFlags::default() },
        );
        let map = state.sessions.lock().unwrap();
        let a = map.get("devA").unwrap();
        assert_eq!(a.package, "com.example.app");
        assert_eq!(a.startup_extra.unwrap().0, 500);
        assert!(map.get("devB").unwrap().startup_extra.is_none());
    }

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

    #[test]
    fn test_build_summary_from_series() {
        let pkg = format!("test_baseline_{}", std::process::id());
        let mut cpu = std::collections::HashMap::new();
        cpu.insert("100".to_string(), vec![(1000.0, 10.0), (2000.0, 20.0)]);
        cpu.insert("200".to_string(), vec![(1000.0, 5.0)]);
        let mut fps = std::collections::HashMap::new();
        fps.insert("SVM Container_0".to_string(), vec![(1000.0, 58.0, 1u32), (2000.0, 60.0, 0u32)]);
        let gpu = vec![(1000.0, 12.0, 0.0, 585u32)];
        let s = build_summary_from_series(&pkg, 1000, &cpu, &Default::default(), &fps, &gpu, &Default::default(), &[]).unwrap();
        // CPU 样本合并（2+1）、pids 并集排序
        assert_eq!(s.cpu.as_ref().unwrap().count, 3);
        assert!((s.cpu.as_ref().unwrap().avg - 11.666).abs() < 0.01);
        assert_eq!(s.pids, vec!["100".to_string(), "200".to_string()]);
        // 时长 = 首/末样本跨度 1000ms
        assert!((s.duration_s - 1.0).abs() < 1e-9);
        assert_eq!(s.jank_total, Some(1));
        assert_eq!(s.gpu_busy.as_ref().unwrap().max, 12.0);
        assert_eq!(s.restarts, None); // GUI 路径未统计

        // 保存/读取/对比 roundtrip（临时路径，不动用户基线目录）
        let dir = std::env::temp_dir().join(format!("xperf_gui_baseline_{}", std::process::id()));
        let path = dir.join("baseline.json");
        xperf_core::baseline::save_to(&path, &s).unwrap();
        let base = xperf_core::baseline::load_from(&path).unwrap();
        let report = xperf_core::baseline::compare(&base, &s);
        assert!(report.contains("基线对比报告"));
        assert!(report.contains("✅ 无回归"));
        std::fs::remove_dir_all(&dir).ok();

        // 空数据报错（与 export_csv 行为一致）
        let r = build_summary_from_series(&pkg, 1000, &Default::default(), &Default::default(), &Default::default(), &Default::default(), &Default::default(), &[]);
        assert!(r.is_err());
    }

    // ---- save_baseline / compare_baseline 命令端到端（写真实基线目录后清理）----

    #[tokio::test]
    async fn test_save_and_compare_baseline_commands() {
        let pkg = format!("test_baseline_cmd_{}", std::process::id());
        let mut cpu = std::collections::HashMap::new();
        cpu.insert("100".to_string(), vec![(1000.0, 12.0), (2000.0, 14.0)]);
        let cleanup = || {
            let _ = std::fs::remove_file(
                xperf_core::baseline::baseline_dir().join(format!("{}.json", pkg)),
            );
        };
        cleanup();
        // 保存：返回基线路径（用户数据目录）
        let path = save_baseline(pkg.clone(), 1000, cpu.clone(), Default::default(), Default::default(), Default::default(), Default::default(), Default::default()).await.unwrap();
        assert!(path.contains(&pkg));
        assert!(std::path::Path::new(&path).exists());
        // 对比：同数据 → 无回归
        let report = compare_baseline(pkg.clone(), 1000, cpu, Default::default(), Default::default(), Default::default(), Default::default(), Default::default()).await.unwrap();
        assert!(report.contains("基线对比报告"));
        assert!(report.contains("✅ 无回归"));
        // 空数据 → 报错（暂无采样数据——build_summary 前置校验先于基线读取）
        let r = compare_baseline(pkg.clone(), 1000, Default::default(), Default::default(), Default::default(), Default::default(), Default::default(), Default::default()).await;
        assert!(r.unwrap_err().contains("暂无采样数据"));
        cleanup();
        // 清理后无基线 → 对比报"未找到基线"（须带数据，否则前置校验先报错）
        let mut cpu2 = std::collections::HashMap::new();
        cpu2.insert("100".to_string(), vec![(1000.0, 12.0)]);
        let r = compare_baseline(pkg, 1000, cpu2, Default::default(), Default::default(), Default::default(), Default::default(), Default::default()).await;
        let e = r.unwrap_err();
        assert!(e.contains("未找到基线"));
    }
}

