//! FPS 采样：基于 SurfaceFlinger 图层帧时间戳统计。
//!
//! 为什么不用 gfxinfo：`dumpsys gfxinfo framestats` 只统计 View 层级（HWUI/RenderThread）
//! 绘制的帧；游戏、视频、全景影像等直接向 Surface 渲染（SurfaceView/GLSurfaceView/
//! NativeActivity/Vulkan）的应用，gfxinfo 拿不到数据。所有 buffer 无论走哪条路径，
//! 最终都经 SurfaceFlinger 合成上屏，因此对图层取帧时间戳可得到真实帧率。
//!
//! 采样方式：`dumpsys SurfaceFlinger --latency <layer>` 返回该图层最近 127 帧的
//! 上屏时间戳（actualPresent），逐轮取差值得到本窗口新帧数。
//! 注意不用 `--latency-clear` 的读数做采样：实测部分设备（如此车机）clear 只清空
//! 缓冲而不返回数据。缓冲区 127 帧 ≈ 2.1s@60fps / 1.05s@120fps，采样间隔大于该
//! 时长时老帧被挤出，计数是下界（ interval ≤ 1s 时精确）。

use crate::utils;
use anyhow::Result;
use chrono::{DateTime, Local};
use std::time::Instant;

/// 连续零帧达到该轮数后重新发现图层（图层可能因 Surface 重建而换名，如 #0 → #1）
const REDISCOVER_AFTER_ZERO_ROUNDS: u32 = 10;

/// 一个图层的采样状态
pub struct LayerState {
    pub name: String,
    /// 上次采样时刻 + 当时缓冲里最新的上屏时间戳；None = 尚未建立基线
    last_sample: Option<(Instant, Option<u64>)>,
}

/// 一轮 FPS 采样结果（取该 PID 下帧数最多的图层作为应用帧率）
#[derive(Debug, Clone)]
pub struct FpsSample {
    pub timestamp: DateTime<Local>,
    pub layer: String,
    pub fps: f32,
    pub frame_count: u32,
    pub jank_count: u32,
}

/// FPS 时序数据（用于退出时导出 CSV；多图层并存时逐图层各占一行，靠 layer 列区分）
#[derive(Default)]
pub struct FpsTimeSeriesData {
    pub timestamps: Vec<DateTime<Local>>,
    pub fps: Vec<f32>,
    pub jank_counts: Vec<u32>,
    pub layers: Vec<String>,
}

impl FpsTimeSeriesData {
    pub fn add_data_point(&mut self, timestamp: DateTime<Local>, fps: f32, jank_count: u32, layer: &str) {
        self.timestamps.push(timestamp);
        self.fps.push(fps);
        self.jank_counts.push(jank_count);
        self.layers.push(layer.to_string());
    }
}

/// 某 PID 的 FPS 采样状态：图层列表 + 发现/重发现 bookkeeping
pub struct FpsPidState {
    layers: Vec<LayerState>,
    discovery_attempted: bool,
    zero_rounds: u32,
}

impl FpsPidState {
    pub fn new() -> Self {
        Self {
            layers: Vec::new(),
            discovery_attempted: false,
            zero_rounds: 0,
        }
    }

    /// 采样一轮。返回该 PID 各图层的样本：
    /// - 有帧的图层各自一条（多渲染面并存时不做取舍，避免次活跃面被掩盖、
    ///   "最忙图层"逐轮跳动导致时序混叠）
    /// - 全部图层零帧时只返回一条零帧样本（界面静止是真实状态，如实上报）
    /// - 空 Vec 表示首轮建基线，不出数
    pub fn sample(&mut self, pid: &str, package: &str) -> Result<Vec<FpsSample>> {
        if !self.discovery_attempted || (self.zero_rounds >= REDISCOVER_AFTER_ZERO_ROUNDS) {
            let names = discover_layers(pid, package);
            self.layers = names
                .into_iter()
                .map(|name| LayerState { name, last_sample: None })
                .collect();
            self.discovery_attempted = true;
            self.zero_rounds = 0;
            if self.layers.is_empty() {
                anyhow::bail!("未找到 PID {} 的 SurfaceFlinger 图层（应用可能无界面）", pid);
            }
        }

        let mut samples: Vec<FpsSample> = Vec::new();
        for layer in &mut self.layers {
            if let Some(s) = sample_layer(layer)? {
                samples.push(s);
            }
        }
        if samples.is_empty() {
            return Ok(samples); // 首轮：所有图层都在建基线
        }

        if samples.iter().any(|s| s.frame_count > 0) {
            self.zero_rounds = 0;
            samples.retain(|s| s.frame_count > 0); // 静止图层是噪声，不上报
        } else {
            self.zero_rounds += 1;
            samples.truncate(1); // 全零：一条静止样本即可
        }
        Ok(samples)
    }
}

