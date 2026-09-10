# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 工作流约定

- **待办（backlog）**：`WORKSPACE.md` —— 跨会话的工作项与优先级，完成即勾选并注明 commit。
- **会话历史**：`SESSION.md` —— 每个会话结束前追加一条总结（最新在最上）：日期/任务/commit 列表/关键结论与基线/遗留问题。新会话开始先读它获取近期上下文。
- 一个会话聚焦一个任务线，多会话通过这两个文件同步。
- **代码规则（强制）**：写代码必须同时考虑 `cargo doc`——新增/修改的所有 pub 项（crate/mod/struct/enum/fn/字段/变体）都要有规范完整的 doc 注释（含单位/语义/无值字段要写明），路径/参数/日志样例包反引号或 code block；交付前必须跑完整 `cargo doc` 并做到**零 warning 零 error**（默认 lint 集 + missing_docs，命令见 Commands 节）。
- **git 拓扑**：本机（Mac）→ `hppc`（Linux 机中转远端）→ GitHub。本机 `git push` 推 hppc
  （main 的 upstream 已设 hppc/main）；**向 GitHub 的 push 统一在 hppc 上执行**
  （`ssh hppc 'cd /home/han/code/tools/xtools && git push origin main'`）；LFS 对象随
  push 经 SSH 直传 hppc（locksverify 已关）。拉取：GitHub 变更先在 hppc pull origin，
  本机再 pull hppc。

## Commands

```bash
# Build all host tools (dev；default-members 不含 xperf-agent，主机上不构建设备端 agent)
cargo build

# Build release binaries (host tools；agent 无 macOS/Linux 二进制)
cargo build --release

# Run tests（default-members，主机工具；agent 仅 Android 目标无主机测试）
cargo test

# Run tests for a single crate
cargo test -p xperformance
cargo test -p xrm

# Run a specific test
cargo test -p xrm tests::test_dangerous_operation_detection

# Check for errors without building（主机工具；--workspace 会连 agent 一起检查，
# 在 macOS/Linux 主机上会被 agent 的 Android-only compile_error 拦截）
cargo check

# 文档构建与覆盖检查（两条都要跑：cargo doc 有默认 lint 集——裸尖括号 HTML/
# 裸 URL/未解析链接等 missing_docs 单 lint 查不出来；xperf-core 已
# #![warn(missing_docs)] 常开）
cargo doc 2>&1 | grep -cE "^(warning|error)"   # 应为 0
cargo rustdoc -p xperf-core -- -W missing_docs
cargo rustdoc -p xperformance --bins -- -W missing_docs
cargo rustdoc -p xperf-gui --bins -- -W missing_docs
```

Release binaries are written to `target/release/`。

Workspace 成员：`xperf-core`（采样核心）、`xperformance`（CLI）、`xperf-gui`（Tauri GUI）、`xperf-agent`（设备端低间隔采样器，**仅 Android 二进制**——workspace `default-members` 排除它，主机目标显式构建被 `compile_error!` 拦截；交叉编译 `cargo build -p xperf-agent --target aarch64-linux-android --release`，链接器经 `.cargo/ndk-clang.sh` 按宿主 OS 探测（NDK **>= 25.1.8937393** 中取最相近，显式 ANDROID_NDK_HOME 等优先；API 26））、`xrm`（安全删除）。

---

## xperformance 设计结构

### 整体架构（统一 agent 采样）

CLI 和 GUI 不再有 adb 轮询路径，**所有采样都在设备端 agent（xperf-agent）进行**：

```
CLI:  main() → monitor_process() → monitor_process_agent()
GUI:  start_sampling(serial) / 自动启动 → spawn_sampling()（std::thread 阻塞读流）
        │
        └─ agent::spawn_agent(..., serial)
              ├─ ensure_daemon(serial)：adb forward（复用规则）→ probe hello 版本
              │    ├─ 版本一致 → 直连
              │    ├─ 版本不符 → 发 suicide + pkill 强杀 → 强制重推 → 重启 daemon → 探活
              │    └─ 无 daemon → pkill 清残留 → 强制重推 → setsid nohup 启动 → 探活
              ├─ TCP 连 127.0.0.1:<forward 端口> → 发 `start --package X --cpu ...`
              └─ AgentStream（reader 阻塞读 NDJSON + ping 线程 5s 保活）
```

- **设备端**：xperf-agent 以 **daemon** 常驻（`--daemon`，监听 `localabstract:xperf-agent` 抽象 socket，每连接一个独立采样会话，上限 10；0 会话 60s 自杀）。会话内直接读 /proc（CPU/线程）、smaps_rollup 或 dumpsys meminfo（内存）、本地 dumpsys SurfaceFlinger（FPS），按绝对节拍（start + round×interval，防漂移）逐轮输出 JSON 行。**多 host 并行**：每会话独立节拍线程 + TLS emitter（子模块 emit 零改动）；QNX/topgpu/ligfx 流式 GPU 通道全局独占（QNX 统计链是驱动全局资源，多开会洪泛——实测挤死 frame 链），被占用时后到会话收 err 禁用
- **主机侧**：只是表现层（CLI 打印/流式 CSV/图表；GUI emit 给前端）。ADB 断开 → TCP EOF → `reconnect_agent` 每 500ms 轮询等设备回来，probe 直连 daemon（daemon 扛住断连，host 侧状态时序/峰值/CSV 保留）；Ctrl-C/停止 → 关 TCP → daemon 侧会话收尾（GPU 通道持有者先停链 teardown 再放读线程收——顺序确定，真机验证过竞态）
- **xperf-core 已无轮询实现**（原 Sampler/cpu/memory/fps 参考实现已删除，225d89b）；core 只保留协议类型（ThreadCpuInfo/MemoryDetails/FpsTimeSeriesData/PidStats/SampleEvent）+ agent 传输层（daemon 管理/forward/TCP）+ platform/marker + trace（perfetto 深挖，CLI/GUI 共用）；采样全在 agent（零依赖独立发布，解析逻辑与 core 类型对应）
- **GUI 前端**：**多设备并行（顶栏每台在线设备一个 tab，热插拔动态增删；断开设备 tab 灰显保留数据、插回自动恢复采样）**。每设备页 = 独立侧栏（**公共模块，三页常驻**：「应用管理」=包名输入/刷新包列表/「打开应用」/「重启应用」/Activity 输入（留空自动 `resolve-activity`；打开/重启 force-stop→800ms→`am start -W` 顺带测冷启动，结果进「冷启动」面板，最近 5 次；启动被系统重定向（如车机熄屏进引导页）时状态栏警示）+「数据管理」=导出 CSV/保存基线/对比基线/清理缓存；2026-09-08 重排——采样控制归性能指标页、深挖录制归各自分析页）+ 主区三子 tab（性能指标：顶部 `perf-controls` 控制区=开始停止/采样间隔/窗口/指标勾选横排/实际周期；Perfetto 分析 / Simpleperf 分析：toolbar 内录制时长下拉 + 「录制并分析」就地录制（不切页，进度见状态栏与报告区）。JS 为 `DeviceSession` 类（模板 `<template id="devicePageTpl">` 克隆实例，全部状态/图表/面板设备内隔离；datalist id 须按 serial 唯一化）+ App 管理器（事件 payload 均带 `serial` 分发；顶栏 status 显示激活设备的状态，进度条语义不变）。CPU/内存/FPS 折线（series 保留完整会话历史，窗口跟随 10min / 全部历史切换，绘制时二分裁剪 + stride 抽稀防卡顿；**事件只标脏、150ms 合帧且仅绘制激活页**，uiColors 按主题缓存）、Top 线程表与实时数值（500ms 渲染 + dirty 检查，仅激活页）、峰值面板（新峰值才更新 DOM）、导出 CSV（`export_csv` = 复制后端流式落盘的会话目录快照；GUI 采样与 CLI 一样逐事件流式写 `/tmp/xperf/<pkg>/<ts>-<serial>/`——目录策略与前端会话语义对齐：手动「开始监控」建新目录（前端已重置数据），指标勾选重启同包复用目录 append 续写（图表不重置、CSV 不丢前文），换包建新目录；**前端内存 series 超 2×CHART_SERIES_CAP 每 2 取 1 抽稀**，口径同 CLI——图表/基线用抽稀序列，全分辨率数据在落盘 CSV）、perfetto 深挖（分析页时长下拉 + 按钮 → `start_trace` 命令 → `trace` 事件推进度 → 报告面板展示，与采样并行互不干扰；`--trace N` 命令行自动启动可脚本化验证）、`--package --device` 命令行自动启动与手动开始**同流程**（后端 `startup_sessions` 回查全部活跃会话回填 UI 并切到对应设备页）。**热路径批处理**：采样线程 `next_event_batch` 抽干 TCP 读缓冲（一轮节拍同 burst 的多事件合并一次 `sample` emit，payload `{serial, events[]}` 前端逐条分发）；逐事件 stderr 摘要 `XPERF_DEBUG=1` 才开

