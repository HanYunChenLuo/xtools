//! simpleperf 调用栈采样：函数级 CPU 热点定位（回答"CPU 高在哪个函数"）。
//! CLI（xperformance）与 GUI（xperf-gui）共用——输出目录由调用方传入，报告以文本返回，
//! 进度/打印均由调用方负责（CLI println，GUI 走 Tauri 事件）。
//!
//! 定位：与实时采样、perfetto 深挖互补的三级下钻——采样回答"什么时候高"，
//! perfetto 回答"线程/调度/帧层面为什么高"，本模块回答"函数层面高在哪"：
//! - 线程 CPU 分布（哪个线程在吃 CPU）
//! - 函数热点 self（CPU 时间直接落在哪个函数内——"CPU 高在哪个函数"的直接回答）
//! - 函数热点 children（`--children` 调用链累计开销，热点路径归因）
//!
//! 链路：`adb shell simpleperf record --app <pkg> -g --duration N -o /data/local/tmp/…`
//! （cpu-cycles 事件默认 4000Hz + dwarf 调用栈；`--app` 覆盖该应用全部进程并容忍进程
//! 重启）→ 设备端 `simpleperf report` 生成三视图（报告须在 pull 前跑，`.data` 还在设备
//! 上）→ adb pull 落盘 `log/<pkg>/<会话时间戳>/stack/`（`.data` 保留，可推回设备换参数
//! 复跑 report）。
//!
//! 实测基线与坑（SS3，simpleperf 1.build.47，adbd root，2026-09-04）：
//! - svm 空闲态 8s 录得 8773 样本 / 0 丢失 / 3.3MB（实际样本率 ≈1100/s，随 CPU 活动浮动）
//! - **`--app` 对未运行应用输出 `Waiting for process of app …` 无限等待**（`--duration`
//!   拦不住——等待发生在采样开始前）→ 录制前必须 `pidof` 前置检查 + 主机侧超时兜底
//! - 设备端 so 多为 stripped（无符号表）：函数名显示为 `libxxx.so[+偏移]`，系统库
//!   （libc 等）有符号——偏移可用未剥离 so 离线符号化，报告中如实标注
//! - 非 root 设备上非 debuggable 应用会被 `run-as` 路径拒绝，错误由 simpleperf 透传
//! - report 输出为 header 定宽对齐文本（Symbol 列按最长符号名对齐，可达数百列宽），
//!   行解析用「首 token/尾 token 锚定」而非列位置切片

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Local};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

// ==================== 录制 ====================

/// 录制完成的调用栈数据（拉回主机后的产物描述）
pub struct RecordedStack {
    /// 拉回到主机的 simpleperf 数据文件路径（`perf.data` 格式，可推回设备复跑 report）
    pub local_path: PathBuf,
    /// 设备端生成的原始三视图报告路径（线程分布/self/children 函数热点，完整未截断）
    pub report_path: PathBuf,
    /// 录制发起时刻（墙钟，含 adb 启动开销）
    pub wall_start: DateTime<Local>,
    /// 录制完成时刻（墙钟）
    pub wall_end: DateTime<Local>,
    /// 数据文件大小（字节）
    pub bytes: u64,
    /// 录得样本数（record 输出解析；解析失败为 0，不代表无数据）
    pub samples: u64,
    /// 丢失样本数（record 输出解析）
    pub samples_lost: u64,
}

/// 校验包名字符集（`[A-Za-z0-9._-]`，与 CLI `validate_package_name` / GUI 校验一致）。
/// 包名会拼进 `adb shell` 命令行，调用方虽已校验，此处防御性再拦一次（防 shell 注入）。
fn validate_package(package: &str) -> Result<()> {
    let ok = !package.is_empty()
        && package
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-');
    if !ok {
        bail!("包名不合法（仅允许字母/数字/./_/-）: {}", package);
    }
    Ok(())
}

/// 解析 record 输出中的样本统计（`Samples recorded: 8773. Samples lost: 0.`，
/// simpleperf 日志行内嵌）。未匹配返回 `(0, 0)`。
fn parse_sample_stats(output: &str) -> (u64, u64) {
    let recorded = output
        .find("Samples recorded:")
        .and_then(|i| {
            output[i + "Samples recorded:".len()..]
                .trim_start()
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse()
                .ok()
        })
        .unwrap_or(0);
    let lost = output
        .find("Samples lost:")
        .and_then(|i| {
            output[i + "Samples lost:".len()..]
                .trim_start()
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse()
                .ok()
        })
        .unwrap_or(0);
    (recorded, lost)
}

/// 应用是否有运行中的进程（主进程名 = 包名）。simpleperf `--app` 对未启动应用会
/// 无限等待（实测 `--duration` 拦不住），录制前必须前置检查。
fn app_has_process(package: &str, serial: Option<&str>) -> Result<bool> {
    let out = crate::utils::adb_for(serial)
        .args(["shell", "pidof", package])
        .output()
        .context("执行 adb shell pidof 失败")?;
    Ok(!String::from_utf8_lossy(&out.stdout).trim().is_empty())
}

