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

# 文档构建与覆盖检查（两条都要跑：cargo doc 有默认 lint 集——裸尖括号 HTML/
# 裸 URL/未解析链接等 missing_docs 单 lint 查不出来；xperf-core 已
# #![warn(missing_docs)] 常开）
cargo doc 2>&1 | grep -cE "^(warning|error)"   # 应为 0
cargo rustdoc -p xperf-core -- -W missing_docs
cargo rustdoc -p xperformance --bins -- -W missing_docs
cargo rustdoc -p xperf-gui --bins -- -W missing_docs
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
- **主机侧**：只是表现层（CLI 打印/流式 CSV/图表；GUI emit 给前端）。ADB 断开 → exec-out EOF → `reconnect_agent` 每 500ms 轮询等设备回来，重新部署+启动 agent，主机侧状态（时序/峰值/CSV）保留；Ctrl-C → 关闭连接 → agent 写 stdout 失败自行退出
- **xperf-core 已无轮询实现**（原 Sampler/cpu/memory/fps 参考实现已删除，225d89b）；core 只保留协议类型（ThreadCpuInfo/MemoryDetails/FpsTimeSeriesData/PidStats/SampleEvent）+ agent 传输层 + platform/marker + trace（perfetto 深挖，CLI/GUI 共用）；采样全在 agent（零依赖独立发布，解析逻辑与 core 类型对应）
- **GUI 前端**：CPU/内存/FPS 折线（series 保留完整会话历史，窗口跟随 10min / 全部历史切换，绘制时二分裁剪 + stride 抽稀防卡顿）、Top 线程表（500ms 节流渲染）、峰值面板（新峰值才更新 DOM）、导出 CSV（`export_csv` 命令写 `log/<pkg>/<导出时刻>/`）、perfetto 深挖（侧栏秒数+按钮 → `start_trace` 命令 → `trace` 事件推进度 → 报告面板展示，与采样并行互不干扰；`--trace N` 命令行自动启动可脚本化验证）

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

- interval ≥ 500ms：设备端 `dumpsys meminfo <pid>`（App Summary 全分类明细）+ smaps_rollup 补 RSS
- interval < 500ms：只读 `/proc/<pid>/smaps_rollup`（Pss/Rss，~1ms；dumpsys ~100ms 太重）

**真机格式注意**：App Summary 分类行与 `TOTAL PSS:` 之间隔一个空行——空行结束区块，TOTAL 必须在区块外兜底解析。

`MemoryDetails` 字段（单位 KB）：`java_heap`, `native_heap`, `code`, `stack`, `graphics`, `private_other`, `system`, `total_pss`。

---

### FPS 采样流程（agent 设备端实现）

为什么不用 gfxinfo：`dumpsys gfxinfo framestats` 只统计 View 层级（HWUI）绘制的帧；游戏/相机/SurfaceView 直渲染应用的帧不上 gfxinfo。所有 buffer 最终都经 SurfaceFlinger 合成，因此对**图层**取帧时间戳是通用方案。

