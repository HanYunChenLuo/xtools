//! 标准 Android 手机/平板实现（桩，待补充）。
//!
//! 预期 GPU 数据源：kgsl sysfs 直通（/sys/class/kgsl/kgsl-3d0/gpubusy），
//! 真实 thermal zones（/sys/class/thermal/thermal_zone*），无需 QNX/telnet。

use super::{Platform, PlatformId};

/// 标准安卓平台（未识别的 product 归此）
pub struct Android;

impl Platform for Android {
    fn id(&self) -> PlatformId { PlatformId::Android }
    fn name(&self) -> &'static str { "Android（标准手机/平板）" }
    fn gpu_hint(&self) -> &'static str { "kgsl" }
    fn description(&self) -> &'static str {
        "kgsl sysfs 直通（gpubusy）；真实 thermal zones"
    }
}