/// 录制 N 秒调用栈并生成三视图报告，拉回 out_dir（如 `log/<pkg>/<会话时间戳>/stack/`，
/// 自动创建）。阻塞至录制完成（simpleperf `--duration` 到点自动退出）。
///
/// 流程：包名校验（防 shell 注入）→ simpleperf 存在性检查 → `pidof` 前置拦截 →
/// `simpleperf record --app <pkg> -g --duration N`（主机侧超时与 Ctrl-C 兜底）→
/// 设备端三视图 report（须在 pull 前跑，`.data` 还在设备上）→ adb pull → 清理设备端。
///
/// `progress`：录制进度回调（GUI 走事件推进度用）；每到整秒以已录制秒数（1..=N）
/// 触发一次，录制等待循环内同步调用。`None` = 不上报（CLI 打印会刷屏，默认不传）。
///
/// 包名校验与进度展示由调用方负责（目录路径含包名，防遍历）。
/// `serial`：目标设备（多设备并行会话用，`None` 回退全局选择）。
pub fn record(
    seconds: u64,
    package: &str,
    out_dir: &Path,
    progress: Option<&dyn Fn(u64)>,
    serial: Option<&str>,
) -> Result<RecordedStack> {
    validate_package(package)?;
    std::fs::create_dir_all(out_dir)?;
    let stem = Local::now().format("%Y%m%d_%H%M%S").to_string();
    let local_path = out_dir.join(format!("stack_{}.data", stem));
    let report_path = out_dir.join("simpleperf_report.txt");
    let dev_path = format!("/data/local/tmp/xperf_stack_{}.data", stem);

    // 设备端 simpleperf 存在性检查（错误信息直接可读，避免 record 报 "not found"）
    let probe = crate::utils::adb_for(serial)
        .args(["shell", "which", "simpleperf"])
        .output()
        .context("执行 adb shell which 失败")?;
    if String::from_utf8_lossy(&probe.stdout).trim().is_empty() {
        bail!("设备端无 simpleperf（/system/bin/simpleperf 不存在）");
    }
    // --app 对未运行应用无限等待，前置拦截（--duration 拦不住，等待在采样开始前）
    if !app_has_process(package, serial)? {
        bail!("应用 {} 无运行中的进程，请先启动应用再采样", package);
    }

    let wall_start = Local::now();
    // stderr 合并进 stdout（设备端 2>&1）：simpleperf 的样本统计与报错都走日志行
    let mut child = crate::utils::adb_for(serial)
        .args([
            "shell", "simpleperf", "record", "--app", package, "-g",
            "--duration", &seconds.to_string(), "-o", &dev_path, "2>&1",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("启动 adb shell simpleperf record 失败")?;
    // 手动超时兜底（同 trace 模式）：--duration 自会退出，但 --app 在进程全部死亡后
    // 会退回等待态，须有上限；Ctrl-C 全局中断标志可提前放弃
    let deadline = Instant::now() + Duration::from_secs(seconds + 25);
    let started = Instant::now();
    let mut last_reported: u64 = 0;
    loop {
        // 进度上报放循环头：adb 启动开销 ~0.5s 会推迟首个整秒，若只在 None 分支报，
        // 10s 录制只能看到 1..=9（最后一秒在 try_wait=Some 的轮次里被跳过）
        if let Some(cb) = progress {
            let elapsed = started.elapsed().as_secs();
            if elapsed > last_reported {
                last_reported = elapsed;
                cb(elapsed);
            }
        }
        match child.try_wait()? {
            Some(_) => break,
            None if crate::utils::is_interrupted() => {
                let _ = child.kill();
                bail!("录制被 Ctrl-C 中断；设备端可能残留 {}（可手动 adb pull）", dev_path);
            }
            None if Instant::now() > deadline => {
                let _ = child.kill();
                bail!(
                    "simpleperf 录制超时（>{}s）。设备端残留 {} 可手动 adb pull 分析",
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
            "录制被中断（如 Ctrl-C）；设备端可能残留 {}（可手动 adb pull）",
            dev_path
        );
    }
    let record_log = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        bail!("simpleperf 录制失败: {}", record_log.trim());
    }
    let (samples, samples_lost) = parse_sample_stats(&record_log);

    // 设备端三视图 report（pull 前执行：.data 尚在设备上；警告走 /dev/null 不污染视图）
    let thread_view = device_report(&dev_path, &[
        "--sort", "comm,pid,tid", "--percent-limit", "1",
    ], serial);
    let self_view = device_report(&dev_path, &[
        "--sort", "symbol,dso", "--percent-limit", "1",
    ], serial);
    let symbol_view = device_report(&dev_path, &[
        "--children", "--sort", "symbol,dso", "--percent-limit", "1",
    ], serial);
    let mut report_text = String::new();
    report_text.push_str("================ 线程 CPU 分布（--sort comm,pid,tid） ================\n");
    match &thread_view {
        Ok(t) => report_text.push_str(t),
        Err(e) => report_text.push_str(&format!("[线程分布生成失败: {}]\n", e)),
    }
    report_text.push_str("\n================ 函数热点 self（--sort symbol,dso） ================\n");
    match &self_view {
        Ok(t) => report_text.push_str(t),
        Err(e) => report_text.push_str(&format!("[self 视图生成失败: {}]\n", e)),
    }
    report_text.push_str("\n================ 函数热点 children（--children --sort symbol,dso） ================\n");
    match &symbol_view {
        Ok(t) => report_text.push_str(t),
        Err(e) => report_text.push_str(&format!("[children 视图生成失败: {}]\n", e)),
    }
    std::fs::write(&report_path, &report_text).context("写入 simpleperf_report.txt 失败")?;

    // pull + 清理（清理失败不致命：文件名含时间戳，不影响下次录制）
    let pull = crate::utils::adb_for(serial)
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
    let _ = crate::utils::adb_for(serial).args(["shell", "rm", "-f", &dev_path]).output();
    let bytes = std::fs::metadata(&local_path)?.len();

    Ok(RecordedStack {
        local_path,
        report_path,
        wall_start,
        wall_end,
        bytes,
        samples,
        samples_lost,
    })
}

/// 设备端跑一次 simpleperf report。
/// stderr 合并进 stdout（设备端 `2>&1`）：真实报错必须带回——若走 `2>/dev/null` 会连
/// 报错一起吞掉，失败时错误信息为空。成功路径再过滤 simpleperf 自身日志行（W/I 级别，
/// 如 dso 符号表缺失警告）不污染视图。输出经空格压缩：simpleperf 按最长符号名做列
/// 对齐（Symbol 列可达数百列宽），原样落盘会让报告文件膨胀到数倍于 `.data`
/// （实测 4.9MB vs 2.7MB），压缩后语义不变（符号名内出现连续空格的概率≈0，单空格保留）。
/// 失败返回 Err，不中断整体流程（另两个视图仍可生成，`.data` 仍会被 pull 回）。
fn device_report(dev_path: &str, extra_args: &[&str], serial: Option<&str>) -> Result<String> {
    let mut cmd = crate::utils::adb_for(serial);
    cmd.args(["shell", "simpleperf", "report", "-i", dev_path]);
    for a in extra_args {
        cmd.arg(a);
    }
    cmd.arg("2>&1");
    let out = cmd.output().context("执行 adb shell simpleperf report 失败")?;
    let merged = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        bail!("simpleperf report 失败: {}", merged.trim());
    }
    Ok(squeeze_spaces(&filter_simpleperf_logs(&merged)))
}

/// 过滤 simpleperf 自身日志行（`simpleperf W/I …` 前缀，如符号表缺失警告），只留报告表体
fn filter_simpleperf_logs(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        if line.starts_with("simpleperf ") {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// 连续 2+ 空格压缩为 1 个（列对齐 padding 去除，保留语义空格）
fn squeeze_spaces(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_spaces = false;
    for c in text.chars() {
        if c == ' ' {
            if !in_spaces {
                out.push(' ');
            }
            in_spaces = true;
        } else {
            in_spaces = false;
            out.push(c);
        }
    }
    out
}

// ==================== 浏览器火焰图（report_html.py） ====================

/// AOSP 官方脚本缓存目录（`~/.cache/xperf/simpleperf_scripts`）：`report_html.py` 及其
/// 依赖 + 主机平台 report 库。布局对齐上游 `get_script_dir()`/`get_host_binary_path()`
/// 的相对定位规则（`bin/<os>/<arch>/<lib>`）。
const SCRIPTS_CACHE_SUBDIR: &str = ".cache/xperf/simpleperf_scripts";
/// gitiles blob 下载基址。**+archive 不支持多级子路径**（实测 `+archive/main/simpleperf/
/// scripts/simpleperf.tar.gz` 返回 INVALID_ARGUMENT；整仓 tarball 80MB 太重）→ 逐文件
/// blob `?format=TEXT`（base64 文本）下载，合计 ~10MB。
const AOSP_SCRIPTS_BASE: &str =
    "https://android.googlesource.com/platform/system/extras/+/main/simpleperf/scripts";
/// 进程内互斥：首次引导下载期间并发调用必须等待而非交错写缓存文件（同 perfetto UI
/// 镜像锁模式）
static SCRIPTS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 主机平台的 report 库相对目录与文件名（对齐上游 `get_host_binary_path`：linux-x86_64
/// 用 `.so`，darwin-x86_64 用 `.dylib`；上游未发布其他主机预编译库）。
/// 不支持的主机返回 Err（附支持列表）。
fn host_report_lib() -> Result<(&'static str, &'static str)> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok(("bin/linux/x86_64", "libsimpleperf_report.so")),
        ("macos", "x86_64") => Ok(("bin/darwin/x86_64", "libsimpleperf_report.dylib")),
        (os, arch) => bail!(
            "主机 {}/{} 无 simpleperf report 库预编译版本（上游仅提供 linux-x86_64 / darwin-x86_64）",
            os,
            arch
        ),
    }
}

/// 从 gitiles blob API 下载一个文件（`?format=TEXT` 为 base64 文本，经
/// `python3 -m base64 -d` 解码；python3 是本功能的硬依赖，此处不回避）。
/// URL 与路径均为固定字符集常量/推导值，无注入面。
fn fetch_aosp_blob(rel_path: &str, dest: &Path) -> Result<()> {
    let url = format!("{}/{}?format=TEXT", AOSP_SCRIPTS_BASE, rel_path);
    let dest_str = dest.to_string_lossy();
    let script = format!(
        "curl -sL -m 60 '{url}' | python3 -m base64 -d > '{dest_str}' && test -s '{dest_str}'"
    );
    let st = Command::new("sh")
        .arg("-c")
        .arg(&script)
        .status()
        .with_context(|| format!("下载 {} 失败", rel_path))?;
    if !st.success() {
        bail!("下载 {} 失败（网络不通或 AOSP 源不可达）", rel_path);
    }
    Ok(())
}

/// 确保 `report_html.py` 脚本集可用（首次使用从 AOSP 引导下载 ~10MB，之后离线）。
/// 返回脚本目录。完整性检查逐文件进行：半截缓存下次补齐缺项。
fn ensure_simpleperf_scripts() -> Result<PathBuf> {
    let _guard = SCRIPTS_LOCK.lock().expect("脚本缓存锁失败");
    let (lib_rel, lib_name) = host_report_lib()?;
    let dir = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(SCRIPTS_CACHE_SUBDIR);
    let lib_path = dir.join(lib_rel).join(lib_name);
    let mut needed: Vec<(String, PathBuf)> = [
        "report_html.py",
        // write_script 内嵌的前端脚本（add_file('report_html.js')，缺则生成半截 HTML 后失败）
        "report_html.js",
        "simpleperf_report_lib.py",
        "simpleperf_utils.py",
        // report_lib 的 ETM 解析依赖（import etm_types）；该文件自身无本地依赖，闭包到此为止
        "etm_types.py",
    ]
    .iter()
    .map(|f| (f.to_string(), dir.join(f)))
    .collect();
    needed.push((format!("{}/{}", lib_rel, lib_name), lib_path.clone()));
    let complete = needed
        .iter()
        .all(|(_, p)| p.is_file() && std::fs::metadata(p).map(|m| m.len() > 0).unwrap_or(false));
    if complete {
        return Ok(dir);
    }
    eprintln!("[simpleperf] 首次使用：从 AOSP 下载 report_html.py 脚本与主机 report 库（~10MB）…");
    for (rel, dest) in &needed {
        if dest.is_file() && std::fs::metadata(dest).map(|m| m.len() > 0).unwrap_or(false) {
            continue; // 半截缓存：只补缺项
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        fetch_aosp_blob(rel, dest)?;
    }
    Ok(dir)
}

/// 数据文件对应的火焰图 HTML 输出路径（同目录同名换扩展：`stack_x.data` → `stack_x.html`）
fn html_path_for(data_path: &Path) -> PathBuf {
    data_path.with_extension("html")
}

/// 在浏览器中查看 simpleperf 数据（GUI「函数热点」tab 的查看入口）：
/// 用 AOSP 官方 `report_html.py` 把 `.data` 渲染成**单文件 HTML**（含火焰图/Chart/
/// Sample Table，实测 3.3MB data → 7.8MB html ~1.2s），再 xdg-open 打开。
///
/// - 首次使用自动从 AOSP 引导下载脚本集到 `~/.cache/xperf/simpleperf_scripts/`
///   （~10MB，之后离线可用）；需要 `python3`
/// - HTML 已存在且新于 `.data` 时直接复用（同一份数据反复查看不重渲染）
/// - 生成带手动超时上限（300s，超大 `.data` 防挂死）与 Ctrl-C 中断响应
///
/// 生成失败时 `.data` 不受影响（报告文本/三视图不受影响）。
pub fn open_stack_in_browser(data_path: &Path) -> Result<String> {
    if !data_path.is_file() {
        bail!("数据文件不存在: {}", data_path.display());
    }
    // python3 前置（脚本运行与 blob 解码引导都依赖）
    let py = Command::new("sh")
        .args(["-c", "command -v python3"])
        .output()
        .context("检测 python3 失败")?;
    if String::from_utf8_lossy(&py.stdout).trim().is_empty() {
        bail!("需要 python3（report_html.py 渲染依赖），未在 PATH 中检测到");
    }
    let scripts_dir = ensure_simpleperf_scripts()?;
    let html = html_path_for(data_path);
    // 复用：HTML 已存在且新于 data（同一份数据的查看不重渲染）
    let reuse = html.is_file()
        && match (std::fs::metadata(data_path), std::fs::metadata(&html)) {
            (Ok(d), Ok(h)) => d.modified().ok().zip(h.modified().ok()).is_some_and(|(d, h)| h > d),
            _ => false,
        };
    if !reuse {
        let mut child = Command::new("python3")
            .arg(scripts_dir.join("report_html.py"))
            .arg("-i")
            .arg(data_path)
            .arg("-o")
            .arg(&html)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("启动 python3 report_html.py 失败")?;
        let deadline = Instant::now() + Duration::from_secs(300);
        loop {
            match child.try_wait()? {
                Some(_) => break,
                None if crate::utils::is_interrupted() => {
                    let _ = child.kill();
                    bail!("火焰图生成被中断");
                }
                None if Instant::now() > deadline => {
                    let _ = child.kill();
                    bail!("火焰图生成超时（>300s，数据过大？）");
                }
                None => std::thread::sleep(Duration::from_millis(200)),
            }
        }
        let out = child.wait_with_output()?;
        if !out.status.success() {
            // 失败清理：python 崩溃可能已写出半截 HTML，不清会让下次 reuse 误判复用
            let _ = std::fs::remove_file(&html);
            // stderr 取末尾若干行（report 日志可能很长，含符号查找警告）
            let tail: String = String::from_utf8_lossy(&out.stderr)
                .lines()
                .rev()
                .take(5)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n");
            bail!("report_html.py 生成失败: {}", tail);
        }
    }
    // 打开本地 HTML（无 CSP/网络限制，同 reveal_trace_and_open_ui 的可靠性）
    #[cfg(target_os = "macos")]
    let open = "open";
    #[cfg(not(target_os = "macos"))]
    let open = "xdg-open";
    Command::new(open)
        .arg(&html)
        .spawn()
        .with_context(|| format!("打开浏览器失败（{}）", open))?;
    Ok(if reuse {
        format!(
            "火焰图已生成过，直接打开: {}",
            html.display()
        )
    } else {
        format!("火焰图已生成并打开: {}", html.display())
    })
}

// ==================== 缓存清理 ====================

/// 缓存清理结果（各目录不存在视为 0，不报错）
pub struct CleanReport {
    /// 清掉的字节数
    pub bytes: u64,
    /// 清掉的文件数
    pub files: u64,
}

/// 删除目录并统计（不存在静默返回 0；删除中目录被并发重建的部分忽略）
fn remove_dir_counted(path: &Path) -> (u64, u64) {
    if !path.is_dir() {
        return (0, 0);
    }
    let (mut bytes, mut files) = (0u64, 0u64);
    // 先统计再删（目录树小：UI 镜像 ~20 文件、simpleperf 脚本 6 个）
    fn count_dir(p: &Path, bytes: &mut u64, files: &mut u64) {
        for e in std::fs::read_dir(p).into_iter().flatten().flatten() {
            let meta = match e.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.is_dir() {
                count_dir(&e.path(), bytes, files);
            } else {
                *bytes += meta.len();
                *files += 1;
            }
        }
    }
    count_dir(path, &mut bytes, &mut files);
    let _ = std::fs::remove_dir_all(path);
    (bytes, files)
}

/// 清理全部缓存与采集数据：`~/.cache/xperf`（perfetto UI 镜像 + simpleperf 脚本集，
/// 首次使用会重新引导下载）+ `/tmp/xperf`（采集数据目录，含 CSV/图表/trace/调用栈）。
/// trace_processor 官方缓存 `~/.local/share/perfetto` **不在清理范围**（属
/// get.perfetto.dev 官方工具缓存，与 perfetto UI 镜像不同源）。
/// 正在采样/录制时调用是安全的：文件被删后流式写入方 create/append 会按需重建，
/// 但当前会话的 CSV 追加与图表生成可能丢——建议空闲时清。
pub fn clean_all_caches() -> Result<CleanReport> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let mut total = CleanReport { bytes: 0, files: 0 };
    for dir in [
        home.join(".cache").join("xperf"),
        std::env::temp_dir().join("xperf"),
    ] {
        let (b, f) = remove_dir_counted(&dir);
        total.bytes += b;
        total.files += f;
    }
    Ok(total)
}

// ==================== 报告解析与渲染 ====================

/// 单线程 CPU 开销（线程视图行：`--sort comm,pid,tid`，值为 self 开销占比）
pub struct ThreadOverhead {
    /// 线程名（comm）
    pub name: String,
    /// 进程 PID
    pub pid: u32,
    /// 线程 TID
    pub tid: u32,
    /// CPU 开销占比（%，线程自身）
    pub percent: f64,
}

/// 单函数热点（self 与 children 两个函数视图共用；各自缺失的占比为 0）
pub struct SymbolOverhead {
    /// 函数名（stripped so 显示为 `libxxx.so[+偏移]`）
    pub symbol: String,
    /// 所属共享库完整路径（渲染时取 basename）
    pub dso: String,
    /// 调用链累计开销（%，children：出现在任何调用链上的总占比；self 视图恒 0）
    pub children_percent: f64,
    /// 自身开销（%，self：CPU 时间直接落在该函数内；children 视图有真实值）
    pub self_percent: f64,
}

/// 解析 report 线程视图（跳过 Cmdline/Arch/Event/Samples 头部，取 Overhead 列表）。
/// 行格式（定宽但按 token 锚定解析）：`67.90%  XFW:Main  4428  6002`——
/// 首 token 为百分比、尾 token 为 TID、次尾为 PID、中间合并为线程名（可含空格）。
fn parse_thread_view(text: &str) -> Vec<ThreadOverhead> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let line = line.trim_end();
        // 表头行与空行跳过；数据行以百分比开头
        if !line.starts_with(|c: char| c.is_ascii_digit()) {
            continue;
        }
        let mut parts = line.split_whitespace();
        let (Some(pct), Some(name), Some(pid), Some(tid), None) =
            (parts.next(), parts.next(), parts.next(), parts.next(), parts.next())
        else {
            // 线程名含空格：首=百分比、尾=TID、次尾=PID、中间=name
            let tokens: Vec<&str> = line.split_whitespace().collect();
            if tokens.len() < 4 || !tokens[0].ends_with('%') {
                continue;
            }
            let tid = match tokens[tokens.len() - 1].parse() {
                Ok(v) => v,
                Err(_) => continue,
            };
            let pid = match tokens[tokens.len() - 2].parse() {
                Ok(v) => v,
                Err(_) => continue,
            };
            let percent = tokens[0].trim_end_matches('%').parse().unwrap_or(0.0);
            rows.push(ThreadOverhead {
                name: tokens[1..tokens.len() - 2].join(" "),
                pid,
                tid,
                percent,
            });
            continue;
        };
        // 线程名不含空格的快路径：四个 token 恰好用完
        if !pct.ends_with('%') {
            continue;
        }
        rows.push(ThreadOverhead {
            percent: pct.trim_end_matches('%').parse().unwrap_or(0.0),
            name: name.to_string(),
            pid: pid.parse().unwrap_or(0),
            tid: tid.parse().unwrap_or(0),
        });
    }
    rows
}

