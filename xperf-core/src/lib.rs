pub mod agent;
pub mod cpu;
pub mod fps;
pub mod marker;
pub mod memory;
pub mod platform;
pub mod trace;
pub mod utils;

pub use agent::MetricFlags;
pub use cpu::ThreadCpuInfo;
pub use fps::FpsTimeSeriesData;
pub use marker::{send_marker, start_marker_listener, Marker};
pub use memory::{MemoryDetails, MemoryTimeSeriesData};
pub use platform::{detect_platform, detect_platform_live, from_id, Platform, PlatformId};
pub use utils::{run_adb_command, ProcOutput};

use chrono::{DateTime, Local};
use serde::Serialize;
use std::collections::VecDeque;

/// 内存中时序序列的抽稀上限：采样数据由 CLI 边采边流式落盘（CSV 始终完整），
/// 内存序列只服务退出时的图表渲染——超过 2×CAP 时每 2 取 1 抽稀，
/// 保留完整时间范围、分辨率随运行时长自适应降级，长测内存有界。
/// CAP=30_000：50ms 间隔下约 25 分钟全精度；1s 间隔下永远达不到上限。
pub const CHART_SERIES_CAP: usize = 30_000;

/// 每 2 取 1 原地抽稀（保留首尾，容量减半）
pub fn decimate<T>(dq: &mut VecDeque<T>) {
    let mut i = 0;
    dq.retain(|_| {
        let keep = i % 2 == 0;
        i += 1;
        keep
    });
}

/// Vec 版抽稀（FpsTimeSeriesData 用 Vec 存储）
pub(crate) fn decimate_vec<T>(v: &mut Vec<T>) {
    let mut i = 0;
    v.retain(|_| {
        let keep = i % 2 == 0;
        i += 1;
        keep
    });
}

/// 单个 PID 的监控状态。多进程应用会有多个 PidStats，按 pid 索引在 HashMap 中。
#[derive(Default)]
pub struct PidStats {
    pub cpu_data: CpuTimeSeriesData,
    pub memory_data: MemoryTimeSeriesData,
    pub cpu_usage: f32, // 该 PID 的峰值 CPU %
    pub cpu_time: Option<DateTime<Local>>, // 该 PID 达到峰值 CPU 的时间（None = 尚无采样）
    pub memory_usage: u64, // 该 PID 的峰值内存 KB
    pub memory_time: Option<DateTime<Local>>, // 该 PID 达到峰值内存的时间（None = 尚无采样）
    pub fps_data: FpsTimeSeriesData, // 该 PID 的 FPS 时序
    pub start_time: String, // 该 PID 的启动时间
    pub active: bool, // 是否仍在运行（动态跟随：消失的 PID 置 false 但保留数据）
}

#[derive(Default)]
pub struct CpuTimeSeriesData {
    pub timestamps: VecDeque<DateTime<Local>>,
    pub process_cpu: VecDeque<f32>,
    pub top_threads: VecDeque<Vec<ThreadCpuInfo>>,
}

impl CpuTimeSeriesData {
    pub fn add_data_point(
        &mut self,
        timestamp: DateTime<Local>,
        process_cpu: f32,
        top_threads: Vec<ThreadCpuInfo>,
    ) {
        if self.timestamps.len() >= 2 * CHART_SERIES_CAP {
            decimate(&mut self.timestamps);
            decimate(&mut self.process_cpu);
            decimate(&mut self.top_threads);
        }
        self.timestamps.push_back(timestamp);
        self.process_cpu.push_back(process_cpu);
        self.top_threads.push_back(top_threads);
    }
}

