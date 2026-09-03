//! GPU 采样：五通道按平台/探测结果选择（detect_gpu_path_ex）。
//! - Kgsl：标准 Android / SS2，sysfs gpubusy 直通（GVM 内有 kgsl）
//! - Qnx：SS3/8295，QNX host 侧 kgsl slog（GVM 内无 kgsl，telnet 读 QNX）
//! - TopGpu：SS2MAX/8155，topgpu 工具（push 到 /data/，读 sysfs 或 ftrace）
//! - Ligfx：SS4/8797，PVM 侧 logcat -s ligfxprofilerd（每帧 Utilization + 每进程 busy）
//! - DumpMem：保底，dumpsys gpu 每 PID 显存
//!
//! QNX/TopGpu/Ligfx 是流式通道（子进程持续输出行），共用一套读线程骨架
//! （spawn_stream_parser）：逐行读 stdout → 解析成 GpuEvent → emit；
//! kgsl/DumpMem 由主循环按节拍轮询。

mod kgsl;
mod ligfx;
mod qnx;
mod topgpu;

use crate::{dumpsys, emit, json_escape, now_ms};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub(crate) use kgsl::{read_gpu_busy, Kgsl};

/// GPU 采样路径（枚举变体即通道）
pub(crate) enum GpuPath {
    Kgsl(Kgsl),
    Qnx,
    TopGpu,
    Ligfx,
    DumpMem,
}

/// 设置 QNX 地址（main 启动时调用一次，必须在任何通道探测之前）
pub(crate) fn set_qnx_host(ip: &str) {
    qnx::set_qnx_host(ip);
}

/// GPU 路径探测（带平台提示）
/// platform: "ss3" → QNX | "ss2max" → TopGpu | "ss4" → Ligfx | "android" → Kgsl | None → 自动探测
pub(crate) fn detect_gpu_path_ex(platform: Option<&str>) -> Option<GpuPath> {
    match platform {
        Some("ss3") => {
            // SS3：跳过 kgsl，QNX 优先，失败则 dumpsys 保底
            if qnx::available() {
                Some(GpuPath::Qnx)
            } else {
                dumpsys(&["gpu"]).and_then(|s| parse_gpu_mem_snapshot(&s).map(|_| GpuPath::DumpMem))
            }
        }
        Some("ss2max") | Some("ss2pro") => {
            // SS2 系列：kgsl sysfs 优先（直通），失败则 topgpu 工具，再失败 dumpsys 保底
            if let Some(k) = kgsl::detect_kgsl() {
                Some(GpuPath::Kgsl(k))
            } else if topgpu::available() {
                Some(GpuPath::TopGpu)
            } else {
                dumpsys(&["gpu"]).and_then(|s| parse_gpu_mem_snapshot(&s).map(|_| GpuPath::DumpMem))
            }
        }
        Some("ss4") => {
            // SS4：ligfxprofilerd logcat 优先，失败则 kgsl（可能有），再失败 dumpsys 保底
            if ligfx::available() {
                Some(GpuPath::Ligfx)
            } else if let Some(k) = kgsl::detect_kgsl() {
                Some(GpuPath::Kgsl(k))
            } else {
                dumpsys(&["gpu"]).and_then(|s| parse_gpu_mem_snapshot(&s).map(|_| GpuPath::DumpMem))
            }
        }
        _ => {
            // 自动探测 / android：kgsl 优先，QNX 次之，dumpsys 保底
            if let Some(k) = kgsl::detect_kgsl() {
                Some(GpuPath::Kgsl(k))
            } else if qnx::available() {
                Some(GpuPath::Qnx)
            } else {
                dumpsys(&["gpu"]).and_then(|s| parse_gpu_mem_snapshot(&s).map(|_| GpuPath::DumpMem))
            }
        }
    }
}

/// 流式通道统一事件：三通道（QNX/TopGpu/Ligfx）样本归一后交给公共读线程 emit。
pub(crate) enum GpuEvent {
    /// 系统级 busy%；util/maxmhz 按通道有无（wire 上按需输出）
    Sys { mhz: u32, maxmhz: Option<u32>, util: Option<f32>, busy: f32 },
    /// 进程级 busy%（按进程名归因到 Android PID）
    Proc { name: String, busy: f32 },
}

/// 公共读线程骨架：逐行读子进程 stdout → parse → 发 gpu/gpuproc 事件。
/// keepalive：QNX telnet 的 stdin 须移交线程持有保活（drop 即 EOF，telnet 会退出）。
/// eof_err：流断开时的 err 文案（None 则静默退出）。
/// agent 退出时管道断开，子进程随会话结束自行清理。
fn spawn_stream_parser(
    child: std::process::Child,
    reader: std::io::BufReader<std::process::ChildStdout>,
    keepalive: Option<std::process::ChildStdin>,
    eof_err: Option<&'static str>,
    pid_names: &Arc<Mutex<HashMap<String, u32>>>,
    parse: fn(&str) -> Option<GpuEvent>,
) {
    let pid_names = pid_names.clone();
    std::thread::spawn(move || {
        use std::io::BufRead;
        let _keepalive = keepalive;
        let mut child = child;
        let mut reader = reader;
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => {
                    let _ = child.kill();
                    if let Some(msg) = eof_err {
                        emit(&format!("{{\"t\":\"err\",\"msg\":\"{}\"}}", json_escape(msg)));
                    }
                    return;
                }
                Ok(_) => {
                    if let Some(ev) = parse(&line) {
                        emit_event(ev, &pid_names);
                    }
                }
            }
        }
    });
}

