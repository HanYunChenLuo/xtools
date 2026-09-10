# WORKSPACE.md — xtools 工作区待办与状态

> 本文件记录跨会话的待办事项（backlog）。每次会话的历史总结见 `SESSION.md`。
> 完成一项就把状态改为 ✅ 并注明完成的 commit；新增想法随时追加。
> 最后更新：2026-09-10（晚）：**H 节 SS4 指标适配完成**（任务 A-E：frametimeline FPS host 通道 7a3e9fb / ligfx GPU host 通道 9004f96 / simpleperf cpu-clock + 千分位解析 b8754bf；九项指标矩阵 + C 类全回归，详见 SESSION 当日条目）

## 当前状态速览

- 采样架构：**设备端 agent（xperf-agent）daemon 化为唯一采样路径**——`--daemon` 常驻监听 `localabstract:xperf-agent`，host 经 adb forward+TCP 连接（版本握手/suicide 重推/start/stop/ping；多 host 上限 10；0 会话 60s 自杀）；断连自动重连恢复；模块化布局（main/proc/mem/fps/thermal + gpu/ 五通道）
- 指标覆盖（9 项全实现）：CPU（单核口径）、内存、FPS、CPU 频率、温度/热降频、GPU、IO、网络、GPU 显存
- **多设备 adb（两级）**：全局 `TARGET_SERIAL`（CLI）+ **会话级 `serial: Option<&str>` 参数**（core 各入口：spawn_agent/deploy/trace/simpleperf/coldstart/detect_platform，`adb_for`/`run_adb_command_for` 注入）；**GUI 每设备一 tab 并行**（详见 CLAUDE.md「多设备 adb」）
- **GUI 多设备改版**：顶栏设备 tab（热插拔动态增删、断开灰显保留数据插回自动恢复）+ 每设备独立页（侧栏 + 性能指标/Perfetto/Simpleperf 三子 tab）+ `DeviceSession` 类（事件按 payload.serial 分发）+ **「应用操作」**（打开/重启应用，activity 留空自动 resolve-activity，`am start -W` 顺带测冷启动进「冷启动」面板，系统重定向警示）
- **平台抽象**：`xperf-core/src/platform/` trait + adb devices -l 自动检测（SS2MAX/SS2PRO/SS3/SS4/Android）；agent 加 `--platform`/`--qnx-host` 参数
- **GPU 五通道**（detect_gpu_path_ex 按平台选路）：kgsl sysfs（Android/SS2）/ QNX telnet（SS3，真 busy%/util%/频率+每进程）/ topgpu（SS2MAX）/ ligfxprofilerd logcat（SS4，GVM 无输出永不命中——实际走 host 侧通道）/ dumpsys gpu 显存保底；全部补采 dumpsys gpu 显存
- **SS4 host 侧指标通道**（hostchan.rs）：GPU busy=经桥接网关读 MindRT logcat ligfxprofilerd（合成 AgentEvent 经 mpsc 汇入 AgentStream，CLI/GUI 零改动）；**FPS 已回归设备端 per-layer 路径**（协议 v5：A16 图层名须带 `<hex> ` 前缀，曾误判阉割绕道 host frametimeline 已废弃删除）；SS4 平台限制：GVM 无 cpufreq/thermal（VM 隔离）、PMU 未虚拟化（simpleperf 自动 cpu-clock）
- **C 类验证能力**：阈值告警（--threshold，静止界面不误报）+ 退出验证报告 + 冷启动（--cold-start / GUI 打开/重启应用，模块 core/coldstart.rs）+ **simpleperf 函数热点**（--stack N：调用栈录制 + 线程/self/children 三视图报告，CLI 独立/并行两模式 + GUI 独立 tab）+ **基线对比**（--save-baseline/--compare-baseline：两次运行 diff 回归判定，CLI/GUI 共用，详见 CLAUDE.md「基线对比模式」）
- 落盘：数据根 `/tmp/xperf`（CLI 流式 CSV + 退出图表；GUI 完整历史 + CSV 导出，共用同根；GUI 深挖目录 `<pkg>/<ts>-<serial>/` 防双设备撞名）；清理走 CLI `--clean-cache` / GUI 按钮（~/.cache/xperf + /tmp/xperf，2bd6bca）
- GUI：9 张折线图 + 实时数值面板 + Top 线程 + 峰值 + 冷启动面板 + 间隔档位下拉 + 实际周期标注 + 勾选即时生效（自动重启会话）+ Perfetto 分析（独立 tab 报告 + 浏览器自动加载 + 每秒录制进度）+ **函数热点**（独立 tab + 每秒录制进度 + 浏览器火焰图）+ 暗/亮双主题
- agent 部署：自动尝试 adb root（**仅车机平台**，XPERF_NO_AUTO_ROOT=1 旁路；hello 带 root 标志，非 root 按能力降级——WORKSPACE G 节矩阵）；src 树内任一 .rs mtime 变化自动重建
- 测试：**全量全绿**（core 81+7 ignored（4 条 hppc 集成：隧道×2/init+acquire_root）：协议/transport/trace/simpleperf/baseline/coldstart/设备 diff/auto-root 守卫；xperformance 5：alerts；GUI 6：export_csv×2 + 基线×2 + 多会话隔离 + 远程配置；xrm 2），clippy 零警告，**cargo doc 零 warning**（默认 lint 集 + missing_docs 三 crate）
- **SSH 远程后端（feature/ssh-remote 已合 main）**：`--remote hppc` 经 SSH 隧道连远端 adb server（hop#1 承载 adb 协议 + hop#2 每设备一条承载 agent 流），采样/trace/simpleperf/断连重连/GUI 连接切换全通；详见 CLAUDE.md「SSH 远程后端」与 `docs/DESIGN-ssh-remote.md`
- 设备：SS3 6eb792dfb0f（adbd root，QNX GPU 通道 + 多设备并行已真机回归）；SS2MAX d1f39648c1f（adb root 可用；**多设备并行 + 冷启动 COLD 874ms 已真机验证**）；SS4 经 hppc 桥接为 localhost:5559（MindRT `42087266b1f` 中继；桥接/指标双适配完成，H 节）
- **测试对象（555ffab 起统一）**：`example/apk/filament-gltf-viewer-v1.76.0-android.apk`（git-lfs 管理，包名 `com.google.android.filament.gltf`，入口 `.MainActivity`）——真机测试一律用它，不再用 svm。已装 SS3 + SS2MAX