```
fps_sample_round(pid)                    ← agent 内每 PID 每 FPS 轮一次（限频 ≥500ms，见下）
  ├─ sf_discover_layers(pid, package)    ← 首个 FPS 轮 + 连续 10 个 FPS 轮零帧后重做（Surface 重建会换名 #0→#1）；
  │    │                                    发现为空也记零帧轮（进程刚重启 Surface 未建时按阈值节流重试，
  │    │                                    全量 dump ~1.5s 不能每轮试）
  │    ├─ dumpsys SurfaceFlinger（全量）  → 按 BufferStateLayer 块 metadata 的 ownerPID 归属匹配
  │    └─ 兜底：dumpsys SurfaceFlinger --list → 按包名匹配（去掉 "<hex> " 别名前缀，去重）
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

`--fps` 流式写入 `log/<pkg>/<ts>/fps/<pkg>_fps_data_pid<pid>.csv`（Timestamp,FPS,Jank,Layer）。GUI 有 FPS 勾选框 + 折线图（自适应纵轴，多图层逐层一条线，图层短名作图例）。

---

### B 类指标（设备级上下文，agent 内实现）

五个开关（`--freq/--thermal/--gpu/--io/--net`），协议与采样成本：

| 开关 | 数据源 | 周期 | 协议事件 |
|------|--------|------|---------|
| `--freq` | 每核 `scaling_cur_freq`（KHz；hello 带 `maxkhz` 基线） | 每轮（µs 级） | `{"t":"freq","khz":[...]}` |
| `--io` | `/proc/<pid>/io` 计数器差值 → KB/s（r/w=rchar/wchar 逻辑读写，dr/dw=read_bytes/write_bytes 磁盘读写） | 每轮 | `{"t":"io","pid":..,"r":..,"w":..,"dr":..,"dw":..}` |
| `--net` | `/proc/net/dev` 物理口聚合（排除 lo/sit/tun/gre/dummy/vti/ip6*）→ KB/s | 每轮 | `{"t":"net","rx":..,"tx":..}` |
| `--gpu` | 三级探测：kgsl `gpubusy`（GVM 直通）→ **QNX telnet**（hypervisor：QNX host 的 kgsl slog，真 busy%/util%/频率+每进程 busy）→ `dumpsys gpu` 每 PID 显存（保底，限频 ≥1s） | 每轮 / QNX 1s / 保底 ≥1s | `{"t":"gpu","busy":..,"util":..,"mhz":..,"maxmhz":..}` / `{"t":"gpuproc","pid":..,"busy":..}` / `{"t":"gpumem","pid":..,"bytes":..,"global":..}` |
| `--thermal` | `dumpsys thermalservice`（温度 sensors + Thermal Status 热降频级别） | 限频 ≥2s（~50ms dumpsys 会拖长低间隔节拍轮） | `{"t":"temp","status":..,"sensors":[[名,类型,°C]]}` |

关键设计点：
- **net 是整机口径**：Android 应用共享 netns，`/proc/<pid>/net/dev` 与整机一致；per-app 流量需 qtaguid（内核无）或 eBPF maps（不便读），实测被测包 uid=1000 系统聚合也无意义——如实标注整机。
- **QNX 通道细节**（SS3/8295，GPU 由 QNX host 管理，GVM 内无 kgsl 任何东西）：agent 起 `busybox telnet 172.31.101.52`（QNX 侧 root 免密）长连接，**`exec 3>/dev/kgsl-control` 持 fd 写入**开统计（gpu_set_log_level 4 + gpubusystats + gpu_per_process_busy 经 `>&3`），`slog2info -W | grep kgsl &` 流式读（**-W 不回放历史**，-w 会先倒几百行 backlog；grep 挡 VHAL 刷屏；**必须后台 &**，前台时 shell 阻塞、自愈命令滞留 tty 缓冲）。读线程独立不占节拍；进程行按 comm 名归因（QNX 显示名 = /proc/<pid>/comm）。
- **QNX kgsl 统计链**（2026-09-03 实测）：驱动全局（开机自带 5000ms 链），会话/fd 关闭都不清理。写入语义：**fd3 长活连接（exec 3>）写入 → 存量链全部重相位（计数归零锁步）持续输出；`echo>` 死写入者是 toggle——流链→停、停链→复活**。故启动命令必须 exec 3> 持 fd 写；多链锁步重复行由读线程按"与上一行全等"去重；frame 静默超宽限（3 连续缺失）由看门狗经 fd3 重写自愈。**链清理三层**：agent 退出钩子（stdout EPIPE 路径，emit 失败先跑钩子再 exit）；spawn_agent 加 `setsid`（脱离 adbd 会话，让断连走 EPIPE 而非信号直杀）；host `qnx_stop_stats` 条件兜底（纯观察探测≥2 帧才发 echo> 停链——对已停链写入会复活，不可无条件执行），CLI/GUI 会话结束均调用。三层均带**多会话并发保护**（pgrep 检测其他 agent 存在则跳过停链——会杀掉对方采样中的流；host 侧须先轮询等自身收尸，adbd 收尸有 1-2s 延迟，立即探测会把残留自身误判为他人）。已知并发交互（不修）：后启动会话给先启动方一次 ~7s 停走；各方事件密度 ~2×（非锁步多链）。
- 坑：QNX `login:`/`# ` 提示符**无换行**，必须逐字节读；子进程 stdin 句柄 drop 即 EOF，telnet 会退出（须移交读线程持有）。
- **gpu/thermal 自适应降级**：探测失败发 err 并降级/禁用；此车机 thermalservice 是 test HAL 假数据（恒定 30.8°C），代码按标准接口实现，真手机有效。
- **host 侧开关收敛为 `MetricFlags`**（xperf-core/agent.rs）：`spawn_agent`/`reconnect_agent` 签名从逐 bool 改为该结构体，CLI/GUI 共用。
- **速率类指标（io/net/gpu）首样建基线不出数**，窗口按墙钟差值（非假定间隔），overrun 时速率仍准。

