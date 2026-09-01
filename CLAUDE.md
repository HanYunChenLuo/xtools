# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
# Build all tools (dev)
cargo build --workspace

# Build release binaries
cargo build --release --workspace

# Run tests
cargo test --workspace

# Run tests for a single crate
cargo test -p xperformance
cargo test -p xrm

# Run a specific test
cargo test -p xrm tests::test_dangerous_operation_detection

# Check for errors without building
cargo check --workspace
```

Release binaries are written to `target/release/`.

---

## xperformance 设计结构

### 整体架构

```
main()
 └─ monitor_process()          ← 主控函数，async
     ├─ tokio::spawn            ← 独立任务：ADB 连接守护
     │   └─ monitor_adb_connection()
     └─ 采样主循环（while running）
         ├─ cpu::sample_cpu()
         └─ memory::sample_memory()
```

程序入口 `main()` 直接调用 `monitor_process()`，无其他初始化逻辑。

---

### 并发模型

使用 `tokio` 单线程异步运行时（`#[tokio::main]`）：

- **主任务**：`monitor_process()` 主循环，顺序执行 CPU 采样 → 内存采样 → 图表触发检查
- **后台任务**：`tokio::spawn(monitor_adb_connection(...))` 每秒轮询 `adb devices`，ADB 断开时通过共享 `Arc<AtomicBool>` 将 `running` 置为 false，使主循环退出
- **信号处理**：`ctrlc::set_handler` 注册 Ctrl-C 回调，同样置 `running=false` 并调用 `utils::set_interrupt_flag()`

两个共享状态：
- `Arc<AtomicBool> running`：主循环退出标志，主任务和 ADB 守护任务共享
- `static AtomicBool INTERRUPT_FLAG`（在 utils.rs）：区分"用户主动中断"与"ADB 断开"，用于 CPU 采样错误时的日志过滤

---

### 采样时序机制

```
start_time (Instant)
sample_count: u64 = 0

每轮循环:
  should_be_at_sample = elapsed_secs / interval
  if should_be_at_sample > sample_count:
    跳帧数 = should_be_at_sample - sample_count - 1
    sample_count = should_be_at_sample
  执行本轮采样
```

**设计意图**：`sample_cpu` 内部包含 `sleep(interval_ms)`，这是两次 `/proc` 读取之间的等待窗口，也自然充当了循环节拍。程序通过绝对时间基准（`Instant`）检测漂移，丢弃已过期的采样点，保持时间序列的准确性。出现"System too slow! Skipping N sample(s)"说明 ADB 读写加上 sleep 的总耗时超过了 `interval` 设定值（通常是 ADB 延迟过高）。

循环内**不含额外 sleep**——节拍由 `sample_cpu` 内部的 sleep 提供。`-i` 参数单位为**毫秒**（默认 1000），支持亚秒级采样（如 `-i 500`）。

---

### 数据结构

```rust
PeakStats                          // 全局统计，贯穿整个监控会话
├── cpu_usage: f32                 // 峰值 CPU %
├── cpu_time: DateTime<Local>      // 峰值时间点
├── memory_usage: u64              // 峰值内存 KB
├── memory_time: DateTime<Local>
├── restart_count: u32             // 进程重启次数
├── cpu_data: CpuTimeSeriesData    // CPU 时序（无上限，持续追加）
│   ├── timestamps: VecDeque<DateTime<Local>>
│   ├── process_cpu: VecDeque<f32>
│   └── top_threads: VecDeque<Vec<ThreadCpuInfo>>
└── memory_data: MemoryTimeSeriesData  // 内存时序（最多保留 300 点）
    ├── timestamps: VecDeque<DateTime<Local>>
    └── memory_details: VecDeque<MemoryDetails>

thread_time_series: HashMap<thread_name, Vec<ThreadCpuInfo>>
    // --thread 模式下，按线程名聚合，仅保存 cpu_usage > 0 的点
```

`MemoryTimeSeriesData` 有容量限制（300 点），`CpuTimeSeriesData` 无限制——长时间运行会持续占用内存。

---

### CPU 采样流程（cpu.rs）

通过读取 Linux `/proc` 文件系统两次差值计算 CPU 使用率，不依赖 `pidstat`，对设备性能影响极低。

```
sample_cpu(package, interval_ms)
  └─ get_process_info(package)
  └─ adb shell ls /proc/<pid>/task          → 获取所有线程 TID 列表

  【第一次采样】
  └─ adb shell cat /proc/stat               → 系统总 jiffies（所有 CPU 核之和）
  └─ adb shell cat /proc/<pid>/stat         → 进程 jiffies (utime+stime)
  └─ adb shell cat /proc/<pid>/task/*/stat  → 所有线程 jiffies（一条命令批量读取）

  └─ sleep(interval_ms)                     → 等待采样窗口

  【第二次采样】（同上结构）

  【计算】（单核口径，与 adb top 一致：100% = 占满一个核，多线程可超 100%）
  total_delta  = sys_jiffies2 - sys_jiffies1（所有核之和）
  num_cores    = /proc/stat 中 cpuN 行数
  process_cpu% = (proc_jiffies_delta / total_delta) × 100 × num_cores
  thread_cpu%  = (thread_jiffies_delta / total_delta) × 100 × num_cores（每线程独立计算）

  └─ adb shell cat /proc/<pid>/task/*/comm  → 批量读取线程名
```