---

## A. 已知缺陷

（无——两轮 code review 的严重/一般问题已全部修复）

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

- ~~SS4 FPS 数据源升级候选：getfps -w~~（**已核销**，2026-09-10 晚）：getfps 逆向发现其底层即 `dumpsys SurfaceFlinger --latency`，价值是揭示了 SS4/A16 的图层名须带 `<hex> ` 别名前缀——agent v5 据此修复查询名，设备端 per-layer 路径恢复（见 E 节 FPS 条目终态）
- [x] ~~agent 单文件拆分~~（531798a + 99d1b74 review 修复）：main.rs 1848 行 → 10 文件（main 493 + proc/mem/fps/thermal + gpu/{mod,kgsl,qnx,topgpu,ligfx}），三份读线程骨架抽公共 `gpu::spawn_stream_parser`，四段相同的 gpumem 补采臂合并；测试 23 个随模块迁移全绿。真机回归：SS2MAX 新旧 agent 同机对比事件分布/wire 格式/smaps 值一致。附带修复 host 侧 `ensure_agent_built` 只盯 main.rs 的 mtime 检查（改扫 src 树，touch 子模块已验证触发重建）
- [x] ~~SS3 QNX 通道真机回归 + kgsl 统计链停滞修复~~（2026-09-03）：回归发现 QNX frame 流"1 条后停走、跨会话交替通/停"。黑盒实验（重启车机前后共 10+ 组对照）定位根因：**kgsl 统计链是驱动全局的，会话/fd 关闭都不清理**（泄漏直到整机重启）；`echo >` 式即开即死连接写入撞存量链只 flush 一窗即停，长活连接（`exec 3>`）写入则全链重相位持续输出；多链锁步产生重复行。修复（qnx.rs）：① 启动命令改 `exec 3>` 持 fd 写入；② slog 行按"与上一行完全相同"去重（Sys/Proc 各一条）；③ 看门狗兜底（frame 静默超 3×周期经 fd3 重写自愈，≤3 次）。真机验证：4 条泄漏链硬场景下启动顿 ~5s + 1 次自愈后稳定 1/s；gpu/gpuproc/gpumem 三类事件与 CSV 全通（eid→pid 9671 归因正确）；连续多轮 kill/重跑稳定。已知残留：存量链的窗口 flush 会带来少量同值重复样本（数值正确，重启清零）
- [x] ~~xperf-core 轮询参考实现删除~~（225d89b，-1653 行；保留 ThreadCpuInfo/MemoryDetails/FpsTimeSeriesData/PidStats/SampleEvent 等协议类型）

