//! 内存采样：低间隔优先 `/proc/<pid>/smaps_rollup`（Pss/Rss，~1ms），
//! interval ≥500ms 用本地 dumpsys meminfo（App Summary 全分类明细）。
//! 低间隔下 dumpsys meminfo 太重（~100ms），故优先 smaps_rollup（只有 Pss/Rss）。
//! **非 root 降级**：smaps_rollup 受 PTRACE 限制 shell 不可读（两平台实测），
//! 此时降级为 dumpsys meminfo（shell 可用）并由主循环限频 ≥500ms（[decide_mode]）。

use crate::{dumpsys, emit};
use std::fs;

/// 读 `/proc/<pid>/smaps_rollup`：返回 (Pss KB, Rss KB)
fn read_smaps_rollup(pid: u32) -> Option<(u64, u64)> {
    let content = fs::read_to_string(format!("/proc/{}/smaps_rollup", pid)).ok()?;
    parse_smaps_rollup(&content)
}

/// 聚合 `/proc/<pid>/smaps` 中 dmabuf 映射（VMA 名 `/dmabuf…` 或 `[anon:dmabuf…`，
/// gralloc/dma-heap 分配的图形/媒体缓冲 CPU mmap）的 Pss（KB）。
/// App Summary 把这些落进 Private Other 兜底桶（名字不匹配 Graphics 桶的设备节点
/// 模式——Graphics 只认 kgsl/drm 等节点映射），直渲染应用的大头因此"无指向"；
/// root 下读 smaps 可按 VMA 名拆出单列。不可读（非 root，PTRACE 限制）返回 None。
fn read_dmabuf_pss(pid: u32) -> Option<u64> {
    let content = fs::read_to_string(format!("/proc/{}/smaps", pid)).ok()?;
    Some(parse_dmabuf_pss(&content))
}

/// smaps 文本中 dmabuf VMA 的 Pss 合计（KB）。头行按字段解析（第 6 字段起为路径名，
/// 列宽随地址/inode 位数浮动，不按固定列切片）；路径名可含空格，dmabuf 名无空格，
/// 取首 token 前缀匹配。
fn parse_dmabuf_pss(smaps: &str) -> u64 {
    let mut total = 0u64;
    let mut in_dmabuf = false;
    for line in smaps.lines() {
        // mapping 头行："<addr>-<addr> <perms> <offset> <dev> <inode> [name…]"
        // 以十六进制数字开头且首 token 含 '-' 判定（指标行都是字母开头）
        if line.starts_with(|c: char| c.is_ascii_hexdigit()) {
            let mut fields = line.split_whitespace();
            let addr = fields.next().unwrap_or("");
            in_dmabuf = addr.contains('-')
                // perms / offset / dev / inode 四字段必须齐全才是头行
                && fields.next().is_some()
                && fields.next().is_some()
                && fields.next().is_some()
                && fields.next().is_some()
                && fields.next().is_some_and(|name| {
                    name.starts_with("/dmabuf") || name.starts_with("[anon:dmabuf")
                });
        } else if in_dmabuf {
            if let Some(v) = line.strip_prefix("Pss:") {
                total += v.split_whitespace().next().and_then(|s| s.parse().ok()).unwrap_or(0);
            }
        }
    }
    total
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
    /// TOTAL RSS（smaps_rollup 不可读时的 Rss 兜底；0 = dumpsys 输出未携带）
    rss: u64,
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
        // 兜底：TOTAL PSS 在 App Summary 空行之后（区块外）；同行还有 TOTAL RSS
        // （smaps_rollup 不可读时的 Rss 兜底数据源）
        if !in_summary {
            if let Some(rest) = line.strip_prefix("TOTAL PSS:") {
                if let Some(Ok(kb)) = rest.split_whitespace().next().map(|s| s.parse::<u64>()) {
                    bd.total = kb;
                }
                if let Some(rpos) = rest.find("TOTAL RSS:") {
                    let after = &rest[rpos + "TOTAL RSS:".len()..];
                    if let Some(Ok(kb)) = after.split_whitespace().next().map(|s| s.parse::<u64>()) {
                        bd.rss = kb;
                    }
                }
            }
        }
    }
    (bd.total > 0).then_some(bd)
}

/// 内存采样路径（会话内首个被采 PID 出现时由 [decide_mode] 决定一次）
pub(crate) enum MemMode {
    /// interval ≥500ms：dumpsys meminfo 全分类明细（shell 可用，无 root 依赖）
    Full,
    /// 低间隔快路：只读 smaps_rollup（Pss/Rss，~1ms；分类字段全 0）
    Smaps,
    /// 低间隔 + 非 root（smaps_rollup 不可读）降级：dumpsys meminfo，主循环限频 ≥500ms
    DumpsysFallback,
}

