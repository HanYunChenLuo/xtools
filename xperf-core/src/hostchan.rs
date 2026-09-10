//! host 侧采样通道（SS4 专属）：设备端 agent 数据源不可达时的替代路径。
//!
//! 当前仅 **GPU busy** 一项：ligfxprofilerd 在 MindRT 侧输出 logcat，GVM 内无任何
//! 输出，agent 的 ligfx 通道永久不可用，须 host 侧经桥接网关读 MindRT logcat。
//!
//! （历史注记：SS4 的 FPS 曾走 host 侧 frametimeline perfetto 短窗通道——当时误判
//! `dumpsys SurfaceFlinger --latency` 被平台阉割；2026-09-10 复测确证根因是 A16 SF
//! 只认带 `<hex> ` 别名前缀的图层名，agent v5 修复后设备端 per-layer 路径恢复，
//! host FPS 通道已删除。）
//!
//! 架构：[`maybe_spawn`](crate::hostchan::maybe_spawn) 在 `spawn_agent` 内按平台+指标
//! 开关启动 host 侧线程，线程合成 [`AgentEvent`](crate::agent::AgentEvent) 经 mpsc 汇入
//! [`crate::agent::AgentStream`] 的 `next_event`/`next_event_batch`（消费端 CLI/GUI
//! 零改动）。线程生命周期随 AgentStream：流析构 → receiver Drop → 发送失败退出；
//! 断连恢复由 `reconnect_agent` → `spawn_agent` 重启通道。

use crate::agent::AgentEvent;
use crate::platform::PlatformId;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::Stdio;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, atomic::AtomicBool, atomic::Ordering};
use std::time::{Duration, Instant};

/// 合成事件的发送端类型（Ok = 合成事件；Err 保留给行解析失败语义，本模块只用 Ok）
pub type HostTx = mpsc::Sender<std::result::Result<AgentEvent, String>>;
/// 合成事件的接收端类型（挂在 [`crate::agent::AgentStream`] 上）
pub type HostRx = mpsc::Receiver<std::result::Result<AgentEvent, String>>;

/// 连续失败上限：超过则通道线程退出（防止设备长期离线时 adb 报错空转；
/// 断连恢复由 reconnect_agent → spawn_agent 重启通道）
const MAX_CONSECUTIVE_FAILURES: u32 = 3;

/// 按平台与指标开关启动 SS4 host 侧通道，返回合成事件接收端。
///
/// 非 SS4 平台 / 未开 gpu / 无包名时返回 None（无通道）。`serial` 为 None 时
/// 走 adb 全局目标（CLI 单设备路径，与 agent 其余调用一致）。
/// `stop`：会话停止标志（AgentStream 的 ping_stop，kill/drop 时置位）——通道线程
/// 在重连点检查，保证会话结束后线程有界退出（发送失败检查只覆盖有事件可发的
/// 路径，设备离线期间无事件可发，须靠该标志）。
pub fn maybe_spawn(
    flags: crate::agent::MetricFlags,
    platform: Option<&dyn crate::platform::Platform>,
    package: Option<&str>,
    serial: Option<&str>,
    stop: Arc<AtomicBool>,
) -> Option<HostRx> {
    if platform?.id() != PlatformId::Ss4 || !flags.gpu {
        return None;
    }
    let pkg = package?.to_string();
    let serial = serial.map(str::to_string);
    let (tx, rx) = mpsc::channel();
    spawn_ligfx(pkg, serial, tx, stop);
    Some(rx)
}

// ==================== ligfx GPU 通道（经桥接网关读 MindRT logcat） ====================

/// 主机墙钟毫秒（合成事件 ts 用）
fn now_ms() -> u64 {
    chrono::Local::now().timestamp_millis() as u64
}

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
/// [`MAX_CONSECUTIVE_FAILURES`] 次报错退出（断连恢复由 reconnect_agent →
/// spawn_agent 重启通道）。
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
