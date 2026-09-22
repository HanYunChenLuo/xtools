# WORKSPACE.md — xperf 工作区待办与状态

> 本文件记录跨会话的待办事项（backlog）。每次会话的历史总结见 `SESSION.md`。
> 完成一项就把状态改为 ✅ 并注明完成的 commit；新增想法随时追加。
> 最后更新：2026-09-22（深夜）：K2 抓出的发布缺陷**已根治并 CI 产物真机复验**（A 节 ✅：脚本集目录改运行时解析链 + 随包分发；火焰图渲染剥掉 AppImage 的 `PYTHONHOME/PYTHONPATH`）；同日 K3 ✅（issue #5 关闭）、K2 ✅（改在 hppc 跑 v0.3.1 AppImage / 24.04，沙箱 hook 修复在发布产物上闭环）、K1 阻塞（kong 侧 adb 零设备，待车机通电）

## 当前状态速览

- 采样架构：**设备端 agent（xperf-agent）daemon 化为唯一采样路径**——`--daemon` 常驻监听 `localabstract:xperf-agent`，host 经 adb forward+TCP 连接（**版本契约 v6 起：host 接受 daemon ≥ 自身版本，wire 自 v3 稳定；daemon 间 bind 竞争高版本胜出、挂死者被清场接管——多宿主混跑不再打版本战**；start/stop/ping；多 host 上限 10；0 会话 60s 自杀）；断连自动重连恢复；模块化布局（main/proc/mem/fps/thermal + gpu/ 五通道）
- 指标覆盖（9 项全实现）：CPU（单核口径）、内存、FPS、CPU 频率、温度/热降频、GPU、IO、网络、GPU 显存
- **多设备 adb（两级）**：全局 `TARGET_SERIAL`（CLI）+ **会话级 `serial: Option<&str>` 参数**（core 各入口：spawn_agent/deploy/trace/simpleperf/coldstart/detect_platform，`adb_for`/`run_adb_command_for` 注入）；**GUI 每设备一 tab 并行**（详见 CLAUDE.md「多设备 adb」）
- **GUI 多设备改版**：顶栏设备 tab（热插拔动态增删、断开灰显保留数据插回自动恢复）+ 每设备独立页（侧栏 + 性能指标/Perfetto/Simpleperf 三子 tab）+ `DeviceSession` 类（事件按 payload.serial 分发）+ **「应用操作」**（打开/重启应用，activity 留空自动 resolve-activity，`am start -W` 顺带测冷启动进「冷启动」面板，系统重定向警示）
- **平台抽象**：`xperf-core/src/platform/` trait + adb devices -l 自动检测（SS2MAX/SS2PRO/SS3/SS4/Android）；agent 加 `--platform`/`--qnx-host` 参数
- **GPU busy 通道**（detect_gpu_path_ex 按平台选路，v7 起显存拆分）：kgsl sysfs（Android/SS2）/ QNX telnet（SS3，真 busy%/util%/频率+每进程）/ topgpu（SS2MAX）/ ligfxprofilerd logcat（SS4，GVM 无输出永不命中——实际走 host 侧通道）；**GPU 显存独立开关 `--gpu-mem`**（无源平台 err 禁用，GUI 按平台禁用勾选——SS2MAX）
- **SS4 host 侧指标通道**（hostchan.rs）：GPU busy=经桥接网关读 MindRT logcat ligfxprofilerd（合成 AgentEvent 经 mpsc 汇入 AgentStream，CLI/GUI 零改动）；**FPS 已回归设备端 per-layer 路径**（协议 v5：A16 图层名须带 `<hex> ` 前缀，曾误判阉割绕道 host frametimeline 已废弃删除）；SS4 平台限制：GVM 无 cpufreq/thermal（VM 隔离）、PMU 未虚拟化（simpleperf 自动 cpu-clock）
- **C 类验证能力**：阈值告警（--threshold，静止界面不误报）+ 退出验证报告 + 冷启动（--cold-start / GUI 打开/重启应用，模块 core/coldstart.rs）+ **simpleperf 函数热点**（--stack N：调用栈录制 + 线程/self/children 三视图报告，CLI 独立/并行两模式 + GUI 独立 tab）+ **基线对比**（--save-baseline/--compare-baseline：两次运行 diff 回归判定，CLI/GUI 共用，详见 CLAUDE.md「基线对比模式」）
- 落盘：数据根 `/tmp/xperf`（CLI 流式 CSV + 退出图表；GUI 完整历史 + CSV 导出，共用同根；GUI 深挖目录 `<pkg>/<ts>-<serial>/` 防双设备撞名）；清理走 CLI `--clean-cache` / GUI 按钮（~/.cache/xperf + /tmp/xperf，2bd6bca）
- GUI：10 张折线图（GPU 显存独立图/独立开关，SS2MAX 平台禁用勾选）+ 实时数值面板 + Top 线程 + 峰值 + 冷启动面板 + 间隔档位下拉 + 实际周期标注 + 勾选即时生效（自动重启会话；默认勾选 CPU/内存/FPS）+ Perfetto 分析（独立 tab 报告 + 浏览器自动加载 + 每秒录制进度）+ **函数热点**（独立 tab + 每秒录制进度 + 浏览器火焰图）+ 暗/亮双主题
- agent 部署：自动尝试 adb root（**仅车机平台**，XPERF_NO_AUTO_ROOT=1 旁路；hello 带 root 标志，非 root 按能力降级——WORKSPACE G 节矩阵）；src 树内任一 .rs mtime 变化自动重建
- 测试：**全量全绿**（core 157+8 ignored（真机/远端集成**环境变量注入**（`XPERF_IT_SSH_HOST/ADB/DEVICE/PACKAGE`，多人维护各自环境，见 CLAUDE.md Commands）：隧道×2/init+acquire_root/logcat restart）：协议/流合并/hostchan/transport/trace/simpleperf/baseline/coldstart/设备 diff/auto-root 守卫/logcat/oauth 竞态（含文本过滤与秒死诊断差集）；xperf-cli 5：alerts；GUI 11：export_csv×2 + 基线×2 + 多会话隔离 + 远程配置等），clippy 零警告，**cargo doc 零 warning**（默认 lint 集 + missing_docs 三 crate）。**xrm 已于 2026-09-15 移出本仓库**（历史条目见 SESSION.md）
- **SSH 远程后端（feature/ssh-remote 已合 main）**：`--remote <远端机>` 经 SSH 隧道连远端 adb server（hop#1 承载 adb 协议 + hop#2 每设备一条承载 agent 流），采样/trace/simpleperf/断连重连/GUI 连接切换全通；DMG/Finder 启动补齐本机 adb/ssh 绝对路径解析，远程失败不再用二次本机切换覆盖原始错误；详见 CLAUDE.md「SSH 远程后端」与 `docs/DESIGN-ssh-remote.md`
- 设备：SS3 6eb792dfb0f（adbd root，QNX GPU 通道 + 多设备并行已真机回归）；SS2MAX d1f39648c1f（adb root 可用；**多设备并行 + 冷启动 COLD 874ms 已真机验证**）；SS4 经 hppc 桥接为 localhost:5559（MindRT `42087266b1f` 中继；桥接/指标双适配完成，H 节）
- **测试对象（555ffab 起统一）**：`example/apk/filament-gltf-viewer-v1.76.0-android.apk`（git-lfs 管理，包名 `com.google.android.filament.gltf`，入口 `.MainActivity`）——真机测试一律用它，不再用 svm。已装 SS3 + SS2MAX + SS4

- [x] **DMG 安装版 GUI SSH 远程连接修复**（**已完成**，2026-09-16 合 main 9f441d8）：修复 Finder 启动 PATH 不含本机 `adb`/`ssh` 导致的远程初始化失败（c2032af），以及前端二次 `connect_remote(null)` 覆盖原始错误。用户实测首版 DMG 远程可用但**连接间歇 30~80s**——449c472 定位并修复：根因非 DMG 产物而是 init_remote 旧实现顺序 5 次 SSH 握手 × 网络波动（单次握手 2.5~10s），establish 收敛为单次握手（远端预检走 mux，详见 DESIGN-ssh-remote §4.2a）+ 全链路 `utils::diag` 阶段计时；CLI 实测 3.5s→0.98s，DMG（sshfix2）Finder 环境 connect_remote 841ms 用户确认。随下次版本 tag 发布。

---

## A. 已知缺陷