/// 决定内存采样路径（会话内决定一次，需一个活 PID 探测 smaps_rollup 可读性）。
/// 返回 (模式, 降级提示)——仅降级路径带提示（主循环发 err 行告知 host/用户）。
pub(crate) fn decide_mode(pid: u32, interval_ms: u64) -> (MemMode, Option<String>) {
    if interval_ms >= 500 {
        return (MemMode::Full, None);
    }
    if read_smaps_rollup(pid).is_some() {
        (MemMode::Smaps, None)
    } else {
        (
            MemMode::DumpsysFallback,
            Some(format!(
                "pid {} 的 smaps_rollup 不可读（需 root），内存降级为 dumpsys meminfo（有效周期 ≥500ms）",
                pid
            )),
        )
    }
}

/// 一轮内存采样并 emit。
/// - Full / DumpsysFallback：dumpsys meminfo 全分类；Rss 优先 smaps_rollup（~1ms
///   精确），不可读（非 root）时用 App Summary 的 TOTAL RSS 兜底
/// - Full 模式（≥500ms 节拍）额外拆 DMA-BUF：root 下扫全量 smaps 按 VMA 名聚合
///   Pss（~3MB 文本 <50ms，叠加 dumpsys ~100ms 可承受），从 Private Other 兜底桶
///   扣减单列（8 分类合计仍恒等 PSS）；smaps 不可读（非 root）dmabuf=0 不破坏原 7 类
/// - Smaps：只读 smaps_rollup（分类字段全 0，dmabuf=0——低间隔路径不接全量 smaps）
pub(crate) fn sample_memory(pid: u32, ts: u64, mode: &MemMode) {
    match mode {
        MemMode::Full | MemMode::DumpsysFallback => {
            if let Some(bd) = dumpsys(&["meminfo", &pid.to_string()])
                .and_then(|s| parse_meminfo_summary(&s))
            {
                // rss 不在 App Summary 分类表里（同块 TOTAL 行另有 TOTAL RSS）
                let rss = read_smaps_rollup(pid).map(|(_, r)| r).unwrap_or(bd.rss);
                // DumpsysFallback 即 smaps 不可读路径，跳过全量 smaps 读取
                let dmabuf = match mode {
                    MemMode::Full => read_dmabuf_pss(pid).unwrap_or(0),
                    _ => 0,
                };
                let other = bd.other.saturating_sub(dmabuf);
                emit(&format!(
                    "{{\"t\":\"mem\",\"ts\":{},\"pid\":{},\"pss\":{},\"rss\":{},\"java\":{},\"native\":{},\"code\":{},\"stack\":{},\"gfx\":{},\"other\":{},\"sys\":{},\"dmabuf\":{}}}",
                    ts, pid, bd.total, rss, bd.java, bd.native_, bd.code, bd.stack, bd.gfx, other, bd.sys, dmabuf
                ));
            }
        }
        MemMode::Smaps => {
            if let Some((pss, rss)) = read_smaps_rollup(pid) {
                emit(&format!(
                    "{{\"t\":\"mem\",\"ts\":{},\"pid\":{},\"pss\":{},\"rss\":{},\"java\":0,\"native\":0,\"code\":0,\"stack\":0,\"gfx\":0,\"other\":0,\"sys\":0,\"dmabuf\":0}}",
                    ts, pid, pss, rss
                ));
            }
        }
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
        // TOTAL RSS 与 TOTAL PSS 同行（smaps_rollup 不可读时的 Rss 兜底）
        assert_eq!(bd.rss, 639384);
    }

    #[test]
    fn test_parse_meminfo_summary_no_total() {
        assert!(parse_meminfo_summary("garbage\n").is_none());
    }

    #[test]
    fn test_parse_dmabuf_pss() {
        // 真机片段格式（SS4 gltf）：/dmabuf: 与 [anon:dmabuf 两种前缀，
        // 非 dmabuf VMA（含名字带空格的）不计；Pss 逐 VMA 累计
        let smaps = "7c41a00000-7c41b00000 rw-p 00000000 00:05 1234       /dmabuf:4567\n\
                     Size:                64 kB\n\
                     Rss:                 64 kB\n\
                     Pss:                 32 kB\n\
                     Anonymous:            0 kB\n\
                     7c41b00000-7c43b00000 rw-p 00000000 00:00 0          [anon:dmabuf:hal_buffer]\n\
                     Size:               2048 kB\n\
                     Pss:               1024 kB\n\
                     55f7000000-55f71a9000 rw-p 00000000 00:00 0          [anon:dalvik-main space (startup)]\n\
                     Pss:              31012 kB\n\
                     7c41c00000-7c41d00000 rw-p 00000000 00:00 0\n\
                     Pss:                 16 kB\n";
        assert_eq!(parse_dmabuf_pss(smaps), 32 + 1024);
    }

    #[test]
    fn test_parse_dmabuf_pss_none() {
        let smaps = "55f7000000-55f71a9000 rw-p 00000000 00:00 0          [anon:dalvik-main space]\n\
                     Pss:              31012 kB\n";
        assert_eq!(parse_dmabuf_pss(smaps), 0);
        assert_eq!(parse_dmabuf_pss(""), 0);
    }
}
