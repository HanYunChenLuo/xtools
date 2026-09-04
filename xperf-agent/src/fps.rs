//! FPS 采样（SurfaceFlinger 图层帧时间戳，设备端本地 dumpsys）。
//! 与 xperf-core/fps.rs 同源的解析逻辑（agent 零依赖独立发布，有意复制而非共享）。
//! 设备端用 Command 直接 exec dumpsys，无 adb shell 拼接，图层名无需引号。
//!
//! 为什么不用 gfxinfo：`dumpsys gfxinfo framestats` 只统计 View 层级（HWUI）绘制的帧；
//! 游戏/相机/SurfaceView 直渲染应用的帧不上 gfxinfo。所有 buffer 最终都经
//! SurfaceFlinger 合成，因此对**图层**取帧时间戳是通用方案。

use crate::{dumpsys, emit, json_escape};
use std::time::Instant;

/// 连续零帧达到该采样轮数后重新发现图层（Surface 重建会换名，如 #0 → #1）。
/// 计的是 FPS 采样轮（限频后每轮 ≥500ms，即 ≥5s 持续零帧才重发现）。
const FPS_REDISCOVER_ZERO_ROUNDS: u32 = 10;

/// FPS 采样限频：每多少主循环轮采一次，保证 FPS 有效周期 ≥500ms。
/// dumpsys SurfaceFlinger 单轮开销 ~100ms 级，低间隔下每轮执行会拖垮节拍。
pub(crate) fn fps_every_n_rounds(interval_ms: u64) -> u64 {
    500u64.div_ceil(interval_ms).max(1)
}

struct FpsLayerState {
    name: String,
    /// 上轮时刻 + 上轮缓冲末尾时间戳；None = 未建基线
    last: Option<(Instant, Option<u64>)>,
}

#[derive(Default)]
pub(crate) struct FpsState {
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

/// --list 解析：保留包名匹配的行，去 `<hex> ` 别名前缀，去重。
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

impl FpsState {
    /// 某 PID 一轮 FPS 采样：有帧图层各发一行；全零时发一条零帧行（界面静止是真实状态）。
    /// 图层发现：首轮 + 连续零帧达阈值后重做。包名用于兜底匹配（ownerPID 是首选）。
    /// 发现为空（进程刚重启 Surface 未建，或应用本无界面）也记零帧轮，
    /// 靠阈值节流重试——全量 dump 在此车机 ~1.5s，不能每轮试。
    pub(crate) fn sample_round(&mut self, pid: u32, package: &str, ts: u64) {
        if !self.attempted || self.zero_rounds >= FPS_REDISCOVER_ZERO_ROUNDS {
            self.layers = sf_discover_layers(pid, package)
                .into_iter()
                .map(|name| FpsLayerState { name, last: None })
                .collect();
            self.attempted = true;
            self.zero_rounds = 0;
        }
        if self.layers.is_empty() {
            self.zero_rounds += 1;
            return;
        }

        let now = Instant::now();
        let mut samples: Vec<(String, f32, u32, u32)> = Vec::new();
        for layer in &mut self.layers {
            let presents = dumpsys(&["SurfaceFlinger", "--latency", &layer.name])
                .map(|s| parse_latency_output(&s))
                .unwrap_or_default();
            if presents.is_empty() {
                match layer.last {
                    // 已有基线但本轮读空（dumpsys 失败/缓冲被清）：记零帧样本（zero_rounds
                    // 记账与重发现照常）但**保留基线不动**——否则基线被 None 覆盖后，
                    // 下轮会把整个 127 帧缓冲误计为新帧，FPS 瞬时虚高数倍。
                    Some((_, Some(_))) => {
                        samples.push((layer.name.clone(), 0.0, 0, 0));
                    }
                    // 无基线（新图层）：建基线
                    _ => layer.last = Some((now, None)),
                }
                continue;
            }
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
                // 上轮基线为 None（新图层首轮缓冲为空）：不把整个 127 帧历史计入，
                // 只计最近 elapsed 时长内的帧（presents 是单调纳秒时间戳，
                // 缓冲中最新帧 ≈ now，因此下界 = latest - elapsed）
                None => {
                    let elapsed_ns = (elapsed * 1e9) as u64;
                    let cutoff = latest.unwrap_or(0).saturating_sub(elapsed_ns);
                    presents.into_iter().filter(|&t| t > cutoff).collect()
                }
            };
            let fps = new_frames.len() as f32 / elapsed;
            let jank = count_jank(last_p, &new_frames);
            samples.push((layer.name.clone(), fps, new_frames.len() as u32, jank));
        }
        if samples.is_empty() {
            return;
        }

        if samples.iter().any(|s| s.2 > 0) {
            self.zero_rounds = 0;
            samples.retain(|s| s.2 > 0); // 静止图层是噪声，不上报
        } else {
            self.zero_rounds += 1;
            samples.truncate(1); // 全零：一条静止样本即可
        }
        for (layer, fps, frames, jank) in samples {
            emit(&format!(
                "{{\"t\":\"fps\",\"ts\":{},\"pid\":{},\"layer\":\"{}\",\"fps\":{:.2},\"frames\":{},\"jank\":{}}}",
                ts, pid, json_escape(&layer), fps, frames, jank
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fps_every_n_rounds() {
        // FPS 有效周期 ≥500ms：低间隔限频，≥500ms 每轮都采
        assert_eq!(fps_every_n_rounds(50), 10); // 50ms → 每 10 轮（500ms）
        assert_eq!(fps_every_n_rounds(300), 2); // 300ms → 每 2 轮（600ms）
        assert_eq!(fps_every_n_rounds(500), 1);
        assert_eq!(fps_every_n_rounds(1000), 1);
    }

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
}
