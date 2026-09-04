//! 线程 CPU 数据类型（采样逻辑已迁移至 xperf-agent 设备端实现，本文件只保留协议类型）

use chrono::{DateTime, Local};
use serde::Serialize;
use std::cmp::Ordering;

/// 线程 CPU 使用信息（agent 协议 → 前端展示）
#[derive(Debug, Clone, Serialize)]
pub struct ThreadCpuInfo {
    /// 线程 TID
    pub tid: String,
    /// 线程 CPU %（单核口径）
    pub cpu_usage: f32,
    /// 线程名
    pub name: String,
    /// 采样时刻（None = 未知）
    pub timestamp: Option<DateTime<Local>>,
}

impl PartialEq for ThreadCpuInfo {
    fn eq(&self, other: &Self) -> bool {
        self.cpu_usage == other.cpu_usage
    }
}

impl Eq for ThreadCpuInfo {}

impl PartialOrd for ThreadCpuInfo {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ThreadCpuInfo {
    fn cmp(&self, other: &Self) -> Ordering {
        self.cpu_usage
            .partial_cmp(&other.cpu_usage)
            .unwrap_or(Ordering::Equal)
            .reverse()
    }
}