### CPU 采样口径（agent）

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

- interval ≥ 500ms：设备端 `dumpsys meminfo <pid>`（App Summary 全分类明细）+ smaps_rollup 补 RSS（smaps 不可读时用 App Summary 的 TOTAL RSS 兜底）
- interval < 500ms：优先只读 `/proc/<pid>/smaps_rollup`（Pss/Rss，~1ms；dumpsys ~100ms 太重）；**非 root 不可读时降级 dumpsys meminfo 限频 ≥500ms**（decide_mode 探测一次 + err 行告知）

**真机格式注意**：App Summary 分类行与 `TOTAL PSS:` 之间隔一个空行——空行结束区块，TOTAL 必须在区块外兜底解析。

`MemoryDetails` 字段（单位 KB，agent 协议与基线 JSON 存储口径）：`java_heap`, `native_heap`, `code`, `stack`, `graphics`, `private_other`, `system`, `total_pss`。

**展示单位统一（78f93a9）**：内存的终端打印/GUI 面板与图表/CSV 导出全 **MB**（协议与基线 JSON 存储仍 KB，展示层换算）——memory CSV 表头逐列标 `(MB)`；旧会话 CSV 为 KB 无标注，消费时看表头。其余指标核对无混用：GPU 显存全 MB、IO/网络全 KB/s、频率 MHz（hello 规格行 GHz）、温度 °C。

---

### FPS 采样流程（agent 设备端实现）

为什么不用 gfxinfo：`dumpsys gfxinfo framestats` 只统计 View 层级（HWUI）绘制的帧；游戏/相机/SurfaceView 直渲染应用的帧不上 gfxinfo。所有 buffer 最终都经 SurfaceFlinger 合成，因此对**图层**取帧时间戳是通用方案。

```
fps_sample_round(pid)                    ← agent 内每 PID 每 FPS 轮一次（限频 ≥500ms，见下）
  ├─ sf_discover_layers(pid, package)    ← 首个 FPS 轮 + 连续 10 个 FPS 轮零帧后重做（Surface 重建会换名 #0→#1）；
  │    │                                    发现为空也记零帧轮（进程刚重启 Surface 未建时按阈值节流重试，
  │    │                                    全量 dump ~1.5s 不能每轮试）
  │    ├─ dumpsys SurfaceFlinger（全量）  → 按 BufferStateLayer 块 metadata 的 ownerPID 归属匹配
  │    └─ dumpsys SurfaceFlinger --list → 按包名匹配（去掉 "<hex> " 别名前缀，去重）
  │    └── 两路结果取**并集**（9cfa9a5：ownerPID 非空即跳过兜底曾致 SS3 漏
  │         SurfaceView[...](BLAST)#0 层——Android 12+ BLAST 合成 app 直提 buffer，
  │         真实帧流在该层而 ownerPID 只拿到静止 Activity View 层 → FPS 恒 0）
  └─ 每图层每轮：dumpsys SurfaceFlinger --latency '<layer>'（设备端本地调用，无 adb 往返）
       解析最近 127 帧的 actualPresent（过滤 0=空槽、i64::MAX=已入队未上屏哨兵）
       与上轮缓冲末尾时间戳取差 → 本窗口新帧数 → FPS = 新帧数 / 窗口墙钟时长
       有帧的图层各发一行（多渲染面不取舍、不混叠）；全零时发一条静止样本
```

**FPS 限频（与 CPU/内存节拍解耦）**：`fps_every_n_rounds(interval)` = ⌈500/interval⌉，每 N 轮采一次，
有效周期 ≥500ms。50ms 间隔下每轮跑 dumpsys SurfaceFlinger 实测约半数轮次 overrun；限频后
同采 CPU+FPS 10 秒 0 overrun。图层发现的全量 dump（此车机 ~1.5s）在节拍时钟启动前的预热
阶段执行，避免首轮 backlog 追帧期 CPU 窗口不齐。

关键设计点：
- **图层名可能不含包名**（如 svm 的渲染层叫 `SVM Container`），只能靠 ownerPID 归属识别
- **不用 `--latency-clear`**：实测部分设备（如此车机）clear 只清空缓冲而不返回数据；改用 `--latency` 逐轮差值
- 缓冲 127 帧 ≈ 2.1s@60fps：采样间隔大于该值时老帧被挤出，计数为下界（interval ≤ 1s 精确）
- **jank 不按 vsync 阈值**（30fps 相机流在 60Hz 屏上帧间隔 33ms 会被误判全卡）：用间隔 > 2×窗口中位间隔，<3 帧不计；FPS 窗口限频 ≥500ms 后帧数足够，jank 统计有效
- 静止界面 FPS=0 如实上报（事件照常发，GUI 折线落底）

`--fps` 流式写入 `/tmp/xperf/<pkg>/<ts>/fps/<pkg>_fps_data_pid<pid>.csv`（Timestamp,FPS,Jank,Layer）。GUI 有 FPS 勾选框 + 折线图（自适应纵轴，多图层逐层一条线，图层短名作图例）。

---

### B 类指标（设备级上下文，agent 内实现）

五个开关（`--freq/--thermal/--gpu/--io/--net`），协议与采样成本：

| 开关 | 数据源 | 周期 | 协议事件 |
|------|--------|------|---------|
| `--freq` | 每核 `scaling_cur_freq`（KHz；hello 带 `maxkhz` 基线） | 每轮（µs 级） | `{"t":"freq","khz":[...]}` |
| `--io` | `/proc/<pid>/io` 计数器差值 → KB/s（r/w=rchar/wchar 逻辑读写，dr/dw=read_bytes/write_bytes 磁盘读写） | 每轮 | `{"t":"io","pid":..,"r":..,"w":..,"dr":..,"dw":..}` |
| `--net` | `/proc/net/dev` 物理口聚合（排除 lo/sit/tun/gre/dummy/vti/ip6*）→ KB/s | 每轮 | `{"t":"net","rx":..,"tx":..}` |
| `--gpu` | 三级探测：kgsl `gpubusy`（GVM 直通；`GpuBusyCalc` 自适应累计/窗口两种内核语义——SS2MAX 为窗口语义，读数自含占比）→ **QNX telnet**（hypervisor：QNX host 的 kgsl slog，真 busy%/util%/频率+每进程 busy）→ `dumpsys gpu` 每 PID 显存（保底，限频 ≥1s） | 每轮 / QNX 1s / 保底 ≥1s | `{"t":"gpu","busy":..,"util":..,"mhz":..,"maxmhz":..}` / `{"t":"gpuproc","pid":..,"busy":..}` / `{"t":"gpumem","pid":..,"bytes":..,"global":..}` |
| `--thermal` | `dumpsys thermalservice`（温度 sensors + Thermal Status 热降频级别） | 限频 ≥2s（~50ms dumpsys 会拖长低间隔节拍轮） | `{"t":"temp","status":..,"sensors":[[名,类型,°C]]}` |

