//! 设备端采样器（xperf-agent）的主机侧传输层——统一的采样引擎。
//!
//! 采样循环在设备上常驻（直接读 /proc / 本地 dumpsys），主机通过
//! `adb exec-out` 长连接读取 NDJSON 事件流，无每轮 adb 往返开销。
//! 协议见 xperf-agent/main.rs 头注释。

use crate::platform::Platform;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Command;

const DEVICE_AGENT_PATH: &str = "/data/local/tmp/xperf-agent";

/// 采样指标开关：与 agent 命令行 --cpu/--memory/--fps/--freq/--io/--net/--gpu/--thermal 一一对应。
/// 设备无关指标（freq/thermal/net/gpu）不区分 PID；io 为每 PID。
#[derive(Debug, Clone, Copy, Default)]
pub struct MetricFlags {
    /// CPU（进程 + 线程）
    pub cpu: bool,
    /// 内存（PSS + 分类明细）
    pub memory: bool,
    /// FPS（SurfaceFlinger 图层帧时间戳）
    pub fps: bool,
    /// 每核 CPU 频率（sysfs）
    pub freq: bool,
    /// 温度与热降频状态
    pub thermal: bool,
    /// GPU busy%（按平台选路：kgsl/QNX/topgpu/ligfx/显存保底）
    pub gpu: bool,
    /// 每 PID IO 速率
    pub io: bool,
    /// 整机网络速率
    pub net: bool,
}

impl MetricFlags {
    /// 是否至少开启一项
    pub fn any(&self) -> bool {
        self.cpu || self.memory || self.fps || self.freq || self.thermal || self.gpu || self.io || self.net
    }

