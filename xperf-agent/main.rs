//! xperf-agent：设备端常驻采样器。
//!
//! 低间隔（<500ms）采样时主机侧 adb 轮询不可行（单轮多次 adb 调用的开销就超过
//! 间隔本身），因此把采样循环搬到设备上：直接读 /proc（微秒级），结果以 NDJSON
//! 行流式写 stdout，主机用 `adb exec-out` 一条长连接持续读取。
//!
//! 协议（每行一个 JSON 对象）：
//!   {"t":"hello","ncores":8,"version":1}
//!   {"t":"cpu","ts":<wall_ms>,"pid":29697,"cpu":15.4,"th":[[29697,"main",5.1],...]}
//!   {"t":"mem","ts":<wall_ms>,"pid":29697,"pss":484880,"rss":612000}
//!   {"t":"exit","pid":29697}
//!   {"t":"noproc"}
//!   {"t":"err","msg":"..."}
//!
//! 用法：xperf-agent --package <pkg> [--pid N]... --interval 50 [--cpu] [--memory]
//!
//! 与主机侧 adb 模式的差异：
//! - CPU 口径相同（jiffies 差值 ×核数，单核基准），但窗口是相邻两轮之间
//!   （agent 常驻保有上一轮状态，无需主机侧 phase1/phase2 结构）
//! - 内存改读 /proc/<pid>/smaps_rollup 的 Pss/Rss（~1ms），不用 dumpsys meminfo
//!   （~100ms，低间隔下既慢又扰动被测系统）
//! - FPS 不在 agent 内实现：SurfaceFlinger 127 帧缓冲在 1s 轮询下已是帧级分辨率，
//!   低间隔轮询无收益，仍由主机侧负责

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Args {
    package: Option<String>,
    pids: Vec<u32>,
    interval_ms: u64,
    cpu: bool,
    memory: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut package = None;
    let mut pids = Vec::new();
    let mut interval_ms = 50u64;
    let mut cpu = false;
    let mut memory = false;
    let argv: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < argv.len() {
        match argv[i].as_str() {
            "--package" => {
                i += 1;
                package = Some(argv.get(i).ok_or("--package 缺参数")?.clone());
            }
            "--pid" => {
                i += 1;
                pids.push(
                    argv.get(i)
                        .and_then(|s| s.parse().ok())
                        .ok_or("--pid 参数非法")?,
                );
            }
            "--interval" => {
                i += 1;
                interval_ms = argv
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .ok_or("--interval 参数非法")?;
            }
            "--cpu" => cpu = true,
            "--memory" => memory = true,
            other => return Err(format!("未知参数: {}", other)),
        }
        i += 1;
    }
    if package.is_none() && pids.is_empty() {
        return Err("需要 --package 或 --pid".into());
    }
    if !cpu && !memory {
        return Err("需要 --cpu 或 --memory 至少一个".into());
    }
    if interval_ms == 0 {
        return Err("--interval 不能为 0".into());
    }
    Ok(Args { package, pids, interval_ms, cpu, memory })
}

// ---------- /proc 读取 ----------

/// 读 /proc/stat：返回 (所有核总 jiffies, 核数)
fn read_total_jiffies() -> Option<(u64, u32)> {
    let content = fs::read_to_string("/proc/stat").ok()?;
    let mut total = None;
    let mut ncores = 0u32;
    for line in content.lines() {
        if line.starts_with("cpu ") {
            total = Some(
                line.split_whitespace()
                    .skip(1)
                    .map(|v| v.parse::<u64>().unwrap_or(0))
                    .sum(),
            );
        } else if line.starts_with("cpu")
            && line.as_bytes().get(3).is_some_and(|b| b.is_ascii_digit())
        {
            ncores += 1;
        }
    }
    Some((total?, ncores.max(1)))
}

/// 读 /proc/<pid>/stat 或 task/<tid>/stat 的 utime+stime jiffies
fn read_stat_jiffies(path: &str) -> Option<u64> {
    let content = fs::read_to_string(path).ok()?;
    parse_stat_jiffies(&content)
}

fn parse_stat_jiffies(stat: &str) -> Option<u64> {
    let paren_end = stat.trim().rfind(')')?;
    let fields: Vec<&str> = stat.trim()[paren_end + 1..].split_whitespace().collect();
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some(utime + stime)
}

