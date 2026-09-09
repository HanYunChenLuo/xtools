use anyhow::{Context, Result};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::Mutex;

// 全局静态变量，用于跟踪中断状态
static INTERRUPT_FLAG: AtomicBool = AtomicBool::new(false);

/// 子进程输出包装
pub struct ProcOutput {
    /// 子进程 stdout（已清洗 ANSI 控制字符）。
    pub stdout: String,
}

/// 执行子进程，返回 stdout。
///
/// 仅当子进程无法启动时返回 `Err`；退出码非零不返回 `Err`，`stdout` 照常返回。
pub fn run_command(program: &str, args: &[&str]) -> Result<ProcOutput> {
    run_command_inner(Command::new(program).args(args), program)
}

fn run_command_inner(cmd: &mut Command, label: &str) -> Result<ProcOutput> {
    let output = cmd
        .env("TERM", "dumb")
        .output()
        .with_context(|| format!("Failed to execute command: {}", label))?;

    Ok(ProcOutput {
        stdout: clean_control_chars(&String::from_utf8_lossy(&output.stdout)),
    })
}

/// 构造 adb 命令。本机恒为 adb **客户端**；远程模式经环境变量指向远端 server
/// （`ADB_SERVER_SOCKET`，见 `crate::transport`；环境变量不参与 argv 顺序，
/// 不与调用方追加的 `-s`/子命令位置冲突——这是不用 `-H/-P` 参数的原因）。
fn adb_command() -> Command {
    let mut c = Command::new("adb"); // 恒本机 adb 二进制
    if matches!(crate::transport::transport(), crate::transport::Transport::Ssh(_)) {
        if let Some(p) = crate::transport::tunnel_server_port() {
            c.env("ADB_SERVER_SOCKET", format!("tcp:127.0.0.1:{p}"));
        }
    }
    c
}

/// 执行 adb 命令（经 `adb_command` 构造：远程模式自动指向远端 server，
/// 本机模式与改造前逐字节一致）。语义同 [`run_command`]。
pub fn run_adb(args: &[&str]) -> Result<ProcOutput> {
    run_command_inner(adb_command().args(args), "adb")
}

/// 执行 adb 命令，显式指定目标设备（多设备并行会话用）。
///
/// `serial`：`Some(s)` 注入 `-s s`（空串视同 `None`）；`None` 回退全局选择
/// （[`target_serial`]）。
pub fn run_adb_command_for(serial: Option<&str>, args: &[&str]) -> Result<ProcOutput> {
    match resolve_serial(serial) {
        Some(serial) => {
            let full: Vec<String> = std::iter::once("-s".to_string())
                .chain(std::iter::once(serial))
                .chain(args.iter().map(|s| s.to_string()))
                .collect();
            let refs: Vec<&str> = full.iter().map(|s| s.as_str()).collect();
            run_adb(&refs)
        }
        None => run_adb(args),
    }
}

// ---------- 目标设备选择（多设备 -s 注入）----------

/// 当前目标设备 serial（全局）。`None` = 未选择，adb 命令不带 `-s`
/// （仅单台设备连接时可正常工作；多台时 adb 报 "more than one device"）。
/// CLI 启动时经 `--device`/自动检测写入；GUI 由 `select_device` 命令写入。
static TARGET_SERIAL: Mutex<Option<String>> = Mutex::new(None);

/// 设置目标设备 serial（`None` 清除选择）。会话间可重复调用（GUI 切换设备）。
pub fn set_target_serial(serial: Option<String>) {
    if let Ok(mut guard) = TARGET_SERIAL.lock() {
        *guard = serial;
    }
}

/// 当前目标设备 serial（`None` = 未选择）
pub fn target_serial() -> Option<String> {
    TARGET_SERIAL.lock().ok().and_then(|g| g.clone())
}

/// 构造已注入 `-s <serial>` 的 adb 命令，显式指定目标设备（多设备并行会话用）。
///
/// `serial`：`Some(s)` 注入 `-s s`（空串视同 `None`）；`None` 回退全局选择
/// （[`target_serial`]）。基于 `adb_command` 构造，远程模式自动携带
/// `ADB_SERVER_SOCKET`。所有 adb 调用统一经此构造，保证多设备场景命令路由到目标设备。
pub fn adb_for(serial: Option<&str>) -> Command {
    let mut c = adb_command();
    if let Some(s) = resolve_serial(serial) {
        c.args(["-s", &s]);
    }
    c
}

