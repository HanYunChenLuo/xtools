//! 基线对比：两次采样会话的关键统计对比（性能回归验证）。
//!
//! 工作流：`--save-baseline` 把本次会话的汇总统计存为该包的基线（JSON），
//! 改动后再次采样用 `--compare-baseline` 与基线对比，输出逐指标的
//! 回归/改善/持平判定报告。CLI 与 GUI 共用本模块（两侧各自灌入样本，
//! 汇总口径一致），基线文件互通。
//!
//! 基线存放于 `~/.local/share/xperf/baselines/<pkg>.json`（XDG 数据目录，
//! 用户数据语义——`--clean-cache` 不清理）。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 单指标汇总统计（跨 PID 合并全部样本后计算）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MetricSummary {
    /// 全部样本的算术平均
    pub avg: f64,
    /// 全部样本的最大值
    pub max: f64,
    /// 样本数
    pub count: u64,
}

impl MetricSummary {
    /// 由样本值序列计算（空序列或全 NaN 返回 `None`——该指标未采集；
    /// NaN 样本被过滤，计数为有效样本数）
    fn from_values(values: &[f64]) -> Option<Self> {
        let values: Vec<f64> = values.iter().copied().filter(|v| !v.is_nan()).collect();
        if values.is_empty() {
            return None;
        }
        let sum: f64 = values.iter().sum();
        Some(MetricSummary {
            avg: sum / values.len() as f64,
            max: values.iter().cloned().fold(f64::MIN, f64::max),
            count: values.len() as u64,
        })
    }
}

/// 一次采样会话的汇总统计（基线保存与对比的数据体）。
///
/// 指标口径：
/// - 多 PID 合并：包内全部进程的样本合并统计（`pids` 记录参与的 PID）
/// - CPU：单核口径 %（多线程可超 100）
/// - 内存：PSS，单位 KB（报告展示为 MB）
/// - FPS：全部样本均值（静止界面 0 帧样本计入——两次同场景对比下口径公平）
/// - Jank：总次数（对比时换算为每分钟次数，须 `duration_s` > 0）
/// - GPU busy：%（QNX/降级通道的整机 busy）
/// - IO/网络：KB/s 速率均值
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    /// 基线文件格式版本（未来字段演进用）
    pub version: u32,
    /// 包名
    pub package: String,
    /// 保存时刻（本地时区 `YYYY-MM-DD HH:MM:SS`，报告展示用）
    pub saved_at: String,
    /// 采样间隔（毫秒）
    pub interval_ms: u64,
    /// 采样时长（秒；首样本到末样本的墙钟跨度，0 = 单样本或无样本）
    pub duration_s: f64,
    /// CPU 样本总数（全部 PID 合并）
    pub samples: u64,
    /// 参与统计的 PID 列表（排序后）
    pub pids: Vec<String>,
    /// 进程重启次数（`None` = 该会话未统计，如 GUI 路径）
    pub restarts: Option<u32>,
    /// 冷启动 TotalTime 毫秒数（`None` = 本次未测量）
    pub cold_start_ms: Option<u64>,
    /// CPU %（单核口径，全 PID 合并）
    pub cpu: Option<MetricSummary>,
    /// 内存 PSS（KB，全 PID 合并）
    pub mem_pss_kb: Option<MetricSummary>,
    /// FPS（全部样本均值，静止 0 帧计入）
    pub fps: Option<MetricSummary>,
    /// Jank 帧总数（FPS 采集时有效；对比换算为每分钟次数）
    pub jank_total: Option<u64>,
    /// GPU busy %
    pub gpu_busy: Option<MetricSummary>,
    /// IO 读速率（KB/s，全 PID 合并）
    pub io_read_kb_s: Option<MetricSummary>,
    /// IO 写速率（KB/s，全 PID 合并）
    pub io_write_kb_s: Option<MetricSummary>,
    /// 网络接收速率（KB/s，整机口径）
    pub net_rx_kb_s: Option<MetricSummary>,
    /// 网络发送速率（KB/s，整机口径）
    pub net_tx_kb_s: Option<MetricSummary>,
}

