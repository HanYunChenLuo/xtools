//! scrcpy 屏幕镜像集成：拉起外部 scrcpy 窗口（视频解码/触控注入由 scrcpy 客户端
//! 承担，本模块只负责进程与隧道生命周期）。CLI `--mirror` 与 GUI「屏幕镜像」按钮共用。
//!
//! SSH 远程模式（[`crate::transport::Transport::Ssh`]）：scrcpy 默认的 `adb reverse`
//! 通道在该拓扑下不可用——reverse 规则的路由终点是远端 adb server，视频流回不到
//! 经隧道的 TCP 客户端（2026-09-11 实测：规则注册成功但设备侧连接永不到达）。
//! 故必须 `--tunnel-port=P`（隐含 `--force-adb-forward`）：scrcpy 把
//! `adb forward tcp:P localabstract:scrcpy-<scid>` 注册到远端 server，再连接本机
//! `127.0.0.1:P`——该端口须由 hop#2（[`crate::transport::SshTunnel::add_forward_pinned`]）
//! 映射回远端同名端口，即 P 须在**本机与远端两侧同时空闲**（端口池 27183..=27199
//! 扫描，与 scrcpy 默认候选范围一致）。

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};

/// scrcpy 隧道端口池下限（与 scrcpy 默认候选端口范围一致）
const TUNNEL_PORT_MIN: u16 = 27183;
/// scrcpy 隧道端口池上限（含）
const TUNNEL_PORT_MAX: u16 = 27199;

/// stderr 尾行缓冲容量（诊断 scrcpy 启动失败用；scrcpy 日志量小，12 行足够）
const STDERR_TAIL_CAP: usize = 12;

/// 镜像退出的语义（供调用方区分「用户关窗」与「真异常」——scrcpy 正常运行也会
/// 往 stderr 打启动日志，不能凭 stderr 非空判异常）
#[derive(Debug)]
pub enum MirrorExit {
    /// 用户主动停止（[`MirrorHandle::stop`]）或宿主进程退出中（中断标志置位）
    Stopped,
    /// scrcpy 正常退出（exit code 0，典型为用户关闭镜像窗口）
    Closed,
    /// scrcpy 异常退出（非零 exit code；附 stderr 尾行诊断）
    Failed(String),
}

/// scrcpy 可执行文件探测：PATH 各目录 → 常见安装位置兜底
/// （GUI 从 Finder/桌面环境启动时 PATH 常不含 `/opt/homebrew/bin` 等）。
/// 仅面向 macOS/Linux 宿主（无 .exe 探测）。
pub fn find_scrcpy() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).map(|d| d.join("scrcpy")).collect())
        .unwrap_or_default();
    for extra in [
        "/opt/homebrew/bin/scrcpy",
        "/usr/local/bin/scrcpy",
        "/usr/bin/scrcpy",
        "/opt/local/bin/scrcpy",
    ] {
        candidates.push(PathBuf::from(extra));
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// 解析 `adb forward --list` 的一行：`<serial> tcp:<port> <target>` →
/// `Some((serial, port, target))`；格式不符返回 None。
fn parse_forward_line(line: &str) -> Option<(&str, u16, &str)> {
    let tok: Vec<&str> = line.split_whitespace().collect();
    if tok.len() != 3 {
        return None;
    }
    let port = tok[1].strip_prefix("tcp:")?.parse::<u16>().ok()?;
    Some((tok[0], port, tok[2]))
}

/// 远端 adb server 上已被占用的 forward 端口集合（forward 端口是 server 全局资源，
/// 不区分 serial——其他设备/工具注册的规则同样占用）
fn remote_busy_ports() -> HashSet<u16> {
    let mut busy = HashSet::new();
    if let Ok(out) = crate::utils::adb_for(None).args(["forward", "--list"]).output() {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if let Some((_, port, _)) = parse_forward_line(line) {
                busy.insert(port);
            }
        }
    }
    busy
}

