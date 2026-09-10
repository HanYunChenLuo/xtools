//! host 侧采样通道（SS4 专属兜底）：设备端 agent 数据源失效时的替代路径。
//!
//! 背景（SS4 = SA8797P，MindRT + Android GVM 双系统）：
//! - **FPS**：QCM SDE 定制 SurfaceFlinger 构建的 `dumpsys SurfaceFlinger --latency`
//!   对全部图层恒空（2026-09-10 全图层实测确证，无 getprop 恢复开关）。唯一帧源是
//!   perfetto `android.surfaceflinger.frametimeline` 数据源（单数据源 ~9.5KB/s，
//!   与全配置 `--trace` 并发无冲突）。agent 侧 `--fps` 在 SS4 短路（协议 v4 起）。
//! - **GPU busy**：ligfxprofilerd 在 MindRT 侧输出 logcat，GVM 内无任何输出，
//!   agent 的 ligfx 通道永久不可用，须 host 侧经桥接网关读 MindRT logcat。
//!
//! 架构：[`maybe_spawn`](crate::hostchan::maybe_spawn) 在 `spawn_agent` 内按平台+指标开关启动
//! host 侧线程，线程合成 [`AgentEvent`](crate::agent::AgentEvent) 经 mpsc 汇入
//! [`crate::agent::AgentStream`] 的 `next_event`/`next_event_batch`（消费端 CLI/GUI
//! 零改动）。线程生命周期随 AgentStream：流析构 → receiver Drop → 发送失败退出
//! （最坏多跑完一个窗口）。
//!
//! ## frametimeline FPS 的口径（实测结论，勿踩坑）
//!
//! per-layer `TX - <层名>` 行只覆盖非 BLAST 系统窗（StatusBar/HUD 等）；BLAST 层
//! （SurfaceView 直渲应用，含 gltf viewer）的帧被折叠进 `layer_name IS NULL` 的
//! display 级合成流（vsync 合并）。因此事件语义 = **display 合成 FPS ≈ 被测应用
//! FPS（单动画源场景）**，含系统低频动画底噪（静止时非 0），不做 per-layer 归因；
//! `layer` 字段固定 `(display)` 标注。应用进程不在时不发事件（底噪不归因）。

use crate::agent::AgentEvent;
use crate::platform::PlatformId;
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write as _};
use std::path::Path;
use std::process::Stdio;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, atomic::AtomicBool, atomic::Ordering};
use std::time::{Duration, Instant};

/// 合成事件的发送端类型（Ok = 合成事件；Err 保留给行解析失败语义，本模块只用 Ok）
pub type HostTx = mpsc::Sender<std::result::Result<AgentEvent, String>>;
/// 合成事件的接收端类型（挂在 [`crate::agent::AgentStream`] 上）
pub type HostRx = mpsc::Receiver<std::result::Result<AgentEvent, String>>;

/// frametimeline-only 录制窗口时长（秒）。开销 ~9.5KB/s，5s ≈ 50KB/窗。
const FPS_WINDOW_SECS: u64 = 5;

/// 连续失败上限：超过则通道线程退出（防止设备长期离线时 adb 报错空转；
/// 断连恢复由 reconnect_agent → spawn_agent 重启通道）
const MAX_CONSECUTIVE_FAILURES: u32 = 3;

