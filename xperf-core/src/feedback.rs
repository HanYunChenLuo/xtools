//! 问题反馈：一键收集 xperf 自身日志与产物，打包并上传内部 GitLab issue。
//!
//! 目标：用户遇到问题时一次点击收齐「能分析问题」的全部证据。采集范围（时间窗默认
//! 最近 1 小时，按文件 mtime 过滤）：
//! 1. 会话产物（[`crate::csvstream::data_root`] 下 `<pkg>/<ts>/`）：采样 CSV、perfetto
//!    报告（`trace_analysis.txt`/`trace_queries.sql`）、simpleperf 报告与 `.data`、
//!    截屏、会话 logcat。**不入包**的大文件：`.pftrace`（10s≈46MB）、录屏 `.mp4`、
//!    火焰图 `.html`（可由 `.data` 再生）——清单列出本机路径，需要时手动附加
//! 2. GUI 诊断日志（`XPERF_DIAG_LOG` 指向的文件，CLI 下回退探测默认路径）尾段
//! 3. 每台在线设备（SS4 MindRT 网关除外）的 agent daemon 日志（adb pull，
//!    SSH 远程经 hop#1 天然承载零改动）
//! 4. 环境信息 `manifest.json`（工具版本/OS/传输模式/设备快照/自检清单）
//!
//! 设备 logcat 不自动采集（非 xperf 自身日志）——issue 正文模板引导用户按需手动
//! 执行 `adb logcat -d` 附加到 issue。
//!
//! 覆盖率自检：逐项生成 [`ChecklistEntry`](crate::feedback::ChecklistEntry)，缺失项
//! 如实标注原因，不静默缺数据；
//! 单文件超 32MB 上限截尾保留最近内容并注明。打包用 `tar`+`flate2`、
//! 上传用 `reqwest`（rustls 内置证书链），不依赖系统 tar/curl 命令。
//!
//! 上传链路与 [`crate::utils::run_adb`] 等不同，须经网络：`submit` 使用
//! `reqwest::blocking`，调用方须在独立阻塞线程调用（GUI/CLI 均如此）。

use anyhow::{anyhow, bail, Context, Result};
use chrono::Local;
use serde::Serialize;
use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::utils::{self, AdbDevice};
use crate::Transport;

/// 默认采集时间窗（最近 1 小时）
pub const DEFAULT_WINDOW: Duration = Duration::from_secs(3600);

/// 单文件入包上限（32 MB）：超出截尾保留末尾内容（清单注明），防洪泛日志撑爆归档
const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;

/// GUI 诊断日志尾段截取上限（512 KB）
const DIAG_TAIL_BYTES: u64 = 512 * 1024;

/// GitLab 项目 ID（ligraphic/xperf）
const DEFAULT_PROJECT_ID: &str = "39859";

/// GitLab API base（与 `scripts/release_upload.py` 同源）
const DEFAULT_GITLAB_API: &str = "https://gitlab.chehejia.com/api/v4";

/// 设备端 agent daemon 日志路径
const AGENT_LOG_DEVICE_PATH: &str = "/data/local/tmp/xperf-agent.log";

/// 覆盖率自检清单条目（存在且非空 = `ok`；缺失/失败须在 `note` 写明原因）。
///
/// 序列化进 `manifest.json`，GUI 也可直接取用展示。
#[derive(Debug, Clone, Serialize)]
pub struct ChecklistEntry {
    /// 条目名（如「采样 CSV」「agent 日志 6eb792dfb0f」）
    pub name: String,
    /// 是否达标（收集成功；源本就不存在且属正常使用路径时 `ok=true` 并注明「无产出」）
    pub ok: bool,
    /// 说明：收集量 / 缺失原因 / 截断标注
    pub note: String,
}

/// 收集 + 打包完成的反馈包（上传前可供 GUI 预览清单）
pub struct FeedbackBundle {
    /// tar.gz 归档本机路径（上传失败不删除，报错中给出路径供用户手动附加）
    pub archive: PathBuf,
    /// issue 标题（`[feedback] <场景摘要> <日期时间>`）
    pub title: String,
    /// issue 正文（markdown：问题描述/环境信息/数据清单/手动附加指引）
    pub body: String,
    /// 自检清单（与归档内 `manifest.json` 中一致）
    pub checklist: Vec<ChecklistEntry>,
    /// 归档大小（字节）
    pub archive_bytes: u64,
}

/// 扫描出的会话产物文件
struct ScannedFile {
    /// 源文件绝对路径
    src: PathBuf,
    /// 相对数据根目录的路径（归档内保持该结构）
    rel: PathBuf,
    /// 文件大小（字节）
    size: u64,
}

/// 因体积/可再生被排除的大文件（清单列出本机路径，不入包）
#[derive(Debug)]
struct SkippedFile {
    /// 本机绝对路径
    path: PathBuf,
    /// 文件大小（字节）
    size: u64,
    /// 排除原因（写入 issue 正文）
    reason: &'static str,
}

