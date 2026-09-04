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
    let output = Command::new(program)
        .args(args)
        .env("TERM", "dumb")
        .output()
        .with_context(|| format!("Failed to execute command: {}", program))?;

    Ok(ProcOutput {
        stdout: clean_control_chars(&String::from_utf8_lossy(&output.stdout)),
    })
}

/// 执行 adb 命令。`run_command` 的薄封装。
///
/// 已选择目标设备（`set_target_serial`）时自动在参数前注入 `-s <serial>`，
/// 保证多设备场景路由到目标设备。
/// 测试可通过 `set_adb_runner_for_test` 注入 mock 实现，避免真实拉起 adb 子进程
/// （mock 收到的是注入后的完整参数序列，未选择设备时与调用方原始参数一致）。
pub fn run_adb_command(args: &[&str]) -> Result<ProcOutput> {
    if let Ok(guard) = ADB_RUNNER_OVERRIDE.lock() {
        if let Some(runner) = *guard {
            return runner(args);
        }
    }
    match target_serial() {
        Some(serial) => {
            let full: Vec<String> = std::iter::once("-s".to_string())
                .chain(std::iter::once(serial))
                .chain(args.iter().map(|s| s.to_string()))
                .collect();
            let refs: Vec<&str> = full.iter().map(|s| s.as_str()).collect();
            run_command("adb", &refs)
        }
        None => run_command("adb", args),
    }
}

/// adb 命令执行器的类型（函数指针，不捕获外部状态，按 args 分支返回）。
pub type AdbRunner = fn(&[&str]) -> Result<ProcOutput>;

static ADB_RUNNER_OVERRIDE: Mutex<Option<AdbRunner>> = Mutex::new(None);

/// 测试串行锁：注入 mock adb runner 的测试共享全局状态（ADB_RUNNER_OVERRIDE，
/// 以及 cpu.rs 的 MOCK_PHASE 等模块级标志），并行执行会互相覆盖导致偶发失败。
/// 所有调用 set_adb_runner_for_test 的测试必须全程持有此锁
/// （async 测试 `.lock().await`，sync 测试 `.blocking_lock()`）。
#[cfg(test)]
pub static ADB_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 注入 mock adb 执行器，仅用于单元测试。
/// 可重复调用（覆盖前一次设置）；测试间共享全局状态，调用方须持有 ADB_TEST_LOCK。
#[cfg(test)]
pub fn set_adb_runner_for_test(runner: AdbRunner) {
    if let Ok(mut guard) = ADB_RUNNER_OVERRIDE.lock() {
        *guard = Some(runner);
    }
}

/// 清除 mock adb 执行器，恢复真实 adb 调用，仅用于单元测试。
#[cfg(test)]
pub fn clear_adb_runner_for_test() {
    if let Ok(mut guard) = ADB_RUNNER_OVERRIDE.lock() {
        *guard = None;
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

/// 构造已注入 `-s <serial>` 的 adb 命令（未选择设备时不注入）。
/// 所有 adb 调用统一经此构造，保证多设备场景命令路由到目标设备。
pub fn adb() -> Command {
    let mut c = Command::new("adb");
    if let Some(s) = target_serial() {
        c.args(["-s", &s]);
    }
    c
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
        });
    }
    out
}

/// 拉取在线设备列表（`adb devices -l`；`-s` 对 `devices` 子命令无效，恒列全部）
pub fn list_adb_devices() -> Result<Vec<AdbDevice>> {
    let out = run_command("adb", &["devices", "-l"]).context("执行 adb devices -l 失败")?;
    Ok(parse_adb_devices(&out.stdout))
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
            devices.iter().map(|d| format!("{}（model: {}）", d.serial, d.model)).collect::<Vec<_>>().join("\n  ")
        ),
    }
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
            AdbDevice { serial: "1280da60".into(), product: "dada".into(), model: "24129PN74C".into() }
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
}