CLI 退出图表用通用 helper `generate_multi_line_chart`（xperformance/utils.rs）：freq 每核一条、temp 每传感器一条、io 每 PID 读写两条、net RX/TX、gpu busy%。

---

### 平台抽象（xperf-core/src/platform/）

Platform trait + `adb devices -l` product 字段自动检测（HU_SS3/HU_SS2MAXF/HU_SS2PRO/HU_SS4 → 对应平台，否则 Android）。host 检测后经 spawn_agent 传 `--platform`/`--qnx-host` 给 agent。

**GPU 通道按平台选路**（agent `detect_gpu_path_ex`）：kgsl sysfs（Android/SS2）→ QNX telnet（SS3：172.31.101.52，写 /dev/kgsl-control 开统计，slog2info -W 流读，独立线程）→ topgpu（SS2MAX，需 push 工具）→ ligfxprofilerd logcat（SS4）→ dumpsys gpu 显存保底。SS3/SS4 有每进程 GPU busy（gpuproc 事件，按 comm 名归因，`lookup_pid` 15 字符截断匹配）。

**SS2MAX 特性**：温度走 sysfs thermal zones 兜底（thermalservice sensors 列表为空但 HAL 有数据，条件须 `!sensors.is_empty()`）；IO 需 adb root（agent 自动 try_adb_root + id 验证）；GPU 显存无数据源；gpubusy 计数器恒 `0 0`（停走，kgsl busy 通道因此无事件；gpuclk 可读 427MHz）——实测确认非代码问题。

---

### C 类验证能力

- `--threshold cpu>80,mem>500,fps<30,gpu>90`：实时告警（静止界面 fps=0 不触发低值规则）+ 退出验证报告（触发次数/极值/总结论）
- `--cold-start .MainActivity`：am start -W 解析（30s 超时，status!=ok 报错）
- 打点：CLI Unix socket `/tmp/xperf-marker.sock`（`echo 标签 | nc -U ...`，每连接线程+10s 读超时）或 GUI 按钮 → 图表竖线 + markers.csv

### perfetto 深挖模式（`--trace N`，xperf-core/src/trace.rs，CLI 与 GUI 共用）

「录制-分析」模式，与实时采样互补：采样回答"什么时候高"，trace 回答"为什么高"。CLI 侧可与采样指标并行（`--cpu --trace 10`：后台线程录制 + 采样限时同窗口，到点自动结束）或单独使用（无指标 flag 时只录 trace）；GUI 侧深挖按钮与采样会话并行（采样不限时，窗口对照靠时间戳）。core 模块不打印不建目录：输出目录由调用方传入，报告以文本返回（CLI println / GUI 走 Tauri `trace` 事件 `{stage: recording|recorded|done|error, message}`，done 的 message 即完整报告）。

