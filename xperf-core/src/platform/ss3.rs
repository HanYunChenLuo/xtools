//! SS3 / SA8295P 平台实现（当前唯一完整实现）。
//!
//! GPU 由 QNX host 管理，Android GVM 内无 kgsl sysfs/设备节点/ftrace 事件。
//! 通过 busybox telnet 连接 QNX host（172.31.101.52），写 /dev/kgsl-control
//! 开统计，slog2info -W 流式读 kgsl slog 行。
//! 同时每秒补采 dumpsys gpu 显存（GVM 侧可用）。

use super::{Platform, PlatformId};

pub struct Ss3;

impl Platform for Ss3 {
    fn id(&self) -> PlatformId { PlatformId::Ss3 }
    fn name(&self) -> &'static str { "SS3 (SA8295P)" }
    fn gpu_hint(&self) -> &'static str { "qnx" }
    fn qnx_host(&self) -> Option<&'static str> { Some("172.31.101.52") }
    fn description(&self) -> &'static str {
        "GPU 由 QNX host 管理，走 telnet 172.31.101.52 + slog2info；\n\
         dumpsys gpu 显存补采；thermalservice 为 test HAL 假数据"
    }
}
