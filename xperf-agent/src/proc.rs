//! /proc 与 sysfs 读取：CPU jiffies、进程发现、cpufreq、IO、网络，
//! 以及进程级 CPU 采样状态（jiffies 基线 + 线程名缓存）。

use crate::json_escape;
use std::collections::HashMap;
use std::fs;

/// 读 /proc/stat：返回 (所有核总 jiffies, 核数)
pub(crate) fn read_total_jiffies() -> Option<(u64, u32)> {
    let content = fs::read_to_string("/proc/stat").ok()?;
    let mut total = None;
    let mut ncores = 0u32;
    for line in content.lines() {
        if line.starts_with("cpu ") {
            total = Some(
                line.split_whitespace()
                    .skip(1)
                    .map(|v| v.parse::<u64>().unwrap_or(0))
                    .sum(),
            );
        } else if line.starts_with("cpu")
            && line.as_bytes().get(3).is_some_and(|b| b.is_ascii_digit())
        {
            ncores += 1;
        }
    }
    Some((total?, ncores.max(1)))
}

/// 读 /proc/<pid>/stat 或 task/<tid>/stat 的 utime+stime jiffies
pub(crate) fn read_stat_jiffies(path: &str) -> Option<u64> {
    let content = fs::read_to_string(path).ok()?;
    parse_stat_jiffies(&content)
}

/// 进程名含括号且可能有空格，找最后一个 `)` 后取第 12、13 字段（utime, stime，0-indexed）
fn parse_stat_jiffies(stat: &str) -> Option<u64> {
    let paren_end = stat.trim().rfind(')')?;
    let fields: Vec<&str> = stat.trim()[paren_end + 1..].split_whitespace().collect();
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some(utime + stime)
}

/// 按包名解析 PID（扫 /proc/*/cmdline）
pub(crate) fn resolve_pids(package: &str) -> Vec<u32> {
    let mut pids = Vec::new();
    if let Ok(entries) = fs::read_dir("/proc") {
        for e in entries.flatten() {
            let name = e.file_name();
            let Some(name) = name.to_str() else { continue };
            let Ok(pid) = name.parse::<u32>() else { continue };
            if let Ok(cmd) = fs::read_to_string(format!("/proc/{}/cmdline", pid)) {
                if cmd.trim_end_matches('\0') == package {
                    pids.push(pid);
                }
            }
        }
    }
    pids.sort_unstable();
    pids
}

pub(crate) fn read_u64_file(path: &str) -> Option<u64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// 读 cpu<N> 的某个 cpufreq 节点（KHz）；核离线/节点缺失返回 None
pub(crate) fn read_cpufreq(core: u32, node: &str) -> Option<u64> {
    read_u64_file(&format!("/sys/devices/system/cpu/cpu{}/cpufreq/{}", core, node))
}

/// 读全部核 scaling_cur_freq（KHz）；读不到的核补 0，保持下标与核号对齐
pub(crate) fn read_cpu_freqs(ncores: u32) -> Vec<u64> {
    (0..ncores).map(|i| read_cpufreq(i, "scaling_cur_freq").unwrap_or(0)).collect()
}

/// 读 /proc/<pid>/io：(rchar, wchar, read_bytes, write_bytes)，单位字节。
/// rchar/wchar 是逻辑读写（含 page cache），read_bytes/write_bytes 是真实磁盘 IO。
pub(crate) fn read_pid_io(pid: u32) -> Option<(u64, u64, u64, u64)> {
    let content = fs::read_to_string(format!("/proc/{}/io", pid)).ok()?;
    parse_pid_io(&content)
}

fn parse_pid_io(content: &str) -> Option<(u64, u64, u64, u64)> {
    let (mut r, mut w, mut dr, mut dw) = (None, None, None, None);
    for line in content.lines() {
        if let Some(v) = line.strip_prefix("rchar:") {
            r = v.trim().parse().ok();
        } else if let Some(v) = line.strip_prefix("wchar:") {
            w = v.trim().parse().ok();
        } else if let Some(v) = line.strip_prefix("read_bytes:") {
            dr = v.trim().parse().ok();
        } else if let Some(v) = line.strip_prefix("write_bytes:") {
            dw = v.trim().parse().ok();
        }
    }
    Some((r?, w?, dr?, dw?))
}

/// 读 /proc/net/dev 聚合物理口收发字节数：(rx_bytes, tx_bytes)。
/// 排除回环与隧道/虚拟口（lo/sit/tun/gre/dummy/vti/ip6*），只统计真实网络活动。
/// 注意是整机口径：Android 应用共享 netns，/proc/<pid>/net/dev 与整机内容一致，
/// per-app 流量需 qtaguid/eBPF（此车机均不可用）。
pub(crate) fn read_net_dev() -> Option<(u64, u64)> {
    let content = fs::read_to_string("/proc/net/dev").ok()?;
    Some(parse_net_dev(&content))
}

fn parse_net_dev(content: &str) -> (u64, u64) {
    let (mut rx, mut tx) = (0u64, 0u64);
    for line in content.lines().skip(2) {
        let Some((iface, rest)) = line.split_once(':') else { continue };
        let iface = iface.trim();
        if iface == "lo"
            || iface.starts_with("sit")
            || iface.starts_with("tun")
            || iface.starts_with("gre")
            || iface.starts_with("dummy")
            || iface.contains("vti")
            || iface.starts_with("ip6")
        {
            continue;
        }
        let fields: Vec<&str> = rest.split_whitespace().collect();
        rx += fields.first().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
        tx += fields.get(8).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
    }
    (rx, tx)
}

