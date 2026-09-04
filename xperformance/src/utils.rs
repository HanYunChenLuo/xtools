use anyhow::Result;
use chrono::{DateTime, Local};
use plotters::element::PathElement;
use plotters::prelude::*;
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use xperf_core::ThreadCpuInfo;

/// CSV 时间戳格式（与历史导出文件一致，毫秒精度）
const CSV_TS_FMT: &str = "%Y-%m-%d %H:%M:%S%.3f";

// 存储当前执行期间的timestamp目录路径
static TIMESTAMP_DIR: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();

/// 校验包名格式（防止路径遍历）
pub fn validate_package_name(pkg: &str) -> Result<()> {
    if pkg.is_empty() || pkg.len() > 255 {
        anyhow::bail!("包名不能为空且不超过 255 字符: {}", pkg);
    }
    if !pkg.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-') {
        anyhow::bail!("包名包含非法字符: {}", pkg);
    }
    Ok(())
}

pub fn create_log_dir_if_needed(package: &str) -> Result<PathBuf> {
    validate_package_name(package)?;
    // 数据统一落 /tmp/xperf/<pkg>（跨 CLI/GUI 一处存放；/tmp 重启自清 + 可显式清理）
    let log_dir = data_root().join(package);
    if !log_dir.exists() {
        fs::create_dir_all(&log_dir)?;
        println!("Created log directory: {}", log_dir.display());
    }
    Ok(log_dir)
}

/// 采集数据根目录 `/tmp/xperf`（CLI 与 GUI 共用；见 CLAUDE.md「输出目录」）
pub fn data_root() -> PathBuf {
    std::env::temp_dir().join("xperf")
}

pub fn generate_cpu_chart(
    timestamps: &VecDeque<DateTime<Local>>,
    process_cpu: &VecDeque<f32>,
    pid: &str,
) -> Result<PathBuf> {
    if timestamps.is_empty() || process_cpu.is_empty() {
        return Err(anyhow::format_err!("No CPU data to chart"));
    }

    let temp_dir = std::env::temp_dir();
    let output_file = temp_dir.join(format!("cpu_{}_chart.png", pid));
    let output_file_clone = output_file.clone();

    let x_range = (*timestamps.front().unwrap())..(*timestamps.back().unwrap());
    let root = BitMapBackend::new(&output_file, (1920, 1080)).into_drawing_area();
    root.fill(&WHITE)?;

    let areas = root.split_evenly((1, 1));
    // 单核口径下 CPU% 可超过 100%（多线程）：纵轴下限固定 100，超出自动扩展
    let max_cpu = process_cpu.iter().cloned().fold(0f32, f32::max);
    let y_max = if max_cpu > 100.0 { max_cpu * 1.1 } else { 100.0 };
    let mut process_chart = ChartBuilder::on(&areas[0])
        .margin(15)
        .x_label_area_size(40)
        .y_label_area_size(60)
        .build_cartesian_2d(x_range.clone(), 0f32..y_max)?;

    let mut mesh_config = process_chart.configure_mesh();
    mesh_config
        .y_desc("Process CPU")
        .y_label_formatter(&|v| format!("{:.1}", v))
        .x_desc("Time")
        .x_labels(10)
        .x_label_formatter(&|x| x.format("%H:%M:%S").to_string());
    mesh_config.draw()?;

    let series = process_cpu
        .iter()
        .zip(timestamps.iter())
        .map(|(y, x)| (*x, *y));

    process_chart
        .draw_series(LineSeries::new(series, BLUE.stroke_width(2)))?
        .label(format!("Process CPU (PID: {})", pid))
        .legend(|(x, y)| PathElement::new(vec![(x, y), (x + 20, y)], BLUE.stroke_width(2)));

    process_chart
        .configure_series_labels()
        .background_style(WHITE.mix(0.8))
        .border_style(BLACK)
        .draw()?;

    Ok(output_file_clone)
}

