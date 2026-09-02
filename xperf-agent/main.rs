//! xperf-agent：设备端常驻采样器。
//!
//! 低间隔（<500ms）采样时主机侧 adb 轮询不可行（单轮多次 adb 调用的开销就超过
//! 间隔本身），因此把采样循环搬到设备上：直接读 /proc（微秒级），结果以 NDJSON
//! 行流式写 stdout，主机用 `adb exec-out` 一条长连接持续读取。
//!
//! 协议（每行一个 JSON 对象）：
//!   {"t":"hello","ncores":8,"version":1}
//!   {"t":"cpu","ts":<wall_ms>,"pid":29697,"cpu":15.4,"th":[[29697,"main",5.1],...]}
//!   {"t":"mem","ts":<wall_ms>,"pid":29697,"pss":484880,"rss":612000,
//!    "java":..,"native":..,"code":..,"stack":..,"gfx":..,"other":..,"sys":..}
//!   （内存分类字段仅 interval≥500ms 的 dumpsys meminfo 路径有值，低间隔 smaps_rollup 路径为 0）
//!   {"t":"fps","ts":<wall_ms>,"pid":29697,"layer":"SVM Container#0","fps":30.0,"frames":32,"jank":0}
//!   {"t":"exit","pid":29697}
//!   {"t":"noproc"}
//!   {"t":"err","msg":"..."}
//!
//! 用法：xperf-agent --package <pkg> [--pid N]... --interval 50 [--cpu] [--memory] [--fps]
//!
//! 与主机侧 adb 模式的差异：
//! - CPU 口径相同（jiffies 差值 ×核数，单核基准），但窗口是相邻两轮之间
//!   （agent 常驻保有上一轮状态，无需主机侧 phase1/phase2 结构）
//! - 内存：interval ≥ 500ms 用本地 dumpsys meminfo（全分类明细，同轮询模式）；
//!   低间隔改读 /proc/<pid>/smaps_rollup（Pss/Rss，~1ms）
//! - FPS：设备端本地 dumpsys SurfaceFlinger（无 adb 中转，图层名无需引号转义）；
//!   限频至 ≥500ms 周期（每 fps_every_n_rounds 轮一次），与 CPU/内存节拍解耦——
//!   低间隔下每轮跑 dumpsys SurfaceFlinger 会拖垮节拍（实测 50ms 间隔约半数轮次 overrun）

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
    fps: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut package = None;
    let mut pids = Vec::new();
    let mut interval_ms = 50u64;
    let mut cpu = false;
    let mut memory = false;
    let mut fps = false;
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
            "--fps" => fps = true,
            other => return Err(format!("未知参数: {}", other)),
        }
        i += 1;
    }
    if package.is_none() && pids.is_empty() {
        return Err("需要 --package 或 --pid".into());
    }
    if !cpu && !memory && !fps {
        return Err("需要 --cpu / --memory / --fps 至少一个".into());
    }
    if interval_ms == 0 {
        return Err("--interval 不能为 0".into());
    }
    Ok(Args { package, pids, interval_ms, cpu, memory, fps })
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

// ---------- FPS（SurfaceFlinger 图层帧时间戳，设备端本地 dumpsys）----------
// 与 xperf-core/fps.rs 同源的解析逻辑（agent 零依赖独立发布，有意复制而非共享）。
// 设备端用 Command 直接 exec dumpsys，无 adb shell 拼接，图层名无需引号。

/// 连续零帧达到该采样轮数后重新发现图层（Surface 重建会换名，如 #0 → #1）。
/// 计的是 FPS 采样轮（限频后每轮 ≥500ms，即 ≥5s 持续零帧才重发现）。
const FPS_REDISCOVER_ZERO_ROUNDS: u32 = 10;

/// FPS 采样限频：每多少主循环轮采一次，保证 FPS 有效周期 ≥500ms。
/// dumpsys SurfaceFlinger 单轮开销 ~100ms 级，低间隔下每轮执行会拖垮节拍。
fn fps_every_n_rounds(interval_ms: u64) -> u64 {
    500u64.div_ceil(interval_ms).max(1)
}

struct FpsLayerState {
    name: String,
    /// 上轮时刻 + 上轮缓冲末尾时间戳；None = 未建基线
    last: Option<(Instant, Option<u64>)>,
}