/// 收集并打包反馈证据。
///
/// `description` 为用户一句话场景摘要（可空）。返回的 [`FeedbackBundle::archive`]
/// 始终保留在本机（上传失败时由调用方把路径提示给用户）。
pub fn collect(description: &str) -> Result<FeedbackBundle> {
    let root = crate::csvstream::data_root();
    let devices = utils::list_adb_devices().unwrap_or_else(|e| {
        utils::diag(&format!("feedback: list_adb_devices 失败（按无设备继续）: {e}"));
        Vec::new()
    });
    collect_in(&root, description, DEFAULT_WINDOW, &devices, MAX_FILE_BYTES)
}

/// 上传归档并创建 issue，返回 issue URL。
///
/// 失败返回 Err（错误信息含归档路径提示）；归档文件不删除。
/// 须在非 tokio runtime 线程调用（`reqwest::blocking`）。
pub fn submit(bundle: &FeedbackBundle) -> Result<String> {
    let token = resolve_token()?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(180))
        .build()
        .context("构建 HTTP client 失败")?;

    let attachment = upload_attachment(&client, &token, &bundle.archive)
        .context("上传归档失败（本地归档保留，可手动附加到 issue）")?;
    let body = format!("{}\n\n## 附件\n\n{}\n", bundle.body, attachment);
    create_issue(&client, &token, &bundle.title, &body).context("创建 issue 失败（本地归档保留，可手动建 issue 附加）")
}

// ==================== 收集 ====================

/// collect 的可测内核：数据根/时间窗/设备列表/单文件上限均可注入。
fn collect_in(
    root: &Path,
    description: &str,
    window: Duration,
    devices: &[AdbDevice],
    max_file_bytes: u64,
) -> Result<FeedbackBundle> {
    let now = SystemTime::now();
    let ts = Local::now().format("%Y%m%d-%H%M%S").to_string();
    let feedback_dir = root.join("feedback");
    let staging = feedback_dir.join(format!("staging-{ts}"));
    fs::create_dir_all(&staging).with_context(|| format!("创建暂存目录失败: {}", staging.display()))?;

    let mut checklist: Vec<ChecklistEntry> = Vec::new();

    // ① 会话产物（排除 feedback/ 自身目录）
    let (scanned, skipped) = scan_sessions(root, now, window);
    let staged_sessions = stage_session_files(&scanned, &staging, max_file_bytes, &mut checklist);
    checklist.extend(bucket_entries(&staged_sessions, skipped.is_empty()));
    for s in &skipped {
        // 大文件不入包但必须在清单留痕（路径在 issue 正文「大文件」节）
        utils::diag(&format!(
            "feedback: 排除大文件 {}（{}）",
            s.path.display(),
            s.reason
        ));
    }

    // ② GUI 诊断日志（尾段截取）
    checklist.push(stage_diag_log(&staging));

    // ③ 每台在线设备（网关除外）的 agent daemon 日志
    let android_devices: Vec<&AdbDevice> = devices.iter().filter(|d| !d.is_gateway).collect();
    if android_devices.is_empty() {
        checklist.push(ChecklistEntry {
            name: "agent 日志".into(),
            ok: false,
            note: "无在线 Android 设备（adb devices 为空）".into(),
        });
    }
    for d in &android_devices {
        checklist.push(stage_agent_log(d, &staging));
    }

    // ④ 环境信息 manifest.json
    let manifest = render_manifest(description, window, devices, &checklist);
    let manifest_path = staging.join("manifest.json");
    fs::write(&manifest_path, &manifest).context("写 manifest.json 失败")?;

    // issue 标题/正文（同时落盘 issue.md 进归档）
    let title = render_title(description);
    let body = render_issue_body(description, window, devices, &checklist, &skipped);
    fs::write(staging.join("issue.md"), &body).context("写 issue.md 失败")?;

    // 打包（tar + gzip，纯 Rust 实现不依赖系统 tar）
    let archive = feedback_dir.join(format!("xperf-feedback-{ts}.tar.gz"));
    pack_tar_gz(&staging, &archive).context("打包归档失败")?;
    let archive_bytes = fs::metadata(&archive).map(|m| m.len()).unwrap_or(0);
    fs::remove_dir_all(&staging).ok();

    utils::diag(&format!(
        "feedback: 收集完成 → {}（{} 字节，{} 项清单）",
        archive.display(),
        archive_bytes,
        checklist.len()
    ));
    Ok(FeedbackBundle { archive, title, body, checklist, archive_bytes })
}

