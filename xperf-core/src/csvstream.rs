//! 流式 CSV 落盘（边采边写）：样本到达即追加写入对应 CSV 并 flush，
//! 进程崩溃只丢未 flush 的尾部，而不是全部数据。
//!
//! CLI 与 GUI 共用：CLI 用 `CsvStream::default()`（根目录走进程级
//! `create_timestamp_subdir` 全局缓存，一次运行一个目录）；GUI 多设备并行，
//! 用 `CsvStream::with_root` 给每个采样会话显式指定目录
//! （`<pkg>/<ts>-<serial>`，serial 后缀防双设备同秒撞名）。

use anyhow::Result;
use chrono::{DateTime, Local};
use std::collections::HashMap;
use std::fs;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// CSV 时间戳格式（毫秒精度，与历史导出文件一致）
const CSV_TS_FMT: &str = "%Y-%m-%d %H:%M:%S%.3f";

/// 本次运行的落盘时间戳目录（进程级缓存：CLI 一次运行一个目录；
/// GUI 不经此全局——多设备并行各会话独立目录，见 [`CsvStream::with_root`]）
static TIMESTAMP_DIR: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();

/// 校验包名格式（防止路径遍历；包名会拼入落盘目录路径）
pub fn validate_package_name(pkg: &str) -> Result<()> {
    if pkg.is_empty() || pkg.len() > 255 {
        anyhow::bail!("包名不能为空且不超过 255 字符: {}", pkg);
    }
    if !pkg.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-') {
        anyhow::bail!("包名包含非法字符: {}", pkg);
    }
    Ok(())
}

/// 采集数据根目录（`/tmp/xperf`，CLI 与 GUI 共用；/tmp 重启自清 + 可显式清理）
pub fn data_root() -> PathBuf {
    std::env::temp_dir().join("xperf")
}

/// 创建包的日志根目录（`<data_root>/<pkg>`，幂等）
pub fn create_log_dir_if_needed(package: &str) -> Result<PathBuf> {
    validate_package_name(package)?;
    let log_dir = data_root().join(package);
    if !log_dir.exists() {
        fs::create_dir_all(&log_dir)?;
        println!("Created log directory: {}", log_dir.display());
    }
    Ok(log_dir)
}

/// 本次运行的时间戳子目录（`<data_root>/<pkg>/<ts>`）：进程级 OnceLock 缓存，
/// 整个 CLI 会话只创建一个时间戳目录（首个样本落盘时创建并缓存）。
/// CLI 专用；GUI 多会话场景用 [`CsvStream::with_root`] 显式指定。
pub fn create_timestamp_subdir(package: &str) -> Result<PathBuf> {
    let cell = TIMESTAMP_DIR.get_or_init(|| Mutex::new(None));
    let mut guard = cell.lock().unwrap();

    if let Some(ref dir) = *guard {
        return Ok(dir.clone());
    }

    let log_dir = create_log_dir_if_needed(package)?;
    let timestamp_str = Local::now().format("%Y%m%d_%H%M%S").to_string();
    let timestamp_dir = log_dir.join(&timestamp_str);

    if !timestamp_dir.exists() {
        fs::create_dir_all(&timestamp_dir)?;
        println!("Created timestamp directory: {}", timestamp_dir.display());
    }

    *guard = Some(timestamp_dir.clone());
    Ok(timestamp_dir)
}

/// CSV 字段转义：含逗号/引号/换行的字段加双引号并转义内嵌引号
pub fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// 样本到达即追加写入对应 CSV：进程崩溃只丢未 flush 的尾部，而不是全部数据。
/// 文件名与历史退出导出命名一致；退出阶段不再重写 CSV，只生成图表。
#[derive(Default)]
pub struct CsvStream {
    root: Option<PathBuf>, // 时间戳根目录：default() 走全局懒创建，with_root() 为显式指定
    cpu: HashMap<u32, BufWriter<fs::File>>,
    mem: HashMap<u32, BufWriter<fs::File>>,
    fps: HashMap<u32, BufWriter<fs::File>>,
    thread: HashMap<(u32, u32), BufWriter<fs::File>>, // (pid, tid)
    io: HashMap<u32, BufWriter<fs::File>>,            // pid
    gpumem: HashMap<u32, BufWriter<fs::File>>,        // pid
    gpuproc: HashMap<u32, BufWriter<fs::File>>,       // pid
    misc: HashMap<&'static str, BufWriter<fs::File>>, // freq/thermal/gpu/net（设备级单文件，按指标名一个 writer）
    broken: bool, // 写盘失败后停用（防逐行刷屏告警）
}