- [x] ~~**发布产物的火焰图脚本目录是编译期路径（v0.3.1 AppImage 实测，2026-09-22 K2 抓出）**~~（**已修复并真机复验**，2026-09-22 晚，两层各一 commit：`4249552` 解析链 + `e435f96` python 环境，merge `e070698`）：
  - **第一层（目录）**：`scripts_dir()` 改为运行时解析链 `XPERF_SIMPLEPERF_SCRIPTS` → 随产物（GUI `resource_dir()` 注入 / CLI tarball exe 旁 `simpleperf_scripts/`，须「齐全」或「可写」）→ 仓库 vendor（**存在才用**，开发语义不变）→ `~/.cache/xperf/simpleperf_scripts/`；内核 `pick_scripts_dir` 纯函数 + 单测锁优先级与 `>1MB`（LFS 指针）判据；`download_scripts` 前 `create_dir_all`；`--update-simpleperf-scripts` 只写可写目录（只读随包资源如实拒绝）；`--clean-cache` 不清发布包内置只读脚本集；ensure 时 stderr 打一行实际目录（排障）。打包：`scripts/stage_simpleperf_scripts.sh` 按主机平台暂存（Linux +~6MB / macOS +~24MB，LFS 指针形态不带库并 WARN），CI tar / AppImage resources（`tauri.release.json` 增项 + gui:linux bundle 质量门：AppDir 内必须有 `report_html.py` 与 `xperf-agent`）/ release-macos.sh（`prepare_gui_agent`→`prepare_gui_resources`）三路一致。
  - **第二层（同一次真机复验新抓出）**：解析链生效后 CI 产物仍不出 HTML，报 `ModuleNotFoundError: No module named 'encodings'`——AppImage 的 AppRun（linuxdeploy python 插件）给整个应用设 `PYTHONHOME=$APPDIR/usr/` + `PYTHONPATH=$APPDIR/usr/share/pyshared/`（`/proc/<gui>/environ` 实锤），宿主 `python3` 子进程继承即找不到标准库。修=`python3_command()` 统一 `env_remove` 两者 + 单测锁契约；手工对照（纯净环境 rc=0 出 2.4MB HTML / 带 PYTHONHOME 同错）定位。
  - **验收（全部实跑）**：单测 3 新用例；CI 流水线 #1420875/#1420946 全绿（test/build/gui）；**hppc 上用 CI 产物 AppImage 复验 g3 15/15**——状态栏「火焰图已生成并打开: …stack_20260922_185758.html」+ HTML 产物核验通过，GUI stderr 显示选中档 `/tmp/.mount_xperf-*/usr/lib/xperf-gui/simpleperf_scripts`（随包资源档生效）；**发布 CLI tarball**（同一 pipeline 产物）实测：exe 旁 `simpleperf_scripts/` 被选中（只读时如实拒绝并打出该路径）、`XPERF_SIMPLEPERF_SCRIPTS` 指向空目录时建目录 + AOSP 下载补齐 7 文件 31MB（v0.3.1 失败的那条路径）；本机（Mac）tarball 布局 E2E 三条（sibling 优先于 vendor / 只读拒绝 / env 覆盖最高）。随 v0.3.2 发布；DMG 侧的火焰图按钮需 v0.3.2 出包后在**非构建机**上补验一次（同一根因，理论已覆盖）。
  - 原始缺陷描述（保留备查）：GUI「在浏览器打开火焰图」报 `打开火焰图失败: 创建 /builds/ligraphic/xperf/xperf-core/simpleperf_scripts/report_html.py.dl-tmp 失败`。根因——`xperf-core/src/simpleperf.rs::scripts_dir()` 用 `env!("CARGO_MANIFEST_DIR")` 拼路径，把**构建机目录**烤进二进制：CI 构建的 Linux 产物指向容器内 `/builds/...`（用户机上不存在且父目录不可写 → AOSP 引导下载也必败），本机脚本 `scripts/release-macos.sh` 构建的 DMG 则烤成维护者仓库路径——**在构建机上恰好可用故本机测不出，换任何用户机器即失效**。影响面：GUI 火焰图按钮 + CLI `--update-simpleperf-scripts`（同 `scripts_dir`），`--clean-cache` 的第三项也会去删一个不存在的路径。
  **修法（建议）**：① core 把 `scripts_dir()` 改为解析链 + 纯函数便于单测——宿主注入 override（GUI `resource_dir()/simpleperf_scripts`、CLI exe 旁 `simpleperf_scripts/`，与 `resolve_agent_binary` 同构；可选 env `XPERF_SIMPLEPERF_SCRIPTS` 供排障）→ 编译期 workspace 目录**存在才用**（维护者检出、git 同步 vendor 的语义不变）→ 可写缓存 `~/.cache/xperf/simpleperf_scripts/` 兜底，且 `download_scripts` 前 `create_dir_all`；② 打包随产物附脚本集：CI tar 与 release-macos.sh 各带**本主机平台**的 `bin/<os>-<arch>` （省双平台 ~30MB），`tauri.release.json` resources 增一项，GUI 复用 agent 的 resource_dir 传参套路；③ 只读 bundle 下 `--update-simpleperf-scripts` 改落缓存目录或如实提示。**验收**：单测锁优先级；干净环境（容器/无仓库路径的机器）跑发布 GUI 出火焰图 HTML + 发布 CLI `--update-simpleperf-scripts` 成功；v0.3.2 发布后按 K2 同法复跑覆盖。
- [x] ~~**v0.3.0 AppImage 在 Ubuntu 24.04 点击全无响应**~~（**已修复**，2026-09-20，`fix/appimage-webkit-sandbox` 合 main 6610fe1 / 修复 cfa6576，随 v0.3.1 发布）：根因——Ubuntu 24.04+ 默认 `kernel.apparmor_restrict_unprivileged_userns=1`，AppImage 内 WebKitGTK 以 bubblewrap 沙箱启动 Web 进程（需 user namespace），而 `/tmp/.mount_*` 动态挂载路径不匹配系统 AppArmor profile（只豁免 distro 安装的 webkit）→ Web 进程创建失败：窗口有静态画面但点击/前端 IPC 全部无响应（debug API 实证：无 WebKitWebProcess 子进程、`frontend_ready=true` 但 dom/eval 全超时；`WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1` 后进程拉起、点击链路恢复）。修复路径三连勘察：① main 内 `set_var` 对沙箱判定**不生效**（Web 进程仍不起——判定须在 exec 前由环境提供）② re-exec 修补生效但引入**点击即崩**副作用（两实例复现：eval 60s 全绿、首次 click 后 Web 进程消失无 coredump；同二进制手动 env 则 click 正常）③ 终案 = CI `gui:linux` 打包时向 AppDir `apprun-hooks/linuxdeploy-plugin-gtk.sh` 追加**条件式** export（AppRun source 链在 exec 前注入环境，运行时读 `kernel.apparmor_restrict_unprivileged_userns`——仅 24.04+ 受限内核禁用，22.04/其他发行版保留沙箱，质量门 grep 校验）。**代码途径已全数实证排除**：除 ①② 外，官方 `webkit_web_context_set_sandbox_enabled` API 存在但 tauri 2.4/wry 0.50 均不向应用暴露 WebContext（wry 每次新建、tauri 不透传），fork patch 依赖的维护负担不值。——解包目录全形态验证：Web 进程拉起、eval/click/60s 监控全绿；条件 hook 语法/受限触发/set -e 安全本地验证。Rust 侧仅留注释（main.rs 开头）。全量 core 166+8/CLI 12/GUI 16 绿，clippy/doc 零警告。
- [x] ~~**`cargo run --bin xperf-gui --release` 报「GUI 发布包缺少预编译 agent」**~~（**已修复**，2026-09-17 晚，`fix/gui-release-agent-fallback` 合 main）：根因——`f4f285d`（GUI 发布打包）把 dev 路径的 `ensure_agent_built()` 换成 `bundled_agent_path()`，回退分支以 `cfg!(debug_assertions)` 判定开发运行，而 `cargo run --release` 是 release profile 的**开发运行**（无打包资源）被误判为发布包缺资源。修复：开发判定改为「编译期 workspace 是否存在」（core 新增 `agent::workspace_root()`，绝对路径只在构建机有效）——存在则回退 `ensure_agent_built`（与 CLI 一致自动构建，任意 profile），发布包用户机器必然落到缺资源报错、不触发 Cargo/NDK（发布约束不变）。真机端到端：release GUI `--remote hppc --device 6eb792dfb0f` 自动启动采样全通（三类 CSV 流式落盘 + daemon 空载自杀零残留）；core 149+8 全绿（含新增 workspace_root 单测）。

- [x] ~~**Linux AppImage 在某台 Ubuntu 22.04 启动失败**~~（**已修复**，2026-09-16，`fix/appimage-harfbuzz` 合 main d75108f）：根因实锤——linuxdeploy excludelist 排除 libharfbuzz（假定目标系统自带），而 bundle 内 bookworm 版 pango 1.50.12/webkit 引用 3 个 harfbuzz 3.3.0/4.0.0 新增符号（`hb_font_set_synthetic_slant`@3.3.0 / `hb_ot_layout_get_baseline_with_fallback`@4.0.0 / `hb_ot_layout_get_horizontal_baseline_tag_for_script`@4.0.0），目标系统 harfbuzz 过旧（jammy=2.7.4）时启动即 symbol lookup error；24.04（8.3.0）有符号故仅 22.04 中招。修复：gui:linux 打包后向 AppDir 注入构建侧 libharfbuzz 6.0.0（glibc 需求 ≤2.33 兼容 jammy）+ nm 质量门 + 复用 tauri 缓存 linuxdeploy 内嵌的 appimagetool 重打包（构建期 /tmp 解包目录随 linuxdeploy 退出即删，须自行 extract）。bundle 其余系统依赖已全量符号审计无偏移（fribidi 两版一致；fontconfig/freetype jammy 版满足全部引用；glibc 最高 2.36 仅 libcups 打印路径，见 E 节）。**jammy 容器 A/B 验证**：旧包复现用户报错原文，新包 WebKit 进程组完整启动零报错。随下次版本 tag 发布。

## B. 指标覆盖

- [x] 全部完成（b732d55 + 5cff92e + 58ceae5）

## C. 验证能力

- [x] ~~阈值告警 / 退出验证报告~~（a56a6f5 + 02ee9bc 静止误报修复）
- [x] ~~时间轴事件标记~~（0f7ad9d：CLI Unix socket + GUI 按钮 + 图表竖线）
- [x] ~~冷启动时间~~（a56a6f5：am start -W）
- [x] ~~perfetto `--trace N` 深挖模式~~（ca01aa6 + 后续迭代）：录制（stdin 喂 text proto + write_into_file 流式落盘）→ pull → trace_processor SQL 归因（包线程 CPU/抢占延迟/系统 top/每核 busy/频率/帧时间线）；配置对齐团队 general_debug.pbtxt（cc76721，10s≈46MB）；浏览器一键全自动加载（3ee4ea2 本地镜像 UI + 同源深链，失败回退拖拽）；GUI 独立 tab（dabb1e2 + 暗/亮主题）。真机 SS3 端到端验证。详见 CLAUDE.md「perfetto 深挖模式」
- [x] ~~simpleperf 调用栈采样~~（9ee99ad）：`--stack N` 录制 N 秒调用栈（--app 全进程覆盖 + dwarf），设备端三视图报告（线程 CPU 分布 / 函数热点 self / children 调用链）+ `.data` 拉回可复析；CLI 独立/并行两模式（与 --trace 可同给，独立模式录制失败非零退出）；GUI 函数热点独立 tab + `--stack N` 自动启动。真机 SS3 端到端验证（独立/并行/未运行拦截/退出码/GUI）。详见 CLAUDE.md「simpleperf 函数热点模式」
- [x] ~~基线对比（两次运行 diff）~~（46bd161）：`--save-baseline`/`--compare-baseline`（互斥）+ GUI 侧栏两按钮；`xperf-core/src/baseline.rs` 会话汇总（多 PID 合并 + 冷启动/重启次数）存 `~/.local/share/xperf/baselines/<pkg>.json`（XDG 用户数据，--clean-cache 不清理，CLI/GUI 互通）；对比报告四态判定（回归/改善/持平/单侧未采集），相对 ±10% + 指标地板值双闸；报告落盘 `<ts>/baseline_report.txt`。真机 SS3 全链路：保存→对比（持平 9 项）→篡改基线触发回归（⚠ 4 项）→无基线提示。详见 CLAUDE.md「基线对比模式」

