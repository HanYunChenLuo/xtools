//! SS2 PRO 平台实现（桩，待补充）。
//!
//! 预期与 SS2 MAX 类似（同代芯片），GPU 数据源待实测确认。

use super::{Platform, PlatformId};

/// SS2 PRO 平台
pub struct Ss2Pro;

impl Platform for Ss2Pro {
    fn id(&self) -> PlatformId { PlatformId::Ss2Pro }
    fn name(&self) -> &'static str { "SS2 PRO" }
    fn gpu_hint(&self) -> &'static str { "auto" }
    fn description(&self) -> &'static str {
        "TODO: 与 SS2 MAX 类似，GPU 数据源待实测确认"
    }
}