    fn to_agent_args(self) -> Vec<String> {
        let mut args = Vec::new();
        if self.cpu { args.push("--cpu".into()); }
        if self.memory { args.push("--memory".into()); }
        if self.fps { args.push("--fps".into()); }
        if self.freq { args.push("--freq".into()); }
        if self.io { args.push("--io".into()); }
        if self.net { args.push("--net".into()); }
        if self.gpu { args.push("--gpu".into()); }
        if self.thermal { args.push("--thermal".into()); }
        args
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
/// 设备端 agent 的 NDJSON 事件（每行一个 JSON 对象，`t` 字段为类型标签）。
/// 详见 xperf-agent/src/main.rs 头部的协议注释。
pub enum AgentEvent {
    /// maxkhz: 每核最大频率（KHz），旧版 agent 无此字段时为空
    Hello {
        /// 设备核数
        ncores: u32,
        /// 每核最大频率（KHz）
        #[serde(default)]
        maxkhz: Vec<u64>,
        /// 协议版本（host 校验：不符则 suicide + 重推二进制）
        #[serde(default)]
        version: u32,
        /// agent 进程是否 uid=0（v3 起；无 root 时 IO/smaps_rollup 等路径降级）
        #[serde(default)]
        root: bool,
    },
    /// ts: 墙钟毫秒；cpu: 单核口径 %；th: [tid, 线程名, cpu%]（仅 >0.05% 的线程）
    Cpu {
        /// 墙钟毫秒
        ts: u64,
        /// 进程 PID
        pid: u32,
        /// 进程 CPU %（单核口径）
        cpu: f32,
        /// 线程明细：(tid, 线程名, cpu%)
        th: Vec<(u32, String, f32)>,
    },
    /// pss/rss 及分类明细，单位 KB；分类字段仅 interval≥500ms（dumpsys meminfo 路径）有值
    Mem {
        /// 墙钟毫秒
        ts: u64,
        /// 进程 PID
        pid: u32,
        /// 总 PSS（KB）
        pss: u64,
        /// RSS（KB）
        rss: u64,
        /// Java 堆（KB）
        #[serde(default)]
        java: u64,
        /// Native 堆（KB）
        #[serde(default)]
        native: u64,
        /// 代码段（KB）
        #[serde(default)]
        code: u64,
        /// 栈（KB）
        #[serde(default)]
        stack: u64,
        /// 图形缓冲（KB）
        #[serde(default)]
        gfx: u64,
        /// 其他私有（KB）
        #[serde(default)]
        other: u64,
        /// 系统分摊（KB）
        #[serde(default)]
        sys: u64,
    },
    /// 每个活跃图层一条；全静止时一条零帧样本
    Fps {
        /// 墙钟毫秒
        ts: u64,
        /// 进程 PID
        pid: u32,
        /// 图层名
        layer: String,
        /// 帧率
        fps: f32,
        /// 窗口内新帧数
        frames: u32,
        /// 窗口内 jank 帧数
        jank: u32,
    },
    /// 每核当前频率（KHz），下标与 Hello 的 maxkhz 对应；0 = 该核离线/读失败
    Freq {
        /// 墙钟毫秒
        ts: u64,
        /// 每核当前频率（KHz）
        khz: Vec<u64>,
    },
    /// 每 PID IO 速率 KB/s：r/w=rchar/wchar 逻辑读写，dr/dw=read_bytes/write_bytes 磁盘读写
    Io {
        /// 墙钟毫秒
        ts: u64,
        /// 进程 PID
        pid: u32,
        /// 逻辑读速率（KB/s）
        r: f32,
        /// 逻辑写速率（KB/s）
        w: f32,
        /// 磁盘读速率（KB/s）
        dr: f32,
        /// 磁盘写速率（KB/s）
        dw: f32,
    },
    /// 整机网络速率 KB/s（聚合物理口，排除回环/隧道；per-app 无数据源）
    Net {
        /// 墙钟毫秒
        ts: u64,
        /// 下行速率（KB/s）
        rx: f32,
        /// 上行速率（KB/s）
        tx: f32,
    },
    /// GPU：busy 为窗口内 busy 占比 %；mhz 为当前时钟（0 = 无时钟源）；
    /// util/maxmhz 仅 QNX 路径有值（util = busy 按频率折算的利用率，kgsl 路径为 0）
    Gpu {
        /// 墙钟毫秒
        ts: u64,
        /// GPU busy %
        busy: f32,
        /// GPU util %（QNX 路径）
        #[serde(default)]
        util: f32,
        /// 当前时钟 MHz（0 = 无时钟源）
        mhz: u32,
        /// 最大时钟 MHz（QNX 路径）
        #[serde(default)]
        maxmhz: u32,
    },
    /// QNX 路径：每进程 GPU busy %（按 comm 名归因到 Android PID）
    GpuProc {
        /// 墙钟毫秒
        ts: u64,
        /// 进程 PID
        pid: u32,
        /// 该进程 GPU busy %
        busy: f32,
    },
    /// --gpu 降级路径（GPU 在 hypervisor 后的平台）：每 PID GPU 显存字节 + 整机 global
    GpuMem {
        /// 墙钟毫秒
        ts: u64,
        /// 进程 PID
        pid: u32,
        /// 进程 GPU 显存（字节）
        bytes: u64,
        /// 整机 GPU 显存（字节）
        global: u64,
    },
    /// 温度与热降频：status 为 Android ThermalStatus（-1=未知）；sensors 为 [名称, 类型, °C]
    Temp {
        /// 墙钟毫秒
        ts: u64,
        /// Android ThermalStatus（-1 = 未知）
        status: i32,
        /// 传感器读数：(名称, 类型, °C)
        #[serde(default)]
        sensors: Vec<(String, i32, f32)>,
    },
    /// 进程退出（agent 检测到后重扫包名进程）
    Exit {
        /// 退出的进程 PID
        pid: u32,
    },
    /// 包名下无进程（agent 每秒上报一次直至进程出现）
    Noproc,
    /// agent 侧非致命错误（如单轮 overrun）
    Err {
        /// 错误描述
        msg: String,
    },
}

/// 与 agent 的协议版本：与 xperf-agent 的 PROTOCOL_VERSION 同步 bump（改 wire 协议/命令时）。
/// host 连接时校验 hello 的 version，不一致则通知 suicide + 强杀重推。
/// v3：hello 增加 `root` 字段。
/// v4：agent 侧 SS4 --fps 短路（FPS 由 host 侧 frametimeline 通道合成，见 hostchan）。
pub const AGENT_PROTOCOL_VERSION: u32 = 4;

/// daemon 的抽象 socket 名（设备端 `localabstract:xperf-agent`）
const AGENT_ABSTRACT_SOCK: &str = "xperf-agent";

/// 与设备端 daemon 的会话流（`adb forward` + TCP 到 localabstract:xperf-agent）。
///
/// 架构（2026-09-07 daemon 化改版）：agent 常驻设备做 socket 服务，host 每会话一条
/// TCP 连接（连接即收 hello；`start` 开采样 / `stop` / `ping` / `suicide`）。
/// host 死亡 → TCP 断开 → daemon 侧会话即收；0 会话持续 60s daemon 自杀。
/// `writer` 同时供 ping 线程保活（每 5s 一次；agent 30s 无数据断开连接）。
pub struct AgentStream {
    writer: std::sync::Arc<std::sync::Mutex<std::net::TcpStream>>,
    reader: BufReader<std::net::TcpStream>,
    ping_stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// host 侧合成事件通道（SS4 frametimeline FPS 等，见 [`crate::hostchan`]）。
    /// 与设备端事件统一经 next_event/next_event_batch 取出，消费端零改动；
    /// 发送端线程随本流析构（receiver Drop → 发送失败退出）。
    extra_rx: Option<crate::hostchan::HostRx>,
}

impl AgentStream {
    /// 非阻塞取一条 host 侧合成事件；通道断开（发送线程退出）则摘除返回 None。
    fn take_extra(&mut self) -> Option<std::result::Result<AgentEvent, String>> {
        let rx = self.extra_rx.as_ref()?;
        match rx.try_recv() {
            Ok(ev) => Some(ev),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.extra_rx = None;
                None
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
        }
    }

    /// 阻塞读取下一行事件；流结束（会话断开/daemon 退出）返回 Ok(None)。
    /// 解析失败的行跳过（返回 Some(Err) 由调用方决定）。
    ///
    /// host 侧合成事件在每趟读行前检查；设备端静默时 agent 每轮发心跳空行
    /// （整轮零输出探活），故合成事件的投递延迟至多一个采样间隔。
    pub fn next_event(&mut self) -> Result<Option<std::result::Result<AgentEvent, String>>> {
        loop {
            if let Some(ev) = self.take_extra() {
                return Ok(Some(ev));
            }
            let mut line = String::new();
            let n = self.reader.read_line(&mut line)?;
            if n == 0 {
                // EOF 前先放尽已排队的 host 合成事件（如通道退出终态 err），
                // 否则随 None 返回被静默丢弃
                if let Some(ev) = self.take_extra() {
                    return Ok(Some(ev));
                }
                return Ok(None); // EOF
            }
            let line = line.trim();
            if line.is_empty() {
                continue; // 心跳空行：回到 loop 头先取合成事件
            }
            return Ok(Some(
                serde_json::from_str(line).map_err(|e| format!("{} (行: {})", e, line)),
            ));
        }
    }

    /// 批量读取：阻塞等首条，随后抽干读缓冲里已完整的行（不阻塞、不改协议）。
    /// 一轮节拍的多条事件（多 PID/多指标）通常同 burst 到达，批量后宿主侧
    /// 一次 emit/分发即可，显著降低 IPC 次数（GUI 热路径）。
    /// 返回 None = 流结束；Some(vec) 至少含一条。
    pub fn next_event_batch(&mut self) -> Result<Option<Vec<std::result::Result<AgentEvent, String>>>> {
        let mut out = Vec::new();
        match self.next_event()? {
            None => return Ok(None),
            Some(e) => out.push(e),
        }
        loop {
            let buf = self.reader.buffer();
            let Some(nl) = buf.iter().position(|b| *b == b'\n') else {
                break; // 缓冲内无完整行：不阻塞等剩余部分
            };
            let line: Vec<u8> = buf[..=nl].to_vec();
            self.reader.consume(nl + 1);
            let line = String::from_utf8_lossy(&line);
            let line = line.trim();
            if line.is_empty() {
                continue; // 心跳空行
            }
            out.push(serde_json::from_str(line).map_err(|e| format!("{} (行: {})", e, line)));
        }
        // 同 burst 抽干 host 侧合成事件队列
        while let Some(ev) = self.take_extra() {
            out.push(ev);
        }
        Ok(Some(out))
    }

    /// 结束会话（停 ping + 断开 TCP；daemon 侧会话线程随连接断开收尾，daemon 本体常驻）
    pub fn kill(&mut self) {
        self.ping_stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Ok(w) = self.writer.lock() {
            let _ = w.shutdown(std::net::Shutdown::Both);
        }
    }
}

impl Drop for AgentStream {
    fn drop(&mut self) {
        self.kill();
    }
}

/// 本机 agent 二进制路径（交叉编译产物）
pub fn agent_binary_path() -> PathBuf {
    // xperf-core/Cargo.toml 所在目录的上级 = workspace 根
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("target/aarch64-linux-android/release/xperf-agent")
}

/// 目录树下最新 .rs 文件的 mtime（递归；目录不可读/为空返回 None）
fn newest_mtime_under(dir: &Path) -> Option<std::time::SystemTime> {
    std::fs::read_dir(dir).ok()?
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.is_dir() {
                newest_mtime_under(&p)
            } else if p.extension().is_some_and(|x| x == "rs") {
                std::fs::metadata(&p).ok().and_then(|m| m.modified().ok())
            } else {
                None
            }
        })
        .max()
}