## E. 已知遗留（评估过，低风险不阻塞）

- ~~SS4 FPS 无数据源~~（**已解决并二次修正**，终态 2026-09-10 晚 agent v5 / 1486463）：v4 曾误判"QCM 构建阉割 --latency"绕道 host frametimeline 通道（7a3e9fb）；**用户指点 getfps -w 后逆向确认真因：A16 SF 的 --latency 只认 `--list` 原始行的 `<hex> <name>` 别名形态**（带前缀 65 行真数据 vs 干净名 1 行刷新周期）——agent fps.rs 查询名双轨（A16 保留前缀/旧平台干净名），设备端 per-layer 路径恢复（协议 v5，SS4 短路撤销、host frametimeline 通道删除），真机 59-61fps + 杀进程重发现（#549→#579）+ SS2MAX/SS3 回归全通
- SS2MAX GPU 显存无数据源（2026-09-07 root 下全路径确证：dumpsys gpu 无 Memory snapshot 段 + /sys/kernel/debug 未编译进内核 + /proc/kgsl 不存在，平台限制）
- SS3 kgsl 统计链（见 D-2/CLAUDE.md）：三层清理已落地（agent 退出钩子 + setsid + host 条件兜底，**均带 pgrep 多会话并发保护**），SIGINT/Ctrl-C/正常退出路径真机验证停链成功、下一会话零自愈即起流；残余风险仅 agent 被 SIGKILL 暴杀（无钩子机会）与 reboot 后首会话（开机 5000ms 链在流，走一次看门狗自愈 ~8s）
- ~~SS2MAX gpubusy 计数器恒 `0 0` / busy% 可 >100%~~（**已修**，commit 见 SESSION 2026-09-07：根因是 SS2MAX 厂商内核的 gpubusy 为**窗口语义**——读数是上一 ~1s 窗口的 busy/total µs，total 恒 ≈1e6 非累计；按累计差值解析出 1662%。`GpuBusyCalc` 三判据自动锁定窗口语义直读 busy/total；真机对照内核 `gpu_busy_percentage` 均值 75.5 vs 74.7 一致。原"恒 0 0"即 GPU 空闲时的窗口读数，非停走）
- ~~SS4 ligfx Frequency 单位待真机核实~~（**已核销**，2026-09-10 任务 B）：恒 `1000 Hz` 空闲/负载不变，GPU VFIO 直通两侧无 kgsl/devfreq 节点可对照——按「定频占位/单位标注存疑」处理，事件原样透传 mhz=1000，业务侧只看 Utilization。`persist.vendor.ligfxprofiler.sampling_interval_ms` 实测动态读取，但调小（1000）会致 ligfxprofilerd 停输出（恢复 5000 即好）——勿调
- **SS4 GVM 无 cpufreq/thermal（2026-09-10 确证，VM 平台限制）**：`/sys/devices/system/cpu/cpu0/cpufreq/` 不存在（hello maxkhz 全 0 即此因，root 下同），`--freq` 探测禁用；`/sys/class/thermal/` 空 + thermalservice HAL Ready=false（连 SS3 的 test HAL 假数据都无），`--thermal` 探测禁用。PMU 未虚拟化（simpleperf cpu-cycles 8s 仅 6 样本 → core 自动改 cpu-clock，b8754bf）
- **QNX proc 链泄漏（2026-09-07 发现，未修）**：三层清理只写 `gpubusystats`（frame 链），`gpu_per_process_busy` 进程链无停止手段（实测死写入者 toggle/写 0/log_level 0 均无效）——每 --gpu 会话泄漏一条，多日累积 ~20 条锁步洪泛，疑似挤占致 frame 链无法启动（两轮会话 0 frame 事件）。恢复 = `adb reboot`（整 SoC 复位含 QNX）。待找到正确停链命令后补进 agent 退出钩子与 host qnx_stop_stats
- ~~孤儿 adb exec-out 泄漏~~（**已根治**，daemon 化 commit 见 SESSION 2026-09-07 晚条目：agent 常驻 daemon + host 经 forward/TCP 连接，host 死亡 → TCP 断开 → 会话即收，不再产生孤儿流；daemon 0 会话 60s 自杀。早前过渡方案 e692d4e 的 cleanup_orphan_agents/stdin EOF 监测已被 daemon 化取代并移除）
- ~~多设备连接时所有 adb 命令不带 -s 会失败~~（**46bd161 已修**：全局 `-s` 注入 + CLI `--device` + GUI 设备下拉，SS3+手机双连真机回归；原候补转正，详见 CLAUDE.md「多设备 adb」）。GUI 多台未指定 `--device` 的自动启动跳过路径为逻辑验证 + 单测覆盖（验证时手机恰断开未双机复现，行为由 pick_device 单测锁定）
- QNX 双会话并发交互（五轮 review 实测）：①后启动会话的 fd3 写入给先启动方一次 ~7s GPU 停走（看门狗自愈恢复）；②各方 GPU 事件密度升至 ~2×（双方写入产生非锁步多链，行级全等去重不覆盖，值为真值仅密度偏高）；③退出清理已有并发保护（pgrep 检测其他 agent 跳过停链，agent 钩子 >1 / host 兜底 ≥1+收尸等待，真机验证）——并发监控本身罕见，记录不修
- ~~GUI add_marker 不写 markers.csv~~（已失效：GUI 打点功能整体删除，78f93a9，仅剩 CLI socket 打点）
- marker 每连接线程无界（有 10s 读超时兜底）
- **多宿主协议版本战（2026-09-10 晚实测踩坑，协议 bump 期间必看）**：新旧两个 host 进程（如旧版 GUI + 新版 CLI）同时连同一设备时，各自 `ensure_daemon` 发现版本不符就 suicide+重推 daemon——两边版本不同则**互相杀死对方的 daemon 无限循环**，表现：会话 hello 后流冻结、设备端 daemon 进程与 `@xperf-agent` socket 堆积（实测 2 进程/6 socket）。协议 bump 升级时旧宿主进程必须先退出；排查命令 `adb shell 'pgrep -f xperf-age[n]t; grep -c xperf-agent /proc/net/unix'`，清理 `pkill -f xperf-age[n]t`
- **bridge 边缘态（2026-09-10 review 记录）**：bootstrap 探测失败冷却期（60s）内 MindRT 以 `is_gateway=false` 漏进设备列表（pick_device 可选中/GUI 可建 tab）——仅中继坏掉时出现，选中后采样会按普通设备失败；不修（正常路径 bootstrap 秒成，冷却语义是防反复探测）
- GUI 基线/应用操作按钮与设备 tab 切换的点击渲染为人工目验项（后端链路由命令级测试锁定：save/compare 端到端 + build_summary 口径 + 多会话隔离；真机日志已验手动开始/勾选重启/trace 录制全链路）