关键设计点：
- **net 是整机口径**：Android 应用共享 netns，`/proc/<pid>/net/dev` 与整机一致；per-app 流量需 qtaguid（内核无）或 eBPF maps（不便读），实测被测包 uid=1000 系统聚合也无意义——如实标注整机。
- **QNX 通道细节**（SS3/8295，GPU 由 QNX host 管理，GVM 内无 kgsl 任何东西）：agent **内嵌极简 telnet client**（纯 TCP + RFC 854 最小协商全拒；2026-09-09 起替代 busybox telnet——shell 身份下 /vendor/bin/busybox 被 SELinux 拒执行，内嵌后**非 root 设备 QNX 通道可用**，网络层 shell 可达已实测）长连接，**`exec 3>/dev/kgsl-control` 持 fd 写入**开统计（gpu_set_log_level 4 + gpubusystats + gpu_per_process_busy 经 `>&3`），`slog2info -W | grep kgsl &` 流式读（**-W 不回放历史**，-w 会先倒几百行 backlog；grep 挡 VHAL 刷屏；**必须后台 &**，前台时 shell 阻塞、自愈命令滞留 tty 缓冲）。读线程独立不占节拍（读半=行源，写半 Arc 共享给看门狗/teardown）；进程行按 comm 名归因（QNX 显示名 = /proc/<pid>/comm）。
- **QNX kgsl 统计链**（2026-09-03 实测）：驱动全局（开机自带 5000ms 链），会话/fd 关闭都不清理。写入语义：**fd3 长活连接（exec 3>）写入 → 存量链全部重相位（计数归零锁步）持续输出；`echo>` 死写入者是 toggle——流链→停、停链→复活**。故启动命令必须 exec 3> 持 fd 写；多链锁步重复行由读线程按"与上一行全等"去重；frame 静默超宽限（3 连续缺失）由看门狗经 fd3 重写自愈。**链清理（daemon 化后）**：teardown 注册进 GPU_TEARDOWN 槽，持有会话结束（先停链、后放读线程杀 telnet——顺序确定）或 daemon 退出时执行一次；host `qnx_stop_stats` 条件兜底（纯观察探测≥2 帧才发 echo> 停链——对已停链写入会复活，不可无条件执行）。流式 GPU 通道全局独占（GPU_STREAM_BUSY），多会话不再各开链。**已知缺陷（2026-09-07 发现，未修）**：清理只写 `gpubusystats`，**`gpu_per_process_busy` 进程链无停止手段**（实测死写入者 500 toggle / 写 0 / `gpu_set_log_level 0` 均无效）——每 --gpu 会话泄漏一条进程链，多日累积成 ~20 条锁步洪泛（疑似挤占资源致 frame 链无法启动，两轮会话 0 frame 事件）；**恢复手段 = `adb reboot`（整 SoC 复位含 QNX，链全清回开机基线）**；另 daemon 被 SIGKILL/`timeout` SIGTERM 杀时 teardown 无机会执行，同样泄链（手动直跑 agent 验证时勿用 timeout）。
- 坑：QNX `login:`/`# ` 提示符**无换行**，必须逐字节读（内嵌 client 的 read_until 逐字节 + 读超时驱动 deadline）。
- **权限模型（2026-09-09 非 root 支持）**：hello 带 `root` 标志（协议 v3，agent /proc/self/status Uid==0）。**auto-root 仅车机平台**（SS2/SS3/SS4 内部开发设备；泛型 Android 不默认提权，`XPERF_NO_AUTO_ROOT=1` 整体旁路供回归测试）；GUI 侧栏「设备权限」徽章 + 「获取 root」按钮（core `acquire_root`：adb root + 轮询确认，结果反馈状态栏；adbd 重启杀 daemon，采样走重连恢复）。**非 root 实测矩阵见 WORKSPACE G 节**：CPU/FPS/频率/温度/网络/显存/perfetto/冷启动 shell 全可用；内存低间隔 smaps 不可读自动降级 dumpsys meminfo 限频 ≥500ms（RSS 用 TOTAL RSS 兜底）；IO 无数据源发 err 禁用（GUI 灰显勾选）；simpleperf 仅 debuggable 应用。
- **gpu/thermal 自适应降级**：探测失败发 err 并降级/禁用；此车机 thermalservice 是 test HAL 假数据（恒定 30.8°C），代码按标准接口实现，真手机有效。
- **host 侧开关收敛为 `MetricFlags`**（xperf-core/agent.rs）：`spawn_agent`/`reconnect_agent` 签名从逐 bool 改为该结构体，CLI/GUI 共用。
- **速率类指标（io/net/gpu）首样建基线不出数**，窗口按墙钟差值（非假定间隔），overrun 时速率仍准。（例外：kgsl 窗口语义下读数自含占比，首样即出数。）

CLI 退出图表用通用 helper `generate_multi_line_chart`（xperformance/utils.rs）：freq 每核一条、temp 每传感器一条、io 每 PID 读写两条、net RX/TX、gpu busy%。

---

### simpleperf 函数热点模式（`--stack N`，xperf-core/src/simpleperf.rs，CLI 与 GUI 共用）

「录制-分析」三级下钻的函数层：采样回答"什么时候高"，perfetto 回答"线程/调度/帧层面为什么高"，simpleperf 回答"**CPU 高在哪个函数**"。CLI 侧独立（`--stack N` 无指标 flag 时只录调用栈）或与采样并行（`--cpu --stack 10`：后台线程录制 + 采样限时同窗口）；`--trace` 与 `--stack` 可同给（并行录制同窗口对照，报告按 trace → stack 顺序输出，采样限时取两者 max）；独立模式录制失败**非零退出**（脚本化验证门槛；分析失败不算——数据已拉回可手动处理）。GUI 侧「simpleperf 分析」独立 tab（与「Perfetto 分析」并列，均隐藏侧栏）+ 共享录制时长下拉 + 两按钮（`.flex-fill` 等分防长文案挤出）+ `stack` 事件推进度（`{stage: recording|recorded|done|error, message, data_path}`，与 trace 事件同构；**recorded 阶段前端退出进度态显示"生成报告中"**——不退会冻结在 100%"录制中"）+ `--stack N` 命令行自动启动。

- **录制链路**：`adb shell simpleperf record --app <pkg> -g --duration N -o /data/local/tmp/xperf_stack_<ts>.data`（cpu-cycles 默认 4000Hz + dwarf 调用栈；`--app` 覆盖该应用全部进程并容忍进程重启，root 下非 debuggable 也可采）。**坑：`--app` 对未运行应用输出 `Waiting for process of app …` 无限等待，`--duration` 拦不住**（等待发生在采样开始前）→ 录制前 `pidof` 前置拦截 + 主机侧超时兜底（N+25s，Ctrl-C 中断标志可提前放弃，同 trace 模式）
- **三视图报告**（设备端 `simpleperf report`，须在 pull 前跑——`.data` 还在设备上；单个视图失败不中断，错误嵌入报告文本）：线程 CPU 分布（`--sort comm,pid,tid`）/ 函数热点 self（`--sort symbol,dso`，**"CPU 高在哪个函数"的直接回答**）/ 函数热点 children（`--children --sort symbol,dso`，调用链累计热点路径）；均 `--percent-limit 1` 去噪
- **解析**：report 为 header 定宽对齐文本（Symbol 列按最长符号名 padding，可达数百列宽），行解析按「首/尾 token 锚定」而非列位置切片（线程名/符号名可含空格，dso 恒无空格）；报告文件落盘前空格压缩（实测 4.9MB → 566KB）
- **产物**：`/tmp/xperf/<pkg>/<ts>/stack/{stack_<ts>.data, simpleperf_report.txt}`；`.data` 可 `adb push` 回设备换参数复跑 report（如 `--full-callgraph`）
- **浏览器火焰图**（`open_stack_in_browser`，GUI「函数热点」tab 按钮）：AOSP 官方 `report_html.py` 把 `.data` 渲染成单文件 HTML（火焰图/Chart/Sample Table，3.3MB→7.8MB ~1.2s）后 `open`/`xdg-open`。脚本集 **vendor 进仓库**（`xperf-core/simpleperf_scripts/`，git 管理：report_html.py/js + simpleperf_report_lib/utils + etm_types + `bin/{linux/x86_64/libsimpleperf_report.so, darwin/x86_64/libsimpleperf_report.dylib}` 双平台库——dylib 为 universal；文件齐全**零网络**，缺项才从 gitiles blob `?format=TEXT` 逐文件补齐（**+archive 不支持多级子路径（实测 INVALID_ARGUMENT）、整仓 tarball 80MB 太重**故逐文件）；**更新**=CLI `--update-simpleperf-scripts` 或 GUI 数据管理「更新火焰图脚本」按钮，双平台库一起重拉，覆盖后 git 提交同步）。**双平台 report 库走 LFS**（.gitattributes
`bin/**/*.dylib|so`，2026-09-08 起；LFS 未拉取时本地是 ~130B 指针文本，ensure/download
按 >1MB 判存在自动回退 AOSP 下载）。失败清半截 HTML（防 reuse 误判）；HTML 新于 `.data` 复用不重渲染；需 python3
- **录制进度**：core `record` 带 `progress: Option<&dyn Fn(u64)>` 回调（等待循环**循环头**每整秒触发 elapsed 1..=N——放 None 分支会漏报：adb 启动开销 ~0.5s 推迟首秒 + try_wait=Some 轮次跳过最后上报，曾致 10s 只显示 6s）；GUI 传闭包 emit `stage:'progress'`（message 如 `调用栈录制中 3/8s`），前端 status 栏以**绿色进度条**呈现（`#status.progress` 类，linear-gradient 按 elapsed/N 百分比铺开；完成/失败自动退回普通样式），CLI 传 None（打印会刷屏）。录制时长下拉与「录制并分析」按钮在**各分析页 toolbar 内**（Perfetto/Simpleperf 各自独立，`trace-seconds`/`stack-seconds` 5/10/15/30/60/120s；2026-09-08 起侧栏不再有深挖入口，采样控制在性能指标页控制区）
- **实测基线（SS3，simpleperf 1.build.47）**：svm 空闲态 8s ≈ 8500 样本 / 0 丢失 / 3.3MB（样本率随 CPU 活动浮动）；设备端应用 so 多为 stripped（函数名显示 `libxxx.so[+偏移]`，偏移可用未剥离 so 离线符号化），系统库与 `[kernel.kallsyms]` 有符号；非 root 设备上非 debuggable 应用被 run-as 路径拒绝（错误由 simpleperf 透传）