/// 若本机尚未交叉编译 agent 二进制则自动构建；源码变更（mtime 更新）时也自动重建。
/// （链接器：`.cargo/ndk-clang.sh` 探测——NDK >= 25.1.8937393 中取最相近，
/// 显式 ANDROID_NDK_HOME 等环境变量优先，Mac/Linux 均可）
pub fn ensure_agent_built() -> Result<PathBuf> {
    let bin = agent_binary_path();
    let needs_build = if !bin.exists() {
        true
    } else {
        // 源码变更检测：src 下任一 .rs 比 mtime 新则重建（agent 已拆多模块，不能只盯 main.rs）
        let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap()
            .join("xperf-agent/src");
        let bin_mtime = std::fs::metadata(&bin).ok().and_then(|m| m.modified().ok());
        newest_mtime_under(&src_dir) > bin_mtime
    };
    if needs_build {
        eprintln!("agent 需要构建/重建（aarch64-linux-android）...");
        let mut cmd = Command::new("cargo");
        cmd.args(["build", "-p", "xperf-agent", "--target", "aarch64-linux-android", "--release"])
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap());
        // 链接器由 .cargo/config.toml → .cargo/ndk-clang.sh 跨平台探测（版本策略见脚本头注释）
        let status = cmd.status()?;
        if !status.success() {
            anyhow::bail!("交叉编译 xperf-agent 失败（需要 Android NDK，见 .cargo/config.toml）");
        }
    }
    Ok(bin)
}

/// 部署时是否自动尝试 adb root（纯函数，策略由单测锁定）：仅车机平台
/// （SS2/SS3/SS4 内部开发设备）；泛型 Android（手机等用户设备）不默认提权。
fn should_auto_root(id: crate::platform::PlatformId) -> bool {
    !matches!(id, crate::platform::PlatformId::Android)
}

/// 尝试 adb root（生产构建可能失败，静默忽略）。
/// 不解析 adb root 文案（各版本不同），直接 `adb shell id` 验证 uid。
/// `serial`：目标设备（多设备并行会话用，`None` 回退全局选择）。
///
/// 策略（2026-09-09）：**仅车机平台（SS2/SS3/SS4）自动 root**——车机是内部开发
/// 设备，root 无副作用且 IO 等指标依赖它；非车机（普通 Android 手机等）跳过，
/// 不在用户设备上默认提权（无 root 时各指标按能力降级，矩阵见 WORKSPACE G 节）。
/// `XPERF_NO_AUTO_ROOT=1` 整体禁用自动 root（非 root 降级路径的回归测试用）。
fn try_adb_root(serial: Option<&str>) {
    if std::env::var_os("XPERF_NO_AUTO_ROOT").is_some() {
        return;
    }
    let platform_id = crate::platform::detect_platform_live(serial).id();
    if !should_auto_root(platform_id) {
        return; // 非车机：不默认获取 root
    }
    if adb_root_verify(serial) {
        eprintln!("adb root: 成功（uid=0）");
        return;
    }
    // Ss4 兜底（设计 §6 ②）：标准 adb 直连 root 失败 → 经 MindRT 网关 rootandroid.sh
    if matches!(platform_id, crate::platform::PlatformId::Ss4)
        && ss4_root_via_gateway(serial).is_ok()
    {
        eprintln!("adb root: 成功（经网关 rootandroid.sh，uid=0）");
        return;
    }
    let id = crate::utils::run_adb_command_for(serial, &["shell", "id"]).map(|o| o.stdout).unwrap_or_default();
    eprintln!("adb root: 未生效（{}），无 root 指标按能力降级（IO 等不可用）", id.trim());
}

/// 直连 `adb root` + 等 adbd 回来 + `shell id` 验证（uid=0 返回 true）
fn adb_root_verify(serial: Option<&str>) -> bool {
    let adb = || crate::utils::adb_for(serial);
    let _ = adb().args(["root"]).output();
    // adbd 重启后等设备回来
    let _ = adb().args(["wait-for-device"]).output();
    shell_uid0(serial)
}

/// `shell id` 是否 uid=0
fn shell_uid0(serial: Option<&str>) -> bool {
    crate::utils::run_adb_command_for(serial, &["shell", "id"])
        .map(|o| o.stdout.contains("uid=0"))
        .unwrap_or(false)
}

/// Ss4 root 兜底路径（设计 §6 ②，S0 实测脚本在 MindRT 的 `/system_ext/bin/`、
/// PATH 内）：经网关 `adb -s <mindrt> shell rootandroid.sh` 把 Android adbd 提为
/// root——adbd 提权重启会断开伪设备连接，经 [`crate::bridge::reconnect`] 重连后
/// 轮询 `id` 确认（15s 上限，与 acquire_root 同骨架）。
/// Ok = uid=0 已验证；Err 携带失败原因（无网关信息/脚本执行失败/超时未生效）。
fn ss4_root_via_gateway(serial: Option<&str>) -> Result<String, String> {
    use std::time::{Duration, Instant};
    let eff = crate::utils::resolve_serial(serial).ok_or("无目标 serial")?;
    let gateway = crate::bridge::gateway_for_android(&eff)
        .ok_or_else(|| "桥接未收敛，无网关信息".to_string())?;
    let out = crate::utils::run_adb_command_for(Some(&gateway), &["shell", "rootandroid.sh"])
        .map_err(|e| format!("rootandroid.sh 执行失败: {}", e))?;
    // adbd 提权重启掉连接 → 重连（规则若被清一并重建）
    let _ = crate::bridge::reconnect(&eff);
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(500));
        if shell_uid0(serial) {
            return Ok("经网关 rootandroid.sh，uid=0".to_string());
        }
    }
    Err(format!("rootandroid.sh 未生效（15s 超时；脚本输出: {}）", out.stdout.trim()))
}

