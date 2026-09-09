//! adb 传输后端：本机 adb server 或经 SSH 隧道的远端 adb server。
//!
//! 设计约束：本机恒为 adb **客户端**（经 `ADB_SERVER_SOCKET` 指向远端 server），
//! 故 `pull`/`push` 落点、分析工具链（trace_processor/report_html.py）、桌面打开
//! 全部留在本机零改动；只有 `adb forward` 的监听端口在远端 server 侧，
//! agent NDJSON 流需第二跳隧道映射回本机（见 `SshTunnel`，S2 引入）。
//!
//! 完整设计与实测依据见 `docs/DESIGN-ssh-remote.md`。

// ---------- SSH 隧道（hop#1：本机端口 → 远端 adb server）----------

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{bail, Context, Result};

/// UNIX socket 路径长度上限（`sockaddr_un.sun_path`：macOS 104 / Linux 108），留余量取 100
const MAX_CONTROL_PATH_LEN: usize = 100;

/// 一条 SSH ControlMaster 连接，承载 hop#1（adb server）与 N 条 hop#2（agent 通道）。
///
/// 生命周期由本工具全权管理：Drop 时 `-O exit` 关隧道；进程被 SIGKILL 时
/// control socket 文件名带 pid，下次 [`SshTunnel::establish`] 清理无主残留（R9）。
pub struct SshTunnel {
    /// ssh 目标（Host 别名或 `user@host`），`-O` 管理命令的必备参数
    host: String,
    /// ControlMaster socket 路径（`~/.ssh/cm/xperf-<host>-<pid>`，超长退化）
    control_path: PathBuf,
    /// hop#1 本机端口（→ 远端 `127.0.0.1:<remote_port>` adb server）
    server_port: u16,
    /// hop#2 映射表：远端 forward 端口 → 本机端口。复用防泄漏
    /// （`adb forward tcp:0` 每次调用新建规则，附录 A #32；同 remote_port 只建一条）
    forwards: Mutex<HashMap<u16, u16>>,
}