### 平台抽象（xperf-core/src/platform/）

Platform trait + `adb devices -l` product 字段自动检测（HU_SS3/HU_SS2MAXF/HU_SS2PRO/HU_SS4 → 对应平台，否则 Android）。host 检测后经 spawn_agent 传 `--platform`/`--qnx-host` 给 agent。

**GPU 通道按平台选路**（agent `detect_gpu_path_ex`）：kgsl sysfs（Android/SS2）→ QNX telnet（SS3：172.31.101.52，写 /dev/kgsl-control 开统计，slog2info -W 流读，独立线程）→ topgpu（SS2MAX，需 push 工具）→ ligfxprofilerd logcat（SS4）→ dumpsys gpu 显存保底。SS3/SS4 有每进程 GPU busy（gpuproc 事件，按 comm 名归因，`lookup_pid` 15 字符截断匹配）。

**SS2MAX 特性**：温度走 sysfs thermal zones 兜底（thermalservice sensors 列表为空但 HAL 有数据，条件须 `!sensors.is_empty()`）；IO 需 root（车机平台 auto-root 覆盖；非 root 时 /proc/<pid>/io 拒读发 err 禁用）；GPU 显存无数据源（dumpsys gpu 无 Memory snapshot 段，/sys/kernel/debug 未编译进内核，/proc/kgsl 不存在——2026-09-07 root 下确证）；**gpubusy 是窗口语义**（读数为上一 ~1s 窗口的 busy/total µs，total 恒 ≈1e6，非累计计数器；按累计差值解析曾出 1662% 荒谬值）——`GpuBusyCalc` 自动判别累计/窗口双语义（幅值/回退/>100% 三判据锁定），窗口语义直读 busy/total、与 `gpu_busy_percentage` 节点同刻值互证一致；gpubusy 节点 shell 可读（非 root 亦可采 GPU busy%）。

---

### C 类验证能力

- `--threshold cpu>80,mem>500,fps<30,gpu>90`：实时告警（静止界面 fps=0 不触发低值规则）+ 退出验证报告（触发次数/极值/总结论）
- `--cold-start .MainActivity`：am start -W 解析（15s 手动超时，status!=ok 报错）；测量结果（TotalTime ms）进基线汇总。**模块在 `xperf-core/src/coldstart.rs`**（CLI/GUI 共用，带 serial 参数）：`measure`（activity 留空自动 `resolve_activity` 解析主入口）、`force_stop`（重启前置）、`ColdStartResult`（Serialize，GUI 直接回前端）。GUI 侧栏「打开应用」/「重启应用」按钮同链路（force-stop→800ms→am start -W），结果进「冷启动」面板（最近 5 次）；**启动被系统重定向（Activity 不属于目标包，如车机熄屏进引导页）时状态栏警示**——测量值（0ms）无效，唤醒设备重试
- `--stack N`：simpleperf 函数热点（详见下节）
- `--save-baseline` / `--compare-baseline`：基线对比（详见下节）
- 打点：CLI Unix socket `/tmp/xperf-marker.sock`（`echo 标签 | nc -U ...`，每连接线程+10s 读超时）→ 图表竖线 + markers.csv（GUI 打点功能已删，78f93a9）

### 基线对比模式（`--save-baseline`/`--compare-baseline`，xperf-core/src/baseline.rs，CLI 与 GUI 共用）

「保存-对比」模式回答"改完有没有变差"：第一次运行 `--save-baseline` 把会话汇总存为该包基线，改动后再跑 `--compare-baseline` 出逐指标 diff 报告。CLI 与 GUI 数据互通（同一基线文件）。

- **汇总口径**（`SessionSummary`，多 PID 样本全量合并）：CPU%（单核口径）、PSS（KB）、FPS（全部样本均值，静止 0 帧计入——两次同场景对比下口径公平）、Jank（总数，对比换算为**每分钟次数**，须时长 > 0）、GPU busy%、IO 读/写 KB/s、网络 RX/TX KB/s、进程重启次数（绝对差判定，不用百分比）、冷启动 TotalTime ms（`--cold-start` 测量时才有）。会话时长取全部时序（含 B 类设备级指标——GPU-only 会话也有时长）首/末样本跨度；CLI/GUI 长会话内存序列均已抽稀时均值为均匀抽稀近似（CSV 全量在）
- **判定口径**：变化须**同时**超过相对 ±10% 与指标绝对地板值才判回归/改善，否则持平（地板值抑制近零噪声：CPU 2pp / PSS 4MB / FPS 2 / Jank 0.5 次每分 / GPU 3pp / IO·网络 50KB/s / 冷启动 150ms；基线为 0 时退化为纯绝对差）；仅一侧采集的指标如实标「⊘ 单侧未采集」不硬判
- **存放**：`~/.local/share/xperf/baselines/<pkg>.json`（XDG 数据目录，用户数据语义，`--clean-cache` **不**清理）；包名拼路径前校验（与包名校验同字符集）；保存即覆盖
- **CLI**：两 flag 互斥；退出阶段在验证报告之后执行；对比报告落盘 `<ts>/baseline_report.txt`（与 CSV 同目录）；无采样 flag 时跳过并提示
- **GUI**：侧栏「数据管理」区「保存基线/对比基线」两按钮（数据源 `collectSessionData()`，与导出 CSV 同一份前端序列）；报告展示在指标页峰值区的「基线对比」面板，新会话自动隐藏清空；后端命令 `save_baseline`/`compare_baseline`（build_summary_from_series 与 CLI 口径一致——restarts 为 None（GUI 路径未统计，如实单侧标注））
- **真机基线（SS3 svm，2026-09-04）**：空闲态 20s×2 次，CPU 均值 30.3/29.8（持平）、PSS 462MB（持平）、FPS 29.7/30.0、Jank 5.98 次/分（持平）、GPU busy 15.3/15.4（持平）——svm 稳态噪声远小于判定闸门；篡改基线制造回归场景正确触发（⚠ 4 项 + 指标名列表）

### 多设备 adb（core 会话级 `-s`；CLI 全局/`--device`，GUI 每设备一 tab 并行）

多台设备同连时 adb 不带 `-s` 全部报 `more than one device`——工具链整体失效（SS3 车机 + 手机双连实测触发，2026-09-04）。

