//! scrcpy 屏幕镜像集成：拉起外部 scrcpy 窗口（视频解码/触控注入由 scrcpy 客户端
//! 承担，本模块只负责进程与隧道生命周期）。CLI `--mirror` 与 GUI「屏幕镜像」按钮共用。
//!
//! SSH 远程模式（[`crate::transport::Transport::Ssh`]）：scrcpy 默认的 `adb reverse`
//! 通道在该拓扑下不可用——reverse 规则的路由终点是远端 adb server，视频流回不到
//! 经隧道的 TCP 客户端（2026-09-11 实测：规则注册成功但设备侧连接永不到达）。
//! 故必须 forward 模式，且**注册口与连接口必须同号钉死**（端口 P 经 hop#2
//! `local==remote` 同号映射回远端）：scrcpy 4.x 的 `--tunnel-port` **只钉本地
//! connect 口**，`adb forward` 注册口由 `-p/--port`（port_range，默认 27183:27199）
//! 在 **adb server 侧**扫描决定（v4.1 源码 `adb_tunnel.c::enable_tunnel_forward_any_port`
//! 扫 port_range，`server.c` connect 用 tunnel_port）——只传 `--tunnel-port` 时
//! 注册口由远端扫描自选，与其他镜像并存时扫描结果漂移，连接口与注册口错配必败
//! （"Server connection failed"，2026-09-11 用户实撞：SS3 镜像在跑时 SS4 起不来）。
//! 因此传 `-p P --tunnel-port=P`（注册/连接双钉同号；`-p` 单口即 range {P,P}，
//! 老版本 scrcpy 的 --tunnel-port 语义下同义，向后兼容）。

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

