//! logcat 日志抓取模块（CLI `--logcat` 与 GUI「日志」tab 共用）。
//!
//! 形态：host 侧 spawn `adb logcat` 流式子进程，行读线程把日志流式写入本机文件
//! （崩溃只丢尾部），可选回调批量把行推给 GUI 前端。SSH 远程模式下 adb 命令经
//! hop#1 隧道（`ADB_SERVER_SOCKET`，见 [`crate::utils::adb_for`]），本机读 stdout
//! 落盘，零隧道改动。
//!
//! **按包过滤**（[`crate::logcat::resolve_package_filter`]）：Android 12+ 用 `logcat --uid=`（uid
//! 不随进程重启变化，崩溃/重启后新进程日志不丢；SS4 多用户返回逗号列表原样透传）；
//! Android 11 及以下 logcat 无 `--uid`（SS2MAX 实测 `Unknown option`），降级
//! `--pid=<pidof 首个 pid>`——限制：进程重启后新进程日志缺失，且只覆盖首个进程，
//! 调用方应如实提示。无包名时 [`crate::logcat::LogcatFilter::All`] 全机抓取。
//!
//! **时间轴对齐**：日志行格式 `-v threadtime -v year`（设备时钟），与采样 CSV 的
//! Timestamp（agent 设备端 epoch）同源，天然可对齐。`-T 0` 不回放历史缓冲（A11 会
//! 降级为 1 行 backlog 并在 stderr 告警，stdout 不受影响）。
//!
//! **断连恢复**：adb logcat 因设备断开 EOF 时，1s 退避自动重 spawn（与采样
//! `reconnect_agent` 语义一致）；设备在线但子进程秒死（参数错误类永久性失败）连续
//! 3 次则放弃并经 [`crate::logcat::LogcatEvent::Error`] 上报（避免静默死循环）。

use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::Local;

use crate::csvstream::validate_package_name;
use crate::utils::{adb_for, run_adb_command_for};

/// 日志行推送给前端的批量间隔（聚合突发流量，防 GUI 事件风暴）
const EMIT_BATCH_INTERVAL: Duration = Duration::from_millis(200);
/// 单次批量推送的最大行数（到达即提前推送）
const EMIT_BATCH_MAX_LINES: usize = 64;
/// 暂停事件期间后端攒批上限（尾部保留；前端视图 ring buffer 2000 行的 2 倍余量，
/// 超出从头部丢——与视图「最近 N 行」语义一致，全量数据在落盘文件）
const PENDING_CAP: usize = 4000;
/// 断连重连退避间隔
const RECONNECT_BACKOFF: Duration = Duration::from_secs(1);
/// 子进程存活低于该时长视为「秒死」（参数错误类永久性失败的判据）
const FAST_DEATH: Duration = Duration::from_secs(3);
/// 设备在线前提下连续秒死次数上限，达到即放弃抓取并上报错误
const MAX_FAST_DEATHS: u32 = 3;

/// logcat 过滤口径
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogcatFilter {
    /// 全机抓取（无包名场景）
    All,
    /// 按 uid 过滤（Android 12+；多用户为逗号列表，如 `10220,99910220`）
    Uid(String),
    /// 按 pid 过滤（Android 11 及以下降级路径；进程重启后新进程日志缺失）
    Pid(u32),
}

/// 抓取向调用方上报的事件
#[derive(Debug)]
pub enum LogcatEvent {
    /// 批量日志行（threadtime+year 格式原文）
    Lines(Vec<String>),
    /// 抓取终止（连续秒死，message 含诊断）；正常停止/断连重连不产生本事件
    Error(String),
}

