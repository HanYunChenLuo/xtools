//! SS4 / SA8797P 平台实现（MindRT Linux PVM + Android 16 GVM 双系统）。
//!
//! 双系统拓扑与 adb 自动桥接见 `docs/DESIGN-ss4-adb.md` 与 CLAUDE.md「SS4 adb
//! 自动桥接」；指标数据源细节见 `docs/DESIGN-ss4-metrics.md`。要点：
//!
//! - **GPU busy/每进程**：ligfxprofilerd 在 **MindRT 侧**（GVM logcat 无输出，
//!   agent 的 ligfx 通道在 SS4 永不命中仅作探测保留）——host 侧通道经桥接网关
//!   读 MindRT logcat（`xperf-core/hostchan.rs`），业务侧只看 Utilization。
//!   `Frequency: 1000 Hz` 恒值（空闲/负载不变；GPU VFIO 直通，两侧均无
//!   kgsl/devfreq 节点可对照）——按定频占位/单位标注存疑处理，事件原样透传。
//!   `persist.vendor.ligfxprofiler.sampling_interval_ms` 实测为动态读取，但
//!   调小到 1000 会让 ligfxprofilerd 停止输出（恢复 5000 后恢复），勿调。
//! - **GPU 显存**：`dumpsys gpu` Memory snapshot 可用（agent 保底通道生效）。
//! - **FPS**：QCM SDE 定制 SF 构建 `--latency` 全图层恒空；host 侧
//!   frametimeline-only perfetto 短窗通道兜底（hostchan，display 合成流口径）。
//! - **CPU 频率/温度**：GVM 无 cpufreq sysfs（`cpu0/cpufreq` 不存在）、无
//!   thermal zones 且 thermalservice HAL Ready=false——VM 隔离的平台限制，
//!   agent 探测后如实 err 禁用（hello maxkhz 全 0 即此原因）。
//! - **simpleperf**：GVM 硬件 PMU 未虚拟化，cpu-cycles 近乎无样本；
//!   core 自动改用软件事件 cpu-clock。

use super::{Platform, PlatformId};

/// SS4 平台
pub struct Ss4;

impl Platform for Ss4 {
    fn id(&self) -> PlatformId { PlatformId::Ss4 }
    fn name(&self) -> &'static str { "SS4 (SA8797P)" }
    fn gpu_hint(&self) -> &'static str { "ligfx(host)" }
    fn description(&self) -> &'static str {
        "MindRT+GVM 双系统（adb 桥接）；GPU busy 走 host 侧 ligfx 通道（MindRT logcat），FPS 走 frametimeline perfetto；GVM 无 cpufreq/thermal（VM 隔离）"
    }
}
