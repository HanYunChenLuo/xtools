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

/// 读 gpubusy 原始读数 "busy_time total_time"（µs）。
/// 语义由 [`GpuBusyCalc`] 判别（累计 / 窗口两种内核实现，见该类型文档）。
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

/// 累计语义的 total 是开机至今的 µs 数：开机超过 60s 后必大于该值。
/// 读数低于它即不可能是累计计数器（误判窗口：设备开机 <60s 内采样，
/// 窗口语义下 busy/total 直读与累计差值占比同量级，值仍有界）。
const CUMULATIVE_MIN_TOTAL_US: u64 = 60_000_000;

/// gpubusy 忙碌占比计算器。该 sysfs 节点存在两种内核实现：
///
/// - **累计语义**（标准 kgsl，如手机/SS2PRO）：busy/total 为开机至今 µs 计数，
///   单调不减，逐窗取差值算占比。
/// - **窗口语义**（SS2MAX/8155 厂商内核，2026-09 实测）：每次读数是上一个 ~1s
///   窗口的 busy/total µs（total 恒 ≈1e6，随窗口边界上下波动，GPU 空闲报 "0 0"），
///   busy/total 直接即占比。按累计差值解析会得出 >100% 的荒谬值（实测 1662%）。
///
/// 判别（命中任一即永久锁定窗口语义——累计计数器不可能出现这些特征）：
/// - `total < `[`CUMULATIVE_MIN_TOTAL_US`]；
/// - total 下降，或 total 不变/上升而 busy 下降（累计计数器不回退）；
/// - 差值占比 >100%（busy 增量超过 total 增量）。
///
/// 返回 None 表示本轮不出数：累计语义首轮建基线、窗口语义下窗口未刷新（内核 ~1s
/// 更新一次，节拍更快时重复读到同一窗口，读数与上轮全等）或 GPU 空闲报 "0 0"。
/// 窗口语义读数自含占比，首轮即出数；累计语义须等第二轮差值。
pub(crate) struct GpuBusyCalc {
    prev: Option<(u64, u64)>,
    windowed: bool,
}

impl GpuBusyCalc {
    pub(crate) fn new() -> Self {
        Self { prev: None, windowed: false }
    }

    /// 喂入一条 gpubusy 读数，返回 busy%（0..=100；出数条件见类型文档）。
    pub(crate) fn sample(&mut self, busy: u64, total: u64) -> Option<f32> {
        if !self.windowed
            && (total < CUMULATIVE_MIN_TOTAL_US
                || self.prev.is_some_and(|(pb, pt)| {
                    total < pt || busy < pb || (total > pt && busy - pb > total - pt)
                }))
        {
            self.windowed = true;
        }
        let prev = self.prev.replace((busy, total));
        if self.windowed {
            // 读数与上轮全等 = 窗口未刷新（内核 ~1s 更新一次，节拍更快时拿到同一窗口）
            if total == 0 || prev == Some((busy, total)) {
                return None;
            }
            return Some(busy as f32 / total as f32 * 100.0);
        }
        let (pb, pt) = prev?;
        let dtotal = total - pt;
        if dtotal == 0 {
            return None;
        }
        Some((busy - pb) as f32 / dtotal as f32 * 100.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_gpu_busy() {
        assert_eq!(parse_gpu_busy("12345 67890\n"), Some((12345, 67890)));
        assert_eq!(parse_gpu_busy("12345\n"), None);
    }

    #[test]
    fn test_busy_calc_cumulative() {
        // 标准 kgsl 累计语义：total 为开机至今 µs（≥60e6），单调不减
        let mut c = GpuBusyCalc::new();
        assert_eq!(c.sample(30_000_000, 60_000_000), None); // 首轮建基线
        assert_eq!(c.sample(40_000_000, 70_000_000), Some(100.0)); // 10e6/10e6
        assert_eq!(c.sample(45_000_000, 80_000_000), Some(50.0));
        assert_eq!(c.sample(45_000_000, 80_000_000), None); // 读数未变不出数
        assert_eq!(c.sample(45_500_000, 81_000_000), Some(50.0));
    }

    #[test]
    fn test_busy_calc_windowed_ss2max() {
        // SS2MAX 真机序列（2026-09 采集，total 恒 ≈1e6 即 ~1s 窗口）：
        // 读数自含占比，首轮即出数；与 gpu_busy_percentage 节点同刻值一致
        let mut c = GpuBusyCalc::new();
        let cases = [
            (746411, 1012533, 73.72),
            (743758, 1001581, 74.26),
            (708806, 1002405, 70.71),
            (770193, 1015232, 75.86),
        ];
        for (busy, total, expect) in cases {
            let pct = c.sample(busy, total).unwrap();
            assert!((pct - expect).abs() < 0.01, "{busy}/{total}: {pct} vs {expect}");
        }
        // 同一窗口重复读数（total 未变）不出数；GPU 空闲 "0 0" 不出数
        assert_eq!(c.sample(770193, 1015232), None);
        assert_eq!(c.sample(0, 0), None);
        // 空闲后新窗口恢复出数
        assert!(c.sample(750606, 1003047).is_some());
    }

    #[test]
    fn test_busy_calc_windowed_latch_by_anomaly() {
        // total ≥60e6 不触发幅值判别，但累计计数器不可能出现的特征仍锁定窗口语义：
        // busy 回退（total 上升）
        let mut c = GpuBusyCalc::new();
        assert_eq!(c.sample(500_000_000, 600_000_000), None);
        let pct = c.sample(400_000_000, 700_000_000).unwrap();
        assert!((pct - 400.0 / 7.0).abs() < 0.01);
        // 差值占比 >100%（busy 增量 > total 增量）
        let mut c = GpuBusyCalc::new();
        assert_eq!(c.sample(100_000_000, 200_000_000), None);
        let pct = c.sample(300_000_000, 310_000_000).unwrap();
        assert!((pct - 300.0 / 3.1).abs() < 0.01);
    }
}
