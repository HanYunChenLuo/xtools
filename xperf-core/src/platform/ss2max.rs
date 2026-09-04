//! SS2 MAX / SA8155P 平台实现（桩，待补充）。
//!
//! 预期 GPU 数据源：topgpu 工具（push 到 /data/，读 gpu_busy_percentage sysfs 或
//! adreno_cmdbatch ftrace 事件），见平台文档《各平台 GPU 负载获取》。
//! 可能也有 kgsl sysfs 直通（需实测确认）。

use super::{Platform, PlatformId};

/// SS2 MAX 平台（SA8155）
pub struct Ss2Max;

impl Platform for Ss2Max {
    fn id(&self) -> PlatformId { PlatformId::Ss2Max }
    fn name(&self) -> &'static str { "SS2 MAX (SA8155P)" }
    fn gpu_hint(&self) -> &'static str { "auto" }
    fn description(&self) -> &'static str {
        "TODO: topgpu 工具 或 kgsl sysfs（待实测确认数据源）"
    }
}