/// 按平台与指标开关启动 SS4 host 侧通道，返回合成事件接收端。
///
/// 非 SS4 平台 / 未开 fps|gpu / 无包名时返回 None（无通道）。`serial` 为 None 时
/// 走 adb 全局目标（CLI 单设备路径，与 agent 其余调用一致）。
/// `stop`：会话停止标志（AgentStream 的 ping_stop，kill/drop 时置位）——通道线程
/// 在窗口边界/重连点检查，保证会话结束后线程有界退出（发送失败检查只覆盖有事件
/// 可发的路径，设备离线期间无事件可发，须靠该标志）。
pub fn maybe_spawn(
    flags: crate::agent::MetricFlags,
    platform: Option<&dyn crate::platform::Platform>,
    package: Option<&str>,
    serial: Option<&str>,
    stop: Arc<AtomicBool>,
) -> Option<HostRx> {
    if platform?.id() != PlatformId::Ss4 {
        return None;
    }
    let pkg = package?.to_string();
    if !flags.fps && !flags.gpu {
        return None;
    }
    let serial = serial.map(str::to_string);
    let (tx, rx) = mpsc::channel();
    if flags.fps {
        spawn_frametimeline(pkg.clone(), serial.clone(), tx.clone(), stop.clone());
    }
    if flags.gpu {
        spawn_ligfx(pkg, serial, tx, stop);
    }
    Some(rx)
}

// ==================== frametimeline FPS 通道 ====================

/// frametimeline-only perfetto 配置（数据源名写错会被静默忽略，勿改）。
/// stdin 喂入（`-c -`），避免配置文件落 `/data/local/tmp` 被 perfetto 拒读（errno 13）。
fn frametimeline_config(secs: u64) -> String {
    format!(
        "buffers {{ size_kb: 1024 fill_policy: RING_BUFFER }}\n\
         data_sources {{ config {{ name: \"android.surfaceflinger.frametimeline\" }} }}\n\
         duration_ms: {}\n\
         write_into_file: true\n\
         file_write_period_ms: 1000\n\
         flush_period_ms: 1000\n",
        secs * 1000
    )
}

/// 窗口解析 SQL：只取 display 级合成流（layer_name IS NULL，BLAST 应用帧折叠于此）。
/// per-layer `TX -` 行只覆盖系统窗，混入会重复计数。
const FRAMETIMELINE_SQL: &str =
    "select ts, dur from actual_frame_timeline_slice where layer_name is null order by ts;\n";

/// 启动 frametimeline FPS 线程（5s 窗循环：录 → pull → trace_processor 解析 → 发事件）
fn spawn_frametimeline(pkg: String, serial: Option<String>, tx: HostTx, stop: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let tp = match crate::trace::ensure_trace_processor() {
            Ok(p) => p,
            Err(e) => {
                let _ = tx.send(Ok(AgentEvent::Err {
                    msg: format!("SS4 FPS（frametimeline）：trace_processor 不可用，通道未启动: {e}"),
                }));
                return;
            }
        };
        let mut watermark: i64 = 0;
        let mut fails = 0u32;
        loop {
            if stop.load(Ordering::Relaxed) {
                return; // 会话结束（AgentStream kill/drop）
            }
            match record_and_parse_window(&tp, serial.as_deref(), FPS_WINDOW_SECS) {
                Ok(frames) => {
                    fails = 0;
                    // 应用进程不在时不发事件（display 流含系统底噪，不归因给死进程）
                    let Some(pid) = first_pid_of(serial.as_deref(), &pkg) else {
                        continue;
                    };
                    let (n, fps, jank) = summarize_window(&frames, &mut watermark, FPS_WINDOW_SECS as f32);
                    let ev = AgentEvent::Fps {
                        ts: now_ms(),
                        pid,
                        layer: "(display)".to_string(),
                        fps,
                        frames: n,
                        jank,
                    };
                    if tx.send(Ok(ev)).is_err() {
                        return; // 会话结束（AgentStream 已析构）
                    }
                }
                Err(e) => {
                    fails += 1;
                    let msg = if fails >= MAX_CONSECUTIVE_FAILURES {
                        format!("SS4 FPS frametimeline 连续 {fails} 窗失败，通道退出（最后错误: {e:#}）")
                    } else {
                        format!("SS4 FPS frametimeline 窗口失败（{fails}/{MAX_CONSECUTIVE_FAILURES}）: {e:#}")
                    };
                    if tx.send(Ok(AgentEvent::Err { msg })).is_err() || fails >= MAX_CONSECUTIVE_FAILURES {
                        return;
                    }
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
        }
    });
}

