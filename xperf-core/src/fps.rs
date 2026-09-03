//! FPS 数据类型（采样逻辑已迁移至 xperf-agent 设备端实现，本文件只保留协议类型）

use crate::CHART_SERIES_CAP;
use chrono::{DateTime, Local};
use std::collections::VecDeque;

/// 单个图层的一条 FPS 样本
#[derive(Debug, Clone)]
pub struct FpsSample {
    pub timestamp: DateTime<Local>,
    pub layer: String,
    pub fps: f32,
    pub frame_count: u32,
    pub jank_count: u32,
}

/// FPS 时序：多图层各一条序列（按图层名分组），抽稀上限由 CHART_SERIES_CAP 控制
#[derive(Default)]
pub struct FpsTimeSeriesData {
    pub timestamps: VecDeque<DateTime<Local>>,
    pub fps: Vec<f32>,
    pub layers: Vec<String>,
    pub jank_counts: Vec<u32>,
}

impl FpsTimeSeriesData {
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