/// 会话汇总构建器：调用方（CLI/GUI）逐指标灌入样本值，`finish` 计算。
///
/// 样本为裸数值（时间戳不参与统计，时长由调用方另行计算传入）。
#[derive(Debug, Default)]
pub struct SummaryBuilder {
    package: String,
    saved_at: String,
    interval_ms: u64,
    duration_s: f64,
    pids: Vec<String>,
    restarts: Option<u32>,
    cold_start_ms: Option<u64>,
    cpu: Vec<f64>,
    mem_pss_kb: Vec<f64>,
    fps: Vec<f64>,
    jank_total: u64,
    gpu_busy: Vec<f64>,
    io_read: Vec<f64>,
    io_write: Vec<f64>,
    net_rx: Vec<f64>,
    net_tx: Vec<f64>,
}

impl SummaryBuilder {
    /// 创建构建器（`package`/`interval_ms`/`duration_s` 为会话元信息）
    pub fn new(package: &str, interval_ms: u64, duration_s: f64) -> Self {
        SummaryBuilder {
            package: package.to_string(),
            saved_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            interval_ms,
            duration_s,
            ..Default::default()
        }
    }

    /// 记录参与统计的 PID 列表（报告展示用；不清除重复，调用方传排序去重结果）
    pub fn pids(&mut self, pids: Vec<String>) -> &mut Self {
        self.pids = pids;
        self
    }

    /// 进程重启次数（`None` = 未统计）
    pub fn restarts(&mut self, n: Option<u32>) -> &mut Self {
        self.restarts = n;
        self
    }

    /// 冷启动 TotalTime 毫秒数（`None` = 未测量）
    pub fn cold_start_ms(&mut self, ms: Option<u64>) -> &mut Self {
        self.cold_start_ms = ms;
        self
    }

    /// 追加一个 CPU 样本（单核口径 %）
    pub fn push_cpu(&mut self, v: f64) {
        self.cpu.push(v);
    }

    /// 追加一个内存 PSS 样本（KB）
    pub fn push_mem(&mut self, v: f64) {
        self.mem_pss_kb.push(v);
    }

    /// 追加一个 FPS 样本（含该样本的 jank 帧数一并累计）
    pub fn push_fps(&mut self, fps: f64, jank: u32) {
        self.fps.push(fps);
        self.jank_total += jank as u64;
    }

    /// 追加一个 GPU busy 样本（%）
    pub fn push_gpu(&mut self, v: f64) {
        self.gpu_busy.push(v);
    }

    /// 追加一个 IO 速率样本（读/写 KB/s）
    pub fn push_io(&mut self, r: f64, w: f64) {
        self.io_read.push(r);
        self.io_write.push(w);
    }

    /// 追加一个网络速率样本（RX/TX KB/s，整机口径）
    pub fn push_net(&mut self, rx: f64, tx: f64) {
        self.net_rx.push(rx);
        self.net_tx.push(tx);
    }

    /// 计算汇总（未采集的指标为 `None`，对比时跳过并如实标注）
    pub fn finish(self) -> SessionSummary {
        let samples = self.cpu.len() as u64;
        SessionSummary {
            version: 1,
            package: self.package,
            saved_at: self.saved_at,
            interval_ms: self.interval_ms,
            duration_s: self.duration_s,
            samples,
            pids: self.pids,
            restarts: self.restarts,
            cold_start_ms: self.cold_start_ms,
            cpu: MetricSummary::from_values(&self.cpu),
            mem_pss_kb: MetricSummary::from_values(&self.mem_pss_kb),
            fps: MetricSummary::from_values(&self.fps),
            jank_total: if self.fps.is_empty() { None } else { Some(self.jank_total) },
            gpu_busy: MetricSummary::from_values(&self.gpu_busy),
            io_read_kb_s: MetricSummary::from_values(&self.io_read),
            io_write_kb_s: MetricSummary::from_values(&self.io_write),
            net_rx_kb_s: MetricSummary::from_values(&self.net_rx),
            net_tx_kb_s: MetricSummary::from_values(&self.net_tx),
        }
    }
}

/// 基线存放根目录：`$XDG_DATA_HOME/xperf/baselines`（缺省 `$HOME/.local/share/…`；
/// HOME 亦未设时落到当前目录 `./xperf/baselines`）
pub fn baseline_dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|h| !h.is_empty())
                .map(|h| PathBuf::from(h).join(".local").join("share"))
        })
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("xperf").join("baselines")
}

