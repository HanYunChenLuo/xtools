# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 工作流约定

- **待办（backlog）**：`WORKSPACE.md` —— 跨会话的工作项与优先级，完成即勾选并注明 commit。
- **会话历史**：`SESSION.md` —— 每个会话结束前追加一条总结（最新在最上）：日期/任务/commit 列表/关键结论与基线/遗留问题。新会话开始先读它获取近期上下文。
- 一个会话聚焦一个任务线，多会话通过这两个文件同步。

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

Release binaries are written to `target/release/`。

Workspace 成员：`xperf-core`（采样核心）、`xperformance`（CLI）、`xperf-gui`（Tauri GUI）、`xperf-agent`（设备端低间隔采样器，交叉编译 `cargo build -p xperf-agent --target aarch64-linux-android --release`，链接器配置在 `.cargo/config.toml`）、`xrm`（安全删除）。

---

## xperformance 设计结构

### 整体架构（统一 agent 采样）

CLI 和 GUI 不再有 adb 轮询路径，**所有采样都在设备端 agent（xperf-agent）进行**：

```
CLI:  main() → monitor_process() → monitor_process_agent()
GUI:  start_sampling / 自动启动 → spawn_sampling()（std::thread 阻塞读流）
        │
        ├─ agent::ensure_agent_built()   ← 无二进制时自动交叉编译（NDK）
        ├─ agent::deploy_agent()         ← push 到 /data/local/tmp（大小不一致才推）
        ├─ agent::spawn_agent()          ← adb exec-out 长连接
        └─ 事件循环：next_event() 阻塞读 NDJSON 行 → 打印/emit + 累积 pid_stats
```

- **设备端**：xperf-agent 常驻，直接读 /proc（CPU/线程）、smaps_rollup 或 dumpsys meminfo（内存）、本地 dumpsys SurfaceFlinger（FPS），按绝对节拍（start + round×interval，防漂移）逐轮输出 JSON 行
- **主机侧**：只是表现层（CLI 打印/CSV/图表；GUI emit 给前端）。ADB 断开 → exec-out EOF → 循环退出；Ctrl-C → 关闭连接 → agent 写 stdout 失败自行退出
- **xperf-core 的 Sampler/cpu/memory/fps 轮询模块是参考实现（含完整单测），CLI/GUI 已不再调用**；agent 复制了其中的解析逻辑（零依赖独立发布的要求）

### CPU 采样口径（agent 与参考实现一致）

单核口径，与 `adb top` 一致：100% = 占满一个核，多线程可超 100%。

```
process_cpu% = (proc_jiffies_delta / total_jiffies_delta) × 100 × num_cores
  total_jiffies_delta = /proc/stat 聚合行两轮差值（所有核之和）
  num_cores           = /proc/stat 中 cpuN 行数
  线程同理（/proc/<pid>/task/<tid>/stat 两轮差值）
```

`/proc/<pid>/stat` 解析：进程名含括号且可能有空格，找最后一个 `)` 后取第 12、13 字段（utime, stime，0-indexed）。

进程重启：agent 端读 stat 失败 → 发 exit 行并重扫包名进程（约 1s 一次），主机侧计 restart_count。

### 内存采样

- interval ≥ 500ms：设备端 `dumpsys meminfo <pid>`（App Summary 全分类明细）+ smaps_rollup 补 RSS
- interval < 500ms：只读 `/proc/<pid>/smaps_rollup`（Pss/Rss，~1ms；dumpsys ~100ms 太重）

**真机格式注意**：App Summary 分类行与 `TOTAL PSS:` 之间隔一个空行——空行结束区块，TOTAL 必须在区块外兜底解析。

`MemoryDetails` 字段（单位 KB）：`java_heap`, `native_heap`, `code`, `stack`, `graphics`, `private_other`, `system`, `total_pss`。

---

### FPS 采样流程（agent 设备端实现）

为什么不用 gfxinfo：`dumpsys gfxinfo framestats` 只统计 View 层级（HWUI）绘制的帧；游戏/相机/SurfaceView 直渲染应用的帧不上 gfxinfo。所有 buffer 最终都经 SurfaceFlinger 合成，因此对**图层**取帧时间戳是通用方案。