/// logcat 抓取句柄：[`LogcatHandle::stop`] 杀子进程并收线程（幂等）；Drop 同语义兜底
pub struct LogcatHandle {
    /// 落盘文件路径（`<dest_dir>/logcat.log`）
    path: PathBuf,
    stop: Arc<AtomicBool>,
    child: Arc<std::sync::Mutex<Option<Child>>>,
    /// 当前过滤口径（[`LogcatHandle::restart`] 热更新；写线程每次重 spawn 时读取）
    config: Arc<std::sync::Mutex<(LogcatFilter, Option<char>)>>,
    /// restart 触发的计划内重 spawn 标记（EOF 处理处消费）：restart 的 kill 是主动行为，
    /// 不计入秒死统计——否则快速连切级别会误判永久失败把抓线程打死
    restarting: Arc<AtomicBool>,
    /// 事件推送暂停标志（[`LogcatHandle::pause_events`]）：置位期间写线程不发 `Lines`
    /// 事件（攒入 pending，上限 4000 行尾部保留），恢复后 ≤200ms 一次性补发——用于 GUI
    /// 日志页不可见时关掉 webview IPC 开销（实测全机洪泛 ~190 行/s 时 ~3% 单核）。
    /// `Error` 事件不受暂停影响（抓取终止必须让前端复位按钮）
    events_paused: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl LogcatHandle {
    /// 落盘文件路径
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 热切换过滤口径（级别/按包过滤变更）：更新共享配置 + 杀当前子进程——
    /// 写线程走既有断连重连路径按新配置重 spawn（同一文件续写，标记行带新口径）。
    /// 重连退避窗口内调用同样生效（重 spawn 时读的是最新配置）；进程重启换包等
    /// 需重新解析 uid/pid 的场景，调用方先走 [`crate::logcat::resolve_package_filter`]
    pub fn restart(&self, filter: LogcatFilter, min_level: Option<char>) {
        *self.config.lock().unwrap() = (filter, min_level);
        self.restarting.store(true, Ordering::SeqCst);
        if let Some(c) = self.child.lock().unwrap().as_mut() {
            let _ = c.kill();
        }
    }

    /// 暂停 `Lines` 事件推送（GUI 日志页不可见时调用，关掉 webview IPC 开销）：
    /// 落盘不受影响；行攒入 pending（上限 4000 行尾部保留），恢复后补发
    pub fn pause_events(&self) {
        self.events_paused.store(true, Ordering::SeqCst);
    }

    /// 恢复事件推送：pending 攒批 ≤200ms 内一次性补发（写线程下一个合帧周期排空）
    pub fn resume_events(&self) {
        self.events_paused.store(false, Ordering::SeqCst);
    }

    /// 抓取是否已终止（写线程退出：连续秒死放弃 / 已 stop）。GUI 重启会话前探测，
    /// 避免死句柄占槽位导致「已在抓取」误拒（error 事件可能晚到/丢失，不能只靠前端复位）
    pub fn is_done(&self) -> bool {
        self.join.as_ref().map(|j| j.is_finished()).unwrap_or(true)
    }

    /// 停止抓取：置停止标志 + 杀子进程（读线程随 EOF 退出）+ join 写线程。
    /// 幂等；与 Drop 共用同一路径
    pub fn stop(mut self) {
        self.stop_inner();
    }

    fn stop_inner(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(mut c) = self.child.lock().unwrap().take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for LogcatHandle {
    fn drop(&mut self) {
        self.stop_inner();
    }
}

/// 解析 `pm list package -U <pkg>` 输出中的 uid 列表。
/// 形如 `package:<pkg> uid:10136` 或 SS4 多用户 `uid:10220,99910220`；
/// 找不到（包未安装）返回 None。
fn parse_uid_list(pm_output: &str, package: &str) -> Option<String> {
    let prefix = format!("package:{}", package);
    for line in pm_output.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(&prefix) {
            if let Some(uids) = rest.trim().strip_prefix("uid:") {
                let uids = uids.trim();
                // 校验逗号列表全为数字，原样透传给 --uid（防异常输出拼进命令行）
                if !uids.is_empty()
                    && uids.split(',').all(|u| !u.is_empty() && u.bytes().all(|b| b.is_ascii_digit()))
                {
                    return Some(uids.to_string());
                }
            }
        }
    }
    None
}

/// 解析 `ro.build.version.release` 主版本号（`12`/`16`/`8.1.0` → 12/16/8）
fn parse_android_major(release: &str) -> Option<u32> {
    release
        .trim()
        .split('.')
        .next()?
        .bytes()
        .take_while(|b| b.is_ascii_digit())
        .fold(String::new(), |mut s, b| {
            s.push(b as char);
            s
        })
        .parse()
        .ok()
}

/// 目标设备的 Android 主版本号（`getprop ro.build.version.release`）
fn android_major_version(serial: Option<&str>) -> Result<u32> {
    let out = run_adb_command_for(serial, &["shell", "getprop", "ro.build.version.release"])?;
    parse_android_major(&out.stdout)
        .with_context(|| format!("无法解析 Android 版本: {:?}", out.stdout.trim()))
}

/// 解析按包过滤口径：Android 12+ → [`LogcatFilter::Uid`]（uid 经
/// `pm list package -U` 获取，多用户逗号列表原样透传）；Android 11 及以下
/// logcat 无 `--uid`，降级 [`LogcatFilter::Pid`]（`pidof` 首个 pid）。
///
/// 失败语义：包未安装 / A11 下应用未运行均报错（调用方如实透传）。
pub fn resolve_package_filter(serial: Option<&str>, package: &str) -> Result<LogcatFilter> {
    validate_package_name(package)?;
    if android_major_version(serial)? >= 12 {
        let out = run_adb_command_for(serial, &["shell", "pm", "list", "package", "-U", package])?;
        let uids = parse_uid_list(&out.stdout, package)
            .with_context(|| format!("包未安装或 uid 解析失败: {}", package))?;
        Ok(LogcatFilter::Uid(uids))
    } else {
        let out = run_adb_command_for(serial, &["shell", "pidof", package])?;
        let pid: u32 = out
            .stdout
            .split_whitespace()
            .next()
            .and_then(|p| p.parse().ok())
            .with_context(|| {
                format!(
                    "Android 11 及以下平台 logcat 无 --uid 过滤，按 pid 过滤需应用已运行: {}",
                    package
                )
            })?;
        Ok(LogcatFilter::Pid(pid))
    }
}

/// 组装 `adb logcat` 参数（纯函数，便于单测）：`-v threadtime -v year -T 0`
/// + 过滤口径 + 可选最低级别（`*:W` 形式；level 限 V/D/I/W/E/F）
fn build_logcat_args(filter: &LogcatFilter, min_level: Option<char>) -> Vec<String> {
    let mut args: Vec<String> = [
        "logcat", "-v", "threadtime", "-v", "year", "-T", "0",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    match filter {
        LogcatFilter::All => {}
        LogcatFilter::Uid(uids) => args.push(format!("--uid={}", uids)),
        LogcatFilter::Pid(pid) => args.push(format!("--pid={}", pid)),
    }
    if let Some(l) = min_level {
        if "VDIWEF".contains(l) {
            args.push(format!("*:{}", l));
        }
    }
    args
}

/// 行读线程 → 写线程的消息
enum Msg {
    /// 一行日志（不含换行）
    Line(String),
    /// 子进程 stdout EOF（设备断开或子进程死亡）
    Eof,
}

/// spawn 一个 logcat 子进程 + 行读线程（行经 tx 送写线程）
fn spawn_logcat(
    serial: Option<&str>,
    args: &[String],
    tx: mpsc::Sender<Msg>,
) -> Result<Child> {
    let mut child = adb_for(serial)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("启动 adb logcat 失败")?;
    let stdout = child.stdout.take().context("adb logcat stdout 不可用")?;
    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            match line {
                Ok(l) => {
                    if tx.send(Msg::Line(l)).is_err() {
                        return;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = tx.send(Msg::Eof);
    });
    Ok(child)
}

/// 启动 logcat 抓取：流式写入 `<dest_dir>/logcat.log`（逐行 flush，崩溃只丢尾部）。
///
/// - `serial`：目标设备（`None` 回退全局选择，语义同其他 core 入口）
/// - `filter`：[`resolve_package_filter`] 的结果或 [`LogcatFilter::All`]
/// - `min_level`：最低级别（V/D/I/W/E/F），`None` 不过滤
/// - `on_event`：可选事件回调（GUI 批量推前端；CLI 传 `None` 只落盘）
///
/// 返回 [`LogcatHandle`]；设备断开自动重连（1s 退避，`-T 0` 不回放历史）。
/// 文件头写入一行 `#` 注释（host 时间/serial/过滤口径），每次重连追加一行标记。
pub fn start_logcat(
    serial: Option<&str>,
    dest_dir: &Path,
    filter: LogcatFilter,
    min_level: Option<char>,
    on_event: Option<Box<dyn Fn(LogcatEvent) + Send>>,
) -> Result<LogcatHandle> {
    std::fs::create_dir_all(dest_dir)?;
    let path = dest_dir.join("logcat.log");
    // 过滤口径共享槽：restart() 热更新，写线程每次重 spawn 时重读组装参数
    let config = Arc::new(std::sync::Mutex::new((filter, min_level)));
    let (init_filter, init_level) = config.lock().unwrap().clone();
    let args = build_logcat_args(&init_filter, init_level);
    let serial_owned = serial.map(|s| s.to_string());

    let (tx, rx) = mpsc::channel::<Msg>();
    let child = spawn_logcat(serial, &args, tx.clone())?;
    let child_slot: Arc<std::sync::Mutex<Option<Child>>> = Arc::new(std::sync::Mutex::new(Some(child)));
    let stop = Arc::new(AtomicBool::new(false));
    let restarting = Arc::new(AtomicBool::new(false));
    let events_paused = Arc::new(AtomicBool::new(false));

    let mut file = BufWriter::new(std::fs::File::create(&path)?);
    let header = format!(
        "# xperf logcat | start={} | serial={} | filter={:?} | level={}",
        Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
        crate::utils::resolve_serial(serial).unwrap_or_else(|| "(auto)".into()),
        init_filter,
        init_level.map(|l| l.to_string()).unwrap_or_else(|| "V".into()),
    );
    writeln!(file, "{}", header)?;
    file.flush()?;

    let writer_stop = stop.clone();
    let writer_child = child_slot.clone();
    let writer_config = config.clone();
    let writer_restarting = restarting.clone();
    let writer_paused = events_paused.clone();
    let join = std::thread::spawn(move || {
        let mut batch: Vec<String> = Vec::new();
        // 暂停事件期间的攒批（尾部保留，恢复时优先补发）
        let mut pending: Vec<String> = Vec::new();
        let mut last_emit = Instant::now();
        let mut fast_deaths: u32 = 0;
        let mut spawned_at = Instant::now();

        // 推送当前批（与恢复后排空的 pending）：暂停时攒入 pending；未暂停时
        // pending 先于新批发出（保序）；无回调（CLI 落盘模式）两者都直接清
        let emit_lines = |batch: &mut Vec<String>, pending: &mut Vec<String>| {
            if let Some(cb) = &on_event {
                if writer_paused.load(Ordering::SeqCst) {
                    if !batch.is_empty() {
                        pending.append(batch);
                        while pending.len() > PENDING_CAP {
                            pending.remove(0);
                        }
                    }
                    return;
                }
                if !pending.is_empty() {
                    cb(LogcatEvent::Lines(std::mem::take(pending)));
                }
                if !batch.is_empty() {
                    cb(LogcatEvent::Lines(std::mem::take(batch)));
                }
            } else {
                batch.clear();
                pending.clear();
            }
        };

        loop {
            match rx.recv_timeout(EMIT_BATCH_INTERVAL) {
                Ok(Msg::Line(l)) => {
                    let _ = writeln!(file, "{}", l);
                    let _ = file.flush();
                    batch.push(l);
                    if batch.len() >= EMIT_BATCH_MAX_LINES
                        || last_emit.elapsed() >= EMIT_BATCH_INTERVAL
                    {
                        emit_lines(&mut batch, &mut pending);
                        last_emit = Instant::now();
                    }
                }
                Ok(Msg::Eof) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    emit_lines(&mut batch, &mut pending);
                    // 子进程 EOF：收尸并决定重连或结束
                    let mut slot = writer_child.lock().unwrap();
                    if let Some(mut c) = slot.take() {
                        let _ = c.kill();
                        let _ = c.wait();
                    }
                    if writer_stop.load(Ordering::SeqCst) {
                        break;
                    }
                    // restart 触发的计划内重 spawn：消费标记，跳过秒死统计
                    let planned = writer_restarting.swap(false, Ordering::SeqCst);
                    let died_fast = !planned && spawned_at.elapsed() < FAST_DEATH;
                    if died_fast {
                        fast_deaths += 1;
                    } else {
                        fast_deaths = 0;
                    }
                    // 设备在线但秒死 = 永久性失败（参数错误等），连续达到上限即放弃；
                    // 设备离线（get-state != device）属断连，按退避重连
                    if fast_deaths >= MAX_FAST_DEATHS
                        && crate::agent::device_online(serial_owned.as_deref())
                    {
                        if let Some(cb) = &on_event {
                            cb(LogcatEvent::Error(format!(
                                "logcat 子进程连续 {} 次秒死，放弃抓取（设备在线，疑似参数不兼容）",
                                fast_deaths
                            )));
                        }
                        break;
                    }
                    // 重 spawn（断连重连 / restart 热切换同路径）：读最新配置组装参数
                    let (cur_filter, cur_level) = writer_config.lock().unwrap().clone();
                    let cur_args = build_logcat_args(&cur_filter, cur_level);
                    let _ = writeln!(
                        file,
                        "# xperf logcat respawn {} | filter={:?} | level={}",
                        Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
                        cur_filter,
                        cur_level.map(|l| l.to_string()).unwrap_or_else(|| "V".into()),
                    );
                    let _ = file.flush();
                    std::thread::sleep(RECONNECT_BACKOFF);
                    if writer_stop.load(Ordering::SeqCst) {
                        break;
                    }
                    match spawn_logcat(serial_owned.as_deref(), &cur_args, tx.clone()) {
                        Ok(mut c) => {
                            // 竞态收敛（restart 落在「读 config→spawn」窗口内）：restart 的
                            // kill 可能落空（当时 slot 已被 take），标记滞留会致新口径静默
                            // 失效直到下次断连——spawn 后补检 restarting，置位则立即杀掉
                            // 刚起的进程，随 Eof 走 planned 重 spawn 读到新口径
                            if writer_restarting.load(Ordering::SeqCst) {
                                let _ = c.kill();
                                let _ = c.wait();
                            } else {
                                *slot = Some(c);
                                spawned_at = Instant::now();
                            }
                        }
                        Err(_) => {
                            // spawn 失败（如 adb 不可用）按断连处理，下轮重试
                            std::thread::sleep(RECONNECT_BACKOFF);
                            // 注入一条 Eof 驱动循环继续（无读线程不会再有消息）
                            let _ = tx.send(Msg::Eof);
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // 空转周期也尝试排空（暂停恢复后无新行时 pending 及时补发）
                    emit_lines(&mut batch, &mut pending);
                    last_emit = Instant::now();
                }
            }
            if writer_stop.load(Ordering::SeqCst) {
                // 停止路径：杀掉子进程后读线程 EOF，随下条 Eof 消息收尾；
                // 此处提前 drain 批量避免滞留
                emit_lines(&mut batch, &mut pending);
            }
        }
        let _ = file.flush();
    });

    Ok(LogcatHandle {
        path,
        stop,
        child: child_slot,
        config,
        restarting,
        events_paused,
        join: Some(join),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_uid_list_single() {
        let out = "package:com.google.android.filament.gltf uid:10136\n";
        assert_eq!(
            parse_uid_list(out, "com.google.android.filament.gltf"),
            Some("10136".to_string())
        );
    }

    #[test]
    fn test_parse_uid_list_multi_user() {
        // SS4（Android 16）多用户返回逗号列表
        let out = "package:com.google.android.filament.gltf uid:10220,99910220\n";
        assert_eq!(
            parse_uid_list(out, "com.google.android.filament.gltf"),
            Some("10220,99910220".to_string())
        );
    }

    #[test]
    fn test_parse_uid_list_not_found() {
        assert_eq!(parse_uid_list("", "com.x"), None);
        assert_eq!(parse_uid_list("package:com.other uid:1\n", "com.x"), None);
    }

    #[test]
    fn test_parse_uid_list_rejects_garbage() {
        // uid 非纯数字逗号列表时拒绝透传（防异常输出拼进 --uid 命令行）
        let out = "package:com.x uid:10x36\n";
        assert_eq!(parse_uid_list(out, "com.x"), None);
        let out = "package:com.x uid:\n";
        assert_eq!(parse_uid_list(out, "com.x"), None);
    }

    #[test]
    fn test_parse_android_major() {
        assert_eq!(parse_android_major("12"), Some(12));
        assert_eq!(parse_android_major("16\n"), Some(16));
        assert_eq!(parse_android_major("11"), Some(11));
        assert_eq!(parse_android_major("8.1.0"), Some(8));
        assert_eq!(parse_android_major(""), None);
        assert_eq!(parse_android_major("abc"), None);
    }

    #[test]
    fn test_build_logcat_args_all() {
        let args = build_logcat_args(&LogcatFilter::All, None);
        assert_eq!(
            args,
            vec!["logcat", "-v", "threadtime", "-v", "year", "-T", "0"]
        );
    }

    #[test]
    fn test_build_logcat_args_uid_and_level() {
        let args = build_logcat_args(&LogcatFilter::Uid("10220,99910220".into()), Some('W'));
        assert_eq!(
            args,
            vec![
                "logcat", "-v", "threadtime", "-v", "year", "-T", "0",
                "--uid=10220,99910220", "*:W"
            ]
        );
    }

    #[test]
    fn test_build_logcat_args_pid() {
        let args = build_logcat_args(&LogcatFilter::Pid(9671), None);
        assert!(args.contains(&"--pid=9671".to_string()));
    }

    #[test]
    fn test_build_logcat_args_invalid_level_ignored() {
        let args = build_logcat_args(&LogcatFilter::All, Some('X'));
        assert!(!args.iter().any(|a| a.starts_with("*:")));
    }

    /// 集成测试：start → 全机抓取有行 → restart 切级别+按包过滤（同文件续写
    /// 标记行）→ restart 回全机 → stop。需 hppc 远程环境 + SS3 在线 +
    /// gltf 已安装（过滤解析只要求已安装，不要求运行）。
    #[test]
    #[ignore = "需要 hppc：SSH 免密 + 远端 adb + SS3(6eb792dfb0f) 在线"]
    fn test_logcat_restart_hppc() {
        let target = crate::transport::SshTarget::new("hppc")
            .with_adb_path("~/Android/Sdk/platform-tools/adb");
        crate::transport::init_remote(target).expect("init_remote 失败");
        let serial = "6eb792dfb0f";
        let dir = std::env::temp_dir().join(format!("xperf_logcattest_{}", std::process::id()));
        let r = (|| -> Result<()> {
            let h = start_logcat(Some(serial), &dir, LogcatFilter::All, None, None)?;
            std::thread::sleep(std::time::Duration::from_secs(4));
            // 热切换：按包过滤 + W 级别
            let f = resolve_package_filter(Some(serial), "com.google.android.filament.gltf")?;
            h.restart(f, Some('W'));
            std::thread::sleep(std::time::Duration::from_secs(4));
            // 热切回全机
            h.restart(LogcatFilter::All, None);
            std::thread::sleep(std::time::Duration::from_secs(3));
            let path = h.path().to_path_buf();
            h.stop();
            let content = std::fs::read_to_string(&path)?;
            // 同文件续写：两次 respawn 标记行（携带新口径）
            let marks = content.matches("# xperf logcat respawn").count();
            assert!(marks >= 2, "应至少 2 条 respawn 标记行，实际 {}：\n{}", marks, content);
            assert!(content.contains("filter=Uid"), "标记行应含 Uid 过滤口径");
            // 全机段应有真实日志行（threadtime+year 行首）
            assert!(content.contains("2026-"), "应有真实日志行");
            Ok(())
        })();
        crate::transport::shutdown_remote();
        let _ = std::fs::remove_dir_all(&dir);
        r.expect("logcat restart 集成测试失败");
    }
}
