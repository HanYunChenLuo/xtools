//! perfetto 深挖模式：录制 N 秒系统级 trace，拉回主机用 trace_processor SQL 归因。
//! CLI（xperformance）与 GUI（xperf-gui）共用——输出目录由调用方传入，报告以文本返回，
//! 进度/打印均由调用方负责（CLI println，GUI 走 Tauri 事件）。
//!
//! 定位：与实时采样互补的「录制-分析」模式——采样回答"什么时候高"，trace 回答"为什么高"：
//! - 包内每线程的精确 CPU 时间（ftrace sched_switch，微秒级，不受采样间隔量化）
//! - 抢占/调度延迟（thread_state Runnable：唤醒→上核，谁被抢、被抢多久）
//! - 同窗口系统级 CPU top（谁在抢核）、每核 busy/上下文切换次数（排除 idle）
//! - CPU 频率区间（平台无 cpufreq ftrace 事件时如实标注，如 SS3 GVM 频率归 hypervisor）
//! - 帧时间线全局统计（此平台 frametimeline 无进程/图层归属，与 agent 图层 FPS 互补）
//!
//! 链路：`adb shell perfetto -c - --txt -o /data/misc/perfetto-traces/…`（stdin 喂配置，
//! write_into_file 流式落盘，长录制内存有界）→ adb pull → `trace_processor -q <sql>` 输出
//! CSV → 解析。trace 文件始终保留，可拖入 https://ui.perfetto.dev 交互分析。
//!
//! SQL 执行约定：全部查询写在一个文件里单次执行（trace 只加载一次），marker 查询分段；
//! 某条查询出错会中止整个文件的后续语句，故按「表必然存在 → 可能缺失」排序——帧时间线
//! 表可能不存在（数据源未产数据）放最后，结尾 `===END===` 哨兵判断是否执行完整，缺失段
//! 从 stderr 取原因。实测基线（SS3，10s trace）：sched 14 万事件，多语句输出为
//! "表头+数据行+空行"的结果集序列。

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Local};
use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// get.perfetto.dev 官方下载脚本缓存原生二进制的目录（~/.local/share/perfetto/prebuilts）
const TP_CACHE_SUBDIR: &str = ".local/share/perfetto/prebuilts";

// ==================== 录制 ====================

pub struct RecordedTrace {
    pub local_path: PathBuf,
    pub wall_start: DateTime<Local>,
    pub wall_end: DateTime<Local>,
    pub bytes: u64,
}

/// trace 配置（text proto）。设备端 v15.0 实测：
/// - write_into_file + 2s 刷盘周期：录制中文件持续增长，长录制内存有界（ring buffer 只做缓冲）
/// - cpu_frequency 事件在 SS3 GVM 不存在（频率归 hypervisor），perfetto 不报错、正常录其余源
fn trace_config(seconds: u64) -> String {
    format!(
        "duration_ms: {ms}\n\
         buffers {{\n  size_kb: 65536\n  fill_policy: RING_BUFFER\n}}\n\
         data_sources {{\n  config {{\n    name: \"linux.ftrace\"\n    ftrace_config {{\n      \
         ftrace_events: \"sched_switch\"\n      ftrace_events: \"sched_waking\"\n      \
         ftrace_events: \"cpu_frequency\"\n      ftrace_events: \"sched_process_exit\"\n    }}\n  }}\n}}\n\
         data_sources {{\n  config {{ name: \"linux.process_stats\" }}\n}}\n\
         data_sources {{\n  config {{ name: \"android.surfaceflinger.frametimeline\" }}\n}}\n\
         write_into_file: true\nfile_write_period_ms: 2000\nflush_period_ms: 2000\n",
        ms = seconds * 1000
    )
}

