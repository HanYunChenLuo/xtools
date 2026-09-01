pub mod agent;
pub mod cpu;
pub mod fps;
pub mod memory;
pub mod utils;

pub use cpu::{CpuSample1, ThreadCpuInfo};
pub use fps::{FpsPidState, FpsTimeSeriesData};
pub use memory::{MemoryDetails, MemoryTimeSeriesData};
pub use utils::{get_all_processes, run_adb_command, run_command, ProcessInfo, ProcOutput};

use chrono::{DateTime, Local};
use serde::Serialize;
use std::collections::{HashMap, VecDeque};

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
    /// 包名下无任何进程
    NoProcess { error: String },
    /// 采样中的非致命错误（如单次 ADB 失败）
    SampleError { pid: Option<String>, stage: String, error: String },
}

/// 采样器：封装多 PID 动态跟随的采样核心逻辑。
///
/// 状态归属：`pid_stats` / `restart_count` 在 Sampler 内部，调用方不直接持有。
/// 调用方每轮调 `sample_once()`，拿到 `Vec<SampleEvent>` 自行表现。
pub struct Sampler {
    package: String,
    interval_ms: u64,
    cpu: bool,
    memory: bool,
    thread: bool,
    fps: bool,
    pid_stats: HashMap<String, PidStats>,
    fps_states: HashMap<String, FpsPidState>,
    restart_count: u32,
}

impl Sampler {
    pub fn new(
        package: &str,
        interval_ms: u64,
        cpu: bool,
        memory: bool,
        thread: bool,
        fps: bool,
    ) -> Self {
        Self {
            package: package.to_string(),
            interval_ms,
            cpu,
            memory,
            thread,
            fps,
            pid_stats: HashMap::new(),
            fps_states: HashMap::new(),
            restart_count: 0,
        }
    }

    pub fn restart_count(&self) -> u32 {
        self.restart_count
    }

    pub fn pid_stats(&self) -> &HashMap<String, PidStats> {
        &self.pid_stats
    }

