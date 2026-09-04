//! 平台抽象：不同车机平台（SS2MAX/SS2PRO/SS3/SS4/标准Android）的性能数据采集方式有差异。
//!
//! 检测：从 `adb devices -l` 的 product/model 字段匹配平台标识（如 HU_SS3 → SS3）。
//! 各平台实现自己的 Platform trait，封装 GPU 数据源、QNX 地址、agent 额外参数等差异。
//! 当前仅 SS3 完整实现（QNX telnet GPU 通道）；其余平台为桩，待后续补充。

pub mod android;
pub mod ss2max;
pub mod ss2pro;
pub mod ss3;
pub mod ss4;

/// 平台标识（adb devices -l 的 product 字段映射）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformId {
    /// SS2 MAX（SA8155）
    Ss2Max,
    /// SS2 PRO
    Ss2Pro,
    /// SS3（SA8295P，GPU 由 QNX host 管理）
    Ss3,
    /// SS4
    Ss4,
    /// 标准 Android（未识别的 product 一律归此）
    Android,
}

impl PlatformId {
    /// 小写标识字符串（agent --platform 参数值）
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ss2Max => "ss2max",
            Self::Ss2Pro => "ss2pro",
            Self::Ss3 => "ss3",
            Self::Ss4 => "ss4",
            Self::Android => "android",
        }
    }

    /// 标识字符串解析（agent --platform 参数与 from_str 互逆）
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "ss2max" => Some(Self::Ss2Max),
            "ss2pro" => Some(Self::Ss2Pro),
            "ss3" => Some(Self::Ss3),
            "ss4" => Some(Self::Ss4),
            "android" => Some(Self::Android),
            _ => None,
        }
    }
}

/// 平台 trait：封装各平台性能数据采集差异。
pub trait Platform: Send + Sync {
    /// 平台标识
    fn id(&self) -> PlatformId;
    /// 展示名（含关键特性提示）
    fn name(&self) -> &'static str;
    /// GPU 采集方式提示
    fn gpu_hint(&self) -> &'static str;
    /// QNX host telnet 地址（仅 QNX 路径用，None = 无 QNX 侧）
    fn qnx_host(&self) -> Option<&'static str> { None }
    /// 平台特有的 agent 额外参数
    fn agent_args(&self) -> Vec<String> {
        vec!["--platform".into(), self.id().as_str().into()]
    }
    /// 简要说明
    fn description(&self) -> &'static str { "" }
}

/// 从 adb devices -l 输出检测平台
pub fn detect_platform(adb_devices_output: &str) -> Box<dyn Platform> {
    let mut matched: Vec<PlatformId> = Vec::new();
    for line in adb_devices_output.lines().skip(1) {
        let line = line.trim();
        if line.is_empty() || !line.contains("device") { continue; }
        if let Some(product) = line.split("product:").nth(1).and_then(|s| s.split_whitespace().next()) {
            let id = match product {
                "HU_SS2MAXF" => Some(PlatformId::Ss2Max),
                "HU_SS2PRO" => Some(PlatformId::Ss2Pro),
                "HU_SS3" => Some(PlatformId::Ss3),
                "HU_SS4" => Some(PlatformId::Ss4),
                _ => None,
            };
            if let Some(id) = id { matched.push(id); continue; }
        }
        if line.contains("HU_SS4") || line.contains("Smart_space_4") {
            matched.push(PlatformId::Ss4);
        }
    }
    // 多设备匹配：取第一个但打印警告
    if matched.len() > 1 {
        eprintln!("⚠️ 检测到 {} 台设备，使用第一台（{}）。多设备场景请确保 adb 目标正确。", matched.len(), matched[0].as_str());
    }
    match matched.first() {
        Some(&id) => from_id(id),
        None => Box::new(android::Android),
    }
}

/// 通过 adb devices -l 实时检测平台
pub fn detect_platform_live() -> Box<dyn Platform> {
    let output = crate::run_adb_command(&["devices", "-l"]).map(|o| o.stdout).unwrap_or_default();
    detect_platform(&output)
}

/// 按平台标识构造实现（from_id(PlatformId::Ss3) → Ss3 实例）
pub fn from_id(id: PlatformId) -> Box<dyn Platform> {
    match id {
        PlatformId::Ss2Max => Box::new(ss2max::Ss2Max),
        PlatformId::Ss2Pro => Box::new(ss2pro::Ss2Pro),
        PlatformId::Ss3 => Box::new(ss3::Ss3),
        PlatformId::Ss4 => Box::new(ss4::Ss4),
        PlatformId::Android => Box::new(android::Android),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_ss3() {
        let out = "List of devices attached\n6eb792dfb0f  device usb:1-13 product:HU_SS3 model:HU_SS3 device:HU_SS3 transport_id:3\n";
        assert_eq!(detect_platform(out).id(), PlatformId::Ss3);
    }

    #[test]
    fn test_detect_ss2max() {
        let out = "List of devices attached\nd1f39648c1f  device usb:1-13 product:HU_SS2MAXF model:HU_SS2MAXF device:HU_SS2MAXF transport_id:4\n";
        assert_eq!(detect_platform(out).id(), PlatformId::Ss2Max);
    }

    #[test]
    fn test_detect_ss4() {
        let out = "List of devices attached\n56df7065b0f  device usb:1-1.3 transport_id:1\nlocalhost:5559  device product:HU_SS4 model:HU_Smart_space_4_0 device:HU_SS4 transport_id:2\n";
        assert_eq!(detect_platform(out).id(), PlatformId::Ss4);
    }

    #[test]
    fn test_detect_android_fallback() {
        let out = "List of devices attached\n12345  device product:SomePhone model:Pixel device:foo transport_id:1\n";
        assert_eq!(detect_platform(out).id(), PlatformId::Android);
    }
}