/// 录制 N 秒 trace 并拉回 out_dir（如 `log/<pkg>/<会话时间戳>/trace/`，自动创建）。
/// 阻塞至录制完成（perfetto duration_ms 到点自动退出并写完文件）。
/// 包名校验与进度展示由调用方负责（目录路径含包名，防遍历）。
pub fn record(seconds: u64, out_dir: &Path) -> Result<RecordedTrace> {
    std::fs::create_dir_all(out_dir)?;
    let stem = Local::now().format("%Y%m%d_%H%M%S").to_string();
    let local_path = out_dir.join(format!("trace_{}.pftrace", stem));
    let dev_path = format!("/data/misc/perfetto-traces/xperf_{}.pftrace", stem);

    let wall_start = Local::now();
    let mut child = Command::new("adb")
        .args(["shell", "perfetto", "-c", "-", "--txt", "-o", &dev_path])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("启动 adb shell perfetto 失败")?;
    {
        let mut stdin = child.stdin.take().expect("stdin just piped");
        stdin.write_all(trace_config(seconds).as_bytes())?;
    } // drop → EOF，perfetto 开始录制
      // 手动超时（同 coldstart 模式）：录制时长 + 启动/flush 余量；Ctrl-C 全局中断标志可提前放弃
    let deadline = Instant::now() + Duration::from_secs(seconds + 25);
    loop {
        match child.try_wait()? {
            Some(_) => break,
            None if crate::utils::is_interrupted() => {
                let _ = child.kill();
                bail!("录制被 Ctrl-C 中断；设备端可能残留 {}（可手动 adb pull）", dev_path);
            }
            None if Instant::now() > deadline => {
                let _ = child.kill();
                bail!(
                    "perfetto 录制超时（>{}s）。设备端残留 {} 可手动 adb pull 分析",
                    seconds + 25,
                    dev_path
                );
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    let out = child.wait_with_output()?;
    let wall_end = Local::now();
    // Ctrl-C 的 SIGINT 发给整个前台进程组，adb 会被一并杀死（退出码为信号而非非零 code）
    let killed_by_signal = {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            out.status.signal().is_some()
        }
        #[cfg(not(unix))]
        {
            false
        }
    };
    if killed_by_signal {
        bail!(
            "录制被中断（如 Ctrl-C）；设备端可能残留 {}（traced TTL 后停止写入，可手动 adb pull）",
            dev_path
        );
    }
    if !out.status.success() {
        bail!(
            "perfetto 录制失败: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let pull = Command::new("adb")
        .arg("pull")
        .arg(&dev_path)
        .arg(&local_path)
        .output()
        .context("执行 adb pull 失败")?;
    if !pull.status.success() {
        bail!(
            "adb pull 失败: {}\n设备端文件: {}",
            String::from_utf8_lossy(&pull.stderr).trim(),
            dev_path
        );
    }
    // 清理设备端文件（失败不致命：文件名含时间戳，不影响下次录制）
    let _ = Command::new("adb").args(["shell", "rm", "-f", &dev_path]).output();
    let bytes = std::fs::metadata(&local_path)?.len();
    if bytes == 0 {
        bail!("trace 文件为空（perfetto 未写出数据）");
    }
    Ok(RecordedTrace { local_path, wall_start, wall_end, bytes })
}

// ==================== trace_processor 定位/引导 ====================

/// 定位 trace_processor：官方缓存 > PATH > /tmp 下载脚本（自举后原生二进制落在官方缓存）
fn find_trace_processor() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME") {
        let cache = PathBuf::from(home).join(TP_CACHE_SUBDIR);
        if let Ok(rd) = std::fs::read_dir(&cache) {
            // 同名多版本（hash 后缀）取最新
            let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
            for e in rd.flatten() {
                if !e.file_name().to_string_lossy().starts_with("trace_processor_shell") {
                    continue;
                }
                let mtime = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                if best.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
                    best = Some((mtime, e.path()));
                }
            }
            if let Some((_, p)) = best {
                return Some(p);
            }
        }
    }
    let which = Command::new("sh")
        .args(["-c", "command -v trace_processor"])
        .output();
    if let Ok(o) = which {
        if o.status.success() {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if !s.is_empty() {
                return Some(PathBuf::from(s));
            }
        }
    }
    let tmp = PathBuf::from("/tmp/trace_processor");
    if tmp.is_file() {
        return Some(tmp);
    }
    None
}

/// 确保 trace_processor 可用；无则从 get.perfetto.dev 引导下载官方脚本并自举原生二进制。
pub fn ensure_trace_processor() -> Result<PathBuf> {
    if let Some(p) = find_trace_processor() {
        return Ok(p);
    }
    eprintln!("[trace] 未找到 trace_processor，从 get.perfetto.dev 引导下载…");
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".local/share/perfetto");
    std::fs::create_dir_all(&base)?;
    let script = base.join("trace_processor");
    let url = "https://get.perfetto.dev/trace_processor";
    let ok = Command::new("curl")
        .args(["-fsSL", "-m", "60", "-o"])
        .arg(&script)
        .arg(url)
        .output()
        .map(|o| o.status.success())
        .or_else(|_| {
            Command::new("wget")
                .args(["-q", "-T", "60", "-O"])
                .arg(&script)
                .arg(url)
                .output()
                .map(|o| o.status.success())
        })
        .unwrap_or(false);
    if !ok {
        bail!(
            "下载失败。请手动安装：curl -fsSL {} -o <dir>/trace_processor && chmod +x <dir>/trace_processor",
            url
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755));
    }
    // 跑一次 --version：脚本会把原生二进制下载到官方缓存再转exec
    let _ = Command::new(&script).arg("--version").output();
    find_trace_processor().context("trace_processor 自举失败")
}