- **录制链路**：`adb shell perfetto -c - --txt -o /data/misc/perfetto-traces/xperf_<ts>.pftrace`，配置经 stdin 喂入（text proto，**对齐团队 Performance_Tools general_debug.pbtxt 口径**：ftrace 全事件 sched/power/gpu_mem/ext4/f2fs/kmem/mmc + atrace 17 类目 + `atrace_apps "*"` + android.log/packages_list/gpu.memory/process_stats(scan_all_on_start) + frametimeline；buffer0 128MB ftrace + buffer1 32MB stats/log，`write_into_file: true` + 2s 刷盘长录制内存有界）；10s ≈ 46MB（~4.6MB/s，600s ≈ 2.8GB）。不存在的 ftrace 事件（memory_bus/\*、SS3 的 cpu_frequency）perfetto 静默忽略。录完 adb pull 到 `log/<pkg>/<ts>/trace/` 并清理设备端文件。SS3 实测：atrace slice 17 万/10s（surfaceflinger 等系统进程有 app 层轨道）、log 3425 条、cpuidle counter 生效、cpufreq 仍无（GVM）。
- **trace_processor 定位链**：`~/.local/share/perfetto/prebuilts/trace_processor_shell*`（get.perfetto.dev 官方脚本缓存）→ PATH → `/tmp/trace_processor`（自举脚本）→ 均无则从 get.perfetto.dev 下载引导。分析失败不致命（trace 文件已保存，提示 ui.perfetto.dev 手动分析）。
- **SQL 分析**：全部查询写一个文件单次执行（trace 只加载一次），marker 查询 `select '===段名===' as m;` 分段，结尾 `===END===` 哨兵判断执行完整性。**查询出错会中止整个文件后续语句**，故按"表必然存在 → 可能缺失"排序，帧时间线表殿后。输出为"表头+数据行+空行"的 CSV 结果集序列，[NULL] 值需按空处理。
- **报告段**（trace_analysis.txt + trace_queries.sql 同目录留存）：trace 窗口/boot 基线、包 CPU 总量（单核口径 % 窗口）、包线程 CPU 时间 top15、抢占/调度延迟（thread_state R/R+：唤醒→上核的 runnable 时间）、系统 CPU top 与每核 busy/切换次数（**均须排除 idle：swapper 切片 utid=0 挂 upid=0 无名进程，不排除则空闲机器每核"busy"恒 ~100%、(内核线程) 桶被 idle 淹没 80%+**；排除后 (内核线程) 桶 = 真 kthreads）、CPU 频率（**SS3 GVM 无 cpufreq ftrace 事件，如实标注**，实时值用 --freq）、帧时间线全局统计+最差 5 帧（与 agent 图层 FPS 互补；按图层/进程归属深入分析用浏览器 Perfetto UI）。
- **Ctrl-C 语义**：SIGINT 发给整个前台进程组 → adb 被一并杀死（退出码为信号）→ 与非零 code 区分，报"录制被中断 + 设备残留路径"；采样产物不受影响。独立模式注册 handler 优雅退出，录制等待循环查 `xperf_core::utils::is_interrupted()` 可提前放弃。
- **浏览器一键分析（`open_trace_in_local_ui`，core；GUI 的 open_perfetto_ui 命令调用）——本地镜像 Perfetto UI + 同源自动加载**：首次使用联网镜像 ui.perfetto.dev（headless Chrome 跑一遍 UI + netlog 提取资源清单 + curl 逐个下载，缓存 `~/.cache/xperf/perfetto_ui/`，之后离线可用）→ 单例本地服务器（127.0.0.1 随机端口，**每连接一线程**，serve 镜像静态文件 + 动态注册的 trace；`service_worker.js` 固定 404）→ 深链 `http://127.0.0.1:PORT/#!/viewer?url=http://127.0.0.1:PORT/<trace>`（**url 参数必须绝对 URL**：SPA 路由对其执行 `new URL()` 不传 base，相对路径抛 "Invalid URL" 中断路由停在主页）。同源 fetch 过 CSP `'self'`、http 页面无 mixed content、loopback→loopback 不跨地址空间无 LNA——**全自动加载零交互**，服务器随进程存活（浏览器内刷新不受影响）。失败（离线/无 Chrome）回退 `reveal_trace_and_open_ui` 拖拽方案（ui.perfetto.dev + dbus FileManager1 高亮 trace 文件，File API 无网络请求）。
- **为什么不用「ui.perfetto.dev + 本地 HTTP + ?url= 深链」（2026-09-03 实测 Chrome 152 双重拦截）**：① ui.perfetto.dev 自带 CSP `connect-src` 白名单（'self'/localhost:8080/127.0.0.1:9001 等固定口），随机端口全被 block（fetch 抛 TypeError，零网络请求）；② 白名单内端口再被 Chrome LNA（Local Network Access）权限拦——公网 https 页面 fetch loopback 需用户授权，深链自动 fetch 无手势被静默拒（带 Access-Control-Allow-Private-Network: true 也过不了）。
- **验证方法论教训（本轮踩坑，后续必守）**：① headless Chrome `--dump-dom` 验证 SPA 必须用正确判据——数 `<canvas` 标签/检查页面特征文本，**不能 `grep -c canvas`**（Chrome 错误页内嵌 JS 也含该字符串，"Sched" 会匹配 "scheduled"，本轮曾据误判得出"方案已验证"）；② `--virtual-time-budget` 不等 wasm 真实解析（16MB trace 解析需真实时间），dump 总在解析完成前——**渲染验证看完整 console 日志**（成功标志：`Opening trace using built-in WASM engine` → `Loading trace N MB` → 路由切 `#!/viewer?local_cache_key=...` → WebGL 活动日志）；③ 对照实验的 server 进程会被自己早期的清理脚本误杀（pkill 模式匹配过宽），"对照成功"可能是 ERR_CONNECTION_REFUSED 错误页——对照前先确认对照体存活。
- **GUI 侧**：主区双 tab（性能指标 / Perfetto 分析），报告不与指标混排；**分析页自动隐藏左侧采样控制栏**（报告占满全宽，切回指标页恢复）；录制期间留在指标页观察实时曲线，done/error 自动切到分析页；分析页顶部「在浏览器打开 Perfetto UI」按钮（recorded 起 enabled，`trace` 事件 payload 附 trace_path）→ 本地镜像 UI 自动加载，失败自动回退拖拽。**暗/亮双主题**（tabBar 右侧按钮切换 + localStorage 持久化）：CSS 变量全量化（mocha/latte 双色板）+ `color-scheme`（select/number 等原生控件暗色渲染）；**checkbox 为 appearance:none 自绘**（webkit2gtk 对原生 checkbox 暗色渲染支持不全，实测仍白底——自绘不依赖引擎原生渲染）；canvas 图表与实时面板取色动态跟随主题（uiColors() 读 CSS 变量 + 双 series 色板）；status 栏移至 tabBar 行右端（分析页隐藏侧栏后仍可见）。
- **实测基线（SS3）**：裸配置时期 10s ≈ 11.8MB / sched 14 万事件；对齐团队 general_debug 配置后 10s ≈ 46MB（atrace slice 17 万 + log 3425 条 + cpuidle counter）；svm 空闲时包内仍可见线程级毫秒级 CPU/抢占明细；不存在的包名 → "无调度事件"如实上报；1s 极限窗口正常。