impl CsvStream {
    /// 显式指定落盘根目录（GUI 每会话独立目录：`<pkg>/<ts>-<serial>`）。
    /// 目录在首个样本写入时才创建（无数据不落空目录）。
    pub fn with_root(root: PathBuf) -> Self {
        Self {
            root: Some(root),
            ..Default::default()
        }
    }
}

/// 懒打开目标文件（文件不存在或为空时写表头）并追加一行 + flush。
/// map 按 key 每文件一个 writer；root 为会话时间戳目录（首个样本时创建）。
/// append 模式：GUI 同包重启采样复用目录时续写前文（CLI 每运行新目录，行为不变）。
#[allow(clippy::too_many_arguments)]
fn stream_write<K: Eq + std::hash::Hash>(
    broken: &mut bool,
    root: &mut Option<PathBuf>,
    pkg: &str,
    map: &mut HashMap<K, BufWriter<fs::File>>,
    key: K,
    subdir: &str,
    filename: String,
    header: &str,
    row: &str,
) {
    if *broken {
        return;
    }
    let result = (|| -> Result<()> {
        let w = match map.entry(key) {
            std::collections::hash_map::Entry::Occupied(o) => o.into_mut(),
            std::collections::hash_map::Entry::Vacant(v) => {
                if root.is_none() {
                    *root = Some(create_timestamp_subdir(pkg)?);
                }
                let dir = root.as_ref().expect("root just set").join(subdir);
                fs::create_dir_all(&dir)?;
                // append 模式（不截断）：CLI 每次运行新目录，文件必然不存在，与旧
                // File::create 行为一致；GUI 同包重启采样复用目录时续写不丢前文
                let path = dir.join(filename);
                let need_header =
                    !path.exists() || fs::metadata(&path).map(|m| m.len() == 0).unwrap_or(true);
                let mut w =
                    BufWriter::new(fs::OpenOptions::new().create(true).append(true).open(path)?);
                if need_header {
                    writeln!(w, "{}", header)?;
                }
                v.insert(w)
            }
        };
        writeln!(w, "{}", row)?;
        w.flush()?; // 每行 flush：行体小、频率低（≤20 行/s/PID），崩溃丢尾最小化
        Ok(())
    })();
    if let Err(e) = result {
        *broken = true;
        eprintln!("CSV 流式落盘失败，后续样本不再写盘: {}", e);
    }
}

impl CsvStream {
    /// CPU 行：`Timestamp,Process CPU (%)`
    pub fn cpu_row(&mut self, pkg: &str, pid: u32, t: DateTime<Local>, cpu: f32) {
        let row = format!("{},{:.2}", t.format(CSV_TS_FMT), cpu);
        stream_write(
            &mut self.broken, &mut self.root, pkg, &mut self.cpu, pid,
            "cpu", format!("cpu_{}_data.csv", pid), "Timestamp,Process CPU (%)", &row,
        );
    }

    /// 内存 CSV 行（MB，单位统一口径：展示/图表/CSV 全 MB；协议与基线 JSON 存储仍 KB）。
    /// DMA-BUF 列（协议 v8 起）已从 Other 扣减单列；低间隔/非 root 路径该列为 0.0
    pub fn mem_row(&mut self, pkg: &str, pid: u32, t: DateTime<Local>, d: &crate::MemoryDetails) {
        let row = format!(
            "{},{:.1},{:.1},{:.1},{:.1},{:.1},{:.1},{:.1},{:.1},{:.1}",
            t.format(CSV_TS_FMT),
            d.total_pss as f64 / 1024.0,
            d.java_heap as f64 / 1024.0,
            d.native_heap as f64 / 1024.0,
            d.code as f64 / 1024.0,
            d.stack as f64 / 1024.0,
            d.graphics as f64 / 1024.0,
            d.dmabuf as f64 / 1024.0,
            d.private_other as f64 / 1024.0,
            d.system as f64 / 1024.0
        );
        stream_write(
            &mut self.broken, &mut self.root, pkg, &mut self.mem, pid,
            "memory", format!("memory_{}_data.csv", pid),
            "Timestamp,Total PSS (MB),Java Heap (MB),Native Heap (MB),Code (MB),Stack (MB),Graphics (MB),DMA-BUF (MB),Other (MB),System (MB)", &row,
        );
    }