/// 读 /proc/<pid>/smaps_rollup：返回 (Pss KB, Rss KB)
fn read_smaps_rollup(pid: u32) -> Option<(u64, u64)> {
    let content = fs::read_to_string(format!("/proc/{}/smaps_rollup", pid)).ok()?;
    parse_smaps_rollup(&content)
}

fn parse_smaps_rollup(content: &str) -> Option<(u64, u64)> {
    let mut pss = None;
    let mut rss = None;
    for line in content.lines() {
        if let Some(v) = line.strip_prefix("Pss:") {
            pss = v.split_whitespace().next().and_then(|s| s.parse().ok());
        } else if let Some(v) = line.strip_prefix("Rss:") {
            rss = v.split_whitespace().next().and_then(|s| s.parse().ok());
        }
        if pss.is_some() && rss.is_some() {
            break;
        }
    }
    Some((pss?, rss?))
}

/// 按包名解析 PID（扫 /proc/*/cmdline）
fn resolve_pids(package: &str) -> Vec<u32> {
    let mut pids = Vec::new();
    if let Ok(entries) = fs::read_dir("/proc") {
        for e in entries.flatten() {
            let name = e.file_name();
            let Some(name) = name.to_str() else { continue };
            let Ok(pid) = name.parse::<u32>() else { continue };
            if let Ok(cmd) = fs::read_to_string(format!("/proc/{}/cmdline", pid)) {
                if cmd.trim_end_matches('\0') == package {
                    pids.push(pid);
                }
            }
        }
    }
    pids.sort_unstable();
    pids
}

// ---------- 输出 ----------

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn emit(line: &str) {
    let mut out = std::io::stdout().lock();
    // 对端断开（adb 连接关闭）时写失败，直接退出
    if writeln!(out, "{}", line).is_err() || out.flush().is_err() {
        std::process::exit(0);
    }
}

// ---------- 采样状态 ----------