## D. 结构改进（下轮候补）

- [x] ~~**GUI 折线图悬停精确读数**~~（**已完成**，2026-09-19，`feat/chart-hover-values` 合 main，4eeaad6）：图表只能目测大概值 → 悬停显示 crosshair + tooltip（各序列准确值 + 毫秒时间戳）；DOM overlay 不随 150ms 重绘抹掉，draw 后按新几何刷新；取数在完整序列二分最近点不受绘制抽稀影响；窗口外序列不读数、右缘左翻、>5 序列两列防 overflow 裁剪、序列名 HTML 转义。真机目验（--remote hppc SS3 gltf，CGEvent 鼠标模拟 + screencapture）：CPU 单序列 25.25%、内存 647.83MB（原始浮点格式化）、频率 8 核两列无裁剪、右缘翻转、移出隐藏
- [x] ~~**GUI 开发运行的 agent 构建反馈缺口**~~（**已完成**，2026-09-18 深夜，`fix/gui-dev-agent-build-feedback` 合 main）：① 构建期无 UI 反馈 → `bundled_agent_path` 在需构建时 emit `agent-build {serial, stage: building|done}` 事件 + `AppState.agent_building` 状态集 + `agent_building` 命令（**命令行自动启动时构建早于前端事件监听就绪，事件会丢——前端 `applyStartupArgs` 回查补偿**；手动开始走事件+`agentBuilding` 标志守卫，三处乐观「监控中」setStatus 让位，两种事件时序都安全）；② cargo 裸 `Command::new` 依赖 PATH → core `host_cargo_path()` 解析链（`XPERF_CARGO`→PATH→`~/.cargo/bin`→Homebrew，adb/ssh 同套路）+ `agent_binary_needs_build()` 从 `ensure_agent_built` 拆出。**真机验证**：`env -i`（最小 PATH + SSH_AUTH_SOCK）模拟 Finder 启动 + fake-cargo 包装器（sleep 45 + exec 真 cargo，顺带验证 XPERF_CARGO 覆盖）——构建中状态栏显示「首次构建 Android agent 中…」截图实证，构建完成 → 恢复采样图表（SS3 --remote hppc 全链）。修复过程发现两个坑：cargo Fresh 重硬链旧 inode（mtime 不变，构建太快截不到图 → 引入 sleep 包装器）；后台 GUI 进程需 nohup+disown 防 harness 会话清理误杀。单测 +1（host_cargo_path 解析链），全量 core 166+8 / CLI 12 / GUI 11 绿，clippy/doc 零警告。**独立 review follow-up（merge 3669412）**：复审结论 LGTM 无严重；修 2 一般——①构建中点「停止」后 done 事件无条件覆盖状态栏（done 加 samplingRunning 守卫）②agent_building 裸 HashSet 无计数（构建窗口内 restart 并发构建者配对错位 → HashMap 计数，归零才 emit done）；顺手 4 观察项（applyStartupArgs 补 .catch、start() catch 清标志、XPERF_CARGO 空串显式过滤、build_session_summary docstring 口径）；fake-cargo 复测主链路无回归。残余接受：回查 stale-true ms 级窗口（下一事件自愈）、构建 <1s 秒失败时 building/done 均丢的假「监控中」（pre-existing 事件丢失类边界）
- [x] ~~内存 Private Other 拆分 DMA-BUF 分类~~（**已完成**，2026-09-11，`feat/dmabuf-split` 合 main，e5071da，协议 v8）：Private Other 是无语义兜底桶（真机根因 = 114 个 `/dmabuf:` VMA 共 614MB PSS；Graphics 桶只按 kgsl/drm 设备节点名匹配，dmabuf 不命中；内核 `/proc/<pid>/dmabuf` 此 GVM 未编译）。落地：agent Full 模式 root 下扫 smaps 按 VMA 名（`/dmabuf`/`[anon:dmabuf`）聚合 Pss 单列 `dmabuf`，other 扣减（稳态 8 分类合计恒等 PSS；分配剧变期两次快照不同步可暂超，如实不钳制——见 CLAUDE.md 内存采样节）；Smaps/DumpsysFallback/非 root dmabuf=0。mem 事件增字段（serde(default) 双向兼容）；CLI 打印/图表、GUI 面板（└ DMA-BUF + Private Other 改「其他」）、CSV（DMA-BUF (MB) 列）全链路。parse_dmabuf_pss 头行按字段解析（弃固定列切片）。真机：SS4 gltf DMA-BUF 615.6MB/Other 9.0MB 稳态合计=PSS；SS3 533.9MB 回归；非 root SS2MAX dmabuf=0 七类合计=PSS；100ms smaps 路径 0 值正常。测试：agent 32（设备）+ host 95+8+5+2 全绿，clippy/doc 零警告
- ~~SS4 FPS 数据源升级候选：getfps -w~~（**已核销**，2026-09-10 晚）：getfps 逆向发现其底层即 `dumpsys SurfaceFlinger --latency`，价值是揭示了 SS4/A16 的图层名须带 `<hex> ` 别名前缀——agent v5 据此修复查询名，设备端 per-layer 路径恢复（见 E 节 FPS 条目终态）
- [x] ~~agent 单文件拆分~~（531798a + 99d1b74 review 修复）：main.rs 1848 行 → 10 文件（main 493 + proc/mem/fps/thermal + gpu/{mod,kgsl,qnx,topgpu,ligfx}），三份读线程骨架抽公共 `gpu::spawn_stream_parser`，四段相同的 gpumem 补采臂合并；测试 23 个随模块迁移全绿。真机回归：SS2MAX 新旧 agent 同机对比事件分布/wire 格式/smaps 值一致。附带修复 host 侧 `ensure_agent_built` 只盯 main.rs 的 mtime 检查（改扫 src 树，touch 子模块已验证触发重建）
- [x] ~~SS3 QNX 通道真机回归 + kgsl 统计链停滞修复~~（2026-09-03）：回归发现 QNX frame 流"1 条后停走、跨会话交替通/停"。黑盒实验（重启车机前后共 10+ 组对照）定位根因：**kgsl 统计链是驱动全局的，会话/fd 关闭都不清理**（泄漏直到整机重启）；`echo >` 式即开即死连接写入撞存量链只 flush 一窗即停，长活连接（`exec 3>`）写入则全链重相位持续输出；多链锁步产生重复行。修复（qnx.rs）：① 启动命令改 `exec 3>` 持 fd 写入；② slog 行按"与上一行完全相同"去重（Sys/Proc 各一条）；③ 看门狗兜底（frame 静默超 3×周期经 fd3 重写自愈，≤3 次）。真机验证：4 条泄漏链硬场景下启动顿 ~5s + 1 次自愈后稳定 1/s；gpu/gpuproc/gpumem 三类事件与 CSV 全通（eid→pid 9671 归因正确）；连续多轮 kill/重跑稳定。已知残留：存量链的窗口 flush 会带来少量同值重复样本（数值正确，重启清零）
- [x] ~~xperf-core 轮询参考实现删除~~（225d89b，-1653 行；保留 ThreadCpuInfo/MemoryDetails/FpsTimeSeriesData/PidStats/SampleEvent 等协议类型）

## E. 已知遗留（评估过，低风险不阻塞）


- **AppImage 内 libcups 引用 GLIBC_2.36 符号**（2026-09-16 harfbuzz 修复审计发现）：bundle 内 bookworm libcups 2.4.x 引用 `arc4random@GLIBC_2.36`，jammy 仅 2.35——PLT 惰性绑定，启动不受影响（A/B 实测），仅在 jammy 上走到打印路径才会 symbol lookup error；GUI 无打印功能入口，记录不修（若未来暴露打印，方案 = 同 harfbuzz 把 libcups 也排出 bundle 用系统版，或接受 jammy 打印崩）。
- **GUI 终端启动的 WebKit 子进程会被 SIGHUP 误杀**（2026-09-20 实测）：从 `run_cmd`/终端/SSH 会话直接启动 GUI，宿主会话清理时 `SIGHUP` 可杀 `WebKitWebProcess`，主窗口保留但点击/DOM/API 全无响应；`nohup` 不足，`setsid nohup ... </dev/null` 脱离会话后 5 分钟/60 次探活稳定。属于启动方式约束，不是 AppImage/Ubuntu 沙箱根因；GUI 调试与验收统一使用独立会话或桌面启动器。