    /// FPS 行：`Timestamp,FPS,Jank,Layer`
    pub fn fps_row(&mut self, pkg: &str, pid: u32, t: DateTime<Local>, layer: &str, fps: f32, jank: u32) {
        let row = format!("{},{:.2},{},{}", t.format(CSV_TS_FMT), fps, jank, csv_escape(layer));
        stream_write(
            &mut self.broken, &mut self.root, pkg, &mut self.fps, pid,
            "fps", format!("{}_fps_data_pid{}.csv", pkg, pid), "Timestamp,FPS,Jank,Layer", &row,
        );
    }

    /// 线程行（CLI `--thread` 用）：`Timestamp,CPUUsage`
    pub fn thread_row(&mut self, pkg: &str, pid: u32, tid: u32, name: &str, t: DateTime<Local>, cpu: f32) {
        let row = format!("{},{}", t.format(CSV_TS_FMT), cpu);
        let sanitized = csv_escape(&name.replace(' ', "_").replace('/', "-"));
        stream_write(
            &mut self.broken, &mut self.root, pkg, &mut self.thread, (pid, tid),
            "thread", format!("thread_{}_{}_{}.csv", sanitized, tid, pid), "Timestamp,CPUUsage", &row,
        );
    }

    /// 每核频率一行（MHz），表头列数由首个样本核数决定
    pub fn freq_row(&mut self, pkg: &str, t: DateTime<Local>, mhz: &[f32]) {
        let header = format!(
            "Timestamp,{}",
            (0..mhz.len()).map(|i| format!("cpu{} (MHz)", i)).collect::<Vec<_>>().join(",")
        );
        let row = format!(
            "{},{}",
            t.format(CSV_TS_FMT),
            mhz.iter().map(|m| format!("{:.0}", m)).collect::<Vec<_>>().join(",")
        );
        stream_write(
            &mut self.broken, &mut self.root, pkg, &mut self.misc, "freq",
            "freq", "freq_data.csv".to_string(), &header, &row,
        );
    }

    /// 温度长格式：每传感器一行（`Timestamp,Status,Sensor,TempC`）
    pub fn temp_row(&mut self, pkg: &str, t: DateTime<Local>, status: i32, sensors: &[(String, i32, f32)]) {
        for (name, _, value) in sensors {
            let row = format!("{},{},{},{:.1}", t.format(CSV_TS_FMT), status, csv_escape(name), value);
            stream_write(
                &mut self.broken, &mut self.root, pkg, &mut self.misc, "thermal",
                "thermal", "thermal_data.csv".to_string(), "Timestamp,Status,Sensor,TempC", &row,
            );
        }
    }

    /// GPU 行：`Timestamp,Busy (%),Util (%),Clock (MHz),Max Clock (MHz)`
    pub fn gpu_row(&mut self, pkg: &str, t: DateTime<Local>, busy: f32, util: f32, mhz: u32, maxmhz: u32) {
        let row = format!("{},{:.2},{:.2},{},{}", t.format(CSV_TS_FMT), busy, util, mhz, maxmhz);
        stream_write(
            &mut self.broken, &mut self.root, pkg, &mut self.misc, "gpu",
            "gpu", "gpu_data.csv".to_string(), "Timestamp,Busy (%),Util (%),Clock (MHz),Max Clock (MHz)", &row,
        );
    }

    /// QNX 路径每进程 GPU busy 行
    pub fn gpuproc_row(&mut self, pkg: &str, pid: u32, t: DateTime<Local>, busy: f32) {
        let row = format!("{},{:.2}", t.format(CSV_TS_FMT), busy);
        stream_write(
            &mut self.broken, &mut self.root, pkg, &mut self.gpuproc, pid,
            "gpu", format!("gpu_proc_{}_data.csv", pid), "Timestamp,Busy (%)", &row,
        );
    }

    /// IO 行（KB/s）：`Timestamp,Read,Write,Disk Read,Disk Write`
    #[allow(clippy::too_many_arguments)] // r/w/dr/dw 四个速率是协议字段，不再包结构体
    pub fn io_row(&mut self, pkg: &str, pid: u32, t: DateTime<Local>, r: f32, w: f32, dr: f32, dw: f32) {
        let row = format!("{},{:.2},{:.2},{:.2},{:.2}", t.format(CSV_TS_FMT), r, w, dr, dw);
        stream_write(
            &mut self.broken, &mut self.root, pkg, &mut self.io, pid,
            "io", format!("io_{}_data.csv", pid),
            "Timestamp,Read (KB/s),Write (KB/s),Disk Read (KB/s),Disk Write (KB/s)", &row,
        );
    }