// ==================== SQL 分析 ====================

pub struct ThreadCpuMs {
    pub tid: u32,
    pub name: String,
    pub cpu_ms: f64,
}

pub struct ThreadRunnable {
    pub tid: u32,
    pub name: String,
    pub count: u64,
    pub runnable_ms: f64,
    pub max_ms: f64,
}

pub struct CoreBusy {
    pub cpu: u32,
    pub busy_ms: f64,
    pub switches: u64,
}

pub struct FreqStat {
    pub cpu: u32,
    pub min_mhz: f64,
    pub max_mhz: f64,
    pub avg_mhz: f64,
}

pub struct FrameStats {
    pub frames: u64,
    pub avg_ms: f64,
    pub max_ms: f64,
}

pub struct Analysis {
    pub window_ms: f64,
    pub pkg_cpu_ms: f64,
    pub pkg_threads: Vec<ThreadCpuMs>,
    pub pkg_runnable: Vec<ThreadRunnable>,
    pub top_procs: Vec<(String, f64)>,
    pub per_core: Vec<CoreBusy>,
    pub cpufreq: Vec<FreqStat>,
    pub frame: Option<FrameStats>,
    pub worst_frames: Vec<(f64, f64)>, // (boot ts_ms, dur_ms)
    pub notes: Vec<String>,            // 执行中止等异常说明
}

fn sql_escape(s: &str) -> String {
    s.replace('\'', "''")
}

/// 生成全部查询。排序原则：表必然存在的在前，帧时间线表可能不存在殿后，END 哨兵收尾。
fn build_sql(package: &str) -> String {
    let pkg = sql_escape(package);
    let marker = "select '===%s===' as m;\n";
    let mut q = String::new();
    // 1. trace 窗口（trace_bounds 必然存在）
    q.push_str(&marker.replace("%s", "bounds"));
    q.push_str("select (end_ts - start_ts)/1e6 as window_ms from trace_bounds;\n");
    // 2. 包 CPU 总量（空窗口 coalesce 兜底）
    q.push_str(&marker.replace("%s", "pkg_total"));
    q.push_str(&format!(
        "select coalesce(sum(s.dur), 0)/1e6 as cpu_ms from sched s \
         join thread t using(utid) join process p on t.upid = p.upid where p.name = '{pkg}';\n"
    ));
    // 3. 包线程 CPU 时间 top
    q.push_str(&marker.replace("%s", "pkg_threads"));
    q.push_str(&format!(
        "select t.tid, ifnull(t.name, '(unnamed)') as thread, sum(s.dur)/1e6 as cpu_ms \
         from sched s join thread t using(utid) join process p on t.upid = p.upid \
         where p.name = '{pkg}' group by t.tid, t.name order by cpu_ms desc limit 15;\n"
    ));
    // 4. 包线程抢占/调度延迟（R=Runnable 可上核被抢，R+=迁移中）
    q.push_str(&marker.replace("%s", "pkg_runnable"));
    q.push_str(&format!(
        "select t.tid, ifnull(t.name, '(unnamed)') as thread, count(*) as n, \
         sum(st.dur)/1e6 as runnable_ms, max(st.dur)/1e6 as max_ms \
         from thread_state st join thread t using(utid) join process p on t.upid = p.upid \
         where st.state in ('R', 'R+') and p.name = '{pkg}' \
         group by t.tid, t.name order by runnable_ms desc limit 10;\n"
    ));
    // 5. 系统级 CPU top（排除 idle：swapper 切片 utid=0 且挂到 upid=0 的无名进程，
    //    不排除会把 idle 时间淹没进"(内核线程)"桶，空闲机器上虚占 80%+）
    q.push_str(&marker.replace("%s", "top_procs"));
    q.push_str(
        "select ifnull(p.name, '(内核线程)') as process, sum(s.dur)/1e6 as cpu_ms \
         from sched s join thread t using(utid) join process p on t.upid = p.upid \
         where s.utid != 0 group by p.name order by cpu_ms desc limit 10;\n",
    );
    // 6. 每核 busy/切换次数（排除 idle 切片与 idle 切换，反映真实调度压力；
    //    sched 表含切换到 swapper 的切片，不排除则每核恒 ~100%）
    q.push_str(&marker.replace("%s", "per_core"));
    q.push_str(
        "select cpu, sum(dur)/1e6 as busy_ms, count(*) as switches from sched \
         where utid != 0 group by cpu order by cpu;\n",
    );
    // 7. CPU 频率（无 cpufreq ftrace 事件的平台为空结果，如 SS3 GVM）
    q.push_str(&marker.replace("%s", "cpufreq"));
    q.push_str(
        "select ct.cpu, min(c.value)/1e3 as min_mhz, max(c.value)/1e3 as max_mhz, \
         avg(c.value)/1e3 as avg_mhz \
         from counter c join cpu_counter_track ct on c.track_id = ct.id \
         where ct.name = 'cpufreq' group by ct.cpu order by ct.cpu;\n",
    );
    // 8. 帧时间线（表可能不存在 → 放最后，出错只损失本段）
    q.push_str(&marker.replace("%s", "frame_stats"));
    q.push_str(
        "select count(*) as frames, ifnull(avg(dur)/1e6, 0) as avg_ms, \
         ifnull(max(dur)/1e6, 0) as max_ms from actual_frame_timeline_slice;\n",
    );
    q.push_str(&marker.replace("%s", "worst_frames"));
    q.push_str(
        "select ts/1e6 as ts_ms, dur/1e6 as dur_ms from actual_frame_timeline_slice \
         order by dur desc limit 5;\n",
    );
    // 9. 哨兵：判断查询链是否执行完整
    q.push_str(&marker.replace("%s", "END"));
    q
}

