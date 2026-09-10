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
use std::io::Write as _;
use std::path::Path;
use std::process::Stdio;
use std::sync::mpsc;
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
pub fn maybe_spawn(
    flags: crate::agent::MetricFlags,
    platform: Option<&dyn crate::platform::Platform>,
    package: Option<&str>,
    serial: Option<&str>,
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
        spawn_frametimeline(pkg.clone(), serial.clone(), tx.clone());
    }
    // flags.gpu 的 ligfx host 通道：任务 B（DESIGN-ss4-metrics.md）实施
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
fn spawn_frametimeline(pkg: String, serial: Option<String>, tx: HostTx) {
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
}
