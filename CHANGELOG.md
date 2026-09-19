# Changelog

本文件记录 xperf 各版本的用户可见变更。格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)。

## [Unreleased]

## [v0.3.0] - 2026-09-19

### 新增

- **GUI 可编程调试接口**（默认开启，`XPERF_GUI_DEBUG=0` 关闭）：GUI 进程内嵌 loopback HTTP 接口（axum，随机端口 + token 鉴权，发现文件 `~/.config/xperf/gui-debug-<pid>.json`），供脚本/AI agent 自动化目验 GUI——读取 UI 结构与元素位置（`/api/dom`，含 bounding rect）、监控状态（`/api/status`/`/api/state`，含图表 series 摘要、悬停 tooltip、logcat 行、实时数值面板）、单图表全分辨率读数（`/api/series`）、注入操作（`/api/action`：click/input/select/check/hover/scroll/key，真实 DOM 事件序列）、任意 JS 逃逸舱（`/api/eval`）。
- **GUI 折线图悬停精确读数**：鼠标悬停任一折线图显示竖线游标 + 浮层，列出该时刻各序列的准确值与毫秒级时间戳（取数在完整序列上二分最近点，不受绘制抽稀影响）；多序列（如 8 核频率图）浮层两列排布。
- **`--duration <秒>` 限时采样**：到点自动走正常退出流程（汇总/退出图表/验证报告/基线保存对比照常），供脚本与 AI agent 做有界调用；与 `--trace`/`--stack`/`--record` 同给时采样窗口取最长者。设备断连重连同样遵守限时（含 SSH 隧道重建退避切片，到点及时退出）。
- **采样会话退出落 `summary.json`**：结构化会话汇总（与基线 JSON 同 schema）+ 阈值判定（all_pass + 逐规则触发详情）+ 基线动作与对比结论（verdict/回归指标名），CI 断言免解析终端文本；零样本会话也落盘（samples=0）。
- **CLI 发布包附带 agent 文档**：tar 包新增 `AGENTS.md` 与 `skills/xperf/SKILL.md`（编码 agent 的能力发现入口，装入 agent 的 skill 根目录即可按任务描述触发）——此前发布包只有二进制与 README，只下载发布包的用户缺少 agent 调用指引。
- **问题反馈**：CLI `--feedback "问题描述"` 与 GUI 顶栏「问题反馈」按钮——收集最近 1 小时工具自身证据（会话产物 / GUI 诊断日志 / 各设备端 agent 日志），逐项自检后打包上传内部 GitLab issue 并给出链接；issue 默认指派维护者（`XPERF_GITLAB_ASSIGNEE` 可覆盖）。
- **GitLab OAuth 登录**：`--gitlab-login` / `--gitlab-logout`（浏览器授权码 + PKCE，兼容 SSO/2FA），登录后 `--feedback` 以本人身份创建 issue；凭证优先级 `GITLAB_TOKEN` 环境变量 > `~/.config/xperf/gitlab-token`（PAT）> OAuth 登录态。
- **SSH 远程密码认证**：GUI 远程配置表单支持 用户名+IP+密码+端口；CLI 经 `XPERF_SSH_PASSWORD` 环境变量传密码。密码仅进程内存驻留、不落盘（不进配置文件/日志）；可选把主机保存为纯 Host 条目（HostName/User/Port，零秘密）写入 ssh config 复用。密码错误立即报错，不占用服务器重试配额。支持非 22 SSH 端口。

### 修复

- 修复 release tar 包安装的 `xperf-cli` 找不到捆绑 agent：解析链改为 `XPERF_AGENT_BIN` 环境变量（指向不存在即报错不回退、空串视同未设置）→ 二进制旁 `agent/xperf-agent`（tar 包布局）→ 开发检出自动构建兜底。
- 修复独立 `--cold-start`（不带采样指标）静默不执行仍退出 0：现真正执行测量，且失败（如 Activity 不存在）以退出码 1 上报。
- 修复纯 `--freq`/`--thermal` 会话 `summary.json` 的 `duration_s` 恒 0：频率/温度序列计入会话时长跨度（两指标本身仍为 CSV-only，见 AGENTS.md）。
- 修复 Linux AppImage 在 Ubuntu 22.04 启动失败（`libpango` symbol lookup error）：AppImage 内 pango/WebKitGTK 引用的 libharfbuzz 新符号（3.3.0 / 4.0.0）在 jammy 自带版本（2.7.4）中缺失；现向 AppDir 注入构建侧 libharfbuzz 后重打包（24.04 不受影响）。
- 修复 `cargo run --bin xperf-gui --release` 开发运行误报「GUI 发布包缺少预编译 agent」：agent 解析的开发判定从 `debug_assertions` 改为「编译期 workspace 是否存在」——开发运行（`cargo run`/`cargo install`，任意 profile）回退与 CLI 一致的自动构建，发布包（DMG/AppImage）行为不变（只认包内资源，不触发编译）。

### 变更

- 文档：新增 `AGENTS.md`（Codex 自动发现约定）与 `skills/xperf/SKILL.md`（Claude Code/siada skill），README 双语补「For AI agents / 面向 AI agent」节。
- 文档：README 标题与资产命名统一为 xperf，补中文版 `README_zh.md`（与主 README 互链），功能面与 GUI 章节更新至当前版本。