impl SshTunnel {
    /// 建立隧道（hop#1）。
    ///
    /// 流程：① `ssh <host> '<adb_path> start-server'` 确保远端 server 在跑
    /// （**绝不 `kill-server`**——远端 server 可能被他人共用，R2）；
    /// ② 清理无主 control socket（R9）；③ 自选本机空闲端口 + 建 master
    /// （`ExitOnForwardFailure=yes` 使端口占用立即失败，TOCTOU 换端口重试 3 次，R4）；
    /// ④ 探活：对 hop#1 端口做 adb `host:version` 握手（验证隧道+远端 server 全链路）。
    pub fn establish(target: &SshTarget) -> Result<Self> {
        // ① 远端 server 就绪。远端命令经登录 shell 执行，adb_path 的 `~` 可展开。
        let start = format!("{} start-server", target.adb_path);
        let out = Command::new("ssh")
            .args(ssh_base_opts())
            .arg(&target.host)
            .arg(&start)
            .output()
            .context("执行 ssh start-server 失败（ssh 不可用？需免密登录）")?;
        if !out.status.success() {
            bail!(
                "远端 adb start-server 失败（{}）：{}——确认 --remote-adb 路径正确（远端 PATH 常不含 adb）",
                target.host,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }

        // ② R9：清理上次异常退出（SIGKILL）残留的无主 control socket
        cleanup_stale_control_sockets(&target.host);
        let control_path = control_socket_path(&target.host);
        if let Some(dir) = control_path.parent() {
            std::fs::create_dir_all(dir).ok();
        }

        // ③ 自选端口建 master，失败（ExitOnForwardFailure）换端口重试
        let mut last_err = anyhow::anyhow!("端口自选失败");
        for _ in 0..3 {
            let port = pick_free_port()?;
            let ok = Command::new("ssh")
                .args(master_ssh_args(&control_path, port, target))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !ok {
                last_err = anyhow::anyhow!("ssh master 建立失败（端口 {port} 或网络问题）");
                continue;
            }
            // ④ 探活：adb host:version 握手（~2s 内重试，隧道刚起可能未就绪）
            let mut probe_err = anyhow::anyhow!("探活未执行");
            for _ in 0..10 {
                match probe_adb_server(port) {
                    Ok(_) => {
                        return Ok(Self {
                            host: target.host.clone(),
                            control_path,
                            server_port: port,
                            forwards: Mutex::new(HashMap::new()),
                        });
                    }
                    Err(e) => {
                        probe_err = e;
                        std::thread::sleep(Duration::from_millis(200));
                    }
                }
            }
            last_err = probe_err.context("hop#1 探活失败（adb host:version 无应答）");
            let _ = kill_master(&control_path, &target.host); // 探活失败的 master 不留
        }
        Err(last_err.context("建立 SSH 隧道失败（已重试 3 次）"))
    }

    /// hop#1 本机端口（写 `ADB_SERVER_SOCKET` 用）
    pub fn server_port(&self) -> u16 {
        self.server_port
    }

    /// 隧道存活性：`ssh -S <ctl> -O check <host>`（R3 的定期探活入口）
    pub fn is_alive(&self) -> bool {
        self.control_path.exists()
            && Command::new("ssh")
                .arg("-S")
                .arg(&self.control_path)
                .arg("-O")
                .arg("check")
                .arg(&self.host)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
    }

    /// 为远端 forward 端口新增 hop#2，返回**本机**端口；已映射则复用（防泄漏，R10）。
    ///
    /// `ssh -S <ctl> -O forward -L <P_loc>:127.0.0.1:<remote> <host>`（不新起 ssh 进程，
    /// 单连接多 channel 复用，附录 A #31）。映射表锁全程持有（含 ssh 调用，~100ms），
    /// 防多线程并发为同一 `remote_port` 建重复转发。
    pub fn add_forward(&self, remote_port: u16) -> Result<u16> {
        let mut map = self.lock_forwards();
        if let Some(&local) = map.get(&remote_port) {
            return Ok(local);
        }
        let mut last_err = anyhow::anyhow!("端口自选失败");
        for _ in 0..3 {
            let local = pick_free_port()?;
            if self.ssh_control("forward", local, remote_port) {
                map.insert(remote_port, local);
                return Ok(local);
            }
            last_err = anyhow::anyhow!("-O forward 失败（本机端口 {local} → 远端 {remote_port}）");
        }
        Err(last_err.context("建立 hop#2 失败（已重试 3 次）"))
    }

    /// 移除 hop#2（会话结束）。`ssh -S <ctl> -O cancel -L <与建立时完全相同的 spec>`
    pub fn remove_forward(&self, remote_port: u16) -> Result<()> {
        let mut map = self.lock_forwards();
        let Some(local) = map.remove(&remote_port) else {
            return Ok(()); // 未建立过，幂等
        };
        // cancel 失败（隧道已断等）不算错误：映射已移除，残留由隧道关闭兜底
        let _ = self.ssh_control("cancel", local, remote_port);
        Ok(())
    }

    /// `-O forward/cancel` 控制操作（返回是否成功；spec 与建立时逐字节一致才可 cancel）。
    /// `ExitOnForwardFailure=yes` 使 `-O forward` 端口占用时返回非零（R4 重试的前提）。
    fn ssh_control(&self, op: &str, local: u16, remote: u16) -> bool {
        Command::new("ssh")
            .arg("-S")
            .arg(&self.control_path)
            .arg("-O")
            .arg(op)
            .arg("-o")
            .arg("ExitOnForwardFailure=yes")
            .arg("-L")
            .arg(fwd_spec(local, remote))
            .arg(&self.host)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn lock_forwards(&self) -> MutexGuard<'_, HashMap<u16, u16>> {
        self.forwards.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
impl SshTunnel {
    /// 构造仅含端口信息的伪隧道（注入测试用；control_path 指向不存在路径，
    /// 任何 `-O` 操作都会失败，不能用于真实转发）
    pub(crate) fn for_test(server_port: u16) -> Self {
        Self {
            host: "test".into(),
            control_path: PathBuf::from("/nonexistent/xperf-test"),
            server_port,
            forwards: Mutex::new(HashMap::new()),
        }
    }
}

/// 当前远程隧道（仅 [`Transport::Ssh`] 模式存在；由 `init_remote`/`shutdown_remote`
/// 安装与清理，S4）
static TUNNEL: Mutex<Option<std::sync::Arc<SshTunnel>>> = Mutex::new(None);

/// 当前隧道（远程模式下 agent 通道建 hop#2 用；非远程模式或无隧道为 `None`）
pub fn tunnel() -> Option<std::sync::Arc<SshTunnel>> {
    TUNNEL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// hop#1 本机端口（adb 命令注入 `ADB_SERVER_SOCKET` 用；无隧道为 `None`）
pub fn tunnel_server_port() -> Option<u16> {
    tunnel().map(|t| t.server_port())
}

/// 当前隧道是否存活（`-O check`；非远程模式/无隧道返回 `false`）——
/// 供热插拔监视器在隧道死时短路（adb 全灭，枚举无意义）
pub fn tunnel_alive() -> bool {
    tunnel().map(|t| t.is_alive()).unwrap_or(false)
}

/// 安装当前隧道（`init_remote` 建隧成功后调用）
pub(crate) fn install_tunnel(t: SshTunnel) {
    *TUNNEL.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::sync::Arc::new(t));
}

/// 摘除当前隧道（`shutdown_remote` 调用；摘除后 Drop 触发 `-O exit` 关隧道）
pub(crate) fn clear_tunnel() {
    let _ = TUNNEL.lock().unwrap_or_else(|e| e.into_inner()).take();
}

// ---------- 远程后端生命周期（S4）----------

use crate::utils::AdbDevice;

/// 初始化远程后端：建 hop#1 → 校验 adb 协议版本（R2）→ 返回远端设备列表。
///
/// 顺序是硬约束：本函数必须早于**任何** adb 调用。成功后 [`transport`] 切到
/// `Ssh`，全部 adb 命令经 hop#1 指向远端 server。
/// 任一步失败：隧道回收、传输保持 `Local`，不产生残留。
pub fn init_remote(target: SshTarget) -> Result<Vec<AdbDevice>> {
    // R2 协议版本校验先于一切 adb 客户端调用：版本不符时 adb 客户端会
    // kill 远端 server（可能正被他人共用），故先用两侧 CLI banner 比对。
    check_protocol_version(&target)?;

    let t = SshTunnel::establish(&target)?;
    install_tunnel(t);
    set_transport(Transport::Ssh(target.clone()));

    match crate::utils::list_adb_devices() {
        Ok(devices) => Ok(devices),
        Err(e) => {
            // 设备枚举失败：完整回滚，不留半截状态
            clear_tunnel();
            set_transport(Transport::Local);
            Err(e.context("远程 adb server 设备枚举失败"))
        }
    }
}

/// 关闭远程后端：逐条清理本工具注册的 forward 规则（R10，**禁用
/// `--remove-all`**——全局生效会踢掉他人规则）→ 关隧道 → 回落 `Local`。
/// 幂等：非远程模式调用为空操作。隧道已断时清理失败仅忽略（规则残留由
/// 远端 server 持有，无法触及，下次 init 的 ensure_forward 复用逻辑消化）。
pub fn shutdown_remote() {
    let Some(t) = tunnel() else {
        set_transport(Transport::Local);
        return;
    };
    // 本工具注册的远端 forward 规则 = hop#2 映射表的键集合
    let remote_ports: Vec<u16> = t.lock_forwards().keys().copied().collect();
    for p in remote_ports {
        let _ = crate::utils::run_adb(&["forward", "--remove", &format!("tcp:{p}")]);
    }
    clear_tunnel(); // take → Drop → -O exit（带走 hop#1 与全部 hop#2）
    set_transport(Transport::Local);
}

/// 重建 SSH 隧道（断线重连，S9）：旧隧道丢弃（Drop `-O exit` 尽力清理）→
/// 同一 target 重新 establish + 安装。hop#2 映射表随新隧道清空（旧映射指向
/// 死端口，必须废弃）；远端 forward 规则由 server 持有、跨隧道存活，
/// `ensure_forward` 查 list 复用不重复创建（R10）。版本已在 init 校验过，不重复。
/// 非远程模式调用为空操作。
pub fn rebuild_tunnel() -> Result<()> {
    let Transport::Ssh(target) = transport() else { return Ok(()) };
    clear_tunnel();
    let t = SshTunnel::establish(&target)?;
    install_tunnel(t);
    Ok(())
}

/// R2：adb 协议版本校验（判据是**协议**版本而非 platform-tools 版本，
/// 附录 A #30：36.0.2 连 36.0.0 安全，因协议同为 1.0.41）。
/// 比对本机 `adb version` 与远端 `<adb_path> version` 的 banner 协议串；
/// 不一致**报错中止**（绝不主动 kill-server——远端 server 可能被共用）。
fn check_protocol_version(target: &SshTarget) -> Result<()> {
    let local_out = crate::utils::run_command("adb", &["version"])
        .context("本机 adb version 执行失败")?;
    let remote_cmd = format!("{} version", target.adb_path);
    let remote_out = Command::new("ssh")
        .args(ssh_base_opts())
        .arg(&target.host)
        .arg(&remote_cmd)
        .output()
        .context("ssh 远端 adb version 执行失败")?;
    if !remote_out.status.success() {
        bail!(
            "远端 adb version 失败：{}——确认 --remote-adb 路径正确",
            String::from_utf8_lossy(&remote_out.stderr).trim()
        );
    }
    let remote_stdout = String::from_utf8_lossy(&remote_out.stdout);
    let local_v = parse_adb_version(&local_out.stdout)
        .context("本机 adb version 输出无法解析")?;
    let remote_v = parse_adb_version(&remote_stdout)
        .context("远端 adb version 输出无法解析")?;
    if local_v != remote_v {
        bail!(
            "adb 协议版本不一致（本机 {local_v} vs 远端 {remote_v}）：继续会导致 adb 客户端 \
             kill 远端 server（影响他人调试）。请对齐两端 platform-tools 后重试"
        );
    }
    Ok(())
}

/// 从 `adb version` 输出解析协议版本串（`Android Debug Bridge version 1.0.41` → `1.0.41`）
fn parse_adb_version(output: &str) -> Option<&str> {
    output
        .lines()
        .find_map(|l| l.strip_prefix("Android Debug Bridge version "))
        .map(|v| v.split_whitespace().next().unwrap_or(v))
        .filter(|v| !v.is_empty())
}

impl Drop for SshTunnel {
    /// 关隧道：`-O exit` 带走 hop#1 与全部 hop#2（实测可靠，附录 A #31）
    fn drop(&mut self) {
        let _ = kill_master(&self.control_path, &self.host);
        let _ = std::fs::remove_file(&self.control_path);
    }
}

/// `ssh -S <ctl> -O exit <host>` 关 master（不存在/已死均忽略错误）
fn kill_master(control_path: &std::path::Path, host: &str) -> Result<()> {
    Command::new("ssh")
        .arg("-S")
        .arg(control_path)
        .arg("-O")
        .arg("exit")
        .arg(host)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    Ok(())
}

/// 交互类 ssh 调用的公共参数：禁交互询问（防密码 prompt 挂起）+ 连接超时快速失败
fn ssh_base_opts() -> Vec<&'static str> {
    vec!["-o", "BatchMode=yes", "-o", "ConnectTimeout=10"]
}

/// master 建链参数（每条均有实测依据，见设计 §4.2）
fn master_ssh_args(control_path: &std::path::Path, local_port: u16, target: &SshTarget) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-M".into(), // ControlMaster 模式
        "-S".into(),
        control_path.display().to_string(),
        "-f".into(), // 认证后转后台
        "-N".into(), // 不执行远程命令（纯转发）
    ];
    args.extend(ssh_base_opts().into_iter().map(Into::into));
    args.extend([
        // 网络黑洞 15s×3≈45s 内主动断连（R3）
        "-o".into(), "ServerAliveInterval=15".into(),
        "-o".into(), "ServerAliveCountMax=3".into(),
        // 端口占用立即失败而非静默降级（R4 重试的前提）
        "-o".into(), "ExitOnForwardFailure=yes".into(),
        // 实测 47MB trace 拉取 11.17s → 2.68s（4.2×，附录 A #16）
        "-o".into(), "Compression=yes".into(),
        // master 寿命由本工具管理（Drop -O exit），不留交互式重连后门
        "-o".into(), "ControlPersist=no".into(),
        "-L".into(),
        format!("{}:127.0.0.1:{}", local_port, target.remote_port),
        target.host.clone(),
    ]);
    args
}

