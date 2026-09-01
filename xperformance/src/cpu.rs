use crate::utils;
use anyhow::Result;
use chrono::{DateTime, Local};
use colored::*;
use std::cmp::Ordering;

// 定义线程CPU使用信息结构体
#[derive(Debug, Clone)]
pub struct ThreadCpuInfo {
    pub tid: String,
    pub cpu_usage: f32,
    pub name: String,
    pub timestamp: Option<DateTime<Local>>,
}

// 实现比较特性以便在最大堆中使用
impl PartialEq for ThreadCpuInfo {
    fn eq(&self, other: &Self) -> bool {
        self.cpu_usage == other.cpu_usage
    }
}

impl Eq for ThreadCpuInfo {}

impl PartialOrd for ThreadCpuInfo {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ThreadCpuInfo {
    fn cmp(&self, other: &Self) -> Ordering {
        self.cpu_usage
            .partial_cmp(&other.cpu_usage)
            .unwrap_or(Ordering::Equal)
            .reverse()
    }
}

/// 从 /proc/stat 的第一行解析系统总 CPU jiffies
/// 格式: cpu  user nice system idle iowait irq softirq steal guest guest_nice
fn parse_total_cpu_jiffies(stat_output: &str) -> Option<u64> {
    let line = stat_output.lines().find(|l| l.starts_with("cpu "))?;
    let total: u64 = line
        .split_whitespace()
        .skip(1) // 跳过 "cpu" 标签
        .map(|v| v.parse::<u64>().unwrap_or(0))
        .sum();
    Some(total)
}

/// 从 /proc/<pid>/stat 解析进程/线程的 CPU jiffies (utime + stime)
/// stat 文件第14列=utime, 第15列=stime（1-indexed）
fn parse_proc_stat_jiffies(stat_output: &str) -> Option<u64> {
    // stat 文件只有一行，但进程名可能含空格和括号，需特殊处理
    // 格式: pid (comm) state ppid ... utime stime ...
    // 找到最后一个 ')' 后从第3个字段开始计数
    let line = stat_output.trim();
    let paren_end = line.rfind(')')?;
    let after_comm = &line[paren_end + 1..];
    let fields: Vec<&str> = after_comm.split_whitespace().collect();
    // after_comm 中: [0]=state [1]=ppid [2]=pgrp [3]=session [4]=tty_nr
    //               [5]=tpgid [6]=flags [7]=minflt [8]=cminflt [9]=majflt
    //               [10]=cmajflt [11]=utime [12]=stime ...
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some(utime + stime)
}

/// 从 /proc/<pid>/task/<tid>/comm 读取线程名
fn parse_thread_name(comm_output: &str) -> String {
    comm_output.trim().to_string()
}

/// 读取所有线程 TID 列表
fn parse_tid_list(ls_output: &str) -> Vec<String> {
    ls_output
        .split_whitespace()
        .filter(|s| s.chars().all(|c| c.is_ascii_digit()))
        .map(|s| s.to_string())
        .collect()
}

/// 批量读取指定 TID 列表对应的 /proc/<pid>/task/<tid>/<suffix> 内容，
/// 返回 (tid, 该文件首行内容) 的映射。
///
/// 关键设计：不使用 `cat file1 file2 ...`（某文件不存在时 cat 退出码非零、
/// 且 stdout 行数会少于文件数，导致 tid 与内容错位/丢失），而是用 shell 循环
/// 逐个输出 `<tid>:<content>`，单文件缺失只丢该行，tid 与内容始终绑定。
///
/// suffix: "stat" 或 "comm"
fn read_thread_files(
    pid: &str,
    tids: &[String],
    suffix: &str,
) -> std::collections::HashMap<String, String> {
    if tids.is_empty() {
        return std::collections::HashMap::new();
    }
    // 对每个 tid 输出 "tid:首行内容"；文件不存在时输出空内容行 "tid:"，
    // 2>/dev/null 吞掉 cat 的报错，保证整条命令退出码为 0。
    let mut script = String::from("for t in ");
    for tid in tids {
        script.push_str(tid);
        script.push(' ');
    }
    script.push_str(&format!(
        "; do echo \"$t:$(head -n1 /proc/{}/task/$t/{} 2>/dev/null)\"; done",
        pid, suffix
    ));
    let output = match utils::run_adb_command(&["shell", &script]) {
        Ok(o) => o.stdout,
        Err(_) => return std::collections::HashMap::new(),
    };
    parse_thread_files_output(&output)
}

/// 解析 shell 循环输出的 `<tid>:<content>` 行为 (tid, content) 映射。
///
/// - 仅接受纯数字 tid（避免误解析 shell 报错行等）。
/// - 用 `split_once(':')` 在第一个冒号分割，content 即便含冒号也完整保留
///   （如线程名 `Binder:15803_3`）。
/// - 文件不存在的行输出 `tid:`（空 content），tid 仍被记录（content 为空），
///   由调用方决定如何处理空 content（如 parse_proc_stat_jiffies 会过滤）。
fn parse_thread_files_output(output: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for line in output.lines() {
        if let Some((tid, content)) = line.split_once(':') {
            if !tid.is_empty() && tid.chars().all(|c| c.is_ascii_digit()) {
                map.insert(tid.to_string(), content.to_string());
            }
        }
    }
    map
}

/// 第一次采样的中间状态：持有系统/进程/线程的 jiffies + 线程列表。
/// sleep 窗口在 phase1 与 phase2 之间由主循环统一调度（多 PID 共享一个窗口）。
#[derive(Debug)]
pub struct CpuSample1 {
    pid: String,
    tids: Vec<String>,
    sys_total: u64,
    proc_jiffies: u64,
    thread_jiffies: std::collections::HashMap<String, u64>,
}

/// 第一次采样：读取系统总 jiffies、进程 jiffies、所有线程 jiffies + 线程列表。
pub async fn sample_cpu_phase1(pid: &str) -> Result<CpuSample1> {
    // 线程列表
    let tid_list_output =
        utils::run_adb_command(&["shell", "ls", &format!("/proc/{}/task", pid)])?.stdout;
    let tids = parse_tid_list(&tid_list_output);

    // 系统总 jiffies
    let sys_stat = utils::run_adb_command(&["shell", "cat", "/proc/stat"])?.stdout;
    let sys_total = parse_total_cpu_jiffies(&sys_stat)
        .ok_or_else(|| anyhow::format_err!("无法解析 /proc/stat"))?;

    // 进程 jiffies
    let proc_stat = utils::run_adb_command(&["shell", "cat", &format!("/proc/{}/stat", pid)])?
        .stdout;
    // stdout 为空说明进程已不存在（cat 退出码非零、无输出），返回 "Process not found"
    // 以便主循环的重启检测（匹配该字符串）能正确标记该 PID 失活。
    if proc_stat.trim().is_empty() {
        anyhow::bail!("Process not found for pid: {}", pid);
    }
    let proc_jiffies = parse_proc_stat_jiffies(&proc_stat)
        .ok_or_else(|| anyhow::format_err!("无法解析 /proc/{}/stat", pid))?;

    // 批量读取所有线程的第一次 jiffies（tid 与内容绑定，单线程退出不影响其余）
    let thread_jiffies: std::collections::HashMap<String, u64> = read_thread_files(pid, &tids, "stat")
        .into_iter()
        .filter_map(|(tid, content)| parse_proc_stat_jiffies(&content).map(|j| (tid, j)))
        .collect();

    Ok(CpuSample1 {
        pid: pid.to_string(),
        tids,
        sys_total,
        proc_jiffies,
        thread_jiffies,
    })
}

/// 第二次采样：再读一次 jiffies，结合 phase1 结果算出进程/线程 CPU%。
/// timestamp 取第二次采样时刻。
pub async fn sample_cpu_phase2(
    phase1: &CpuSample1,
) -> Result<(f32, DateTime<Local>, Vec<ThreadCpuInfo>)> {
    let pid = &phase1.pid;
    let timestamp = Local::now();

    let sys_stat2 = utils::run_adb_command(&["shell", "cat", "/proc/stat"])?.stdout;
    let proc_stat2 = utils::run_adb_command(&["shell", "cat", &format!("/proc/{}/stat", pid)])?
        .stdout;
    if proc_stat2.trim().is_empty() {
        anyhow::bail!("Process not found for pid: {}", pid);
    }

    let thread_stats2: std::collections::HashMap<String, u64> = read_thread_files(pid, &phase1.tids, "stat")
        .into_iter()
        .filter_map(|(tid, content)| parse_proc_stat_jiffies(&content).map(|j| (tid, j)))
        .collect();

    // 批量读取线程名（comm 文件，变化少，每轮读一次即可）
    let thread_names: std::collections::HashMap<String, String> =
        read_thread_files(pid, &phase1.tids, "comm")
            .into_iter()
            .map(|(tid, content)| (tid, parse_thread_name(&content)))
            .collect();

    // 系统总 CPU delta
    let total2 = parse_total_cpu_jiffies(&sys_stat2)
        .ok_or_else(|| anyhow::format_err!("无法解析 /proc/stat"))?;
    let total_delta = total2.saturating_sub(phase1.sys_total) as f32;

    if total_delta == 0.0 {
        anyhow::bail!("CPU delta 为零，采样间隔过短");
    }

    // 进程 CPU%
    let proc2 = parse_proc_stat_jiffies(&proc_stat2)
        .ok_or_else(|| anyhow::format_err!("无法解析 /proc/{}/stat", pid))?;
    let process_cpu = (proc2.saturating_sub(phase1.proc_jiffies) as f32 / total_delta) * 100.0;

    // 每个线程的 CPU%：仅对两次采样都存在的线程求差，
    // 采样窗口内创建/退出的线程直接跳过（无法可靠计算 delta）。
    let mut threads: Vec<ThreadCpuInfo> = thread_stats2
        .iter()
        .filter_map(|(tid, j2)| {
            let j1 = phase1.thread_jiffies.get(tid)?;
            let cpu_usage = (j2.saturating_sub(*j1) as f32 / total_delta) * 100.0;
            let name = thread_names
                .get(tid)
                .cloned()
                .unwrap_or_else(|| format!("thread-{}", tid));
            Some(ThreadCpuInfo {
                tid: tid.clone(),
                cpu_usage,
                name,
                timestamp: Some(timestamp),
            })
        })
        .collect();

    // 按 CPU 使用率降序排列
    threads.sort_by(|a, b| {
        b.cpu_usage
            .partial_cmp(&a.cpu_usage)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    println!(
        "[{}] Process CPU: {}% (pid: {})",
        timestamp.format("%H:%M:%S"),
        format!("{:.1}", process_cpu).blue(),
        pid.yellow()
    );

    Ok((process_cpu, timestamp, threads))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- parse_total_cpu_jiffies ----

    #[test]
    fn test_parse_total_cpu_jiffies_sums_all_fields() {
        // 真实 /proc/stat 第一行格式：cpu user nice system idle iowait irq softirq steal guest guest_nice
        let stat = "cpu  100 200 300 400 50 10 5 0 0 0\ncpu0 10 20 30 40\n";
        assert_eq!(parse_total_cpu_jiffies(stat), Some(100 + 200 + 300 + 400 + 50 + 10 + 5));
    }

    #[test]
    fn test_parse_total_cpu_jiffies_ignores_per_core_lines() {
        // 只取 "cpu "（带空格）的总行，不误匹配 "cpu0"
        let stat = "cpu0 1 2 3 4\ncpu  10 20 30 40\n";
        assert_eq!(parse_total_cpu_jiffies(stat), Some(10 + 20 + 30 + 40));
    }

    #[test]
    fn test_parse_total_cpu_jiffies_returns_none_if_no_cpu_line() {
        assert_eq!(parse_total_cpu_jiffies(""), None);
        assert_eq!(parse_total_cpu_jiffies("cpu0 1 2 3 4\n"), None);
    }

    #[test]
    fn test_parse_total_cpu_jiffies_skips_unparseable_fields() {
        // 非数字字段按 0 处理，不 panic
        let stat = "cpu  100 abc 200\n";
        assert_eq!(parse_total_cpu_jiffies(stat), Some(300));
    }

    // ---- parse_proc_stat_jiffies ----

    #[test]
    fn test_parse_proc_stat_jiffies_basic() {
        // utime=24130 (idx11), stime=38458 (idx12) → sum=62588
        let stat = "15803 (xiang.car.x.svm) S 22359 22359 0 0 -1 1077936448 444355 0 1 0 24130 38458 0 0 1 -19 87 0";
        assert_eq!(parse_proc_stat_jiffies(stat), Some(62588));
    }

    #[test]
    fn test_parse_proc_stat_jiffies_comm_with_spaces() {
        // comm 含空格（括号内），靠 rfind(')') 定位
        let stat = "123 (Signal Catcher) S 1 2 3 0 -1 0 0 0 0 0 100 200 0 0 20 0 1 0";
        assert_eq!(parse_proc_stat_jiffies(stat), Some(300));
    }

    #[test]
    fn test_parse_proc_stat_jiffies_comm_with_colon_and_digits() {
        // comm 含冒号和数字（Binder:15803_3），冒号在括号内不影响 rfind(')')
        let stat = "15829 (Binder:15803_3) S 1 2 3 0 -1 0 0 0 0 0 50 30 0 0 20 0 1 0";
        assert_eq!(parse_proc_stat_jiffies(stat), Some(80));
    }

    #[test]
    fn test_parse_proc_stat_jiffies_empty_returns_none() {
        // 进程退出时 cat 返回空 → None
        assert_eq!(parse_proc_stat_jiffies(""), None);
        assert_eq!(parse_proc_stat_jiffies("   \n"), None);
    }

    #[test]
    fn test_parse_proc_stat_jiffies_truncated_returns_none() {
        // 字段不足（无 utime/stime）→ None
        let stat = "123 (comm) S 1 2 3 0 -1 0 0 0 0 0";
        assert_eq!(parse_proc_stat_jiffies(stat), None);
    }

    #[test]
    fn test_parse_proc_stat_jiffies_no_paren_returns_none() {
        // 无 ')' → None
        assert_eq!(parse_proc_stat_jiffies("no parens here 1 2 3"), None);
    }

    // ---- parse_thread_name ----

    #[test]
    fn test_parse_thread_name_trims_whitespace() {
        assert_eq!(parse_thread_name("Binder:15803_3\n"), "Binder:15803_3");
        assert_eq!(parse_thread_name("  HeapTaskDaemon  "), "HeapTaskDaemon");
        assert_eq!(parse_thread_name(""), "");
    }

    // ---- parse_tid_list ----

    #[test]
    fn test_parse_tid_list_filters_non_digits() {
        // 真实 ls /proc/<pid>/task 输出：纯数字 TID，可能夹杂权限错误行
        let ls = "15803\n15814\n15815\nls: /proc/0/task: Permission denied\n";
        assert_eq!(
            parse_tid_list(ls),
            vec!["15803".to_string(), "15814".to_string(), "15815".to_string()]
        );
    }

    #[test]
    fn test_parse_tid_list_digits_only_preserved() {
        // 纯数字行全部保留
        let ls = "100\n200\n300\n";
        assert_eq!(
            parse_tid_list(ls),
            vec!["100".to_string(), "200".to_string(), "300".to_string()]
        );
    }

    #[test]
    fn test_parse_tid_list_empty() {
        assert!(parse_tid_list("").is_empty());
        assert!(parse_tid_list("no digits here").is_empty());
    }

    // ---- parse_thread_files_output（本轮核心修复的解析逻辑）----

    #[test]
    fn test_parse_thread_files_output_normal() {
        let out = "15803:15803 (xiang.car.x.svm) S 1\n15814:15814 (Signal Catcher) S 1\n";
        let map = parse_thread_files_output(out);
        assert_eq!(map.get("15803").unwrap(), "15803 (xiang.car.x.svm) S 1");
        assert_eq!(map.get("15814").unwrap(), "15814 (Signal Catcher) S 1");
    }

    #[test]
    fn test_parse_thread_files_output_dead_tid_preserved_as_empty() {
        // 线程退出时输出 "tid:"（空 content），tid 仍被记录——这是容错的关键：
        // 不再像旧版 cat 那样因退出码非零丢弃整批数据。
        let out = "15803:15803 (xiang.car.x.svm) S 1\n999999:\n15814:15814 (Signal Catcher) S 1\n";
        let map = parse_thread_files_output(out);
        assert_eq!(map.len(), 3);
        assert_eq!(map.get("15803").unwrap(), "15803 (xiang.car.x.svm) S 1");
        assert_eq!(map.get("999999").unwrap(), ""); // 空 content
        assert_eq!(map.get("15814").unwrap(), "15814 (Signal Catcher) S 1");
    }

    #[test]
    fn test_parse_thread_files_output_comm_with_colon_preserved() {
        // comm 含冒号（Binder:15803_3），split_once 在第一个冒号分割，content 完整保留
        let out = "15829:15829 (Binder:15803_3) S 1\n";
        let map = parse_thread_files_output(out);
        assert_eq!(map.get("15829").unwrap(), "15829 (Binder:15803_3) S 1");
    }

    #[test]
    fn test_parse_thread_files_output_rejects_non_digit_tid() {
        // shell 报错行等非数字 tid 被过滤
        let out = "cat: error\n15803:content here\nabc:bad\n";
        let map = parse_thread_files_output(out);
        assert_eq!(map.len(), 1);
        assert!(map.contains_key("15803"));
    }

    #[test]
    fn test_parse_thread_files_output_empty_input() {
        assert!(parse_thread_files_output("").is_empty());
    }

    // ---- sample_cpu_phase1 / phase2（注入 mock adb runner）----
    // 这些测试覆盖进程消失时返回 "Process not found" 的关键分支，
    // 该分支在真机上难以稳定复现（进程恰好消失的时机不可控）。
    //
    // 注：read_thread_files 走动态生成的 shell 脚本命令，mock 难以精确匹配，
    // 故这里只验证 proc/sys jiffies 解析和错误分支；线程 jiffies 的解析逻辑
    // 已由 parse_thread_files_output / parse_proc_stat_jiffies 单测覆盖。

    /// 进程 stat 行：utime(字段11) + stime(字段12) = jiffies
    fn stat_line(pid: &str, comm: &str, utime: u64, stime: u64) -> String {
        format!("{} ({}) S 1 2 3 0 -1 0 0 0 0 0 {} {} 0 0 20 0 1 0", pid, comm, utime, stime)
    }

    /// mock: /proc/stat 和 /proc/<pid>/stat 都返回固定值，read_thread_files 命令返回空
    fn phase_runner(args: &[&str]) -> anyhow::Result<utils::ProcOutput> {
        let stdout = if args[1] == "cat" && args[2].starts_with("/proc/stat") {
            "cpu  1000 0 0 0\n".to_string()
        } else if args[1] == "cat" && args[2].ends_with("/stat") {
            stat_line("15803", "xiang.car.x.svm", 100, 200) // jiffies=300
        } else {
            // ls /proc/<pid>/task 返回空（无线程），read_thread_files 脚本也走这里 → 空
            String::new()
        };
        Ok(utils::ProcOutput { stdout })
    }

    #[tokio::test]
    async fn test_sample_cpu_phase1_parses_jiffies() {
        utils::set_adb_runner_for_test(phase_runner);
        let p1 = sample_cpu_phase1("15803").await.unwrap();
        assert_eq!(p1.pid, "15803");
        assert_eq!(p1.proc_jiffies, 300); // 100 + 200
        assert_eq!(p1.sys_total, 1000);
        // 无线程（mock 的 task 列表返回空）
        assert!(p1.thread_jiffies.is_empty());
        utils::clear_adb_runner_for_test();
    }

    #[tokio::test]
    async fn test_sample_cpu_phase2_zero_delta_bails() {
        // 同一 mock，phase1 和 phase2 的 sys jiffies 都相同 → delta=0 → 报错
        utils::set_adb_runner_for_test(phase_runner);
        let p1 = sample_cpu_phase1("15803").await.unwrap();
        let err = sample_cpu_phase2(&p1).await.unwrap_err();
        assert!(err.to_string().contains("CPU delta 为零"));
        utils::clear_adb_runner_for_test();
    }

    /// 进程消失：phase1 时 cat /proc/<pid>/stat 返回空 stdout → "Process not found"
    fn dead_runner(args: &[&str]) -> anyhow::Result<utils::ProcOutput> {
        let stdout = if args[1] == "cat" && args[2].ends_with("/stat") && !args[2].starts_with("/proc/stat") {
            String::new() // 进程 stat 空 → 进程不存在
        } else if args[1] == "cat" && args[2].starts_with("/proc/stat") {
            "cpu  1000 0 0 0\n".to_string()
        } else {
            String::new()
        };
        Ok(utils::ProcOutput { stdout })
    }

    #[tokio::test]
    async fn test_sample_cpu_phase1_process_not_found() {
        utils::set_adb_runner_for_test(dead_runner);
        let err = sample_cpu_phase1("99999").await.unwrap_err();
        assert!(
            err.to_string().contains("Process not found"),
            "应报 Process not found，实际: {}",
            err
        );
        utils::clear_adb_runner_for_test();
    }

    // ---- phase2 的 delta 计算（直接覆盖，非间接）----
    // fn 指针无状态，但 phase1/phase2 调用同样的 cat 命令。用一个全局阶段标志区分：
    // 测试在 phase1 调用前设 PHASE=1，phase2 调用前设 PHASE=2，
    // runner 据此返回不同 sys/proc jiffies，使 delta 非零，直接验证 CPU% 计算。
    use std::sync::atomic::{AtomicU8, Ordering};
    static MOCK_PHASE: AtomicU8 = AtomicU8::new(1);

    fn delta_runner(args: &[&str]) -> anyhow::Result<utils::ProcOutput> {
        let phase = MOCK_PHASE.load(Ordering::SeqCst);
        let stdout = if args[1] == "cat" && args[2].starts_with("/proc/stat") {
            // phase1: sys=1000; phase2: sys=2000 → delta=1000
            if phase == 1 { "cpu  1000 0 0 0\n".to_string() } else { "cpu  2000 0 0 0\n".to_string() }
        } else if args[1] == "cat" && args[2].ends_with("/stat") {
            // phase1: utime=100+stime=200=300; phase2: utime=400+stime=200=600 → delta=300 → 30%
            if phase == 1 {
                stat_line("15803", "xiang.car.x.svm", 100, 200)
            } else {
                stat_line("15803", "xiang.car.x.svm", 400, 200)
            }
        } else {
            String::new()
        };
        Ok(utils::ProcOutput { stdout })
    }

    #[tokio::test]
    async fn test_sample_cpu_phase2_calculates_cpu_percent() {
        utils::set_adb_runner_for_test(delta_runner);
        // phase1：读第一次 jiffies
        MOCK_PHASE.store(1, Ordering::SeqCst);
        let p1 = sample_cpu_phase1("15803").await.unwrap();
        assert_eq!(p1.sys_total, 1000);
        assert_eq!(p1.proc_jiffies, 300);

        // phase2：读第二次 jiffies，sys delta=1000, proc delta=300 → 30.0%
        MOCK_PHASE.store(2, Ordering::SeqCst);
        let (cpu, _ts, threads) = sample_cpu_phase2(&p1).await.unwrap();
        assert!(
            (cpu - 30.0).abs() < 0.01,
            "expected 30.0%, got {}",
            cpu
        );
        // 无线程（mock 的 task 列表返回空）→ 线程列表为空
        assert!(threads.is_empty());
        utils::clear_adb_runner_for_test();
    }

    #[tokio::test]
    async fn test_sample_cpu_phase2_negative_delta_clamps_to_zero() {
        // 进程 jiffies 不应倒退，但若 phase2 < phase1（如进程重启），saturating_sub 归 0 → CPU=0%
        utils::set_adb_runner_for_test(delta_runner);
        MOCK_PHASE.store(1, Ordering::SeqCst);
        let p1 = sample_cpu_phase1("15803").await.unwrap(); // proc=300
        // phase2 返回更小的 proc jiffies（100+200=300 不变会 delta=0 报错，故用更小值）
        // 用一个返回更小 jiffies 的 runner
        fn smaller_runner(args: &[&str]) -> anyhow::Result<utils::ProcOutput> {
            let stdout = if args[1] == "cat" && args[2].starts_with("/proc/stat") {
                "cpu  2000 0 0 0\n".to_string() // sys delta=1000 非零
            } else if args[1] == "cat" && args[2].ends_with("/stat") {
                stat_line("15803", "xiang.car.x.svm", 10, 20) // jiffies=30 < phase1 的 300
            } else {
                String::new()
            };
            Ok(utils::ProcOutput { stdout })
        }
        utils::set_adb_runner_for_test(smaller_runner);
        let (cpu, _ts, _threads) = sample_cpu_phase2(&p1).await.unwrap();
        // proc delta = 30 - 300 < 0 → saturating_sub → 0 → CPU=0%
        assert!(
            (cpu - 0.0).abs() < 0.01,
            "负 delta 应 saturating 到 0%，got {}",
            cpu
        );
        utils::clear_adb_runner_for_test();
        MOCK_PHASE.store(1, Ordering::SeqCst);
    }

    // ---- 线程 CPU% 计算的直接覆盖 ----
    // read_thread_files 走 shell 脚本命令（"for t in ..."），mock 据此匹配并返回
    // 预设的 "tid:stat_line" / "tid:comm" 输出，使 phase2 的线程差值计算被直接测试。
    // 覆盖三种情况：两次都有的线程算差值；窗口内退出/新建的线程被跳过。
    fn thread_runner(args: &[&str]) -> anyhow::Result<utils::ProcOutput> {
        let phase = MOCK_PHASE.load(Ordering::SeqCst);
        // shell 脚本命令（read_thread_files 生成）
        if args[1].starts_with("for t in") {
            let is_comm = args[1].contains("/comm");
            let stdout = if is_comm {
                // comm 文件：tid=100→"Binder:1"，tid=200→"Jit"
                "100:Binder:1\n200:Jit\n".to_string()
            } else {
                // stat 文件：线程 jiffies
                // phase1: tid100=100, tid200=200（tid300 尚不存在）
                // phase2: tid100=300, tid300=400（tid200 退出，tid300 新建）
                if phase == 1 {
                    "100:100 (Binder:1) S 1 2 3 0 -1 0 0 0 0 0 100 0 0 0 20 0 1 0\n200:200 (Jit) S 1 2 3 0 -1 0 0 0 0 0 200 0 0 0 20 0 1 0\n".to_string()
                } else {
                    "100:100 (Binder:1) S 1 2 3 0 -1 0 0 0 0 0 300 0 0 0 20 0 1 0\n300:300 (NewThread) S 1 2 3 0 -1 0 0 0 0 0 400 0 0 0 20 0 1 0\n".to_string()
                }
            };
            return Ok(utils::ProcOutput { stdout });
        }
        // 进程/系统 stat
        let stdout = if args[1] == "cat" && args[2].starts_with("/proc/stat") {
            if phase == 1 { "cpu  1000 0 0 0\n".to_string() } else { "cpu  2000 0 0 0\n".to_string() }
        } else if args[1] == "cat" && args[2].ends_with("/stat") {
            if phase == 1 { stat_line("15803", "xiang.car.x.svm", 100, 200) } else { stat_line("15803", "xiang.car.x.svm", 400, 200) }
        } else if args[1] == "ls" {
            // task 列表：phase1 有 100/200，phase2 有 100/300（模拟线程生灭）
            if phase == 1 { "100\n200\n".to_string() } else { "100\n300\n".to_string() }
        } else {
            String::new()
        };
        Ok(utils::ProcOutput { stdout })
    }

    #[tokio::test]
    async fn test_sample_cpu_phase2_thread_delta_calculation() {
        utils::set_adb_runner_for_test(thread_runner);
        // phase1：线程 100(jiffies=100)、200(jiffies=200)
        MOCK_PHASE.store(1, Ordering::SeqCst);
        let p1 = sample_cpu_phase1("15803").await.unwrap();
        assert_eq!(p1.thread_jiffies.len(), 2);
        assert_eq!(p1.thread_jiffies.get("100"), Some(&100));
        assert_eq!(p1.thread_jiffies.get("200"), Some(&200));

        // phase2：线程 100(jiffies=300)、300(jiffies=400，新建)
        // sys delta=1000
        MOCK_PHASE.store(2, Ordering::SeqCst);
        let (cpu, _ts, threads) = sample_cpu_phase2(&p1).await.unwrap();
        // 进程 delta=300, sys delta=1000 → 30%
        assert!((cpu - 30.0).abs() < 0.01, "process CPU expected 30%, got {}", cpu);

        // 只有 tid100 两次都在 → 算差值：(300-100)/1000*100 = 20%
        // tid200 phase2 不在 → 跳过；tid300 phase1 不在 → 跳过
        assert_eq!(threads.len(), 1, "只有 tid100 应被计算，got {:?}", threads);
        let t100 = &threads[0];
        assert_eq!(t100.tid, "100");
        assert!((t100.cpu_usage - 20.0).abs() < 0.01, "tid100 CPU expected 20%, got {}", t100.cpu_usage);
        assert_eq!(t100.name, "Binder:1"); // 来自 comm 文件
        utils::clear_adb_runner_for_test();
        MOCK_PHASE.store(1, Ordering::SeqCst);
    }

    // ---- 线程排序的直接覆盖 ----
    // 多个线程两次都在、CPU% 不同，验证返回的 threads 按 CPU% 降序排列。
    fn sort_runner(args: &[&str]) -> anyhow::Result<utils::ProcOutput> {
        let phase = MOCK_PHASE.load(Ordering::SeqCst);
        if args[1].starts_with("for t in") {
            let is_comm = args[1].contains("/comm");
            let stdout = if is_comm {
                "100:Low\n200:High\n300:Mid\n".to_string()
            } else {
                // phase1: tid100=100, tid200=100, tid300=100
                // phase2: tid100=100(delta=0→0%), tid200=500(delta=400→40%), tid300=300(delta=200→20%)
                if phase == 1 {
                    "100:100 (Low) S 1 2 3 0 -1 0 0 0 0 0 100 0 0 0 20 0 1 0\n200:200 (High) S 1 2 3 0 -1 0 0 0 0 0 100 0 0 0 20 0 1 0\n300:300 (Mid) S 1 2 3 0 -1 0 0 0 0 0 100 0 0 0 20 0 1 0\n".to_string()
                } else {
                    "100:100 (Low) S 1 2 3 0 -1 0 0 0 0 0 100 0 0 0 20 0 1 0\n200:200 (High) S 1 2 3 0 -1 0 0 0 0 0 500 0 0 0 20 0 1 0\n300:300 (Mid) S 1 2 3 0 -1 0 0 0 0 0 300 0 0 0 20 0 1 0\n".to_string()
                }
            };
            return Ok(utils::ProcOutput { stdout });
        }
        let stdout = if args[1] == "cat" && args[2].starts_with("/proc/stat") {
            if phase == 1 { "cpu  1000 0 0 0\n".to_string() } else { "cpu  2000 0 0 0\n".to_string() }
        } else if args[1] == "cat" && args[2].ends_with("/stat") {
            if phase == 1 { stat_line("15803", "xiang.car.x.svm", 100, 200) } else { stat_line("15803", "xiang.car.x.svm", 400, 200) }
        } else if args[1] == "ls" {
            "100\n200\n300\n".to_string()
        } else {
            String::new()
        };
        Ok(utils::ProcOutput { stdout })
    }

    #[tokio::test]
    async fn test_sample_cpu_phase2_threads_sorted_desc() {
        utils::set_adb_runner_for_test(sort_runner);
        MOCK_PHASE.store(1, Ordering::SeqCst);
        let p1 = sample_cpu_phase1("15803").await.unwrap();

        MOCK_PHASE.store(2, Ordering::SeqCst);
        let (_cpu, _ts, threads) = sample_cpu_phase2(&p1).await.unwrap();
        // 三个线程都两次都在：tid100=0%, tid200=40%, tid300=20%
        assert_eq!(threads.len(), 3);
        // 按 CPU% 降序：High(40%) > Mid(20%) > Low(0%)
        assert_eq!(threads[0].name, "High");
        assert!((threads[0].cpu_usage - 40.0).abs() < 0.01);
        assert_eq!(threads[1].name, "Mid");
        assert!((threads[1].cpu_usage - 20.0).abs() < 0.01);
        assert_eq!(threads[2].name, "Low");
        assert!((threads[2].cpu_usage - 0.0).abs() < 0.01);
        utils::clear_adb_runner_for_test();
        MOCK_PHASE.store(1, Ordering::SeqCst);
    }
}
