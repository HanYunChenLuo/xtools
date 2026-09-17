# xperf

Android 应用性能分析工具集：CLI + 桌面 GUI + 设备端采样 agent。

**中文** | [English](README_en.md)

## 项目列表

| 工具 | 说明 |
|------|------|
| `xperf-cli` | CLI：Android 应用性能监控（CPU / 内存 / FPS / GPU / 深挖 / 捕获 / ...） |
| `xperf-gui` | Tauri 2 GUI：同指标的实时图表 |
| `xperf-agent` | 设备端采样器 daemon（自动部署，不单独使用） |

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
  捕获全功能经 SSH 隧道工作（单条 ControlMaster 连接承载 adb 协议，
  每设备一条事件流隧道）。认证支持 ssh config 别名免密，或
  用户名+IP+密码（GUI 表单 / CLI `XPERF_SSH_PASSWORD` 环境变量——
  密码仅进程内存驻留，不落盘）
- **问题反馈**（`--feedback "问题描述"`）：收集最近 1 小时工具自身证据
  （会话产物/诊断日志/各设备端 agent 日志）自检打包上传内部 GitLab issue；
  `--gitlab-login` 浏览器 OAuth 授权后 issue 作者为本人（兼容 SSO/2FA），
  或用 `GITLAB_TOKEN` 环境变量 / `~/.config/xperf/gitlab-token`（PAT）
- **工具命令**：`--force-stop`（清场停止应用）、`--clean-cache`（清理缓存与
  采集数据）、`--update-simpleperf-scripts`（更新火焰图脚本）
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
./target/release/xperf-cli --package <包名> [选项...]
```

常用选项（完整列表见 `--help`）：

| 分组 | 选项 |
|------|------|
| 基础 | `--package, -p`（包名）、`--interval, -i`（间隔 ms，默认 1000，最小 50）、`--device, -d`（多设备时指定 serial） |
| 指标 | `--cpu`、`--memory`、`--thread`（需 `--cpu`）、`--fps`、`--freq`、`--thermal`、`--gpu`、`--gpu-mem`、`--io`、`--net` |
| 深挖 | `--trace <秒>`（perfetto）、`--stack <秒>`（simpleperf 函数热点） |
| 验证 | `--threshold <规则>`、`--cold-start <Activity>`、`--save-baseline` / `--compare-baseline` |
| 捕获 | `--screenshot`、`--record <秒>`、`--mirror`、`--logcat [--logcat-regex <正则>]` |
| 远程 | `--remote <主机>`、`--remote-adb <路径>`、`--remote-adb-port <端口>` |
| 反馈 | `--feedback <描述>`、`--gitlab-login` / `--gitlab-logout` |
| 工具 | `--force-stop`、`--clean-cache`、`--update-simpleperf-scripts` |

使用 CLI 时，设备端 agent 如缺失会在首次运行时自动编译并推送到
`/data/local/tmp/xperf-agent`；发布版 GUI 随包携带预编译 agent，运行时不会编译。

示例：
```bash
# 默认 1s 间隔监控 CPU、内存、FPS
./target/release/xperf-cli --package com.example.app --cpu --memory --fps

# 50ms 细粒度 CPU 毛刺分析（含线程级明细）
./target/release/xperf-cli --package com.example.app --cpu --thread -i 50

# 采样并行录制 10s perfetto（同窗口对照，结束自动出 SQL 归因报告）
./target/release/xperf-cli --package com.example.app --cpu --trace 10

# 阈值告警 + 会话结束保存基线（下次 --compare-baseline 对比回归）
./target/release/xperf-cli --package com.example.app --cpu --memory \
    --threshold cpu>80,mem>500 --save-baseline

# 抓取 logcat（按包过滤 + 消息体正则）
./target/release/xperf-cli --package com.example.app --logcat --logcat-regex 'ANR|FATAL'

# SSH 远程后端（真机接在远端 Linux 机 hppc）
./target/release/xperf-cli --remote hppc --package com.example.app --cpu
```

#### 输出格式

间隔 ≥500ms 时逐条详细打印；低于 500ms 时按秒聚合（avg/max）打印——
全量明细始终在导出的 CSV 中。

```
[20:23:18] Process CPU: 27.6% (pid: 29697)
[20:23:18] Memory Usage: 482692 KB (Java: 7328, Native: 117872, Code: 36100, Graphics: 0) [pid 29697]
[20:23:18] FPS: 60.0 (jank: 0, frames: 30, layer: SurfaceView[com.example.app](BLAST)#0) [pid 29697]
```

### xperf-gui

与 CLI 同一 agent 传输层的 Tauri 2 桌面 GUI：

- **多设备并行**：顶栏每台在线设备一个 tab（热插拔动态增删；断开设备
  灰显保留数据，插回自动恢复采样）
- **每设备独立页**：
  - 侧栏——应用管理（包名选择/打开/重启/停止应用，冷启动测量随打开/
    重启自动进行）、屏幕捕获（截屏/录屏/镜像）、数据管理（导出 CSV/
    保存与对比基线/清理缓存）、设备权限徽章与「获取 root」
  - 四个子 tab——性能指标（CPU/内存/FPS/GPU 等折线图 + 实时数值 +
    Top 线程 + 峰值/基线对比/冷启动面板）、Perfetto 分析、Simpleperf
    函数热点（浏览器火焰图）、日志（logcat live 视图，级别/按包/正则
    过滤，口径热切换）
- **SSH 远程连接**：顶栏下拉切换远端（含 用户名+IP+密码 配置表单）
- **问题反馈**：顶栏一键收集上传 GitLab issue
- 暗/亮双主题；`--package --device` 命令行参数自动启动采样

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

## 许可证

[MIT](LICENSE)