/// 单个 PID 的 CPU 时序引用（pid, timestamps, process_cpu），用于汇总图。
pub type CpuSeriesRef<'a> = (String, &'a VecDeque<DateTime<Local>>, &'a VecDeque<f32>);

/// 生成多 PID CPU 汇总对比图：每个 PID 一条折线（不同颜色）。
pub fn generate_cpu_summary_chart(
    output_dir: &std::path::Path,
    package: &str,
    series: &[CpuSeriesRef<'_>],
) -> Result<PathBuf> {
    let path = output_dir.join("cpu_summary.png");
    let path_clone = path.clone();
    let root = BitMapBackend::new(&path, (1920, 1080)).into_drawing_area();
    root.fill(&WHITE)?;

    let mut min_time = None;
    let mut max_time = None;
    let mut max_cpu = 1.0f32;
    for (_, ts, cpu) in series {
        if let (Some(&t0), Some(&tn)) = (ts.front(), ts.back()) {
            min_time = Some(min_time.map_or(t0, |m: DateTime<Local>| m.min(t0)));
            max_time = Some(max_time.map_or(tn, |m: DateTime<Local>| m.max(tn)));
        }
        for &c in cpu.iter() {
            if c > max_cpu { max_cpu = c; }
        }
    }
    let min_time = min_time.ok_or_else(|| anyhow::format_err!("No CPU data for summary"))?;
    let max_time = max_time.unwrap_or(min_time);
    max_cpu *= 1.1;
    if max_cpu < 1.0 { max_cpu = 1.0; }

    let mut chart = ChartBuilder::on(&root)
        .caption(format!("{} - CPU Summary ({} PIDs)", package, series.len()), ("sans-serif", 22).into_font())
        .margin(15)
        .x_label_area_size(40)
        .y_label_area_size(60)
        .build_cartesian_2d(min_time..max_time, 0f32..max_cpu)?;

    chart.configure_mesh()
        .y_desc("Process CPU (%)")
        .x_desc("Time")
        .x_labels(10)
        .x_label_formatter(&|x| x.format("%H:%M:%S").to_string())
        .draw()?;

    let colors = [
        RGBColor(31, 119, 180), RGBColor(255, 127, 14), RGBColor(44, 160, 44),
        RGBColor(214, 39, 40), RGBColor(148, 103, 189), RGBColor(140, 86, 75),
        RGBColor(227, 119, 194), RGBColor(127, 127, 127), RGBColor(188, 189, 34),
        RGBColor(23, 190, 207),
    ];

    for (i, (pid, ts, cpu)) in series.iter().enumerate() {
        let color = colors[i % colors.len()];
        let pts: Vec<(DateTime<Local>, f32)> = ts.iter().zip(cpu.iter()).map(|(t, &c)| (*t, c)).collect();
        chart.draw_series(LineSeries::new(pts, color.stroke_width(2)))?
            .label(format!("PID {}", pid))
            .legend(move |(x, y)| PathElement::new(vec![(x, y), (x + 20, y)], color.stroke_width(2)));
    }
    chart.configure_series_labels()
        .background_style(WHITE.mix(0.8))
        .border_style(BLACK)
        .draw()?;

    root.present()?;
    Ok(path_clone)
}

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
        std::fs::create_dir_all(&timestamp_dir)?;
        println!("Created timestamp directory: {}", timestamp_dir.display());
    }

    *guard = Some(timestamp_dir.clone());
    Ok(timestamp_dir)
}

