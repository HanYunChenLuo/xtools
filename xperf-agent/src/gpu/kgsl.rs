//! kgsl sysfs 通道（标准 Android / SS2：GVM 内有 kgsl 节点直通）。

use std::fs;

/// kgsl sysfs 路径（GPU 使用率 + 时钟）。GPU 在 hypervisor 后的车机平台无此节点。
pub(crate) struct Kgsl {
    pub(crate) busy_path: &'static str,
    pub(crate) clk_path: Option<&'static str>,
}

pub(super) fn detect_kgsl() -> Option<Kgsl> {
    const BUSY: &str = "/sys/class/kgsl/kgsl-3d0/gpubusy";
    fs::metadata(BUSY).ok()?;
    let clk_path = ["/sys/class/kgsl/kgsl-3d0/gpuclk", "/sys/class/kgsl/kgsl-3d0/devfreq/cur_freq"]
        .into_iter()
        .find(|p| fs::metadata(p).is_ok());
    Some(Kgsl { busy_path: BUSY, clk_path })
}

/// 读 gpubusy："busy_time total_time"（µs 计数器），差值算窗口占比
pub(crate) fn read_gpu_busy(path: &str) -> Option<(u64, u64)> {
    let content = fs::read_to_string(path).ok()?;
    parse_gpu_busy(&content)
}

fn parse_gpu_busy(content: &str) -> Option<(u64, u64)> {
    let mut it = content.split_whitespace();
    let busy = it.next()?.parse().ok()?;
    let total = it.next()?.parse().ok()?;
    Some((busy, total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_gpu_busy() {
        assert_eq!(parse_gpu_busy("12345 67890\n"), Some((12345, 67890)));
        assert_eq!(parse_gpu_busy("12345\n"), None);
    }
}