/// 从端口池中选一个本机/远端两侧同时空闲的端口（纯函数便于单测；
/// `local_free` 为可注入的本机可用性判定）
fn pick_tunnel_port(
    busy: &HashSet<u16>,
    local_free: impl Fn(u16) -> bool,
) -> Option<u16> {
    (TUNNEL_PORT_MIN..=TUNNEL_PORT_MAX).find(|&p| !busy.contains(&p) && local_free(p))
}

/// 本机端口可用性判定（bind 即放；TOCTOU 窗口由 `-O forward` 的
/// `ExitOnForwardFailure` 快速失败兜底）
fn local_port_free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// 清扫指定设备残留的 scrcpy forward 规则（scrcpy 被 SIGKILL 时无自清机会，
/// 规则由 adb server 持有残留；按 serial 限定作用域，不影响其他设备/其他工具）。
/// 启动前与退出清理各执行一次；adb 失败 best-effort 忽略。
pub fn sweep_scrcpy_rules(serial: &str) {
    let Ok(out) = crate::utils::adb_for(None).args(["forward", "--list"]).output() else {
        return;
    };
    let list = String::from_utf8_lossy(&out.stdout).to_string();
    for line in list.lines() {
        if let Some((s, _, target)) = parse_forward_line(line) {
            if s == serial && target.starts_with("localabstract:scrcpy") {
                let port = parse_forward_line(line).map(|(_, p, _)| p);
                if let Some(p) = port {
                    let _ = crate::utils::adb_for(Some(serial))
                        .args(["forward", "--remove", &format!("tcp:{p}")])
                        .output();
                }
            }
        }
    }
}

/// 一路屏幕镜像会话（scrcpy 子进程 + 远程模式的 hop#2 端口映射）。
///
/// 生命周期：GUI 监护线程/CLI 镜像-only 模式经 [`MirrorHandle::wait_exit`] 阻塞等待
/// （用户关窗/进程退出/全局中断标志置位都会返回，返回前已做完整清理）；
/// 显式停止用 [`MirrorHandle::stop`]（杀子进程，清理由 wait_exit/Drop 兜底）；
/// Drop 保证清理只执行一次：杀子进程（若还活着）→ 摘除 hop#2 → 清扫该设备
/// 残留的 scrcpy forward 规则。
pub struct MirrorHandle {
    /// 目标设备 serial（清理时按它清扫规则）
    serial: String,
    /// scrcpy 子进程（Arc 共享：watcher 轮询 try_wait 与 stop 的 kill 并发安全）
    child: Arc<Mutex<Child>>,
    /// stderr 尾行缓冲（drain 线程持有写端，容量有界；错误诊断用）
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    /// 远程模式建立的 hop#2 远端端口（清理时经隧道摘除；本地模式为 None）
    hop2_port: Option<u16>,
    /// 清理只执行一次（wait_exit/Drop 双入口）
    cleaned: Arc<Mutex<bool>>,
    /// 用户主动停止标志（stop 置位；wait_exit 据此归因为 Stopped 而非异常）
    stopped: Arc<std::sync::atomic::AtomicBool>,
}

impl MirrorHandle {
    /// scrcpy 进程是否仍存活
    pub fn is_alive(&self) -> bool {
        self.child
            .lock()
            .map(|mut g| matches!(g.try_wait(), Ok(None)))
            .unwrap_or(false)
    }