fn run_trace_processor(tp: &Path, trace: &Path, sql_path: &Path) -> Result<(String, String)> {
    let out = Command::new(tp)
        .arg("-q")
        .arg(sql_path)
        .arg(trace)
        .output()
        .context("执行 trace_processor 失败")?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    // 查询出错时 shell 以非零退出，但 stdout 里已完成语句的结果仍有效——只要非空就继续解析
    if stdout.is_empty() && !out.status.success() {
        bail!("trace_processor 查询失败: {}", stderr.trim());
    }
    Ok((stdout, stderr))
}

/// 解析 trace_processor 的多语句输出：结果集以空行分隔，marker 结果集用于分段。
/// 返回 段名 → 数据行（已去表头），每行 = 字段数组。空段（表存在但 0 行）也登记，
/// 以区分"空数据"与"未执行到"。
fn parse_sections(stdout: &str) -> HashMap<String, Vec<Vec<String>>> {
    let mut sections: HashMap<String, Vec<Vec<String>>> = HashMap::new();
    let mut current: Option<String> = None;
    for block in stdout.replace("\r\n", "\n").split("\n\n") {
        let lines: Vec<&str> = block.lines().filter(|l| !l.trim().is_empty()).collect();
        if lines.is_empty() {
            continue;
        }
        let rows: Vec<Vec<String>> = lines.iter().skip(1).map(|l| parse_csv_line(l)).collect();
        if rows.len() == 1 && rows[0].len() == 1 && rows[0][0].starts_with("===") {
            let name = rows[0][0].trim_matches('=').to_string();
            sections.entry(name.clone()).or_default();
            current = Some(name);
        } else if let Some(name) = &current {
            sections.entry(name.clone()).or_default().extend(rows);
        }
    }
    sections
}

/// 简单 CSV 行解析（处理双引号包裹与引号转义）
fn parse_csv_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if in_quotes && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => fields.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    fields.push(cur);
    fields
}

fn cell_f64(row: &[String], i: usize) -> f64 {
    row.get(i)
        .and_then(|s| if s == "[NULL]" { None } else { s.trim().parse().ok() })
        .unwrap_or(0.0)
}

fn cell_u64(row: &[String], i: usize) -> u64 {
    cell_f64(row, i).max(0.0) as u64
}

fn cell_str(row: &[String], i: usize) -> String {
    row.get(i).cloned().unwrap_or_default()
}