/// 基线文件路径 `<root>/<package>.json`（包名拼入路径前校验防遍历）
fn baseline_path_in(root: &Path, package: &str) -> Result<PathBuf> {
    if package.is_empty() || package.len() > 255
        || !package
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
    {
        anyhow::bail!("包名不合法（基线路径拼接）: {:?}", package);
    }
    Ok(root.join(format!("{}.json", package)))
}

/// 把会话汇总写入指定路径（pretty JSON；父目录自动创建）
pub fn save_to(path: &Path, summary: &SessionSummary) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建基线目录失败: {}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(summary).context("基线 JSON 序列化失败")?;
    std::fs::write(path, text).with_context(|| format!("基线写入失败: {}", path.display()))
}



/// 读取基线（JSON 解析失败/文件不存在均报错并带路径上下文）。
/// 版本字段（version）缺失或非 1 时报错（字段演进后再加迁移，当前只接受 v1）
pub fn load_from(path: &Path) -> Result<SessionSummary> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("基线文件不存在或不可读: {}", path.display()))?;
    let summary: SessionSummary =
        serde_json::from_str(&text).with_context(|| format!("基线 JSON 解析失败: {}", path.display()))?;
    if summary.version != 1 {
        anyhow::bail!(
            "基线版本不兼容（文件 version={}，当前只支持 1）：{}",
            summary.version,
            path.display()
        );
    }
    Ok(summary)
}

/// 保存为该包的基线（覆盖旧基线），返回文件路径
pub fn save(package: &str, summary: &SessionSummary) -> Result<PathBuf> {
    let path = baseline_path_in(&baseline_dir(), package)?;
    save_to(&path, summary)?;
    Ok(path)
}

/// 读取该包已保存的基线
pub fn load(package: &str) -> Result<SessionSummary> {
    let path = baseline_path_in(&baseline_dir(), package)?;
    load_from(&path)
}

// ---------- 对比 ----------

/// 显著性判定的相对容差（百分比）：|变化| ≥ 10% 才可能判回归/改善
const REL_TOL_PCT: f64 = 10.0;

/// 单指标对比结论
#[derive(Debug, Clone, Copy, PartialEq)]
enum Verdict {
    /// 本次比基线差（超容差）
    Regression,
    /// 本次比基线好（超容差）
    Improvement,
    /// 变化在容差内
    Flat,
    /// 仅一侧采集了该指标，无法对比
    NoCompare,
}

/// 对比一行：指标名、两侧值、变化量与结论
struct Row {
    name: String,
    /// 基线值（`None` = 基线未采集）
    base: Option<f64>,
    /// 本次值（`None` = 本次未采集）
    cur: Option<f64>,
    /// 数值格式化（含单位）
    fmt: fn(f64) -> String,
    /// 绝对地板值：|变化| 低于它视为噪声（相对容差之外的第二道闸）
    floor: f64,
    /// true = 数值越低越好（CPU/内存/IO…）；false = 越高越好（FPS）
    lower_better: bool,
    /// 变化百分比是否参与判定（计数类指标如重启次数不用百分比，绝对差即判定）
    pct_relevant: bool,
}

impl Row {
    fn verdict(&self) -> Verdict {
        let (Some(b), Some(c)) = (self.base, self.cur) else {
            return Verdict::NoCompare;
        };
        let delta = c - b;
        // 相对容差取绝对值：回归与改善方向（delta 正负）对称判显著
        // ——曾用带符号百分比致 delta<0（FPS 回归/CPU 改善）恒判持平（review S1）
        let significant = if self.pct_relevant && b > 0.0 {
            delta.abs() >= self.floor && (delta / b * 100.0).abs() >= REL_TOL_PCT
        } else {
            delta.abs() >= self.floor
        };
        if !significant {
            return Verdict::Flat;
        }
        let worse = if self.lower_better { delta > 0.0 } else { delta < 0.0 };
        if worse {
            Verdict::Regression
        } else {
            Verdict::Improvement
        }
    }

    /// 变化列文本：相对百分比（基线 > 0 时），否则绝对差
    fn delta_text(&self) -> String {
        let (Some(b), Some(c)) = (self.base, self.cur) else {
            return "⊘".into();
        };
        if b > 0.0 {
            format!("{:+.1}%", (c - b) / b * 100.0)
        } else if (c - b).abs() < f64::EPSILON {
            "0%".into()
        } else {
            format!("{:+.1}", c - b) // 基线为 0：显示绝对差
        }
    }
}