/// 递归扫描数据根目录，返回（窗口内且未排除的文件，被排除的大文件）。
/// `feedback/` 目录（本功能自身产物）跳过。
fn scan_sessions(root: &Path, now: SystemTime, window: Duration) -> (Vec<ScannedFile>, Vec<SkippedFile>) {
    let mut files = Vec::new();
    let mut skipped = Vec::new();
    walk(root, root, now, window, &mut files, &mut skipped);
    (files, skipped)
}

/// [`scan_sessions`] 的递归工作函数。
fn walk(
    root: &Path,
    dir: &Path,
    now: SystemTime,
    window: Duration,
    files: &mut Vec<ScannedFile>,
    skipped: &mut Vec<SkippedFile>,
) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let rel = match path.strip_prefix(root) {
            Ok(r) => r.to_path_buf(),
            Err(_) => continue,
        };
        if path.is_dir() {
            // 跳过反馈功能自身目录（防自引用收集上一次的归档）
            if rel == Path::new("feedback") {
                continue;
            }
            walk(root, &path, now, window, files, skipped);
            continue;
        }
        if let Some(reason) = excluded_reason(&path) {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            skipped.push(SkippedFile { path, size, reason });
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let mtime = match meta.modified() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if !within_window(mtime, now, window) {
            continue;
        }
        files.push(ScannedFile { src: path, rel, size: meta.len() });
    }
}

/// mtime 是否落在 `[now - window, now]` 内（纯函数，单测用）
fn within_window(mtime: SystemTime, now: SystemTime, window: Duration) -> bool {
    let cutoff = now.checked_sub(window).unwrap_or(SystemTime::UNIX_EPOCH);
    mtime >= cutoff && mtime <= now + Duration::from_secs(60)
}

/// 不入包的扩展名（体积过大或可由包内其他产物再生），返回排除原因
fn excluded_reason(path: &Path) -> Option<&'static str> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("pftrace") => Some("perfetto trace 体积过大（10s≈46MB）"),
        Some("mp4") => Some("录屏文件体积过大"),
        Some("html") => Some("火焰图 HTML 可由归档内 .data 再生"),
        _ => None,
    }
}

/// 把扫描到的文件复制进暂存目录；超上限文件截尾保留末尾内容。
/// 截尾文件逐个生成清单条目（注明原始大小）。
fn stage_session_files(
    scanned: &[ScannedFile],
    staging: &Path,
    max_file_bytes: u64,
    checklist: &mut Vec<ChecklistEntry>,
) -> Vec<(String, u64)> {
    let mut buckets: Vec<(String, u64)> = Vec::new(); // (桶名/文件数) 累计
    for f in scanned {
        let dst = staging.join("data/sessions").join(&f.rel);
        if let Some(parent) = dst.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let truncated = f.size > max_file_bytes;
        let result = if truncated {
            copy_tail(&f.src, &dst, max_file_bytes)
        } else {
            fs::copy(&f.src, &dst).map(|_| ()).context("copy")
        };
        if let Err(e) = result {
            checklist.push(ChecklistEntry {
                name: format!("会话产物 {}", f.rel.display()),
                ok: false,
                note: format!("复制失败: {e:#}"),
            });
            continue;
        }
        if truncated {
            checklist.push(ChecklistEntry {
                name: format!("截尾 {}", f.rel.display()),
                ok: true,
                note: format!(
                    "原 {} → 保留末尾 {}（超出单文件上限）",
                    human_bytes(f.size),
                    human_bytes(max_file_bytes)
                ),
            });
        }
        let bucket = bucket_of(&f.rel);
        match buckets.iter_mut().find(|(name, _)| *name == bucket) {
            Some((_, n)) => *n += 1,
            None => buckets.push((bucket.to_string(), 1)),
        }
    }
    buckets
}

/// 会话产物按子目录/扩展名归桶（清单逐桶一行）
fn bucket_of(rel: &Path) -> &'static str {
    let s = rel.to_string_lossy();
    if s.contains("/trace/") {
        "perfetto 报告"
    } else if s.contains("/stack/") {
        "simpleperf 报告"
    } else if s.contains("/capture/") {
        "截屏"
    } else if s.contains("/logcat/") {
        "会话 logcat"
    } else if s.ends_with(".csv") {
        "采样 CSV"
    } else {
        "其他产物"
    }
}

/// 把归桶计数转成清单条目（0 文件桶不出现；整体为空生成一条不达标项）
fn bucket_entries(buckets: &[(String, u64)], no_skipped: bool) -> Vec<ChecklistEntry> {
    if buckets.is_empty() {
        let note = if no_skipped {
            "时间窗内无任何会话产物（数据根目录为空或文件均已超窗）"
        } else {
            "时间窗内会话产物均为被排除的大文件（见 issue 正文「大文件」节）"
        };
        return vec![ChecklistEntry { name: "会话产物".into(), ok: false, note: note.into() }];
    }
    buckets
        .iter()
        .map(|(name, n)| ChecklistEntry {
            name: name.clone(),
            ok: true,
            note: format!("{n} 个文件"),
        })
        .collect()
}