---

### agent（设备端采样器，xperf-agent）

**为什么**：adb 轮询单轮固定 6+ 次调用（每次 ~13ms 起，`dumpsys meminfo` ~100ms），低间隔下开销超过间隔本身，且每次 adb 调用都扰动被测系统。agent 常驻设备直接读 /proc（微秒级），NDJSON 经 `adb exec-out` 长连接流式回传（PerfDog Agent 同构思路，但免装 APK：纯静态二进制）。当前 CLI/GUI 的**唯一**采样路径。

**部署**：
- 本机二进制：`target/aarch64-linux-android/release/xperf-agent`（不存在时自动执行 `cargo build -p xperf-agent --target aarch64-linux-android --release`；需 NDK，链接器配置在 `.cargo/config.toml`，当前绑定 NDK 25.1.8937393 / API 26）
- 设备端路径：`/data/local/tmp/xperf-agent`
- 更新机制（`agent::deploy_agent`）：大小+mtime 双判（源码变更自动重建：ensure_agent_built 比较 src 树内任一 .rs 的最新 mtime vs 二进制 mtime——agent 已多模块，不能只盯 main.rs）；deploy 前自动 `try_adb_root`（IO 等需 root 的指标）
- 手动重建推送：`cargo build -p xperf-agent --target aarch64-linux-android --release && adb push target/aarch64-linux-android/release/xperf-agent /data/local/tmp/`