/// 本机空闲端口自选（`TcpListener::bind(:0)` 取端口后 drop；
/// 存在极小 TOCTOU 窗口 ⇒ 调用方失败换端口重试，R4）
fn pick_free_port() -> Result<u16> {
    let l = TcpListener::bind(("127.0.0.1", 0)).context("本机空闲端口自选失败")?;
    Ok(l.local_addr()?.port())
}

/// 端口转发 spec（`-L` 参数值）：建立与 cancel 必须逐字节一致
fn fwd_spec(local_port: u16, remote_port: u16) -> String {
    format!("{local_port}:127.0.0.1:{remote_port}")
}

/// 探活 adb server：原始 adb 协议 `host:version` 握手，返回版本 payload。
///
/// 协议：发 `000c`（长度 12 的十六进制）+ `host:version`；收 `OKAY` +
/// 4 位十六进制长度 + payload（两端同一问法 ⇒ payload 字符串可直接比对，S4 用）。
fn probe_adb_server(port: u16) -> Result<String> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).context("连接 adb server 失败")?;
    s.set_read_timeout(Some(Duration::from_secs(5))).ok();
    s.set_write_timeout(Some(Duration::from_secs(5))).ok();
    s.write_all(b"000chost:version")?;
    let mut hdr = [0u8; 4];
    s.read_exact(&mut hdr)?;
    if &hdr != b"OKAY" {
        bail!("adb server 应答非 OKAY：{:?}", hdr);
    }
    let mut lenb = [0u8; 4];
    s.read_exact(&mut lenb)?;
    let len = u32::from_str_radix(std::str::from_utf8(&lenb)?, 16)?;
    let mut payload = vec![0u8; len as usize];
    s.read_exact(&mut payload)?;
    Ok(String::from_utf8_lossy(&payload).to_string())
}