/// 解析 children 函数视图（`--children --sort symbol,dso`）。
/// 行格式：`65.83%  0.00%  symbol name…  /path/to/lib.so`——首 token=children、
/// 次首=self、尾 token=dso（路径无空格）、中间合并为符号名（可含空格，如
/// `__pthread_start(void*)` 或 `std::string::size() const`）。
fn parse_children_view(text: &str) -> Vec<SymbolOverhead> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let line = line.trim_end();
        if !line.starts_with(|c: char| c.is_ascii_digit()) {
            continue;
        }
        let tokens: Vec<&str> = line.split_whitespace().collect();
        // children% + self% + symbol + dso 最少 4 个 token
        if tokens.len() < 4 || !tokens[0].ends_with('%') || !tokens[1].ends_with('%') {
            continue;
        }
        let dso = tokens[tokens.len() - 1].to_string();
        // dso 恒为路径（/ 开头或 [kernel.kallsyms]）；不符合说明中间含了路径外的 token，
        // 不动最后一位的锚定语义（dso 无空格是 simpleperf 输出的稳定结构）
        rows.push(SymbolOverhead {
            children_percent: tokens[0].trim_end_matches('%').parse().unwrap_or(0.0),
            self_percent: tokens[1].trim_end_matches('%').parse().unwrap_or(0.0),
            symbol: tokens[2..tokens.len() - 1].join(" "),
            dso,
        });
    }
    rows
}