/// 解析会话 serial：`Some(非空)` 原样返回（owned）；`None`/空串回退全局选择
/// （[`target_serial`]）。core 各多设备入口（`spawn_agent`/`trace::record` 等）
/// 的 `serial: Option<&str>` 参数统一经此归一。
pub fn resolve_serial(serial: Option<&str>) -> Option<String> {
    match serial {
        Some(s) if !s.is_empty() => Some(s.to_string()),
        _ => target_serial(),
    }
}

/// `adb devices -l` 解析出的一台在线设备
#[derive(Debug, Clone, PartialEq)]
pub struct AdbDevice {
    /// 设备 serial（`-s` 参数值 / 输出行首 token）
    pub serial: String,
    /// product 字段（如 `HU_SS3`、`dada`；缺失为空）
    pub product: String,
    /// model 字段（展示用；缺失为空）
    pub model: String,
    /// Android 版本（`ro.build.version.release`，如 `"14"`；`parse_adb_devices`
    /// 纯解析不填，恒为空串，`list_adb_devices` 逐台补齐；取失败为 `?`）。
    /// 采集方式与 Android 版本相关（如 BLAST 合成层是 12+ 特性），供选路参考。
    pub android_version: String,
}

/// 解析 `adb devices -l` 输出为在线设备列表（跳过 `offline`/`unauthorized` 行）。
/// 输出格式：`<serial>\tdevice usb:… product:<p> model:<m> device:<d> transport_id:<n>`
pub fn parse_adb_devices(output: &str) -> Vec<AdbDevice> {
    let mut out = Vec::new();
    for line in output.lines().skip(1) {
        let mut fields = line.split_whitespace();
        let (Some(serial), Some(state)) = (fields.next(), fields.next()) else {
            continue;
        };
        if state != "device" {
            continue; // offline / unauthorized / recovery：不可用
        }
        let field = |name: &str| {
            line.split(&format!("{}:", name))
                .nth(1)
                .and_then(|s| s.split_whitespace().next())
                .unwrap_or("")
                .to_string()
        };
        out.push(AdbDevice {
            serial: serial.to_string(),
            product: field("product"),
            model: field("model"),
            android_version: String::new(),
        });
    }
    out
}

/// 拉取在线设备列表（`adb devices -l`；`-s` 对 `devices` 子命令无效，恒列全部），
/// 并逐台 `getprop ro.build.version.release` 补 Android 版本（采集方式与版本相关，
/// 如 BLAST 合成层是 12+ 特性；取失败标 `?`）
pub fn list_adb_devices() -> Result<Vec<AdbDevice>> {
    let out = run_adb(&["devices", "-l"]).context("执行 adb devices -l 失败")?;
    let mut devices = parse_adb_devices(&out.stdout);
    for d in &mut devices {
        let ver = run_adb(&["-s", &d.serial, "shell", "getprop", "ro.build.version.release"])
            .ok()
            .map(|o| o.stdout.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "?".to_string());
        d.android_version = ver;
    }
    Ok(devices)
}

/// 设备选择策略：`preferred`（须在在线列表中）> 单台自动 > 多台报错（错误信息带设备清单）。
/// 返回选中的设备；由调用方负责 `set_target_serial(Some(serial))` 生效。
pub fn pick_device(preferred: Option<&str>, devices: &[AdbDevice]) -> Result<AdbDevice> {
    if let Some(p) = preferred {
        return devices
            .iter()
            .find(|d| d.serial == p)
            .cloned()
            .context(format!("指定设备 {} 不在线（当前在线：{}）", p, devices.iter().map(|d| d.serial.as_str()).collect::<Vec<_>>().join(", ")));
    }
    match devices.len() {
        0 => anyhow::bail!("无 adb 设备在线（adb devices 为空）"),
        1 => Ok(devices[0].clone()),
        _ => anyhow::bail!(
            "多台设备连接，请用 --device <serial> 指定（GUI 在侧栏设备下拉选择）：\n  {}",
            devices.iter().map(|d| format!("{}（model: {}，Android {}）", d.serial, d.model, d.android_version)).collect::<Vec<_>>().join("\n  ")
        ),
    }
}