    /// 执行一轮采样，返回这轮产生的所有事件。
    ///
    /// 流程：
    /// 1. get_all_processes 动态跟随（新 PID 加入并 emit PidDiscovered）
    /// 2. CPU 采样（phase1 → sleep → phase2），emit CpuUpdate
    /// 3. 内存采样，emit MemoryUpdate
    /// 4. FPS 采样（SurfaceFlinger 图层帧时间戳），emit FpsUpdate
    /// 5. 进程消失时 emit PidDisappeared 并标记 active=false
    pub async fn sample_once(&mut self) -> Vec<SampleEvent> {
        let mut events = Vec::new();

        // 1. 动态跟随：枚举当前所有进程
        let current_processes = match get_all_processes(&self.package) {
            Ok(ps) => ps,
            Err(e) => {
                // 包名下无任何进程：所有已知 PID 标记失活
                for s in self.pid_stats.values_mut() {
                    if s.active {
                        s.active = false;
                        self.restart_count += 1;
                    }
                }
                events.push(SampleEvent::NoProcess { error: e.to_string() });
                // 无 CPU 采样 → 需自行维持节拍（由调用方处理 sleep，这里不 sleep）
                return events;
            }
        };
        for p in &current_processes {
            let is_new = !self.pid_stats.contains_key(&p.pid);
            self.pid_stats
                .entry(p.pid.clone())
                .or_insert_with(|| PidStats {
                    start_time: p.start_time.clone(),
                    active: true,
                    ..Default::default()
                });
            if let Some(s) = self.pid_stats.get_mut(&p.pid) {
                s.active = true;
                if s.start_time.is_empty() {
                    s.start_time = p.start_time.clone();
                }
            }
            if is_new {
                events.push(SampleEvent::PidDiscovered {
                    pid: p.pid.clone(),
                    start_time: p.start_time.clone(),
                });
            }
        }

        let active_pids: Vec<String> = self
            .pid_stats
            .iter()
            .filter(|(_, s)| s.active)
            .map(|(pid, _)| pid.clone())
            .collect();

        // 2. CPU 采样：两阶段，所有 PID 共享一个 sleep 窗口
        if self.cpu && !active_pids.is_empty() {
            let mut phase1_results: HashMap<String, CpuSample1> = HashMap::new();
            for pid in &active_pids {
                match cpu::sample_cpu_phase1(pid).await {
                    Ok(p1) => {
                        phase1_results.insert(pid.clone(), p1);
                    }
                    Err(e) => {
                        if e.to_string().contains("Process not found") {
                            if let Some(s) = self.pid_stats.get_mut(pid) {
                                s.active = false;
                                self.restart_count += 1;
                            }
                            events.push(SampleEvent::PidDisappeared { pid: pid.clone() });
                        } else {
                            events.push(SampleEvent::SampleError {
                                pid: Some(pid.clone()),
                                stage: "cpu_phase1".to_string(),
                                error: e.to_string(),
                            });
                        }
                    }
                }
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(self.interval_ms)).await;

            for (pid, p1) in &phase1_results {
                match cpu::sample_cpu_phase2(p1).await {
                    Ok((process_cpu, timestamp, threads)) => {
                        let s = self
                            .pid_stats
                            .get_mut(pid)
                            .expect("pid in phase1_results implies pid_stats has it");
                        s.cpu_data.add_data_point(timestamp, process_cpu, threads.clone());
                        if s.cpu_time.is_none() || process_cpu > s.cpu_usage {
                            s.cpu_usage = process_cpu;
                            s.cpu_time = Some(timestamp);
                        }
                        events.push(SampleEvent::CpuUpdate {
                            pid: pid.clone(),
                            timestamp,
                            process_cpu,
                            threads: threads.clone(),
                        });
                    }
                    Err(e) => {
                        if e.to_string().contains("Process not found") {
                            if let Some(s) = self.pid_stats.get_mut(pid) {
                                s.active = false;
                                self.restart_count += 1;
                            }
                            events.push(SampleEvent::PidDisappeared { pid: pid.clone() });
                        } else {
                            events.push(SampleEvent::SampleError {
                                pid: Some(pid.clone()),
                                stage: "cpu_phase2".to_string(),
                                error: e.to_string(),
                            });
                        }
                    }
                }
            }
        }

        // 3. 内存采样
        if self.memory && !active_pids.is_empty() {
            for pid in &active_pids {
                match memory::sample_memory(pid).await {
                    Ok((total_pss, timestamp, details)) => {
                        let s = self
                            .pid_stats
                            .get_mut(pid)
                            .expect("active pid must be in pid_stats");
                        s.memory_data.add_data_point(timestamp, details.clone());
                        if s.memory_time.is_none() || total_pss > s.memory_usage {
                            s.memory_usage = total_pss;
                            s.memory_time = Some(timestamp);
                        }
                        events.push(SampleEvent::MemoryUpdate {
                            pid: pid.clone(),
                            timestamp,
                            total_pss,
                            details,
                        });
                    }
                    Err(e) => {
                        if e.to_string().contains("Process not found") {
                            if let Some(s) = self.pid_stats.get_mut(pid) {
                                s.active = false;
                                self.restart_count += 1;
                            }
                            events.push(SampleEvent::PidDisappeared { pid: pid.clone() });
                        } else {
                            events.push(SampleEvent::SampleError {
                                pid: Some(pid.clone()),
                                stage: "memory".to_string(),
                                error: e.to_string(),
                            });
                        }
                    }
                }
            }
        }

        // 4. FPS 采样（SurfaceFlinger 图层帧时间戳，覆盖 SurfaceView/游戏直渲染）
        if self.fps && !active_pids.is_empty() {
            for pid in &active_pids {
                let state = self.fps_states.entry(pid.clone()).or_default();
                let result = state.sample(pid, &self.package);
                match result {
                    Ok(samples) => {
                        for sample in samples {
                            let s = self
                                .pid_stats
                                .get_mut(pid)
                                .expect("active pid must be in pid_stats");
                            s.fps_data.add_data_point(
                                sample.timestamp,
                                sample.fps,
                                sample.jank_count,
                                &sample.layer,
                            );
                            events.push(SampleEvent::FpsUpdate {
                                pid: pid.clone(),
                                timestamp: sample.timestamp,
                                layer: sample.layer,
                                fps: sample.fps,
                                frame_count: sample.frame_count,
                                jank_count: sample.jank_count,
                            });
                        }
                    }
                    Err(e) => {
                        events.push(SampleEvent::SampleError {
                            pid: Some(pid.clone()),
                            stage: "fps".to_string(),
                            error: e.to_string(),
                        });
                    }
                }
            }
        }

        events
    }

    /// 节拍 sleep：当未启用 CPU 采样时（CPU 采样内部已 sleep），调用方应调用此方法维持间隔。
    pub async fn tick_if_needed(&self) {
        if !self.cpu {
            tokio::time::sleep(tokio::time::Duration::from_millis(self.interval_ms)).await;
        }
    }

    pub fn interval_ms(&self) -> u64 {
        self.interval_ms
    }

    pub fn cpu_enabled(&self) -> bool {
        self.cpu
    }

    pub fn thread_enabled(&self) -> bool {
        self.thread
    }
}