#[derive(Default)]
struct FpsState {
    layers: Vec<FpsLayerState>,
    attempted: bool,
    zero_rounds: u32,
}

/// 解析 --latency 输出：首行刷新周期（跳过），随后每行三列取 actualPresent。
/// 过滤 0（空槽）和 i64::MAX（已入队未上屏哨兵）。
fn parse_latency_output(output: &str) -> Vec<u64> {
    output
        .lines()
        .skip(1)
        .filter_map(|line| {
            let mut cols = line.split_whitespace();
            cols.next()?;
            let actual: u64 = cols.next()?.parse().ok()?;
            (actual > 0 && actual < i64::MAX as u64).then_some(actual)
        })
        .collect()
}

/// jank：相邻帧间隔 > 2×窗口中位间隔（<3 间隔不计；含跨窗口边界帧）。
/// 不按 vsync 阈值：30fps 相机流在 60Hz 屏上间隔 33ms 会被误判全卡。
fn count_jank(prev: Option<u64>, presents: &[u64]) -> u32 {
    let mut intervals: Vec<u64> = Vec::with_capacity(presents.len());
    let mut last = prev;
    for &p in presents {
        if let Some(l) = last {
            intervals.push(p.saturating_sub(l));
        }
        last = Some(p);
    }
    if intervals.len() < 3 {
        return 0;
    }
    let mid = intervals.len() / 2;
    let median = *intervals.select_nth_unstable(mid).1;
    let threshold = median * 2;
    intervals.iter().filter(|&&d| d > threshold).count() as u32
}

/// 全量 dumpsys 解析：BufferStateLayer 块 metadata 的 ownerPID 归属匹配。
/// （直渲染应用的 SurfaceView 图层名常不含包名，如 "SVM Container"，只能靠归属识别）
fn parse_owned_buffer_layers(dump: &str, pid: u32) -> Vec<String> {
    let owner_marker = format!("ownerPID:{}", pid);
    let mut layers = Vec::new();
    let mut current: Option<String> = None;
    for line in dump.lines() {
        if let Some(pos) = line.find("BufferStateLayer (") {
            let start = pos + "BufferStateLayer (".len();
            current = line[start..].find(')').map(|end| line[start..start + end].to_string());
        } else if line.contains("Layer (") {
            current = None;
        } else if let Some(name) = &current {
            if line.contains(&owner_marker) {
                layers.push(name.clone());
                current = None;
            }
        }
    }
    layers
}

/// --list 解析：保留包名匹配的行，去 "<hex> " 别名前缀，去重。
fn parse_list_layers(list: &str, package: &str) -> Vec<String> {
    let mut layers = Vec::new();
    for line in list.lines() {
        let line = line.trim();
        if !line.contains(package) {
            continue;
        }
        let name = match line.split_once(' ') {
            Some((head, rest)) if !head.is_empty() && head.chars().all(|c| c.is_ascii_hexdigit()) => rest.trim(),
            _ => line,
        };
        if !layers.contains(&name.to_string()) {
            layers.push(name.to_string());
        }
    }
    layers
}

