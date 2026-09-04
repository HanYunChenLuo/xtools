//! 设备端采样器（xperf-agent）的主机侧传输层——统一的采样引擎。
//!
//! 采样循环在设备上常驻（直接读 /proc / 本地 dumpsys），主机通过
//! `adb exec-out` 长连接读取 NDJSON 事件流，无每轮 adb 往返开销。
//! 协议见 xperf-agent/main.rs 头注释。

use crate::platform::Platform;
use anyhow::Result;
use serde::Deserialize;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

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

/// 与设备端 agent 的 exec-out 长连接流（阻塞逐行读 NDJSON 事件）
pub struct AgentStream {
    child: Child,
    reader: BufReader<std::process::ChildStdout>,
}

impl AgentStream {
    /// 阻塞读取下一行事件；流结束（agent 退出/断开）返回 Ok(None)。
    /// 解析失败的行跳过（返回 Some(Err) 由调用方决定）。
    pub fn next_event(&mut self) -> Result<Option<std::result::Result<AgentEvent, String>>> {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line)?;
        if n == 0 {
            return Ok(None); // EOF
        }
        let line = line.trim();
        if line.is_empty() {
            return self.next_event();
        }
        Ok(Some(
            serde_json::from_str(line).map_err(|e| format!("{} (行: {})", e, line)),
        ))
    }

    /// 杀掉设备端 agent 连接（kill adb 子进程并收尸；agent 侧因 stdout 写失败自行退出）
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
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

/// 探测本机 Android NDK 的 aarch64 链接器（host 感知：Linux=~/Android/Sdk/ndk，
/// macOS=~/Library/Android/sdk/ndk，或 ANDROID_NDK_HOME/ANDROID_HOME 环境变量）。
/// 返回 aarch64-linux-android26-clang 路径（无 26 时取其它 API 级别中最高者）。
fn find_ndk_linker() -> Option<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    // 直接指向 NDK 版本目录的环境变量（如 ~/Android/Sdk/ndk/25.1.8937393）
    for var in ["ANDROID_NDK_HOME", "ANDROID_NDK_ROOT", "NDK_HOME"] {
        if let Ok(v) = std::env::var(var) {
            roots.push(PathBuf::from(v));
        }
    }
    // 指向 SDK 根的环境变量 → ndk/ 子目录下可能有多版本
    let mut sdk_roots: Vec<PathBuf> = Vec::new();
    for var in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Ok(v) = std::env::var(var) {
            sdk_roots.push(PathBuf::from(v));
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        sdk_roots.push(home.join("Android/Sdk")); // Linux 默认
        sdk_roots.push(home.join("Library/Android/sdk")); // macOS 默认
    }
    for sdk in sdk_roots {
        if let Ok(rd) = std::fs::read_dir(sdk.join("ndk")) {
            let mut vers: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
            vers.sort();
            vers.reverse(); // 版本号降序，优先新版
            roots.extend(vers);
        }
    }
    // 每个候选：toolchains/llvm/prebuilt/<host-tag>/bin/aarch64-linux-android<api>-clang
    let mut fallback: Option<(u32, PathBuf)> = None;
    for ver in roots {
        let prebuilt = ver.join("toolchains/llvm/prebuilt");
        let Ok(hosts) = std::fs::read_dir(&prebuilt) else { continue };
        for host in hosts.filter_map(|e| e.ok()) {
            let bin = host.path().join("bin");
            let Ok(entries) = std::fs::read_dir(&bin) else { continue };
            for e in entries.filter_map(|e| e.ok()) {
                let name = e.file_name().to_string_lossy().to_string();
                let Some(api) = name
                    .strip_prefix("aarch64-linux-android")
                    .and_then(|s| s.strip_suffix("-clang"))
                    .and_then(|s| s.parse::<u32>().ok())
                else {
                    continue;
                };
                if api == 26 {
                    return Some(e.path()); // 与工作区基线一致，直接命中
                }
                if fallback.as_ref().is_none_or(|(a, _)| api > *a) {
                    fallback = Some((api, e.path()));
                }
            }
        }
    }
    fallback.map(|(_, p)| p)
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
/// （链接器：config.toml 默认值，探测到本机 NDK 时用 CARGO_TARGET_..._LINKER 环境变量覆盖，Mac/Linux 均可）
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
        // .cargo/config.toml 写死了 Linux NDK 路径；探测到本机 NDK 时用环境变量覆盖（优先级更高）
        if let Some(linker) = find_ndk_linker() {
            eprintln!("使用 NDK 链接器: {}", linker.display());
            cmd.env("CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER", &linker);
        }
        let status = cmd.status()?;
        if !status.success() {
            anyhow::bail!("交叉编译 xperf-agent 失败（需要 Android NDK，见 .cargo/config.toml）");
        }
    }
    Ok(bin)
}