/// 事件落线：gpu（busy[,util][,mhz][,maxmhz]，按通道字段有无按需输出）/ gpuproc（按名归因）
fn emit_event(ev: GpuEvent, pid_names: &Mutex<HashMap<String, u32>>) {
    match ev {
        GpuEvent::Sys { mhz, maxmhz, util, busy } => {
            let mut s = format!("{{\"t\":\"gpu\",\"ts\":{},\"busy\":{:.2}", now_ms(), busy);
            if let Some(u) = util {
                s.push_str(&format!(",\"util\":{:.2}", u));
            }
            s.push_str(&format!(",\"mhz\":{}", mhz));
            if let Some(m) = maxmhz {
                s.push_str(&format!(",\"maxmhz\":{}", m));
            }
            s.push('}');
            emit(&s);
        }
        GpuEvent::Proc { name, busy } => {
            if let Some(pid) = lookup_pid(&pid_names.lock().unwrap(), &name) {
                emit(&format!(
                    "{{\"t\":\"gpuproc\",\"ts\":{},\"pid\":{},\"busy\":{:.2}}}",
                    now_ms(), pid, busy
                ));
            }
        }
    }
}

/// 进程名归因：QNX/topgpu/ligfx 显示名可能完整（包名>15字符），/proc/comm 截断到 15 字符。
/// 匹配策略：先精确匹配，失败则截断到 15 字符再匹配。
/// 注意：多字节 UTF-8 字符（如中文进程名）不能用字节切片（&name[..15] 会 panic），
/// 用 chars().take() 保证安全。comm 截断到 15 字节后可能与其他进程前缀冲突
/// （如 com.lixiang.car.x.svm 和 com.lixiang.car.x.browser 都截为 com.lixiang.car），
/// 后插入者覆盖先者——GPU busy 可能错归因到同前缀的另一个 pid，这是数据源限制。
fn lookup_pid(pid_names: &HashMap<String, u32>, name: &str) -> Option<u32> {
    pid_names.get(name).copied().or_else(|| {
        let truncated: String = name.chars().take(15).collect();
        pid_names.get(&truncated).copied()
    })
}

/// 启动流式通道读线程（QNX/TopGpu/Ligfx）；kgsl/DumpMem 由主循环按节拍轮询。
pub(crate) fn start_stream_channels(gpu_path: &GpuPath, interval_ms: u64, pid_names: &Arc<Mutex<HashMap<String, u32>>>) {
    match gpu_path {
        GpuPath::Qnx => qnx::start(interval_ms, pid_names),
        GpuPath::TopGpu => topgpu::start(interval_ms, pid_names),
        GpuPath::Ligfx => ligfx::start(pid_names),
        GpuPath::Kgsl(_) | GpuPath::DumpMem => {}
    }
}

/// 保底/补采路径：dumpsys gpu Memory snapshot → 每 PID GPU 显存（限频由主循环控制）。
pub(crate) fn emit_gpumem(active_pids: &[u32], ts: u64) {
    if let Some((global, procs)) = dumpsys(&["gpu"]).as_deref().and_then(parse_gpu_mem_snapshot) {
        for &pid in active_pids {
            let bytes = procs.iter().find(|(p, _)| *p == pid).map(|(_, b)| *b).unwrap_or(0);
            emit(&format!(
                "{{\"t\":\"gpumem\",\"ts\":{},\"pid\":{},\"bytes\":{},\"global\":{}}}",
                ts, pid, bytes, global
            ));
        }
    }
}

/// 解析 dumpsys gpu 的 Memory snapshot 段。返回 None 表示无该段（设备不支持）。
fn parse_gpu_mem_snapshot(out: &str) -> Option<(u64, Vec<(u32, u64)>)> {
    if !out.contains("Memory snapshot") {
        return None;
    }
    let mut global = None;
    let mut procs = Vec::new();
    for line in out.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Global total:") {
            global = rest.trim().parse().ok();
        } else if let Some(rest) = line.strip_prefix("Proc ") {
            // "778 total: 628928512"
            if let Some((pid_s, rest2)) = rest.split_once(' ') {
                if let (Ok(pid), Some(bytes)) = (
                    pid_s.parse::<u32>(),
                    rest2.trim().strip_prefix("total:").and_then(|s| s.trim().parse::<u64>().ok()),
                ) {
                    procs.push((pid, bytes));
                }
            }
        }
    }
    Some((global?, procs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_gpu_mem_snapshot() {
        // 真机 dumpsys gpu 输出（hypervisor 平台）
        let out = "Stable Game Driver: unsupported\n\
                   Pre-release Game Driver: unsupported\n\n\
                   Memory snapshot for GPU 0:\n\
                   Global total: 2639089664\n\
                   Proc 778 total: 628928512\n\
                   Proc 29697 total: 154370048\n";
        let (global, procs) = parse_gpu_mem_snapshot(out).unwrap();
        assert_eq!(global, 2639089664);
        assert_eq!(procs, vec![(778, 628928512), (29697, 154370048)]);
        // 无 Memory snapshot 段 → None（该设备不支持降级路径）
        assert!(parse_gpu_mem_snapshot("garbage\n").is_none());
    }
}