    /// 网络行（整机 KB/s）：`Timestamp,RX (KB/s),TX (KB/s)`
    pub fn net_row(&mut self, pkg: &str, t: DateTime<Local>, rx: f32, tx: f32) {
        let row = format!("{},{:.2},{:.2}", t.format(CSV_TS_FMT), rx, tx);
        stream_write(
            &mut self.broken, &mut self.root, pkg, &mut self.misc, "net",
            "net", "net_data.csv".to_string(), "Timestamp,RX (KB/s),TX (KB/s)", &row,
        );
    }

    /// GPU 显存行（`--gpu` 降级路径）：每 PID 一个文件，列为进程 MB 与整机 MB
    pub fn gpumem_row(&mut self, pkg: &str, pid: u32, t: DateTime<Local>, mb: f32, global_mb: f32) {
        let row = format!("{},{:.1},{:.0}", t.format(CSV_TS_FMT), mb, global_mb);
        stream_write(
            &mut self.broken, &mut self.root, pkg, &mut self.gpumem, pid,
            "gpumem", format!("gpumem_{}_data.csv", pid),
            "Timestamp,Process GPU Mem (MB),Global GPU Mem (MB)", &row,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// append 模式：同目录两个 CsvStream 实例（GUI 同包重启采样场景）续写同一文件，
    /// 表头只写一次、数据不截断
    #[test]
    fn test_with_root_append_continuity() {
        let dir = std::env::temp_dir().join(format!("xperf_csvtest_{}", std::process::id()));
        let t = Local::now();
        {
            let mut csv = CsvStream::with_root(dir.clone());
            csv.cpu_row("com.test", 100, t, 10.5);
        }
        {
            let mut csv2 = CsvStream::with_root(dir.clone());
            csv2.cpu_row("com.test", 100, t, 20.5);
        }
        let content = std::fs::read_to_string(dir.join("cpu/cpu_100_data.csv")).unwrap();
        assert_eq!(content.matches("Timestamp,Process CPU (%)").count(), 1, "表头只写一次");
        assert!(content.contains(",10.50\n"), "前文保留");
        assert!(content.contains(",20.50\n"), "新行追加");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// with_root 在无样本时不建目录（懒创建）
    #[test]
    fn test_with_root_lazy_dir() {
        let dir = std::env::temp_dir().join(format!("xperf_csvtest_lazy_{}", std::process::id()));
        let _csv = CsvStream::with_root(dir.clone());
        assert!(!dir.exists(), "无样本不落空目录");
    }

    /// mem_row：表头与数据行列数一致（10 列，v8 起含 DMA-BUF），
    /// DMA-BUF 列在 Graphics 与 Other 之间；锁表头/行对齐防回归
    #[test]
    fn test_mem_row_dmabuf_column() {
        let dir = std::env::temp_dir().join(format!("xperf_csvtest_mem_{}", std::process::id()));
        let d = crate::MemoryDetails {
            java_heap: 1024, native_heap: 2048, code: 3072, stack: 4096,
            graphics: 5120, dmabuf: 6144, private_other: 7168, system: 8192,
            total_pss: 10240,
        };
        {
            let mut csv = CsvStream::with_root(dir.clone());
            csv.mem_row("com.test", 100, Local::now(), &d);
        }
        let content = std::fs::read_to_string(dir.join("memory/memory_100_data.csv")).unwrap();
        let mut lines = content.lines();
        let header = lines.next().unwrap();
        let row = lines.next().unwrap();
        assert_eq!(header.matches(',').count(), row.matches(',').count(), "表头/数据列数不一致");
        assert!(header.contains("Graphics (MB),DMA-BUF (MB),Other (MB)"), "DMA-BUF 列位置: {}", header);
        // 行值：Timestamp + 9 个 MB 值（KB/1024）：dmabuf 6144KB = 6.0MB
        let cols: Vec<&str> = row.split(',').collect();
        assert_eq!(cols.len(), 10);
        assert_eq!(cols[7], "6.0", "DMA-BUF 列值（Graphics 后）");
        assert_eq!(cols[8], "7.0", "Other 列值");
        std::fs::remove_dir_all(&dir).ok();
    }
}