- **core 注入点（两级）**：全局 `utils::TARGET_SERIAL`（`set_target_serial`/`target_serial`，CLI main 解析 `--device` 后写入）+ **会话级 `adb_for(serial)`/`run_adb_command_for(serial, …)`/`resolve_serial`**——`spawn_agent`/`deploy_agent`/`reconnect_agent`/`qnx_stop_stats`/`trace::record`/`simpleperf::record`/`detect_platform_live`/`coldstart::*` 均带 `serial: Option<&str>` 参数：`None` 回退全局（CLI 调用点零行为变化）、`Some(s)` 显式路由（空串视同 None）。CLI 自身无直接 adb 调用（全走 core 入口的 None 回退）。每台设备的 adb 长连接互不干扰，**双机并行采样/深挖实测可用**
- **选择策略**（`pick_device`）：`--device` 显式（须在线）> 单台自动 > 多台报错（错误信息列设备清单，含 Android 版本）。CLI 在 main() 解析（冷启动/采样之前）；GUI `--package` 自动启动前做前置解析（多台未指定**确定性跳过**自动启动；`--device` 无效同样跳过，不静默换台）
- **Android 版本检测**（9cfa9a5）：`AdbDevice.android_version`（`ro.build.version.release`，`list_adb_devices` 逐台 getprop，失败标 `?`）；CLI「目标设备」打印/多台报错清单、GUI 设备 tab 均展示。采集方式与版本相关——实测印证：SS3=Android 12（帧走 `SurfaceView[...](BLAST)` 层）、SS2MAX=Android 11（帧走 `SurfaceView - ` 层，无 BLAST）
- **GUI 多设备并行**：`AppState.sessions: HashMap<serial, DeviceSession>`（每台设备独立 running/trace_running/stack_running/package/startup_extra；`session()` 取或建 + `record_startup()` 写启动记录）；全部命令带 serial（`start_sampling`/`stop_sampling`（幂等，不建空会话）/`start_trace`/`start_stack`/`list_packages`/`launch_app`/`restart_app`——前置校验设备在线 `ensure_device_online` + 包名防遍历）；事件 payload 均带 serial（`sample={serial, event}`、`trace`/`stack`/`sampling-error` 同）→ 前端按 serial 分发到对应 `DeviceSession`；`startup_sessions` 回查全部活跃会话（自动启动回填 + 切到对应设备页）；关窗遍历全部会话置停止。GUI 不再使用全局 serial（`select_device`/`is_running` 命令已删，`list_devices` 无 selected 字段）
- **trace/stack 落盘目录隔离**：GUI 深挖目录 `<pkg>/<ts>-<serial>/`（serial 后缀防双设备同秒录制撞目录；CLI 结构 `<pkg>/<ts>/` 不变）
- **动态检测（热插拔）**：`spawn_device_monitor` 线程每 3s 轮询 `adb devices -l`，与上次快照 diff（`diff_devices` 纯函数），变化时 emit `devices-changed {devices, added, removed}` → 前端：新设备建 tab，断开设备 tab 灰显「（已断开）」——**数据与采样线程保留，设备插回后 `reconnect_agent` 自动恢复采样**（采样中状态栏提示等待重连）；首轮只建快照不通知。**软件手段（kill-server/reconnect/wait-for-disconnect）制造不出 diff**——server 重启后枚举快于 3s 轮询窗且 serial 不变，验证须物理插拔
- **默认窗口大小（7cb4b4b）**：`resize_default` 命令按屏幕逻辑尺寸动态计算（宽 72% clamp[1080,1600]、高 88% clamp[880,1280]，各留屏幕边距防超出；min 1080×720 + 侧栏 overflow 兜底），前端加载完成后调用；conf 固定尺寸 1400×1000 为检测失败兜底。侧栏指标勾选两列 grid + PIDs 列表限高 140px，默认窗口高度下侧栏免滚动全量显示。（曾把 webview 空白归因于 setup 阶段 `set_size` 时序——**9cfa9a5 修正为误判**，真凶见下条）
- **webview 间歇空白（9cfa9a5 + b30d149 修）**：webkit2gtk 两条失效路径，均进程内 `set_var` 于 main 最开头（须在任何 webview 初始化前，单线程安全）——①启动时空白：DMABUF 渲染路径在浏览器等 GPU 重负载应用占用时间歇失败 → `WEBKIT_DISABLE_DMABUF_RENDERER=1`；②Alt+Tab 切走（常为切到 GPU 重负载应用）再切回内容有概率空白：加速合成路径失效 → `WEBKIT_DISABLE_COMPOSITING_MODE=1`（无 CSS 动画/变换，禁用无性能影响）。**判别方法**：空白时带对应变量手动启动对比即可确认
- **平台检测**：`detect_platform_live` 按 serial 过滤设备行再 detect——**坑：过滤须补回表头**（`detect_platform` 按 `skip(1)` 跳表头，曾因表头被滤掉、SS3 行被当表头跳过而误判 Android，真机复现+单测锁定）
- **重连**：`device_online` 只认目标设备（`adb -s X get-state` = "device"）——多台同连时其他设备在线不算"回来了"
- **QNX 收尾**：`qnx_stop_stats` 的 pgrep 多会话保护/probe/停链均带 `-s`（语义不变，作用域收敛到目标设备）

### SS4 adb 自动桥接（`xperf-core/src/bridge.rs`；设计 `docs/DESIGN-ss4-adb.md`）

SS4（SA8797P）是 **MindRT（Linux PVM，USB 可见）+ Android（GVM，USB 不可见）** 双系统：`adb devices` 只见 MindRT（**无 product 字段的 USB 设备**），Android 须经 `forward tcp:<port> tcp:5557`（MindRT 上的 adb 中继，常驻）+ `adb connect localhost:<port>` 桥接成伪设备。bridge 模块让桥接全自动：桥接后 Android 以 `localhost:<port>`（恒 localhost 字面，**serial 稳定性不变量**，勿用 127.0.0.1）进入现有全链路，下游零感知。

- **四个集成点**（全链路仅此）：①`list_adb_devices` 尾部 `bridge::refresh`（幂等收敛：forward --list 恢复网关映射/已知网关 connect 自愈每轮都试/候选设备 bootstrap 失败 60s 冷却；新建连接有界重枚举 ≤2 轮）②`device_online` 的 `localhost:*` 分支 → `bridge::reconnect`（GVM 重启自愈双保险之一，另一是监视器轮询 refresh）③`try_adb_root`/`acquire_root` Ss4 兜底（直连 adb root 失败 → 经网关 `rootandroid.sh` + 重连 + 15s 轮询，①②报错各自透传）④`pick_device` 过滤 `AdbDevice.is_gateway`
- **网关识别**：forward 规则 target=`tcp:5557` → 确定网关（谁建的都复用，含用户手工/厂商 tool）；无 product 直连设备 → 候选探测（connect 失败即清规则冷却，对普通设备零影响）；**MindRT adbd 重启（root 提权）清 forward 规则**——refresh/reconnect 按规则存在性重建
- **root**：S0 实测标准 adb 直连 root 成功（无 TCP 限制）为主路径，`rootandroid.sh`（MindRT `/system_ext/bin/`）兜底；MindRT 自身 root 免（best-effort）
- **GUI**：`visible_devices` 统一过滤三处 payload（devices_json 内聚/list_devices/connect_remote/监视器 diff 前）——前端永远看不到 MindRT tab；`ensure_device_online` 拒绝网关 serial 并指引 `localhost:<port>`；前端零改动（serial 冒号在 dataset/id 安全）
- **SSH 远程零改动**：forward/connect/devices 全是 adb server 侧语义，经 hop#1 天然到达 hppc server；`-s localhost:5559` 路由与 hop#2 映射按 serial 过滤正常
- **S0+S6 真机验证**：免 root 桥接、GVM 重启 serial 不消失（offline ~24s 自动回 device，零干预自愈）、采样中 GVM reboot 自动重连恢复、root 双路径（①直连 ②网关兜底均在重连竞态中真实触发）、SS3+SS2MAX+SS4 三机并行采样不互扰
- **ligfx 注意（S0/R8 推翻）**：ligfxprofilerd 在 **MindRT 侧**（GVM logcat 无输出）——agent `gpu/ligfx.rs`（读 GVM logcat）不成立，SS4 GPU 通道须改 host 侧经网关读 MindRT logcat（~5s/帧块，`GVM_<comm>` 按 comm 归因，Frequency 恒 1000 单位存疑），属 WORKSPACE H 节 GPU 项