/// 执行分析。sql_path 处写入实际使用的查询语句（可复现）。
pub fn analyze(tp: &Path, trace: &Path, package: &str, sql_path: &Path) -> Result<Analysis> {
    let sql = build_sql(package);
    std::fs::write(sql_path, &sql).context("写入 trace_queries.sql 失败")?;
    let (stdout, stderr) = run_trace_processor(tp, trace, sql_path)?;
    let sec = parse_sections(&stdout);
    let mut notes = Vec::new();
    if !sec.contains_key("END") {
        let reason = stderr
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("未知原因")
            .to_string();
        notes.push(format!("部分查询未执行完（帧时间线等末尾段可能缺失）: {}", reason));
    }
    let window_ms = sec
        .get("bounds")
        .and_then(|rows| rows.first())
        .map(|r| cell_f64(r, 0))
        .unwrap_or(0.0);
    let pkg_cpu_ms = sec
        .get("pkg_total")
        .and_then(|rows| rows.first())
        .map(|r| cell_f64(r, 0))
        .unwrap_or(0.0);
    let pkg_threads = sec
        .get("pkg_threads")
        .map(|rows| {
            rows.iter()
                .map(|r| ThreadCpuMs {
                    tid: cell_u64(r, 0) as u32,
                    name: cell_str(r, 1),
                    cpu_ms: cell_f64(r, 2),
                })
                .collect()
        })
        .unwrap_or_default();
    let pkg_runnable = sec
        .get("pkg_runnable")
        .map(|rows| {
            rows.iter()
                .map(|r| ThreadRunnable {
                    tid: cell_u64(r, 0) as u32,
                    name: cell_str(r, 1),
                    count: cell_u64(r, 2),
                    runnable_ms: cell_f64(r, 3),
                    max_ms: cell_f64(r, 4),
                })
                .collect()
        })
        .unwrap_or_default();
    let top_procs = sec
        .get("top_procs")
        .map(|rows| rows.iter().map(|r| (cell_str(r, 0), cell_f64(r, 1))).collect())
        .unwrap_or_default();
    let per_core = sec
        .get("per_core")
        .map(|rows| {
            rows.iter()
                .map(|r| CoreBusy {
                    cpu: cell_u64(r, 0) as u32,
                    busy_ms: cell_f64(r, 1),
                    switches: cell_u64(r, 2),
                })
                .collect()
        })
        .unwrap_or_default();
    let cpufreq = sec
        .get("cpufreq")
        .map(|rows| {
            rows.iter()
                .map(|r| FreqStat {
                    cpu: cell_u64(r, 0) as u32,
                    min_mhz: cell_f64(r, 1),
                    max_mhz: cell_f64(r, 2),
                    avg_mhz: cell_f64(r, 3),
                })
                .collect()
        })
        .unwrap_or_default();
    let frame = sec
        .get("frame_stats")
        .and_then(|rows| rows.first())
        .map(|r| FrameStats {
            frames: cell_u64(r, 0),
            avg_ms: cell_f64(r, 1),
            max_ms: cell_f64(r, 2),
        });
    let worst_frames = sec
        .get("worst_frames")
        .map(|rows| rows.iter().map(|r| (cell_f64(r, 0), cell_f64(r, 1))).collect())
        .unwrap_or_default();
    Ok(Analysis {
        window_ms,
        pkg_cpu_ms,
        pkg_threads,
        pkg_runnable,
        top_procs,
        per_core,
        cpufreq,
        frame,
        worst_frames,
        notes,
    })
}

