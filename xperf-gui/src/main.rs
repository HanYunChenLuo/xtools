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
        AgentEvent::Hello { ncores, maxkhz, root, .. } => {
            eprintln!("[sampling] agent 已启动（{} 核，{}）", ncores, if root { "root" } else { "shell" });
            out.push(SampleEvent::AgentHello { ncores, maxkhz, root });
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
        AgentEvent::Err { msg } => {
            eprintln!("[sampling] agent: {}", msg);
            // 能力/降级类 err 透传前端状态栏（用户需要看到「IO 需 root」等提示）；
            // 节拍 overrun 警告（"round N overrun"）高频重复只进日志
            if !msg.starts_with("round ") {
                out.push(SampleEvent::SampleError { pid: None, stage: "agent".to_string(), error: msg });
            }
        }
    }
    out
}

/// 后台采样循环（start_sampling 命令与自动启动共用）。
/// 采样在设备端 agent 进行，本线程只阻塞读事件流并转发给前端；
/// 全部 adb 调用带 `-s <serial>` 路由到该设备（多设备并行互不干扰）。
/// `sample` 事件 payload：`{serial, event}`（前端按 serial 分发到对应设备页）。
fn spawn_sampling(app: tauri::AppHandle, serial: String, package: String, interval: u64, flags: MetricFlags, running: Arc<Mutex<bool>>, fresh: bool) {
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
        // 采样热路径的逐事件 stderr 摘要默认关闭（GUI 无控制台时纯开销：
        // 每事件一次 String 构造 + write 系统调用）；排查用 XPERF_DEBUG=1 打开
        let debug_events = std::env::var_os("XPERF_DEBUG").is_some();
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
        // 流式 CSV 落盘（与 CLI 同根同格式）：目录 <pkg>/<ts>-<serial>，首个样本到达
        // 时创建；同包重采复用（指标勾选重启不丢 CSV 连续性）；前端内存序列只服务
        // 图表/基线，全量数据以落盘为准
        let csv_dir = app
            .try_state::<AppState>()
            .map(|s| s.csv_dir_for(&serial, &package, fresh))
            .unwrap_or_else(|| gui_data_root().join(&package));
        let mut csv = xperf_core::csvstream::CsvStream::with_root(csv_dir);
        while *running.lock().unwrap() {
            // 批量读取：一轮节拍的多条事件（多 PID/多指标）同 burst 到达，
            // 合并为一次 emit（极端配置 50ms×3PID 下 IPC 从 ~260 次/s 降到 ~20 次/s）
            match stream.next_event_batch() {
                Ok(Some(batch)) => {
                    let mut sevs = Vec::new();
                    for ev in batch {
                        match ev {
                            Ok(ev) => sevs.extend(map_event(ev, &mut known_pids)),
                            Err(e) => eprintln!("[sampling] 协议解析失败: {}", e),
                        }
                    }
                    if sevs.is_empty() {
                        continue;
                    }
                    for sev in &sevs {
                        csv_write_event(&mut csv, &package, sev);
                    }
                    if debug_events {
                        for sev in &sevs {
                            eprintln!("[sampling] {}", brief_event(sev));
                        }
                    }
                    let _ = app.emit("sample", serde_json::json!({ "serial": serial, "events": sevs }));
                }
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
        // QNX 统计链清理兜底：正常路径由 daemon 会话 teardown 完成；
        // 仅 daemon 异常死亡（teardown 未跑）时本调用才会真正动链（详见 core 实现）
        if flags.gpu {
            agent::qnx_stop_stats(&*platform, interval, Some(&serial));
        }
    });
}