/// host 净化为文件名片段：保留 `[A-Za-z0-9._@-]`，其余转 `-`
fn sanitize_host(host: &str) -> String {
    host.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '@' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// ControlMaster socket 路径：`~/.ssh/cm/xperf-<host>-<pid>-<n>`（pid 防跨实例互踩、
/// n 防同进程多隧道互踩（并行测试等场景），R9）；超 UNIX 路径上限退化
/// `temp_dir()` → `/tmp`（极端长 host 截断）。
fn control_socket_path(host: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static TUNNEL_SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = TUNNEL_SEQ.fetch_add(1, Ordering::Relaxed);
    let fname = |h: &str| format!("xperf-{}-{}-{}", sanitize_host(h), std::process::id(), seq);
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join(".ssh").join("cm").join(fname(host)));
    }
    candidates.push(std::env::temp_dir().join(fname(host)));
    // 极端长 host 兜底：/tmp 下截断 host 段
    let max_host = 40usize.min(host.len());
    candidates.push(PathBuf::from(format!("/tmp/{}", fname(&host[..max_host]))));
    candidates
        .into_iter()
        .find(|p| p.as_os_str().len() <= MAX_CONTROL_PATH_LEN)
        .unwrap_or_else(|| PathBuf::from(format!("/tmp/xperf-{}-{}", std::process::id(), seq)))
}