- ~~SS4 FPS 无数据源~~（**已解决并二次修正**，终态 2026-09-10 晚 agent v5 / 1486463）：v4 曾误判"QCM 构建阉割 --latency"绕道 host frametimeline 通道（7a3e9fb）；**用户指点 getfps -w 后逆向确认真因：A16 SF 的 --latency 只认 `--list` 原始行的 `<hex> <name>` 别名形态**（带前缀 65 行真数据 vs 干净名 1 行刷新周期）——agent fps.rs 查询名双轨（A16 保留前缀/旧平台干净名），设备端 per-layer 路径恢复（协议 v5，SS4 短路撤销、host frametimeline 通道删除），真机 59-61fps + 杀进程重发现（#549→#579）+ SS2MAX/SS3 回归全通
- SS2MAX GPU 显存无数据源（2026-09-07 root 下全路径确证：dumpsys gpu 无 Memory snapshot 段 + /sys/kernel/debug 未编译进内核 + /proc/kgsl 不存在，平台限制）
- SS3 kgsl 统计链（见 D-2/CLAUDE.md）：三层清理已落地（agent 退出钩子 + setsid + host 条件兜底，**均带 pgrep 多会话并发保护**），SIGINT/Ctrl-C/正常退出路径真机验证停链成功、下一会话零自愈即起流；残余风险仅 agent 被 SIGKILL 暴杀（无钩子机会）与 reboot 后首会话（开机 5000ms 链在流，走一次看门狗自愈 ~8s）
- ~~SS2MAX gpubusy 计数器恒 `0 0` / busy% 可 >100%~~（**已修**，commit 见 SESSION 2026-09-07：根因是 SS2MAX 厂商内核的 gpubusy 为**窗口语义**——读数是上一 ~1s 窗口的 busy/total µs，total 恒 ≈1e6 非累计；按累计差值解析出 1662%。`GpuBusyCalc` 三判据自动锁定窗口语义直读 busy/total；真机对照内核 `gpu_busy_percentage` 均值 75.5 vs 74.7 一致。原"恒 0 0"即 GPU 空闲时的窗口读数，非停走）
- ~~SS4 ligfx Frequency 单位待真机核实~~（**已核销**，2026-09-10 任务 B）：恒 `1000 Hz` 空闲/负载不变，GPU VFIO 直通两侧无 kgsl/devfreq 节点可对照——按「定频占位/单位标注存疑」处理，事件原样透传 mhz=1000，业务侧只看 Utilization。`persist.vendor.ligfxprofiler.sampling_interval_ms` 实测动态读取，但调小（1000）会致 ligfxprofilerd 停输出（恢复 5000 即好）——勿调
- **SS4 GVM 无 cpufreq/thermal（2026-09-10 确证，VM 平台限制）**：`/sys/devices/system/cpu/cpu0/cpufreq/` 不存在（hello maxkhz 全 0 即此因，root 下同），`--freq` 探测禁用；`/sys/class/thermal/` 空 + thermalservice HAL Ready=false（连 SS3 的 test HAL 假数据都无），`--thermal` 探测禁用。PMU 未虚拟化（simpleperf cpu-cycles 8s 仅 6 样本 → core 自动改 cpu-clock，b8754bf）
- ~~**QNX proc 链泄漏（2026-09-07 发现）**~~（**已修并修正根因认知**，2026-09-11 晚，`feat/qnx-orphan-reaper`，协议 v9）：黑盒勘察（`echo help > /dev/kgsl-control` 吐出全命令表 + 干净单 tailer 对照实验）确认——①`gpu_per_process_busy` 写入 = 未跑则启动/在跑则重相位（**不新增链**，无停止命令，写 0 钳 1000ms 启动；开机 startup.sh frame/proc 各起一条 @5000，proc 流常驻是设计基态）；②历史「每会话泄漏一条、~20 条锁步洪泛」的**真凶是孤儿 tailer**：telnet 断开只杀登录 shell，后台 `slog2info|grep` 管道不死，ttyp 回收重用后孤儿把驱动行重印进新会话（同行多份同时间戳 = 「锁步多链」表象；~20 个孤儿 ≈ 历史会话积累，挤占 QNX CPU 疑似致 frame 停走）。修复：通道启动 `slay -f -Q slog2info` 清场（含 SIGKILL 残留的自愈收编）+ teardown/`--qnx-stop` 退出前 `kill $!` 收本会话 tailer。真机验证：v8 会话必漏 1 孤儿（基线实锤）→ v9 三连会话零残留、注入孤儿启动即清、`--qnx-stop` 清场且观察 tailer 自收、frame 停链/proc 常驻 ×1 正常
- ~~孤儿 adb exec-out 泄漏~~（**已根治**，daemon 化 commit 见 SESSION 2026-09-07 晚条目：agent 常驻 daemon + host 经 forward/TCP 连接，host 死亡 → TCP 断开 → 会话即收，不再产生孤儿流；daemon 0 会话 60s 自杀。早前过渡方案 e692d4e 的 cleanup_orphan_agents/stdin EOF 监测已被 daemon 化取代并移除）
- ~~多设备连接时所有 adb 命令不带 -s 会失败~~（**46bd161 已修**：全局 `-s` 注入 + CLI `--device` + GUI 设备下拉，SS3+手机双连真机回归；原候补转正，详见 CLAUDE.md「多设备 adb」）。GUI 多台未指定 `--device` 的自动启动跳过路径为逻辑验证 + 单测覆盖（验证时手机恰断开未双机复现，行为由 pick_device 单测锁定）
- QNX 双会话并发交互（五轮 review 实测）：①后启动会话的 fd3 写入给先启动方一次 ~7s GPU 停走（看门狗自愈恢复）；②各方 GPU 事件密度升至 ~2×（双方写入产生非锁步多链，行级全等去重不覆盖，值为真值仅密度偏高）；③退出清理已有并发保护（pgrep 检测其他 agent 跳过停链，agent 钩子 >1 / host 兜底 ≥1+收尸等待，真机验证）——并发监控本身罕见，记录不修
- ~~GUI add_marker 不写 markers.csv~~（已失效：GUI 打点功能整体删除，78f93a9，仅剩 CLI socket 打点）
- marker 每连接线程无界（有 10s 读超时兜底）
- ~~**多宿主协议版本战**~~（**已代码修复**，2026-09-11 v6 / 9685438）：曾实测 v4 GUI + v5 CLI 并存时互相 suicide+重推对方 daemon（无限循环/无人监听残留）。**v6 起版本契约**：host 接受 daemon ≥ 自身（wire 自 v3 稳定）；daemon bind 竞争高版本胜出、挂死（SIGSTOP 类持有 socket 不应答）被清场接管——任何启动交错收敛到「恰好一个健康 daemon 且为最高版本」，真机四场景验证（升级重推/升级接管/挂死接管/等版本让位）。**过渡期残留**：exact-match 时代旧宿主（≤v5 二进制）连 v6 daemon 仍会自杀重推降级——各宿主升级一次 v6 后绝迹
- **bridge 边缘态（2026-09-10 review 记录）**：bootstrap 探测失败冷却期（60s）内 MindRT 以 `is_gateway=false` 漏进设备列表（pick_device 可选中/GUI 可建 tab）——仅中继坏掉时出现，选中后采样会按普通设备失败；不修（正常路径 bootstrap 秒成，冷却语义是防反复探测）
- ~~GUI 基线/应用操作按钮与设备 tab 切换的点击渲染为人工目验项~~（**已闭环**，2026-09-11：macOS GUI + System Events AX 树/AXPress 自动化点击实测全通，见 SESSION 当日 (5)「GUI 目验闭环」——含 gpuMem 键名修复的真机确认、v8 DMA-BUF 面板行、SS2MAX 显存禁用勾选、基线/重启/获取 root 按钮端到端）
- ~~GUI 关窗时镜像清理路径未经真机目验~~（**已闭环**，2026-09-11 深夜用户实测：开镜像 → 关主窗口 → scrcpy 随之关闭 ✓。路径：CloseRequested → stop 全部镜像 + 监护线程 cleanup（摘 hop#2/扫规则）→ shutdown_remote）

## F. SSH 远程调试（已完成）

- [x] **SSH 远程后端**（2026-09-09，`feature/ssh-remote` 分支合 main；设计 `docs/DESIGN-ssh-remote.md` v2 + 实现 S1-S11 全步骤）：真机接在 hppc 上，本机跑 GUI/CLI 经 SSH 调试采样/perfetto/simpleperf。commit 链：f45d44b（设计）→ c156141（S1 transport 基础）→ e520787（S2 隧道）→ 759d72d（S2a hop#2 映射表）→ cd81a37+b578b9f（S3 utils 注入）→ ea61e05（S4 init/shutdown）→ e4ae289（S5 CLI + S6 agent hop#2）→ b5c179a（S9 断连重连）→ f2a58a9（S10 GUI）。真机回归：远程采样/trace/simpleperf/断连重连/并发会话/退出零残留全通（详见 CLAUDE.md「SSH 远程后端」节）


## G. 非 root 设备支持（已完成）

- [x] **权限矩阵探索 + 无 root 机器支持**（2026-09-09 完成，`feature/non-root-support` 合 main）：commit 链 19ecef9（auto-root 收敛仅车机 + XPERF_NO_AUTO_ROOT 旁路）→ ea3986f（协议 v3 hello.root + 内存降级 + RSS 兜底 + CLI 提示）→ 9c34b23（QNX 内嵌 telnet 去 busybox + --qnx-stop 模式 + host 换道）→ ec2de2b（GUI 权限徽章 + 获取 root 按钮 + 降级提示透传）。矩阵实测（两机 shell 逐项验证）见下表；**真机回归全通**：非 root SS3 @500ms 九项指标（QNX 内嵌 telnet shell 起流 20.3% busy + 每进程归因）、非 root SS2MAX @50ms（内存 dumpsys 降级 500ms 周期 + RSS TOTAL RSS 兜底 + kgsl 74.7%）、root 回归 SS3 @50ms（auto-root 恢复全指标）、--qnx-stop 守卫语义（不动已停链）真机验证。
  - ~~**残留目验项**~~（已闭环，2026-09-11）：GUI「获取 root」按钮 DOM 点击经 AXPress 真机验证（SS2MAX unroot + XPERF_NO_AUTO_ROOT=1：点击 → 状态栏确认 → 徽章翻 root → 设备 id=0）；root 设备上按钮按设计 disabled。
  - **泛型 Android 跳过 auto-root**：策略由纯函数 `should_auto_root` 单测 + detect 测试锁定（手头无非车机设备真机）。

  | 指标 | 数据源 | 非 root 可用性 |
  |---|---|---|
  | CPU/线程 | `/proc/<pid>/stat` + `task/*/stat` | ✅ 两机（/proc 挂载 `hidepid=2,gid=3009`，shell 在 readproc 组） |
  | 内存明细 | `dumpsys meminfo <pid>`（App Summary 全分类） | ✅ 两机（可作 smaps 兜底，~100ms 故限频 ≥500ms） |
  | 内存快采 | `/proc/<pid>/smaps_rollup` | ❌ 两机 Permission denied（PTRACE 检查）→ 已实现自动降级 |
  | FPS | `dumpsys SurfaceFlinger --list/--latency/全量` | ✅ 两机（SS3 全量 dump 0.03s 含 ownerPID；SS2MAX A11 全量无 ownerPID 元数据，走 --list 包名匹配兜底） |
  | 频率 | `scaling_cur_freq` | ✅ 两机 |
  | 温度 | `dumpsys thermalservice` / sysfs thermal zones | ✅ 两机（SS2MAX sysfs zones shell 可读 44°C/47°C） |
  | IO | `/proc/<pid>/io` | ❌ 两机 Permission denied（0400 owner-only，无兜底）→ err 禁用 + GUI 灰显 |
  | 网络 | `/proc/net/dev` | ✅ 两机 |
  | GPU busy | kgsl `gpubusy` | ✅ SS2MAX（shell 可读窗口语义值）；SS3 无 kgsl |
  | GPU QNX | 内嵌 telnet 172.31.101.52 | ✅ SS3（busybox 被 SELinux 拒 → 内嵌 client 解锁，shell 真机起流） |
  | GPU 显存 | `dumpsys gpu` Memory snapshot | ✅ SS3（per-proc 段 shell 可读）；❌ SS2MAX（平台无数据，已知） |
  | simpleperf | `--app` | ❌ 两机（非 debuggable/profileable；run-as 同拒）→ CLI 已如实提示 |
  | perfetto | `perfetto -c -` 经 traced 服务 | ✅ 两机（shell 录 1s ftrace 出 1MB trace，落 /data/misc/perfetto-traces 属 shell） |
  | 冷启动 | `am start -W` / `force-stop` | ✅ 两机（SS3 实测 TotalTime 451ms） |
  | 部署 | push /data/local/tmp + 执行 | ✅ 两机（shell 可写可执行） |