/// 解析 self 函数视图（`--sort symbol,dso`，无 `--children`）——"CPU 高在哪个函数"的
/// 直接回答。行格式：`5.26%  symbol name…  [kernel.kallsyms]`——首 token=self%、
/// 尾 token=dso、中间合并为符号名（可含空格）。children_percent 恒 0。
fn parse_self_view(text: &str) -> Vec<SymbolOverhead> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let line = line.trim_end();
        if !line.starts_with(|c: char| c.is_ascii_digit()) {
            continue;
        }
        let tokens: Vec<&str> = line.split_whitespace().collect();
        // self% + symbol + dso 最少 3 个 token；次首 token 须非百分比（与 children 视图区分）
        if tokens.len() < 3 || !tokens[0].ends_with('%') || tokens[1].ends_with('%') {
            continue;
        }
        rows.push(SymbolOverhead {
            children_percent: 0.0,
            self_percent: tokens[0].trim_end_matches('%').parse().unwrap_or(0.0),
            symbol: tokens[1..tokens.len() - 1].join(" "),
            dso: tokens[tokens.len() - 1].to_string(),
        });
    }
    rows
}

/// 按分隔行切出报告文件中的三视图文本（找不到对应段落返回空串）。
/// 分隔行形如 `================ <段名>（…） ================`，段正文为分隔行之后、
/// 下一分隔行之前的所有行。
fn section<'a>(report: &'a str, marker: &str) -> &'a str {
    report
        .find(marker)
        .and_then(|i| {
            // 跳过分隔行剩余部分（含参数与右边界 =====），正文从下一行开始
            let after_marker = &report[i + marker.len()..];
            let body_start = after_marker.find('\n').map(|j| i + marker.len() + j + 1)?;
            let body = &report[body_start..];
            // 下一分隔行（================ 开头）前为本段
            match body.find("\n================") {
                Some(j) => Some(&body[..j]),
                None => Some(body),
            }
        })
        .unwrap_or("")
}

