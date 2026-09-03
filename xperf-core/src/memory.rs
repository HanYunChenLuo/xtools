//! 内存数据类型（采样逻辑已迁移至 xperf-agent 设备端实现，本文件只保留协议类型）

use crate::CHART_SERIES_CAP;
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// 进程内存分类明细（dumpsys meminfo App Summary，单位 KB）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MemoryDetails {
    pub java_heap: u64,
    pub native_heap: u64,
    pub code: u64,
    pub stack: u64,
    pub graphics: u64,
    pub private_other: u64,
    pub system: u64,
    pub total_pss: u64,
}

/// 内存时序（抽稀上限由 CHART_SERIES_CAP 控制）
#[derive(Default)]
pub struct MemoryTimeSeriesData {
    pub timestamps: VecDeque<DateTime<Local>>,
    pub memory_details: VecDeque<MemoryDetails>,
}

impl MemoryTimeSeriesData {
    pub fn add_data_point(&mut self, timestamp: DateTime<Local>, details: MemoryDetails) {
        if self.timestamps.len() >= 2 * CHART_SERIES_CAP {
            crate::decimate(&mut self.timestamps);
            crate::decimate(&mut self.memory_details);
        }
        self.timestamps.push_back(timestamp);
        self.memory_details.push_back(details);
    }
}
