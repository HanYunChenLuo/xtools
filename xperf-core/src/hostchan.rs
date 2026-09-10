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
//!
//! 注意：合成事件的 `ts` 取 **host 墙钟**，设备端事件取设备墙钟——GVM 时钟与
//! host 漂移时两类事件在时间轴上会有固定偏移（已知局限，车机通常 NTP 同步）。

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
                    // 水位无条件推进（含应用死亡窗口的底噪帧），否则复活后首个
                    // 样本把死亡期累积帧全部计入（fps/jank 虚高）
                    let (n, fps, jank) = summarize_window(&frames, &mut watermark, FPS_WINDOW_SECS as f32);
                    // 应用进程不在时不发事件（display 流含系统底噪，不归因给死进程）
                    let Some(pid) = first_pid_of(serial.as_deref(), &pkg) else {
                        continue;
                    };
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
                let _ = child.wait(); // 回收僵尸进程
                bail!("录制被 Ctrl-C 中断");
            }
            None if Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
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

/// 拉回窗口 trace 到本地临时文件（30s 超时兜底：通道是循环结构，pull 挂起
/// 会永久 wedge 通道线程，不像一次性 --trace 可以接受无超时）
fn pull_window(serial: Option<&str>, dev_path: &str, local_path: &Path) -> Result<()> {
    let mut child = crate::utils::adb_for(serial)
        .arg("pull")
        .arg(dev_path)
        .arg(local_path)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("执行 adb pull 失败")?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match child.try_wait()? {
            Some(status) => {
                if !status.success() {
                    let mut err = String::new();
                    if let Some(mut e) = child.stderr.take() {
                        let _ = std::io::Read::read_to_string(&mut e, &mut err);
                    }
                    bail!("adb pull 失败: {}", err.trim());
                }
                return Ok(());
            }
            None if crate::utils::is_interrupted() => {
                let _ = child.kill();
                let _ = child.wait();
                bail!("pull 被 Ctrl-C 中断");
            }
            None if Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                bail!("adb pull 超时（>30s）");
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

/// trace_processor 解析窗口 trace，返回 display 合成流帧时间戳（ns，升序）
fn parse_window(tp: &Path, local_path: &Path) -> Result<Vec<i64>> {
    let sql_path = local_path.with_extension("sql");
    std::fs::write(&sql_path, FRAMETIMELINE_SQL)?;
    // 先取结果再删临时文件：查询失败路径也不遗留 .sql
    let res = crate::trace::run_trace_processor(tp, local_path, &sql_path);
    let _ = std::fs::remove_file(&sql_path);
    let (stdout, _stderr) = res?;
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
/// jank 阈值语义与 agent fps.rs 一致（间隔 > 2×窗口中位间隔，间隔数 <3 不计），
/// 但**不含跨窗口边界间隔**（agent 的 prev 末帧语义此处不适用：相邻两次录制之间
/// 有 ~1-2s 盲区，边界间隔混入的是录制间隙而非真实卡顿）。
/// `watermark` 为跨窗口帧去重水位（顺序录制窗口本不相交，水位为防御性保底）。
fn summarize_window(frames_ns: &[i64], watermark: &mut i64, wall_secs: f32) -> (u32, f32, u32) {
    let kept: Vec<i64> = frames_ns.iter().copied().filter(|ts| *ts > *watermark).collect();
    if let Some(max) = kept.iter().max() {
        *watermark = *max;
    }
    let n = kept.len() as u32;
    let fps = if wall_secs > 0.0 { n as f32 / wall_secs } else { 0.0 };
    let jank = if kept.len() >= 4 {
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
///
/// 条目附带会话 stop 标志的弱引用：重连路径上 reconnect_agent 先 spawn 新会话、
/// 后 drop 旧流（旧线程滞后退出），若按「登记在即占用」判定会让重连后的会话
/// 永远被前任占位禁用。弱引用升级失败或 stop 已置位的前任视为可接管。
static LIGFX_BUSY: Mutex<Vec<(String, std::sync::Weak<AtomicBool>)>> = Mutex::new(Vec::new());

/// ligfx logcat 断流/启动失败后的重连间隔
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
        // 格式 `GVM_<comm>-<会话id>`：rsplit 剥最后一段会话 id（comm 本身可含 '-'，
        // 如 `GVM_my-proc-123` → `my-proc`；agent 侧 ligfx.rs 的 split('-').next()
        // 同源缺陷不在此修复——该通道在 SS4 永不命中）
        let raw = &after[..name_end];
        let name = raw.rsplit_once('-').map(|(n, _)| n).unwrap_or(raw).trim().to_string();
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

    /// 重建映射：`pidof <pkg>` → 逐 pid 读 `/proc/<pid>/comm`（内核已截断 15 字符）。
    /// pid token 先解析校验再拼 shell 命令（纵深防御；pidof 输出虽可信）
    fn refresh(&mut self, serial: Option<&str>, pkg: &str) {
        self.last_refresh = Instant::now();
        let Ok(out) = crate::utils::run_adb_command_for(serial, &["shell", "pidof", pkg]) else {
            return;
        };
        let mut m = HashMap::new();
        for pid in out.stdout.split_whitespace() {
            let Ok(pid) = pid.parse::<u32>() else {
                continue;
            };
            let Ok(comm) =
                crate::utils::run_adb_command_for(serial, &["shell", "cat", &format!("/proc/{pid}/comm")])
            else {
                continue;
            };
            m.insert(comm.stdout.trim().to_string(), pid);
        }
        self.map = m;
    }
}

/// ligfx 通道独占登记：占用者为活会话（stop 未置位）返回 false；前任已停/已死
/// 则接管并返回 true
fn ligfx_register(serial: &str, stop: &Arc<AtomicBool>) -> bool {
    let mut g = LIGFX_BUSY.lock().unwrap();
    if let Some(pos) = g.iter().position(|(s, _)| s == serial) {
        let incumbent_alive = g[pos].1.upgrade().map(|f| !f.load(Ordering::Relaxed)).unwrap_or(false);
        if incumbent_alive {
            return false;
        }
        g.remove(pos);
    }
    g.push((serial.to_string(), Arc::downgrade(stop)));
    true
}

/// 带重连宽限的登记：重连路径上 reconnect_agent 先 spawn 新会话、后 drop 旧流
/// （旧线程经看门狗 ~250ms 级才退出并释放登记）——登记被拒时按 500ms×20 重试，
/// 宽限内前任退出即接管；宽限耗尽仍被占才认定是真并发会话，报错放弃
fn ligfx_register_with_grace(serial: &str, stop: &Arc<AtomicBool>, tx: &HostTx) -> bool {
    for _ in 0..20 {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        if ligfx_register(serial, stop) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let _ = tx.send(Ok(AgentEvent::Err {
        msg: "SS4 GPU（ligfx）：通道被本进程另一会话占用，本会话 GPU busy 禁用（显存不受影响）".into(),
    }));
    false
}

/// 释放独占登记：仅摘除指向本会话 stop 的条目（可能已被重连后的新会话接管）
fn ligfx_unregister(serial: &str, stop: &Arc<AtomicBool>) {
    LIGFX_BUSY.lock().unwrap().retain(|(s, w)| {
        !(s == serial && w.upgrade().map(|f| Arc::ptr_eq(&f, stop)).unwrap_or(false))
    });
}

/// 启动 ligfx GPU 线程：经桥接网关在 MindRT 上跑 `logcat -s ligfxprofilerd`，
/// 系统行 → Gpu 事件，进程行 → GpuProc 事件（comm 归因到被测包 pid）。
///
/// 断流（MindRT 重启/桥接重建/adb 离线退出）后按 [`LIGFX_RECONNECT_SECS`] 间隔
/// 重连，网关 serial 每次尝试重新解析（桥接重建后可能变化）；连续速败达
/// [`MAX_CONSECUTIVE_FAILURES`] 次报错退出（与 frametimeline 通道同口径，
/// 断连恢复由 reconnect_agent → spawn_agent 重启通道）。
fn spawn_ligfx(pkg: String, serial: Option<String>, tx: HostTx, stop: Arc<AtomicBool>) {
    // 生效 serial（None 走全局目标）；bridged SS4 的 Android serial 恒为 localhost:<port>
    let Some(android_serial) = crate::utils::resolve_serial(serial.as_deref()) else {
        let _ = tx.send(Ok(AgentEvent::Err { msg: "SS4 GPU（ligfx）：无目标设备 serial，通道未启动".into() }));
        return;
    };
    std::thread::spawn(move || {
        // 登记入线程内带宽限重试（重连竞态：新 spawn 先于旧流 drop，旧线程
        // 滞后退出——同步登记必败，见 ligfx_register_with_grace）
        if !ligfx_register_with_grace(&android_serial, &stop, &tx) {
            return;
        }
        let mut comms = CommMap::new();
        let mut fails = 0u32;
        while !stop.load(Ordering::Relaxed) {
            // 网关每次尝试重新解析（桥接重建后映射可能变化）
            let Some(gateway) = crate::bridge::gateway_for_android(&android_serial) else {
                fails += 1;
                if ligfx_fail(&tx, fails, "无桥接网关信息（bridge 未收敛？）") || fails >= MAX_CONSECUTIVE_FAILURES {
                    break;
                }
                std::thread::sleep(Duration::from_secs(LIGFX_RECONNECT_SECS));
                continue;
            };
            // -T 0：不回放 logcat 历史缓冲（旧块会以当前时刻批量入账，污染时序）
            let child = crate::utils::adb_for(Some(&gateway))
                .args(["shell", "logcat", "-T", "0", "-s", "ligfxprofilerd"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn();
            let mut child = match child {
                Ok(c) => c,
                Err(e) => {
                    fails += 1;
                    if ligfx_fail(&tx, fails, &format!("logcat 启动失败: {e}")) || fails >= MAX_CONSECUTIVE_FAILURES {
                        break;
                    }
                    std::thread::sleep(Duration::from_secs(LIGFX_RECONNECT_SECS));
                    continue;
                }
            };
            let Some(stdout) = child.stdout.take() else {
                let _ = child.kill();
                let _ = child.wait();
                std::thread::sleep(Duration::from_secs(LIGFX_RECONNECT_SECS));
                continue;
            };
            // 看门狗：stop 置位时杀掉 logcat 子进程解除阻塞读（ligfx 静默期
            // 无线程可读行，仅靠行到达检查 stop 会让线程/子进程永久驻留）
            let conn_done = Arc::new(AtomicBool::new(false));
            let child_shared = Arc::new(Mutex::new(child));
            let wd = {
                let (c, s, d) = (child_shared.clone(), stop.clone(), conn_done.clone());
                std::thread::spawn(move || loop {
                    std::thread::sleep(Duration::from_millis(250));
                    if s.load(Ordering::Relaxed) || d.load(Ordering::Relaxed) {
                        if let Ok(mut c) = c.lock() {
                            let _ = c.kill();
                        }
                        break;
                    }
                })
            };
            let mut got_line = false;
            for line in BufReader::new(stdout).lines() {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(line) = line else { break }; // 流断
                got_line = true;
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
                    break; // 会话结束（receiver 已析构）
                }
            }
            // 收尾：置位让看门狗退出，杀+wait 回收子进程，join 看门狗
            conn_done.store(true, Ordering::Relaxed);
            if let Ok(mut c) = child_shared.lock() {
                let _ = c.kill();
                let _ = c.wait();
            }
            let _ = wd.join();
            if stop.load(Ordering::Relaxed) {
                break;
            }
            // 读到过行的连接视为健康（断流属正常重启场景）；零行速败计失败
            // （adb 离线立即 EOF，不计数会形成无间隔热重连循环刷 adb server）
            if got_line {
                fails = 0;
            } else {
                fails += 1;
                if ligfx_fail(&tx, fails, "logcat 流零行即断（设备离线？）") || fails >= MAX_CONSECUTIVE_FAILURES {
                    break;
                }
            }
            std::thread::sleep(Duration::from_secs(LIGFX_RECONNECT_SECS));
        }
        // 释放进程内独占（仅摘本会话条目，重连接管者不受影响）
        ligfx_unregister(&android_serial, &stop);
    });
}

/// ligfx 通道失败上报：true = 发送端已死（调用方直接退出线程）。
/// 连续失败上限在调用点判定（终态文案由这里统一发）
fn ligfx_fail(tx: &HostTx, fails: u32, detail: &str) -> bool {
    let msg = if fails >= MAX_CONSECUTIVE_FAILURES {
        format!("SS4 GPU（ligfx）连续 {fails} 次失败，通道退出（最后: {detail}）")
    } else {
        format!("SS4 GPU（ligfx）{detail}（{fails}/{MAX_CONSECUTIVE_FAILURES}，{LIGFX_RECONNECT_SECS}s 后重连）")
    };
    tx.send(Ok(AgentEvent::Err { msg })).is_err()
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
        // comm 含 '-'：rsplit 只剥会话 id 后缀
        let dash = "x ligfxprofilerd: [GPU0]   GVM_my-proc-123: Busy=1.0%, Utilization=1.0%";
        match parse_ligfx_line(dash) {
            Some(LigfxEvent::Proc { name, .. }) => assert_eq!(name, "my-proc"),
            _ => panic!("应为 Proc"),
        }
    }

    /// 独占登记：活会话占用被拒；前任 stop 置位（重连竞态）可接管；
    /// 释放只摘本会话条目，不误伤接管者
    #[test]
    fn test_ligfx_registry_takeover() {
        let serial = "test-serial-registry";
        let stop1 = Arc::new(AtomicBool::new(false));
        assert!(ligfx_register(serial, &stop1));
        // 活会话占用 → 拒绝
        let stop2 = Arc::new(AtomicBool::new(false));
        assert!(!ligfx_register(serial, &stop2));
        // 前任 stop 置位（重连：新 spawn 先于旧流 drop）→ 接管
        stop1.store(true, Ordering::Relaxed);
        assert!(ligfx_register(serial, &stop2));
        // 前任滞后的释放不得误伤接管者
        ligfx_unregister(serial, &stop1);
        let stop3 = Arc::new(AtomicBool::new(false));
        assert!(!ligfx_register(serial, &stop3), "接管者应仍在位");
        // 正常释放后清空
        ligfx_unregister(serial, &stop2);
        assert!(ligfx_register(serial, &stop3));
        ligfx_unregister(serial, &stop3);
    }

    /// 带宽限的登记：前任 300ms 后释放（模拟重连竞态下旧线程滞后退出），
    /// 宽限内应接管成功
    #[test]
    fn test_ligfx_register_grace() {
        let serial = "test-serial-grace";
        let stop1 = Arc::new(AtomicBool::new(false));
        let stop2 = Arc::new(AtomicBool::new(false));
        assert!(ligfx_register(serial, &stop1));
        // 300ms 后前任停止并释放
        {
            let (s1, serial) = (stop1.clone(), serial.to_string());
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(300));
                s1.store(true, Ordering::Relaxed);
                ligfx_unregister(&serial, &s1);
            });
        }
        let (tx, _rx) = mpsc::channel();
        assert!(ligfx_register_with_grace(serial, &stop2, &tx));
        ligfx_unregister(serial, &stop2);
    }
}