    /// 阻塞等待 scrcpy 退出（用户关窗/连接失败自杀）或全局中断标志置位
    /// （进程退出中——CLI Ctrl-C / GUI 关窗）；返回前清理已完成。
    /// 返回退出语义（[`MirrorExit`]）：主动停止/正常关窗/异常退出（附 stderr 尾行）。
    pub fn wait_exit(&self) -> MirrorExit {
        let mut status = None;
        loop {
            {
                let mut done = false;
                if let Ok(mut g) = self.child.lock() {
                    if let Ok(Some(s)) = g.try_wait() {
                        status = Some(s);
                        done = true;
                    }
                } else {
                    done = true; // 锁损坏：按已退出处理，保证清理执行
                }
                if done {
                    break;
                }
            }
            if crate::utils::is_interrupted() {
                break;
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        self.cleanup();
        if self.stopped.load(std::sync::atomic::Ordering::Relaxed)
            || crate::utils::is_interrupted()
        {
            return MirrorExit::Stopped;
        }
        match status {
            Some(s) if s.success() => MirrorExit::Closed,
            _ => MirrorExit::Failed(self.stderr_tail()),
        }
    }

    /// 停止镜像（置主动停止标志 + 杀子进程；隧道/规则清理由 wait_exit/Drop 路径
    /// 的 cleanup 完成）
    pub fn stop(&self) {
        self.stopped.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Ok(mut g) = self.child.lock() {
            let _ = g.kill();
        }
    }

    /// stderr 尾行拼接（诊断展示用）
    fn stderr_tail(&self) -> String {
        self.stderr_tail
            .lock()
            .map(|g| g.iter().cloned().collect::<Vec<_>>().join("\n"))
            .unwrap_or_default()
    }

    /// 幂等清理：杀子进程（若活着）→ 摘 hop#2 → 清扫残留 forward 规则。
    /// 须在隧道关闭（`shutdown_remote`）之前调用，否则 hop#2 摘除必然失败
    /// （残留规则由下次启动的 sweep 消化）。
    fn cleanup(&self) {
        {
            let mut done = self.cleaned.lock().unwrap_or_else(|e| e.into_inner());
            if *done {
                return;
            }
            *done = true;
        }
        if let Ok(mut g) = self.child.lock() {
            let _ = g.kill();
            let _ = g.wait();
        }
        if let Some(p) = self.hop2_port {
            if let Some(t) = crate::transport::tunnel() {
                let _ = t.remove_forward(p);
            }
        }
        sweep_scrcpy_rules(&self.serial);
    }
}

impl Drop for MirrorHandle {
    fn drop(&mut self) {
        self.cleanup();
    }
}

/// drain 子进程 stderr 到有界尾行缓冲（防管道写满阻塞 scrcpy；留诊断尾部）
fn spawn_stderr_drain(child: &mut Child, tail: Arc<Mutex<VecDeque<String>>>) {
    let Some(err) = child.stderr.take() else { return };
    std::thread::spawn(move || {
        use std::io::{BufRead, BufReader};
        for line in BufReader::new(err).lines() {
            let Ok(line) = line else { break };
            let mut g = tail.lock().unwrap_or_else(|e| e.into_inner());
            if g.len() >= STDERR_TAIL_CAP {
                g.pop_front();
            }
            g.push_back(line);
        }
    });
}

/// 启动一路屏幕镜像：探测 scrcpy → 清扫残留规则 →（远程模式）建固定端口 hop#2
/// → spawn scrcpy（`-s <serial> --no-audio --window-title xperf:<serial>`，
/// 远程追加 `--tunnel-port=P` + `ADB_SERVER_SOCKET` 指向 hop#1）。
///
/// 启动后有 600ms 宽限期：参数错误/连接立败（如端口被抢）在此窗口内暴露为 Err
/// （附带 stderr 尾行）；此后存活即视为成功，后续退出由调用方经
/// [`MirrorHandle::wait_exit`] 感知。
///
/// `serial`：目标设备（`None` 回退全局选择——CLI 经 `select_device` 已写入）。
pub fn start_mirror(serial: Option<&str>) -> Result<MirrorHandle> {
    let bin = find_scrcpy()
        .context("未找到 scrcpy 可执行文件（安装：brew install scrcpy / 发行版包管理器）")?;
    let eff = crate::utils::resolve_serial(serial).context("未选择目标设备（先连接设备）")?;
    sweep_scrcpy_rules(&eff);

    let mut cmd = Command::new(&bin);
    cmd.args(["-s", &eff, "--no-audio", "--window-title"])
        .arg(format!("xperf: {eff}"));

    let mut hop2_port = None;
    if matches!(crate::transport::transport(), crate::transport::Transport::Ssh(_)) {
        let tun = crate::transport::tunnel().context("远程模式但隧道不存在（未连接远程后端？）")?;
        let busy = remote_busy_ports();
        let port = pick_tunnel_port(&busy, local_port_free).context(format!(
            "无可用隧道端口（{TUNNEL_PORT_MIN}..={TUNNEL_PORT_MAX} 全被本机或远端占用）"
        ))?;
        cmd.arg(format!("--tunnel-port={port}"));
        cmd.env(
            "ADB_SERVER_SOCKET",
            format!("tcp:127.0.0.1:{}", tun.server_port()),
        );
        // 先建 hop#2 再放行 scrcpy：其 forward 注册在远端 server，视频流回本机依赖此映射
        tun.add_forward_pinned(port, port)?;
        hop2_port = Some(port);
    }

    let mut child = match cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            if let Some(p) = hop2_port {
                if let Some(t) = crate::transport::tunnel() {
                    let _ = t.remove_forward(p);
                }
            }
            return Err(e).context(format!("scrcpy 启动失败（{}）", bin.display()));
        }
    };
    let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));
    spawn_stderr_drain(&mut child, stderr_tail.clone());
    let handle = MirrorHandle {
        serial: eff,
        child: Arc::new(Mutex::new(child)),
        stderr_tail,
        hop2_port,
        cleaned: Arc::new(Mutex::new(false)),
        stopped: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };

    // 宽限期：scrcpy 的参数错误/隧道连接立败在此暴露（正常启动后窗口期间进程存活）
    std::thread::sleep(Duration::from_millis(600));
    if !handle.is_alive() {
        let tail = handle.stderr_tail();
        handle.cleanup();
        bail!("scrcpy 启动即退出{}{}", if tail.is_empty() { "" } else { "：" }, tail);
    }
    Ok(handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_forward_line() {
        assert_eq!(
            parse_forward_line("6eb792dfb0f tcp:27183 localabstract:scrcpy-1234abcd"),
            Some(("6eb792dfb0f", 27183, "localabstract:scrcpy-1234abcd"))
        );
        assert_eq!(
            parse_forward_line("localhost:5559 tcp:40759 localabstract:xperf-agent"),
            Some(("localhost:5559", 40759, "localabstract:xperf-agent"))
        );
        // 非法行：列数不符 / 非 tcp 端口 / 端口非数字
        assert_eq!(parse_forward_line(""), None);
        assert_eq!(parse_forward_line("serial tcp:1234"), None);
        assert_eq!(parse_forward_line("serial 1234 target"), None);
        assert_eq!(parse_forward_line("serial tcp:abc target"), None);
    }

    #[test]
    fn test_pick_tunnel_port_skips_remote_busy() {
        let busy: HashSet<u16> = (27183..=27185).collect();
        // 远端占 27183-27185 → 应选 27186（本机全空闲）
        assert_eq!(pick_tunnel_port(&busy, |_| true), Some(27186));
        // 全池占用 → None
        let all: HashSet<u16> = (TUNNEL_PORT_MIN..=TUNNEL_PORT_MAX).collect();
        assert_eq!(pick_tunnel_port(&all, |_| true), None);
        // 本机占 27183（local_free 判定）→ 跳过
        let empty = HashSet::new();
        assert_eq!(pick_tunnel_port(&empty, |p| p != 27183), Some(27184));
        // 本机真实判定：池首端口空闲时应命中池首
        assert_eq!(pick_tunnel_port(&empty, local_port_free), Some(27183));
    }
}
