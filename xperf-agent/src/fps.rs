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
    /// 干净图层名（事件 layer 字段/CSV 图例用）
    name: String,
    /// `--latency` 查询名。Android 16（SS4）的 QCM SF 构建要求传 `--list` 原始行的
    /// `<hex> <name>` 别名形态——只传干净名恒空（实测：带前缀 65 行真帧数据 vs
    /// 不带 1 行刷新周期；2026-09-10 经 getfps 逆向 + A/B 直测锁定）。旧平台为
    /// 干净名（历史行为，SS2MAX/SS3 真机验证，勿改）。
    query: String,
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

/// `--list` 解析：保留包名匹配的行，返回 (干净名, --latency 查询名)，按干净名去重。
/// Android 16 起 `--list` 行带 `RequestedLayerState{<hex> <name> parentId=… …}` 包装
/// （SS4/A16 实测），须拆壳取名字段；且该构建的 `--latency` 只认 `<hex> <name>`
/// 别名形态（见 [`FpsLayerState::query`]），查询名保留前缀。旧格式平台查询名 =
/// 干净名（行为不变）。
fn parse_list_layers(list: &str, package: &str) -> Vec<(String, String)> {
    let mut layers: Vec<(String, String)> = Vec::new();
    for line in list.lines() {
        let line = line.trim();
        if !line.contains(package) {
            continue;
        }
        // A16 包装拆壳：RequestedLayerState{…} → 壳内内容
        let (wrapped, s) = match line.strip_prefix("RequestedLayerState{") {
            Some(rest) => (true, rest),
            None => (false, line),
        };
        // 拆 `<hex> ` 别名前缀（旧格式与 A16 壳内均有）
        let (hex, name) = match s.split_once(' ') {
            Some((head, rest)) if !head.is_empty() && head.chars().all(|c| c.is_ascii_hexdigit()) => {
                (Some(head), rest.trim())
            }
            _ => (None, s),
        };
        // A16 壳内名字段后接 parentId=/relativeParentId=/z= 元数据与结尾 `}` → 截断
        let name = if wrapped {
            let end = [" parentId=", " relativeParentId=", " z=", "}"]
                .iter()
                .filter_map(|m| name.find(m))
                .min()
                .unwrap_or(name.len());
            name[..end].trim_end()
        } else {
            name
        };
        // A16 查询名带别名前缀；旧格式保持干净名
        let query = match (wrapped, hex) {
            (true, Some(h)) => format!("{h} {name}"),
            _ => name.to_string(),
        };
        if !layers.iter().any(|(n, _)| n == name) {
            layers.push((name.to_string(), query));
        }
    }
    layers
}

/// 图层发现：ownerPID 归属匹配 ∪ `--list` 包名匹配（并集去重）。
///
/// 不能 ownerPID 非空就跳过兜底：SS3 实测 ownerPID 只拿到 Activity View 层
/// （静止无新帧），真正的渲染层 `SurfaceView[...](BLAST)#0`（Android 12+
/// BLAST 合成，app 直提 buffer）漏掉 → FPS 恒 0；包名匹配能把 BLAST 层补进。
/// 无 buffer 的辅助层（Background for/Bounds for/ActivityRecord…）并入无害：
/// 空缓冲层不建帧基线、不发事件（sample_round 的 None 基线路径）。
fn sf_discover_layers(pid: u32, package: &str) -> Vec<(String, String)> {
    // 全量 dump 路径的层先入列（查询名兜底为干净名）；--list 同名层会覆盖其查询名
    // （借 A16 的 `<hex> <name>` 形态——全量 dump 块里拿不到别名前缀）
    let mut layers: Vec<(String, String)> = dumpsys(&["SurfaceFlinger"])
        .map(|s| parse_owned_buffer_layers(&s, pid))
        .unwrap_or_default()
        .into_iter()
        .map(|name| (name.clone(), name))
        .collect();
    let by_name = dumpsys(&["SurfaceFlinger", "--list"])
        .map(|s| parse_list_layers(&s, package))
        .unwrap_or_default();
    for (name, query) in by_name {
        if let Some(slot) = layers.iter_mut().find(|(n, _)| *n == name) {
            slot.1 = query;
        } else {
            layers.push((name, query));
        }
    }
    layers
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
                .map(|(name, query)| FpsLayerState { name, query, last: None })
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
            let presents = dumpsys(&["SurfaceFlinger", "--latency", &layer.query])
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
        // 旧格式：查询名 = 干净名（历史行为不变）
        let list = "147955a com.pkg/com.pkg.MainActivity#0\ncom.pkg/com.pkg.MainActivity#0\ncom.other/Main#0\n";
        assert_eq!(
            parse_list_layers(list, "com.pkg"),
            vec![("com.pkg/com.pkg.MainActivity#0".to_string(), "com.pkg/com.pkg.MainActivity#0".to_string())]
        );
    }

    #[test]
    fn test_parse_list_layers_android16_requested_layer_state() {
        // SS4（Android 16）真机 --list 形态：RequestedLayerState{<hex> <name> parentId=… […]}
        // 查询名保留 `<hex> ` 别名前缀（A16 SF 的 --latency 只认该形态）；
        // 壳内无 hex 的行查询名退化为干净名
        let list = "RequestedLayerState{92cc982 SurfaceView[com.pkg/com.pkg.Main](BLAST)#367 parentId=366}\n\
                    RequestedLayerState{710a43c SurfaceView[com.other/Act](BLAST)#276 parentId=275}\n\
                    RequestedLayerState{com.pkg/com.pkg.Main#362}\n\
                    RequestedLayerState{9ff Bounds for - com.pkg/com.pkg.Main#365 parentId=362 z=-2}\n";
        assert_eq!(
            parse_list_layers(list, "com.pkg"),
            vec![
                (
                    "SurfaceView[com.pkg/com.pkg.Main](BLAST)#367".to_string(),
                    "92cc982 SurfaceView[com.pkg/com.pkg.Main](BLAST)#367".to_string()
                ),
                ("com.pkg/com.pkg.Main#362".to_string(), "com.pkg/com.pkg.Main#362".to_string()),
                (
                    "Bounds for - com.pkg/com.pkg.Main#365".to_string(),
                    "9ff Bounds for - com.pkg/com.pkg.Main#365".to_string()
                ),
            ]
        );
    }
}
