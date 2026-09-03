//! 阈值告警引擎：解析阈值规则、实时检测超阈值、退出时输出验证报告。
//!
//! 规则格式：metric op value，如 `cpu>80`、`mem>500`、`fps<30`、`gpu>90`
//! metric: cpu(CPU%)、mem(内存PSS MB)、fps(FPS)、gpu(GPU busy%)
//! op: > 或 <
//! value: 数值

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
    /// 规则 → 最大观测值
    pub max_observed: HashMap<String, f32>,
}

impl AlertStats {
    pub fn record(&mut self, rule: &str, value: f32, time: &str) {
        let e = self.triggers.entry(rule.into()).or_default();
        e.0 += 1;
        e.1 = time.into();
        let m = self.max_observed.entry(rule.into()).or_insert(f32::MIN);
        if value > *m { *m = value; }
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

/// 生成退出验证报告
pub fn generate_report(thresholds: &[Threshold], stats: &AlertStats) -> String {
    if thresholds.is_empty() {
        return String::new();
    }
    let mut lines = vec!["========== 验证报告 ==========".to_string()];
    let mut all_pass = true;
    for t in thresholds {
        let triggers = stats.triggers.get(&t.raw).map(|(n, _)| *n).unwrap_or(0);
        let max_val = stats.max_observed.get(&t.raw).copied().unwrap_or(0.0);
        let last_time = stats.triggers.get(&t.raw).map(|(_, t)| t.as_str()).unwrap_or("-");
        let pass = triggers == 0;
        if !pass { all_pass = false; }
        lines.push(format!(
            "  {} {} {} → {} (触发 {} 次, 峰值 {:.1}, 最近 {})",
            t.metric, t.op, t.value,
            if pass { "✅ 达标" } else { "❌ 超标" },
            triggers, max_val, last_time
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
        stats.record("cpu>80", 85.0, "12:00:00");
        let report = generate_report(&t, &stats);
        assert!(report.contains("❌ 超标"));
        assert!(report.contains("触发 1 次"));
        assert!(report.contains("峰值 85.0"));
    }
}