/// 单事件流式落盘一行（与 CLI 的 CsvStream 行口径完全一致）。
/// PidDiscovered/PidDisappeared/AgentHello/NoProcess/SampleError 无数值不落 CSV。
/// GUI 无 --thread 开关，线程明细不写文件（Top 线程面板是内存态）。
fn csv_write_event(csv: &mut xperf_core::csvstream::CsvStream, pkg: &str, ev: &SampleEvent) {
    match ev {
        SampleEvent::CpuUpdate { pid, timestamp, process_cpu, .. } => {
            if let Ok(p) = pid.parse() {
                csv.cpu_row(pkg, p, *timestamp, *process_cpu);
            }
        }
        SampleEvent::MemoryUpdate { pid, timestamp, details, .. } => {
            if let Ok(p) = pid.parse() {
                csv.mem_row(pkg, p, *timestamp, details);
            }
        }
        SampleEvent::FpsUpdate { pid, timestamp, layer, fps, jank_count, .. } => {
            if let Ok(p) = pid.parse() {
                csv.fps_row(pkg, p, *timestamp, layer, *fps, *jank_count);
            }
        }
        SampleEvent::FreqUpdate { timestamp, khz } => {
            let mhz: Vec<f32> = khz.iter().map(|k| *k as f32 / 1000.0).collect();
            csv.freq_row(pkg, *timestamp, &mhz);
        }
        SampleEvent::TempUpdate { timestamp, status, sensors } => {
            csv.temp_row(pkg, *timestamp, *status, sensors);
        }
        SampleEvent::GpuUpdate { timestamp, busy, util, mhz, maxmhz } => {
            csv.gpu_row(pkg, *timestamp, *busy, *util, *mhz, *maxmhz);
        }
        SampleEvent::GpuProcUpdate { pid, timestamp, busy } => {
            if let Ok(p) = pid.parse() {
                csv.gpuproc_row(pkg, p, *timestamp, *busy);
            }
        }
        SampleEvent::GpuMemUpdate { pid, timestamp, bytes, global } => {
            if let Ok(p) = pid.parse() {
                csv.gpumem_row(pkg, p, *timestamp, *bytes as f32 / 1e6, *global as f32 / 1e6);
            }
        }
        SampleEvent::IoUpdate { pid, timestamp, r, w, dr, dw } => {
            if let Ok(p) = pid.parse() {
                csv.io_row(pkg, p, *timestamp, *r, *w, *dr, *dw);
            }
        }
        SampleEvent::NetUpdate { timestamp, rx, tx } => {
            csv.net_row(pkg, *timestamp, *rx, *tx);
        }
        _ => {}
    }
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
    /// 当前采样会话的流式 CSV 目录与所属包名（`<pkg>/<ts>-<serial>`，首个样本落盘时
    /// 创建；导出 CSV = 复制该目录快照）。元组带包名：换包重采时判失配建新目录，
    /// 同包重采复用（指标勾选重启采样不丢 CSV 连续性）
    csv_dir: Arc<Mutex<Option<(String, std::path::PathBuf)>>>,
}