/// 每轮采样产出的结构化事件。调用方（CLI/Tauri）据此决定如何表现：打印/emit/落盘。
#[derive(Debug, Clone, Serialize)]
pub enum SampleEvent {
    /// 发现新 PID（动态跟随：新出现的进程）
    PidDiscovered { pid: String, start_time: String },
    /// PID 消失（进程退出/被杀）
    PidDisappeared { pid: String },
    /// CPU 采样结果（某 PID 一轮的进程 CPU% + top 线程）
    CpuUpdate {
        pid: String,
        timestamp: DateTime<Local>,
        process_cpu: f32,
        threads: Vec<ThreadCpuInfo>,
    },
    /// 内存采样结果（某 PID 一轮的内存详情）
    MemoryUpdate {
        pid: String,
        timestamp: DateTime<Local>,
        total_pss: u64,
        details: MemoryDetails,
    },
    /// FPS 采样结果（该 PID 最活跃图层的帧率；界面静止时 fps=0 为真实状态）
    FpsUpdate {
        pid: String,
        timestamp: DateTime<Local>,
        layer: String,
        fps: f32,
        frame_count: u32,
        jank_count: u32,
    },
    /// agent 握手信息（核数 + 每核最大频率 KHz）
    AgentHello { ncores: u32, maxkhz: Vec<u64> },
    /// 每核当前频率（KHz），下标与 AgentHello 的 maxkhz 对应
    FreqUpdate { timestamp: DateTime<Local>, khz: Vec<u64> },
    /// 温度与热降频状态（status: Android ThermalStatus，-1=未知；sensors: [名称, 类型, °C]）
    TempUpdate { timestamp: DateTime<Local>, status: i32, sensors: Vec<(String, i32, f32)> },
    /// GPU：busy 为窗口占比 %；mhz 为当前时钟（0 = 无时钟源）；util/maxmhz 仅 QNX 路径有值
    GpuUpdate { timestamp: DateTime<Local>, busy: f32, util: f32, mhz: u32, maxmhz: u32 },
    /// QNX 路径：每进程 GPU busy %
    GpuProcUpdate { pid: String, timestamp: DateTime<Local>, busy: f32 },
    /// GPU 显存（--gpu 降级路径，hypervisor 平台）：每 PID 字节数 + 整机 global
    GpuMemUpdate { pid: String, timestamp: DateTime<Local>, bytes: u64, global: u64 },
    /// 每 PID IO 速率 KB/s：r/w=逻辑读写，dr/dw=磁盘读写
    IoUpdate { pid: String, timestamp: DateTime<Local>, r: f32, w: f32, dr: f32, dw: f32 },
    /// 整机网络速率 KB/s（聚合物理口，排除回环/隧道；per-app 无数据源）
    NetUpdate { timestamp: DateTime<Local>, rx: f32, tx: f32 },
    /// 包名下无任何进程
    NoProcess { error: String },
    /// 采样中的非致命错误（如单次 ADB 失败）
    SampleError { pid: Option<String>, stage: String, error: String },
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cpu_time_series_decimates_beyond_cap() {
        let mut d = CpuTimeSeriesData::default();
        let t0 = Local::now();
        let n = 2 * CHART_SERIES_CAP + 10;
        for i in 0..n {
            d.add_data_point(t0 + chrono::Duration::milliseconds(i as i64), i as f32, Vec::new());
        }
        // 达到 2×CAP 时抽稀一次（每 2 取 1），再追加 10 个点
        assert_eq!(d.timestamps.len(), CHART_SERIES_CAP + 10);
        assert_eq!(d.process_cpu.len(), CHART_SERIES_CAP + 10);
        assert_eq!(d.top_threads.len(), CHART_SERIES_CAP + 10);
        // 首点保留；抽稀保留偶数位；末点保留 → 时间范围完整
        assert_eq!(d.process_cpu[0], 0.0);
        assert_eq!(d.process_cpu[1], 2.0);
        assert_eq!(*d.process_cpu.back().unwrap(), (n - 1) as f32);
        assert_eq!(d.timestamps[0], t0);
        assert_eq!(
            *d.timestamps.back().unwrap(),
            t0 + chrono::Duration::milliseconds((n - 1) as i64)
        );
    }

    #[test]
    fn test_memory_time_series_decimates_beyond_cap() {
        let mut d = MemoryTimeSeriesData::default();
        let n = 2 * CHART_SERIES_CAP + 5;
        for i in 0..n {
            d.add_data_point(
                Local::now(),
                MemoryDetails { total_pss: i as u64, ..Default::default() },
            );
        }
        assert_eq!(d.timestamps.len(), CHART_SERIES_CAP + 5);
        assert_eq!(d.memory_details.len(), CHART_SERIES_CAP + 5);
        assert_eq!(d.memory_details[0].total_pss, 0);
        assert_eq!(d.memory_details[1].total_pss, 2);
        assert_eq!(d.memory_details.back().unwrap().total_pss, (n - 1) as u64);
    }

    #[test]
    fn test_fps_time_series_decimates_beyond_cap() {
        let mut d = FpsTimeSeriesData::default();
        let n = 2 * CHART_SERIES_CAP + 5;
        for i in 0..n {
            d.add_data_point(Local::now(), i as f32, i as u32, "L");
        }
        assert_eq!(d.timestamps.len(), CHART_SERIES_CAP + 5);
        assert_eq!(d.fps[0], 0.0);
        assert_eq!(d.fps[1], 2.0);
        assert_eq!(*d.fps.last().unwrap(), (n - 1) as f32);
        assert_eq!(d.layers.len(), CHART_SERIES_CAP + 5);
        assert_eq!(d.jank_counts.len(), CHART_SERIES_CAP + 5);
    }

    #[test]
    fn test_series_below_cap_untouched() {
        let mut d = CpuTimeSeriesData::default();
        for i in 0..100 {
            d.add_data_point(Local::now(), i as f32, Vec::new());
        }
        assert_eq!(d.process_cpu.len(), 100);
        assert_eq!(d.process_cpu[1], 1.0);
    }
}

