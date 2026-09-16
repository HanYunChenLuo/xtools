# XTools

Android 开发工具集合。

## 项目列表

| 工具 | 说明 |
|------|------|
| `xperf-cli` | CLI：Android 应用性能监控（CPU / 内存 / FPS / GPU / 深挖 / 捕获 / ...） |
| `xperf-gui` | Tauri 2 GUI：同指标的实时图表 |
| `xperf-agent` | 设备端采样器二进制（自动推送，不单独使用） |

### xperf-cli

实时 Android 应用性能监控。**所有采样都在设备端**由常驻 agent daemon（`xperf-agent`）
完成，经 adb forward 的 TCP 长连接以 NDJSON 流式回传——没有逐轮 adb 轮询，
因此 50ms 级低间隔采样可行。

#### 功能特性

- **CPU 使用率**（单核口径，与 `adb top` 一致：100% = 占满一个核）
  - 进程级 + 线程级使用率
  - 峰值跟踪、进程重启检测
  - 时间序列图表与 CSV 导出（毫秒级时间戳）
- **内存使用**
  - 总 PSS；间隔 ≥500ms 时有分类明细（Java/Native/Code/Stack/Graphics/
    DMA-BUF/...，设备端 `dumpsys meminfo`）
  - 间隔 <500ms 时退化为 `/proc/<pid>/smaps_rollup`（仅 Pss/Rss——
    `dumpsys meminfo` 单次 ~100ms，低间隔下太重）
- **FPS**（`--fps`）
  - 基于 SurfaceFlinger 图层帧时间戳的逐图层帧率——对 SurfaceView/游戏
    直渲染应用同样有效（这类应用 `gfxinfo` 拿不到数据）
  - 多渲染层分别上报，互不混叠
  - 卡顿统计：帧间隔 > 2×窗口中位间隔
- **设备级上下文指标**：CPU 频率（`--freq`）、温度/热降频（`--thermal`）、
  GPU busy/显存（`--gpu` / `--gpu-mem`）、进程 IO（`--io`）、网络（`--net`）
- **深挖录制**：perfetto trace + SQL 归因（`--trace N`）、simpleperf 调用栈
  热点 + 浏览器火焰图（`--stack N`）
- **屏幕捕获**：截屏（`--screenshot`）、scrcpy 录屏（`--record N`）、屏幕镜像
  （`--mirror`）、logcat 抓取（`--logcat`，支持按包/级别/正则文本过滤
  `--logcat-regex`）
- **验证能力**：阈值告警（`--threshold`）、冷启动测量（`--cold-start`）、
  基线保存/对比（`--save-baseline` / `--compare-baseline`）
- **SSH 远程后端**（`--remote HOST`）：真机接在远端 Linux 机时，采样/深挖/
  捕获全功能经 SSH 隧道工作。
  从 Finder/DMG 启动时，本机 `adb` 依次从 `XPERF_ADB`、Android SDK 环境变量、当前
  `PATH` 和 macOS 常见 SDK 路径解析；`ssh` 同样使用固定路径兜底。
  远程连接失败会保留原始错误并显示“远程连接失败（当前仍为本机）”，不再用二次静默
  的本机切换覆盖诊断信息。
- **多设备**：`--device SERIAL`（GUI 内每设备独立会话并行）
- **数据导出**
  - 流式 CSV + 图表，位于 `/tmp/xperf/<包名>/<时间戳>/{cpu,memory,fps,thread,...}/`

#### 环境要求

- Android 设备（adb 可达）。**root 最优**；非 root 按能力降级可用（内存降级
  为限频 `dumpsys`、进程 IO 不可用——完整矩阵见 WORKSPACE.md G 节）
- 主机：Rust 工具链；agent 交叉编译需要 Android NDK（≥ 25.1）——链接器由
  `.cargo/ndk-clang.sh` 按宿主 OS 自动探测

#### 使用方法

```bash
./target/release/xperf-cli --package <包名> [--cpu] [--memory] [--fps] [--thread] [-i <间隔毫秒>]
```

选项：
- `--package, -p`：要监控的 Android 包名
- `--cpu`：监控 CPU 使用率
- `--memory`：监控内存使用
- `--fps`：监控 FPS（按 SurfaceFlinger 图层）
- `--thread`：监控线程活动（需配合 --cpu）
- `--interval, -i`：采样间隔，**毫秒**（默认 1000）

使用 CLI 时，设备端 agent 如缺失会在首次运行时自动编译并推送到
`/data/local/tmp/xperf-agent`；发布版 GUI 随包携带预编译 agent，运行时不会编译。

示例：
```bash
# 默认 1s 间隔监控 CPU、内存、FPS
./target/release/xperf-cli --package com.example.app --cpu --memory --fps

# 50ms 细粒度 CPU 毛刺分析
./target/release/xperf-cli --package com.example.app --cpu -i 50

# 只监控内存
./target/release/xperf-cli --package com.example.app --memory
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

## 下载

预编译发布包在内部 GitLab：[Releases](https://gitlab.chehejia.com/ligraphic/xperf/-/releases)。

- `xperf-vX.Y.Z-linux-x86_64.tar.gz`——Ubuntu 20.04+（glibc 2.31，CLI + agent）
- `xperf-vX.Y.Z-linux-x86_64-gui.AppImage`——Ubuntu 22.04+（Tauri 2 GUI + 内置 agent）
- `xperf-vX.Y.Z-macos-arm64.tar.gz` / `xperf-vX.Y.Z-macos-x86_64.tar.gz`——CLI + agent
- `xperf-vX.Y.Z-macos-arm64-gui.dmg` / `xperf-vX.Y.Z-macos-x86_64-gui.dmg`——GUI + 内置 agent

CLI tar 包内含 `xperf-cli`、`agent/xperf-agent`；GUI AppImage/DMG 内含应用资源 `agent/xperf-agent`，运行时不编译 agent。
macOS DMG 默认使用完整 ad-hoc bundle 签名以封印应用资源；正式对外分发仍需 Developer ID 签名并完成 Apple 公证，否则 Gatekeeper 可能阻止启动。
版本变更说明见 [CHANGELOG.md](CHANGELOG.md)。

## 构建

Cargo workspace 管理全部工具。构建所有主机侧工具（agent 仅 Android 目标，
不在默认成员集内）：

```bash
cargo build --release
```

设备端 agent 交叉编译（通常首次运行时自动完成）：

```bash
cargo build -p xperf-agent --target aarch64-linux-android --release
```

主机二进制在 `target/release/`；agent 二进制在
`target/aarch64-linux-android/release/`。

## 测试

```bash
cargo test
```

（仅主机侧工具——默认成员集；`--workspace` 会触发 agent 主机目标的
`compile_error!` 拦截。）