pub fn generate_thread_time_series_chart(
    path: PathBuf,
    package: &str,
    pid: &str,
    thread_data: &std::collections::HashMap<String, Vec<ThreadCpuInfo>>,
) -> Result<String> {
    if thread_data.is_empty() {
        return Ok(String::new());
    }

    let active_threads: std::collections::HashMap<String, Vec<ThreadCpuInfo>> = thread_data
        .iter()
        .filter_map(|(tid, threads)| {
            let active_points: Vec<ThreadCpuInfo> =
                threads.iter().filter(|t| t.cpu_usage > 0.0).cloned().collect();
            if !active_points.is_empty() {
                Some((tid.clone(), active_points))
            } else {
                None
            }
        })
        .collect();

    if active_threads.is_empty() {
        return Ok(String::new());
    }

    let timestamp_str = Local::now().format("%Y%m%d_%H%M%S").to_string();
    let chart_filename = format!("thread_time_series_{}_pid{}.png", timestamp_str, pid);
    let filepath = path.join(&chart_filename);

    let root = BitMapBackend::new(&filepath, (1920, 1080)).into_drawing_area();
    root.fill(&WHITE)?;

    let title = format!("Thread CPU Time Series - {} (PID: {})", package, pid);
    let colors = [
        &RED, &BLUE, &GREEN, &YELLOW, &MAGENTA, &CYAN,
        &RGBColor(128, 0, 0), &RGBColor(0, 128, 0), &RGBColor(0, 0, 128),
        &RGBColor(128, 128, 0), &RGBColor(128, 0, 128), &RGBColor(0, 128, 128),
    ];

    let (title_area, chart_area) = root.split_vertically(50);
    title_area.titled(&title, ("sans-serif", 20))?;

    let mut min_time = chrono::Local::now();
    let mut max_time = chrono::Local::now() - chrono::Duration::hours(1);
    let mut max_cpu = 0.1f32;
    for thread_points in active_threads.values() {
        for point in thread_points {
            if let Some(timestamp) = point.timestamp {
                if timestamp < min_time { min_time = timestamp; }
                if timestamp > max_time { max_time = timestamp; }
                if point.cpu_usage > max_cpu { max_cpu = point.cpu_usage; }
            }
        }
    }
    if max_time <= min_time {
        max_time = min_time + chrono::Duration::minutes(5);
    }
    max_cpu *= 1.1;
    if max_cpu < 1.0 { max_cpu = 1.0; }

    let mut chart = ChartBuilder::on(&chart_area)
        .margin(10)
        .x_label_area_size(40)
        .y_label_area_size(60)
        .build_cartesian_2d(min_time..max_time, 0f32..max_cpu)?;

    chart.configure_mesh()
        .x_labels(8)
        .x_label_formatter(&|x| x.format("%H:%M:%S").to_string())
        .y_desc("CPU Usage (%)")
        .x_desc("Time")
        .draw()?;

    let mut legend_entries = Vec::new();
    for (idx, (tid, thread_points)) in active_threads.iter().enumerate().take(12) {
        if thread_points.is_empty() || thread_points[0].timestamp.is_none() {
            continue;
        }
        let thread_name = thread_points[0].name.clone();
        let legend_name = format!("{} ({})", thread_name, tid);
        let color = *colors[idx % colors.len()];
        let line_data: Vec<(DateTime<Local>, f32)> = thread_points
            .iter()
            .filter_map(|p| p.timestamp.map(|ts| (ts, p.cpu_usage)))
            .collect();
        chart.draw_series(LineSeries::new(line_data, color))?
            .label(legend_name.clone())
            .legend(move |(x, y)| PathElement::new(vec![(x, y), (x + 20, y)], color));
        legend_entries.push((legend_name, color));
    }

    if !legend_entries.is_empty() {
        chart.configure_series_labels()
            .background_style(WHITE.mix(0.8))
            .border_style(BLACK)
            .position(SeriesLabelPosition::UpperRight)
            .margin(10)
            .draw()?;
    }

    root.present()?;
    Ok(chart_filename)
}

// ---------- 边采边落盘（流式 CSV）----------

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
    root: Option<PathBuf>, // 时间戳根目录，首个样本时创建
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