impl DeviceSession {
    fn new() -> Self {
        Self {
            running: Arc::new(Mutex::new(false)),
            trace_running: Arc::new(Mutex::new(false)),
            stack_running: Arc::new(Mutex::new(false)),
            package: String::new(),
            startup_extra: None,
            csv_dir: Arc::new(Mutex::new(None)),
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

    /// 取采样会话的流式 CSV 目录：fresh（手动「开始监控」=前端已重置数据）→ 新目录；
    /// 否则同包复用（指标勾选重启采样图表不重置，CSV 续写不丢连续性）、换包建新目录
    fn csv_dir_for(&self, serial: &str, package: &str, fresh: bool) -> std::path::PathBuf {
        let session = self.session(serial);
        let mut guard = session.csv_dir.lock().unwrap();
        if !fresh {
            if let Some((pkg, dir)) = &*guard {
                if pkg == package {
                    return dir.clone();
                }
            }
        }
        let dir = gui_data_root()
            .join(package)
            .join(format!("{}-{}", Local::now().format("%Y%m%d_%H%M%S"), serial));
        *guard = Some((package.to_string(), dir.clone()));
        dir
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

/// SS4 网关（MindRT）过滤：Linux 主控不是 Android 采样目标，不进任何设备
/// payload（`list_devices`/`devices_json`/热插拔监视器三处统一经此收敛——
/// 前端永远看不到网关，网关插拔也不产生 devices-changed 噪声）
fn visible_devices(devices: Vec<xperf_core::AdbDevice>) -> Vec<xperf_core::AdbDevice> {
    devices.into_iter().filter(|d| !d.is_gateway).collect()
}

/// 设备在线性校验（命令入口用）：serial 须在当前在线列表中，
/// 不在线返回带设备清单的错误（前端展示给用户）。SS4 网关（MindRT）拒绝采样，
/// 错误信息指引其桥接出的 Android 伪设备。
fn ensure_device_online(serial: &str) -> Result<(), String> {
    let devices = xperf_core::list_adb_devices().map_err(|e| e.to_string())?;
    match devices.iter().find(|d| d.serial == serial) {
        Some(d) if d.is_gateway => {
            let android = xperf_core::bridge::gateway_android_serial(serial)
                .unwrap_or_else(|| "localhost:5559".to_string());
            Err(format!(
                "{} 是 SS4 MindRT 网关（Linux 主控，非采样目标），请选择 {}（Android）",
                serial, android
            ))
        }
        Some(_) => Ok(()),
        None => Err(format!(
            "设备 {} 不在线（当前在线：{}）",
            serial,
            if devices.is_empty() { "无".to_string() } else { devices.iter().map(|d| d.serial.as_str()).collect::<Vec<_>>().join(", ") }
        )),
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
    fresh: bool,
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
    spawn_sampling(app, serial, package, interval, flags, session.running.clone(), fresh);

    Ok(())
}

/// 停止指定设备的采样（`running` 置 false，采样线程下一轮检测到后退出并清理）。
/// 会话不存在时静默成功（幂等；前端状态由自身管理，不回读）。
#[tauri::command]
fn stop_sampling(serial: String, state: State<'_, AppState>) -> Result<(), String> {
    if let Ok(map) = state.sessions.lock() {
        if let Some(session) = map.get(&serial) {
            let mut running = session.running.lock().map_err(|e| e.to_string())?;
            *running = false;
        }
    }
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
/// `~/.cache/xperf`（perfetto UI 镜像；simpleperf 脚本集已 vendor 进仓库不受影响）
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

/// 更新 simpleperf 火焰图脚本与双平台 report 库（从 AOSP 强制重新拉取，覆盖
/// `xperf-core/simpleperf_scripts/` 的 vendor 文件；git 提交后同步到其他机器）。
/// 逐 MB 进度经 `scripts-update` 事件推给前端（stage: progress/done；percent 为
/// 基于既有 vendor 文件大小的总体百分比，首装无参照时为 null → 只显示字节计数）。
#[tauri::command]
async fn update_simpleperf_scripts(app: tauri::AppHandle) -> Result<String, String> {
    let emit =
        |message: String, percent: Option<f64>| {
            let _ = app.emit(
                "scripts-update",
                serde_json::json!({ "stage": "progress", "message": message, "percent": percent }),
            );
        };
    let progress = |p: &xperf_core::simpleperf::ScriptsDownloadProgress| {
        let percent = p
            .overall_expected
            .map(|t| ((p.overall_bytes as f64 / t as f64) * 100.0).clamp(0.0, 100.0));
        let msg = match p.overall_expected {
            Some(t) => format!(
                "更新中 {:.1}/{:.1} MB · {}/{} {}",
                p.overall_bytes as f64 / 1e6,
                t as f64 / 1e6,
                p.index,
                p.files,
                p.rel
            ),
            None => format!(
                "更新中 {}/{} {}: 已下载 {:.1} MB",
                p.index,
                p.files,
                p.rel,
                p.bytes as f64 / 1e6
            ),
        };
        emit(msg, percent);
    };
    let msg = xperf_core::simpleperf::update_simpleperf_scripts(Some(&progress))
        .map_err(|e| e.to_string())?;
    let _ = app.emit(
        "scripts-update",
        serde_json::json!({ "stage": "done", "message": msg }),
    );
    Ok(msg)
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

/// 重启指定设备上的应用并测量冷启动：解析主入口（activity 留空时，**先于
/// force-stop**——解析失败不杀应用）→ force-stop → 等进程死透（800ms）→
/// `am start -W`。应用已在采样监控中时，重启后 agent 端自动重扫包名进程
/// （exit 事件 + 新 PID 发现），前端时序/峰值保留。
#[tauri::command]
async fn restart_app(serial: String, package: String, activity: String) -> Result<xperf_core::coldstart::ColdStartResult, String> {
    validate_package(&package)?;
    ensure_device_online(&serial)?;
    // activity 留空时先解析主入口：resolve 失败则不 force-stop（避免应用被杀未拉起）
    let activity = if activity.is_empty() {
        xperf_core::coldstart::resolve_activity(&package, Some(&serial)).map_err(|e| e.to_string())?
    } else {
        activity
    };
    xperf_core::coldstart::force_stop(&package, Some(&serial)).map_err(|e| e.to_string())?;
    // force-stop 异步杀进程，立即 start 会测到残留路径；800ms 缓冲进程死透
    std::thread::sleep(std::time::Duration::from_millis(800));
    xperf_core::coldstart::measure(&package, &activity, Some(&serial)).map_err(|e| e.to_string())
}

/// 显式获取设备 root 权限（侧栏「获取 root」按钮，结果文案直接反馈到状态栏）。
/// `adb root` 重启 adbd：设备短暂离线、daemon 被杀；采样中会话走既有重连恢复
/// （新 daemon 以 root 身份重建，新 hello 带 root=true，前端据此更新徽章/解禁 IO）。
/// 阻塞数秒（adbd 重启 + 轮询确认），async 不卡 UI。
#[tauri::command]
async fn acquire_root(serial: String) -> Result<String, String> {
    ensure_device_online(&serial)?;
    xperf_core::agent::acquire_root(Some(&serial)).map_err(|e| e.to_string())
}

/// 在线设备清单（顶栏设备 tab 用）：`{devices: [{serial, model, version}]}`
#[tauri::command]
fn list_devices() -> Result<serde_json::Value, String> {
    let devices = xperf_core::list_adb_devices().map_err(|e| e.to_string())?;
    Ok(devices_json(devices))
}

// ---------- SSH 远程后端（连接切换；设计 docs/DESIGN-ssh-remote.md §6.2） ----------

/// 远程连接配置（`~/.config/xperf/remotes.json` 持久化，顶栏「连接」控件编辑）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct RemoteConfig {
    /// 展示名（下拉选项文本与唯一键，如 `hppc`）
    name: String,
    /// ssh 目标（`ssh_config` Host 别名或 `user@host`）
    host: String,
    /// 远端 adb 可执行路径（默认 `adb`；远端 PATH 常不含）
    adb_path: String,
    /// 远端 adb server 端口（默认 5037）
    remote_port: u16,
}

fn remotes_config_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")));
    base.map(|b| b.join("xperf").join("remotes.json"))
}

fn load_remotes() -> Vec<RemoteConfig> {
    remotes_config_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// 设备列表转 JSON（list_devices / connect_remote / devices-changed 共用；
/// 网关经 visible_devices 过滤，前端永远看不到 MindRT）
fn devices_json(devices: Vec<xperf_core::AdbDevice>) -> serde_json::Value {
    serde_json::json!({
        "devices": visible_devices(devices)
            .into_iter()
            .map(|d| serde_json::json!({ "serial": d.serial, "model": d.model, "version": d.android_version }))
            .collect::<Vec<_>>(),
    })
}

/// 已保存的远程连接配置列表
#[tauri::command]
fn list_remotes() -> Vec<RemoteConfig> {
    load_remotes()
}

/// 从 `~/.ssh/config` 解析 Host 别名（连接下拉的数据源之一——已配置的远端机
/// 免手工录入）。跳过含通配符（`*`/`?`/`!`）的模式行与注释；`Host` 关键字
/// 大小写不敏感，一行可带多个别名。
fn parse_ssh_config_hosts(content: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in content.lines() {
        let line = line.split('#').next().unwrap_or("").trim(); // 去行内注释
        let mut fields = line.split_whitespace();
        let Some(kw) = fields.next() else { continue };
        if !kw.eq_ignore_ascii_case("host") {
            continue;
        }
        for alias in fields {
            if !alias.contains(['*', '?', '!']) && !out.iter().any(|h| h == alias) {
                out.push(alias.to_string());
            }
        }
    }
    out
}

/// `~/.ssh/config` 中配置的 Host 别名列表（无该文件时为空）
#[tauri::command]
fn list_ssh_hosts() -> Vec<String> {
    std::env::var_os("HOME")
        .map(|h| std::path::PathBuf::from(h).join(".ssh").join("config"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| parse_ssh_config_hosts(&s))
        .unwrap_or_default()
}

/// 增/改远程连接配置（按 name upsert 后落盘）
#[tauri::command]
fn save_remote(cfg: RemoteConfig) -> Result<(), String> {
    if cfg.name.trim().is_empty() || cfg.host.trim().is_empty() {
        return Err("名称与 ssh 目标不能为空".into());
    }
    let mut list = load_remotes();
    list.retain(|r| r.name != cfg.name);
    list.push(cfg);
    let p = remotes_config_path().ok_or("无法确定配置目录（HOME 未设置）")?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&p, serde_json::to_string_pretty(&list).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}

/// 当前远程状态：`{mode: "local"|"ssh", host, alive}`（顶栏状态/启动回填用）
#[tauri::command]
fn remote_status() -> serde_json::Value {
    match xperf_core::transport() {
        xperf_core::Transport::Local => {
            serde_json::json!({"mode": "local", "host": null, "alive": true})
        }
        xperf_core::Transport::Ssh(t) => serde_json::json!({
            "mode": "ssh",
            "host": t.host,
            "alive": xperf_core::tunnel_alive(),
        }),
    }
}

/// 切换连接：停全部会话 → 关旧远程 → （可选）建新远程 → 返回新侧设备列表。
/// `host: None` = 切回本机。前端成功后重建全部设备 tab。
/// 进行中的 trace/stack 录制不等待（时间有界，adb 断开自然报错收尾）。
#[tauri::command]
async fn connect_remote(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    host: Option<String>,
) -> Result<serde_json::Value, String> {
    let emit = |state_s: &str, host: Option<&str>, message: String| {
        let _ = app.emit(
            "remote-status",
            serde_json::json!({"state": state_s, "host": host, "message": message}),
        );
    };
    // 停全部采样会话（采样线程下一轮退出；AgentStream drop 关 TCP → daemon 会话收尾）
    if let Ok(map) = state.sessions.lock() {
        for s in map.values() {
            *s.running.lock().unwrap() = false;
        }
    }
    xperf_core::shutdown_remote();

    let Some(host) = host else {
        emit("local", None, "已切回本机".to_string());
        return xperf_core::list_adb_devices()
            .map(devices_json)
            .map_err(|e| e.to_string());
    };

    emit("connecting", Some(&host), format!("正在连接 {host}…"));
    // 配置查找（按 name 或 host 匹配）；未保存的临时目标用默认 adb 路径/端口
    let cfg = load_remotes()
        .into_iter()
        .find(|r| r.name == host || r.host == host);
    let mut target = xperf_core::SshTarget::new(&host);
    if let Some(c) = cfg {
        target = xperf_core::SshTarget::new(&c.host)
            .with_adb_path(&c.adb_path)
            .with_remote_port(c.remote_port);
    }
    match xperf_core::init_remote(target) {
        Ok(devices) => {
            emit("connected", Some(&host), format!("已连接 {host}"));
            Ok(devices_json(devices))
        }
        Err(e) => {
            let msg = format!("{:#}", e);
            emit("error", Some(&host), msg.clone());
            Err(msg)
        }
    }
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
        let mut tunnel_was_dead = false;
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3));
            // SSH 远程：隧道死则 adb 全灭，枚举只会每 3s 刷错——短路并边沿通知；
            // 恢复后轮询自动继续（隧道重建由采样重连或用户重连触发）
            let tunnel_dead = matches!(xperf_core::transport(), xperf_core::Transport::Ssh(_))
                && !xperf_core::tunnel_alive();
            if tunnel_dead {
                if !tunnel_was_dead {
                    tunnel_was_dead = true;
                    let _ = app.emit(
                        "remote-status",
                        serde_json::json!({"state": "tunnel-down", "message": "SSH 隧道断开，等待恢复…"}),
                    );
                }
                continue;
            }
            if tunnel_was_dead {
                tunnel_was_dead = false;
                last.clear(); // 清快照强制全量 diff，设备 tab 状态与新侧对齐
                first_round = true;
                let _ = app.emit(
                    "remote-status",
                    serde_json::json!({"state": "tunnel-up", "message": "SSH 隧道已恢复"}),
                );
            }
            let devices = match xperf_core::list_adb_devices() {
                // diff 之前过滤网关（设计 §4.4）：网关插拔不产生 devices-changed
                // 噪声、不进 added/removed，前端不会为 MindRT 建 tab
                Ok(d) => visible_devices(d),
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
            let mut payload = devices_json(devices.clone());
            payload["added"] = serde_json::json!(added);
            payload["removed"] = serde_json::json!(removed);
            let _ = app.emit("devices-changed", payload);
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
/// GPU 系统导出行：(ms, busy%, util%, mhz)
type GpuExportPoints = Vec<(f64, f64, f64, u32)>;

/// 导出当前采样会话的流式 CSV：复制会话目录快照，返回新目录路径。
/// 采样数据已逐事件流式落盘到 `<pkg>/<ts>-<serial>/`（与 CLI 同根同格式，全分辨率），
/// 导出 = 复制该目录为 `<pkg>/<ts>-<serial>-export-<时刻>/`。复制而非直接返回原目录：
/// 导出语义是快照（会话可能仍在追加写）。
#[tauri::command]
async fn export_csv(serial: String, state: State<'_, AppState>) -> Result<String, String> {
    let session = state.session(&serial);
    export_session_csv(&session)
}

/// export_csv 的实现体（命令壳拆出便于单测）：复制会话 CSV 目录快照
fn export_session_csv(session: &DeviceSession) -> Result<String, String> {
    let (pkg, src) = session.csv_dir.lock().unwrap().clone()
        .ok_or_else(|| "当前无采样会话（先开始监控）".to_string())?;
    if !src.is_dir() {
        return Err("暂无已落盘的数据（采样会话尚无样本）".into());
    }
    validate_package(&pkg)?;
    let dst = src.with_file_name(format!(
        "{}-export-{}",
        src.file_name().and_then(|n| n.to_str()).unwrap_or("session"),
        Local::now().format("%Y%m%d_%H%M%S")
    ));
    copy_dir_all(&src, &dst).map_err(|e| format!("导出失败: {}", e))?;
    Ok(dst.to_string_lossy().into_owned())
}

/// 递归复制目录（导出快照用；dst 由调用方保证为本次新建）
fn copy_dir_all(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
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
    // 加速合成路径在窗口失焦（Alt+Tab 切走，常为切到浏览器等 GPU 重负载应用）
    // 再切回时也有概率内容空白（2026-09-07 用户反馈，与本机无法稳定复现的
    // DMABUF 场景同源）——禁用合成模式回退非合成渲染；本工具 UI 无 CSS 动画/
    // 变换，禁用的性能影响可忽略。
    std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
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
    // SSH 远程后端（--remote <host> [--remote-adb <path>] [--remote-adb-port <n>]）：
    // 须在任何 adb 调用（设备枚举/选择）之前建立隧道——隧道在，设备列表即指向远端
    if let Some(host) = get_opt("--remote") {
        let mut target = xperf_core::SshTarget::new(&host);
        if let Some(p) = get_opt("--remote-adb") {
            target = target.with_adb_path(p);
        }
        if let Some(p) = get_opt("--remote-adb-port").and_then(|v| v.parse().ok()) {
            target = target.with_remote_port(p);
        }
        match xperf_core::init_remote(target) {
            Ok(devices) => eprintln!("[startup] 远程后端: {}（在线 {} 台）", host, devices.len()),
            Err(e) => {
                eprintln!("[startup] 远程后端初始化失败: {:#}（前端可经顶栏「连接」重试）", e);
                auto_start = false;
            }
        }
    }
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
                            true,
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
            update_simpleperf_scripts,
            diag_log,
            list_packages,
            startup_sessions,
            launch_app,
            restart_app,
            acquire_root,
            export_csv,
            save_baseline,
            compare_baseline,
            list_devices,
            list_remotes,
            list_ssh_hosts,
            save_remote,
            connect_remote,
            remote_status,
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
                // 关窗收尾远程后端：清理 forward 规则 + 关隧道（R9/R10）
                xperf_core::shutdown_remote();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- SSH 远程连接配置（save_remote/list_remotes；XDG_CONFIG_HOME 隔离到临时目录） ----

    #[test]
    fn test_parse_ssh_config_hosts() {
        let cfg = r#"
# 注释行
Host hppc
  HostName 10.0.0.2
  User han

Host lab dev-lab   # 一行多别名 + 行内注释
  HostName 192.168.1.10

Host *             # 通配符跳过
  ServerAliveInterval 30
Host *.corp !jump  # 含通配/否定整行跳过
HOST Upper         # 大小写不敏感
Host hppc          # 重复别名去重
"#;
        assert_eq!(parse_ssh_config_hosts(cfg), vec!["hppc", "lab", "dev-lab", "Upper"]);
        assert!(parse_ssh_config_hosts("").is_empty());
        assert!(parse_ssh_config_hosts("Host *\n").is_empty());
    }

    #[test]
    fn test_remote_config_upsert() {
        let dir = std::env::temp_dir().join(format!("xperf-remotes-test-{}", std::process::id()));
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        let cfg = |name: &str, host: &str| RemoteConfig {
            name: name.into(),
            host: host.into(),
            adb_path: "adb".into(),
            remote_port: 5037,
        };
        // 新增两条 + name 相同 upsert（host 更新）
        save_remote(cfg("hppc", "hppc")).unwrap();
        save_remote(cfg("lab", "user@lab")).unwrap();
        save_remote(cfg("hppc", "hppc2")).unwrap();
        let list = list_remotes();
        assert_eq!(list.len(), 2);
        assert_eq!(list.iter().find(|r| r.name == "hppc").unwrap().host, "hppc2");
        assert_eq!(list.iter().find(|r| r.name == "lab").unwrap().host, "user@lab");
        // 校验：空名称/空 host 拒绝
        assert!(save_remote(cfg("", "x")).is_err());
        assert!(save_remote(cfg("x", "")).is_err());
        std::env::remove_var("XDG_CONFIG_HOME");
        let _ = std::fs::remove_dir_all(&dir);
    }

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

    #[test]
    fn test_export_csv_snapshot_copies_session_dir() {
        // 造会话：csv_dir 指向临时会话目录（内含嵌套子目录与 CSV 文件）
        let pkg = format!("test_export_{}", std::process::id());
        let src_dir = gui_data_root().join(&pkg).join("20260101_000000-devX");
        let cpu_dir = src_dir.join("cpu");
        std::fs::create_dir_all(&cpu_dir).unwrap();
        std::fs::write(
            cpu_dir.join("cpu_1234_data.csv"),
            "Timestamp,Process CPU (%)\n2026-01-01 00:00:00.000,12.50\n",
        ).unwrap();
        let mut session = DeviceSession::new();
        session.package = pkg.clone();
        *session.csv_dir.lock().unwrap() = Some((pkg.clone(), src_dir));
        // 导出 = 复制快照（<ts>-<serial>-export-<时刻>），内容与源一致
        let dst = export_session_csv(&session).unwrap();
        assert!(dst.contains("-export-"), "快照目录命名: {}", dst);
        let copied = std::fs::read_to_string(format!("{}/cpu/cpu_1234_data.csv", dst)).unwrap();
        assert!(copied.starts_with("Timestamp,Process CPU (%)\n"));
        assert!(copied.contains(",12.50\n"));
        std::fs::remove_dir_all(gui_data_root().join(&pkg)).ok();
    }

    #[test]
    fn test_csv_dir_for_fresh_and_reuse() {
        let state = AppState { sessions: Mutex::new(HashMap::new()) };
        let pkg = format!("test_csvdir_{}", std::process::id());
        // 同包 + 非 fresh（指标勾选重启）→ 复用同一目录
        let d1 = state.csv_dir_for("devA", &pkg, false);
        let d2 = state.csv_dir_for("devA", &pkg, false);
        assert_eq!(d1, d2, "同包重启应复用目录");
        // 同包 + fresh（手动开始，前端已重置数据）→ 新目录请求（目录名按秒，
        // 同秒内可能重名——append 模式续写不丢数据，见 csvstream 实现）
        let d3 = state.csv_dir_for("devA", &pkg, true);
        assert!(d3.to_string_lossy().contains(&pkg));
        // 换包 → 必为新目录（路径含包名）
        let d4 = state.csv_dir_for("devA", &format!("{}_b", pkg), false);
        assert_ne!(d1, d4, "换包须新目录");
    }

    #[test]
    fn test_export_csv_no_session_errors() {
        // 无 csv_dir（从未采样）→ 报错；csv_dir 目录不存在（尚无样本落盘）→ 报错
        let mut session = DeviceSession::new();
        session.package = "x".into();
        assert!(export_session_csv(&session).is_err());
        *session.csv_dir.lock().unwrap() =
            Some(("x".to_string(), gui_data_root().join("nonexistent-xyz-qe")));
        assert!(export_session_csv(&session).is_err());
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