/// 尝试 adb root（生产构建可能失败，静默忽略）。
/// 不解析 adb root 文案（各版本不同），直接 `adb shell id` 验证 uid。
fn try_adb_root() {
    let _ = crate::utils::adb().args(["root"]).output();
    // adbd 重启后等设备回来
    let _ = crate::utils::adb().args(["wait-for-device"]).output();
    let id = crate::run_adb_command(&["shell", "id"]).map(|o| o.stdout).unwrap_or_default();
    if id.contains("uid=0") {
        eprintln!("adb root: 成功（uid=0）");
    } else {
        eprintln!("adb root: 未生效（{}），IO/GPU 显存等指标不可用", id.trim());
    }
}

/// 推送 agent 到设备（设备上不存在或大小/mtime 不一致时）
/// 大小+修改时间双判：同尺寸不同版本（改代码但恰好等长）也能被更新。
pub fn deploy_agent(local: &Path) -> Result<()> {
    // 自动尝试 root（生产构建会静默失败，不影响后续流程）
    try_adb_root();
    let local_meta = std::fs::metadata(local)?;
    let local_size = local_meta.len();
    let local_mtime = local_meta.modified().ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let remote_size = crate::run_adb_command(&["shell", "stat", "-c", "%s", DEVICE_AGENT_PATH])
        .ok()
        .and_then(|o| o.stdout.trim().parse::<u64>().ok());
    let remote_mtime = crate::run_adb_command(&["shell", "stat", "-c", "%Y", DEVICE_AGENT_PATH])
        .ok()
        .and_then(|o| o.stdout.trim().parse::<u64>().ok());
    if remote_size == Some(local_size) && remote_mtime == Some(local_mtime) {
        return Ok(()); // 已是最新（大小一致）
    }
    crate::run_adb_command(&[
        "push",
        &local.to_string_lossy(),
        DEVICE_AGENT_PATH,
    ])?;
    // push 后同步 mtime 对齐本地，使下次部署的 mtime 匹配判断生效
    let _ = crate::run_adb_command(&["shell", &format!("touch -d @{} {}", local_mtime, DEVICE_AGENT_PATH)]);
    crate::run_adb_command(&["shell", "chmod", "755", DEVICE_AGENT_PATH])?;
    Ok(())
}

/// 启动设备端采样器，返回事件流。Ctrl-C/断连时 agent 因 stdout 写失败自行退出。
/// platform: 平台提示（如 "ss3"），传入时 agent 跳过对应探测
pub fn spawn_agent(
    package: Option<&str>,
    interval_ms: u64,
    flags: MetricFlags,
    platform: Option<&dyn Platform>,
) -> Result<AgentStream> {
    // setsid：agent 脱离 adb 会话——adbd 断连清理时不再被信号直杀，而是走 stdout
    // 写失败（EPIPE）路径：心跳/退出钩子得以执行（QNX 链清理等），随后自行退出。
    // 真机验证：无 setsid 时 timeout 杀 adb 后 QNX 统计链残留；加 setsid 后钩子生效。
    let mut cmd_args = vec!["exec-out".to_string(), "setsid".to_string(), DEVICE_AGENT_PATH.to_string()];
    if let Some(pkg) = package {
        cmd_args.extend(["--package".to_string(), pkg.to_string()]);
    }
    cmd_args.extend(["--interval".to_string(), interval_ms.to_string()]);
    cmd_args.extend(flags.to_agent_args());
    // 平台参数：让 agent 跳过运行时探测，直接用平台指定路径
    if let Some(p) = platform {
        cmd_args.extend(["--platform".to_string(), p.id().as_str().to_string()]);
        if let Some(qnx) = p.qnx_host() {
            cmd_args.extend(["--qnx-host".to_string(), qnx.to_string()]);
        }
    }
    let mut child = crate::utils::adb()
        .args(&cmd_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdout = child.stdout.take().expect("stdout piped");
    Ok(AgentStream {
        child,
        reader: BufReader::new(stdout),
    })
}

/// 目标设备是否在线（重连轮询用）。已选择目标设备时只认该设备
/// （`adb -s <serial> get-state` = "device"）——多台设备同连时其他设备在
/// 不算"回来了"；未选择设备时任意一台在线即可。
fn device_online() -> bool {
    let any_online = crate::utils::adb()
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
    match crate::utils::target_serial() {
        // get-state：正常输出 "device"；offline/unauthorized/serial 无效时 adb 报错（status != 0）
        Some(_) => crate::utils::adb()
            .arg("get-state")
            .output()
            .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "device")
            .unwrap_or(false),
        None => true,
    }
}