### SSH 远程后端（`--remote`，xperf-core/src/transport.rs，CLI 与 GUI 共用）

**为什么**：真机接在远端 Linux 机（hppc）时，本机（Mac）跑 GUI/CLI 经 SSH 完成采样/perfetto/simpleperf 全部功能。完整设计与逐条实测依据：`docs/DESIGN-ssh-remote.md`。

**架构**：adb server 前移 + SSH 隧道——本机恒为 adb **客户端**（`ADB_SERVER_SOCKET` 指向 hop#1 本机端口），`pull`/`push` 落点、trace_processor/report_html.py、`/tmp/xperf` 落盘全留本机零改动；唯一例外是 `adb forward` 的监听端口在**远端 server** 侧（v1 曾误判为客户端侧，真实拓扑复验推翻）⇒ agent NDJSON 流需第二跳隧道：

```
hop#1: 本机 P_srv → 远端 127.0.0.1:5037（全部 adb 命令，恒 1 条）
hop#2: 本机 P_loc → 远端 adb forward 分配端口（agent 事件流，每设备 1 条，
       ssh -S <ctl> -O forward/cancel 动态增删，不新起 ssh 进程）
单条 ssh ControlMaster（-M -S ~/.ssh/cm/xperf-<host>-<pid>-<n>）承载两跳；
必带 Compression=yes（实测 47MB 真实 trace 11.17s → 2.68s，4.2×，与 zstd 最优仅差 0.4s）
```

**改造面**（极小）：
- `transport.rs`：`Transport::{Local, Ssh(SshTarget)}` 进程级全局 + `SshTunnel`（establish 四步：远端 `start-server`（**绝不 kill-server**——远端 server 可能被共用，R2）→ 清无主 control socket（R9：pid+序号文件名 + `kill -0` 判活）→ 自选本机端口建 master（`ExitOnForwardFailure` 快速失败换端口重试 3 次，R4）→ adb `host:version` 原始协议握手探活；Drop = `-O exit` 带走全部转发）
- `utils.rs::adb_command()` 唯一 adb 构造点：Ssh 模式注入 `ADB_SERVER_SOCKET` **环境变量**（不用 `-H/-P`：不参与 argv 顺序，不与调用方追加的 `-s`/子命令冲突）；`run_adb`/`run_adb_command_for`/`adb_for`/`list_adb_devices` 全经此，Local 模式零注入（逐字节一致）
- `agent.rs::ensure_daemon` 端口分流：`Local` 直连 forward 端口；`Ssh` 经 `tunnel.add_forward(remote_port)` 换本机端口——连接/探活/协议代码零改动；映射表按 remote_port 复用（重连/重试不重复建）
- 生命周期：`init_remote`（R2 协议版本校验=两侧 `adb version` banner 协议串比对，**先于一切 adb 客户端调用**——版本不符客户端会 kill 远端 server；不符报错中止）/ `shutdown_remote`（逐条 `forward --remove` 本工具注册的规则=hop#2 映射表键集，**禁用 `--remove-all`** 会踢他人规则，R10）/ `rebuild_tunnel`（S9：`reconnect_agent` 先判 `tunnel.is_alive()` 再判 `device_online`，隧道死则指数退避 1s→30s 重建；远端 forward 规则 server 持有跨隧道存活，`ensure_forward` 查 list 复用）
- CLI `--remote/--remote-adb/--remote-adb-port`（init 接在 `select_device` **之前**；正常/错误退出路径显式 `shutdown_remote`——`process::exit` 不跑析构）；GUI 顶栏「连接」下拉（数据源 = `~/.config/xperf/remotes.json` 已存配置 ∪ `~/.ssh/config` 的 Host 别名，免手工录入）+＋配置浮层 + 5 命令（list_remotes/list_ssh_hosts/save_remote/connect_remote/remote_status）+ `remote-status` 事件 + 热插拔监视器隧道死短路（边沿触发，防每 3s 刷错）
- 远端 adb 路径解析（`resolve_remote_adb_path`）：先试配置值，失败自动退标准 SDK 位置 `~/Android/Sdk/platform-tools/adb`（远端 PATH 常不含 adb，附录 A #12）——ssh_config 来源的主机默认 `adb` 也能直连
- 多设备并行天然支持：隧道位于 adb client↔server 之间，比「设备」低一层——`-s` 路由/GUI 每设备 tab/深挖并发零改动（每设备 hop#2 一条）；队头阻塞实测不成立（并发 3×47MB 拉取下 200ms 节拍 p50 不退化）

**要点与实测基线**：
- forward 规则由 **server** 持有、跨会话存活（本机进程死亡也不消失）⇒ `ensure_forward` 查 `--list` 复用同名规则是防泄漏关键（`tcp:0` 每次调用新建，实测连调 3 次得 3 条）
- 远端 adb 常不在 PATH（hppc 在 `~/Android/Sdk/platform-tools/adb`）⇒ `--remote-adb` 可配
- 真机回归（Mac←SSH→hppc→SS2MAX，gltf viewer）：`--cpu --interval 500` 采样（hello 8 核/CPU ~42%/线程明细/CSV 节拍）✓、`--trace 10`（44MB 落本机 + SQL 报告）✓、`--stack 10`（5.1MB .data + 三视图）✓、杀隧道断连恢复（重建→重连→采样继续）✓、双进程并发（采样+trace 节拍无退化）✓、优雅退出后 hppc forward 规则与 control socket 零残留 ✓
- 已知边界：多真机远程并行未经双设备验证（hppc 仅挂一台，机制上与本地多设备同层）；SIGKILL 残留的远端 forward 规则由下次会话复用消化

---

### perfetto 深挖模式（`--trace N`，xperf-core/src/trace.rs，CLI 与 GUI 共用）

「录制-分析」模式，与实时采样互补：采样回答"什么时候高"，trace 回答"为什么高"。CLI 侧可与采样指标并行（`--cpu --trace 10`：后台线程录制 + 采样限时同窗口，到点自动结束）或单独使用（无指标 flag 时只录 trace）；GUI 侧深挖按钮与采样会话并行（采样不限时，窗口对照靠时间戳）。core 模块不打印不建目录：输出目录由调用方传入，报告以文本返回（CLI println / GUI 走 Tauri `trace` 事件 `{stage: recording|progress|recorded|done|error, message}`——progress 为每秒录制进度（elapsed/Ns，core `record` 的 `progress` 回调），done 的 message 即完整报告）。