```
fps_sample_round(pid)                    ← agent 内每 PID 每轮一次
  ├─ sf_discover_layers(pid, package)    ← 首轮 + 连续 10 轮零帧后重做（Surface 重建会换名 #0→#1）
  │    ├─ dumpsys SurfaceFlinger（全量）  → 按 BufferStateLayer 块 metadata 的 ownerPID 归属匹配
  │    └─ 兜底：dumpsys SurfaceFlinger --list → 按包名匹配（去掉 "<hex> " 别名前缀，去重）
  └─ 每图层每轮：dumpsys SurfaceFlinger --latency '<layer>'（设备端本地调用，无 adb 往返）
       解析最近 127 帧的 actualPresent（过滤 0=空槽、i64::MAX=已入队未上屏哨兵）
       与上轮缓冲末尾时间戳取差 → 本窗口新帧数 → FPS = 新帧数 / 窗口墙钟时长
       有帧的图层各发一行（多渲染面不取舍、不混叠）；全零时发一条静止样本
```

关键设计点：
- **图层名可能不含包名**（如 svm 的渲染层叫 `SVM Container`），只能靠 ownerPID 归属识别
- **不用 `--latency-clear`**：实测部分设备（如此车机）clear 只清空缓冲而不返回数据；改用 `--latency` 逐轮差值
- 缓冲 127 帧 ≈ 2.1s@60fps：采样间隔大于该值时老帧被挤出，计数为下界（interval ≤ 1s 精确）
- **jank 不按 vsync 阈值**（30fps 相机流在 60Hz 屏上帧间隔 33ms 会被误判全卡）：用间隔 > 2×窗口中位间隔，<3 帧不计；低间隔（<~200ms）窗口帧数太少，jank 恒 0 属预期
- 静止界面 FPS=0 如实上报（事件照常发，GUI 折线落底）

`--fps` 退出时导出 `log/<pkg>/<ts>/fps/<pkg>_fps_data_pid<pid>.csv`（Timestamp,FPS,Jank,Layer）。GUI 有 FPS 勾选框 + 折线图（自适应纵轴，多图层逐层一条线，图层短名作图例）。

---

### agent（设备端采样器，xperf-agent）

**为什么**：adb 轮询单轮固定 6+ 次调用（每次 ~13ms 起，`dumpsys meminfo` ~100ms），低间隔下开销超过间隔本身，且每次 adb 调用都扰动被测系统。agent 常驻设备直接读 /proc（微秒级），NDJSON 经 `adb exec-out` 长连接流式回传（PerfDog Agent 同构思路，但免装 APK：纯静态二进制）。当前 CLI/GUI 的**唯一**采样路径。

**部署**：
- 本机二进制：`target/aarch64-linux-android/release/xperf-agent`（不存在时自动执行 `cargo build -p xperf-agent --target aarch64-linux-android --release`；需 NDK，链接器配置在 `.cargo/config.toml`，当前绑定 NDK 25.1.8937393 / API 26）
- 设备端路径：`/data/local/tmp/xperf-agent`
- 更新机制（`agent::deploy_agent`）：比对本机文件大小与设备上 `stat -c %s`，不一致或不存在才 `adb push` + `chmod 755`，避免每次重复推送
- 手动重建推送：`cargo build -p xperf-agent --target aarch64-linux-android --release && adb push target/aarch64-linux-android/release/xperf-agent /data/local/tmp/`

**要点**：
- 绝对节拍：`start + round × interval`，漂移时发 err 行（"round N overrun"）
- CPU 窗口 = 相邻两轮差值（常驻保有状态，无 phase1/phase2 结构）
- 需要 root（读他进程的 /proc、smaps_rollup）；内部设备 adbd 已 root
- 终端输出：interval ≥ 500ms 逐条详细打印；< 500ms 按 ~1s 聚合（avg/max），全量明细在退出 CSV；CSV 时间戳毫秒精度（`%.3f`）
- 主机断连/Ctrl-C → exec-out 关闭 → agent 写 stdout 失败自行退出

**验证基线**：svm @ 50ms 间隔，78 样本均值 15.03%，与 adb top 一致；50ms 窗口可见 25-47% 的瞬时毛刺（1s 采样看不到）。

---

### 输出文件触发时机

| 场景 | 触发条件 | 输出位置 |
|------|---------|---------|
| CPU 图表 + CSV（退出时） | 程序退出，数据点 > 1 | `log/<pkg>/<ts>/cpu/` |
| 内存图表 + CSV（退出时） | 同上 | `log/<pkg>/<ts>/memory/` |
| FPS CSV（退出时） | `--fps`，有数据 | `log/<pkg>/<ts>/fps/<pkg>_fps_data_pid<pid>.csv` |
| 线程 CSV + 时序图（退出时） | `--thread --cpu`，有数据 | `log/<pkg>/<ts>/thread/` |

**注意**：`create_timestamp_subdir()` 使用 `OnceLock<Mutex>` 缓存目录路径，整个会话只创建一个时间戳目录。


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