/// GUI 诊断日志尾段截取（CLI 环境无 `XPERF_DIAG_LOG` 时回退探测默认路径）
fn stage_diag_log(staging: &Path) -> ChecklistEntry {
    let candidates: Vec<PathBuf> = [
        std::env::var_os("XPERF_DIAG_LOG").map(PathBuf::from),
        Some(std::env::temp_dir().join("xperf_gui_diag.log")),
        Some(PathBuf::from("/tmp/xperf_gui_diag.log")),
    ]
    .into_iter()
    .flatten()
    .collect();
    let src = match candidates.iter().find(|p| p.is_file()) {
        Some(p) => p,
        None => {
            return ChecklistEntry {
                name: "GUI 诊断日志".into(),
                ok: false,
                note: "不存在（未运行过 GUI 或未启用诊断日志）".into(),
            };
        }
    };
    let dst = staging.join("data/gui-diag.log");
    match copy_tail(src, &dst, DIAG_TAIL_BYTES) {
        Ok(()) => {
            let size = fs::metadata(src).map(|m| m.len()).unwrap_or(0);
            let note = if size > DIAG_TAIL_BYTES {
                format!("原 {} → 尾段 {}", human_bytes(size), human_bytes(DIAG_TAIL_BYTES))
            } else {
                format!("完整收录（{}）", human_bytes(size))
            };
            ChecklistEntry { name: "GUI 诊断日志".into(), ok: true, note }
        }
        Err(e) => ChecklistEntry {
            name: "GUI 诊断日志".into(),
            ok: false,
            note: format!("读取失败: {e:#}"),
        },
    }
}

/// 从单台设备 pull agent daemon 日志进暂存目录（serial 中的 `:` 等转 `_`）
fn stage_agent_log(device: &AdbDevice, staging: &Path) -> ChecklistEntry {
    let safe_serial: String = device
        .serial
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' })
        .collect();
    let name = format!("agent 日志 {}", device.serial);
    let dst = staging.join(format!("data/agent-log/{safe_serial}.log"));
    if let Some(parent) = dst.parent() {
        let _ = fs::create_dir_all(parent);
    }
    // 需要退出码/stderr 判断成败，不走 run_adb_command_for（其不暴露状态）
    let result = utils::adb_for(Some(&device.serial))
        .arg("pull")
        .arg(AGENT_LOG_DEVICE_PATH)
        .arg(&dst)
        .output();
    match result {
        Ok(out) if out.status.success() && dst.is_file() => {
            let size = fs::metadata(&dst).map(|m| m.len()).unwrap_or(0);
            if size == 0 {
                return ChecklistEntry { name, ok: false, note: "设备端日志为空".into() };
            }
            ChecklistEntry { name, ok: true, note: human_bytes(size) }
        }
        Ok(out) => {
            let _ = fs::remove_file(&dst);
            let stderr = String::from_utf8_lossy(&out.stderr);
            ChecklistEntry {
                name,
                ok: false,
                note: format!("pull 失败: {}", stderr.lines().next().unwrap_or("未知原因")),
            }
        }
        Err(e) => ChecklistEntry { name, ok: false, note: format!("adb 调用失败: {e:#}") },
    }
}

/// 复制文件末尾 `max` 字节到目标（文件不足 max 等价全量复制）
fn copy_tail(src: &Path, dst: &Path, max: u64) -> Result<()> {
    let mut f = fs::File::open(src).with_context(|| format!("打开 {}", src.display()))?;
    let len = f.metadata()?.len();
    if len > max {
        f.seek(SeekFrom::Start(len - max))?;
    }
    let mut out = fs::File::create(dst).with_context(|| format!("创建 {}", dst.display()))?;
    std::io::copy(&mut f, &mut out)?;
    Ok(())
}

// ==================== 打包 ====================

/// 把暂存目录打成 tar.gz（tar + flate2 纯 Rust 实现；归档内路径相对暂存根）
fn pack_tar_gz(staging: &Path, archive: &Path) -> Result<()> {
    let file = fs::File::create(archive).with_context(|| format!("创建 {}", archive.display()))?;
    let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(enc);
    let mut stack = vec![staging.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)?.flatten() {
            let path = entry.path();
            let rel = path
                .strip_prefix(staging)
                .map_err(|_| anyhow!("暂存路径异常: {}", path.display()))?;
            if path.is_dir() {
                stack.push(path);
            } else {
                builder
                    .append_path_with_name(&path, rel)
                    .with_context(|| format!("归档追加失败: {}", path.display()))?;
            }
        }
    }
    let mut enc = builder.into_inner().context("tar 收尾失败")?;
    enc.flush().context("gzip 刷盘失败")?;
    Ok(())
}

// ==================== 上传（GitLab API） ====================