- **录制链路**：`adb shell perfetto -c - --txt -o /data/misc/perfetto-traces/xperf_<ts>.pftrace`，配置经 stdin 喂入（text proto，**对齐团队 Performance_Tools general_debug.pbtxt 口径**：ftrace 全事件 sched/power/gpu_mem/ext4/f2fs/kmem/mmc + atrace 17 类目 + `atrace_apps "*"` + android.log/packages_list/gpu.memory/process_stats(scan_all_on_start) + frametimeline；buffer0 128MB ftrace + buffer1 32MB stats/log，`write_into_file: true` + 2s 刷盘长录制内存有界）；10s ≈ 46MB（~4.6MB/s，600s ≈ 2.8GB）。不存在的 ftrace 事件（memory_bus/\*、SS3 的 cpu_frequency）perfetto 静默忽略。录完 adb pull 到 `/tmp/xperf/<pkg>/<ts>/trace/` 并清理设备端文件。SS3 实测：atrace slice 17 万/10s（surfaceflinger 等系统进程有 app 层轨道）、log 3425 条、cpuidle counter 生效、cpufreq 仍无（GVM）。
- **trace_processor 定位链**：`~/.local/share/perfetto/prebuilts/trace_processor_shell*`（get.perfetto.dev 官方脚本缓存）→ PATH → `/tmp/trace_processor`（自举脚本）→ 均无则从 get.perfetto.dev 下载引导。分析失败不致命（trace 文件已保存，提示 ui.perfetto.dev 手动分析）。
- **SQL 分析**：全部查询写一个文件落盘（复现用），**执行按语句逐条进行**（新版 trace_processor 禁止一次执行多条返回行的 SELECT——报 "Result rows were returned for multiple queries"；`sql_statements()` 按 `;\n` 切分，marker 语句结果集天然存在，输出拼接后与旧单次执行格式一致，`parse_sections` 无感；`-q file trace` 旧语法新旧版本通吃）。marker 查询 `select '===段名===' as m;` 分段，`===END===` 哨兵段最后；**某条失败即停（与旧单次执行语义一致），故按"表必然存在 → 可能缺失"排序，帧时间线表殿后**。输出为"表头+数据行+空行"的 CSV 结果集序列，[NULL] 值需按空处理。
- **报告段**（trace_analysis.txt + trace_queries.sql 同目录留存）：trace 窗口/boot 基线、包 CPU 总量（单核口径 % 窗口）、包线程 CPU 时间 top15、抢占/调度延迟（thread_state R/R+：唤醒→上核的 runnable 时间）、系统 CPU top 与每核 busy/切换次数（**均须排除 idle：swapper 切片 utid=0 挂 upid=0 无名进程，不排除则空闲机器每核"busy"恒 ~100%、(内核线程) 桶被 idle 淹没 80%+**；排除后 (内核线程) 桶 = 真 kthreads）、CPU 频率（**SS3 GVM 无 cpufreq ftrace 事件，如实标注**，实时值用 --freq）、帧时间线全局统计+最差 5 帧（与 agent 图层 FPS 互补；按图层/进程归属深入分析用浏览器 Perfetto UI）。
- **Ctrl-C 语义**：SIGINT 发给整个前台进程组 → adb 被一并杀死（退出码为信号）→ 与非零 code 区分，报"录制被中断 + 设备残留路径"；采样产物不受影响。独立模式注册 handler 优雅退出，录制等待循环查 `xperf_core::utils::is_interrupted()` 可提前放弃。
- **浏览器一键分析（`open_trace_in_local_ui`，core；GUI 的 open_perfetto_ui 命令调用）——本地镜像 Perfetto UI + 同源自动加载**：首次使用联网镜像 ui.perfetto.dev（headless Chrome 跑一遍 UI + netlog 提取资源清单 + curl 逐个下载，缓存 `~/.cache/xperf/perfetto_ui/`，之后离线可用）→ 单例本地服务器（127.0.0.1 随机端口，**每连接一线程**，serve 镜像静态文件 + 动态注册的 trace；`service_worker.js` 固定 404）→ 深链 `http://127.0.0.1:PORT/#!/viewer?url=http://127.0.0.1:PORT/<trace>`（**url 参数必须绝对 URL**：SPA 路由对其执行 `new URL()` 不传 base，相对路径抛 "Invalid URL" 中断路由停在主页）。同源 fetch 过 CSP `'self'`、http 页面无 mixed content、loopback→loopback 不跨地址空间无 LNA——**全自动加载零交互**，服务器随进程存活（浏览器内刷新不受影响）。失败（离线/无 Chrome）回退 `reveal_trace_and_open_ui` 拖拽方案（ui.perfetto.dev + dbus FileManager1 高亮 trace 文件，File API 无网络请求）。
- **为什么不用「ui.perfetto.dev + 本地 HTTP + ?url= 深链」（2026-09-03 实测 Chrome 152 双重拦截）**：① ui.perfetto.dev 自带 CSP `connect-src` 白名单（'self'/localhost:8080/127.0.0.1:9001 等固定口），随机端口全被 block（fetch 抛 TypeError，零网络请求）；② 白名单内端口再被 Chrome LNA（Local Network Access）权限拦——公网 https 页面 fetch loopback 需用户授权，深链自动 fetch 无手势被静默拒（带 Access-Control-Allow-Private-Network: true 也过不了）。
- **验证方法论教训（本轮踩坑，后续必守）**：① headless Chrome `--dump-dom` 验证 SPA 必须用正确判据——数 `<canvas` 标签/检查页面特征文本，**不能 `grep -c canvas`**（Chrome 错误页内嵌 JS 也含该字符串，"Sched" 会匹配 "scheduled"，本轮曾据误判得出"方案已验证"）；② `--virtual-time-budget` 不等 wasm 真实解析（16MB trace 解析需真实时间），dump 总在解析完成前——**渲染验证看完整 console 日志**（成功标志：`Opening trace using built-in WASM engine` → `Loading trace N MB` → 路由切 `#!/viewer?local_cache_key=...` → WebGL 活动日志）；③ 对照实验的 server 进程会被自己早期的清理脚本误杀（pkill 模式匹配过宽），"对照成功"可能是 ERR_CONNECTION_REFUSED 错误页——对照前先确认对照体存活。
- **GUI 侧**：主区双 tab（性能指标 / Perfetto 分析），报告不与指标混排；侧栏（包名+数据管理）三页常驻不再隐藏（2026-09-08 重排）；录制就地发起（分析页 toolbar），进度见状态栏与报告区，done/error 自动切到分析页；分析页顶部「在浏览器打开 Perfetto UI」按钮（recorded 起 enabled，`trace` 事件 payload 附 trace_path）→ 本地镜像 UI 自动加载，失败自动回退拖拽。**暗/亮双主题**（tabBar 右侧按钮切换 + localStorage 持久化）：CSS 变量全量化（mocha/latte 双色板）+ `color-scheme`（select/number 等原生控件暗色渲染）；**checkbox 为 appearance:none 自绘**（webkit2gtk 对原生 checkbox 暗色渲染支持不全，实测仍白底——自绘不依赖引擎原生渲染）；canvas 图表与实时面板取色动态跟随主题（uiColors() 读 CSS 变量 + 双 series 色板）；status 栏在 tabBar 行右端。
- **实测基线（SS3）**：裸配置时期 10s ≈ 11.8MB / sched 14 万事件；对齐团队 general_debug 配置后 10s ≈ 46MB（atrace slice 17 万 + log 3425 条 + cpuidle counter）；svm 空闲时包内仍可见线程级毫秒级 CPU/抢占明细；不存在的包名 → "无调度事件"如实上报；1s 极限窗口正常。


---

### agent（设备端采样器，xperf-agent）

**为什么**：adb 轮询单轮固定 6+ 次调用（每次 ~13ms 起，`dumpsys meminfo` ~100ms），低间隔下开销超过间隔本身，且每次 adb 调用都扰动被测系统。agent 常驻设备直接读 /proc（微秒级），NDJSON 经 socket 流式回传（PerfDog Agent 同构思路，但免装 APK：纯静态二进制）。当前 CLI/GUI 的**唯一**采样路径。

**部署与 daemon 生命周期（2026-09-07 daemon 化）**：
- 本机二进制：`target/aarch64-linux-android/release/xperf-agent`（不存在时自动执行 `cargo build -p xperf-agent --target aarch64-linux-android --release`；链接器经 `.cargo/ndk-clang.sh` 探测——macOS/Linux 双平台，NDK **>= 25.1.8937393** 中取最相近，显式 ANDROID_NDK_HOME/ANDROID_NDK_ROOT/NDK_HOME 优先且不过滤，无满足版本时报错列出已发现版本）
- 设备端路径：`/data/local/tmp/xperf-agent`；daemon 启动日志 `/data/local/tmp/xperf-agent.log`
- **生命周期**：`ensure_daemon`（spawn_agent 内）全权管理——adb forward（`--list` 复用既有规则，否则 `tcp:0` 新建）→ probe hello 版本 → 一致直连 / 不符 `suicide`+`pkill`+强制重推重启 / 无 daemon 则 pkill 清残留（含老版 stdout agent）+ 强制重推 + `setsid nohup ... --daemon` 启动。**强制重推绕过 size/mtime 快检**（同秒重建的同尺寸二进制会被快检误判跳推，实测造成版本协商死循环）；daemon 0 会话 60s 自杀，设备重启后下次会话自动重建
- 手动重建推送：`cargo build -p xperf-agent --target aarch64-linux-android --release && adb push target/aarch64-linux-android/release/xperf-agent /data/local/tmp/`（下次会话自动 suicide 旧 daemon 重推重启）