/// 两份设备快照的差异（热插拔检测用）：`(接入, 移除)` 各自为 serial 列表。
/// 顺序保持新/旧快照中的出现顺序。
pub fn diff_devices(old: &[AdbDevice], new: &[AdbDevice]) -> (Vec<String>, Vec<String>) {
    let added = new
        .iter()
        .filter(|d| !old.iter().any(|o| o.serial == d.serial))
        .map(|d| d.serial.clone())
        .collect();
    let removed = old
        .iter()
        .filter(|o| !new.iter().any(|d| d.serial == o.serial))
        .map(|o| o.serial.clone())
        .collect();
    (added, removed)
}

fn clean_control_chars(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\x1B' && chars.peek() == Some(&'[') {
            chars.next();
            while let Some(&next) = chars.peek() {
                if next.is_ascii_alphabetic() {
                    chars.next();
                    break;
                }
                chars.next();
            }
            continue;
        }
        result.push(c);
    }
    result
}

/// 置位全局 Ctrl-C 中断标志（ctrlc handler 调用；长阻塞循环轮询 is_interrupted 提前退出）
pub fn set_interrupt_flag() {
    INTERRUPT_FLAG.store(true, AtomicOrdering::SeqCst);
}

/// 是否已收到 Ctrl-C（set_interrupt_flag 置位）
pub fn is_interrupted() -> bool {
    INTERRUPT_FLAG.load(AtomicOrdering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- clean_control_chars：ANSI CSI 转义序列清洗 ----

    #[test]
    fn test_clean_control_chars_strips_color_codes() {
        let input = "\x1B[31mred text\x1B[0m";
        assert_eq!(clean_control_chars(input), "red text");
    }

    #[test]
    fn test_clean_control_chars_strips_multiple_codes() {
        let input = "\x1B[1;32mbold green\x1B[0m and \x1B[33myellow\x1B[0m";
        assert_eq!(clean_control_chars(input), "bold green and yellow");
    }

    #[test]
    fn test_clean_control_chars_no_escape_passes_through() {
        assert_eq!(clean_control_chars("plain text"), "plain text");
        assert_eq!(clean_control_chars(""), "");
    }

    #[test]
    fn test_clean_control_chars_preserves_other_control_chars() {
        assert_eq!(clean_control_chars("line1\nline2\ttab"), "line1\nline2\ttab");
    }

    #[test]
    fn test_clean_control_chars_strips_cursor_movement() {
        let input = "\x1B[2K\x1B[Hhello";
        assert_eq!(clean_control_chars(input), "hello");
    }

    // ---- pidof split 逻辑 ----

    #[test]
    fn test_pidof_multi_pid_split() {
        let stdout = "1119 16071\n";
        let pids: Vec<&str> = stdout.split_whitespace().collect();
        assert_eq!(pids, vec!["1119", "16071"]);
    }

    #[test]
    fn test_pidof_single_pid_split() {
        let stdout = "15803\n";
        let pids: Vec<&str> = stdout.split_whitespace().collect();
        assert_eq!(pids, vec!["15803"]);
    }

    #[test]
    fn test_pidof_empty_split() {
        let stdout = "\n";
        let pids: Vec<&str> = stdout.split_whitespace().collect();
        assert!(pids.is_empty());
    }

    // ---- 多设备 -s 注入 ----

    #[test]
    fn test_parse_adb_devices() {
        let output = "List of devices attached\n\
            1280da60               device usb:1-2 product:dada model:24129PN74C device:dada transport_id:10\n\
            6eb792dfb0f            device usb:1-13 product:HU_SS3 model:HU_SS3 device:HU_SS3 transport_id:9\n\
            deadbeef               offline usb:1-4 transport_id:11\n";
        let devices = parse_adb_devices(output);
        assert_eq!(devices.len(), 2); // offline 行跳过
        assert_eq!(
            devices[0],
            AdbDevice { serial: "1280da60".into(), product: "dada".into(), model: "24129PN74C".into(), android_version: String::new() }
        );
        assert_eq!(devices[1].serial, "6eb792dfb0f");
        assert_eq!(devices[1].product, "HU_SS3");
        assert!(parse_adb_devices("List of devices attached\n").is_empty());
    }

    #[test]
    fn test_pick_device() {
        let devices = parse_adb_devices(
            "List of devices attached\n1280da60 device usb:1-2 product:dada model:m1\n6eb792dfb0f device usb:1-13 product:HU_SS3 model:m2\n",
        );
        // 指定且在线
        assert_eq!(pick_device(Some("6eb792dfb0f"), &devices).unwrap().serial, "6eb792dfb0f");
        // 指定但不在线
        let e = pick_device(Some("xxx"), &devices).unwrap_err().to_string();
        assert!(e.contains("xxx 不在线"));
        // 多台未指定 → 报错并列出设备
        let e = pick_device(None, &devices).unwrap_err().to_string();
        assert!(e.contains("多台设备连接"));
        assert!(e.contains("1280da60") && e.contains("6eb792dfb0f"));
        // 单台自动
        let single = parse_adb_devices("List of devices attached\nabc device product:p model:m\n");
        assert_eq!(pick_device(None, &single).unwrap().serial, "abc");
        // 无设备报错
        assert!(pick_device(None, &[]).is_err());
    }

    // ---- 热插拔 diff ----

    fn dev(serial: &str) -> AdbDevice {
        AdbDevice { serial: serial.into(), product: String::new(), model: String::new(), android_version: String::new() }
    }

    // ---- adb_command 传输注入（S3）----
    // 读写 TRANSPORT/TUNNEL 全局 ⇒ 全程持 TRANSPORT_TEST_LOCK 防并行互踩

    #[test]
    fn test_adb_command_transport_env() {
        use crate::transport::{
            install_tunnel, clear_tunnel, set_transport, SshTarget, SshTunnel, Transport,
            TRANSPORT_TEST_LOCK,
        };
        use std::ffi::OsStr;
        const ENV_KEY: &str = "ADB_SERVER_SOCKET";
        let _serial = TRANSPORT_TEST_LOCK.lock().unwrap();

        // Local（默认）：不注入 ADB_SERVER_SOCKET（与本机既有行为逐字节一致）
        set_transport(Transport::Local);
        let c = adb_command();
        assert!(
            c.get_envs().all(|(k, _)| k != ENV_KEY),
            "Local 模式不应注入 ADB_SERVER_SOCKET"
        );

        // Ssh + 隧道：注入 tcp:127.0.0.1:<hop#1 端口>
        set_transport(Transport::Ssh(SshTarget::new("hppc")));
        install_tunnel(SshTunnel::for_test(54321));
        let c = adb_command();
        let v = c
            .get_envs()
            .find(|&(k, _)| k == OsStr::new(ENV_KEY))
            .and_then(|(_, v)| v)
            .expect("Ssh 模式应注入 ADB_SERVER_SOCKET");
        assert_eq!(v, "tcp:127.0.0.1:54321");

        // adb_for：env 注入与 -s 参数共存不冲突
        let c = adb_for(Some("dev1"));
        assert!(c.get_envs().any(|(k, _)| k == ENV_KEY));

        // 复位
        clear_tunnel();
        set_transport(Transport::Local);
        let c = adb_command();
        assert!(c.get_envs().all(|(k, _)| k != ENV_KEY));
    }

    #[test]
    fn test_diff_devices() {
        // 无变化
        let a = vec![dev("x"), dev("y")];
        assert_eq!(diff_devices(&a, &a), (vec![], vec![]));
        // 接入
        assert_eq!(
            diff_devices(&[dev("x")], &[dev("x"), dev("y")]),
            (vec!["y".to_string()], vec![])
        );
        // 移除
        assert_eq!(
            diff_devices(&[dev("x"), dev("y")], &[dev("y")]),
            (vec![], vec!["x".to_string()])
        );
        // 同时接入+移除
        assert_eq!(
            diff_devices(&[dev("x")], &[dev("y")]),
            (vec!["y".to_string()], vec!["x".to_string()])
        );
        // 空 ↔ 空
        assert_eq!(diff_devices(&[], &[]), (vec![], vec![]));
    }
}