/// 从残留 socket 文件名解析宿主 pid（`xperf-<host>-<pid>-<n>` → pid）
fn parse_stale_pid(filename: &str, host: &str) -> Option<u32> {
    filename
        .strip_prefix(&format!("xperf-{}-", sanitize_host(host)))?
        .split('-')
        .next()?
        .parse()
        .ok()
}

/// R9：清理无主 control socket——文件名 pid 已死的残留，
/// 先 `-O exit` 杀可能存活的 master 进程，再删 socket 文件
fn cleanup_stale_control_sockets(host: &str) {
    let Some(home) = std::env::var_os("HOME") else { return };
    let dir = PathBuf::from(home).join(".ssh").join("cm");
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(pid) = parse_stale_pid(&name, host) else { continue };
        if pid == std::process::id() || pid_alive(pid) {
            continue; // 自己或他存活实例的 socket，不动
        }
        let _ = kill_master(&entry.path(), host);
        let _ = std::fs::remove_file(entry.path());
    }
}

/// pid 存活探测（`kill -0`；仅用于本机进程，权限拒绝视同死亡——socket 在 `~/.ssh/cm`
/// 按用户隔离，同机他用户实例不可见）
fn pid_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// adb 服务端位置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    /// 本机 adb server（默认；行为与改造前逐字节一致）
    Local,
    /// 远端 adb server，经 SSH 本地端口转发抵达
    Ssh(SshTarget),
}