/// QNX kgsl 统计链停止（会话结束由 host 兜底执行）。
/// agent 的退出钩子只在 stdout 写失败（EPIPE）路径生效；adb 断连时 adbd 按进程树
/// 信号直杀 agent（setsid 也挡不住），钩子无机会执行。此函数经独立短 telnet 会话处理：
/// 先纯观察探测（只读 slog，不动 kgsl-control），frame 流在跑才发 echo>（死写入者）
/// 停止命令——对已停链写入会将其全部复活（真机实测 toggle 语义），故不可无条件执行。
pub fn qnx_stop_stats(platform: &dyn crate::platform::Platform, interval_ms: u64) {
    let Some(ip) = platform.qnx_host() else { return };
    // 多会话并发保护：还有其他 agent 在跑则跳过（停链会杀掉对方采样中的流）。
    // 先等自身 agent 退出（adbd 异步收尸有 1-2s 延迟，立即探测会把残留的自身
    // 误判为他人——真机实测），轮询最多 ~5s；之后计数 ≥1 即有他人。
    // `[n]` 正则防检测命令载体自匹配。
    let mut others = 0u32;
    for _ in 0..10 {
        let Ok(out) = crate::utils::adb().arg("shell").arg("pgrep -fc 'xperf-age[n]t'").output() else { return };
        others = String::from_utf8_lossy(&out.stdout).trim().parse::<u32>().unwrap_or(0);
        if others == 0 {
            break; // 自身已收尸且无他人
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    if others >= 1 {
        return; // 有其他会话在采样，链留给对方退出时收
    }
    let period = interval_ms.clamp(100, 1000);
    // 探测：~5s 纯观察（只读 slog 不写 kgsl-control，无副作用）
    let probe = format!(
        "({{ sleep 1; echo root; sleep 1; echo 'slog2info -W | grep frame &'; sleep 3; }} | busybox telnet {})",
        ip
    );
    let Ok(out) = crate::utils::adb().arg("shell").arg(&probe).output() else { return };
    let flowing = String::from_utf8_lossy(&out.stdout).matches("frame ").count();
    if flowing < 2 {
        return; // 链未在流（agent 退出钩子已清理 / 本就无链）：不写，死写入者撞停链会复活
    }
    // 停链：echo>（死写入者）式写入对流链 = 停止全部（真机实测，fd3 活连接存在时亦有效）
    let kill = format!(
        "({{ sleep 1; echo root; sleep 1; echo 'echo gpubusystats {} > /dev/kgsl-control'; sleep 2; }} | busybox telnet {})",
        period, ip
    );
    let _ = crate::utils::adb().arg("shell").arg(&kill).output();
}

/// 断连恢复：事件流 EOF（adb 长连接断开 / agent 进程退出）后调用。
/// 每 500ms 轮询设备状态，设备回来后重新部署并启动 agent。
/// `is_running` 返回 false（用户停止 / Ctrl-C）时返回 None；重连成功返回新事件流。
/// 调用方持有的采样状态（时序、峰值等）不受影响，新 agent 的首轮仅重建基线。
pub fn reconnect_agent(
    package: Option<&str>,
    interval_ms: u64,
    flags: MetricFlags,
    platform: Option<&dyn Platform>,
    is_running: &dyn Fn() -> bool,
) -> Option<AgentStream> {
    loop {
        if !is_running() {
            return None;
        }
        if device_online() {
            match ensure_agent_built()
                .and_then(|bin| deploy_agent(&bin))
                .and_then(|_| spawn_agent(package, interval_ms, flags, platform))
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
    fn test_find_ndk_linker() {
        // 装了 NDK 的机器（如本开发机）必须能探测到 android26-clang；没装则跳过
        let has_ndk = PathBuf::from("/home/han/Android/Sdk/ndk/25.1.8937393").exists();
        if has_ndk {
            let linker = find_ndk_linker().expect("本机有 NDK，必须探测到链接器");
            assert!(linker.ends_with("aarch64-linux-android26-clang"), "命中: {}", linker.display());
        }
    }



    #[test]
    fn test_parse_hello() {
        let ev: AgentEvent = serde_json::from_str(r#"{"t":"hello","ncores":8,"version":1}"#).unwrap();
        assert!(matches!(ev, AgentEvent::Hello { ncores: 8, .. }));
        // 新版带 maxkhz
        let ev: AgentEvent =
            serde_json::from_str(r#"{"t":"hello","ncores":8,"maxkhz":[2841600,2841600],"version":1}"#).unwrap();
        match ev {
            AgentEvent::Hello { ncores, maxkhz } => assert_eq!((ncores, maxkhz), (8, vec![2841600, 2841600])),
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
}