impl Default for FpsPidState {
    fn default() -> Self {
        Self::new()
    }
}

/// 采样单个图层：读帧缓冲，与上次的最新时间戳取差，得本窗口新帧数/FPS/卡顿。
/// 返回 None 表示首轮（只建基线）。
fn sample_layer(layer: &mut LayerState) -> Result<Option<FpsSample>> {
    let now = Instant::now();
    let timestamp = Local::now();
    let output = read_layer_frames(&layer.name)?;
    let presents = parse_latency_output(&output);
    let latest = presents.last().copied();

    let Some((last_instant, last_present)) = layer.last_sample.replace((now, latest)) else {
        return Ok(None); // 首轮：建基线
    };
    let elapsed = (now - last_instant).as_secs_f32();
    if elapsed <= 0.0 {
        return Ok(None);
    }

    // 本窗口新帧：时间戳晚于上次缓冲末尾的帧
    let new_frames: Vec<u64> = match last_present {
        Some(last) => presents.into_iter().filter(|&t| t > last).collect(),
        None => presents, // 上次缓冲为空 → 缓冲内全部是"新"帧
    };
    let frame_count = new_frames.len() as u32;
    let fps = frame_count as f32 / elapsed;
    // 卡顿统计把上次末尾帧作为边界帧，跨窗口的间隔也能覆盖
    let jank_count = count_jank(last_present, &new_frames);
    Ok(Some(FpsSample {
        timestamp,
        layer: layer.name.clone(),
        fps,
        frame_count,
        jank_count,
    }))
}

/// 读图层帧缓冲（--latency，不清空）。
/// 图层名含空格/#（如 'SVM Container#0'），adb 会把 argv 用空格拼接后交给
/// 设备 shell，因此这里必须自带单引号，否则会被拆成多个参数。
fn read_layer_frames(layer: &str) -> Result<String> {
    let quoted = format!("'{}'", layer);
    Ok(utils::run_adb_command(&[
        "shell",
        "dumpsys",
        "SurfaceFlinger",
        "--latency",
        &quoted,
    ])?
    .stdout)
}

/// 解析 --latency 输出：首行是刷新周期（ns，不使用），随后每行三列
/// desiredPresent / actualPresent / frameReady（制表符分隔），取 actualPresent。
/// 过滤两类无效值：0（空槽位）和 i64::MAX（已入队未上屏的哨兵行）。
fn parse_latency_output(output: &str) -> Vec<u64> {
    output
        .lines()
        .skip(1) // 首行是刷新周期
        .filter_map(|line| {
            let mut cols = line.split_whitespace();
            cols.next()?; // desiredPresent
            let actual: u64 = cols.next()?.parse().ok()?;
            (actual > 0 && actual < i64::MAX as u64).then_some(actual)
        })
        .collect()
}

/// 统计卡顿帧数：相邻上屏间隔 > 2×窗口内帧间隔中位数。
///
/// 不能按 vsync 周期算：30fps 相机流在 60Hz 屏幕上每帧间隔 33ms，
/// 按 1.5×vsync(25ms) 会把每帧都误判为卡顿。中位数自适应应用自身节奏。
/// prev 为上个窗口的末尾帧（边界帧），使跨窗口的间隔也被覆盖。
/// 窗口内帧数 < 3 时中位数不可靠，不计卡顿。
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