struct PidState {
    prev_jiffies: u64,
    prev_threads: HashMap<u32, u64>,
    comm_cache: HashMap<u32, String>,
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(2);
        }
    };

    let Some((mut prev_total, ncores)) = read_total_jiffies() else {
        eprintln!("无法读取 /proc/stat");
        std::process::exit(1);
    };
    emit(&format!("{{\"t\":\"hello\",\"ncores\":{},\"version\":1}}", ncores));

    let mut states: HashMap<u32, PidState> = HashMap::new();
    let mut active_pids: Vec<u32> = args.pids.clone();

    // 绝对节拍：按起始时间推算每轮时刻，避免 sleep 累积漂移
    let interval = Duration::from_millis(args.interval_ms);
    let start = Instant::now();
    let mut round: u64 = 0;
    // 进程列表重扫间隔：约 1s 一次（低间隔下每轮扫 /proc 太贵）
    let rescan_rounds = (1000 / args.interval_ms).max(1);

    loop {
        round += 1;
        // 绝对节拍 sleep：本轮目标时刻 = start + round * interval
        let target = start + interval * round as u32;
        let now = Instant::now();
        if target > now {
            std::thread::sleep(target - now);
        } else {
            // 本轮处理已超间隔（设备太忙），打印告警行
            emit(&format!("{{\"t\":\"err\",\"msg\":\"round {} overrun by {}ms\"}}", round, (now - target).as_millis()));
        }

        // 周期性按包名重扫进程（动态跟随新 PID）
        if args.package.is_some() && (active_pids.is_empty() || round.is_multiple_of(rescan_rounds)) {
            let pkg = args.package.as_deref().unwrap();
            let found = resolve_pids(pkg);
            for pid in &found {
                if !active_pids.contains(pid) {
                    active_pids.push(*pid); // 新 PID 首轮建基线（states 中无记录）
                }
            }
            active_pids.retain(|p| found.contains(p));
            if found.is_empty() {
                // 每轮重扫都报：让主机读循环在无进程期间也能定期收到行（保持 Ctrl-C 响应）
                emit("{\"t\":\"noproc\"}");
            }
        }

        let Some((total, _)) = read_total_jiffies() else {
            emit("{\"t\":\"err\",\"msg\":\"read /proc/stat failed\"}");
            continue;
        };
        let total_delta = total.saturating_sub(prev_total);
        prev_total = total;
        if total_delta == 0 {
            continue; // 间隔过短导致 jiffies 无变化，跳过本轮
        }
        let ts = now_ms();

        let mut exited: Vec<u32> = Vec::new();
        for &pid in &active_pids {
            let proc_path = format!("/proc/{}/stat", pid);
            let Some(proc_jiffies) = read_stat_jiffies(&proc_path) else {
                exited.push(pid);
                continue;
            };

            // CPU：与上轮取差（首轮建基线不出数）
            if args.cpu {
                if let Some(st) = states.get_mut(&pid) {
                    let cpu = proc_jiffies.saturating_sub(st.prev_jiffies) as f32
                        / total_delta as f32
                        * 100.0
                        * ncores as f32;
                    st.prev_jiffies = proc_jiffies;

                    // 线程级：读 task/ 下所有 tid
                    let mut th_json = String::new();
                    if let Ok(tids) = fs::read_dir(format!("/proc/{}/task", pid)) {
                        let mut threads: Vec<u32> = tids
                            .flatten()
                            .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
                            .collect();
                        threads.sort_unstable();
                        for tid in threads {
                            let tj = read_stat_jiffies(&format!("/proc/{}/task/{}/stat", pid, tid));
                            let Some(tj) = tj else { continue };
                            let prev = st.prev_threads.insert(tid, tj);
                            if let Some(prev) = prev {
                                let tcpu = tj.saturating_sub(prev) as f32
                                    / total_delta as f32
                                    * 100.0
                                    * ncores as f32;
                                if tcpu > 0.05 {
                                    let name = st.comm_cache.entry(tid).or_insert_with(|| {
                                        fs::read_to_string(format!("/proc/{}/task/{}/comm", pid, tid))
                                            .map(|s| s.trim().to_string())
                                            .unwrap_or_else(|_| "?".into())
                                    });
                                    th_json.push_str(&format!(
                                        ",[{},\"{}\",{:.2}]",
                                        tid,
                                        json_escape(name),
                                        tcpu
                                    ));
                                }
                            }
                        }
                        // 清掉已退出线程的状态
                        st.prev_threads.retain(|tid, _| {
                            fs::metadata(format!("/proc/{}/task/{}", pid, tid)).is_ok()
                        });
                    }
                    emit(&format!(
                        "{{\"t\":\"cpu\",\"ts\":{},\"pid\":{},\"cpu\":{:.2},\"th\":[{}]}}",
                        ts, pid, cpu, th_json.trim_start_matches(',')
                    ));
                } else {
                    states.insert(
                        pid,
                        PidState {
                            prev_jiffies: proc_jiffies,
                            prev_threads: HashMap::new(),
                            comm_cache: HashMap::new(),
                        },
                    );
                }
            }

            // 内存：smaps_rollup（每轮都读，无需基线）
            if args.memory {
                if let Some((pss, rss)) = read_smaps_rollup(pid) {
                    emit(&format!(
                        "{{\"t\":\"mem\",\"ts\":{},\"pid\":{},\"pss\":{},\"rss\":{}}}",
                        ts, pid, pss, rss
                    ));
                }
            }
        }

        for pid in exited {
            active_pids.retain(|&p| p != pid);
            states.remove(&pid);
            emit(&format!("{{\"t\":\"exit\",\"pid\":{}}}", pid));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_stat_jiffies_basic() {
        let stat = "29697 (xiang.car.x.svm) S 1 2 3 0 -1 0 0 0 0 0 24130 38458 0 0 20 0 1 0";
        assert_eq!(parse_stat_jiffies(stat), Some(24130 + 38458));
    }

    #[test]
    fn test_parse_stat_jiffies_comm_with_spaces() {
        let stat = "123 (Signal Catcher) S 1 2 3 0 -1 0 0 0 0 0 100 200 0 0 20 0 1 0";
        assert_eq!(parse_stat_jiffies(stat), Some(300));
    }

    #[test]
    fn test_parse_stat_jiffies_truncated() {
        assert_eq!(parse_stat_jiffies("123 (comm) S 1 2 3"), None);
    }

    #[test]
    fn test_parse_smaps_rollup_basic() {
        let content = "55f7000000-7g0000000 ---p 00000000 00:00 0 [rollup]\n\
                       Rss:             612000 kB\n\
                       Pss:             484880 kB\n\
                       Shared_Clean:      100 kB\n";
        assert_eq!(parse_smaps_rollup(content), Some((484880, 612000)));
    }

    #[test]
    fn test_parse_smaps_rollup_missing_fields() {
        assert_eq!(parse_smaps_rollup("Rss: 100 kB\n"), None);
    }

    #[test]
    fn test_json_escape() {
        assert_eq!(json_escape("a\"b\\c"), "a\\\"b\\\\c");
    }
}