/// 显式获取 root 权限（GUI「获取 root」按钮用；用户显式动作，任意平台都执行——
/// 与 try_adb_root 的「仅车机自动 root」策略不同，这里是用户明确要求）。
/// `adb root` 会重启 adbd：设备短暂离线、设备端 daemon 被杀，采样中会话走既有
/// 重连恢复（新 daemon 继承 root 身份，新 hello root=true）。轮询 `id` 确认
/// （adbd 重启需数秒），上限 15s；生产构建拒 root 时透传 adb 原文报错。
/// Ss4 兜底（设计 §6 ②）：直连失败时经 MindRT 网关 `rootandroid.sh`，
/// ①② 报错各自透传区分。
/// `serial`：目标设备（多设备并行会话用，`None` 回退全局选择）。
pub fn acquire_root(serial: Option<&str>) -> Result<String> {
    use std::time::{Duration, Instant};
    let out = crate::utils::adb_for(serial)
        .args(["root"])
        .output()
        .context("adb root 执行失败")?;
    let msg = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut direct_err: Option<String> = None;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(500));
        let id = crate::utils::run_adb_command_for(serial, &["shell", "id"]).map(|o| o.stdout).unwrap_or_default();
        if id.contains("uid=0") {
            return Ok("已获取 root 权限（uid=0）".to_string());
        }
        // adbd 已回来但仍 shell 且 adb root 明确拒绝（生产构建）：fail-fast 报原文
        if id.contains("uid=") && msg.contains("cannot") {
            direct_err = Some(msg.trim().to_string());
            break;
        }
    }
    let err1 = direct_err.unwrap_or_else(|| {
        if msg.trim().is_empty() {
            "adb root 超时（15s 内设备未以 root 回来）".to_string()
        } else {
            format!("{}（15s 未生效）", msg.trim())
        }
    });
    // ② Ss4 经网关兜底（① 已失败；①② 报错各自透传）
    if matches!(
        crate::platform::detect_platform_live(serial).id(),
        crate::platform::PlatformId::Ss4
    ) {
        match ss4_root_via_gateway(serial) {
            Ok(detail) => return Ok(format!("已获取 root 权限（{}）", detail)),
            Err(e2) => anyhow::bail!("① {}；② 经网关 rootandroid.sh 也失败：{}", err1, e2),
        }
    }
    anyhow::bail!("{}", err1)
}

/// 推送 agent 到设备（设备上不存在或大小/mtime 不一致时）
/// 大小+修改时间双判：同尺寸不同版本（改代码但恰好等长）也能被更新。
/// `serial`：目标设备（多设备并行会话用，`None` 回退全局选择）。
pub fn deploy_agent(local: &Path, serial: Option<&str>) -> Result<()> {
    // 自动尝试 root（仅车机平台，见 try_adb_root；生产构建会静默失败，不影响后续流程）
    try_adb_root(serial);
    let local_meta = std::fs::metadata(local)?;
    let local_size = local_meta.len();
    let local_mtime = local_meta.modified().ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let remote_size = crate::utils::run_adb_command_for(serial, &["shell", "stat", "-c", "%s", DEVICE_AGENT_PATH])
        .ok()
        .and_then(|o| o.stdout.trim().parse::<u64>().ok());
    let remote_mtime = crate::utils::run_adb_command_for(serial, &["shell", "stat", "-c", "%Y", DEVICE_AGENT_PATH])
        .ok()
        .and_then(|o| o.stdout.trim().parse::<u64>().ok());
    if remote_size == Some(local_size) && remote_mtime == Some(local_mtime) {
        return Ok(()); // 已是最新（大小一致）
    }
    crate::utils::run_adb_command_for(serial, &[
        "push",
        &local.to_string_lossy(),
        DEVICE_AGENT_PATH,
    ])?;
    // push 后同步 mtime 对齐本地，使下次部署的 mtime 匹配判断生效
    let _ = crate::utils::run_adb_command_for(serial, &["shell", &format!("touch -d @{} {}", local_mtime, DEVICE_AGENT_PATH)]);
    crate::utils::run_adb_command_for(serial, &["shell", "chmod", "755", DEVICE_AGENT_PATH])?;
    Ok(())
}

/// 启动采样会话：确保 daemon 在跑（版本不符重推重启）→ 连接 → 下发 start，返回事件流。
/// platform: 平台提示（如 "ss3"），传入时 agent 跳过对应探测
/// `serial`：目标设备（多设备并行会话用，`None` 回退全局选择）。
pub fn spawn_agent(
    package: Option<&str>,
    interval_ms: u64,
    flags: MetricFlags,
    platform: Option<&dyn Platform>,
    serial: Option<&str>,
) -> Result<AgentStream> {
    use std::io::Write as _;
    let port = ensure_daemon(serial)?;
    let tcp = std::net::TcpStream::connect(("127.0.0.1", port))?;
    let _ = tcp.set_nodelay(true);
    let writer = std::sync::Arc::new(std::sync::Mutex::new(tcp.try_clone()?));
    let reader = BufReader::new(tcp);
    // start 命令复用 agent argv 语法（daemon 端 parse_args 同款校验）
    let mut cmd = String::from("start");
    if let Some(pkg) = package {
        cmd.push_str(&format!(" --package {}", pkg));
    }
    cmd.push_str(&format!(" --interval {}", interval_ms));
    for a in flags.to_agent_args() {
        cmd.push(' ');
        cmd.push_str(&a);
    }
    // 平台参数：让 agent 跳过运行时探测，直接用平台指定路径
    if let Some(p) = platform {
        cmd.push_str(&format!(" --platform {}", p.id().as_str()));
        if let Some(qnx) = p.qnx_host() {
            cmd.push_str(&format!(" --qnx-host {}", qnx));
        }
    }
    writeln!(writer.lock().unwrap(), "{}", cmd)?;
    writer.lock().unwrap().flush()?;
    // 心跳保活：5s 一次（agent 30s 无数据断开连接）
    let ping_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let (w, stop) = (writer.clone(), ping_stop.clone());
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(5));
            if stop.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            let mut g = match w.lock() {
                Ok(g) => g,
                Err(_) => break,
            };
            if writeln!(g, "ping").and_then(|_| g.flush()).is_err() {
                break;
            }
        });
    }
    // SS4 host 侧通道（frametimeline FPS / ligfx GPU）：合成事件经 extra_rx 汇入
    // 本流；ping_stop 兼作通道线程的会话停止标志（kill/drop 时置位）
    let extra_rx = crate::hostchan::maybe_spawn(flags, platform, package, serial, ping_stop.clone());
    Ok(AgentStream { writer, reader, ping_stop, extra_rx })
}