/// GitLab API base（`GITLAB_API` 环境变量可覆盖，与 `scripts/release_upload.py` 同源）
fn gitlab_api() -> String {
    std::env::var("GITLAB_API")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_GITLAB_API.into())
}

/// GitLab 项目 ID（`GITLAB_PROJECT_ID` 环境变量可覆盖）
fn gitlab_project_id() -> String {
    std::env::var("GITLAB_PROJECT_ID")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_PROJECT_ID.into())
}

/// 解析 GitLab token：`GITLAB_TOKEN` 环境变量 > `~/.config/xperf/gitlab-token` 文件。
pub fn resolve_token() -> Result<String> {
    resolve_token_from(
        std::env::var("GITLAB_TOKEN").ok().as_deref(),
        &token_file_path(),
    )
}

/// token 文件路径（`~/.config/xperf/gitlab-token`，与 remotes.json 同目录约定）
fn token_file_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config/xperf/gitlab-token")
}

/// [`resolve_token`] 的纯函数内核（单测可注入）
fn resolve_token_from(env: Option<&str>, token_file: &Path) -> Result<String> {
    if let Some(t) = env.map(str::trim).filter(|t| !t.is_empty()) {
        return Ok(t.to_string());
    }
    if let Ok(content) = fs::read_to_string(token_file) {
        let t = content.trim();
        if !t.is_empty() {
            return Ok(t.to_string());
        }
    }
    Err(anyhow!(
        "未找到 GitLab token（api 权限 PAT）：请设置 GITLAB_TOKEN 环境变量，\
         或将 token 写入 {}（建议 chmod 600）",
        token_file.display()
    ))
}

/// 上传归档为 issue 附件；超过 `max_attachment_size`（413）时回退 package registry。
/// 返回可嵌入 issue 正文的 markdown 链接。
fn upload_attachment(
    client: &reqwest::blocking::Client,
    token: &str,
    archive: &Path,
) -> Result<String> {
    let fname = archive
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("归档路径异常: {}", archive.display()))?
        .to_string();
    let bytes = fs::read(archive).context("读归档失败")?;
    let url = format!("{}/projects/{}/uploads", gitlab_api(), gitlab_project_id());
    let part = reqwest::blocking::multipart::Part::bytes(bytes.clone()).file_name(fname.clone());
    let resp = client
        .post(&url)
        .header("PRIVATE-TOKEN", token)
        .multipart(reqwest::blocking::multipart::Form::new().part("file", part))
        .send()
        .context("uploads 请求发送失败")?;
    if resp.status() == reqwest::StatusCode::PAYLOAD_TOO_LARGE {
        // 实例附件上限（默认 10MB）不够 → package registry（与 release 资产同路径风格）
        let stem = fname.trim_end_matches(".tar.gz");
        let reg_url = format!(
            "{}/projects/{}/packages/generic/xperf-feedback/{}/{}",
            gitlab_api(),
            gitlab_project_id(),
            stem,
            fname
        );
        let r2 = client
            .put(&reg_url)
            .header("PRIVATE-TOKEN", token)
            .body(bytes)
            .send()
            .context("package registry 请求发送失败")?;
        if !r2.status().is_success() {
            bail!("package registry 上传失败: {} {}", r2.status(), r2.text().unwrap_or_default());
        }
        return Ok(format!(
            "[{fname}]({reg_url})（超过实例附件上限，走 package registry）"
        ));
    }
    if !resp.status().is_success() {
        bail!("uploads 上传失败: {} {}", resp.status(), resp.text().unwrap_or_default());
    }
    let v: serde_json::Value = resp.json().context("解析 uploads 响应失败")?;
    v.get("markdown")
        .and_then(|m| m.as_str())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("uploads 响应缺少 markdown 字段"))
}

/// 创建 issue（labels 不被实例接受时去 labels 重试一次），返回 issue web URL。
fn create_issue(
    client: &reqwest::blocking::Client,
    token: &str,
    title: &str,
    body: &str,
) -> Result<String> {
    let url = format!("{}/projects/{}/issues", gitlab_api(), gitlab_project_id());
    for labels in ["feedback", ""] {
        let mut payload = serde_json::json!({ "title": title, "description": body });
        if !labels.is_empty() {
            payload["labels"] = serde_json::json!(labels);
        }
        let resp = client
            .post(&url)
            .header("PRIVATE-TOKEN", token)
            .json(&payload)
            .send()
            .context("issues 请求发送失败")?;
        if resp.status().is_success() {
            let v: serde_json::Value = resp.json().context("解析 issues 响应失败")?;
            return v
                .get("web_url")
                .and_then(|u| u.as_str())
                .map(str::to_string)
                .ok_or_else(|| anyhow!("issues 响应缺少 web_url 字段"));
        }
        // 非 labels 引起的失败直接报错，不再重试
        if labels.is_empty() || resp.status() != reqwest::StatusCode::BAD_REQUEST {
            bail!("创建 issue 失败: {} {}", resp.status(), resp.text().unwrap_or_default());
        }
    }
    unreachable!("labels 为空时失败已提前返回")
}