fn dumpsys(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("dumpsys").args(args).output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn sf_discover_layers(pid: u32, package: &str) -> Vec<String> {
    let by_owner = dumpsys(&["SurfaceFlinger"])
        .map(|s| parse_owned_buffer_layers(&s, pid))
        .unwrap_or_default();
    if !by_owner.is_empty() {
        return by_owner;
    }
    dumpsys(&["SurfaceFlinger", "--list"])
        .map(|s| parse_list_layers(&s, package))
        .unwrap_or_default()
}

/// 某 PID 一轮 FPS 采样：有帧图层各发一行；全零时发一条零帧行（界面静止是真实状态）。
/// 图层发现：首轮 + 连续零帧达阈值后重做。包名用于兜底匹配（ownerPID 是首选）。
/// 发现为空（进程刚重启 Surface 未建，或应用本无界面）也记零帧轮，
/// 靠阈值节流重试——全量 dump 在此车机 ~1.5s，不能每轮试。
fn fps_sample_round(st: &mut FpsState, pid: u32, package: &str, ts: u64) {
    if !st.attempted || st.zero_rounds >= FPS_REDISCOVER_ZERO_ROUNDS {
        st.layers = sf_discover_layers(pid, package)
            .into_iter()
            .map(|name| FpsLayerState { name, last: None })
            .collect();
        st.attempted = true;
        st.zero_rounds = 0;
    }
    if st.layers.is_empty() {
        st.zero_rounds += 1;
        return;
    }

    let now = Instant::now();
    let mut samples: Vec<(String, f32, u32, u32)> = Vec::new();
    for layer in &mut st.layers {
        let presents = dumpsys(&["SurfaceFlinger", "--latency", &layer.name])
            .map(|s| parse_latency_output(&s))
            .unwrap_or_default();
        let latest = presents.last().copied();
        let Some((last_t, last_p)) = layer.last.replace((now, latest)) else {
            continue; // 首轮建基线
        };
        let elapsed = (now - last_t).as_secs_f32();
        if elapsed <= 0.0 {
            continue;
        }
        let new_frames: Vec<u64> = match last_p {
            Some(lp) => presents.into_iter().filter(|&t| t > lp).collect(),
            None => presents,
        };
        let fps = new_frames.len() as f32 / elapsed;
        let jank = count_jank(last_p, &new_frames);
        samples.push((layer.name.clone(), fps, new_frames.len() as u32, jank));
    }
    if samples.is_empty() {
        return;
    }

    if samples.iter().any(|s| s.2 > 0) {
        st.zero_rounds = 0;
        samples.retain(|s| s.2 > 0); // 静止图层是噪声，不上报
    } else {
        st.zero_rounds += 1;
        samples.truncate(1); // 全零：一条静止样本即可
    }
    for (layer, fps, frames, jank) in samples {
        emit(&format!(
            "{{\"t\":\"fps\",\"ts\":{},\"pid\":{},\"layer\":\"{}\",\"fps\":{:.2},\"frames\":{},\"jank\":{}}}",
            ts, pid, json_escape(&layer), fps, frames, jank
        ));
    }
}

// ---------- 内存分类明细（dumpsys meminfo，interval ≥ 500ms 时启用）----------
// 低间隔下 dumpsys meminfo 太重（~100ms），退化到 smaps_rollup（只有 Pss/Rss）。

#[derive(Default)]
struct MemBreakdown {
    java: u64,
    native_: u64,
    code: u64,
    stack: u64,
    gfx: u64,
    other: u64,
    sys: u64,
    total: u64,
}

/// 解析 dumpsys meminfo 的 App Summary（与 xperf-core/memory.rs 同逻辑）。
/// 注意真机格式：分类行之后有一个空行，然后才是 "TOTAL PSS: ... TOTAL RSS: ..." 行——
/// 空行会结束 App Summary 区块，所以 TOTAL 必须在区块外用兜底逻辑找。
fn parse_meminfo_summary(output: &str) -> Option<MemBreakdown> {
    let mut bd = MemBreakdown::default();
    let mut in_summary = false;
    let mut header_passed = false;
    for line in output.lines() {
        let line = line.trim();
        if line.contains("App Summary") {
            in_summary = true;
            continue;
        }
        if in_summary && (line.contains("Pss(KB)") || line.contains("------")) {
            header_passed |= line.contains("------");
            continue;
        }
        if in_summary && line.is_empty() {
            in_summary = false;
            continue;
        }
        if in_summary && header_passed {
            if let Some((cat, rest)) = line.split_once(':') {
                if let Some(Ok(kb)) = rest.split_whitespace().next().map(|s| s.parse::<u64>()) {
                    match cat.trim() {
                        "Java Heap" => bd.java = kb,
                        "Native Heap" => bd.native_ = kb,
                        "Code" => bd.code = kb,
                        "Stack" => bd.stack = kb,
                        "Graphics" => bd.gfx = kb,
                        "Private Other" => bd.other = kb,
                        "System" => bd.sys = kb,
                        "TOTAL" | "TOTAL PSS" => bd.total = kb,
                        _ => {}
                    }
                }
            }
        }
        // 兜底：TOTAL PSS 在 App Summary 空行之后（区块外）
        if !in_summary {
            if let Some(rest) = line.strip_prefix("TOTAL PSS:") {
                if let Some(Ok(kb)) = rest.split_whitespace().next().map(|s| s.parse::<u64>()) {
                    bd.total = kb;
                }
            }
        }
    }
    (bd.total > 0).then_some(bd)
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
    let mut fps_states: HashMap<u32, FpsState> = HashMap::new();
    let mut active_pids: Vec<u32> = args.pids.clone();

    // 内存分类明细仅在间隔 ≥500ms 时启用（dumpsys meminfo ~100ms，低间隔下太重）
    let full_meminfo = args.memory && args.interval_ms >= 500;
    // --pid 模式下 FPS 兜底匹配需要包名：从 /proc/<pid>/cmdline 反查（一次性缓存）
    let mut pkg_cache: HashMap<u32, String> = HashMap::new();

    // FPS 预热：图层发现的全量 dumpsys SurfaceFlinger 在此车机 ~1.5s，
    // 放在节拍时钟开始前执行，避免首轮 backlog、后续追帧期 CPU 窗口不齐。
    // （进程尚未启动时此处无 PID，发现会推迟到循环内首次 FPS 轮，代价同上但仅一次）
    if args.fps {
        if let Some(pkg) = &args.package {
            for pid in resolve_pids(pkg) {
                if !active_pids.contains(&pid) {
                    active_pids.push(pid);
                }
            }
        }
        for &pid in &active_pids.clone() {
            let pkg = match &args.package {
                Some(p) => p.clone(),
                None => pkg_cache
                    .entry(pid)
                    .or_insert_with(|| {
                        fs::read_to_string(format!("/proc/{}/cmdline", pid))
                            .map(|s| s.trim_end_matches('\0').to_string())
                            .unwrap_or_default()
                    })
                    .clone(),
            };
            // 首轮仅建图层列表 + 帧时间戳基线，不出数
            fps_sample_round(fps_states.entry(pid).or_default(), pid, &pkg, now_ms());
        }
    }

    // 绝对节拍：按起始时间推算每轮时刻，避免 sleep 累积漂移
    let interval = Duration::from_millis(args.interval_ms);
    let start = Instant::now();
    let mut round: u64 = 0;
    // 进程列表重扫间隔：约 1s 一次（低间隔下每轮扫 /proc 太贵）
    let rescan_rounds = (1000 / args.interval_ms).max(1);
    // FPS 限频：与 CPU/内存节拍解耦，有效周期 ≥500ms（50ms 间隔 → 每 10 轮一次）
    let fps_every = fps_every_n_rounds(args.interval_ms);

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

            // 内存：≥500ms 用 dumpsys meminfo（全分类明细）；低间隔用 smaps_rollup（Pss/Rss）
            if args.memory {
                if full_meminfo {
                    if let Some(bd) = dumpsys(&["meminfo", &pid.to_string()])
                        .and_then(|s| parse_meminfo_summary(&s))
                    {
                        // rss 不在 App Summary 里，从 smaps_rollup 补（~1ms）
                        let rss = read_smaps_rollup(pid).map(|(_, r)| r).unwrap_or(0);
                        emit(&format!(
                            "{{\"t\":\"mem\",\"ts\":{},\"pid\":{},\"pss\":{},\"rss\":{},\"java\":{},\"native\":{},\"code\":{},\"stack\":{},\"gfx\":{},\"other\":{},\"sys\":{}}}",
                            ts, pid, bd.total, rss, bd.java, bd.native_, bd.code, bd.stack, bd.gfx, bd.other, bd.sys
                        ));
                    }
                } else if let Some((pss, rss)) = read_smaps_rollup(pid) {
                    emit(&format!(
                        "{{\"t\":\"mem\",\"ts\":{},\"pid\":{},\"pss\":{},\"rss\":{},\"java\":0,\"native\":0,\"code\":0,\"stack\":0,\"gfx\":0,\"other\":0,\"sys\":0}}",
                        ts, pid, pss, rss
                    ));
                }
            }

            // FPS：设备端本地 dumpsys SurfaceFlinger（图层发现 + 帧时间戳差值）。
            // 限频执行（每 fps_every 轮一次）；启动时已预热建基线，
            // 首个 FPS 轮（round == fps_every）即覆盖一个完整周期。
            if args.fps && round.is_multiple_of(fps_every) {
                let pkg = match &args.package {
                    Some(p) => p.clone(),
                    None => pkg_cache.entry(pid).or_insert_with(|| {
                        fs::read_to_string(format!("/proc/{}/cmdline", pid))
                            .map(|s| s.trim_end_matches('\0').to_string())
                            .unwrap_or_default()
                    }).clone(),
                };
                let st = fps_states.entry(pid).or_default();
                fps_sample_round(st, pid, &pkg, ts);
            }
        }

        for pid in exited {
            active_pids.retain(|&p| p != pid);
            states.remove(&pid);
            fps_states.remove(&pid);
            pkg_cache.remove(&pid);
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

    #[test]
    fn test_fps_every_n_rounds() {
        // FPS 有效周期 ≥500ms：低间隔限频，≥500ms 每轮都采
        assert_eq!(fps_every_n_rounds(50), 10); // 50ms → 每 10 轮（500ms）
        assert_eq!(fps_every_n_rounds(300), 2); // 300ms → 每 2 轮（600ms）
        assert_eq!(fps_every_n_rounds(500), 1);
        assert_eq!(fps_every_n_rounds(1000), 1);
    }

    // ---- FPS / meminfo 解析（与 xperf-core 同源逻辑的设备端副本）----

    #[test]
    fn test_parse_latency_filters_zero_and_sentinel() {
        // 真机实测：空槽为 0；已入队未上屏的行 actualPresent = i64::MAX
        let out = "16666666\n2102436563608017\t2102436592973938\t2102436567588086\n0\t0\t0\n2102436613000155\t9223372036854775807\t2102436617210308\n";
        assert_eq!(parse_latency_output(out), vec![2102436592973938]);
    }

    #[test]
    fn test_count_jank_30fps_on_60hz_not_janky() {
        // 30fps 相机流在 60Hz 屏上帧间隔 33ms：中位数自适应，不能误判全卡
        let p0 = 1_000_000_000u64;
        let presents: Vec<u64> = (0..10).map(|i| p0 + i * 33_333_333).collect();
        assert_eq!(count_jank(None, &presents), 0);
    }

    #[test]
    fn test_count_jank_detects_stall() {
        let p0 = 1_000_000_000u64;
        let presents = vec![p0, p0 + 16_666_666, p0 + 33_333_332, p0 + 33_333_332 + 80_000_000];
        assert_eq!(count_jank(None, &presents), 1);
    }

    #[test]
    fn test_parse_owned_buffer_layers() {
        let dump = "+ BufferStateLayer (SVM Container#0) uid=1000\n\
                    \x20     metadata={dequeueTime:123, ownerPID:29697, ownerUID:1000}\n\
                    + BufferStateLayer (com.other/Main#0) uid=10123\n\
                    \x20     metadata={ownerPID:9999}\n";
        assert_eq!(parse_owned_buffer_layers(dump, 29697), vec!["SVM Container#0".to_string()]);
    }

    #[test]
    fn test_parse_list_layers_strips_hex_alias() {
        let list = "147955a com.pkg/com.pkg.MainActivity#0\ncom.pkg/com.pkg.MainActivity#0\ncom.other/Main#0\n";
        assert_eq!(parse_list_layers(list, "com.pkg"), vec!["com.pkg/com.pkg.MainActivity#0".to_string()]);
    }

    #[test]
    fn test_parse_meminfo_summary() {
        // 真机格式：双列（Pss/Rss），分类行与 TOTAL PSS 之间隔一个空行
        let out = " App Summary\n\
                   \x20                       Pss(KB)                        Rss(KB)\n\
                   \x20                        ------                         ------\n\
                   \x20           Java Heap:     8296                          31012\n\
                   \x20         Native Heap:   117492                         122204\n\
                   \x20                Code:    36100                         147340\n\
                   \x20               Stack:     2192                           2204\n\
                   \x20            Graphics:        0                              0\n\
                   \x20       Private Other:   314880\n\
                   \x20              System:     5053\n\
                   \x20             Unknown:                                  336624\n\
                   \x20\n\
                   \x20           TOTAL PSS:   484013            TOTAL RSS:   639384      TOTAL SWAP (KB):        0\n\
                   \x20\n\
                   \x20Objects\n";
        let bd = parse_meminfo_summary(out).unwrap();
        assert_eq!(bd.total, 484013);
        assert_eq!(bd.java, 8296);
        assert_eq!(bd.native_, 117492);
        assert_eq!(bd.code, 36100);
    }

    #[test]
    fn test_parse_meminfo_summary_no_total() {
        assert!(parse_meminfo_summary("garbage\n").is_none());
    }
}