/// 确保设备端 daemon 在跑且协议版本匹配，返回 **host 本机可直连**的端口。
///
/// - 无 daemon：强杀残留（老版 stdout agent/泄漏 daemon）→ 强制重推 → 启动 → 探活
/// - 版本不符：经 probe 连接发 `suicide` 通知 + `pkill` 强杀兜底 → 强制重推 → 重启
///
/// 强制重推（绕过 size/mtime 快检）：同秒重建的同尺寸二进制会被快检误判「已是最新」
/// （2026-09-07 实测：v99/v2 两构建同秒落地同尺寸，快检跳推导致版本协商死循环）。
/// `serial`：目标设备（多设备并行会话用，`None` 回退全局选择）。
///
/// 端口来源按 Transport 分流（SSH 远程后端，设计 §4.4）：
/// - `Local`：`adb forward` 监听在本机 server，端口直接可用；
/// - `Ssh`：forward 监听在**远端** server（本机直连必 refused），经 hop#2
///   （[`crate::transport::SshTunnel::add_forward`]）映射回本机端口。
///   映射表按 remote_port 复用：重连/重试不产生重复转发。
fn ensure_daemon(serial: Option<&str>) -> Result<u16> {
    let mut last_err = String::new();
    for _ in 0..2 {
        let remote_port = ensure_forward(serial)?;
        let port = match crate::transport::transport() {
            crate::transport::Transport::Local => remote_port,
            crate::transport::Transport::Ssh(_) => crate::transport::tunnel()
                .context("远程模式但隧道不存在（init_remote 未调用？）")?
                .add_forward(remote_port)?,
        };
        match probe_daemon(port) {
            Ok(v) if v == AGENT_PROTOCOL_VERSION => return Ok(port),
            Ok(old) => {
                eprintln!("agent 协议版本不符（设备 v{} vs 宿主 v{}），通知 suicide 并重推", old, AGENT_PROTOCOL_VERSION);
                if let Ok(mut t) = std::net::TcpStream::connect(("127.0.0.1", port)) {
                    use std::io::Write as _;
                    let _ = writeln!(t, "suicide");
                    let _ = t.flush();
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
                kill_device_agent(serial);
                deploy_fresh(serial)?;
                start_daemon(serial)?;
                if let Err(e) = wait_probe(port) {
                    last_err = e.to_string();
                    continue;
                }
            }
            Err(e) => {
                last_err = e.to_string();
                kill_device_agent(serial); // 残留一律清（老版 stdout agent 不监听 socket）
                deploy_fresh(serial)?;
                start_daemon(serial)?;
                if let Err(e) = wait_probe(port) {
                    last_err = e.to_string();
                    continue;
                }
            }
        }
    }
    anyhow::bail!("xperf-agent daemon 启动失败：{}", last_err)
}

/// 部署（强制重推）并启动 daemon（探活失败/版本不符路径共用）
fn deploy_fresh(serial: Option<&str>) -> Result<()> {
    let bin = ensure_agent_built()?;
    push_agent_binary(&bin, serial)
}

/// 无条件 push agent 二进制（ensure_daemon 的重推路径用；deploy_agent 的快检供
/// CLI/GUI 启动预热用——那之后 ensure_daemon 仍可能因版本/探活触发强制重推）
fn push_agent_binary(local: &Path, serial: Option<&str>) -> Result<()> {
    try_adb_root(serial);
    let local_mtime = std::fs::metadata(local)?
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    crate::utils::run_adb_command_for(serial, &["push", &local.to_string_lossy(), DEVICE_AGENT_PATH])?;
    // push 后同步 mtime 对齐本地，使后续 deploy_agent 的 mtime 匹配判断生效
    let _ = crate::utils::run_adb_command_for(serial, &["shell", &format!("touch -d @{} {}", local_mtime, DEVICE_AGENT_PATH)]);
    crate::utils::run_adb_command_for(serial, &["shell", "chmod", "755", DEVICE_AGENT_PATH])?;
    Ok(())
}

/// 强杀设备端全部 xperf-agent（`[n]` 防载体 shell 自匹配）
fn kill_device_agent(serial: Option<&str>) {
    let _ = crate::utils::run_adb_command_for(serial, &["shell", "pkill -f 'xperf-age[n]t'"]);
}

/// 启动 daemon：setsid 脱离 adb 会话 + nohup + stdio 全重定向（host 断开不影响存活）。
/// 启动日志在设备端 /data/local/tmp/xperf-agent.log（排查 daemon 启动失败用）。
fn start_daemon(serial: Option<&str>) -> Result<()> {
    crate::utils::run_adb_command_for(
        serial,
        &["shell", &format!("setsid nohup {} --daemon >/data/local/tmp/xperf-agent.log 2>&1 < /dev/null &", DEVICE_AGENT_PATH)],
    )?;
    Ok(())
}

/// 等 daemon 监听就绪：daemon 进程 spawn + bind 需要数百 ms，探活重试 8×400ms。
fn wait_probe(port: u16) -> Result<u32> {
    let mut last_err = String::new();
    for _ in 0..8 {
        match probe_daemon(port) {
            Ok(v) => return Ok(v),
            Err(e) => {
                last_err = e.to_string();
                std::thread::sleep(std::time::Duration::from_millis(400));
            }
        }
    }
    anyhow::bail!("探活超时：{}", last_err)
}

/// 探活 daemon：TCP 连转发端口，读 hello 行解析协议版本（3s 超时）。
/// Err = 无 daemon/连接失败/格式非法（调用方走部署重启路径）。
fn probe_daemon(port: u16) -> Result<u32> {
    use std::io::BufRead;
    let addr: std::net::SocketAddr = ([127, 0, 0, 1], port).into();
    let tcp = std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(1500))?;
    tcp.set_read_timeout(Some(std::time::Duration::from_secs(3)))?;
    let mut line = String::new();
    BufReader::new(tcp).read_line(&mut line)?;
    let ev: AgentEvent = serde_json::from_str(line.trim())?;
    match ev {
        AgentEvent::Hello { version, .. } => Ok(version),
        _ => anyhow::bail!("首行非 hello: {}", line.trim()),
    }
}