## [v0.2.1] - 2026-09-16

补充 GUI 桌面发布资产，不改动实时采样协议。

### GUI 分发

- Linux x86_64 新增 Tauri 2 AppImage，最低 Ubuntu 22.04（WebKitGTK 4.1）。
- macOS 新增 arm64/x86_64 DMG；DMG 内的 `.app` 使用完整 ad-hoc bundle 签名封印资源，避免安装后因资源签名不完整被系统提示“已损坏”。
- GUI 发布包内置 `agent/xperf-agent`，采样、断连重连和 daemon 重启均使用包内预编译二进制，运行时不调用 Cargo 或 Android NDK。公开分发仍需 Developer ID 签名和 Apple 公证。

### CI

- GitLab CI 新增 WebKitGTK 4.1 GUI 构建 job，并在 Linux AppImage 打包时显式固定 `ARCH=x86_64`。
- 修复 macOS DMG/Finder 启动 GUI 时 SSH 远程连接失败：本机 `adb`/`ssh` 不再依赖 shell 的 `PATH`，远程失败时保留原始错误，不再被“已切回本机”覆盖。
- 修复 SSH 远程连接间歇性缓慢（实测 30~80s）：连接建立收敛为单次 SSH 握手（远端预检经 ControlMaster mux 免握手执行，替代原先 5 次独立握手），健康网络下全程 <1s；新增连接链路阶段计时诊断（GUI 写 `/tmp/xperf_gui_diag.log`，CLI 走 stderr）。


## [v0.2.0] - 2026-09-15

自 v0.1.5（2025-03）以来的全部变更——工具链整体重构为设备端 agent 架构，指标、验证能力与平台覆盖大幅扩展。

### 架构

- **设备端 agent（`xperf-agent`）daemon 化为唯一采样路径**：常驻监听抽象 socket，host 经 adb forward + TCP 连接；低开销（直读 /proc，微秒级）、断连自动重连恢复、多 host 并发会话（上限 10）。CLI 与 GUI 的 adb 轮询路径全部移除。
- **CLI 更名** `xperformance` → `xperf-cli`，与 `xperf-core`/`xperf-gui`/`xperf-agent` 命名对齐。
- **平台抽象层**：adb 自动检测 SS2MAX / SS2PRO / SS3 / SS4 / 泛型 Android，按平台选路 GPU 通道与数据源。
- **SSH 远程后端（`--remote`）**：真机接在远端 Linux 机时，本机 GUI/CLI 经 SSH 隧道完成全部功能（采样/perfetto/simpleperf/镜像/录屏/logcat）。
- **SS4 adb 自动桥接**：MindRT（Linux PVM）+ Android（GVM）双系统自动 forward + connect，Android 以稳定 serial 进入现有全链路，含 GVM 重启自愈。

### 指标（9 项全实现）

CPU（单核口径，与 `adb top` 一致）、内存（App Summary 分类明细 + DMA-BUF 拆分）、FPS（SurfaceFlinger per-layer，jank 统计）、CPU 频率、温度/热降频、GPU busy（kgsl/QNX telnet/topgpu/ligfx 四通道按平台）、GPU 显存、IO、网络。

### 验证能力

- `--threshold` 阈值实时告警 + 退出验证报告（静止界面 FPS=0 不误报）
- `--cold-start` 冷启动测量（`am start -W` 解析，GUI 同链路）
- `--trace N` perfetto 深挖：录制→拉回→trace_processor SQL 归因报告→浏览器一键全自动加载
- `--stack N` simpleperf 函数热点：调用栈录制 + 线程/self/children 三视图报告 + 浏览器火焰图
- `--save-baseline` / `--compare-baseline` 基线对比：两次运行 diff 回归判定（CLI/GUI 互通）

### GUI

多设备并行（每设备一 tab，热插拔动态增删、断开保留数据插回恢复）、10 张折线图 + 实时数值面板 + Top 线程 + 峰值面板 + 冷启动面板、Perfetto/Simpleperf/日志独立子 tab、应用管理（打开/重启/停止）、暗/亮双主题。

### 新增功能

- **logcat 抓取**：流式落盘 + GUI live 视图，按包（uid/pid 降级）/级别/文本正则过滤，口径热切换
- **scrcpy 屏幕镜像与录屏**、`adb exec-out` 截屏（PNG 魔数定位剥前缀）
- **非 root 设备支持**：hello 带 root 标志按能力降级（内存降级 dumpsys 限频、IO 禁用等，矩阵见 README）

### 分发

- 首次提供预编译发布包：Linux x86_64（CLI + 预编译 Android agent）与 macOS（arm64 / x86_64，本机构建上传），tag 触发 GitLab CI 自动出包挂 Release。
- GUI 发布形态：Linux x86_64 AppImage（Ubuntu 22.04+，WebKitGTK 4.1）与 macOS arm64/x86_64 DMG；GUI 资源内置预编译 Android agent，运行时不调用 Cargo/NDK。GUI 资产随 `v0.2.1` 发布。

[v0.3.0]: https://gitlab.chehejia.com/ligraphic/xperf/-/releases/v0.3.0
[v0.2.1]: https://gitlab.chehejia.com/ligraphic/xperf/-/releases/v0.2.1
[v0.2.0]: https://gitlab.chehejia.com/ligraphic/xperf/-/releases/v0.2.0