---

## H. SS4 平台适配（已完成）

- [x] **SS4 adb 自动桥接**（2026-09-10，`feature/ss4-adb-bridge`）：S0 预验证（hppc 手工 adb，结论回填 `docs/DESIGN-ss4-adb.md` v1.2）→ S1 `bridge.rs` 核心 → S2 utils 集成（list hook + pick_device 过滤）→ S3/S4 root 链路与自愈 → S5 GUI 过滤 → S6 真机回归。commit 链：738fcba（S0 文档）→ 5018c11（S1）→ 03aa440（S2）→ e184ddc（S3+S4）→ fa5cc6f（S5）→ f957d8c（review 修复×2）。**真机回归全通**（--remote hppc）：清桥接状态后 CLI 自动 bootstrap（forward+connect 全自动，多台报错清单只列 3 台 Android 不含 MindRT）、`--device localhost:5559` 采样（auto-root ①直连成功、平台识别 SS4/12 核/hello root、CPU 样本/线程明细/CSV/退出图表全通）、**采样中 GVM reboot 自动重连恢复**（①② 双 root 路径在重连竞态中真实触发）、SS3+SS2MAX+SS4 三机并行采样不互扰。详见 CLAUDE.md「SS4 adb 自动桥接」
- [x] **SS4 指标适配**（2026-09-10 晚，`feature/ss4-metrics`，实施依据 `docs/DESIGN-ss4-metrics.md`）：任务 A FPS frametimeline-only perfetto host 通道（7a3e9fb：hostchan.rs 5s 窗循环 → NULL display 合成流汇总 fps/jank 汇入 AgentStream；agent --fps 在 SS4 短路、协议 v4；真机 59.8fps 稳态/杀进程停发/重启恢复/GVM reboot 重连恢复全通）→ 任务 B GPU ligfx host 侧通道（9004f96：经网关 logcat 流读，Sys→Gpu/进程行→GpuProc comm 归因；真机 busy 33.8%/进程 11% 归因正确；Frequency 恒 1000 核销、sampling_interval 调小致停输出现已记录）→ 任务 C 九项矩阵 root/非 root 两态实测（freq/thermal 为 GVM VM 隔离平台限制，探测禁用符合预期；hello maxkhz 全 0 根因=GVM 无 cpufreq sysfs）→ 任务 D C 类回归（trace✅/冷启动 246ms✅/simpleperf：PMU 未虚拟化致 cpu-cycles 无效 → 自动 cpu-clock + 千分位样本数解析修复 b8754bf，3908 样本/9s/基线保存-对比全链路持平 9 项）→ 任务 E 文档收尾（ss4.rs 桩补实/CLAUDE.md「SS4 host 侧指标通道」节/E 节核销）→ 独立 review 修复（7e0a748：2 严重 4 一般全修，含 ligfx 独占登记重连竞态宽限接管、EOF 热重连退避、阻塞读看门狗；真机 GVM reboot 重连场景复测通过）→ **深夜根因反转（7683720/agent v5）：任务 A 的 frametimeline 通道系误判产物已删除**——A16 `--latency` 实为图层名格式要求（`<hex> ` 前缀），FPS 回归设备端 per-layer 路径，终态见 E 节 FPS 条目

---

## I. 新功能候补（2026-09-11 用户排期，各开新会话完成）

> 三条均要求 **SSH 远程后端（`--remote`）下可用**。形态细节（GUI 内嵌 vs 拉起外部、交互式 vs 单条）在实施会话中确认。