/// 录一个窗口并解析出 display 合成流的帧时间戳序列（ns，boot 钟，升序）。
fn record_and_parse_window(tp: &Path, serial: Option<&str>, secs: u64) -> Result<Vec<i64>> {
    let stem = format!("{}", now_ms());
    let dev_path = format!("/data/misc/perfetto-traces/xperf_fps_{stem}.pftrace");
    let local_path = std::env::temp_dir().join(format!(
        "xperf_fps_{}_{}.pftrace",
        sanitize(serial.unwrap_or("local")),
        stem
    ));
    let result = record_window(serial, secs, &dev_path)
        .and_then(|_| pull_window(serial, &dev_path, &local_path))
        .and_then(|_| parse_window(tp, &local_path));
    // 设备端与本地临时文件都清（失败不致命；文件名含时间戳不撞下次）
    let _ = crate::utils::adb_for(serial)
        .args(["shell", "rm", "-f", &dev_path])
        .output();
    let _ = std::fs::remove_file(&local_path);
    result
}

/// 录制一个 frametimeline 窗口（阻塞至 perfetto 到点退出）
fn record_window(serial: Option<&str>, secs: u64, dev_path: &str) -> Result<()> {
    let mut child = crate::utils::adb_for(serial)
        .args(["shell", "perfetto", "-c", "-", "--txt", "-o", dev_path])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("启动 adb shell perfetto 失败")?;
    {
        let mut stdin = child.stdin.take().expect("stdin just piped");
        stdin.write_all(frametimeline_config(secs).as_bytes())?;
    } // drop → EOF，perfetto 开始录制
    let deadline = Instant::now() + Duration::from_secs(secs + 15);
    loop {
        match child.try_wait()? {
            Some(_) => break,
            None if crate::utils::is_interrupted() => {
                let _ = child.kill();
                bail!("录制被 Ctrl-C 中断");
            }
            None if Instant::now() > deadline => {
                let _ = child.kill();
                bail!("perfetto 录制超时（>{}s）", secs + 15);
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("perfetto 录制失败: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// 拉回窗口 trace 到本地临时文件
fn pull_window(serial: Option<&str>, dev_path: &str, local_path: &Path) -> Result<()> {
    let pull = crate::utils::adb_for(serial)
        .arg("pull")
        .arg(dev_path)
        .arg(local_path)
        .output()
        .context("执行 adb pull 失败")?;
    if !pull.status.success() {
        bail!("adb pull 失败: {}", String::from_utf8_lossy(&pull.stderr).trim());
    }
    Ok(())
}

/// trace_processor 解析窗口 trace，返回 display 合成流帧时间戳（ns，升序）
fn parse_window(tp: &Path, local_path: &Path) -> Result<Vec<i64>> {
    let sql_path = local_path.with_extension("sql");
    std::fs::write(&sql_path, FRAMETIMELINE_SQL)?;
    let (stdout, _stderr) = crate::trace::run_trace_processor(tp, local_path, &sql_path)?;
    let _ = std::fs::remove_file(&sql_path);
    Ok(parse_frame_rows(&stdout))
}

/// 解析 trace_processor 单查询输出（表头 `"ts","dur"` + 数据行）的 ts 列。
/// 脏行/空行跳过（防御性；正常输出无）。
fn parse_frame_rows(tp_stdout: &str) -> Vec<i64> {
    let mut out = Vec::new();
    for (i, line) in tp_stdout.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if i == 0 && line.contains("ts") {
            continue; // 表头
        }
        let cells = crate::trace::parse_csv_line(line);
        if let Some(ts) = cells.first().and_then(|s| s.parse::<i64>().ok()) {
            out.push(ts);
        }
    }
    out
}

/// 窗口汇总：帧时间戳（ns，boot 钟）经水位去重后计 fps/jank。
///
/// 返回 (新帧数, fps, jank 数)。窗口无新帧时 fps=0（静止是真实状态）。
/// jank 口径与 agent fps.rs 一致：帧间隔 > 2×窗口中位间隔，<3 帧不计。
/// `watermark` 为跨窗口帧去重水位（顺序录制窗口本不相交，水位为防御性保底）。
fn summarize_window(frames_ns: &[i64], watermark: &mut i64, wall_secs: f32) -> (u32, f32, u32) {
    let kept: Vec<i64> = frames_ns.iter().copied().filter(|ts| *ts > *watermark).collect();
    if let Some(max) = kept.iter().max() {
        *watermark = *max;
    }
    let n = kept.len() as u32;
    let fps = if wall_secs > 0.0 { n as f32 / wall_secs } else { 0.0 };
    let jank = if kept.len() >= 3 {
        let mut iv: Vec<i64> = kept.windows(2).map(|w| w[1] - w[0]).collect();
        iv.sort_unstable();
        let median = iv[iv.len() / 2];
        iv.iter().filter(|&&d| d > 2 * median).count() as u32
    } else {
        0
    };
    (n, fps, jank)
}

/// 包名的首个 PID（`pidof` 多进程应用取第一个；无进程返回 None）
fn first_pid_of(serial: Option<&str>, pkg: &str) -> Option<u32> {
    let out = crate::utils::run_adb_command_for(serial, &["shell", "pidof", pkg]).ok()?;
    out.stdout.split_whitespace().next()?.parse().ok()
}

/// 临时文件名用的 serial 清洗（`localhost:5559` → `localhost_5559`）
fn sanitize(serial: &str) -> String {
    serial.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect()
}

/// 主机墙钟毫秒
fn now_ms() -> u64 {
    chrono::Local::now().timestamp_millis() as u64
}

// ==================== ligfx GPU 通道（经桥接网关读 MindRT logcat） ====================

/// ligfx 通道的进程内独占登记（按 Android serial）。logcat 是只读通道，跨进程无
/// QNX 统计链式的写冲突；进程内独占防止多会话在同一 MindRT 上重复起 logcat 读流。
static LIGFX_BUSY: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// ligfx logcat 断流后的重连间隔
const LIGFX_RECONNECT_SECS: u64 = 2;

/// ligfx 行解析结果（归一自系统行/进程行；移植自 xperf-agent gpu/ligfx.rs，
/// 真机行格式见模块测试）
enum LigfxEvent {
    /// 系统行：`[GPU0] Frame N: Frequency: F Hz, ..., Busy=B%, Queued=Q%, Utilization=U%`
    Sys {
        /// Global Busy %
        busy: f32,
        /// Global Utilization %（业务侧关注字段，平台文档口径）
        util: f32,
        /// Frequency 原始值（单位标称 Hz 但恒 1000，疑似定频占位/单位标注错误）
        mhz: u32,
    },
    /// 进程行：`[GPU0]   GVM_<comm 15字符截断>-<会话id>: Busy=B%, ...`
    Proc {
        /// 进程 comm（≤15 字符，GVM_ 前缀与 -会话id 后缀已剥离）
        name: String,
        /// 该进程 Busy %
        busy: f32,
    },
}

/// 解析 ligfxprofilerd logcat 行（移植自 agent `gpu/ligfx.rs::parse_line`，语义一致：
/// 无 ligfxprofilerd 标签 / 关键字段缺失的行丢弃；业务侧只看 Busy/Utilization）
fn parse_ligfx_line(line: &str) -> Option<LigfxEvent> {
    if !line.contains("ligfxprofilerd") {
        return None;
    }
    if line.contains("Frame ") && line.contains("Frequency:") {
        let mhz = line.split("Frequency:").nth(1)?.split_whitespace().next()?.parse::<u32>().ok()?;
        let busy = parse_pct_after(line, "Busy=")?;
        let util = parse_pct_after(line, "Utilization=")?;
        return Some(LigfxEvent::Sys { busy, util, mhz });
    }
    if line.contains("GVM_") {
        let gvm_pos = line.find("GVM_")?;
        let after = &line[gvm_pos + 4..];
        let name_end = after.find(':').unwrap_or(after.len());
        let name = after[..name_end].split('-').next()?.trim().to_string();
        let busy = parse_pct_after(line, "Busy=")?;
        let _ = parse_pct_after(line, "Utilization=")?; // 字段缺失的行整体丢弃
        return Some(LigfxEvent::Proc { name, busy });
    }
    None
}

/// 从 "key=12.34%" 格式中提取 f32
fn parse_pct_after(line: &str, key: &str) -> Option<f32> {
    let pos = line.find(key)? + key.len();
    line[pos..].split('%').next()?.trim().parse().ok()
}

/// ligfx 进程行 comm → Android pid 映射（只覆盖被测包进程；重建时机：首行/
/// 查找未命中且距上次重建 >5s/每 60s 定期——进程重启 pid 变化须跟进）
struct CommMap {
    map: HashMap<String, u32>,
    last_refresh: Instant,
}

impl CommMap {
    fn new() -> Self {
        Self { map: HashMap::new(), last_refresh: Instant::now() - Duration::from_secs(3600) }
    }

    /// 查 name（comm 15 字符截断语义，与 agent lookup_pid 一致）；未命中时
    /// 按需重建一次再查
    fn lookup(&mut self, serial: Option<&str>, pkg: &str, name: &str) -> Option<u32> {
        if self.last_refresh.elapsed() > Duration::from_secs(60) {
            self.refresh(serial, pkg);
        }
        if let Some(pid) = self.get(name) {
            return Some(pid);
        }
        if self.last_refresh.elapsed() > Duration::from_secs(5) {
            self.refresh(serial, pkg);
            return self.get(name);
        }
        None
    }

    fn get(&self, name: &str) -> Option<u32> {
        self.map.get(name).copied().or_else(|| {
            let truncated: String = name.chars().take(15).collect();
            self.map.get(&truncated).copied()
        })
    }

    /// 重建映射：`pidof <pkg>` → 逐 pid 读 `/proc/<pid>/comm`（内核已截断 15 字符）
    fn refresh(&mut self, serial: Option<&str>, pkg: &str) {
        self.last_refresh = Instant::now();
        let Ok(out) = crate::utils::run_adb_command_for(serial, &["shell", "pidof", pkg]) else {
            return;
        };
        let mut m = HashMap::new();
        for pid in out.stdout.split_whitespace() {
            let Ok(comm) =
                crate::utils::run_adb_command_for(serial, &["shell", "cat", &format!("/proc/{pid}/comm")])
            else {
                continue;
            };
            if let Ok(pid) = pid.parse::<u32>() {
                m.insert(comm.stdout.trim().to_string(), pid);
            }
        }
        self.map = m;
    }
}

/// 启动 ligfx GPU 线程：经桥接网关在 MindRT 上跑 `logcat -s ligfxprofilerd`，
/// 系统行 → Gpu 事件，进程行 → GpuProc 事件（comm 归因到被测包 pid）。
/// 流断（MindRT 重启/桥接重建）后按 [`LIGFX_RECONNECT_SECS`] 间隔重连。
fn spawn_ligfx(pkg: String, serial: Option<String>, tx: HostTx, stop: Arc<AtomicBool>) {
    // 生效 serial（None 走全局目标）；bridged SS4 的 Android serial 恒为 localhost:<port>
    let Some(android_serial) = crate::utils::resolve_serial(serial.as_deref()) else {
        let _ = tx.send(Ok(AgentEvent::Err { msg: "SS4 GPU（ligfx）：无目标设备 serial，通道未启动".into() }));
        return;
    };
    let Some(gateway) = crate::bridge::gateway_for_android(&android_serial) else {
        let _ = tx.send(Ok(AgentEvent::Err {
            msg: format!("SS4 GPU（ligfx）：{android_serial} 无桥接网关信息（bridge 未收敛？），通道未启动"),
        }));
        return;
    };
    // 进程内独占（后到会话 err 禁用）
    {
        let mut g = LIGFX_BUSY.lock().unwrap();
        if g.contains(&android_serial) {
            let _ = tx.send(Ok(AgentEvent::Err {
                msg: "SS4 GPU（ligfx）：通道被本进程另一会话占用，本会话 GPU busy 禁用（显存不受影响）".into(),
            }));
            return;
        }
        g.push(android_serial.clone());
    }
    std::thread::spawn(move || {
        let mut comms = CommMap::new();
        while !stop.load(Ordering::Relaxed) {
            // -T 0：不回放 logcat 历史缓冲（旧块会以当前时刻批量入账，污染时序）
            let child = crate::utils::adb_for(Some(&gateway))
                .args(["shell", "logcat", "-T", "0", "-s", "ligfxprofilerd"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn();
            let mut child = match child {
                Ok(c) => c,
                Err(e) => {
                    if tx.send(Ok(AgentEvent::Err { msg: format!("SS4 GPU（ligfx）：logcat 启动失败: {e}") })).is_err() {
                        break;
                    }
                    std::thread::sleep(Duration::from_secs(LIGFX_RECONNECT_SECS));
                    continue;
                }
            };
            let Some(stdout) = child.stdout.take() else {
                std::thread::sleep(Duration::from_secs(LIGFX_RECONNECT_SECS));
                continue;
            };
            let mut stream_dead = false;
            for line in BufReader::new(stdout).lines() {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(line) = line else {
                    stream_dead = true;
                    break;
                };
                let Some(ev) = parse_ligfx_line(&line) else { continue };
                let ts = now_ms();
                let out = match ev {
                    LigfxEvent::Sys { busy, util, mhz } => AgentEvent::Gpu { ts, busy, util, mhz, maxmhz: 0 },
                    LigfxEvent::Proc { name, busy } => {
                        let Some(pid) = comms.lookup(Some(&android_serial), &pkg, &name) else {
                            continue; // 非被测包进程（或进程已退出）：不归因
                        };
                        AgentEvent::GpuProc { ts, pid, busy }
                    }
                };
                if tx.send(Ok(out)).is_err() {
                    stream_dead = true;
                    break;
                }
            }
            let _ = child.kill();
            if stream_dead && !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_secs(LIGFX_RECONNECT_SECS));
            }
        }
        // 释放进程内独占
        LIGFX_BUSY.lock().unwrap().retain(|s| s != &android_serial);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frametimeline_config() {
        let c = frametimeline_config(5);
        assert!(c.contains("android.surfaceflinger.frametimeline"));
        assert!(c.contains("duration_ms: 5000"));
        assert!(c.contains("write_into_file: true"));
        assert!(c.contains("RING_BUFFER"));
    }

    #[test]
    fn test_parse_frame_rows() {
        // 真机输出形态：表头 + 升序数据行（trace_processor CSV）
        let out = "\"ts\",\"dur\"\n748010000000,16666667\n748016666667,16666666\n748033333334,16666667\n";
        assert_eq!(
            parse_frame_rows(out),
            vec![748010000000, 748016666667, 748033333334]
        );
        // 空结果集（静止窗口）：只有表头
        assert!(parse_frame_rows("\"ts\",\"dur\"\n").is_empty());
        // 脏行跳过
        assert_eq!(parse_frame_rows("\"ts\",\"dur\"\ngarbage\n123,1\n"), vec![123]);
    }

    #[test]
    fn test_summarize_window_basic() {
        // 60fps 5s 窗：300 帧等间隔 → fps=60，jank=0
        let frames: Vec<i64> = (0..300).map(|i| 1_000_000_000 + i * 16_666_667).collect();
        let mut wm = 0;
        let (n, fps, jank) = summarize_window(&frames, &mut wm, 5.0);
        assert_eq!(n, 300);
        assert!((fps - 60.0).abs() < 0.01);
        assert_eq!(jank, 0);
        assert_eq!(wm, *frames.last().unwrap());
    }

    #[test]
    fn test_summarize_window_jank() {
        // 中位间隔 10ms；两个 50ms 大间隔 > 2×中位 → jank=2（累积构造保证升序）。
        // 首帧 ts=0 会被初始水位 0 排掉（ts > watermark 严格大于），故水位从 -1 起
        let mut frames = vec![0i64];
        for i in 1..100 {
            let gap = if i == 50 || i == 80 { 50_000_000 } else { 10_000_000 };
            frames.push(frames.last().unwrap() + gap);
        }
        let mut wm = -1;
        let (n, _fps, jank) = summarize_window(&frames, &mut wm, 5.0);
        assert_eq!(n, 100);
        assert_eq!(jank, 2);
    }

    #[test]
    fn test_summarize_window_watermark_and_empty() {
        let mut wm = 1_000_000;
        // 全部 ≤ 水位：无新帧
        let (n, fps, jank) = summarize_window(&[500_000, 1_000_000], &mut wm, 5.0);
        assert_eq!((n, jank), (0, 0));
        assert_eq!(fps, 0.0);
        assert_eq!(wm, 1_000_000);
        // <3 帧不计 jank
        let (n, _fps, jank) = summarize_window(&[2_000_000, 3_000_000_000], &mut wm, 5.0);
        assert_eq!((n, jank), (2, 0));
        assert_eq!(wm, 3_000_000_000);
    }

    #[test]
    fn test_sanitize_serial() {
        assert_eq!(sanitize("localhost:5559"), "localhost_5559");
        assert_eq!(sanitize("6eb792dfb0f"), "6eb792dfb0f");
    }

    #[test]
    fn test_parse_ligfx_line() {
        // 真机样例（DESIGN-ss4-metrics.md 任务 B；logcat 默认格式带标签头）
        let sys = "09-10 14:00:00.656 21047 I ligfxprofilerd: [GPU0] Frame 149556: Frequency: 1000 Hz, Tasks: 3 total, GSL Timestamp: 748015951, Global: Busy=29.28%, Queued=20.24%, Utilization=29.28%";
        match parse_ligfx_line(sys) {
            Some(LigfxEvent::Sys { busy, util, mhz }) => {
                assert!((busy - 29.28).abs() < 0.01);
                assert!((util - 29.28).abs() < 0.01);
                assert_eq!(mhz, 1000);
            }
            _ => panic!("应为 Sys"),
        }
        let proc_ = "09-10 14:00:00.656 21047 I ligfxprofilerd: [GPU0]   GVM_d.filament.gltf-1572152: Busy=8.39%, Queued=5.98%, Utilization=8.39%";
        match parse_ligfx_line(proc_) {
            Some(LigfxEvent::Proc { name, busy }) => {
                assert_eq!(name, "d.filament.gltf"); // comm 15 字符截断 + 会话 id 剥离
                assert!((busy - 8.39).abs() < 0.01);
            }
            _ => panic!("应为 Proc"),
        }
        // 无标签 / 非 ligfx 行 / 关键字段缺失 → 丢弃
        assert!(parse_ligfx_line("random logcat line").is_none());
        assert!(parse_ligfx_line("[GPU0] Frame 1: Frequency: 1000 Hz").is_none()); // 无 ligfxprofilerd 标签
        let no_util = "x ligfxprofilerd: [GPU0]   GVM_abc-1: Busy=1.0%";
        assert!(parse_ligfx_line(no_util).is_none());
    }
}