/// 确保 host→设备的 adb forward 规则存在，返回 host 侧端口。
/// 先查 `adb forward --list` 复用既有规则（规则随 adb server 常驻，跨会话复用），
/// 没有则 `forward tcp:0` 新建（端口由 adb 分配）。
/// `serial`：目标设备（多设备并行会话用，`None` 回退全局选择）。
fn ensure_forward(serial: Option<&str>) -> Result<u16> {
    let eff = crate::utils::resolve_serial(serial);
    let target = format!("localabstract:{}", AGENT_ABSTRACT_SOCK);
    if let Ok(out) = crate::utils::adb_for(None).args(["forward", "--list"]).output() {
        let list = String::from_utf8_lossy(&out.stdout);
        for line in list.lines() {
            let tok: Vec<&str> = line.split_whitespace().collect();
            // 行格式："<serial> tcp:<port> localabstract:<sock>"
            if tok.len() == 3 && tok[2] == target && eff.as_deref().is_none_or(|e| e == tok[0]) {
                if let Some(p) = tok[1].strip_prefix("tcp:").and_then(|p| p.parse::<u16>().ok()) {
                    return Ok(p);
                }
            }
        }
    }
    let out = crate::utils::adb_for(serial)
        .args(["forward", "tcp:0", &target])
        .output()?;
    let s = String::from_utf8_lossy(&out.stdout);
    s.trim()
        .trim_start_matches("tcp:")
        .parse::<u16>()
        .map_err(|e| anyhow::anyhow!("adb forward 返回非法端口: {:?} ({})", s.trim(), e))
}

/// 目标设备是否在线（重连轮询用）。已指定目标设备时只认该设备
/// （`adb -s <serial> get-state` = "device"）——多台设备同连时其他设备在
/// 不算"回来了"；未指定设备时任意一台在线即可。
/// `serial`：目标设备（多设备并行会话用，`None` 回退全局选择）。
fn device_online(serial: Option<&str>) -> bool {
    let adb = || crate::utils::adb_for(serial);
    let any_online = adb()
        .arg("devices")
        .output()
        .map(|o| {
            o.status.success()
                && String::from_utf8_lossy(&o.stdout).lines().skip(1).any(|l| !l.trim().is_empty())
        })
        .unwrap_or(false);
    if !any_online {
        return false;
    }
    match crate::utils::resolve_serial(serial) {
        // get-state：正常输出 "device"；offline/unauthorized/serial 无效时 adb 报错（status != 0）
        Some(eff) => {
            let online = || {
                adb()
                    .arg("get-state")
                    .output()
                    .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "device")
                    .unwrap_or(false)
            };
            if online() {
                return true;
            }
            // SS4 桥接伪设备（localhost:<port>）：GVM 重启掉线后无人重新 connect
            // 会永远等不回——先经 bridge 自愈（按规则存在性重建 + connect），
            // 再探活一次（设计 §4.3.2；GUI 侧监视器 3s 轮询经 refresh 双保险）
            if eff.starts_with("localhost:") && crate::bridge::reconnect(&eff) {
                return online();
            }
            false
        }
        None => true,
    }
}

/// QNX kgsl 统计链停止（CLI/GUI 会话结束由 host 兜底调用）。
///
/// daemon 化（2026-09-07）后的语义：正常路径下链清理由 daemon 的会话 teardown
/// 完成（先停链后断 TCP，顺序确定），本函数只是**daemon 异常死亡**（SIGKILL
/// 等，teardown 无机会执行）时的兜底。故门控极简单：
/// - daemon 进程在（pgrep ≥1）→ teardown 已处理（或别的会话正在采样，停链会
///   杀掉对方的流），直接返回，不等待不探测；
/// - daemon 不在（异常死亡）→ 调设备端 agent `--qnx-stop` 一次性模式：纯观察
///   frame 流（只读 slog 不写 kgsl-control），在跑才发 echo>（死写入者）停止
///   （对已停链写入会将其全部复活——toggle 语义真机实测，不可无条件执行）。
///   2026-09-09 起走 agent 内嵌 telnet（无 busybox 依赖，shell 身份可执行）；
///   旧版设备端二进制无 --qnx-stop 会报错退出，best-effort 忽略。
pub fn qnx_stop_stats(platform: &dyn crate::platform::Platform, interval_ms: u64, serial: Option<&str>) {
    let Some(ip) = platform.qnx_host() else { return };
    // `[n]` 正则防检测命令载体自匹配。pgrep 在目标设备上执行，多设备并行时
    // 各设备天然隔离（一台设备的停链不受另一台上还在采样的 daemon 影响）。
    let adb = || crate::utils::adb_for(serial);
    let others = adb()
        .arg("shell")
        .arg("pgrep -fc 'xperf-age[n]t'")
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<u32>().ok())
        .unwrap_or(1); // 探测失败按「daemon 在」处理（不动链，宁留勿乱）
    if others >= 1 {
        return; // daemon 在：会话 teardown 已停链（或他人在采样）
    }
    let period = interval_ms.clamp(100, 1000);
    // adb 调用本身失败：best-effort 忽略
    if let Ok(o) = adb()
        .arg("shell")
        .arg(format!("{} --qnx-stop {} {}", DEVICE_AGENT_PATH, ip, period))
        .output()
    {
        let out = String::from_utf8_lossy(&o.stdout).trim().to_string();
        let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
        // 诊断留痕（正常路径 teardown 已停链时这里会打印「未在跑，不动链」）
        eprintln!("qnx-stop: {}{}", out, if err.is_empty() { String::new() } else { format!(" | stderr: {}", err) });
    }
}