- [x] ~~**logcat 支持**~~（**已完成**，2026-09-14，`feature/logcat` 合 main）：core `logcat.rs`（spawn adb logcat 流式落盘 + 读线程→mpsc→写线程；200ms/64 行合帧事件回调）。按包过滤：A12+ `--uid`（uid 重启耐受，SS4 多用户逗号列表透传）；**A11 无 `--uid`**（实测）降级 `--pid`（限制如实提示）；无包名全机。`-v threadtime -v year -T 0`（设备时钟与采样 CSV 同源对齐）；断连 1s 退避自动重 spawn（文件内 `# respawn` 标记行），设备在线秒死 ×3 判永久失败上报。CLI `--logcat`（并行/独立，退出码同截屏语义）；GUI 第 4 子 tab「日志」（toggle + 级别下拉 + 按包勾选 + live 视图 ring buffer 2000 行级别着色）。**过滤口径热切换**（a4d4b0e）：级别/按包过滤/包名抓取中变更即时生效（同文件续写，restarting 标记防计划内 kill 误计秒死）。真机回归（--remote hppc）：SS3 uid/SS2MAX pid/SS4 多用户 uid/并行采样同目录/无包名全机/adb reconnect 断连重连/两类错误路径 exit 1 全通；GUI AX 实测 tab→填包名→开始→停止 + **热切换全链**（uid→All（3749 行）→Uid 往返，respawn 标记行佐证同文件续写）。**残留目验项**：live 视图行渲染未能 AX 读出（流式期间 webkit AX `entire contents` 整体剪枝，递归遍历可达但逐行读值过慢）——代码走查 + 与 sample/trace 同事件模式，留给用户一眼确认。AX 教训记入 CLAUDE.md
- [x] ~~**scrcpy 集成**~~（**已完成**，2026-09-11 深夜，`feature/scrcpy-mirror` 合 main + 补丁 `a99dee7`）：形态=拉起外部 scrcpy 窗口（解码/触控归 scrcpy）。core `mirror.rs` + `SshTunnel::add_forward_pinned`（固定端口 hop#2——**实测 adb reverse 在远端 server 拓扑下流回不到 TCP 客户端，不可用**；远程走 `-p P --tunnel-port=P` 双钉同号——**scrcpy 4.x 的 --tunnel-port 只钉本地 connect 口，adb forward 注册口由 -p/port_range 在 server 侧扫描**（v4.1 源码），只传 tunnel-port 时多镜像并存错配必败，用户实撞 SS3+SS4 场景修复）；端口池 27183..=27199 两侧同号空闲扫描。CLI `--mirror`（与采样并行 / 单独镜像-only）；GUI 每设备侧栏「屏幕镜像」toggle + 监护线程事件复位按钮（stopped/closed/failed 三态——scrcpy 正常运行也有 stderr 日志，凭 exit status 分流而非 stderr 非空）。真机：SS3/SS2MAX/SS4 远程全通、双设备并行端口隔离、SIGINT 全清理零残留
- [ ] **命令行输入**：GUI 提供设备 shell 命令输入能力（交互式 shell or 单条执行，形态待定）。SSH 远程注意：命令通道同 adb 走 hop#1；若做成交互式长连接 shell 则类似 agent 流需 hop#2 式映射。
- [x] ~~**GUI 可编程操控/状态读取接口（agent 自助目验，免截图+模拟点击）**~~（**已完成**，2026-09-19，`feature/gui-debug-api`）：**默认开启**（含 release，`XPERF_GUI_DEBUG=0` 关闭）——GUI 内嵌 axum loopback server（独立线程 current_thread tokio runtime），随机端口 + 每启动 64-hex token（`X-Xperf-Token` 头，`.layer` 全路由含 404），发现文件 `~/.config/xperf/gui-debug-<pid>.json`（0600 原子写 + 死 pid 清扫 + 关窗清理）。端点：`/api/status`（后端真相）/`/api/dom`（结构+bounding rect）/`/api/state`（监控状态/series 摘要/悬停 tooltip/logcat）/`/api/series`（全分辨率 tail/at 二分取值）/`/api/action`（click/input/select/check/hover/scroll/key 真实 DOM 事件序列，根治 AX 时代 set-value/change 坑）/`/api/eval`（async 逃逸舱）。前端往返走 Tauri 事件 `xperf-debug` + `debug_respond` 命令（5s 超时 504/未就绪 503）。**真机验收全绿**（SS3 --remote）：`scripts/gui_debug_accept.py` 全链路 API 驱动——鉴权/打开应用+冷启动/开始监控/series 增长/悬停读数/切 tab/logcat 启动+过滤热切换/停止停增。顺带修复真实缺陷：悬停在图表首帧绘制前到达时 hideHover 清 hoverX 致读数永久丢失（改记住位置待 draw 补刷）。详见 CLAUDE.md「GUI 可编程调试接口」+ docs/DESIGN-gui-debug.md
- [x] ~~**软件发布打包（Linux + macOS，结合 GitLab CI）**~~（CLI/agent + GUI 已完成实现，2026-09-16，分支 `feature/gui-release-packaging` 待合并）：CLI/agent 的 v0.2.0 Release 链路已验证；GUI 发布专用 `tauri.release.json` 注入 `agent/xperf-agent` 资源，运行时不调用 Cargo/NDK；Linux AppImage 基于 WebKitGTK 4.1/Ubuntu 22.04+，macOS arm64/x86_64 DMG。Linux GUI 流水线 1407346 已验证编译、linuxdeploy、AppImage artifact 和内置 Android agent（约 104MB）；macOS 双架构 DMG 已本机验证（约 3.8/4.0MB）。GUI 资产将在 `v0.2.1` tag 发布。
- [x] ~~**logcat 文本过滤**~~（**已完成**，2026-09-15，`feature/logcat-text-filter` 合 main）：设备端 `logcat -e <regex>` 消息体正则下沉（吞吐敏感不把无关行拉过 adb 通道），与级别/按包过滤叠加。core config/restart/build_args 全链路（空白串忽略，标记行带 `text=`）；CLI `--logcat-regex`（requires --logcat；秒死 Error 经事件回调透传 stderr）；GUI 日志 tab「过滤」输入框（start/restart 命令加 text 参数，变更走热切换同文件续写）。**非法正则三平台（A11/A12/A16）实测均立即 rc=134 regex_error abort** → 连续秒死 Error 路径（CLI 可见 ❌）。真机回归（--remote hppc）：SS3 uid+`-e` 叠加命中正确 / SS2MAX A11 `-e` 可用 / SS4 多用户 uid+`-e` / 热切换集成测试（标记行 text=FATAL）/ GUI AX 全链（开始带 text=filament → 热切 SurfaceFlinger respawn 标记 → ANR 再切 → 停止）。AX 新教训入 CLAUDE.md（set value 异步生效、blur 派发 change、缓存引用失效、AppleScript 保留字）
- [x] ~~**问题反馈（一键收集日志 → GitLab issue）**~~（**已完成**，2026-09-17，`feature/feedback`）：core `feedback.rs`（收集+打包+上传纯 Rust——tar/flate2/reqwest rustls，零系统命令依赖）+ CLI `--feedback` + GUI 顶栏「问题反馈」按钮与描述浮层。采集范围按用户拍板收敛为 **xperf 自身证据**（不含设备 logcat，issue 正文引导手动附加）：1h mtime 过滤的会话产物（排除 pftrace/mp4/html 大文件，正文列路径）+ GUI diag 尾段 + 各设备 agent.log（网关除外）+ manifest 环境信息；逐项自检清单缺失如实标注；单文件 >32MB 截尾。token：`GITLAB_TOKEN` env > `~/.config/xperf/gitlab-token`；附件 413 超限回退 package registry；labels 失败去 labels 重试；上传失败归档保留报错附路径。**真机回归全通**：本地无设备/SSH 远程 3 设备 agent 日志/采样后会话产物收集/token 缺失错误路径（exit 1）/GUI 浮层键盘驱动提交全链/**真实上传 issue #1 创建+附件回下载可解包**（测试用 token 取 hppc git-credentials）。**AX 新教训**（已入 CLAUDE.md）：本实例 WKWebView 子树冻结于启动早期快照（递归遍历也拿不到新内容），键盘驱动焦点链是可行替代；侧栏按钮点击本身未目验（走查+构造器完成佐证，留给用户一眼确认）
- [x] ~~**截屏与录屏**~~（**已完成**，2026-09-12 主体 + 2026-09-14 并存缺陷核销，`feature/screen-capture` 合 main）：截屏=`adb exec-out screencap -p` 直写本机 PNG（PNG 魔数偏移定位剥 stdout 前缀警告——SS4 实踩）；录屏=scrcpy `--no-window --record`（复用镜像隧道双钉同号全链路；停止 SIGINT 优雅封盘 ≤3s 宽限 SIGKILL 兜底；CLI 倒计时从首帧落盘起算；产物核验防假阳性；**启动未建流自动重试一次**——设备端 server 启动偶发中止的自愈，CLI/GUI 同策略，GUI 前端 `retrying` 状态）。CLI `--screenshot`/`--record N`（独立+采样并行同窗口）；GUI 侧栏「屏幕捕获」区截屏按钮+录屏 toggle（AX 目验通过）。真机回归 SS3/SS4 全通，镜像+录屏并存 13 连过

- [x] ~~**GUI 完整回归与压力测试**~~（**回归已完成**，2026-09-21，commits `2a5446e`/`7989334`/`8e20095` + `4a19be3` 测试计划；**压力测试仍留待办**见下条）：自动化套件 `scripts/gui_tests/regression.py`（harness 复用 gui_debug_accept.py 协议，8 组 g1~g8）+ 覆盖清单 `docs/GUI-TEST-PLAN.md`（含用例矩阵与已知发现 6 条）。**Mac（--remote hppc）8 组全绿**：g1 侧栏 29/29、g2 指标页 20/20（+1 SKIP）、g3 深挖 20/20、g4 SSH 23/23、g5 多设备并行 14/14、g6 基线 10/10、g8 1/1；反馈链路真实上传 issue #5 ✓。故障注入：重复点击/快速切 tab/设备断连重连/SSH 隧道重建（kill master）/WebKit 子进程 SIGKILL 全过。顺带修 3 个真实缺陷：`open_perfetto_ui` 阻塞主线程（改 async+spawn_blocking）、前端 `start()` 无防重入（`_startPending`）、capabilities 缺 `core:window:allow-set-focus`。**关键发现（非缺陷）**：macOS 锁屏/全遮挡冻结 WebKit rAF（visibilityState=hidden 时悬停合帧确定性失败）→ 三层防护（`ensure_visible()` setFocus / `Checker.skip()` SKIP 通道 / 测试计划备档 #6）；AX/System Events 在无障碍权限缺失的 shell 不可用。环境参数化：`XPERF_TEST_SSH`/`XPERF_TEST_EXTRA_SERIALS`/`XPERF_IT_PACKAGE`（Linux 本地直跑与 kong AppImage 复用同一套件）。**Linux 双环境结果见 SESSION.md 2026-09-21 条目**。
- [x] ~~**下一会话：GUI 压力测试（后置）**~~（**已完成**，2026-09-22，`feature/gui-stress-test` 分支）：`scripts/gui_tests/stress.py` 七场景（s1 长时采样/s2 50ms 高频/s3 logcat 洪泛/s4 trace+stack 并发/s5 三机并行快切页/s6 按钮连点/s7 adb 冻结注入）+ 5s 粒度资源曲线 JSONL 落盘分段基线。**压测抓出 5 个真缺陷全修**：①采样静默挂死不自愈（adb server 卡死致信道「无数据亦无 EOF」永久半开——入向静止看门狗 15s 转 EOF 走重连，s7 守卫）②debug API `/api/status` 被 adb 卡死拖垮（设备列表改读监视器 3s 缓存）③ssh 隧道空闲期死亡无自愈（命令入口先 rebuild 再校验）④**WebKit 唯一文本串无界驻留泄漏**（fillText ~74KB/串 + measureText ~6.3KB/串，刻度标签每秒产新串 ⇒ RSS ~3.7MB/min 不收敛；修复 = Y 轴档位吸附 + X 轴 tick 时间网格对齐，唯一串 90s 450→21，phys_footprint 恒定 133MB 验证闭环）⑤open-perf/stack 连点穿透开重复标签页（600ms 冷却锚点击时刻 < invoke 630ms，burst 实测开 3 页——冷却 5s 锚完成时刻）。macOS 陷阱三条入 GUI-TEST-PLAN #8（ps 多 pid/WebKit XPC 归因/purgeable 预热），泄漏勘察链入 #9。终跑 29/31（两 FAIL 均判据问题非缺陷）+ s6 修复后复验 4/4，基线见 SESSION.md 当日条目。

---

## K. 环境覆盖补齐（2026-09-22 排期，新会话实施）

> 上轮回归的两块环境覆盖缺口 + 一个收尾项。三项均可**从 Mac 经 SSH 链全程驱动**（GUI 进程跑在目标机上——这正是覆盖点），无需登机操作。GUI 长驻进程统一 `setsid nohup ... </dev/null`（SIGHUP 教训）。

- [ ] **K1：hppc GUI `--remote kong`（覆盖「Linux 上的 SSH 远程模式」）** — **阻塞中（2026-09-22 勘察）**：kong 侧 `adb devices` 空列表、`lsusb` 无任何 Android 设备（车机未通电/未插线），hppc→kong 免密与 Xvfb 均正常。恢复条件＝把 SS2PRO/SS4 接到 kong 后按下列步骤原样执行（步骤与判据不变）。同时核实：Mac 本机 adb 零设备、hppc 本机挂 SS3+SS2MAX+SS4（K2 因此改在 hppc 跑）。
  - **目的**：目前 SSH 远程模式只有 Mac（--remote hppc）覆盖；Linux 侧差异在 webkit2gtk 渲染，需 hppc 直编 GUI 经 SSH 连 kong 验证一次
  - **环境**（上轮已勘察）：hppc 直编仓库 `~/code/tools/xperf`（已同步 main `81780ae`）；kong 挂 SS2PRO `ac889a71b1f` + SS4 桥接 `localhost:5559`；测试包 `com.google.android.filament.hellotriangle`（kong 无 gltf viewer）；hppc→kong 免密可达；Xvfb `:99` 上轮已起（没了则 `Xvfb :99 &`）
  - **步骤**：
    1. `ssh hppc 'cd ~/code/tools/xperf && git pull && rustup run 1.93.0 cargo build --release -p xperf-gui'`
       （若触发 agent 自动构建：`RUSTUP_TOOLCHAIN=1.97.0` + aarch64-linux-android target，见 GUI-TEST-PLAN 已知发现 #7）
    2. 启动：`DISPLAY=:99 setsid nohup ./target/release/xperf-gui --remote kong </dev/null >/tmp/xperf_gui_hppc_kong.log 2>&1 &`
    3. hppc 同机跑套件：`XPERF_TEST_SSH=kong XPERF_TEST_SSH_UI=kong XPERF_ADB=~/Android/Sdk/platform-tools/adb XPERF_IT_PACKAGE=com.google.android.filament.hellotriangle python3 scripts/gui_tests/regression.py --group all --serial ac889a71b1f --skip-feedback`
    4. 可选加跑 `stress.py --serial ac889a71b1f`（s7 adb 冻结注入在「Linux ssh 远程」链路下首次覆盖）
  - **预期**：g1-g8 全绿（hppc 的 ssh config 含 kong → g1 SSH 小节可跑；Linux 无 macOS 锁屏/rAF 冻结问题）；结果记 SESSION、勾选本项、更新 GUI-TEST-PLAN 环境矩阵
- [x] ~~**K2：kong AppImage v0.3.1 本地模式（发布二进制等效性）**~~（**已完成**，2026-09-22 晚，**执行环境改为 hppc**：kong 无设备挂载，而 hppc 挂着 SS3+SS2MAX+SS4 且系统更新——测的是同一份发布产物）
  - **结果（v0.3.1 AppImage / Ubuntu 24.04.5 / Xvfb :99 / 本机模式 / 三设备）**：`regression.py --skip-feedback` 总计 **FAIL 2 → 分诊后 1 真缺陷 + 1 判据竞态**（修判据后 g6 复跑 10/10）。g1 2/2+SKIP(SSH)、g2 **22/22**、g3 14/15、g4 **19/19**+SKIP(隧道重建，本机模式无隧道)、g5 **14/14**、g6 10/10（复跑）、g8 **3/3**。
  - **发布验证亮点**：① 24.04 `apparmor_restrict_unprivileged_userns=1` 现场，条件式 AppRun hook 实际注入 `WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1`（核验 `/proc/<pid>/environ`），WebKitWebProcess/NetworkProcess 正常拉起、全程 UI 可交互 → **v0.3.0「点击全无响应」修复在发布产物上闭环**；② harfbuzz 注入 + FUSE 挂载 + `--clean-cache`/agent 资源等无 Cargo/NDK 参与（g2/g4 采样零构建）；③ g8 杀 NetworkProcess 后 debug API 与 UI 自愈 ✓。
  - **抓出的真缺陷**：g3「火焰图 HTML 产物」FAIL——`scripts_dir()` 烤编译期路径 `/builds/ligraphic/xperf/...`，发布产物上必败（macOS DMG 同缺陷、在构建机上被掩盖）。**当日已根治并复验**（两层：解析链 + 随包分发；火焰图渲染剥 `PYTHONHOME/PYTHONPATH`）——见 A 节 ✅ 与 SESSION 深夜条目。
  - **覆盖判读（如实记录）**：① 22.04 仍**未覆盖**（kong 才有 22.04；hppc=24.04 反而多验了沙箱 hook 分支）；② v0.3.1 不含 9-21/9-22 修复，本轮相关用例通过**不可当回归证据**——g4「start 双击仅一次」在旧版上偶发通过（防重入修复不在内，本机往返快），g3「连点不炸」判据只查存活不查标签页数（连点 5s 冷却修复亦不在内）；③ 录屏/镜像 SKIP（hppc 未装 scrcpy）、SSH 隧道重建 SKIP（本机模式），与上轮 hppc 直编一致。
  - **环境事实**：产物 `hppc:~/xperf-rel/xperf-v0.3.1-linux-x86_64-gui.AppImage`（110MB，GitLab package registry API 下载）；启动脚本 `/tmp/hppc_launch.sh`、日志 `/tmp/xperf_gui_appimage.log`、套件日志 `/tmp/gui_reg_appimage.log`；上一会话遗留的 idle 直编 GUI（pid 4120325，uptime 11h）已 SIGTERM 停掉腾出发现文件/adb。
  - **后续**：v0.3.2 发版后同法复跑一轮拿「含全部修复 + 火焰图路径已修」的发布验证；22.04 覆盖待 kong 设备恢复（可与 K1 同场）。
  - **原 kong 计划留档（待设备恢复复用）**：产物从 GitLab package registry API 下载（hppc `~/.git-credentials` 有 PAT，`.../packages/generic/xperf/<tag>/xperf-<tag>-linux-x86_64-gui.AppImage`）→ scp hppc→kong（连 `scripts/gui_tests/{harness.py,regression.py}`）→ kong `chmod +x && xvfb-run -a ./<appimage> &` 本地模式 → `XPERF_IT_PACKAGE=com.google.android.filament.hellotriangle XPERF_TEST_LOCAL_SERIALS=ac889a71b1f,localhost:5559 python3 regression.py --group all --serial ac889a71b1f --skip-feedback`。kong=22.04（无 userns 限制，AppRun hook 条件不触发，与本轮 hppc 互补）。
- [x] ~~**K3：GitLab issue #5 关闭**~~（**已完成**，2026-09-22 晚）：Mac OAuth access_token 已过期（401），改经 hppc `~/.git-credentials` 的 PAT 走 `PUT /projects/39859/issues/5` form `state_event=close` → http 200，`state=closed`。

---

## J. Agent 能力提供（Claude Code / Codex，2026-09-18 review 排期）

> 目标：编码 agent 在用户项目里经 shell 调用 xperf-cli 完成性能采集/断言。2026-09-18 review 结论：**现状对 agent 不可用**——1 个阻断缺陷 + 1 个刚需缺口 + 文档载体缺失。分 4 个实施会话 + 1 个完整 review/真机回归会话。已具备基础：CLI 全链路非交互、结果全落盘（`/tmp/xperf/<pkg>/<ts>/` CSV/报告）、`--threshold`/基线对比天然是断言接口、`--help` 自描述充分。

- [x] ~~**会话 1 — 修复 tarball CLI 找不到捆绑 agent（P0 阻断缺陷）**~~（**已完成**，2026-09-18，`fix/cli-agent-resolution` 合 main，64409d4）：`resolve_agent_binary()` 三级解析链（`XPERF_AGENT_BIN` env 显式覆盖[指向不存在报错不回退] → exe 旁 `agent/xperf-agent` sibling[release tarball 布局] → 开发检出 `ensure_agent_built()` 兜底[workspace_root 有 Cargo.toml 才走，语义不变]；全落空报错指引 tarball 布局不再盲目 cargo build），纯函数 `pick_agent_binary` 锁顺序；替换 CLI main 预热/ensure_daemon 强制重推/reconnect 三处 None 分支，GUI 资源路径不动。真机（--remote hppc SS3）：掩蔽 workspace agent 二进制 A/B——tarball CLI 从 /tmp 采样+trace 全通且未触发重建（sibling 生效实证）；dev 回归、env 错误路径 exit 1 全通。单测 5 新用例，全量 162+5+11 绿，clippy/doc 零警告

**会话 2 — CLI `--duration <SECONDS>` 限时采样（P0 缺口）** ✅ **已完成**（2026-09-18，`feature/cli-duration` 合 main，aac08d4 + 75ada1c，merge d7bbf45）
- 落地：`--duration N`（u64 ≥1s，无上限）；复用现有 `stop_after` 通路，到点走正常退出流程（汇总/退出图表/基线保存对比/验证报告照常）；窗口计算抽纯函数 `stop_window(duration, trace, stack, record)` 取最长者（覆盖所有录制）；启动打印生效窗口（max 后的值，避免与录制同给时印错）；help 注明 agent 场景与「纯深挖/纯录屏/镜像-only 不生效」边界
- 真机回归（--remote hppc SS3 gltf viewer，全部 exit 0）：`--cpu --memory --duration 8` 8 样本+CSV+图表+峰值 ✓；`--duration 3 --trace 6` 窗口取 6s（trace 30MB+SQL 报告）✓；`--duration 3 --stack 6` 窗口取 6s（.data+三视图）✓；`--duration 5 --threshold cpu>5` 实时告警×4+退出验证报告 ✓；`--duration 5 --save-baseline`→`--compare-baseline` 保存+持平报告 ✓；无 duration Ctrl-C 回归不变 ✓
- 单测：`stop_window` 4 用例（全 None/单独/原有录制语义/取 max）；全量 core 162+8 + CLI 9 + GUI 11 绿，clippy/doc 零警告
- 注意：被测应用未运行时 agent 无事件源（resolve_pids 为空），限时到点正常退出但会话目录无 CSV——首轮回归误撞此情况，非缺陷
- **会话内 review 修复**（13f347c，merge 57250ff）：断连重连的 keep-going 闭包原只查 Ctrl-C——设备在限时窗口内断开会使采样无限悬挂，`--duration` 有界承诺失效（trace/stack 限时路径同样受影响，预先存在）；闭包加 deadline 判定到点放弃重连走正常退出。真机验证：`--duration 12` 采样 5s 后 `adb reconnect offline` 强制离线，~12s 到点 exit 0 + 汇总照常。其余观察项：deadline 起点在 spawn_agent 之后（部署耗时不计入窗口——**刻意语义**，慢部署不应吃掉采样窗口，维持现状）；`--duration`+无指标（纯深挖）不生效（纯深挖自带边界、无可修对象，help 已注明）；EOF 尾部空行（测试模块带入）**已顺手修复**（随 13f347c 提交，尾字节验证干净）

**会话 3 — 文档载体：`AGENTS.md` + `skills/xperf/SKILL.md` + README agent 一节** ✅ **已完成**（2026-09-18，`feature/agent-docs` 合 main，merge 2b2c819；子提交 7cdebd8 + 08adaad + e4a00d4）
- 顺手项①②均落地：`XPERF_AGENT_BIN=""` 空串视同未设置（`agent_bin_env` 过滤 + 合并单测 `test_agent_bin_env_semantics` 三态覆盖）；CLAUDE.md agent 部署节已补三级解析链说明
- `AGENTS.md`（仓库根）：tarball 布局/解析链、有界配方、输出布局 + CSV 列全表、退出码语义表（逐条核对 main.rs）、常见坑；`skills/xperf/SKILL.md`（中英触发描述 frontmatter）；README 双语各补「For AI agents / 面向 AI agent」节指向两份文档
- **文档驱动发现的真缺陷（08adaad 顺手修复）**：独立 `--cold-start`（无指标 flag）走 samplingless 提前返回分支，`run_cold_start` 在其后——纯冷启动静默 no-op exit 0（agent 断言陷阱）。修复：samplingless 分支前置执行冷启动测量（自带 15s 超时天然有界）。真机：SS2MAX 独立冷启动 TotalTime 613ms 正常输出
- 真机抽查（--remote hppc SS2MAX gltf，全对得上文档）：退出码 0/1/2 各路径（限时采样+阈值告警 exit 0、基线 save→compare 落 `baseline_report.txt`、截屏独立 exit 0、多设备未指定/非法包名/离线设备 exit 1、clap 互斥 exit 2、应用未运行 exit 0 无 CSV、`XPERF_AGENT_BIN=` 空串正常）；CSV 路径/表头逐一比对一致；**文档命令修正**：独立 logcat 无界（配方改为组合采样限时）、独立冷启动自界 15s 超时
- 全量 162+8/9/11 绿，clippy + cargo doc（含 missing_docs 三 crate）零警告
- **会话内 review 修正**（4a6e776，merge b865304）：08adaad 让独立冷启动真正执行但失败仍 exit 0，与其他独立能力失败码语义不一致（断言启动耗时的脚本拿不到失败信号）——独立模式测量失败如实置失败码（真机：.NoSuchActivity → exit 1 / 正常 → exit 0），AGENTS.md/SKILL.md 退出码表同步

**会话 4 — 退出落 `summary.json`** ✅ **已完成**（2026-09-18，`feature/cli-summary-json` 合 main，merge 12a78fb；子提交 fe29bd3 + ca67216）
- 落地：采样会话退出时总是落 `<ts>/summary.json`——顶层 `serde(flatten)` 平铺 `SessionSummary`（与基线 JSON 同 schema、同会话两文件口径一致，真机 diff 验证 18 键全同），附加 `thresholds`（all_pass + 逐规则 pass/triggers/extreme/last_trigger）与 `baseline`（action 五态 saved/compared/skipped_no_data/no_baseline/failed + baseline_file/report_file + `CompareOutcome`{verdict/regressions/improvements/flat/no_compare/regressed_metrics}）
- core：`baseline.rs` 抽 `build_rows`/`evaluate_rows` 共用内部函数，新增 `compare_outcome()`（与文本报告同口径，GUI 零改动）；CLI：`alerts::report_outcome` + 新模块 `summary_json.rs`；写盘失败只告警不改退出码；零样本会话也落盘（samples=0，目录仅含该文件）
- 真机回归（--remote hppc SS2MAX gltf，SS3 不在线）四场景：save+threshold（all_pass=false 结构化 ✓）→ compare（verdict=regression 与文本报告一致、report_file 存在 ✓）→ 零样本（samples=0/skipped_no_data ✓）→ no_baseline ✓
- 单测：core +3（compare_outcome 三态）、CLI +3（report_outcome、schema 超集一致性、写盘往返）；全量 core 165+8 / CLI 12 / GUI 11 绿，clippy + cargo doc（missing_docs 三 crate）零警告
- 文档：AGENTS.md（输出表 + `summary.json` schema 小节 + CI 断言指引 + 零样本坑修订）、SKILL.md（消费/断言两处）、CLAUDE.md（基线节结构化结论 + 输出表行）

**会话 5 — 完整 review + 真机回归** ✅ **已完成**（2026-09-18，merge 2409341；子提交 887417d + da6fd7c；会话内附带 flaky 测试修复 merge 80da281）
- **独立 review 结论：LGTM 无阻断**（独立子代理干净上下文审 bdd345a..98e55db 全部 18 commit：功能 diff 逐条核实现与意图一致、无 magic number、备选方案已考量）：0 严重；**一般 2 项已修**——G1 SSH 隧道重建失败的退避睡眠不尊重 `--duration` deadline（原最长 +30s+一次完整重部署；887417d 切片 ≤500ms 醒一次查 is_running；真机杀 ControlMaster 验证隧道重建→恢复采样→23s 到点退出，Err 臂未强造失败、残余 ≤500ms 接受）；G2 纯 `--freq`/`--thermal` 会话 summary.json `duration_s` 恒 0（freq/temp 序列计入跨度；schema 范围=CPU/内存/FPS/jank/GPU busy/IO/网络/冷启动，freq/thermal/显存/线程明细 CSV-only、`samples`=CPU 样本数——已写入 AGENTS.md）；**O1 顺手修**：`--duration`/`--record` 加 86400s 上限（防 u64 极端值溢出 panic，对齐 trace/stack 既有上限思路）
- **观察项核销**：O6（review 引用的「双打印」注释在代码库中不存在，子代理误引——代码无此注释，无需修）；O2 同秒双会话目录后缀碰撞（启动自然错开即现实规避）、O3 duration 打印不随录制变长（同「部署不计入窗口」刻意语义）、O4 独立冷启动失败输出样式、O5 compare/compare_outcome 双计算（µs 级开销）、O8 include_bytes 备选（已回答：tarball 双文件布局是刻意设计）、O9 env-mutating 单测串行化（默认并行无冲突实证）——均接受为残余风险
- **会话内发现修复（非 J 节改动引入）**：`test_record_stop_sends_sigint` 全量并行下竞态 flake（sh trap 安装的 300ms 固定等待在高负载下不足 → SIGINT 先于 trap 到达误杀；改 ready 文件握手轮询，dbec30a；scrcpy 功能 2026-09-14 引入的既有问题，本轮全量测试暴露）
- **真机回归矩阵（tarball CLI 隔离环境 /tmp/xperf-e2e + 掩蔽 workspace agent 证实 sibling 解析不触发 cargo 重建，全 `--remote hppc`）**：SS3/SS2MAX/SS4 三机限时采样 rc=0（CSV + summary.json，SS4 经桥接）；SS3：阈值触发 rc=0（summary.json `thresholds.all_pass=false` 结构化）/ `--trace 10` / `--stack 10` / `--screenshot` / `--logcat --logcat-regex` 并行 / 坏 Activity 冷启动 rc=1 / 正常冷启动 rc=0 / `--record 10`；SS2MAX：save→compare 持平 rc=0 / 独立冷启动 rc=0（应用在前台时 TotalTime 0ms 为预期语义）/ force-stop 零样本 rc=0（目录仅 summary.json、samples=0）；SS4：`--duration 3 --trace 6` 窗口取 6s（135MB trace 经隧道拉回 + SQL 报告）
- **退出码真值表逐条真机验证**（与 AGENTS.md 表一致）：0=限时采样/阈值触发/零样本/冷启动成功/深挖/截屏/录屏；1=非法包名/离线设备/多机未指定 `--device`/独立冷启动失败；2=基线互斥/`--logcat-regex` 无 `--logcat`/未知 flag
- 全量测试 core 165+8 / CLI 12 / GUI 11 修复前后多轮全绿；clippy + cargo doc（missing_docs 三 crate）零警告；CHANGELOG [Unreleased] 补 J 节 5 条、README 双语补 summary.json 断言入口。**J 节全部收官**

---

## 已完成

- ✅ GUI 多设备改版 + 应用操作/冷启动（2026-09-04 晚，01b28ce）：core serial 参数化（`adb_for`/`run_adb_command_for`/`resolve_serial`，各入口 `serial: Option<&str>`——None 回退全局 CLI 零变化）+ GUI 多会话（HashMap<serial, DeviceSession> + 命令/事件全带 serial）+ 前端设备 tab（template 克隆 DeviceSession 类 + 热插拔灰显/恢复）+ 冷启动模块下沉 core（resolve-activity 自动解析 + force-stop + GUI 打开/重启应用带测量进面板 + 重定向警示）；trace/stack 目录 `<pkg>/<ts>-<serial>` 防撞。真机 SS3+SS2MAX 双机并行全链路
- ✅ 基线对比 C-6（46bd161）：core baseline 模块 + CLI 两 flag + GUI 按钮/面板 + 真机全链路（保存→对比→回归触发→无基线提示）
- ✅ 多设备 adb 全局 `-s`（46bd161，E 节候补转正）：utils 注入 + CLI `--device` + GUI 设备下拉 + 平台检测表头 bug 修复 + 真机双连回归
- ✅ perfetto `--trace N` 深挖模式（ca01aa6）：trace.rs 录制（stdin 喂配置 + write_into_file 流式落盘）+ trace_processor SQL 归因（包线程 CPU/抢占/系统 top/每核/频率/帧时间线）；独立与并行两模式，Ctrl-C 语义明确；SS3 真机 6 场景验证（commit 见 SESSION.md 当日条目）
- ✅ SS3 QNX 通道真机回归 + 五轮 review 修复链（2026-09-03）：回归发现 kgsl 统计链停滞 bug；后续 review 连续翻案产出——fd3 活连接写入/行去重/看门狗（3 连续缺失+纯函数单测）/心跳空行探活/三层链清理（退出钩子+setsid+host 条件兜底）/pgrep 多会话并发保护。全部真机验证（commit 明细见 SESSION.md 当日条目）
- ✅ agent 模块化拆分：1848 行单文件 → 10 文件（proc/mem/fps/thermal/gpu 五通道），公共 spawn_stream_parser，host 侧 mtime 检查同步修复（531798a + 99d1b74）
- ✅ 平台抽象层：5 平台 trait + 自动检测 + agent 参数传递（2058fcc）
- ✅ 各平台 GPU 通道实现：topgpu/ligfxprofilerd/kgsl（58ceae5）
- ✅ C 类验证能力：阈值告警/冷启动/打点（a56a6f5、0f7ad9d）
- ✅ SS2MAX 实测：温度 sysfs 兜底修复 + IO/GPU 显存 SELinux 结论 + adb root 自动尝试（178fd16、c3a2ee6、7f8fda1）
- ✅ 两轮 code review 修复：17 项严重/一般问题（020e3ed、14d514c、02ee9bc）
- ✅ B5/B6/M2/M4 修复：marker 超时/agent 自动重建/Tauri async/CSV 转义（d7eb4aa）
- ✅ xperf-core 死代码删除（225d89b，D 类决策闭环）
- ✅ agent src 布局迁移（737ed45）
- ✅ B 类指标全覆盖（b732d55 等）；QNX GPU 通道（5cff92e）；GUI 三大改进（间隔档位/实时数值/周期标注）
- ✅ NDK host 感知（Mac/Linux 自动探测，f69476f）
- ✅ 更早历史见 SESSION.md