/// 发现 PID 拥有的图层：
/// 1. 首选：全量 `dumpsys SurfaceFlinger`，按 BufferStateLayer 块的 ownerPID 归属匹配
///    （直渲染应用的 SurfaceView 图层名常不含包名，如 "SVM Container"，只能靠归属识别）
/// 2. 兜底：`--list` 按包名匹配（View 层级应用的图层名含包名）
pub fn discover_layers(pid: &str, package: &str) -> Vec<String> {
    let by_owner = utils::run_adb_command(&["shell", "dumpsys", "SurfaceFlinger"])
        .map(|o| parse_owned_buffer_layers(&o.stdout, pid))
        .unwrap_or_default();
    if !by_owner.is_empty() {
        return by_owner;
    }
    utils::run_adb_command(&["shell", "dumpsys", "SurfaceFlinger", "--list"])
        .map(|o| parse_list_layers(&o.stdout, package))
        .unwrap_or_default()
}

/// 解析全量 dumpsys：`+ BufferStateLayer (<name>) uid=...` 起一个新图层块，
/// 后续行 metadata 中含 `ownerPID:<pid>` 则归属该进程。
fn parse_owned_buffer_layers(dump: &str, pid: &str) -> Vec<String> {
    let owner_marker = format!("ownerPID:{}", pid);
    let mut layers = Vec::new();
    let mut current: Option<String> = None;
    for line in dump.lines() {
        if let Some(pos) = line.find("BufferStateLayer (") {
            let start = pos + "BufferStateLayer (".len();
            current = line[start..]
                .find(')')
                .map(|end| line[start..start + end].to_string());
        } else if line.contains("Layer (") {
            // 其他图层类型（ContainerLayer 等）起新块，终止当前块
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

/// 解析 --list 输出：保留包名匹配的行，去掉 "<hex> " 前缀（同一图层的别名行），去重。
fn parse_list_layers(list: &str, package: &str) -> Vec<String> {
    let mut layers = Vec::new();
    for line in list.lines() {
        let line = line.trim();
        if !line.contains(package) {
            continue;
        }
        let name = match line.split_once(' ') {
            Some((head, rest)) if !head.is_empty() && head.chars().all(|c| c.is_ascii_hexdigit()) => {
                rest.trim()
            }
            _ => line,
        };
        if !layers.contains(&name.to_string()) {
            layers.push(name.to_string());
        }
    }
    layers
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- parse_latency_output ----

    #[test]
    fn test_parse_latency_output_real_device_format() {
        // 真机格式：首行刷新周期，后续制表符分隔三列
        let out = "16666666\n2102436563608017\t2102436592973938\t2102436567588086\n2102436613000155\t2102436642905497\t2102436617210308\n";
        assert_eq!(parse_latency_output(out), vec![2102436592973938, 2102436642905497]);
    }

    #[test]
    fn test_parse_latency_output_zero_rows_filtered() {
        // 无新帧时全是 0 行，应被过滤
        let out = "16666666\n0\t0\t0\n0\t0\t0\n";
        assert!(parse_latency_output(out).is_empty());
    }

    #[test]
    fn test_parse_latency_output_filters_pending_sentinel() {
        // 真机实测：已入队未上屏的行 actualPresent = i64::MAX，必须过滤
        let out = "16666666\n2102436563608017\t2102436592973938\t2102436567588086\n2102436613000155\t9223372036854775807\t2102436617210308\n";
        assert_eq!(parse_latency_output(out), vec![2102436592973938]);
    }

    #[test]
    fn test_parse_latency_output_empty() {
        assert!(parse_latency_output("").is_empty());
    }

    // ---- count_jank ----

    #[test]
    fn test_count_jank_normal_sequence_no_jank() {
        // 60Hz（周期 16666666ns）均匀帧：无卡顿
        let p0 = 1_000_000_000u64;
        let presents: Vec<u64> = (0..10).map(|i| p0 + i * 16_666_666).collect();
        assert_eq!(count_jank(None, &presents), 0);
    }

    #[test]
    fn test_count_jank_detects_long_gap() {
        // 第 3、4 帧间隔 50ms（> 1.5×16.7ms=25ms）→ 1 次 jank
        let p0 = 1_000_000_000u64;
        let presents = vec![p0, p0 + 16_666_666, p0 + 16_666_666 * 2, p0 + 16_666_666 * 2 + 50_000_000];
        assert_eq!(count_jank(None, &presents), 1);
    }

    #[test]
    fn test_count_jank_boundary_frame_counts_cross_window_gap() {
        // 边界帧到本窗口首帧间隔 50ms，后续帧正常（16.7ms）：中位数 16.7ms，50ms 超 2× → 1 次 jank
        let p0 = 1_000_000_000u64;
        let presents = vec![p0 + 50_000_000, p0 + 50_000_000 + 16_666_666, p0 + 50_000_000 + 33_333_332];
        assert_eq!(count_jank(Some(p0), &presents), 1);
        // 间隔全部正常 → 不算
        let presents = vec![p0 + 16_666_666, p0 + 33_333_332, p0 + 49_999_998];
        assert_eq!(count_jank(Some(p0), &presents), 0);
    }

    #[test]
    fn test_count_jank_30fps_on_60hz_screen_not_janky() {
        // 回归：30fps 相机流在 60Hz 屏上帧间隔 33ms，中位数自适应 → 不能全判成卡顿
        let p0 = 1_000_000_000u64;
        let presents: Vec<u64> = (0..10).map(|i| p0 + i * 33_333_333).collect();
        assert_eq!(count_jank(None, &presents), 0);
    }

    #[test]
    fn test_count_jank_single_frame_no_jank() {
        // 帧数不足 3（间隔数 < 3）时中位数不可靠，不计卡顿
        assert_eq!(count_jank(None, &[1_000_000_000]), 0);
        assert_eq!(count_jank(Some(1_000_000_000), &[1_016_666_666]), 0);
    }

    // ---- parse_owned_buffer_layers ----

    #[test]
    fn test_parse_owned_buffer_layers_matches_owner_pid() {
        // 真机 dumpsys 片段（简化）：BufferStateLayer 块，metadata 行带 ownerPID
        let dump = "+ ContainerLayer (be31c3f SVM Container#0) uid=1000\n\
                    + BufferStateLayer (SVM Container#0) uid=1000\n\
                    \x20     parent=be31c3f, metadata={dequeueTime:123, windowType:2201, ownerPID:29697, ownerUID:1000}\n\
                    + BufferStateLayer (com.other.app/Main#0) uid=10123\n\
                    \x20     parent=xxx, metadata={dequeueTime:456, ownerPID:9999, ownerUID:10123}\n";
        let layers = parse_owned_buffer_layers(dump, "29697");
        assert_eq!(layers, vec!["SVM Container#0".to_string()]);
    }

    #[test]
    fn test_parse_owned_buffer_layers_no_match() {
        let dump = "+ BufferStateLayer (com.other.app/Main#0) uid=10123\n\
                    \x20     metadata={ownerPID:9999}\n";
        assert!(parse_owned_buffer_layers(dump, "29697").is_empty());
    }

    // ---- parse_list_layers ----

    #[test]
    fn test_parse_list_layers_strips_hex_alias_and_dedups() {
        // 真机格式：同一图层出现两次（带/不带 hex 前缀）
        let list = "147955a com.lixiang.car.x.svm/com.lixiang.car.x.svm.MainActivity#0\n\
                    com.lixiang.car.x.svm/com.lixiang.car.x.svm.MainActivity#0\n\
                    Background for SurfaceView[com.other.app]#0\n";
        let layers = parse_list_layers(list, "com.lixiang.car.x.svm");
        assert_eq!(
            layers,
            vec!["com.lixiang.car.x.svm/com.lixiang.car.x.svm.MainActivity#0".to_string()]
        );
    }

    #[test]
    fn test_parse_list_layers_filters_other_packages() {
        let list = "com.other.app/com.other.app.Main#0\n";
        assert!(parse_list_layers(list, "com.lixiang.car.x.svm").is_empty());
    }
}