**代码结构**（模块拆分，main.rs 只留协议/参数/节拍循环 493 行）：
- `main.rs`：NDJSON 协议头注释、Args/parse_args、节拍主循环、公共工具（emit/json_escape/now_ms/dumpsys，crate 根私有项对所有子模块可见）
- `proc.rs`：/proc 与 sysfs 读取（stat jiffies/resolve_pids/cpufreq/io/net）+ `PidState::sample_cpu`（CPU% + 线程明细）
- `mem.rs`：smaps_rollup（低间隔）+ dumpsys meminfo App Summary（≥500ms），`sample_memory` 直接 emit
- `fps.rs`：SurfaceFlinger 图层发现 + 帧时间戳差值 + jank，`FpsState::sample_round`
- `thermal.rs`：thermalservice 解析 + sysfs thermal zones 兜底，`sample` 返回是否有数据
- `gpu/`：`mod.rs`（GpuPath 枚举 + detect_gpu_path_ex + `spawn_stream_parser` 公共读线程骨架 + emit_gpumem）+ `kgsl.rs`/`qnx.rs`/`topgpu.rs`/`ligfx.rs` 四通道；三流式通道样本归一为 `GpuEvent::Sys/Proc` 后交公共读线程 emit（wire 格式不变：按通道字段有无按需输出 util/maxmhz）

**要点**：
- 绝对节拍：`start + round × interval`，漂移时发 err 行（"round N overrun"）
- CPU 窗口 = 相邻两轮差值（常驻保有状态，无 phase1/phase2 结构）
- 需要 root（读他进程的 /proc、smaps_rollup）；内部设备 adbd 已 root
- 终端输出：interval ≥ 500ms 逐条详细打印；< 500ms 按 ~1s 聚合（avg/max），全量明细在流式 CSV；CSV 时间戳毫秒精度（`%.3f`）
- 主机断连（EOF）→ 自动重连恢复（见上）；Ctrl-C → exec-out 关闭 → agent 写 stdout 失败自行退出（节拍循环整轮零输出时发空行探活，一个周期内感知断连；host 侧 next_event 跳过空行，零协议影响）

**验证基线**：svm @ 50ms 间隔，78 样本均值 15.03%，与 adb top 一致；50ms 窗口可见 25-47% 的瞬时毛刺（1s 采样看不到）。

---

### 输出文件触发时机

| 场景 | 触发条件 | 输出位置 |
|------|---------|---------|
| CPU/内存/FPS/线程/B类指标 CSV | **采样时流式追加**（每个样本到达即写并 flush，崩溃只丢尾部） | `log/<pkg>/<ts>/{cpu,memory,fps,thread,freq,thermal,gpu,io,net}/` |
| CPU 图表（每 PID + 汇总） | 程序退出，数据点 > 1 | `log/<pkg>/<ts>/cpu/` |
| 内存图表（每 PID + 汇总） | 同上 | `log/<pkg>/<ts>/memory/` |
| 线程时序图 | `--thread --cpu`，退出时有数据 | `log/<pkg>/<ts>/thread/` |
| B 类图表（freq 每核/temp 每传感器/io 每 PID/net/gpu） | 退出时对应序列 > 1 点 | `log/<pkg>/<ts>/{freq,thermal,io,net,gpu}/` |
| perfetto 深挖（--trace N） | 录制完成即拉回；分析随即落盘 | `log/<pkg>/<ts>/trace/{*.pftrace, trace_analysis.txt, trace_queries.sql}` |

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
