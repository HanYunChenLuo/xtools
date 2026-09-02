use anyhow::{Context, Result};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::Mutex;

// 全局静态变量，用于跟踪中断状态
static INTERRUPT_FLAG: AtomicBool = AtomicBool::new(false);

#[derive(Debug)]
pub struct ProcessInfo {
    pub pid: String,
    pub start_time: String,
}

pub fn check_adb_connection() -> bool {
    if let Ok(output) = Command::new("adb").arg("devices").output() {
        if output.status.success() {
            let devices = String::from_utf8_lossy(&output.stdout);
            return devices.lines().skip(1).any(|line| !line.trim().is_empty());
        }
    }
    false
}

/// 获取包名下所有进程（cmdline 等于包名的进程，多进程应用可能返回多个）。
/// pidof 在进程不存在时退出码非零且 stdout 为空，靠 stdout 是否为空判断。
pub fn get_all_processes(package: &str) -> Result<Vec<ProcessInfo>> {
    let output = run_adb_command(&["shell", "pidof", package])?;
    let pids: Vec<&str> = output.stdout.split_whitespace().collect();
    if pids.is_empty() {
        anyhow::bail!("Process not found for package: {}", package);
    }
    let mut processes = Vec::with_capacity(pids.len());
    for pid in pids {
        let start_time = run_adb_command(&[
            "shell",
            "stat",
            "-c",
            "%y",
            format!("/proc/{}/cmdline", pid).as_str(),
        ])?;
        processes.push(ProcessInfo {
            pid: pid.to_string(),
            start_time: start_time.stdout.trim().to_string(),
        });
    }
    Ok(processes)
}

/// 子进程执行结果。
///
/// 注意区分两种"失败"：
/// - **子进程无法启动**：`run_command` 返回 `Err`。
/// - **子进程退出码非零**：`stdout` 仍可能含有效内容。例如 `cat` 部分文件缺失、
///   `pidof` 找不到进程、`grep` 未命中都会返回非零退出码，但 stdout 照常返回。
///   调用方按语义判断：只需 stdout 内容时直接用 `stdout`。
///
/// 后续如需严格判断退出码或诊断 stderr，可在此结构体补充字段：
///   `success: bool`（退出码是否为 0）、`exit_code: i32`、`stderr: String`。
#[derive(Debug, Clone)]
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

pub fn set_interrupt_flag() {
    INTERRUPT_FLAG.store(true, AtomicOrdering::SeqCst);
}

pub fn is_being_interrupted() -> bool {
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

    // ---- get_all_processes（注入 mock adb runner）----

    fn mock_runner_for_get_all_processes(args: &[&str]) -> Result<ProcOutput> {
        if args.len() >= 3 && args[0] == "shell" && args[1] == "pidof" {
            return Ok(ProcOutput {
                stdout: "1119 16071\n".to_string(),
            });
        }
        if args.len() >= 5 && args[1] == "stat" {
            let path = args[4];
            if let Some(pid_start) = path.find("/proc/") {
                let rest = &path[pid_start + 6..];
                if let Some(pid_end) = rest.find('/') {
                    let pid = &rest[..pid_end];
                    return Ok(ProcOutput {
                        stdout: format!("2026-09-01 10:00:00.000000000 +0800 pid={}\n", pid),
                    });
                }
            }
        }
        Ok(ProcOutput { stdout: String::new() })
    }

    #[test]
    fn test_get_all_processes_multi_pid() {
        let _lock = ADB_TEST_LOCK.blocking_lock();
        set_adb_runner_for_test(mock_runner_for_get_all_processes);
        let procs = get_all_processes("com.lixiang.car.browser").unwrap();
        assert_eq!(procs.len(), 2);
        assert_eq!(procs[0].pid, "1119");
        assert_eq!(procs[1].pid, "16071");
        assert!(procs[0].start_time.contains("pid=1119"));
        assert!(procs[1].start_time.contains("pid=16071"));
        clear_adb_runner_for_test();
    }

    #[test]
    fn test_get_all_processes_single_pid() {
        fn single(args: &[&str]) -> Result<ProcOutput> {
            if args[1] == "pidof" {
                return Ok(ProcOutput { stdout: "15803\n".to_string() });
            }
            Ok(ProcOutput { stdout: "2026-09-01 10:00:00 +0800\n".to_string() })
        }
        let _lock = ADB_TEST_LOCK.blocking_lock();
        set_adb_runner_for_test(single);
        let procs = get_all_processes("com.x").unwrap();
        assert_eq!(procs.len(), 1);
        assert_eq!(procs[0].pid, "15803");
        clear_adb_runner_for_test();
    }

    #[test]
    fn test_get_all_processes_not_found() {
        fn empty(_args: &[&str]) -> Result<ProcOutput> {
            Ok(ProcOutput { stdout: "\n".to_string() })
        }
        let _lock = ADB_TEST_LOCK.blocking_lock();
        set_adb_runner_for_test(empty);
        let err = get_all_processes("com.nonexistent").unwrap_err();
        assert!(
            err.to_string().contains("Process not found"),
            "应报 Process not found，实际: {}",
            err
        );
        clear_adb_runner_for_test();
    }
}
