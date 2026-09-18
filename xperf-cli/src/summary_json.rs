//! 退出汇总文件 `summary.json`：采样会话结束时落盘 `<ts>/summary.json`。
//!
//! 顶层字段经 `serde(flatten)` 平铺 [`SessionSummary`] 全部字段——与
//! `--save-baseline` 保存的基线 JSON 同 schema（同会话两文件口径一致），
//! 附加阈值判定（`thresholds`）与基线动作结论（`baseline`）两个结构化字段，
//! 供编码 agent 免解析终端文本直接断言（如 `jq .thresholds.all_pass`）。

use anyhow::{Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use xperf_core::baseline::{CompareOutcome, SessionSummary};

use crate::alerts::ThresholdOutcome;

/// `summary.json` 文件体：顶层 = [`SessionSummary`] 全部字段 + 判定结论
#[derive(Debug, Serialize)]
pub struct SummaryFile {
    /// 会话汇总（字段与基线 JSON 完全一致——flatten 平铺到顶层）
    #[serde(flatten)]
    pub summary: SessionSummary,
    /// 阈值判定结论（未给 `--threshold` 时为 null）
    pub thresholds: Option<ThresholdOutcome>,
    /// 基线动作结论（未给 `--save-baseline`/`--compare-baseline` 时为 null）
    pub baseline: Option<BaselineVerdict>,
}

/// 基线动作结论（`--save-baseline` / `--compare-baseline` 的执行结果）
#[derive(Debug, Serialize, PartialEq)]
pub struct BaselineVerdict {
    /// 动作：`saved`（基线已保存）/ `compared`（完成对比）/
    /// `skipped_no_data`（会话无任何采集数据，跳过）/
    /// `no_baseline`（对比时基线文件不存在）/ `failed`（保存/加载/写盘失败，`error` 带原因）
    pub action: String,
    /// 基线文件绝对路径（saved/compared/no_baseline 时给出）
    pub baseline_file: Option<String>,
    /// 对比报告文件绝对路径（compared 且写盘成功时给出）
    pub report_file: Option<String>,
    /// 结构化对比结论（仅 action 为 `compared` 时存在）
    pub outcome: Option<CompareOutcome>,
    /// 失败原因（仅 action 为 `failed` 时给出）
    pub error: Option<String>,
}

impl BaselineVerdict {
    /// 构造只有 action 的结论（其余字段为 None）
    pub fn action_only(action: &str) -> Self {
        BaselineVerdict {
            action: action.to_string(),
            baseline_file: None,
            report_file: None,
            outcome: None,
            error: None,
        }
    }
}

/// 写入 `<dir>/summary.json`（pretty JSON；父目录自动创建），返回文件路径
pub fn write_summary_json(dir: &Path, file: &SummaryFile) -> Result<PathBuf> {
    let path = dir.join("summary.json");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建会话目录失败: {}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(file).context("summary.json 序列化失败")?;
    std::fs::write(&path, text)
        .with_context(|| format!("summary.json 写入失败: {}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use xperf_core::baseline::{save_to, SummaryBuilder};

    fn sample_summary() -> SessionSummary {
        let mut b = SummaryBuilder::new("com.test.app", 1000, 10.0);
        b.push_cpu(10.0);
        b.push_cpu(20.0);
        b.push_mem(300_000.0);
        b.finish()
    }

    /// schema 一致性：summary.json 顶层必须包含基线 JSON 的全部字段且取值相同
    ///（两文件同会话口径相同的验收锚点）
    #[test]
    fn test_summary_json_superset_of_baseline_schema() {
        let dir = std::env::temp_dir().join(format!("xperf_summary_test_{}", std::process::id()));
        let s = sample_summary();
        let baseline_path = dir.join("baseline.json");
        save_to(&baseline_path, &s).unwrap();
        let baseline_json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&baseline_path).unwrap()).unwrap();

        let file = SummaryFile { summary: s, thresholds: None, baseline: None };
        let summary_json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&file).unwrap()).unwrap();

        let base_obj = baseline_json.as_object().unwrap();
        let sum_obj = summary_json.as_object().unwrap();
        for (k, v) in base_obj {
            assert_eq!(sum_obj.get(k), Some(v), "summary.json 字段 {:?} 与基线 JSON 不一致", k);
        }
        // 附加判定字段存在（未启用对应功能时为 null）
        assert!(sum_obj.contains_key("thresholds"));
        assert!(sum_obj.contains_key("baseline"));
        assert!(sum_obj["thresholds"].is_null());
        assert!(sum_obj["baseline"].is_null());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_write_summary_json_roundtrip() {
        let dir = std::env::temp_dir().join(format!("xperf_summary_write_{}", std::process::id()));
        let file = SummaryFile {
            summary: sample_summary(),
            thresholds: Some(ThresholdOutcome { all_pass: true, rules: vec![] }),
            baseline: Some(BaselineVerdict::action_only("saved")),
        };
        let path = write_summary_json(&dir, &file).unwrap();
        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed["package"], "com.test.app");
        assert_eq!(parsed["cpu"]["count"], 2);
        assert_eq!(parsed["thresholds"]["all_pass"], true);
        assert_eq!(parsed["baseline"]["action"], "saved");
        assert!(parsed["baseline"]["outcome"].is_null());
        assert!(parsed["jank_total"].is_null()); // 无 FPS 样本时 jank 为 null
        std::fs::remove_dir_all(&dir).ok();
    }
}