// ==================== 渲染 ====================

/// issue 标题：`[feedback] <场景摘要（单行，≤60 字符）> <日期时间>`
fn render_title(description: &str) -> String {
    let desc: String = description.split_whitespace().collect::<Vec<_>>().join(" ");
    let desc = if desc.is_empty() { "（未填写问题描述）".to_string() } else { desc };
    let mut chars = desc.chars();
    let desc: String = {
        let taken: String = chars.by_ref().take(60).collect();
        if chars.next().is_some() { format!("{taken}…") } else { taken }
    };
    format!("[feedback] {} {}", desc, Local::now().format("%Y-%m-%d %H:%M"))
}

/// 环境信息 + 清单的 manifest.json 内容
fn render_manifest(
    description: &str,
    window: Duration,
    devices: &[AdbDevice],
    checklist: &[ChecklistEntry],
) -> String {
    let transport = match crate::transport() {
        Transport::Local => serde_json::json!({ "mode": "local" }),
        Transport::Ssh(t) => serde_json::json!({ "mode": "ssh", "host": t.host }),
    };
    let devices_json: Vec<serde_json::Value> = devices
        .iter()
        .map(|d| {
            serde_json::json!({
                "serial": d.serial,
                "model": d.model,
                "android_version": d.android_version,
                "platform": d.platform.as_str(),
                "is_gateway": d.is_gateway,
            })
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::json!({
        "tool": "xperf",
        "version": env!("CARGO_PKG_VERSION"),
        "collected_at": Local::now().to_rfc3339(),
        "window_secs": window.as_secs(),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "transport": transport,
        "devices": devices_json,
        "description": description,
        "checklist": checklist,
    }))
    .unwrap_or_else(|_| "{}".into())
}

/// issue 正文（markdown）：描述/环境/数据清单/大文件/手动附加 logcat 指引
fn render_issue_body(
    description: &str,
    window: Duration,
    devices: &[AdbDevice],
    checklist: &[ChecklistEntry],
    skipped: &[SkippedFile],
) -> String {
    let mut b = String::new();
    b.push_str("## 问题描述\n\n");
    let desc = description.trim();
    b.push_str(if desc.is_empty() { "（未填写）" } else { desc });
    b.push_str("\n\n## 采集信息\n\n");
    b.push_str(&format!("- 工具版本：xperf v{}\n", env!("CARGO_PKG_VERSION")));
    b.push_str(&format!(
        "- 采集时刻：{}（窗口：最近 {} 分钟，按文件 mtime 过滤）\n",
        Local::now().format("%Y-%m-%d %H:%M:%S"),
        window.as_secs() / 60
    ));
    let transport = match crate::transport() {
        Transport::Local => "本机".to_string(),
        Transport::Ssh(t) => format!("SSH 远程（{}）", t.host),
    };
    b.push_str(&format!("- 传输模式：{transport}\n"));
    b.push_str(&format!("- 主机：{} {}\n", std::env::consts::OS, std::env::consts::ARCH));
    if devices.is_empty() {
        b.push_str("- 设备：无在线设备\n");
    } else {
        b.push_str("- 设备：\n");
        for d in devices {
            let tag = if d.is_gateway { "（MindRT 网关）" } else { "" };
            b.push_str(&format!(
                "  - `{}` {}{} Android {} [{}]\n",
                d.serial,
                d.model,
                tag,
                d.android_version,
                d.platform.as_str()
            ));
        }
    }
    b.push_str("\n## 数据清单\n\n| 项 | 状态 | 说明 |\n|---|---|---|\n");
    for e in checklist {
        b.push_str(&format!(
            "| {} | {} | {} |\n",
            e.name,
            if e.ok { "✅" } else { "⚠️" },
            e.note.replace('|', "\\|")
        ));
    }
    if !skipped.is_empty() {
        b.push_str("\n## 大文件（未入包，需要时请手动附加到本 issue）\n\n");
        for s in skipped {
            b.push_str(&format!(
                "- `{}`（{}）—— {}\n",
                s.path.display(),
                human_bytes(s.size),
                s.reason
            ));
        }
    }
    b.push_str(
        "\n## 设备 logcat（未自动采集）\n\n\
         设备 logcat 非 xperf 自身日志，未包含在归档内。如问题涉及设备侧行为，请执行：\n\n\
         ```bash\nadb logcat -d -v threadtime -v year > logcat.txt\n```\n\n\
         （多设备加 `-s <serial>`）将 `logcat.txt` 拖拽到本 issue 评论区。\n",
    );
    b
}

/// 人类可读字节数（KB/MB 一位小数）
fn human_bytes(n: u64) -> String {
    if n >= 1024 * 1024 {
        format!("{:.1} MB", n as f64 / 1024.0 / 1024.0)
    } else if n >= 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{n} B")
    }
}

