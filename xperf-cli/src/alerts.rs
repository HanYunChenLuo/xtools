//! 阈值告警引擎：解析阈值规则、实时检测超阈值、退出时输出验证报告。
//!
//! 规则格式：metric op value，如 `cpu>80`、`mem>500`、`fps<30`、`gpu>90`
//! metric: cpu(CPU%)、mem(内存PSS MB)、fps(FPS)、gpu(GPU busy%)
//! op: > 或 <
//! value: 数值

use serde::Serialize;
use std::collections::HashMap;

/// 一条阈值规则
#[derive(Debug, Clone)]
pub struct Threshold {
    pub metric: String,
    pub op: char, // '>' or '<'
    pub value: f32,
    pub raw: String,
}

/// 告警统计（退出报告用）
#[derive(Default)]
pub struct AlertStats {
    /// 规则 → (触发次数, 最近一次触发时间)
    pub triggers: HashMap<String, (u32, String)>,
    /// 规则 → 极值（> 规则记最大观测值，< 规则记最小观测值）
    pub extremes: HashMap<String, f32>,
    /// 规则 → 操作符（用于决定极值方向）
    pub ops: HashMap<String, char>,
}

impl AlertStats {
    pub fn record(&mut self, rule: &str, value: f32, time: &str, op: char) {
        let e = self.triggers.entry(rule.into()).or_default();
        e.0 += 1;
        e.1 = time.into();
        self.ops.insert(rule.into(), op);
        let m = self.extremes.entry(rule.into()).or_insert(match op {
            '>' => f32::MIN,
            '<' => f32::MAX,
            _ => 0.0,
        });
        match op {
            '>' => { if value > *m { *m = value; } }
            '<' => { if value < *m { *m = value; } }
            _ => {}
        }
    }
}

/// 解析阈值规则列表
pub fn parse_thresholds(rules: &[String]) -> Vec<Threshold> {
    let mut out = Vec::new();
    for r in rules {
        let r = r.trim();
        if r.is_empty() { continue; }
        if let Some((metric, rest)) = r.split_once('>') {
            if let Ok(v) = rest.trim().parse::<f32>() {
                out.push(Threshold { metric: metric.trim().to_string(), op: '>', value: v, raw: r.into() });
                continue;
            }
        }
        if let Some((metric, rest)) = r.split_once('<') {
            if let Ok(v) = rest.trim().parse::<f32>() {
                out.push(Threshold { metric: metric.trim().to_string(), op: '<', value: v, raw: r.into() });
                continue;
            }
        }
        eprintln!("警告: 无法解析阈值规则 '{}', 跳过", r);
    }
    out
}

/// 检测单次观测值是否超阈值，返回触发的规则。
/// `is_active`: 该指标是否处于"活跃"状态（如 FPS 静止界面 frames=0 时不触发低 FPS 告警）
pub fn check_value<'a>(thresholds: &'a [Threshold], metric: &str, value: f32, is_active: bool) -> Vec<&'a Threshold> {
    thresholds.iter().filter(|t| {
        if t.metric != metric { return false; }
        // 静止界面（FPS=0 无帧）不触发低值告警（如 fps<30）
        if !is_active && t.op == '<' { return false; }
        match t.op {
            '>' => value > t.value,
            '<' => value < t.value,
            _ => false,
        }
    }).collect()
}

/// 单条阈值规则的判定结果（`summary.json` 结构化结论用，与文本报告同口径）
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RuleOutcome {
    /// 规则原文（如 `cpu>80`）
    pub rule: String,
    /// 是否达标（触发次数为 0）
    pub pass: bool,
    /// 触发次数
    pub triggers: u32,
    /// 极值（`>` 规则为观测峰值，`<` 规则为观测谷值；从未触发时为 0）
    pub extreme: f32,
    /// 最近一次触发时刻（`HH:MM:SS`；从未触发为 None）
    pub last_trigger: Option<String>,
}

/// 阈值验证的结构化结论（`summary.json` 用，与 [`generate_report`] 文本报告同一份统计）
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ThresholdOutcome {
    /// 全部规则达标（所有规则触发次数均为 0）
    pub all_pass: bool,
    /// 逐规则判定结果（顺序与命令行给定顺序一致）
    pub rules: Vec<RuleOutcome>,
}

/// 由告警统计构建结构化结论（未给 `--threshold` 时调用方不应调用）
pub fn report_outcome(thresholds: &[Threshold], stats: &AlertStats) -> ThresholdOutcome {
    let mut all_pass = true;
    let mut rules = Vec::with_capacity(thresholds.len());
    for t in thresholds {
        let (triggers, last) = stats
            .triggers
            .get(&t.raw)
            .map(|(n, last)| (*n, Some(last.clone())))
            .unwrap_or((0, None));
        let extreme = stats.extremes.get(&t.raw).copied().unwrap_or(0.0);
        let pass = triggers == 0;
        if !pass {
            all_pass = false;
        }
        rules.push(RuleOutcome {
            rule: t.raw.clone(),
            pass,
            triggers,
            extreme,
            last_trigger: last,
        });
    }
    ThresholdOutcome { all_pass, rules }
}