/// SSH 远端描述。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshTarget {
    /// ssh 目标：`ssh_config` 中的 Host 别名（如 `hppc`）或 `user@host`。
    /// 用别名可复用用户既有的免密/跳板/端口配置。
    pub host: String,
    /// 远端 adb 可执行路径。**不能假设在 PATH 中**（实测 hppc 即不在），
    /// 默认 `"adb"`。
    pub adb_path: String,
    /// 远端 adb server 监听端口，默认 [`SshTarget::DEFAULT_ADB_PORT`]。
    pub remote_port: u16,
}

impl SshTarget {
    /// 远端 adb server 默认端口（5037）
    pub const DEFAULT_ADB_PORT: u16 = 5037;

    /// 构造远端描述：adb 路径默认 `"adb"`，端口默认 5037。
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            adb_path: "adb".to_string(),
            remote_port: Self::DEFAULT_ADB_PORT,
        }
    }

    /// 指定远端 adb 可执行路径（远端 PATH 常不含 adb 时使用）
    pub fn with_adb_path(mut self, path: impl Into<String>) -> Self {
        self.adb_path = path.into();
        self
    }

    /// 指定远端 adb server 监听端口
    pub fn with_remote_port(mut self, port: u16) -> Self {
        self.remote_port = port;
        self
    }
}

/// 当前传输后端（进程级全局，与既有 `utils::TARGET_SERIAL` 同款模式）。
static TRANSPORT: Mutex<Transport> = Mutex::new(Transport::Local);

/// 传输全局状态（TRANSPORT/TUNNEL）测试串行化锁：所有读写这两个全局的测试
/// 必须先持有它，防同进程并行测试互踩
#[cfg(test)]
pub(crate) static TRANSPORT_TEST_LOCK: Mutex<()> = Mutex::new(());

/// 当前传输后端（默认 [`Transport::Local`]）
pub fn transport() -> Transport {
    TRANSPORT
        .lock()
        .map(|g| g.clone())
        .unwrap_or(Transport::Local)
}

