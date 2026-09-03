//! 内存采样：低间隔走 /proc/<pid>/smaps_rollup（Pss/Rss，~1ms），
//! interval ≥500ms 用本地 dumpsys meminfo（App Summary 全分类明细）。
//! 低间隔下 dumpsys meminfo 太重（~100ms），退化到 smaps_rollup（只有 Pss/Rss）。

use crate::{dumpsys, emit};
use std::fs;

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

/// 一轮内存采样并 emit。full=true：dumpsys meminfo 全分类 + smaps_rollup 补 Rss；
/// false：只读 smaps_rollup（分类字段全 0）。
pub(crate) fn sample_memory(pid: u32, ts: u64, full: bool) {
    if full {
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

#[cfg(test)]
mod tests {
    use super::*;

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
