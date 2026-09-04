//! FPS 数据类型（采样逻辑已迁移至 xperf-agent 设备端实现，本文件只保留协议类型）

use crate::CHART_SERIES_CAP;
use chrono::{DateTime, Local};
use std::collections::VecDeque;

/// 单个图层的一条 FPS 样本
#[derive(Debug, Clone)]
pub struct FpsSample {
    /// 采样时刻
    pub timestamp: DateTime<Local>,
    /// 图层名
    pub layer: String,
    /// 帧率
    pub fps: f32,
    /// 窗口内新帧数
    pub frame_count: u32,
    /// 窗口内 jank 帧数
    pub jank_count: u32,
}

/// FPS 时序：多图层各一条序列（按图层名分组），抽稀上限由 CHART_SERIES_CAP 控制
#[derive(Default)]
pub struct FpsTimeSeriesData {
    /// 采样时刻序列
    pub timestamps: VecDeque<DateTime<Local>>,
    /// 帧率序列
    pub fps: Vec<f32>,
    /// 图层名序列
    pub layers: Vec<String>,
    /// jank 帧数序列
    pub jank_counts: Vec<u32>,
}

impl FpsTimeSeriesData {
    /// 追加一个采样点；序列达到 2×CAP 时先每 2 取 1 抽稀再入队
    pub fn add_data_point(&mut self, timestamp: DateTime<Local>, fps: f32, jank_count: u32, layer: &str) {
        if self.timestamps.len() >= 2 * CHART_SERIES_CAP {
            crate::decimate(&mut self.timestamps);
            crate::decimate_vec(&mut self.fps);
            crate::decimate_vec(&mut self.layers);
            crate::decimate_vec(&mut self.jank_counts);
        }
        self.timestamps.push_back(timestamp);
        self.fps.push(fps);
        self.layers.push(layer.to_string());
        self.jank_counts.push(jank_count);
    }
}