/// dso 展示名（含方括号定界）：路径取 basename 包 `[]`；内核伪 dso
/// （`[kernel.kallsyms]` 等）自带括号原样返回
fn dso_display(dso: &str) -> String {
    if dso.starts_with('[') {
        dso.to_string()
    } else {
        format!(
            "[{}]",
            Path::new(dso)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| dso.to_string())
        )
    }
}

/// 生成人类可读的函数热点报告：窗口/样本概览 + 线程 CPU 分布 + 函数热点 self top20 +
/// 函数热点 children top20 + stripped 符号说明。完整三视图见 `rec.report_path`。
///
/// 报告文件缺失时返回 Err（数据文件仍在，可推回设备复跑 report）。
pub fn analyze_and_report(rec: &RecordedStack, package: &str) -> Result<String> {
    let report = std::fs::read_to_string(&rec.report_path)
        .with_context(|| format!("读取报告失败: {}", rec.report_path.display()))?;
    let threads = parse_thread_view(section(&report, "线程 CPU 分布"));
    let self_funcs = parse_self_view(section(&report, "函数热点 self"));
    let children_funcs = parse_children_view(section(&report, "函数热点 children"));

    let mut s = String::new();
    s.push_str(&format!(
        "========== simpleperf 函数热点分析（{}）==========\n",
        package
    ));
    s.push_str(&format!(
        "录制窗口: {} ~ {}（{:.1}s，cpu-cycles 采样 + dwarf 调用栈）\n",
        rec.wall_start.format("%H:%M:%S"),
        rec.wall_end.format("%H:%M:%S"),
        (rec.wall_end - rec.wall_start).num_milliseconds() as f64 / 1000.0
    ));
    s.push_str(&format!(
        "样本: {}（丢失 {}）  数据: {}（{:.1} MB）\n\n",
        rec.samples,
        rec.samples_lost,
        rec.local_path.display(),
        rec.bytes as f64 / 1e6
    ));

    s.push_str("线程 CPU 分布（self 开销占比，≥1%）：\n");
    if threads.is_empty() {
        s.push_str("  （无数据：窗口内进程可能无 CPU 活动，或报告段落缺失，见完整报告）\n");
    }
    for t in threads.iter().take(12) {
        s.push_str(&format!(
            "  {:>6.2}%  {} (pid {}, tid {})\n",
            t.percent, t.name, t.pid, t.tid
        ));
    }

    s.push_str("\n函数热点 top20（self 自身开销——CPU 时间直接落在该函数内，≥1%）：\n");
    if self_funcs.is_empty() {
        s.push_str("  （无数据：窗口内进程可能无 CPU 活动，或报告段落缺失，见完整报告）\n");
    }
    for f in self_funcs.iter().take(20) {
        s.push_str(&format!(
            "  self {:>6.2}%  {}  {}\n",
            f.self_percent,
            f.symbol,
            dso_display(&f.dso)
        ));
    }

    s.push_str("\n函数热点 top20（children 调用链累计——热点路径归因，≥1%）：\n");
    if children_funcs.is_empty() {
        s.push_str("  （无数据：窗口内进程可能无 CPU 活动，或报告段落缺失，见完整报告）\n");
    }
    for f in children_funcs.iter().take(20) {
        s.push_str(&format!(
            "  children {:>6.2}%  self {:>6.2}%  {}  {}\n",
            f.children_percent,
            f.self_percent,
            f.symbol,
            dso_display(&f.dso)
        ));
    }

    s.push_str("\n说明:\n");
    s.push_str(&format!(
        "- 完整三视图报告: {}\n",
        rec.report_path.display()
    ));
    s.push_str("- 设备端 so 多为 stripped（无符号表），函数名显示为 `libxxx.so[+偏移]`；偏移可用未剥离 so 离线符号化\n");
    s.push_str("- `.data` 为 simpleperf 原始数据，可 `adb push` 回设备换参数复跑 `simpleperf report`（如 `--full-callgraph`）\n");
    if rec.samples == 0 {
        s.push_str("- 样本数为 0：窗口内该应用可能无 CPU 活动，建议复跑或加长窗口\n");
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sample_stats() {
        let log = concat!(
            "simpleperf W dso.cpp:432] /vendor/lib64/libgsl.so doesn't contain symbol table\n",
            "simpleperf I cmd_record.cpp:764] Samples recorded: 8773. Samples lost: 0.\n"
        );
        assert_eq!(parse_sample_stats(log), (8773, 0));
        // 丢失非零
        assert_eq!(
            parse_sample_stats("Samples recorded: 12. Samples lost: 34."),
            (12, 34)
        );
        // 未匹配
        assert_eq!(parse_sample_stats("no stats here"), (0, 0));
    }

    #[test]
    fn test_validate_package() {
        assert!(validate_package("com.lixiang.car.x.svm").is_ok());
        assert!(validate_package("com.example_app").is_ok());
        // '-' 与 CLI/GUI 校验口径一致（Android 允许的自定义段）
        assert!(validate_package("com.example-app").is_ok());
        assert!(validate_package("").is_err());
        assert!(validate_package("a b").is_err());
        assert!(validate_package("a;rm").is_err());
        assert!(validate_package("a$(id)").is_err());
    }

    #[test]
    fn test_filter_simpleperf_logs() {
        let out = concat!(
            "simpleperf W dso.cpp:432] /vendor/lib64/libgsl.so doesn't contain symbol table\n",
            "Cmdline: /system/bin/simpleperf record --app pkg\n",
            "Overhead  Symbol  Shared Object\n",
            "5.26%  memcpy  /lib/libc.so\n",
            "simpleperf I cmd_record.cpp:764] Samples recorded: 8773.\n"
        );
        let filtered = filter_simpleperf_logs(out);
        assert!(!filtered.contains("dso.cpp"));
        assert!(!filtered.contains("cmd_record"));
        assert!(filtered.contains("Cmdline"));
        assert!(filtered.contains("5.26%  memcpy"));
        // 每行补 \n（lines() 吃掉行尾换行，拼接时恢复）
        assert!(filtered.ends_with('\n'));
        // 全日志输入 → 全部过滤为空串
        assert_eq!(filter_simpleperf_logs("simpleperf W x\n"), "");
    }

    #[test]
    fn test_html_path_for() {
        // 同目录同名换扩展：.data → .html（open_stack_in_browser 的输出路径推导）
        assert_eq!(
            html_path_for(Path::new("/a/b/stack_x.data")),
            PathBuf::from("/a/b/stack_x.html")
        );
        assert_eq!(
            html_path_for(Path::new("no_ext")),
            PathBuf::from("no_ext.html")
        );
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn test_host_report_lib() {
        // 本机为 linux-x86_64：库路径对齐上游 get_host_binary_path 规则
        let (dir, name) = host_report_lib().expect("linux-x86_64 应有预编译库");
        assert_eq!(dir, "bin/linux/x86_64");
        assert_eq!(name, "libsimpleperf_report.so");
    }

    /// 真实链路手动测试：取 `log/` 下最新 `.data` 走完整 open_stack_in_browser
    /// （首次会从 AOSP 下载脚本 ~10MB；会 xdg-open 弹浏览器——需要桌面环境）。
    /// 跑法：`cargo test -p xperf-core test_open_stack_in_browser_real -- --ignored --nocapture`
    #[test]
    #[ignore = "真实链路：需要 log/ 下有 .data 且有桌面环境（会弹浏览器）"]
    fn test_open_stack_in_browser_real() {
        // 找 /tmp/xperf/<pkg>/<会话时间戳>/stack/ 下最新的 .data（数据根见 data_root）
        let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
        let stack_root = std::env::temp_dir().join("xperf");
        if let Ok(rd) = std::fs::read_dir(&stack_root) {
            for pkg in rd.flatten() {
                // 每个包名下是多个会话时间戳目录，每个会话里才有 stack/
                let Ok(sessions) = std::fs::read_dir(pkg.path()) else { continue };
                for s in sessions.flatten() {
                    let Ok(files) = std::fs::read_dir(s.path().join("stack")) else { continue };
                    for f in files.flatten() {
                        let p = f.path();
                        if p.extension().and_then(|e| e.to_str()) != Some("data") {
                            continue;
                        }
                        let m = std::fs::metadata(&p)
                            .and_then(|m| m.modified())
                            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                        if best.as_ref().is_none_or(|(bm, _)| m > *bm) {
                            best = Some((m, p));
                        }
                    }
                }
            }
        }
        let (_, data) = best.expect("log/ 下无 .data，先跑一次 --stack 采集");
        println!("测试数据: {}", data.display());
        let msg = open_stack_in_browser(&data).expect("打开失败");
        assert!(msg.contains("火焰图"), "返回消息异常: {}", msg);
    }

    #[test]
    fn test_parse_thread_view() {
        let out = concat!(
            "Cmdline: /system/bin/simpleperf record -p 4428 -g --duration 8 -o /data/local/tmp/x.data\n",
            "Arch: arm64\n",
            "Event: cpu-cycles (type 0, config 0)\n",
            "Samples: 8773\n",
            "Event count: 2623562597\n",
            "\n",
            "Overhead  Command          Pid   Tid\n",
            "67.90%    XFW:Main         4428  6002\n",
            "5.39%     AdrenoOsLib      4428  10000\n",
            "4.37%     RUST:VSYNC       4428  5881\n"
        );
        let rows = parse_thread_view(out);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].name, "XFW:Main");
        assert_eq!(rows[0].pid, 4428);
        assert_eq!(rows[0].tid, 6002);
        assert!((rows[0].percent - 67.90).abs() < 1e-9);
        assert_eq!(rows[1].tid, 10000);
        // 线程名含空格的兜底路径
        let rows2 = parse_thread_view("12.5%  my thread name  100  101\n");
        assert_eq!(rows2.len(), 1);
        assert_eq!(rows2[0].name, "my thread name");
        assert_eq!(rows2[0].pid, 100);
        assert_eq!(rows2[0].tid, 101);
        // 表头行/百分比缺失不产生行
        assert!(parse_thread_view("Overhead  Command  Pid  Tid\n\nabc\n").is_empty());
    }

    #[test]
    fn test_parse_children_view() {
        let out = concat!(
            "Children  Self   Symbol                                                                 Shared Object\n",
            "97.60%    0.00%  __pthread_start(void*)                                                /apex/com.android.runtime/lib64/bionic/libc.so\n",
            "65.83%    0.00%  libinterface.so[+a8de4c]                                              /data/app/~~xxx/lib/arm64/libinterface.so\n",
            "60.30%    0.05%  std::string::size() const                                             /apex/com.android.runtime/lib64/bionic/libc.so\n"
        );
        let rows = parse_children_view(out);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].symbol, "__pthread_start(void*)");
        assert!((rows[0].children_percent - 97.60).abs() < 1e-9);
        assert!((rows[0].self_percent - 0.0).abs() < 1e-9);
        assert!(rows[0].dso.ends_with("libc.so"));
        assert_eq!(rows[1].symbol, "libinterface.so[+a8de4c]");
        // 符号名含空格（demangled）
        assert_eq!(rows[2].symbol, "std::string::size() const");
        // kernel dso（[kernel.kallsyms]）
        let rows2 = parse_children_view("1.20%  0.30%  schedule  [kernel.kallsyms]\n");
        assert_eq!(rows2[0].dso, "[kernel.kallsyms]");
        // 非 children 行（无两个百分比前缀）跳过
        assert!(parse_children_view("97.60%  __pthread_start  /lib/libc.so\n").is_empty());
    }

    #[test]
    fn test_squeeze_spaces() {
        // 列对齐 padding 压缩
        assert_eq!(
            squeeze_spaces("97.60%    0.00%  __start_thread           /lib/libc.so"),
            "97.60% 0.00% __start_thread /lib/libc.so"
        );
        // 单空格保留（符号名内的语义空格）
        assert_eq!(
            squeeze_spaces("std::string::size() const"),
            "std::string::size() const"
        );
        // 换行/制表不受影响
        assert_eq!(squeeze_spaces("a\n  b\tc"), "a\n b\tc");
    }

    #[test]
    fn test_parse_self_view() {
        // self 视图：单百分比 + 符号 + dso
        let out = concat!(
            "Overhead  Symbol                                                                                                                Shared Object\n",
            "5.26%     _raw_spin_unlock_irqrestore                                                                                           [kernel.kallsyms]\n",
            "3.05%     libgsl.so[+2af78]                                                                                                      /vendor/lib64/libgsl.so\n",
            "1.94%     pthread_mutex_lock                                                                                                     /apex/com.android.runtime/lib64/bionic/libc.so\n"
        );
        let rows = parse_self_view(out);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].symbol, "_raw_spin_unlock_irqrestore");
        assert_eq!(rows[0].dso, "[kernel.kallsyms]");
        assert!((rows[0].self_percent - 5.26).abs() < 1e-9);
        assert!((rows[0].children_percent - 0.0).abs() < 1e-9);
        assert_eq!(rows[1].symbol, "libgsl.so[+2af78]");
        // children 视图行（首两 token 均为百分比）不误入 self 视图
        assert!(parse_self_view("97.60%  0.00%  start  /lib/libc.so\n").is_empty());
        // 表头/非数据行跳过
        assert!(parse_self_view("Overhead  Symbol  Shared Object\n").is_empty());
    }

    #[test]
    fn test_section_extraction() {
        let report = concat!(
            "================ 线程 CPU 分布（--sort comm,pid,tid） ================\n",
            "Overhead  Command  Pid  Tid\n",
            "67.90%  XFW:Main  4428  6002\n",
            "\n================ 函数热点 self（--sort symbol,dso） ================\n",
            "Overhead  Symbol  Shared Object\n",
            "5.26%  _raw_spin_unlock_irqrestore  [kernel.kallsyms]\n",
            "\n================ 函数热点 children（--children --sort symbol,dso） ================\n",
            "Children  Self  Symbol  Shared Object\n",
            "97.60%  0.00%  start  /lib/libc.so\n"
        );
        let thread_sec = section(report, "线程 CPU 分布");
        assert!(thread_sec.contains("XFW:Main"));
        assert!(!thread_sec.contains("Shared Object"));
        let self_sec = section(report, "函数热点 self");
        assert!(self_sec.contains("_raw_spin_unlock_irqrestore"));
        assert!(!self_sec.contains("Children"));
        let children_sec = section(report, "函数热点 children");
        assert!(children_sec.contains("Children"));
        assert!(children_sec.contains("97.60%"));
        assert!(section(report, "不存在的段").is_empty());
        // 末段无下一分隔行时取到结尾
        let no_next = "================ 函数热点 children（--children --sort symbol,dso） ================\nabc";
        assert_eq!(section(no_next, "函数热点 children").trim_end(), "abc");
    }

    #[test]
    fn test_report_renders() {
        let dir = std::env::temp_dir().join(format!("xperf_sp_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let report_path = dir.join("simpleperf_report.txt");
        std::fs::write(&report_path, concat!(
            "================ 线程 CPU 分布（--sort comm,pid,tid） ================\n",
            "Overhead  Command  Pid  Tid\n",
            "67.90%  XFW:Main  4428  6002\n",
            "\n================ 函数热点 self（--sort symbol,dso） ================\n",
            "Overhead  Symbol  Shared Object\n",
            "5.26%  _raw_spin_unlock_irqrestore  [kernel.kallsyms]\n",
            "\n================ 函数热点 children（--children --sort symbol,dso） ================\n",
            "Children  Self  Symbol  Shared Object\n",
            "97.60%  0.00%  __pthread_start(void*)  /apex/com.android.runtime/lib64/bionic/libc.so\n"
        )).unwrap();
        let rec = RecordedStack {
            local_path: dir.join("stack_x.data"),
            report_path,
            wall_start: Local::now(),
            wall_end: Local::now(),
            bytes: 3325139,
            samples: 8773,
            samples_lost: 0,
        };
        let text = analyze_and_report(&rec, "com.lixiang.car.x.svm").unwrap();
        assert!(text.contains("com.lixiang.car.x.svm"));
        assert!(text.contains("样本: 8773（丢失 0）"));
        assert!(text.contains("XFW:Main"));
        // self 视图段在前，children 视图段在后
        let pos_self = text.find("self 自身开销").unwrap();
        let pos_children = text.find("children 调用链累计").unwrap();
        assert!(pos_self < pos_children);
        assert!(text.contains("_raw_spin_unlock_irqrestore"));
        assert!(text.contains("__pthread_start(void*)"));
        // dso 渲染：路径取 basename，内核伪 dso 原样（不重复包 []）
        assert!(text.contains("[libc.so]"));
        assert!(text.contains("[kernel.kallsyms]"));
        assert!(!text.contains("[["));
        assert!(text.contains("stripped"));
        // 报告文件缺失 → Err 且附路径
        let rec2 = RecordedStack {
            report_path: dir.join("nope.txt"),
            ..rec
        };
        assert!(analyze_and_report(&rec2, "pkg").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
