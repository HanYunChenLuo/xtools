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
/// 测试可通过 `set_adb_runner_for_test` 注入 mock 实现，避免真实拉起 adb 子进程。
pub fn run_adb_command(args: &[&str]) -> Result<ProcOutput> {
    if let Ok(guard) = ADB_RUNNER_OVERRIDE.lock() {
        if let Some(runner) = *guard {
            return runner(args);
        }
    }
    run_command("adb", args)
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
}
