# 贡献指南

感谢参与 xperf 维护。本指南覆盖环境准备、构建测试与工作流约定；架构与实现细节见 `CLAUDE.md`（它同时是 AI 编码助手的指引，人读同样有效）。

## 仓库结构

| crate | 说明 |
|---|---|
| `xperf-core` | 共享核心：协议类型 + agent 传输层（daemon 管理 / adb forward / TCP 流）+ 平台抽象（SS2/SS3/SS4/Android）+ trace / simpleperf / logcat / 镜像录屏 / 问题反馈等模块，CLI 与 GUI 共用 |
| `xperf-cli` | 命令行工具（实时采样 / 深挖录制 / 验证能力） |
| `xperf-gui` | Tauri 2 桌面 GUI（多设备并行）；前端为 `frontend/` 下纯静态 HTML/JS，**无 node 构建步骤** |
| `xperf-agent` | 设备端采样器 daemon（**仅 Android 目标**，不在默认成员集内，主机上显式构建会被 `compile_error!` 拦截） |

架构要点（为什么采样都在设备端 agent、九项指标口径、平台差异）见 `CLAUDE.md` 对应章节。

## 环境准备

- Rust stable
- Android NDK **≥ 25.1.8937393**（agent 交叉编译；链接器由 `.cargo/ndk-clang.sh` 按宿主 OS 自动探测）
- adb（真机测试需要；真机接在远端 Linux 机时可用 `--remote`，见下）
- [Git LFS](https://git-lfs.com)：`example/apk` 测试应用与 simpleperf report 双平台库走 LFS，clone 前 `git lfs install`

## 构建与测试

```bash
cargo build --release          # 主机侧工具（CLI + GUI）
cargo test                     # 全量单测（默认成员集）
cargo build -p xperf-agent --target aarch64-linux-android --release   # 设备端 agent（通常首次运行自动构建）
```

**代码规则（强制）**：新增/修改的所有 pub 项必须带完整 doc 注释（含单位/语义/无值字段要写明），路径/参数包反引号；交付前跑完整 `cargo doc` 做到**零 warning 零 error**（默认 lint 集 + missing_docs，命令见 `CLAUDE.md` Commands 节）。clippy 同样零警告。

### 集成测试（`#[ignore]`，需真实环境）

涉及 SSH 隧道/真机设备的测试标 `#[ignore]`，手动跑，环境经变量注入（不硬编码主机/设备，各维护者按自己的环境设）：

```bash
XPERF_IT_SSH_HOST=<ssh 别名或 user@host>   # 必填：免密可达 + 远端 adb
XPERF_IT_SSH_ADB=<远端 adb 路径>           # 可选：默认 adb（预检自动探测标准 SDK 位置）
XPERF_IT_DEVICE=<设备 serial>              # 设备相关测试必填
XPERF_IT_PACKAGE=<已安装包名>              # 可选：默认 gltf viewer
cargo test -p xperf-core -- --ignored
```

## 真机测试约定

- **测试对象统一用** `example/apk/filament-gltf-viewer-v1.76.0-android.apk`（包名 `com.google.android.filament.gltf`）
- 真机接在远端 Linux 机时，本机 CLI/GUI 经 `--remote <主机>` 完成**全部**功能（采样/perfetto/simpleperf/捕获），无需把设备接到本机

## 工作流约定

- **分支**：`feature/*`、`fix/*`、`docs/*`、`refactor/*` 分支开发，一步一 commit，`--no-ff` 合 main
- **任务看板**：跨会话待办记 `WORKSPACE.md`（完成勾选并注明 commit）
- **会话历史**：每个工作会话结束前在 `SESSION.md` 顶部追加一条（日期/任务/commit/结论/遗留）
- **发版**：版本号唯一出处 = 根 `Cargo.toml` `[workspace.package]`；流程见 `CLAUDE.md`「软件发布」节

## git

主远端 = **内部 GitLab** `git@gitlab.chehejia.com:ligraphic/xperf.git`：clone 后 feature 分支开发 → `--no-ff` 合 main → `git push origin main`（LFS 对象随 push 经 SSH 直传）。
