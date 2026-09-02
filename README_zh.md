# XTools

Android 开发工具集合。

## 项目列表

| 工具 | 说明 |
|------|------|
| `xperformance` | CLI：Android 应用性能监控（CPU / 内存 / FPS） |
| `xperf-gui` | Tauri 2 GUI：同指标的实时图表 |
| `xperf-agent` | 设备端采样器二进制（自动推送，不单独使用） |
| `xrm` | 带系统路径保护的安全删除工具 |

### xperformance

实时 Android 应用性能监控。**所有采样都在设备端**由常驻 agent（`xperf-agent`）
完成，经单条 `adb exec-out` 长连接以 NDJSON 流式回传——没有逐轮 adb 轮询，
因此 50ms 级低间隔采样可行。

#### 功能特性

- **CPU 使用率**（单核口径，与 `adb top` 一致：100% = 占满一个核）
  - 进程级 + 线程级使用率
  - 峰值跟踪、进程重启检测
  - 时间序列图表（1920x1080）与 CSV 导出（毫秒级时间戳）
- **内存使用**
  - 总 PSS；间隔 ≥500ms 时有分类明细（Java/Native/Code/Stack/Graphics/...，
    设备端 `dumpsys meminfo`）
  - 间隔 <500ms 时退化为 `/proc/<pid>/smaps_rollup`（仅 Pss/Rss——
    `dumpsys meminfo` 单次 ~100ms，低间隔下太重）
- **FPS**（`--fps`）
  - 基于 SurfaceFlinger 图层帧时间戳的逐图层帧率——对 SurfaceView/游戏
    直渲染应用同样有效（这类应用 `gfxinfo` 拿不到数据）
  - 多渲染层分别上报，互不混叠
  - 卡顿统计：帧间隔 > 2×窗口中位间隔
- **数据导出**
  - CSV + 图表，位于 `log/<包名>/<时间戳>/{cpu,memory,fps,thread}/`

#### 环境要求

- Android 设备 **adb 已 root**（adbd 以 root 运行——agent 需要读其他进程的
  `/proc` 条目）
- 主机：Rust 工具链；agent 交叉编译需要 Android NDK（链接器配置在
  `.cargo/config.toml`，可用环境变量
  `CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER` 覆盖）

#### 使用方法

```bash
./target/release/xperformance --package <包名> [--cpu] [--memory] [--fps] [--thread] [-i <间隔毫秒>]
```

选项：
- `--package, -p`：要监控的 Android 包名
- `--cpu`：监控 CPU 使用率
- `--memory`：监控内存使用
- `--fps`：监控 FPS（按 SurfaceFlinger 图层）
- `--thread`：监控线程活动（需配合 --cpu）
- `--interval, -i`：采样间隔，**毫秒**（默认 1000）

设备端 agent 会在首次运行时自动编译（如缺失）并推送到
`/data/local/tmp/xperf-agent`。

示例：
```bash
# 默认 1s 间隔监控 CPU、内存、FPS
./target/release/xperformance --package com.example.app --cpu --memory --fps

# 50ms 细粒度 CPU 毛刺分析
./target/release/xperformance --package com.example.app --cpu -i 50

# 只监控内存
./target/release/xperformance --package com.example.app --memory
```

#### 输出格式

间隔 ≥500ms 时逐条详细打印；低于 500ms 时按秒聚合（avg/max）打印——
全量明细始终在导出的 CSV 中。

```
[20:23:18] Process CPU: 27.6% (pid: 29697)
[20:23:18] Memory Usage: 482692 KB (Java: 7328, Native: 117872, Code: 36100, Graphics: 0) [pid 29697]
[20:23:18] FPS: 30.2 (jank: 0, frames: 30, layer: SVM Container#0) [pid 29697]
```

### xperf-gui

基于同一 agent 传输的 Tauri 2 桌面 GUI：包名选择器、按 PID 的 CPU/内存
图表、按图层的 FPS 图表（实时 Canvas）。

```bash
./target/release/xperf-gui --package <包名> --cpu --memory --fps
```

### xrm

安全删除工具：拒绝删除系统关键路径（sudo 下同样拦截），正确处理悬空符号链接。

## 构建

Cargo workspace 管理全部工具。构建所有主机侧工具：

```bash
cargo build --release --workspace
```

设备端 agent 交叉编译（通常首次运行时自动完成）：

```bash
cargo build -p xperf-agent --target aarch64-linux-android --release
```

主机二进制在 `target/release/`；agent 二进制在
`target/aarch64-linux-android/release/`。

## 测试

```bash
cargo test --workspace -- --test-threads=1
```

（测试共享全局 mock adb runner，必须单线程运行。）