**协议（daemon socket 上的文本行）**：连接即收 `hello`（含 version/ncores/maxkhz；**满员拒连也先发 hello 再发「会话数已满」err**——probe 探活只认 hello，首行非 hello 会触发 host 重推重启流程）；host 发 `start <与 argv 相同参数>`（复用 parse_args，零新依赖；同连接可 `stop` 后重新 `start`——stop 会 join 等会话线程收完再放行）/`stop`/`ping`（每 5s，30s 无数据 agent 断连）/`suicide`；agent 回事件流（与 stdout 模式同一 NDJSON 协议）。**手动调试**仍可用 stdout 模式（无 `--daemon`，须 stdin 保持打开）。

**要点**：
- 绝对节拍：`start + round × interval`，漂移时发 err 行（"round N overrun"）
- CPU 窗口 = 相邻两轮差值（常驻保有状态，无 phase1/phase2 结构）
- 权限：root 最优（他进程 smaps_rollup/io 需 root）；shell 身份自动降级（矩阵见 WORKSPACE G 节）——CPU/FPS/频率/温度/网络/显存/perfetto 全可用，内存低间隔降级 dumpsys 限频，IO 发 err 禁用；hello 带 `root` 标志（协议 v3）
- 终端输出：interval ≥ 500ms 逐条详细打印；< 500ms 按 ~1s 聚合（avg/max），全量明细在流式 CSV；CSV 时间戳毫秒精度（`%.3f`）
- 会话收尾：host 断开（TCP EOF）→ 会话即停；持有 GPU 流式通道的会话**先跑 teardown（QNX 停链写 telnet）再放读线程断 TCP**（顺序确定——先断 TCP 则停链写入落空链残留，真机实测）；daemon 进程退出（suicide/空载/信号）经同一 teardown 槽
- 宿主非正常死亡（kill -9）：TCP 随进程消亡 → daemon 会话即收——不再产生泄漏（旧 exec-out 时代的孤儿泄漏由 daemon 化根治）

**代码结构**（模块拆分）：
- `main.rs`：协议头注释、Args/parse_args、`--qnx-stop` 一次性停链模式、daemon（监听/连接处理/空载自杀）与 stdout 双模式、会话节拍循环（run_session）、TLS emitter 与公共工具（emit/json_escape/now_ms/dumpsys，crate 根私有项对所有子模块可见）
- `proc.rs`：/proc 与 sysfs 读取（stat jiffies/resolve_pids/cpufreq/io/net）+ `PidState::sample_cpu`（CPU% + 线程明细）
- `mem.rs`：smaps_rollup（低间隔）+ dumpsys meminfo App Summary（≥500ms），`MemMode::decide_mode` 探测降级（非 root 低间隔 → dumpsys 限频 ≥500ms），`sample_memory` 直接 emit
- `fps.rs`：SurfaceFlinger 图层发现 + 帧时间戳差值 + jank，`FpsState::sample_round`
- `thermal.rs`：thermalservice 解析 + sysfs thermal zones 兜底，`sample` 返回是否有数据
- `gpu/`：`mod.rs`（GpuPath 枚举 + detect_gpu_path_ex + `spawn_stream_parser` 公共读线程骨架（LineReader 行源抽象 + cleanup 闭包）+ emit_gpumem）+ `kgsl.rs`/`qnx.rs`（内嵌 telnet client `QnxTelnet`）/`topgpu.rs`/`ligfx.rs` 四通道；三流式通道样本归一为 `GpuEvent::Sys/Proc` 后交公共读线程 emit（wire 格式不变：按通道字段有无按需输出 util/maxmhz）

**要点**：
- 绝对节拍：`start + round × interval`，漂移时发 err 行（"round N overrun"）
- CPU 窗口 = 相邻两轮差值（常驻保有状态，无 phase1/phase2 结构）
- 权限：root 最优（他进程 smaps_rollup/io 需 root）；shell 身份自动降级（见上条要点）
- 终端输出：interval ≥ 500ms 逐条详细打印；< 500ms 按 ~1s 聚合（avg/max），全量明细在流式 CSV；CSV 时间戳毫秒精度（`%.3f`）
- 主机断连（EOF）→ 自动重连恢复（见上）；Ctrl-C → adb 连接关闭 → agent 写 stdout 失败自行退出（节拍循环整轮零输出时发空行探活，一个周期内感知断连；host 侧 next_event 跳过空行，零协议影响）；宿主进程死亡 → 孤儿清理 + stdin EOF/停滞看门狗兜底（见「传输与 liveness」）

**验证基线**：svm @ 50ms 间隔，78 样本均值 15.03%，与 adb top 一致；50ms 窗口可见 25-47% 的瞬时毛刺（1s 采样看不到）。

---

### 输出文件触发时机

**数据根目录为 `/tmp/xperf`**（CLI 与 GUI 共用同一根，替代旧的 `./log`；`/tmp` 重启自清）。**清理**：CLI `xperformance --clean-cache`（无需 --package）或 GUI 侧栏「清理缓存与数据」按钮（`tauri-plugin-dialog` 原生 confirm——webkit2gtk 的 JS `confirm()` 窗口标题是 "Javascript-taurixxx"，695946a）——清 `~/.cache/xperf`（perfetto UI 镜像）、`/tmp/xperf`（全部采集数据）与 `xperf-core/simpleperf_scripts/`（火焰图脚本下载缓存，下次使用重新下载或 `--update-simpleperf-scripts` 恢复）；`~/.local/share/perfetto`（trace_processor 官方缓存）不动。采样/录制进行中清理会丢当前会话产物（GUI 有 confirm 确认）。

| 场景 | 触发条件 | 输出位置 |
|------|---------|---------|
| CPU/内存/FPS/B类指标 CSV | **采样时流式追加**（每个样本到达即写并 flush，崩溃只丢尾部；CLI 与 GUI 同——`CsvStream` 在 xperf-core 共用；GUI 无 --thread 故无线程 CSV） | CLI: `/tmp/xperf/<pkg>/<ts>/{cpu,memory,fps,thread,freq,thermal,gpu,io,net}/`；GUI: `<pkg>/<ts>-<serial>/` 同子目录结构 |
| CPU 图表（每 PID + 汇总） | 程序退出，数据点 > 1 | `/tmp/xperf/<pkg>/<ts>/cpu/` |
| 内存图表（每 PID + 汇总） | 同上 | `/tmp/xperf/<pkg>/<ts>/memory/` |
| 线程时序图 | `--thread --cpu`，退出时有数据 | `/tmp/xperf/<pkg>/<ts>/thread/` |
| B 类图表（freq 每核/temp 每传感器/io 每 PID/net/gpu） | 退出时对应序列 > 1 点 | `/tmp/xperf/<pkg>/<ts>/{freq,thermal,io,net,gpu}/` |
| perfetto 深挖（--trace N） | 录制完成即拉回；分析随即落盘 | `/tmp/xperf/<pkg>/<ts>/trace/{*.pftrace, trace_analysis.txt, trace_queries.sql}` |
| simpleperf 函数热点（--stack N） | 录制完成即在设备端生成三视图并拉回 | `/tmp/xperf/<pkg>/<ts>/stack/{*.data, simpleperf_report.txt}` |
| simpleperf 浏览器火焰图（GUI 按钮） | 首次点击时渲染生成（复用不重渲染） | `/tmp/xperf/<pkg>/<ts>/stack/*.html`（同目录同名） |

- 内存中的时序序列只服务退出图表：超过 2×30k 点时每 2 取 1 原地抽稀（`CHART_SERIES_CAP`，保完整时间范围、分辨率随运行时长自适应降级）；CSV 始终全量。
- `CpuTimeSeriesData.top_threads` 已无读者，CLI agent 路径不再写入（线程明细走 thread_time_series + 流式 CSV）。

**注意**：`create_timestamp_subdir()` 使用 `OnceLock<Mutex>` 缓存目录路径，整个会话只创建一个时间戳目录（首个样本落盘时创建）。


---

### 全局状态（utils.rs）

```rust
static INTERRUPT_FLAG: AtomicBool          // Ctrl-C 中断标志
static TIMESTAMP_DIR: OnceLock<Mutex<Option<PathBuf>>>  // 本次会话的输出根目录，首个样本流式落盘时创建并缓存
```

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