/// 懒打开目标文件（首次写表头）并追加一行 + flush。
/// map 按 key 每文件一个 writer；root 为会话时间戳目录（首个样本时创建）。
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
                let mut w = BufWriter::new(fs::File::create(dir.join(filename))?);
                writeln!(w, "{}", header)?;
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
    pub fn cpu_row(&mut self, pkg: &str, pid: u32, t: DateTime<Local>, cpu: f32) {
        let row = format!("{},{:.2}", t.format(CSV_TS_FMT), cpu);
        stream_write(
            &mut self.broken, &mut self.root, pkg, &mut self.cpu, pid,
            "cpu", format!("cpu_{}_data.csv", pid), "Timestamp,Process CPU (%)", &row,
        );
    }

    pub fn mem_row(&mut self, pkg: &str, pid: u32, t: DateTime<Local>, d: &xperf_core::MemoryDetails) {
        let row = format!(
            "{},{},{},{},{},{},{},{},{}",
            t.format(CSV_TS_FMT), d.total_pss, d.java_heap, d.native_heap,
            d.code, d.stack, d.graphics, d.private_other, d.system
        );
        stream_write(
            &mut self.broken, &mut self.root, pkg, &mut self.mem, pid,
            "memory", format!("memory_{}_data.csv", pid),
            "Timestamp,Total PSS,Java Heap,Native Heap,Code,Stack,Graphics,Private Other,System", &row,
        );
    }

    pub fn fps_row(&mut self, pkg: &str, pid: u32, t: DateTime<Local>, layer: &str, fps: f32, jank: u32) {
        let row = format!("{},{:.2},{},{}", t.format(CSV_TS_FMT), fps, jank, csv_escape(layer));
        stream_write(
            &mut self.broken, &mut self.root, pkg, &mut self.fps, pid,
            "fps", format!("{}_fps_data_pid{}.csv", pkg, pid), "Timestamp,FPS,Jank,Layer", &row,
        );
    }

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

    /// 温度长格式：每传感器一行（Timestamp,Status,Sensor,TempC）
    pub fn temp_row(&mut self, pkg: &str, t: DateTime<Local>, status: i32, sensors: &[(String, i32, f32)]) {
        for (name, _, value) in sensors {
            let row = format!("{},{},{},{:.1}", t.format(CSV_TS_FMT), status, csv_escape(name), value);
            stream_write(
                &mut self.broken, &mut self.root, pkg, &mut self.misc, "thermal",
                "thermal", "thermal_data.csv".to_string(), "Timestamp,Status,Sensor,TempC", &row,
            );
        }
    }

    pub fn gpu_row(&mut self, pkg: &str, t: DateTime<Local>, busy: f32, util: f32, mhz: u32, maxmhz: u32) {
        let row = format!("{},{:.2},{:.2},{},{}", t.format(CSV_TS_FMT), busy, util, mhz, maxmhz);
        stream_write(
            &mut self.broken, &mut self.root, pkg, &mut self.misc, "gpu",
            "gpu", "gpu_data.csv".to_string(), "Timestamp,Busy (%),Util (%),Clock (MHz),Max Clock (MHz)", &row,
        );
    }

    /// QNX 路径每进程 GPU busy
    pub fn gpuproc_row(&mut self, pkg: &str, pid: u32, t: DateTime<Local>, busy: f32) {
        let row = format!("{},{:.2}", t.format(CSV_TS_FMT), busy);
        stream_write(
            &mut self.broken, &mut self.root, pkg, &mut self.gpuproc, pid,
            "gpu", format!("gpu_proc_{}_data.csv", pid), "Timestamp,Busy (%)", &row,
        );
    }

    #[allow(clippy::too_many_arguments)] // r/w/dr/dw 四个速率是协议字段，不再包结构体
    pub fn io_row(&mut self, pkg: &str, pid: u32, t: DateTime<Local>, r: f32, w: f32, dr: f32, dw: f32) {
        let row = format!("{},{:.2},{:.2},{:.2},{:.2}", t.format(CSV_TS_FMT), r, w, dr, dw);
        stream_write(
            &mut self.broken, &mut self.root, pkg, &mut self.io, pid,
            "io", format!("io_{}_data.csv", pid),
            "Timestamp,Read (KB/s),Write (KB/s),Disk Read (KB/s),Disk Write (KB/s)", &row,
        );
    }

    pub fn net_row(&mut self, pkg: &str, t: DateTime<Local>, rx: f32, tx: f32) {
        let row = format!("{},{:.2},{:.2}", t.format(CSV_TS_FMT), rx, tx);
        stream_write(
            &mut self.broken, &mut self.root, pkg, &mut self.misc, "net",
            "net", "net_data.csv".to_string(), "Timestamp,RX (KB/s),TX (KB/s)", &row,
        );
    }

    /// GPU 显存（--gpu 降级路径）：每 PID 一个文件，列为进程 MB 与整机 MB
    pub fn gpumem_row(&mut self, pkg: &str, pid: u32, t: DateTime<Local>, mb: f32, global_mb: f32) {
        let row = format!("{},{:.1},{:.0}", t.format(CSV_TS_FMT), mb, global_mb);
        stream_write(
            &mut self.broken, &mut self.root, pkg, &mut self.gpumem, pid,
            "gpumem", format!("gpumem_{}_data.csv", pid),
            "Timestamp,Process GPU Mem (MB),Global GPU Mem (MB)", &row,
        );
    }
}

