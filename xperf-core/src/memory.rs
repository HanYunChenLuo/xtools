//! 内存数据类型（采样逻辑已迁移至 xperf-agent 设备端实现，本文件只保留协议类型）

use crate::CHART_SERIES_CAP;
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// 进程内存分类明细（dumpsys meminfo App Summary，单位 KB）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MemoryDetails {
    /// Java 堆（KB）
    pub java_heap: u64,
    /// Native 堆（KB）
    pub native_heap: u64,
    /// 代码段（KB）
    pub code: u64,
    /// 栈（KB）
    pub stack: u64,
    /// 图形缓冲（KB）
    pub graphics: u64,
    /// 其他私有内存（KB）；协议 v8 起已扣减 dmabuf（DMA-BUF 单列后剩余的无语义兜底桶）
    pub private_other: u64,
    /// DMA-BUF 显存 PSS（KB；协议 v8 起）：agent root 下扫 smaps 按 VMA 名聚合
    /// （gralloc/dma-heap 缓冲的 CPU mmap，App Summary 本落 Private Other）；
    /// 低间隔/非 root（smaps 不可读）路径为 0
    #[serde(default)]
    pub dmabuf: u64,
    /// 系统分摊（KB）
    pub system: u64,
    /// 总 PSS（KB）
    pub total_pss: u64,
}

/// 内存时序（抽稀上限由 CHART_SERIES_CAP 控制）
#[derive(Default)]
pub struct MemoryTimeSeriesData {
    /// 采样时刻序列
    pub timestamps: VecDeque<DateTime<Local>>,
    /// 各时刻的内存明细
    pub memory_details: VecDeque<MemoryDetails>,
}

impl MemoryTimeSeriesData {
    /// 追加一个采样点；序列达到 2×CAP 时先每 2 取 1 抽稀再入队
    pub fn add_data_point(&mut self, timestamp: DateTime<Local>, details: MemoryDetails) {
        if self.timestamps.len() >= 2 * CHART_SERIES_CAP {
            crate::decimate(&mut self.timestamps);
            crate::decimate(&mut self.memory_details);
        }
        self.timestamps.push_back(timestamp);
        self.memory_details.push_back(details);
    }
}