/// 设置传输后端。须在任何 adb 调用之前完成
/// （远程模式：`init_remote` 建隧道成功后才设置为 `Ssh`，见 S4）。
pub fn set_transport(t: Transport) {
    if let Ok(mut g) = TRANSPORT.lock() {
        *g = t;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ssh_target_defaults() {
        let t = SshTarget::new("hppc");
        assert_eq!(t.host, "hppc");
        assert_eq!(t.adb_path, "adb");
        assert_eq!(t.remote_port, 5037);
    }

    #[test]
    fn test_ssh_target_builders() {
        let t = SshTarget::new("user@192.168.1.10")
            .with_adb_path("~/Android/Sdk/platform-tools/adb")
            .with_remote_port(5038);
        assert_eq!(t.host, "user@192.168.1.10");
        assert_eq!(t.adb_path, "~/Android/Sdk/platform-tools/adb");
        assert_eq!(t.remote_port, 5038);
    }

    // 全局 TRANSPORT 的读写集中在单个测试函数内，避免同进程并行测试互踩
    #[test]
    fn test_transport_global_roundtrip() {
        let _serial = TRANSPORT_TEST_LOCK.lock().unwrap();
        assert_eq!(transport(), Transport::Local); // 默认本机
        let target = SshTarget::new("hppc");
        set_transport(Transport::Ssh(target.clone()));
        assert_eq!(transport(), Transport::Ssh(target));
        set_transport(Transport::Local); // 复位，不影响其他测试
        assert_eq!(transport(), Transport::Local);
    }

    // ---- S2：SshTunnel 纯函数 ----

    #[test]
    fn test_sanitize_host() {
        assert_eq!(sanitize_host("hppc"), "hppc");
        assert_eq!(sanitize_host("user@192.168.1.10"), "user@192.168.1.10");
        assert_eq!(sanitize_host("user@host:22/x"), "user@host-22-x");
    }

    #[test]
    fn test_control_socket_path_within_unix_limit() {
        let p = control_socket_path("hppc");
        assert!(p.as_os_str().len() <= MAX_CONTROL_PATH_LEN);
        let fname = p.file_name().unwrap().to_string_lossy();
        assert!(fname.starts_with("xperf-hppc-"));
        assert!(fname.contains(&format!("-{}-", std::process::id())));
        // 同进程多次调用生成不同路径（并行测试/多隧道不互踩）
        assert_ne!(control_socket_path("hppc"), control_socket_path("hppc"));
        // 极端长 host 也必须在上限内（退化路径）
        let long = "a".repeat(200);
        let p = control_socket_path(&long);
        assert!(p.as_os_str().len() <= MAX_CONTROL_PATH_LEN);
    }

    #[test]
    fn test_parse_stale_pid() {
        assert_eq!(parse_stale_pid("xperf-hppc-12345-0", "hppc"), Some(12345));
        assert_eq!(parse_stale_pid("xperf-hppc-12345", "hppc"), Some(12345)); // 兼容无序号
        assert_eq!(parse_stale_pid("xperf-hppc-abc-0", "hppc"), None);
        assert_eq!(parse_stale_pid("xperf-other-12345-0", "hppc"), None); // 别的 host 不匹配
        assert_eq!(parse_stale_pid("unrelated", "hppc"), None);
    }

    #[test]
    fn test_master_ssh_args() {
        let target = SshTarget::new("hppc").with_remote_port(5037);
        let args = master_ssh_args(std::path::Path::new("/tmp/ctl"), 51234, &target);
        let s = args.join(" ");
        // 实测依据参数（设计 §4.2）：ControlMaster / 后台 / 压缩 / 快速失败 / 保活 / hop#1 转发
        for needle in [
            "-M", "-S /tmp/ctl", "-f", "-N",
            "BatchMode=yes", "ConnectTimeout=10",
            "ServerAliveInterval=15", "ServerAliveCountMax=3",
            "ExitOnForwardFailure=yes", "Compression=yes",
            "-L 51234:127.0.0.1:5037",
        ] {
            assert!(s.contains(needle), "缺少参数 {needle}：{s}");
        }
        assert_eq!(args.last().unwrap(), "hppc");
        // 安全红线：绝不携带 kill-server（R2）
        assert!(!s.contains("kill-server"));
    }

    #[test]
    fn test_pick_free_port() {
        let p1 = pick_free_port().unwrap();
        let p2 = pick_free_port().unwrap();
        assert!(p1 > 0 && p2 > 0);
        assert_ne!(p1, p2);
        // 取出后可重新绑定（TOCTOU 窗口内无占用者时）
        TcpListener::bind(("127.0.0.1", p1)).unwrap();
    }

    #[test]
    fn test_fwd_spec() {
        assert_eq!(fwd_spec(51234, 5037), "51234:127.0.0.1:5037");
    }

    // ---- 集成测试（需 hppc SSH 可达 + 远端 adb，标 #[ignore]，手动跑） ----

    /// 隧道全生命周期：establish → is_alive → hop#1 探活 → Drop 后端口拒绝连接
    #[test]
    #[ignore = "需要 hppc：SSH 免密可达 + 远端 adb（~/Android/Sdk/platform-tools/adb）"]
    fn test_tunnel_lifecycle_hppc() {
        let target = SshTarget::new("hppc").with_adb_path("~/Android/Sdk/platform-tools/adb");
        let tun = SshTunnel::establish(&target).expect("establish 失败");
        assert!(tun.is_alive(), "-O check 应通过");
        let v = probe_adb_server(tun.server_port()).expect("hop#1 探活失败");
        assert!(!v.is_empty(), "adb host:version payload 不应为空");
        let port = tun.server_port();
        drop(tun);
        assert!(
            TcpStream::connect(("127.0.0.1", port)).is_err(),
            "Drop 后端口 {port} 应拒绝连接"
        );
    }

    /// hop#2 全生命周期：add_forward（以远端 adb server 5037 为假想 forward 目标，
    /// 免设备依赖）→ 复用同端口 → remove_forward 后拒绝连接
    #[test]
    #[ignore = "需要 hppc：SSH 免密可达 + 远端 adb（~/Android/Sdk/platform-tools/adb）"]
    fn test_hop2_forward_lifecycle_hppc() {
        let target = SshTarget::new("hppc").with_adb_path("~/Android/Sdk/platform-tools/adb");
        let tun = SshTunnel::establish(&target).expect("establish 失败");
        // add：hop#2 把远端 5037（adb server）映射回本机，握手应成功
        let local = tun.add_forward(SshTarget::DEFAULT_ADB_PORT).expect("add_forward 失败");
        let v = probe_adb_server(local).expect("hop#2 探活失败");
        assert!(!v.is_empty());
        // 复用：同 remote_port 返回同一本机端口，不新建
        assert_eq!(tun.add_forward(SshTarget::DEFAULT_ADB_PORT).unwrap(), local);
        // remove：cancel 后本机端口拒绝连接
        tun.remove_forward(SshTarget::DEFAULT_ADB_PORT).unwrap();
        assert!(
            TcpStream::connect(("127.0.0.1", local)).is_err(),
            "cancel 后端口 {local} 应拒绝连接"
        );
        // remove 幂等
        tun.remove_forward(SshTarget::DEFAULT_ADB_PORT).unwrap();
    }

    // ---- S4：init_remote / shutdown_remote ----

    #[test]
    fn test_parse_adb_version() {
        let out = "Android Debug Bridge version 1.0.41\nVersion 36.0.2-12345678\nInstalled as /usr/local/bin/adb\n";
        assert_eq!(parse_adb_version(out), Some("1.0.41"));
        assert_eq!(parse_adb_version("Android Debug Bridge version 1.0.39\n"), Some("1.0.39"));
        assert_eq!(parse_adb_version("garbage\n"), None);
        assert_eq!(parse_adb_version(""), None);
    }

    /// 远程后端全生命周期：init → transport 切 Ssh → adb 命令指向远端 →
    /// shutdown → 回落 Local + 隧道摘除（幂等二次调用不炸）
    #[test]
    #[ignore = "需要 hppc：SSH 免密可达 + 远端 adb（~/Android/Sdk/platform-tools/adb）"]
    fn test_init_shutdown_remote_hppc() {
        let _serial = TRANSPORT_TEST_LOCK.lock().unwrap();
        let target = SshTarget::new("hppc").with_adb_path("~/Android/Sdk/platform-tools/adb");
        let devices = init_remote(target.clone()).expect("init_remote 失败");
        assert_eq!(transport(), Transport::Ssh(target));
        assert!(tunnel().is_some());
        // adb 命令已指向远端 server（设备枚举即经 hop#1 完成，与返回值自洽）
        let out = crate::utils::run_adb(&["devices", "-l"]).unwrap();
        let parsed = crate::utils::parse_adb_devices(&out.stdout);
        assert_eq!(parsed.len(), devices.len(), "init 与 run_adb 枚举应一致");
        shutdown_remote();
        assert_eq!(transport(), Transport::Local);
        assert!(tunnel().is_none());
        shutdown_remote(); // 幂等
        assert_eq!(transport(), Transport::Local);
    }
}