/// 生成退出验证报告
pub fn generate_report(thresholds: &[Threshold], stats: &AlertStats) -> String {
    if thresholds.is_empty() {
        return String::new();
    }
    let mut lines = vec!["========== 验证报告 ==========".to_string()];
    let mut all_pass = true;
    for t in thresholds {
        let triggers = stats.triggers.get(&t.raw).map(|(n, _)| *n).unwrap_or(0);
        let extreme = stats.extremes.get(&t.raw).copied().unwrap_or(0.0);
        let last_time = stats.triggers.get(&t.raw).map(|(_, t)| t.as_str()).unwrap_or("-");
        let pass = triggers == 0;
        if !pass { all_pass = false; }
        let extreme_label = match t.op { '>' => "峰值", '<' => "谷值", _ => "极值" };
        lines.push(format!(
            "  {} {} {} → {} (触发 {} 次, {} {:.1}, 最近 {})",
            t.metric, t.op, t.value,
            if pass { "✅ 达标" } else { "❌ 超标" },
            triggers, extreme_label, extreme, last_time
        ));
    }
    lines.push(format!("  总结论: {}", if all_pass { "✅ 全部达标" } else { "❌ 存在超标" }));
    lines.push("==============================".to_string());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_thresholds() {
        let rules = vec!["cpu>80".into(), "fps<30".into(), "bad".into()];
        let t = parse_thresholds(&rules);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].metric, "cpu");
        assert_eq!(t[0].op, '>');
        assert_eq!(t[0].value, 80.0);
        assert_eq!(t[1].metric, "fps");
        assert_eq!(t[1].op, '<');
    }

    #[test]
    fn test_check_value() {
        let t = parse_thresholds(&["cpu>80".into(), "cpu<10".into()]);
        assert_eq!(check_value(&t, "cpu", 85.0, true).len(), 1); // >80 触发
        assert_eq!(check_value(&t, "cpu", 50.0, true).len(), 0); // 不触发
        assert_eq!(check_value(&t, "cpu", 5.0, true).len(), 1);  // <10 触发
        // 静止时低值告警不触发（如 fps<30 在 fps=0 静止界面）
        let fps_t = parse_thresholds(&["fps<30".into()]);
        assert_eq!(check_value(&fps_t, "fps", 0.0, false).len(), 0); // 静止不触发
        assert_eq!(check_value(&fps_t, "fps", 20.0, true).len(), 1); // 活跃低帧触发
    }

    #[test]
    fn test_report_all_pass() {
        let t = parse_thresholds(&["cpu>80".into()]);
        let stats = AlertStats::default();
        let report = generate_report(&t, &stats);
        assert!(report.contains("✅ 全部达标"));
    }

    #[test]
    fn test_report_fail() {
        let t = parse_thresholds(&["cpu>80".into()]);
        let mut stats = AlertStats::default();
        stats.record("cpu>80", 85.0, "12:00:00", '>');
        let report = generate_report(&t, &stats);
        assert!(report.contains("❌ 超标"));
        assert!(report.contains("触发 1 次"));
        assert!(report.contains("峰值 85.0"));
    }

    #[test]
    fn test_report_less_than() {
        let t = parse_thresholds(&["fps<30".into()]);
        let mut stats = AlertStats::default();
        stats.record("fps<30", 15.0, "12:00:00", '<');
        let report = generate_report(&t, &stats);
        assert!(report.contains("❌ 超标"));
        assert!(report.contains("谷值 15.0"));
    }

    #[test]
    fn test_report_outcome_structure() {
        // 与文本报告同口径：触发>0 的规则不达标，all_pass 为且仅为全部达标
        let t = parse_thresholds(&["cpu>80".into(), "fps<30".into()]);
        let mut stats = AlertStats::default();
        stats.record("cpu>80", 95.5, "12:00:03", '>');
        let o = report_outcome(&t, &stats);
        assert!(!o.all_pass);
        assert_eq!(o.rules.len(), 2);
        assert_eq!(o.rules[0].rule, "cpu>80");
        assert!(!o.rules[0].pass);
        assert_eq!(o.rules[0].triggers, 1);
        assert_eq!(o.rules[0].extreme, 95.5);
        assert_eq!(o.rules[0].last_trigger.as_deref(), Some("12:00:03"));
        assert!(o.rules[1].pass); // 未触发
        assert_eq!(o.rules[1].extreme, 0.0);
        assert_eq!(o.rules[1].last_trigger, None);
        // 文本报告与结构化结论同判定
        assert!(generate_report(&t, &stats).contains("❌ 存在超标"));

        let o = report_outcome(&t, &AlertStats::default());
        assert!(o.all_pass);
        assert!(o.rules.iter().all(|r| r.pass));
    }
}