impl Analysis {
    pub fn report(&self, package: &str, rec: &RecordedTrace) -> String {
        let mut s = String::new();
        s.push_str(&format!("========== Perfetto 深挖分析（{}）==========\n", package));
        s.push_str(&format!(
            "trace 窗口: {} ~ {}（{:.1}s，boot 时间戳基线）\n",
            rec.wall_start.format("%H:%M:%S"),
            rec.wall_end.format("%H:%M:%S"),
            self.window_ms / 1000.0
        ));
        let pct = if self.window_ms > 0.0 {
            self.pkg_cpu_ms / self.window_ms * 100.0
        } else {
            0.0
        };
        s.push_str(&format!(
            "包 CPU 总量: {:.1} ms（{:.2}% 窗口，单核口径）\n",
            self.pkg_cpu_ms, pct
        ));
        s.push_str(&format!(
            "原始 trace: {}（{:.1} MB，可拖入 https://ui.perfetto.dev 交互分析）\n",
            rec.local_path.display(),
            rec.bytes as f64 / 1e6
        ));

        s.push_str("\n── 包线程 CPU 时间 top 15 ──\n");
        if self.pkg_threads.is_empty() {
            s.push_str("  窗口内该包无调度事件（进程未运行或完全空闲）\n");
        } else {
            for t in &self.pkg_threads {
                let p = if self.window_ms > 0.0 { t.cpu_ms / self.window_ms * 100.0 } else { 0.0 };
                s.push_str(&format!("  {:<32} (tid {:<6}) {:>9.2} ms  {:>5.2}%\n", t.name, t.tid, t.cpu_ms, p));
            }
        }

        s.push_str("\n── 抢占 / 调度延迟（Runnable：唤醒→上 CPU）──\n");
        if self.pkg_runnable.is_empty() {
            s.push_str("  无（包线程未被抢占或在等待外部事件而非 CPU）\n");
        } else {
            s.push_str("  线程                              次数      总等待     最长单次\n");
            for t in &self.pkg_runnable {
                s.push_str(&format!(
                    "  {:<32} (tid {:<6}) {:>5}  {:>8.2} ms  {:>7.2} ms\n",
                    t.name, t.tid, t.count, t.runnable_ms, t.max_ms
                ));
            }
        }

        s.push_str("\n── 同窗口系统 CPU top（谁在占核，已排除 idle）──\n");
        if self.top_procs.is_empty() {
            s.push_str("  无调度事件\n");
        } else {
            for (name, ms) in &self.top_procs {
                s.push_str(&format!("  {:<48} {:>10.1} ms\n", name, ms));
            }
        }

        if !self.per_core.is_empty() {
            s.push_str("\n── 每核 busy（非 idle）──\n");
            for c in &self.per_core {
                let p = if self.window_ms > 0.0 { c.busy_ms / self.window_ms * 100.0 } else { 0.0 };
                s.push_str(&format!(
                    "  cpu{}: {:>8.1} ms busy（{:>5.1}%）  {:>6} 次切换\n",
                    c.cpu, c.busy_ms, p, c.switches
                ));
            }
        }

        if self.cpufreq.is_empty() {
            s.push_str("\n── CPU 频率 ──\n  无 cpufreq ftrace 事件（如 SS3 GVM 频率归 hypervisor 管；实时值用 --freq 看 sysfs）\n");
        } else {
            s.push_str("\n── CPU 频率 ──\n");
            for f in &self.cpufreq {
                s.push_str(&format!(
                    "  cpu{}: min {:>6.0} / avg {:>6.0} / max {:>6.0} MHz\n",
                    f.cpu, f.min_mhz, f.avg_mhz, f.max_mhz
                ));
            }
        }

        s.push_str("\n── 帧时间线（全局；此平台无进程/图层归属）──\n");
        match &self.frame {
            Some(f) if f.frames > 0 => {
                s.push_str(&format!(
                    "  帧数 {}，平均 {:.2} ms，最差 {:.2} ms\n",
                    f.frames, f.avg_ms, f.max_ms
                ));
                if !self.worst_frames.is_empty() {
                    let cells: Vec<String> = self
                        .worst_frames
                        .iter()
                        .map(|(ts, dur)| format!("[{:.0}ms] {:.2}ms", ts, dur))
                        .collect();
                    s.push_str(&format!("  最差 {} 帧（boot 时刻 / 耗时）: {}\n", self.worst_frames.len(), cells.join(", ")));
                }
            }
            Some(_) => s.push_str("  无帧数据（录制期间无上屏帧）\n"),
            None => s.push_str("  frametimeline 数据源不可用（本段未执行或表不存在）\n"),
        }

        for n in &self.notes {
            s.push_str(&format!("\n⚠️ {}\n", n));
        }
        s
    }
}

/// 分析 + 落盘（trace_analysis.txt / trace_queries.sql 与 .pftrace 同目录），返回报告文本
/// （尾部附产物路径行）。失败时错误消息自带"trace 已保存 + 手动分析指引"。
/// 报告文件写失败不致命（警告拼进返回文本，报告仍然返回）。
pub fn analyze_and_report(rec: &RecordedTrace, package: &str) -> Result<String> {
    let tp = ensure_trace_processor().map_err(|e| {
        anyhow!(
            "trace_processor 不可用（{}）；trace 已保存，可拖入 https://ui.perfetto.dev 手动分析: {}",
            e,
            rec.local_path.display()
        )
    })?;
    let sql_path = rec.local_path.with_file_name("trace_queries.sql");
    let a = analyze(&tp, &rec.local_path, package, &sql_path).map_err(|e| {
        anyhow!(
            "trace 分析失败（{}）；trace 已保存，可拖入 https://ui.perfetto.dev 手动分析: {}",
            e,
            rec.local_path.display()
        )
    })?;
    let mut text = a.report(package, rec);
    let out_path = rec.local_path.with_file_name("trace_analysis.txt");
    if let Err(e) = std::fs::write(&out_path, &text) {
        text.push_str(&format!("\n⚠️ trace_analysis.txt 写入失败: {}\n", e));
    }
    text.push_str(&format!(
        "\n分析报告: {} | 查询语句: {}",
        out_path.display(),
        sql_path.display()
    ));
    Ok(text)
}

// ==================== 浏览器打开（ui.perfetto.dev 深链） ====================