/// 供测试使用的文件写入工具：创建父目录后写入内容。
#[cfg(test)]
fn write_file(path: &Path, content: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;

    fn test_device(serial: &str, is_gateway: bool) -> AdbDevice {
        AdbDevice {
            serial: serial.into(),
            product: "HU_SS3".into(),
            model: "TEST".into(),
            android_version: "12".into(),
            is_gateway,
            platform: crate::platform::PlatformId::Ss3,
        }
    }

    #[test]
    fn test_within_window_predicate() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000);
        let window = Duration::from_secs(3600);
        assert!(within_window(now - Duration::from_secs(3599), now, window));
        assert!(within_window(now - Duration::from_secs(3600), now, window));
        assert!(!within_window(now - Duration::from_secs(3601), now, window));
        // 轻微未来时间容忍（时钟差）；超出 60s 容忍则排除
        assert!(within_window(now + Duration::from_secs(30), now, window));
        assert!(!within_window(now + Duration::from_secs(120), now, window));
    }

    #[test]
    fn test_excluded_reason() {
        assert!(excluded_reason(Path::new("a/b.pftrace")).is_some());
        assert!(excluded_reason(Path::new("a/b.mp4")).is_some());
        assert!(excluded_reason(Path::new("a/b.html")).is_some());
        assert!(excluded_reason(Path::new("a/b.csv")).is_none());
        assert!(excluded_reason(Path::new("a/b.png")).is_none());
        assert!(excluded_reason(Path::new("a/b.log")).is_none());
        assert!(excluded_reason(Path::new("a/x.data")).is_none());
    }

    #[test]
    fn test_scan_sessions_filters_and_excludes() {
        let root = std::env::temp_dir().join(format!("xperf-fbtest-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        write_file(&root.join("pkg1/ts1/cpu/a_cpu.csv"), b"csv");
        write_file(&root.join("pkg1/ts1/trace/t.pftrace"), b"trace");
        write_file(&root.join("pkg1/ts1/capture/shot.png"), b"png");
        write_file(&root.join("feedback/old.tar.gz"), b"self");
        let (files, skipped) = scan_sessions(&root, SystemTime::now(), Duration::from_secs(365 * 24 * 3600));
        let rels: Vec<String> = files.iter().map(|f| f.rel.to_string_lossy().into_owned()).collect();
        assert!(rels.iter().any(|r| r.ends_with("a_cpu.csv")), "csv 应被收集: {rels:?}");
        assert!(rels.iter().any(|r| r.ends_with("shot.png")), "png 应被收集: {rels:?}");
        assert!(!rels.iter().any(|r| r.contains("feedback")), "feedback/ 自身目录应跳过: {rels:?}");
        assert_eq!(skipped.len(), 1, "pftrace 应被排除: {skipped:?}");
        // 窗口为 0：已存在文件（mtime 在过去）全部不收集
        let (files0, _) = scan_sessions(&root, SystemTime::now(), Duration::ZERO);
        assert!(files0.is_empty(), "零窗口不应收到历史文件: {:?}", files0.len());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_copy_tail_truncates() {
        let dir = std::env::temp_dir().join(format!("xperf-fbtest-tail-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let src = dir.join("big.log");
        let content: Vec<u8> = (0..300u32).map(|i| (i % 256) as u8).collect();
        write_file(&src, &content);
        let dst = dir.join("tail.log");
        copy_tail(&src, &dst, 100).unwrap();
        let got = fs::read(&dst).unwrap();
        assert_eq!(got.len(), 100);
        assert_eq!(got, content[200..], "应保留末尾 100 字节");
        // 不足上限时全量
        let dst2 = dir.join("full.log");
        copy_tail(&src, &dst2, 1000).unwrap();
        assert_eq!(fs::read(&dst2).unwrap(), content);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_bucket_of() {
        assert_eq!(bucket_of(Path::new("p/t/trace/a.txt")), "perfetto 报告");
        assert_eq!(bucket_of(Path::new("p/t/stack/r.data")), "simpleperf 报告");
        assert_eq!(bucket_of(Path::new("p/t/capture/s.png")), "截屏");
        assert_eq!(bucket_of(Path::new("p/t/logcat/l.log")), "会话 logcat");
        assert_eq!(bucket_of(Path::new("p/t/cpu/a.csv")), "采样 CSV");
        assert_eq!(bucket_of(Path::new("p/t/markers.csv")), "采样 CSV");
        assert_eq!(bucket_of(Path::new("p/t/unknown.bin")), "其他产物");
    }

    #[test]
    fn test_resolve_token_from_priority() {
        let dir = std::env::temp_dir().join(format!("xperf-fbtest-tok-{}", std::process::id()));
        let token_file = dir.join("gitlab-token");
        // env 优先
        write_file(&token_file, b"file-token\n");
        assert_eq!(resolve_token_from(Some(" env-token "), &token_file).unwrap(), "env-token");
        // 文件兜底（去空白）
        assert_eq!(resolve_token_from(None, &token_file).unwrap(), "file-token");
        assert_eq!(resolve_token_from(Some("  "), &token_file).unwrap(), "file-token");
        // 均无 → 报错且指引文件路径
        let err = resolve_token_from(None, &dir.join("nonexist")).unwrap_err().to_string();
        assert!(err.contains("GITLAB_TOKEN"), "错误应指引环境变量: {err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_render_title_sanitizes() {
        let t = render_title("  采样 CPU\n异常  飙高 ");
        assert!(t.starts_with("[feedback] 采样 CPU 异常 飙高 "), "标题应单行化: {t}");
        let long = render_title(&"很".repeat(100));
        assert!(long.chars().count() < 90, "标题应截断: {long}");
        let empty = render_title("   ");
        assert!(empty.contains("（未填写问题描述）"), "{empty}");
    }

    #[test]
    fn test_render_issue_body_sections() {
        let checklist = vec![
            ChecklistEntry { name: "采样 CSV".into(), ok: true, note: "3 个文件".into() },
            ChecklistEntry { name: "agent 日志 s1".into(), ok: false, note: "pull 失败: no such file".into() },
        ];
        let skipped = vec![SkippedFile {
            path: PathBuf::from("/tmp/xperf/p/t/trace/x.pftrace"),
            size: 46 * 1024 * 1024,
            reason: "perfetto trace 体积过大（10s≈46MB）",
        }];
        let devices = vec![test_device("s1", false), test_device("mindrt", true)];
        let body = render_issue_body("CPU 飙高", DEFAULT_WINDOW, &devices, &checklist, &skipped);
        assert!(body.contains("CPU 飙高"));
        assert!(body.contains("xperf v"));
        assert!(body.contains("| 采样 CSV | ✅ | 3 个文件 |"));
        assert!(body.contains("| agent 日志 s1 | ⚠️ | pull 失败: no such file |"));
        assert!(body.contains("x.pftrace"), "大文件节应列路径");
        assert!(body.contains("MindRT 网关"), "网关设备应标注: {body}");
        assert!(body.contains("adb logcat -d"), "应含手动附加 logcat 指引");
    }

    #[test]
    fn test_collect_in_end_to_end() {
        let root = std::env::temp_dir().join(format!("xperf-fbtest-e2e-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        write_file(&root.join("pkg1/ts1/cpu/a_cpu.csv"), b"ts,cpu\n1,2\n");
        write_file(&root.join("pkg1/ts1/memory/a_mem.csv"), b"ts,pss\n1,2\n");
        write_file(&root.join("pkg1/ts1/trace/trace_analysis.txt"), b"report");
        // 无设备（空列表）→ agent 日志条目为不达标但流程完成
        let bundle = collect_in(
            &root,
            "测试描述",
            Duration::from_secs(365 * 24 * 3600),
            &[],
            MAX_FILE_BYTES,
        )
        .unwrap();
        assert!(bundle.archive.is_file(), "归档应生成: {}", bundle.archive.display());
        assert!(bundle.archive_bytes > 0);
        assert!(bundle.checklist.iter().any(|e| e.name == "采样 CSV" && e.ok));
        assert!(bundle.checklist.iter().any(|e| e.name == "perfetto 报告" && e.ok));
        assert!(bundle.checklist.iter().any(|e| e.name == "agent 日志" && !e.ok));
        assert!(bundle.title.contains("测试描述"));
        // 归档内容：manifest.json + issue.md + 两个 csv
        let file = fs::File::open(&bundle.archive).unwrap();
        let dec = flate2::read::GzDecoder::new(file);
        let mut ar = tar::Archive::new(dec);
        let names: Vec<String> = ar
            .entries()
            .unwrap()
            .map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().any(|n| n.contains("manifest.json")), "{names:?}");
        assert!(names.iter().any(|n| n.contains("issue.md")), "{names:?}");
        assert!(names.iter().filter(|n| n.ends_with(".csv")).count() == 2, "{names:?}");
        // manifest 内含版本与描述
        let file = fs::File::open(&bundle.archive).unwrap();
        let dec = flate2::read::GzDecoder::new(file);
        let mut ar = tar::Archive::new(dec);
        let mut manifest = String::new();
        for e in ar.entries().unwrap() {
            let mut e = e.unwrap();
            if e.path().unwrap().to_string_lossy().contains("manifest.json") {
                e.read_to_string(&mut manifest).unwrap();
            }
        }
        assert!(manifest.contains("\"tool\": \"xperf\""), "{manifest}");
        assert!(manifest.contains("测试描述"));
        let _ = fs::remove_dir_all(&root);
    }
}