/// 本机端口可用性判定（bind 即放；TOCTOU 窗口由 `-O forward` 的
/// `ExitOnForwardFailure` 快速失败兜底）
fn local_port_free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// 组装 scrcpy 命令行参数（纯函数，单测锁定双钉语义防漂移）：
/// 本地模式 `tunnel_port=None`；远程模式 `Some(P)` → `-p P --tunnel-port=P`
/// 注册口/连接口双钉同号（语义详见模块文档）
fn build_scrcpy_args(serial: &str, tunnel_port: Option<u16>) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-s".into(),
        serial.into(),
        "--no-audio".into(),
        "--window-title".into(),
        format!("xperf: {serial}"),
    ];
    if let Some(p) = tunnel_port {
        args.push(format!("--tunnel-port={p}"));
        args.push("-p".into());
        args.push(p.to_string());
    }
    args
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
        if let Some((s, port, target)) = parse_forward_line(line) {
            if s == serial && target.starts_with("localabstract:scrcpy") {
                let _ = crate::utils::adb_for(Some(serial))
                    .args(["forward", "--remove", &format!("tcp:{port}")])
                    .output();
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
/// 远程追加 `-p P --tunnel-port=P` 双钉同号 + `ADB_SERVER_SOCKET` 指向 hop#1）。
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

    let mut hop2_port = None;
    let mut cmd = Command::new(&bin);
    if matches!(crate::transport::transport(), crate::transport::Transport::Ssh(_)) {
        let tun = crate::transport::tunnel().context("远程模式但隧道不存在（未连接远程后端？）")?;
        // 端口获取循环：远端规则占用（busy）/本机占用（bind 探测）/映射表已占
        // （并发的另一路镜像）/-O forward 失败（TOCTOU 被抢）——均换下一候选端口
        let busy = remote_busy_ports();
        let mut pinned = None;
        for port in TUNNEL_PORT_MIN..=TUNNEL_PORT_MAX {
            if busy.contains(&port) || !local_port_free(port) {
                continue;
            }
            match tun.add_forward_pinned(port, port) {
                Ok(true) => {
                    pinned = Some(port);
                    break;
                }
                Ok(false) | Err(_) => continue,
            }
        }
        let port = pinned.context(format!(
            "无可用隧道端口（{TUNNEL_PORT_MIN}..={TUNNEL_PORT_MAX} 全被本机/远端/并发镜像占用）"
        ))?;
        cmd.args(build_scrcpy_args(&eff, Some(port)));
        cmd.env(
            "ADB_SERVER_SOCKET",
            format!("tcp:127.0.0.1:{}", tun.server_port()),
        );
        hop2_port = Some(port);
    } else {
        cmd.args(build_scrcpy_args(&eff, None));
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

    /// 测试用手柄构造（包一层假子进程；hop2/sweep 在测试环境为空操作——
    /// serial 不存在于任何 forward 规则）
    fn handle_for(child: Child) -> MirrorHandle {
        MirrorHandle {
            serial: "test-nonexistent-serial".into(),
            child: Arc::new(Mutex::new(child)),
            stderr_tail: Arc::new(Mutex::new(VecDeque::new())),
            hop2_port: None,
            cleaned: Arc::new(Mutex::new(false)),
            stopped: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    #[test]
    fn test_wait_exit_stopped() {
        // stop() 主动停止 → Stopped 且子进程确死（GUI 停止按钮/关窗收尾路径）
        let child = Command::new("sleep").arg("30").spawn().unwrap();
        let h = handle_for(child);
        assert!(h.is_alive());
        h.stop();
        assert!(matches!(h.wait_exit(), MirrorExit::Stopped));
        assert!(!h.is_alive());
    }

    #[test]
    fn test_wait_exit_closed() {
        // 子进程 exit 0（用户关窗路径）→ Closed
        let child = Command::new("true").spawn().unwrap();
        let h = handle_for(child);
        assert!(matches!(h.wait_exit(), MirrorExit::Closed));
    }

    #[test]
    fn test_wait_exit_failed() {
        // 子进程非零退出（连接失败等）→ Failed（带 stderr 尾行）
        let mut child = Command::new("sh")
            .args(["-c", "echo some-error 1>&2; exit 1"])
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let tail = Arc::new(Mutex::new(VecDeque::new()));
        spawn_stderr_drain(&mut child, tail.clone());
        // 字面量构造（MirrorHandle 有 Drop，不可用结构更新语法移动字段）
        let h = MirrorHandle {
            serial: "test-nonexistent-serial".into(),
            child: Arc::new(Mutex::new(child)),
            stderr_tail: tail,
            hop2_port: None,
            cleaned: Arc::new(Mutex::new(false)),
            stopped: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        match h.wait_exit() {
            MirrorExit::Failed(t) => assert!(t.contains("some-error"), "尾行应含 stderr: {t}"),
            other => panic!("应为 Failed，实得 {:?}", other),
        }
    }

    #[test]
    fn test_build_scrcpy_args() {
        // 本地模式：无端口参数
        let a = build_scrcpy_args("6eb792dfb0f", None);
        assert_eq!(
            a,
            vec!["-s", "6eb792dfb0f", "--no-audio", "--window-title", "xperf: 6eb792dfb0f"]
        );
        // 远程模式：-p P --tunnel-port=P 双钉同号（注册口/连接口错配必败的回归锚）
        let a = build_scrcpy_args("localhost:5559", Some(27184));
        assert_eq!(
            a,
            vec![
                "-s", "localhost:5559", "--no-audio", "--window-title", "xperf: localhost:5559",
                "--tunnel-port=27184", "-p", "27184",
            ]
        );
    }

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
    fn test_port_pool_bounds_and_local_free() {
        // 池界与 scrcpy 默认候选范围一致
        assert_eq!((TUNNEL_PORT_MIN, TUNNEL_PORT_MAX), (27183, 27199));
        // 本机判定：被占端口应报忙（只测忙向——释放后报闲在并行测试下有抢端口 TOCTOU）
        let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let p = l.local_addr().unwrap().port();
        assert!(!local_port_free(p), "被占端口应报忙");
    }
}