// ---------- B 类指标时序（退出图表用）----------

/// 单条序列的点集：(时间, 值)
pub type SeriesPoints = Vec<(DateTime<Local>, f32)>;
/// 一条命名序列（图例名 + 点集）
pub type NamedSeries = (String, SeriesPoints);
/// 温度传感器读数：(名称, 类型, °C)
pub type SensorReading = (String, i32, f32);
/// 温度样本：(ts, thermal status, [(sensor, °C)])
pub type TempSample = (DateTime<Local>, i32, Vec<(String, f32)>);
/// IO 样本：(ts, r, w, dr, dw) KB/s
pub type IoSample = (DateTime<Local>, f32, f32, f32, f32);

/// 设备级/IO 指标的时序容器：样本到达即追加，超 2×CHART_SERIES_CAP 每 2 取 1 抽稀。
#[derive(Default)]
pub struct ExtraSeries {
    /// (ts, 每核 MHz)
    pub freq: VecDeque<(DateTime<Local>, Vec<f32>)>,
    /// 温度样本序列
    pub temp: VecDeque<TempSample>,
    /// (ts, busy%, util%, clock MHz)（util 仅 QNX 路径有值，kgsl 路径为 0）
    pub gpu: VecDeque<(DateTime<Local>, f32, f32, u32)>,
    /// pid → (ts, busy%)（QNX 路径每进程 GPU busy）
    pub gpuproc: HashMap<u32, VecDeque<(DateTime<Local>, f32)>>,
    /// pid → IO 样本序列
    pub io: HashMap<u32, VecDeque<IoSample>>,
    /// (ts, rx, tx) KB/s（整机口径）
    pub net: VecDeque<(DateTime<Local>, f32, f32)>,
    /// pid → (ts, 进程 MB, 整机 MB)（--gpu 降级路径，hypervisor 平台）
    pub gpumem: HashMap<u32, VecDeque<(DateTime<Local>, f32, f32)>>,
}

impl ExtraSeries {
    fn push_capped<T>(dq: &mut VecDeque<T>, v: T) {
        if dq.len() >= 2 * xperf_core::CHART_SERIES_CAP {
            xperf_core::decimate(dq);
        }
        dq.push_back(v);
    }

    pub fn push_freq(&mut self, t: DateTime<Local>, mhz: Vec<f32>) {
        Self::push_capped(&mut self.freq, (t, mhz));
    }

    pub fn push_temp(&mut self, t: DateTime<Local>, status: i32, sensors: &[(String, i32, f32)]) {
        let sensors: Vec<(String, f32)> = sensors.iter().map(|(n, _, v)| (n.clone(), *v)).collect();
        Self::push_capped(&mut self.temp, (t, status, sensors));
    }

    pub fn push_gpu(&mut self, t: DateTime<Local>, busy: f32, util: f32, mhz: u32) {
        Self::push_capped(&mut self.gpu, (t, busy, util, mhz));
    }