/// 生成对比报告文本（CLI 直接打印、GUI 展示于面板、两侧同格式）。
///
/// 判定口径（详见模块文档）：变化同时超过相对容差（±10%）与
/// 指标绝对地板值才判回归/改善，否则视为持平；仅一侧采集的指标如实标注。
pub fn compare(base: &SessionSummary, cur: &SessionSummary) -> String {
    let ms = |m: &Option<MetricSummary>| m.as_ref().map(|x| x.avg);
    let ms_max = |m: &Option<MetricSummary>| m.as_ref().map(|x| x.max);
    let jank_rate = |s: &SessionSummary| -> Option<f64> {
        // 时长为 0（单样本/无样本）时无法换算速率，视为未采集
        match (s.jank_total, s.duration_s) {
            (Some(total), d) if d > 0.0 => Some(total as f64 / d * 60.0),
            _ => None,
        }
    };
    let rows = vec![
        Row { name: "CPU 均值 (%)".into(), base: ms(&base.cpu), cur: ms(&cur.cpu), fmt: |v| format!("{:.1}", v), floor: 2.0, lower_better: true, pct_relevant: true },
        Row { name: "CPU 峰值 (%)".into(), base: ms_max(&base.cpu), cur: ms_max(&cur.cpu), fmt: |v| format!("{:.1}", v), floor: 2.0, lower_better: true, pct_relevant: true },
        Row { name: "PSS 均值 (MB)".into(), base: ms(&base.mem_pss_kb).map(|kb| kb / 1024.0), cur: ms(&cur.mem_pss_kb).map(|kb| kb / 1024.0), fmt: |v| format!("{:.1}", v), floor: 4.0, lower_better: true, pct_relevant: true },
        Row { name: "PSS 峰值 (MB)".into(), base: ms_max(&base.mem_pss_kb).map(|kb| kb / 1024.0), cur: ms_max(&cur.mem_pss_kb).map(|kb| kb / 1024.0), fmt: |v| format!("{:.1}", v), floor: 4.0, lower_better: true, pct_relevant: true },
        Row { name: "FPS 均值".into(), base: ms(&base.fps), cur: ms(&cur.fps), fmt: |v| format!("{:.1}", v), floor: 2.0, lower_better: false, pct_relevant: true },
        Row { name: "Jank 频率 (次/分)".into(), base: jank_rate(base), cur: jank_rate(cur), fmt: |v| format!("{:.2}", v), floor: 0.5, lower_better: true, pct_relevant: true },
        Row { name: "GPU busy 均值 (%)".into(), base: ms(&base.gpu_busy), cur: ms(&cur.gpu_busy), fmt: |v| format!("{:.1}", v), floor: 3.0, lower_better: true, pct_relevant: true },
        Row { name: "GPU busy 峰值 (%)".into(), base: ms_max(&base.gpu_busy), cur: ms_max(&cur.gpu_busy), fmt: |v| format!("{:.1}", v), floor: 3.0, lower_better: true, pct_relevant: true },
        Row { name: "IO 读均值 (KB/s)".into(), base: ms(&base.io_read_kb_s), cur: ms(&cur.io_read_kb_s), fmt: |v| format!("{:.1}", v), floor: 50.0, lower_better: true, pct_relevant: true },
        Row { name: "IO 写均值 (KB/s)".into(), base: ms(&base.io_write_kb_s), cur: ms(&cur.io_write_kb_s), fmt: |v| format!("{:.1}", v), floor: 50.0, lower_better: true, pct_relevant: true },
        Row { name: "网络 RX 均值 (KB/s)".into(), base: ms(&base.net_rx_kb_s), cur: ms(&cur.net_rx_kb_s), fmt: |v| format!("{:.1}", v), floor: 50.0, lower_better: true, pct_relevant: true },
        Row { name: "网络 TX 均值 (KB/s)".into(), base: ms(&base.net_tx_kb_s), cur: ms(&cur.net_tx_kb_s), fmt: |v| format!("{:.1}", v), floor: 50.0, lower_better: true, pct_relevant: true },
        Row { name: "进程重启次数".into(), base: base.restarts.map(|v| v as f64), cur: cur.restarts.map(|v| v as f64), fmt: |v| format!("{:.0}", v), floor: 1.0, lower_better: true, pct_relevant: false },
        Row { name: "冷启动 (ms)".into(), base: base.cold_start_ms.map(|v| v as f64), cur: cur.cold_start_ms.map(|v| v as f64), fmt: |v| format!("{:.0}", v), floor: 150.0, lower_better: true, pct_relevant: true },
    ];

    let mut lines = vec![
        "========== 基线对比报告 ==========".to_string(),
        format!(
            "基线: {}（时长 {:.0}s，间隔 {}ms，样本 {}，PID [{}]）",
            base.saved_at,
            base.duration_s,
            base.interval_ms,
            base.samples,
            base.pids.join(",")
        ),
        format!(
            "本次: 时长 {:.0}s，间隔 {}ms，样本 {}，PID [{}]",
            cur.duration_s, cur.interval_ms, cur.samples, cur.pids.join(",")
        ),
        String::new(),
        format!("{:<18}{:>12}{:>12}{:>10}  {}", "指标", "基线", "本次", "变化", "结论"),
    ];

    let mut n_reg = 0;
    let mut n_imp = 0;
    let mut n_flat = 0;
    let mut n_nc = 0;
    for r in &rows {
        let verdict = r.verdict();
        match verdict {
            Verdict::Regression => n_reg += 1,
            Verdict::Improvement => n_imp += 1,
            Verdict::Flat => n_flat += 1,
            Verdict::NoCompare => n_nc += 1,
        }
        let verdict_str = match verdict {
            Verdict::Regression => "⚠ 回归",
            Verdict::Improvement => "✅ 改善",
            Verdict::Flat => "— 持平",
            Verdict::NoCompare => "⊘ 单侧未采集",
        };
        let base_str = r.base.map(r.fmt).unwrap_or_else(|| "⊘".into());
        let cur_str = r.cur.map(r.fmt).unwrap_or_else(|| "⊘".into());
        lines.push(format!(
            "{:<18}{:>12}{:>12}{:>10}  {}",
            r.name, base_str, cur_str, r.delta_text(), verdict_str
        ));
    }

    lines.push(String::new());
    if n_reg == 0 {
        if n_nc == rows.len() {
            lines.push("总结论: ⊘ 无可对比指标（基线与本次采集的指标完全不重叠）".to_string());
        } else {
            lines.push(format!(
                "总结论: ✅ 无回归（改善 {} 项，持平 {} 项，单侧未采集 {} 项）",
                n_imp, n_flat, n_nc
            ));
        }
    } else {
        let reg_names: Vec<&str> = rows
            .iter()
            .filter(|r| r.verdict() == Verdict::Regression)
            .map(|r| r.name.as_str())
            .collect();
        lines.push(format!(
            "总结论: ⚠ 存在回归 {} 项（{}）；改善 {} 项，持平 {} 项，单侧未采集 {} 项",
            n_reg,
            reg_names.join("、"),
            n_imp,
            n_flat,
            n_nc
        ));
    }
    lines.push("判定口径: 变化须同时超过相对 ±10% 与指标地板值才判回归/改善".to_string());
    lines.push("（地板值抑制近零噪声：CPU 2pp / PSS 4MB / FPS 2 / Jank 0.5 / GPU 3pp / IO·网络 50KB/s / 冷启动 150ms）".to_string());
    lines.push("==============================".to_string());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary_with_cpu(cpu: &[f64], duration_s: f64) -> SessionSummary {
        let mut b = SummaryBuilder::new("com.test.app", 1000, duration_s);
        for &v in cpu {
            b.push_cpu(v);
        }
        b.finish()
    }

    #[test]
    fn test_metric_summary_from_values() {
        assert_eq!(MetricSummary::from_values(&[]), None);
        let m = MetricSummary::from_values(&[1.0, 2.0, 6.0]).unwrap();
        assert_eq!(m.count, 3);
        assert!((m.avg - 3.0).abs() < 1e-9);
        assert_eq!(m.max, 6.0);
    }

    #[test]
    fn test_builder_optional_metrics() {
        let s = SummaryBuilder::new("p", 500, 10.0).finish();
        assert_eq!(s.cpu, None);
        assert_eq!(s.jank_total, None); // 无 FPS 样本时 jank 不出数
        assert_eq!(s.samples, 0);

        let mut b = SummaryBuilder::new("p", 500, 60.0);
        b.push_cpu(10.0);
        b.push_mem(300_000.0);
        b.push_fps(58.0, 2);
        b.push_gpu(12.0);
        b.push_io(100.0, 200.0);
        b.push_net(30.0, 40.0);
        b.pids(vec!["100".into()]);
        b.restarts(Some(1));
        b.cold_start_ms(Some(1200));
        let s = b.finish();
        assert_eq!(s.cpu.unwrap().avg, 10.0);
        assert_eq!(s.mem_pss_kb.unwrap().max, 300_000.0);
        assert_eq!(s.fps.unwrap().count, 1);
        assert_eq!(s.jank_total, Some(2));
        assert_eq!(s.restarts, Some(1));
        assert_eq!(s.cold_start_ms, Some(1200));
        assert_eq!(s.version, 1);
    }

    #[test]
    fn test_save_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!("xperf_baseline_test_{}", std::process::id()));
        let path = baseline_path_in(&dir, "com.test.app").unwrap();
        let s = summary_with_cpu(&[5.0, 15.0], 30.0);
        save_to(&path, &s).unwrap();
        let loaded = load_from(&path).unwrap();
        assert_eq!(loaded.package, "com.test.app");
        assert_eq!(loaded.cpu, s.cpu);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_baseline_path_rejects_traversal() {
        let dir = Path::new("/tmp");
        assert!(baseline_path_in(dir, "../evil").is_err());
        assert!(baseline_path_in(dir, "").is_err());
        assert!(baseline_path_in(dir, "a/b").is_err());
        assert!(baseline_path_in(dir, "com.ok.app-1").is_ok());
    }

    #[test]
    fn test_verdict_flat_within_tolerance() {
        // +5pp 但相对 5% < 10% → 持平
        let base = summary_with_cpu(&[100.0; 10], 10.0);
        let cur = summary_with_cpu(&[105.0; 10], 10.0);
        let r = compare(&base, &cur);
        assert!(r.contains("CPU 均值"));
        assert!(r.contains("— 持平"));
        assert!(r.contains("✅ 无回归"));
    }

    #[test]
    fn test_verdict_regression() {
        // +50% 且 +50pp → 回归
        let base = summary_with_cpu(&[100.0; 10], 10.0);
        let cur = summary_with_cpu(&[150.0; 10], 10.0);
        let r = compare(&base, &cur);
        assert!(r.contains("⚠ 回归"));
        assert!(r.contains("CPU 均值"));
        assert!(r.contains("⚠ 存在回归 2 项（CPU 均值 (%)、CPU 峰值 (%)）"));
    }

    #[test]
    fn test_verdict_improvement_and_floor() {
        // 高 50% → FPS 改善
        let mut base_b = SummaryBuilder::new("p", 1000, 10.0);
        for _ in 0..10 {
            base_b.push_fps(40.0, 0);
        }
        let mut cur_b = SummaryBuilder::new("p", 1000, 10.0);
        for _ in 0..10 {
            cur_b.push_fps(60.0, 0);
        }
        let r = compare(&base_b.finish(), &cur_b.finish());
        assert!(r.contains("✅ 改善"));

        // 地板值：基线 58 → 59（+1.7%，< 2 地板）→ 持平
        let mut base_b = SummaryBuilder::new("p", 1000, 10.0);
        for _ in 0..10 {
            base_b.push_fps(58.0, 0);
        }
        let mut cur_b = SummaryBuilder::new("p", 1000, 10.0);
        for _ in 0..10 {
            cur_b.push_fps(59.0, 0);
        }
        let r = compare(&base_b.finish(), &cur_b.finish());
        assert!(r.contains("— 持平"));
    }

    #[test]
    fn test_verdict_base_zero() {
        // 基线 0 → 本次 8（≥ 地板 2）→ 回归（无百分比可算，显示绝对差）
        let base = summary_with_cpu(&[0.0; 10], 10.0);
        let cur = summary_with_cpu(&[8.0; 10], 10.0);
        let r = compare(&base, &cur);
        assert!(r.contains("⚠ 回归"));
        assert!(r.contains("+8.0"));

        // 基线 0 → 本次 1.5（< 地板 2）→ 持平
        let cur = summary_with_cpu(&[1.5; 10], 10.0);
        let r = compare(&base, &cur);
        assert!(r.contains("— 持平"));
    }

    #[test]
    fn test_one_side_missing_metric() {
        let base = summary_with_cpu(&[10.0; 10], 10.0);
        let mut cur_b = SummaryBuilder::new("p", 1000, 10.0);
        cur_b.push_mem(300_000.0); // 只采内存
        let r = compare(&base, &cur_b.finish());
        assert!(r.contains("⊘ 单侧未采集"));
    }

    #[test]
    fn test_jank_rate_computed_per_side_duration() {
        // 基线 60s 内 60 次 jank（1 次/分），本次 30s 内 60 次（2 次/分）→ 回归
        let mut base_b = SummaryBuilder::new("p", 1000, 60.0);
        for _ in 0..60 {
            base_b.push_fps(58.0, 1);
        }
        let mut cur_b = SummaryBuilder::new("p", 1000, 30.0);
        for _ in 0..30 {
            cur_b.push_fps(58.0, 2);
        }
        let r = compare(&base_b.finish(), &cur_b.finish());
        assert!(r.contains("Jank 频率 (次/分)"));
        assert!(r.contains("⚠ 回归"));

        // 时长 0 → jank 率不可算 → 单侧未采集
        let mut zero_dur = SummaryBuilder::new("p", 1000, 0.0);
        zero_dur.push_fps(58.0, 5);
        let mut other = SummaryBuilder::new("p", 1000, 10.0);
        other.push_fps(58.0, 0);
        let r = compare(&zero_dur.finish(), &other.finish());
        assert!(r.contains("⊘ 单侧未采集"));
    }

    #[test]
    fn test_restarts_absolute_not_pct() {
        // 2 → 3：绝对差 1（无百分比判定）→ 回归
        let mut base_b = SummaryBuilder::new("p", 1000, 10.0);
        base_b.push_cpu(10.0);
        base_b.restarts(Some(2));
        let mut cur_b = SummaryBuilder::new("p", 1000, 10.0);
        cur_b.push_cpu(10.0);
        cur_b.restarts(Some(3));
        let r = compare(&base_b.finish(), &cur_b.finish());
        assert!(r.contains("进程重启次数"));
        assert!(r.contains("⚠ 回归"));
    }

    // ---- review S1：负 delta 方向（FPS 回归 / CPU 改善）判定 ----

    #[test]
    fn test_verdict_negative_delta_fps_regression() {
        // FPS 60 → 30（-50%，delta=-30 ≥ floor 2 且 |-50%| ≥ 10%）→ 回归
        // 曾因相对容差用带符号百分比致 delta<0 恒判持平（S1）
        let mut base_b = SummaryBuilder::new("p", 1000, 60.0);
        for _ in 0..10 {
            base_b.push_fps(60.0, 0);
        }
        let mut cur_b = SummaryBuilder::new("p", 1000, 60.0);
        for _ in 0..10 {
            cur_b.push_fps(30.0, 0);
        }
        let r = compare(&base_b.finish(), &cur_b.finish());
        assert!(r.contains("⚠ 回归"), "FPS 60→30 应判回归，实际:\n{}", r);
        assert!(r.contains("FPS 均值"));
    }

    #[test]
    fn test_verdict_negative_delta_cpu_improvement() {
        // CPU 50 → 25（-50%，delta=-25 ≥ floor 2 且 |-50%| ≥ 10%，lower_better）→ 改善
        let base = summary_with_cpu(&[50.0; 10], 10.0);
        let cur = summary_with_cpu(&[25.0; 10], 10.0);
        let r = compare(&base, &cur);
        assert!(r.contains("✅ 改善"), "CPU 50→25 应判改善，实际:\n{}", r);
    }

    // ---- review G1：全不可比不应误报"无回归" ----

    #[test]
    fn test_all_no_compare_not_no_regression() {
        // 基线只有 CPU，本次只有内存 → 全部单侧未采集 → ⊘ 无可对比指标
        let base = summary_with_cpu(&[10.0; 10], 10.0);
        let mut cur_b = SummaryBuilder::new("p", 1000, 10.0);
        cur_b.push_mem(300_000.0);
        let r = compare(&base, &cur_b.finish());
        assert!(r.contains("⊘ 无可对比指标"), "全不可比应报无可对比，实际:\n{}", r);
        assert!(!r.contains("✅ 无回归"), "全不可比不应报无回归");
    }
}