// ---------- 进程 CPU 采样状态 ----------

/// 进程级 CPU 采样状态：进程/线程 jiffies 基线 + 线程名缓存。
pub(crate) struct PidState {
    prev_jiffies: u64,
    prev_threads: HashMap<u32, u64>,
    comm_cache: HashMap<u32, String>,
}

impl PidState {
    pub(crate) fn new(proc_jiffies: u64) -> Self {
        PidState {
            prev_jiffies: proc_jiffies,
            prev_threads: HashMap::new(),
            comm_cache: HashMap::new(),
        }
    }

    /// 一轮 CPU 采样：更新基线，返回 (进程 CPU%, 线程明细 th 数组内容)。
    /// 线程明细是 `,[tid,"name",cpu]` 逗号前缀拼接（调用方 trim 前导逗号），
    /// 只含 cpu>0.05% 的线程（静止线程不上报）。
    pub(crate) fn sample_cpu(&mut self, pid: u32, proc_jiffies: u64, total_delta: u64, ncores: u32) -> (f32, String) {
        let cpu = proc_jiffies.saturating_sub(self.prev_jiffies) as f32
            / total_delta as f32
            * 100.0
            * ncores as f32;
        self.prev_jiffies = proc_jiffies;

        // 线程级：读 task/ 下所有 tid
        let mut th_json = String::new();
        if let Ok(tids) = fs::read_dir(format!("/proc/{}/task", pid)) {
            let mut threads: Vec<u32> = tids
                .flatten()
                .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
                .collect();
            threads.sort_unstable();
            for tid in threads {
                let tj = read_stat_jiffies(&format!("/proc/{}/task/{}/stat", pid, tid));
                let Some(tj) = tj else { continue };
                let prev = self.prev_threads.insert(tid, tj);
                if let Some(prev) = prev {
                    let tcpu = tj.saturating_sub(prev) as f32
                        / total_delta as f32
                        * 100.0
                        * ncores as f32;
                    if tcpu > 0.05 {
                        let name = self.comm_cache.entry(tid).or_insert_with(|| {
                            fs::read_to_string(format!("/proc/{}/task/{}/comm", pid, tid))
                                .map(|s| s.trim().to_string())
                                .unwrap_or_else(|_| "?".into())
                        });
                        th_json.push_str(&format!(",[{},\"{}\",{:.2}]", tid, json_escape(name), tcpu));
                    }
                }
            }
            // 清掉已退出线程的状态
            self.prev_threads.retain(|tid, _| {
                fs::metadata(format!("/proc/{}/task/{}", pid, tid)).is_ok()
            });
        }
        (cpu, th_json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_stat_jiffies_basic() {
        let stat = "29697 (xiang.car.x.svm) S 1 2 3 0 -1 0 0 0 0 0 24130 38458 0 0 20 0 1 0";
        assert_eq!(parse_stat_jiffies(stat), Some(24130 + 38458));
    }

    #[test]
    fn test_parse_stat_jiffies_comm_with_spaces() {
        let stat = "123 (Signal Catcher) S 1 2 3 0 -1 0 0 0 0 0 100 200 0 0 20 0 1 0";
        assert_eq!(parse_stat_jiffies(stat), Some(300));
    }

    #[test]
    fn test_parse_stat_jiffies_truncated() {
        assert_eq!(parse_stat_jiffies("123 (comm) S 1 2 3"), None);
    }

    #[test]
    fn test_parse_pid_io() {
        let content = "rchar: 78340951\nwchar: 1099734450\nsyscr: 8814340\nsyscw: 29172547\nread_bytes: 16384\nwrite_bytes: 167936\ncancelled_write_bytes: 0\n";
        assert_eq!(parse_pid_io(content), Some((78340951, 1099734450, 16384, 167936)));
        assert_eq!(parse_pid_io("rchar: 1\n"), None); // 字段不全
    }

    #[test]
    fn test_parse_net_dev_skips_virtual_ifaces() {
        // 真机格式：eth* 物理口统计，lo/sit0/tunl0/gre0/dummy0/ip6_vti0 等虚拟口跳过
        let content = "Inter-|   Receive\n face |bytes\n\
                       eth0.4: 2017964286 23774549 0 0 0 0 0 1155 80722878944 59977192 0 0 0 0 0 0\n\
                       eth1: 9355968477 69004472 0 0 0 0 0 0 16194561619 118076019 0 0 0 0 0 0\n\
                       lo: 999 10 0 0 0 0 0 0 888 10 0 0 0 0 0 0\n\
                       sit0: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n\
                       tunl0: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n\
                       gre0: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n\
                       gretap0: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n\
                       dummy0: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n\
                       ip6_vti0: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n\
                       ip6gre0: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n";
        let (rx, tx) = parse_net_dev(content);
        assert_eq!(rx, 2017964286 + 9355968477);
        assert_eq!(tx, 80722878944 + 16194561619); // 不含 lo 的 888
    }
}