## F. SSH 远程调试（已完成）

- [x] **SSH 远程后端**（2026-09-09，`feature/ssh-remote` 分支合 main；设计 `docs/DESIGN-ssh-remote.md` v2 + 实现 S1-S11 全步骤）：真机接在 hppc 上，本机跑 GUI/CLI 经 SSH 调试采样/perfetto/simpleperf。commit 链：f45d44b（设计）→ c156141（S1 transport 基础）→ e520787（S2 隧道）→ 759d72d（S2a hop#2 映射表）→ cd81a37+b578b9f（S3 utils 注入）→ ea61e05（S4 init/shutdown）→ e4ae289（S5 CLI + S6 agent hop#2）→ b5c179a（S9 断连重连）→ f2a58a9（S10 GUI）。真机回归：远程采样/trace/simpleperf/断连重连/并发会话/退出零残留全通（详见 CLAUDE.md「SSH 远程后端」节）


## G. 非 root 设备支持（已完成）

- [x] **权限矩阵探索 + 无 root 机器支持**（2026-09-09 完成，`feature/non-root-support` 合 main）：commit 链 19ecef9（auto-root 收敛仅车机 + XPERF_NO_AUTO_ROOT 旁路）→ ea3986f（协议 v3 hello.root + 内存降级 + RSS 兜底 + CLI 提示）→ 9c34b23（QNX 内嵌 telnet 去 busybox + --qnx-stop 模式 + host 换道）→ ec2de2b（GUI 权限徽章 + 获取 root 按钮 + 降级提示透传）。矩阵实测（两机 shell 逐项验证）见下表；**真机回归全通**：非 root SS3 @500ms 九项指标（QNX 内嵌 telnet shell 起流 20.3% busy + 每进程归因）、非 root SS2MAX @50ms（内存 dumpsys 降级 500ms 周期 + RSS TOTAL RSS 兜底 + kgsl 74.7%）、root 回归 SS3 @50ms（auto-root 恢复全指标）、--qnx-stop 守卫语义（不动已停链）真机验证。
  - **残留目验项**：仅 GUI「获取 root」按钮的 DOM 点击绑定（一行 addEventListener，invoke 注册编译期锁定）；后端 `acquire_root` 已真机闭合（bf50bbc `test_acquire_root_hppc`：SS2MAX reboot 掉 root 后 3.46s shell→root 全迁移）。
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
- [x] **SS4 指标适配**（2026-09-10 晚，`feature/ss4-metrics`，实施依据 `docs/DESIGN-ss4-metrics.md`）：任务 A FPS frametimeline-only perfetto host 通道（7a3e9fb：hostchan.rs 5s 窗循环 → NULL display 合成流汇总 fps/jank 汇入 AgentStream；agent --fps 在 SS4 短路、协议 v4；真机 59.8fps 稳态/杀进程停发/重启恢复/GVM reboot 重连恢复全通）→ 任务 B GPU ligfx host 侧通道（9004f96：经网关 logcat 流读，Sys→Gpu/进程行→GpuProc comm 归因；真机 busy 33.8%/进程 11% 归因正确；Frequency 恒 1000 核销、sampling_interval 调小致停输出现已记录）→ 任务 C 九项矩阵 root/非 root 两态实测（freq/thermal 为 GVM VM 隔离平台限制，探测禁用符合预期；hello maxkhz 全 0 根因=GVM 无 cpufreq sysfs）→ 任务 D C 类回归（trace✅/冷启动 246ms✅/simpleperf：PMU 未虚拟化致 cpu-cycles 无效 → 自动 cpu-clock + 千分位样本数解析修复 b8754bf，3908 样本/9s/基线保存-对比全链路持平 9 项）→ 任务 E 文档收尾（ss4.rs 桩补实/CLAUDE.md「SS4 host 侧指标通道」节/E 节核销）→ 独立 review 修复（7e0a748：2 严重 4 一般全修，含 ligfx 独占登记重连竞态宽限接管、EOF 热重连退避、阻塞读看门狗；真机 GVM reboot 重连场景复测通过）

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