/// 断连恢复：事件流 EOF（adb 长连接断开 / agent 进程退出）后调用。
/// 每 500ms 轮询设备状态，设备回来后重新部署并启动 agent。
/// `is_running` 返回 false（用户停止 / Ctrl-C）时返回 None；重连成功返回新事件流。
/// 调用方持有的采样状态（时序、峰值等）不受影响，新 agent 的首轮仅重建基线。
/// `serial`：目标设备（多设备并行会话用，`None` 回退全局选择）。
///
/// SSH 远程模式（S9）：隧道本身可能断（网络抖动/远端重启），此时 `device_online`
/// 的 adb 调用也失败——先判隧道再判设备：隧道死则指数退避重建（1s→2s→…→30s 上限），
/// 重建后 hop#2 映射表随新隧道清空（旧映射指向死端口），远端 forward 规则由
/// server 持有跨隧道存活，`ensure_forward` 查 list 复用（R10 不累积）。
pub fn reconnect_agent(
    package: Option<&str>,
    interval_ms: u64,
    flags: MetricFlags,
    platform: Option<&dyn Platform>,
    is_running: &dyn Fn() -> bool,
    serial: Option<&str>,
) -> Option<AgentStream> {
    let mut rebuild_backoff = std::time::Duration::from_secs(1);
    loop {
        if !is_running() {
            return None;
        }
        // 远程模式：先确认隧道存活（隧道死则 adb 全灭，等设备无意义）
        if matches!(crate::transport::transport(), crate::transport::Transport::Ssh(_))
            && !crate::transport::tunnel().map(|t| t.is_alive()).unwrap_or(false)
        {
            match crate::transport::rebuild_tunnel() {
                Ok(()) => {
                    eprintln!("SSH 隧道已重建");
                    rebuild_backoff = std::time::Duration::from_secs(1);
                }
                Err(e) => {
                    eprintln!("SSH 隧道重建失败：{:#}，{:?} 后重试…", e, rebuild_backoff);
                    std::thread::sleep(rebuild_backoff);
                    rebuild_backoff = (rebuild_backoff * 2).min(std::time::Duration::from_secs(30));
                }
            }
            continue;
        }
        if device_online(serial) {
            match ensure_agent_built()
                .and_then(|bin| deploy_agent(&bin, serial))
                .and_then(|_| spawn_agent(package, interval_ms, flags, platform, serial))
            {
                Ok(s) => return Some(s),
                Err(e) => eprintln!("agent 重连失败: {}，继续等待…", e),
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

#[cfg(test)]
mod tests {
    use super::*;



    #[test]
    fn test_parse_hello() {
        let ev: AgentEvent = serde_json::from_str(r#"{"t":"hello","ncores":8,"version":1}"#).unwrap();
        assert!(matches!(ev, AgentEvent::Hello { ncores: 8, .. }));
        // 新版带 maxkhz
        let ev: AgentEvent =
            serde_json::from_str(r#"{"t":"hello","ncores":8,"maxkhz":[2841600,2841600],"version":1}"#).unwrap();
        match ev {
            AgentEvent::Hello { ncores, maxkhz, .. } => assert_eq!((ncores, maxkhz), (8, vec![2841600, 2841600])),
            _ => panic!("应为 Hello 事件"),
        }
    }

    #[test]
    fn test_parse_b_metrics() {
        let ev: AgentEvent =
            serde_json::from_str(r#"{"t":"freq","ts":1,"khz":[2592000,2246400]}"#).unwrap();
        match ev {
            AgentEvent::Freq { khz, .. } => assert_eq!(khz, vec![2592000, 2246400]),
            _ => panic!("应为 Freq 事件"),
        }
        let ev: AgentEvent =
            serde_json::from_str(r#"{"t":"io","ts":1,"pid":29697,"r":12.5,"w":3.0,"dr":0.0,"dw":1.5}"#).unwrap();
        match ev {
            AgentEvent::Io { pid, r, w, dr, dw, .. } => {
                assert_eq!(pid, 29697);
                assert_eq!((r, w, dr, dw), (12.5, 3.0, 0.0, 1.5));
            }
            _ => panic!("应为 Io 事件"),
        }
        let ev: AgentEvent = serde_json::from_str(r#"{"t":"net","ts":1,"rx":123.5,"tx":56.0}"#).unwrap();
        match ev {
            AgentEvent::Net { rx, tx, .. } => assert_eq!((rx, tx), (123.5, 56.0)),
            _ => panic!("应为 Net 事件"),
        }
        let ev: AgentEvent = serde_json::from_str(r#"{"t":"gpu","ts":1,"busy":37.5,"mhz":585}"#).unwrap();
        match ev {
            AgentEvent::Gpu { busy, mhz, .. } => assert_eq!((busy, mhz), (37.5, 585)),
            _ => panic!("应为 Gpu 事件"),
        }
        let ev: AgentEvent =
            serde_json::from_str(r#"{"t":"gpumem","ts":1,"pid":29697,"bytes":154370048,"global":2639089664}"#).unwrap();
        match ev {
            AgentEvent::GpuMem { pid, bytes, global, .. } => {
                assert_eq!((pid, bytes, global), (29697, 154370048, 2639089664));
            }
            _ => panic!("应为 GpuMem 事件"),
        }
        let ev: AgentEvent = serde_json::from_str(
            r#"{"t":"temp","ts":1,"status":0,"sensors":[["soc0",0,42.5],["skin",3,41.0]]}"#,
        )
        .unwrap();
        match ev {
            AgentEvent::Temp { status, sensors, .. } => {
                assert_eq!(status, 0);
                assert_eq!(sensors, vec![("soc0".to_string(), 0, 42.5), ("skin".to_string(), 3, 41.0)]);
            }
            _ => panic!("应为 Temp 事件"),
        }
    }

    #[test]
    fn test_metric_flags_to_agent_args() {
        let flags = MetricFlags { cpu: true, net: true, ..Default::default() };
        assert_eq!(flags.to_agent_args(), vec!["--cpu", "--net"]);
        assert!(!MetricFlags::default().any());
        assert!(flags.any());
    }

    #[test]
    fn test_parse_cpu_with_threads() {
        // 真机协议：th 为 [tid, 线程名, cpu%] 三元组
        let ev: AgentEvent = serde_json::from_str(
            r#"{"t":"cpu","ts":1788258836663,"pid":29697,"cpu":27.59,"th":[[9871,"AdrenoOsLib",20.0],[29797,"XFW:Main",20.0]]}"#,
        )
        .unwrap();
        match ev {
            AgentEvent::Cpu { pid, cpu, th, .. } => {
                assert_eq!(pid, 29697);
                assert!((cpu - 27.59).abs() < 0.01);
                assert_eq!(th.len(), 2);
                assert_eq!(th[0].1, "AdrenoOsLib");
            }
            _ => panic!("应为 Cpu 事件"),
        }
    }

    #[test]
    fn test_parse_mem_exit_noproc_err() {
        let ev: AgentEvent = serde_json::from_str(r#"{"t":"mem","ts":1,"pid":2,"pss":483713,"rss":638728}"#).unwrap();
        match ev {
            // 分类字段缺省应为 0（低间隔 smaps_rollup 路径不带分类）
            AgentEvent::Mem { pss, rss, java, .. } => {
                assert_eq!((pss, rss, java), (483713, 638728, 0));
            }
            _ => panic!("应为 Mem 事件"),
        }
        let ev: AgentEvent = serde_json::from_str(r#"{"t":"exit","pid":29697}"#).unwrap();
        assert!(matches!(ev, AgentEvent::Exit { pid: 29697 }));
        let ev: AgentEvent = serde_json::from_str(r#"{"t":"noproc"}"#).unwrap();
        assert!(matches!(ev, AgentEvent::Noproc));
        let ev: AgentEvent = serde_json::from_str(r#"{"t":"err","msg":"round overrun"}"#).unwrap();
        assert!(matches!(ev, AgentEvent::Err { .. }));
    }

    #[test]
    fn test_parse_fps() {
        let ev: AgentEvent = serde_json::from_str(
            r#"{"t":"fps","ts":1788258836663,"pid":29697,"layer":"SVM Container#0","fps":30.0,"frames":32,"jank":0}"#,
        )
        .unwrap();
        match ev {
            AgentEvent::Fps { pid, layer, fps, frames, .. } => {
                assert_eq!(pid, 29697);
                assert_eq!(layer, "SVM Container#0");
                assert!((fps - 30.0).abs() < 0.01);
                assert_eq!(frames, 32);
            }
            _ => panic!("应为 Fps 事件"),
        }
    }

    /// host 侧合成事件（hostchan extra_rx）与设备端 NDJSON 行的合并读取：
    /// 合成事件优先取出；设备端只有心跳空行时也能投递；发送端退出后通道摘除，
    /// 设备端事件与 EOF 语义不受影响。
    #[test]
    fn test_agent_stream_merges_host_channel() {
        use std::io::Write as _;
        use std::net::TcpListener;
        // loopback 假 daemon：心跳空行 + 一条 freq 事件行，随后保持连接
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let srv = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            s.write_all(b"\n{\"t\":\"freq\",\"ts\":1,\"khz\":[42]}\n").unwrap();
            s.flush().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(200));
        });
        let tcp = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Ok(AgentEvent::Gpu { ts: 9, busy: 1.0, util: 0.0, mhz: 0, maxmhz: 0 })).unwrap();
        let mut stream = AgentStream {
            writer: std::sync::Arc::new(std::sync::Mutex::new(tcp.try_clone().unwrap())),
            reader: BufReader::new(tcp),
            ping_stop: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            extra_rx: Some(rx),
        };
        // 合成事件先于设备端事件取出
        match stream.next_event().unwrap() {
            Some(Ok(AgentEvent::Gpu { ts: 9, .. })) => {}
            other => panic!("应为合成 Gpu 事件: {:?}", other),
        }
        // 设备端真实事件随后到达（心跳空行被跳过）
        match stream.next_event().unwrap() {
            Some(Ok(AgentEvent::Freq { khz, .. })) => assert_eq!(khz, vec![42]),
            other => panic!("应为 Freq 事件: {:?}", other),
        }
        // 批量接口同口径（本例逐条到达：首条后即返回）
        // 发送端退出 + 服务端关闭 → EOF
        drop(tx);
        srv.join().unwrap();
        assert!(stream.next_event().unwrap().is_none());
        assert!(stream.extra_rx.is_none(), "发送端退出后通道应被摘除");
    }

    #[test]
    fn test_parse_mem_full_breakdown() {
        // interval≥500ms 的 dumpsys meminfo 路径：分类字段齐全
        let ev: AgentEvent = serde_json::from_str(
            r#"{"t":"mem","ts":1,"pid":2,"pss":484880,"rss":638728,"java":9684,"native":117624,"code":36112,"stack":100,"gfx":0,"other":20000,"sys":30000}"#,
        )
        .unwrap();
        match ev {
            AgentEvent::Mem { pss, java, native, code, .. } => {
                assert_eq!((pss, java, native, code), (484880, 9684, 117624, 36112));
            }
            _ => panic!("应为 Mem 事件"),
        }
    }

    /// auto-root 平台守卫（2026-09-09 策略）：车机平台自动 root，泛型 Android 不提权
    #[test]
    fn test_should_auto_root() {
        use crate::platform::PlatformId;
        assert!(should_auto_root(PlatformId::Ss2Max));
        assert!(should_auto_root(PlatformId::Ss2Pro));
        assert!(should_auto_root(PlatformId::Ss3));
        assert!(should_auto_root(PlatformId::Ss4));
        assert!(!should_auto_root(PlatformId::Android));
    }

    /// 真机显式获取 root（GUI「获取 root」按钮的后端函数）：shell→root 全迁移
    /// （adb root → adbd 重启 → 轮询 id 确认）。前置：SS2MAX 接 hppc 且当前
    /// **非 root**（`adb -s d1f39648c1f reboot` 掉 root 后跑本测试）。
    #[test]
    #[ignore = "需要 hppc + SS2MAX(d1f39648c1f)（任意权限状态均可，已 root 时幂等成功）"]
    fn test_acquire_root_hppc() {
        let target = crate::transport::SshTarget::new("hppc")
            .with_adb_path("~/Android/Sdk/platform-tools/adb");
        crate::transport::init_remote(target).expect("init_remote 失败");
        let r = acquire_root(Some("d1f39648c1f"));
        // 二次确认：返回值成功之外，设备当前 id 应为 uid=0
        let id = crate::utils::run_adb_command_for(Some("d1f39648c1f"), &["shell", "id"])
            .map(|o| o.stdout)
            .unwrap_or_default();
        crate::transport::shutdown_remote();
        assert!(r.is_ok(), "acquire_root 应成功: {:?}", r.err());
        assert!(id.contains("uid=0"), "acquire_root 后 id 应为 root: {}", id.trim());
    }

    /// hello 版本字段解析（daemon 探活的路径依赖：probe_daemon 据此判版本）
    #[test]
    fn test_hello_version() {
        let ev: AgentEvent =
            serde_json::from_str(r#"{"t":"hello","ncores":8,"maxkhz":[1785600],"version":3,"root":true}"#).unwrap();
        match ev {
            AgentEvent::Hello { ncores, maxkhz, version, root } => {
                assert_eq!((ncores, version), (8, 3));
                assert_eq!(maxkhz, vec![1785600]);
                assert!(root);
            }
            _ => panic!("应为 Hello 事件"),
        }
        // 旧版 agent（无 version/root 字段）兼容解析：version=0、root=false（按非 root 标注）
        let ev: AgentEvent = serde_json::from_str(r#"{"t":"hello","ncores":8}"#).unwrap();
        match ev {
            AgentEvent::Hello { version, root, .. } => {
                assert_eq!(version, 0);
                assert!(!root);
            }
            _ => panic!("应为 Hello 事件"),
        }
    }
}