**`/proc/<pid>/stat` 解析**：文件格式为 `pid (comm) state ppid ...`，进程名含括号且可能有空格，解析时找最后一个 `)` 后按偏移量取第12、13字段（utime, stime，0-indexed）。

**ADB 调用次数/轮**：固定 6 次（ls + cat×2 × 3组），与线程数量无关（批量 cat）。

进程重启检测：`sample_cpu` 返回错误时，主循环调用 `get_process_info` 重新获取新 PID，并递增 `restart_count`。

---

### 内存采样流程（memory.rs）

```
sample_memory(package)
  └─ run_adb_command(["shell", "dumpsys", "meminfo", pid])
     解析策略：
       1. 找到 "App Summary" 行，进入解析模式
       2. 跳过 "Pss(KB)" 标题行，等待 "------" 分隔线
       3. 逐行按 "Category: value" 格式提取各内存分类
       4. 遇空行退出 App Summary 模式
       5. 兜底：在 App Summary 外查找 "TOTAL PSS:" 行
```

`MemoryDetails` 字段（单位 KB）：`java_heap`, `native_heap`, `code`, `stack`, `graphics`, `private_other`, `system`, `total_pss`。

---

### 输出文件触发时机

| 场景 | 触发条件 | 输出位置 |
|------|---------|---------|
| 内存图表（运行中） | 每次内存采样后，数据点 ≥ 5 个 | `log/<pkg>/<ts>/memory/` |
| CPU 图表（运行中） | 进入新的整点小时（hour 变化） | 先写 `/tmp/<pkg>_cpu_chart.png`，仅打印路径，不复制 |
| CPU 图表（退出时） | 程序退出，数据点 > 1 | `/tmp/` → 复制到 `log/<pkg>/<ts>/cpu/` |
| CPU CSV（退出时） | 同上 | `log/<pkg>/<ts>/cpu/<pkg>_cpu_data.csv` |
| 线程 CSV（退出时） | `--thread --cpu`，有数据 | `log/<pkg>/<ts>/thread/thread_<name>_<tid>_<pid>.csv` |
| 线程时序图（退出时） | 同上 | `log/<pkg>/<ts>/thread/thread_time_series_<ts>_pid<pid>.png` |

**注意**：`create_timestamp_subdir()` 使用 `OnceLock<Mutex>` 缓存目录路径，整个会话只创建一个时间戳目录，内存图表运行中触发时与退出时写入同一目录。

---

### 全局状态（utils.rs）

```rust
static INTERRUPT_FLAG: AtomicBool          // Ctrl-C 中断标志
static LOG_FILE_PATH: OnceLock<Mutex<Option<PathBuf>>>  // 日志文件路径（当前未初始化，append_to_log 调用均会静默失败）
static TIMESTAMP_DIR: OnceLock<Mutex<Option<PathBuf>>>  // 本次会话的输出根目录，首次调用 create_timestamp_subdir 时创建并缓存
```

`LOG_FILE_PATH` 目前始终为 `None`（原有的 `init_logging` 调用已注释掉），`append_to_log` 会返回错误但调用方均使用 `let _ =` 忽略。

---

## xrm 设计结构

单文件工具，全同步，无外部依赖（仅 `clap`）。

### 安全检查两层机制

```
main()
 ├─ 层1（sudo 快速拦截）
 │   is_running_with_sudo()         ← 检查 SUDO_USER / SUDO_UID 环境变量
 │   └─ is_dangerous_operation()    ← 检查 /、/*、/.*，或调用 is_system_critical_path()
 │       提前 exit(1)，不进入删除流程
 │
 └─ 层2（逐文件安全检查��remove_item 内部）
     ├─ is_symlink() → is_system_critical_path(原始路径) → remove_file
     ├─ !exists()    → force ? skip : error
     └─ canonicalize() → is_system_critical_path(真实路径) → remove_file / remove_dir_all
```

`is_system_critical_path()` 是唯一的路径黑名单，`is_dangerous_operation()` 直接复用它，两处检查列表保持一致。

受保护的路径：`/`, `/bin`, `/boot`, `/dev`, `/etc`, `/lib`, `/lib64`, `/proc`, `/root`, `/sbin`, `/sys`, `/usr`, `/var`（及其子路径）。

### 符号链接处理顺序

`remove_item` 中优先用 `is_symlink()` 检测，在 `exists()` 之前处理，确保悬空符号链接（目标不存在）也能被正确删除，而不是报"文件不存在"错误。