/// 起本地 HTTP 服务（127.0.0.1 随机端口，只服务该 trace 文件），返回 ui.perfetto.dev 深链
/// （`#!/viewer?url=` 直开该 trace）。服务器线程随进程退出而终止——浏览器内刷新/重开
/// 需本进程仍存活（再次点击按钮可重新起服务）。
/// CORS：https 页面 fetch http://127.0.0.1 属 mixed content，但 Chrome/Firefox 均将
/// loopback 视为 potentially trustworthy 豁免拦截；仍需 Access-Control-Allow-Origin 头。
pub fn open_in_perfetto_ui(trace: &Path) -> Result<String> {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;

    let filename = trace
        .file_name()
        .context("trace 路径无文件名")?
        .to_string_lossy()
        .into_owned();
    let path = trace.to_path_buf();
    let listener = TcpListener::bind("127.0.0.1:0").context("绑定本地端口失败")?;
    let port = listener.local_addr().context("获取本地端口失败")?.port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            // 读请求头到空行（浏览器 GET/OPTIONS 头部 < 16KB）
            let mut buf = [0u8; 4096];
            let mut req = Vec::new();
            loop {
                match stream.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        req.extend_from_slice(&buf[..n]);
                        if req.windows(4).any(|w| w == b"\r\n\r\n") || req.len() > 16384 {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let is_options = req.starts_with(b"OPTIONS");
            let body = if is_options { Vec::new() } else { std::fs::read(&path).unwrap_or_default() };
            let status = if is_options { "204 No Content" } else { "200 OK" };
            let header = format!(
                "HTTP/1.1 {}\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\n\
                 Access-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, OPTIONS\r\n\
                 Access-Control-Allow-Headers: *\r\nConnection: close\r\n\r\n",
                status,
                body.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(&body);
        }
    });
    Ok(format!(
        "https://ui.perfetto.dev/#!/viewer?url=http://127.0.0.1:{}/{}",
        port, filename
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trace_config_fields() {
        let c = trace_config(10);
        assert!(c.contains("duration_ms: 10000"));
        assert!(c.contains("sched_switch"));
        assert!(c.contains("cpu_frequency"));
        assert!(c.contains("linux.process_stats"));
        assert!(c.contains("android.surfaceflinger.frametimeline"));
        assert!(c.contains("write_into_file: true"));
    }

    #[test]
    fn test_build_sql_escapes_and_orders() {
        let q = build_sql("com.lixiang.car.x.svm");
        assert!(q.contains("'com.lixiang.car.x.svm'"));
        // 单引号转义为两个单引号（防注入）
        let q2 = build_sql("a'b");
        assert!(q2.contains("'a''b'"));
        // 帧时间线表查询在 END 哨兵之前、cpufreq 之后（表缺失时只损失末尾段）
        let pos_freq = q.find("cpu_counter_track").unwrap();
        let pos_frame = q.find("actual_frame_timeline_slice").unwrap();
        let pos_end = q.rfind("===END===").unwrap();
        assert!(pos_freq < pos_frame && pos_frame < pos_end);
        // idle（swapper，utid=0）必须从系统级聚合排除：不排除则空闲机器每核
        // "busy" 恒 ~100%、(内核线程) 桶被 idle 时间淹没
        let pos_top = q.find("top_procs").unwrap();
        let top_seg = &q[pos_top..pos_top + 400];
        assert!(top_seg.contains("s.utid != 0"));
        let pos_core = q.find("per_core").unwrap();
        let core_seg = &q[pos_core..pos_core + 300];
        assert!(core_seg.contains("utid != 0"));
        // marker 分段齐全
        for s in ["bounds", "pkg_total", "pkg_threads", "pkg_runnable", "top_procs", "per_core", "cpufreq", "frame_stats", "worst_frames", "END"] {
            assert!(q.contains(&format!("==={}===", s)), "missing marker {}", s);
        }
    }

    #[test]
    fn test_parse_csv_line() {
        assert_eq!(parse_csv_line(r#""a","b""#), vec!["a", "b"]);
        assert_eq!(parse_csv_line(r#""x,y","z""#), vec!["x,y", "z"]);
        assert_eq!(parse_csv_line(r#""he said ""hi""","1""#), vec!["he said \"hi\"", "1"]);
        assert_eq!(parse_csv_line("plain"), vec!["plain"]);
    }

    #[test]
    fn test_parse_sections() {
        // 实测输出格式：表头 + 数据行 + 空行分隔；marker 为单列结果集
        let out = concat!(
            "\"m\"\n\"===pkg_total===\"\n\n",
            "\"cpu_ms\"\n\"12.5\"\n\n",
            "\"m\"\n\"===pkg_threads===\"\n\n",
            "\"tid\",\"thread\",\"cpu_ms\"\n8166,\"Thread-6\",1.969\n\n",
            "\"m\"\n\"===END===\"\n\n"
        );
        let sec = parse_sections(out);
        assert_eq!(sec["pkg_total"][0][0], "12.5");
        assert_eq!(sec["pkg_threads"][0][1], "Thread-6");
        assert_eq!(sec["pkg_threads"][0][2], "1.969");
        assert!(sec.contains_key("END"));
        assert!(sec["END"].is_empty());
        // 空段登记（表存在但 0 行）
        let out2 = "\"m\"\n\"===cpufreq===\"\n\n\"m\"\n\"===END===\"\n\n";
        let sec2 = parse_sections(out2);
        assert!(sec2.contains_key("cpufreq") && sec2["cpufreq"].is_empty());
    }

    #[test]
    fn test_cell_null_handling() {
        let row = vec!["[NULL]".to_string(), "3.5".to_string()];
        assert_eq!(cell_f64(&row, 0), 0.0);
        assert_eq!(cell_f64(&row, 1), 3.5);
        assert_eq!(cell_u64(&row, 1), 3);
        assert_eq!(cell_str(&row, 0), "[NULL]");
    }

    #[test]
    fn test_open_in_perfetto_ui_serves_file() {
        use std::io::{Read as _, Write as _};
        let dir = std::env::temp_dir().join(format!("xperf_tp_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("t.pftrace");
        std::fs::write(&f, b"hello-trace").unwrap();
        let url = open_in_perfetto_ui(&f).unwrap();
        assert!(url.starts_with("https://ui.perfetto.dev/#!/viewer?url=http://127.0.0.1:"));
        assert!(url.ends_with("/t.pftrace"));
        let addr = url.split("url=http://").nth(1).unwrap().split('/').next().unwrap().to_string();
        // GET：200 + CORS 头 + 文件内容
        let mut s = std::net::TcpStream::connect(&addr).unwrap();
        s.write_all(b"GET /t.pftrace HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).unwrap();
        let resp = String::from_utf8_lossy(&buf);
        assert!(resp.starts_with("HTTP/1.1 200 OK"), "resp: {}", &resp[..60.min(resp.len())]);
        assert!(resp.contains("Access-Control-Allow-Origin: *"));
        assert!(resp.contains("Content-Length: 11"));
        assert!(resp.ends_with("hello-trace"));
        // OPTIONS 预检：204
        let mut s = std::net::TcpStream::connect(&addr).unwrap();
        s.write_all(b"OPTIONS /t.pftrace HTTP/1.1\r\nHost: x\r\nOrigin: https://ui.perfetto.dev\r\nConnection: close\r\n\r\n").unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).unwrap();
        assert!(String::from_utf8_lossy(&buf).starts_with("HTTP/1.1 204"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_report_renders_sections() {
        let rec = RecordedTrace {
            local_path: PathBuf::from("/tmp/x.pftrace"),
            wall_start: Local::now(),
            wall_end: Local::now(),
            bytes: 1024,
        };
        let a = Analysis {
            window_ms: 10000.0,
            pkg_cpu_ms: 150.0,
            pkg_threads: vec![ThreadCpuMs { tid: 8166, name: "Thread-6".into(), cpu_ms: 100.0 }],
            pkg_runnable: vec![ThreadRunnable { tid: 8166, name: "Thread-6".into(), count: 3, runnable_ms: 5.0, max_ms: 2.5 }],
            top_procs: vec![("(内核线程)".to_string(), 60000.0)],
            per_core: vec![CoreBusy { cpu: 0, busy_ms: 9900.0, switches: 7351 }],
            cpufreq: vec![],
            frame: Some(FrameStats { frames: 226, avg_ms: 14.6, max_ms: 16.4 }),
            worst_frames: vec![(10991912.4, 16.39)],
            notes: vec![],
        };
        let r = a.report("com.lixiang.car.x.svm", &rec);
        assert!(r.contains("包 CPU 总量: 150.0 ms（1.50% 窗口"));
        assert!(r.contains("Thread-6"));
        assert!(r.contains("无 cpufreq ftrace 事件"));
        assert!(r.contains("帧数 226"));
        assert!(r.contains("ui.perfetto.dev"));
        // 空包线程场景
        let a2 = Analysis { pkg_threads: vec![], frame: None, ..a };
        let r2 = a2.report("p", &rec);
        assert!(r2.contains("窗口内该包无调度事件"));
        assert!(r2.contains("frametimeline 数据源不可用"));
    }
}