    pub fn push_gpuproc(&mut self, pid: u32, t: DateTime<Local>, busy: f32) {
        Self::push_capped(self.gpuproc.entry(pid).or_default(), (t, busy));
    }

    pub fn push_io(&mut self, pid: u32, t: DateTime<Local>, r: f32, w: f32, dr: f32, dw: f32) {
        Self::push_capped(self.io.entry(pid).or_default(), (t, r, w, dr, dw));
    }

    pub fn push_net(&mut self, t: DateTime<Local>, rx: f32, tx: f32) {
        Self::push_capped(&mut self.net, (t, rx, tx));
    }

    pub fn push_gpumem(&mut self, pid: u32, t: DateTime<Local>, mb: f32, global_mb: f32) {
        Self::push_capped(self.gpumem.entry(pid).or_default(), (t, mb, global_mb));
    }
}

/// 通用多序列折线图：B 类指标（freq/temp/gpu/io/net）退出图表共用。
/// 每条序列 (名称, [(时间, 值)])；自适应纵轴。
pub fn generate_multi_line_chart(
    path: &PathBuf,
    title: &str,
    y_desc: &str,
    series: &[NamedSeries],
) -> Result<PathBuf> {
    let series: Vec<&NamedSeries> = series.iter().filter(|(_, pts)| !pts.is_empty()).collect();
    if series.is_empty() {
        return Err(anyhow::format_err!("No data to chart"));
    }

    let mut min_time = None;
    let mut max_time = None;
    let mut max_v = 1.0f32;
    for (_, pts) in &series {
        min_time = Some(min_time.map_or(pts[0].0, |m: DateTime<Local>| m.min(pts[0].0)));
        max_time = Some(max_time.map_or(pts[pts.len() - 1].0, |m: DateTime<Local>| m.max(pts[pts.len() - 1].0)));
        for (_, v) in pts {
            if *v > max_v {
                max_v = *v;
            }
        }
    }
    let min_time = min_time.unwrap();
    let max_time = max_time.unwrap();
    let max_time = if max_time <= min_time { min_time + chrono::Duration::minutes(1) } else { max_time };
    max_v *= 1.1;

    let path_clone = path.clone();
    let root = BitMapBackend::new(path, (1920, 1080)).into_drawing_area();
    root.fill(&WHITE)?;

    let (title_area, chart_area) = root.split_vertically(50);
    title_area.titled(title, ("sans-serif", 20))?;

    let mut chart = ChartBuilder::on(&chart_area)
        .margin(10)
        .margin_right(35)
        .x_label_area_size(40)
        .y_label_area_size(60)
        .build_cartesian_2d(min_time..max_time, 0f32..max_v)?;

    chart
        .configure_mesh()
        .x_labels(8)
        .x_label_formatter(&|x| x.format("%H:%M:%S").to_string())
        .y_desc(y_desc)
        .x_desc("Time")
        .draw()?;

    let colors = [
        RGBColor(31, 119, 180), RGBColor(255, 127, 14), RGBColor(44, 160, 44),
        RGBColor(214, 39, 40), RGBColor(148, 103, 189), RGBColor(140, 86, 75),
        RGBColor(227, 119, 194), RGBColor(127, 127, 127), RGBColor(188, 189, 34),
        RGBColor(23, 190, 207),
    ];

    for (i, (name, pts)) in series.iter().enumerate() {
        let color = colors[i % colors.len()];
        chart
            .draw_series(LineSeries::new(pts.iter().copied(), color.stroke_width(2)))?
            .label(name.clone())
            .legend(move |(x, y)| PathElement::new(vec![(x, y), (x + 20, y)], color.stroke_width(2)));
    }
    chart
        .configure_series_labels()
        .background_style(WHITE.mix(0.8))
        .border_style(BLACK)
        .position(SeriesLabelPosition::UpperRight)
        .margin(10)
        .draw()?;

    root.present()?;
    Ok(path_clone)
}
