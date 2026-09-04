//! SS4 / SA8797P 平台实现（桩，待补充）。
//!
//! 预期 GPU 数据源：PVM 侧 `logcat -s ligfxprofilerd`（每帧 GPU0/GPU1 的
//! Frequency/Busy/Queued/Utilization + 每进程 `GVM_<comm>` busy%）。
//! 见平台文档：业务侧只需关注 Utilization 字段。

use super::{Platform, PlatformId};

/// SS4 平台
pub struct Ss4;

impl Platform for Ss4 {
    fn id(&self) -> PlatformId { PlatformId::Ss4 }
    fn name(&self) -> &'static str { "SS4 (SA8797P)" }
    fn gpu_hint(&self) -> &'static str { "ligfx" }
    fn description(&self) -> &'static str {
        "TODO: PVM 侧 logcat -s ligfxprofilerd（每帧 Utilization + 每进程 busy）"
    }
}
