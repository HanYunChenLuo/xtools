# WORKSPACE.md — xtools 工作区待办与状态

> 本文件记录跨会话的待办事项（backlog）。每次会话的历史总结见 `SESSION.md`。
> 完成一项就把状态改为 ✅ 并注明完成的 commit；新增想法随时追加。
> 最后更新：2026-09-15：**J 节完成真机归因，新增 Filament 源码对照与优化验证任务**——hppc 源码位于 `/home/han/code/graphic/filamentdir/src/`，主线 `filament-v1.38.0-dev`、1.74.1 `filament-v1.74.1-dev`。已锁定 broadcast_error 的 1.74 backend CPU 增量，待新会话结合源码定位优化空间。上一状态：2026-09-14 晚 logcat 支持完成（`feature/logcat` 合 main，详见 I 节条目）；I 节仅剩「命令行输入」

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
- 测试：**全量全绿**（core 94+7 ignored（含 4 条 hppc 集成：隧道×2/init+acquire_root）：协议/流合并/hostchan（ligfx 解析+registry）/transport/trace/simpleperf/baseline/coldstart/设备 diff/auto-root 守卫；xperformance 5：alerts；GUI 8：export_csv×2 + 基线×2 + 多会话隔离 + 远程配置等；xrm 2），clippy 零警告，**cargo doc 零 warning**（默认 lint 集 + missing_docs 三 crate）
- **SSH 远程后端（feature/ssh-remote 已合 main）**：`--remote hppc` 经 SSH 隧道连远端 adb server（hop#1 承载 adb 协议 + hop#2 每设备一条承载 agent 流），采样/trace/simpleperf/断连重连/GUI 连接切换全通；详见 CLAUDE.md「SSH 远程后端」与 `docs/DESIGN-ssh-remote.md`
- 设备：SS3 6eb792dfb0f（adbd root，QNX GPU 通道 + 多设备并行已真机回归）；SS2MAX d1f39648c1f（adb root 可用；**多设备并行 + 冷启动 COLD 874ms 已真机验证**）；SS4 经 hppc 桥接为 localhost:5559（MindRT `42087266b1f` 中继；桥接/指标双适配完成，H 节）
- **测试对象（555ffab 起统一）**：`example/apk/filament-gltf-viewer-v1.76.0-android.apk`（git-lfs 管理，包名 `com.google.android.filament.gltf`，入口 `.MainActivity`）——真机测试一律用它，不再用 svm。已装 SS3 + SS2MAX + SS4

---

## A. 已知缺陷

（空——镜像+录屏并存缺陷 2026-09-14 核销，见 SESSION 当日条目）

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

- [x] ~~内存 Private Other 拆分 DMA-BUF 分类~~（**已完成**，2026-09-11，`feat/dmabuf-split` 合 main，e5071da，协议 v8）：Private Other 是无语义兜底桶（真机根因 = 114 个 `/dmabuf:` VMA 共 614MB PSS；Graphics 桶只按 kgsl/drm 设备节点名匹配，dmabuf 不命中；内核 `/proc/<pid>/dmabuf` 此 GVM 未编译）。落地：agent Full 模式 root 下扫 smaps 按 VMA 名（`/dmabuf`/`[anon:dmabuf`）聚合 Pss 单列 `dmabuf`，other 扣减（稳态 8 分类合计恒等 PSS；分配剧变期两次快照不同步可暂超，如实不钳制——见 CLAUDE.md 内存采样节）；Smaps/DumpsysFallback/非 root dmabuf=0。mem 事件增字段（serde(default) 双向兼容）；CLI 打印/图表、GUI 面板（└ DMA-BUF + Private Other 改「其他」）、CSV（DMA-BUF (MB) 列）全链路。parse_dmabuf_pss 头行按字段解析（弃固定列切片）。真机：SS4 gltf DMA-BUF 615.6MB/Other 9.0MB 稳态合计=PSS；SS3 533.9MB 回归；非 root SS2MAX dmabuf=0 七类合计=PSS；100ms smaps 路径 0 值正常。测试：agent 32（设备）+ host 95+8+5+2 全绿，clippy/doc 零警告
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
- [x] ~~**截屏与录屏**~~（**已完成**，2026-09-12 主体 + 2026-09-14 并存缺陷核销，`feature/screen-capture` 合 main）：截屏=`adb exec-out screencap -p` 直写本机 PNG（PNG 魔数偏移定位剥 stdout 前缀警告——SS4 实踩）；录屏=scrcpy `--no-window --record`（复用镜像隧道双钉同号全链路；停止 SIGINT 优雅封盘 ≤3s 宽限 SIGKILL 兜底；CLI 倒计时从首帧落盘起算；产物核验防假阳性；**启动未建流自动重试一次**——设备端 server 启动偶发中止的自愈，CLI/GUI 同策略，GUI 前端 `retrying` 状态）。CLI `--screenshot`/`--record N`（独立+采样并行同窗口）；GUI 侧栏「屏幕捕获」区截屏按钮+录屏 toggle（AX 目验通过）。真机回归 SS3/SS4 全通，镜像+录屏并存 13 连过

---

## J. 性能分析任务（facedemo filament 版本对比）

- [x] ~~**filament 1.74 CPU 升高归因**~~（**已完成**，2026-09-15，真机 SS4 `--remote hppc` 全程实测）：场景 = `毛绒 3DGS 调试`（FurBoy3dgsActivity2）+ niuzai 3DGS 模型 + awaken2 循环播放（用户指正场景入口；FaceActivity/PIXS 均非目标）。**核心结论**：
  - **1.74.1 渲染帧路径 CPU = 12.0%（单核口径）vs 主线 1.38 帧路径 ≈ 7.7% —— +56%，即文档 7.3→11.6 增幅的来源**（FPS 44.9 / GPU 进程 busy 11.5% 两版完全一致，纯 CPU 侧差异，全部集中在 XFW:Main 渲染线程）。1.74 增量分布（simpleperf 符号化热点）：Vulkan 提交链（`VulkanDriver::finish` 26.7% → `flush` 21.1% → `vkQueueSubmit` 16.9% → ioctl 13.7%）、UBO 上传（`updateBufferObjectCommon` 15.7%）、compute dispatch 10.3%、每帧 `SurfaceRenderer` 析构 10.1%（fdsan close + `gsl_syncobj` 销毁）
  - **主线 1.38 在该设备存在每帧 Vulkan 管线重建病理**：CPU 的 50.6% 持续花在 `VulkanPipelineCache::createPipeline` → `vkCreateGraphicsPipelines` → Adreno `libllvm-qgl` 着色器编译（实测 15.4% 总量，10+ 分钟无衰减、与白天/黑夜模式无关；扣除后 ≈7.7% 恰为文档主线 7.3%）。**机制（反汇编实锤）**：1.38 的 `bindPipeline` 以 336 字节渲染状态快照为缓存 key（MurmurHash3 + 逐字节 memcmp），`bindRenderPass` 把 **VkRenderPass 指针**、`bindVertexArray` 把顶点缓冲句柄直接写入 key（按句柄身份而非兼容性）——demo 每帧有句柄变化（嫌疑：应用层每帧 SurfaceRenderer 重建 / 3DGS 排序缓冲重建，两版共有行为）→ 每帧 miss → 全量编译。1.74 Vulkan 后端已重构（函数族完全不同）不受影响
  - **实测与文档的差异**：v1.74.1 实测 12.0% 吻合文档 11.6%；主线实测 15.4% ≠ 文档 7.3%（差值即上述编译病理）——文档主线测量时无此现象，测量条件（固件/日期/工具口径）待用户确认
  - 附带发现：**facedemo 的 FaceActivity（主驾入口）在两版 APK 上均 100% native 崩溃**（`NoClassDefFoundError: androidx.coordinatorlayout.widget.CoordinatorLayout` 仅被 dex 引用未定义（被 catch 非致命）→ 随后 native 堆损坏 scudo/FORTIFY SIGABRT，109ms 内死）——与 filament 版本无关的应用/环境问题；FurBoy3dgsActivity2 / PixsActivity 不受影响
  - 数据目录：`/tmp/xperf/com.lixiang.facedemo/`（CLI 采样 CSV + simpleperf 报告，Mac 端）；APK 留存 hppc `/tmp/facedemo/`
  - **broadcast_error 干净对照（后续补充，关键）**：换 broadcast_error 动画循环后主线**编译病理消失**（createPipeline 0%——awaken2 特有的每帧 3DGS 点排序/缓冲重建是 miss 诱因），渲染线程 XFW:Main：**主线 7.26% vs 1.74 12.04%（+66%）——精确复现文档 7.3→11.6**（文档测量时主线无编译现象的原因即此）。1.74 渲染路径开销与动画类型无关（两动画均 ~12%）；broadcast 另有语音播报开销（binder×4 + ART GC ≈4.6-5.6pp，两版同量级，进程总量 12.86% vs 16.62%）。**结论修正**：文档增幅 = 1.74 渲染路径真实增量（提交链/UBO 上传/每帧 SurfaceRenderer 析构），与动画无关；awaken2 场景下 1.38 的句柄 key 病理被 1.74 重构顺带修复
  - **broadcast_error 进一步函数归因（2026-09-15）**：严格同场景 60s 重测：主线进程 11.69% / XFW:Main 7.15% / FPS 44.82 / GPU 11.55%；1.74 进程 16.89% / XFW:Main 12.21% / FPS 44.81 / GPU 11.94%。额外 5.06pp 几乎全在 XFW:Main。主线热点为 `VulkanCommands::flush` 8.67%、`VulkanBuffer::loadFromCpu` 8.55%、present 7.87%、`VulkanStagePool::gc` 5.70%；1.74 为 `VulkanDriver::finish` 28.46% → `VulkanCommands::flush` 22.95% → `VulkanCommandBuffer::submit` 22.71% → `vkQueueSubmit` 18.85%，`updateBufferObjectCommon` 17.81%/`updateBufferObject` 17.09%、compute 10.57%、`SurfaceRenderer::drop` 10.37%。children 百分比包含关系不可相加；结论是 1.74 backend 每帧 command buffer 提交、buffer/descriptor 更新、fence/resource 生命周期管理 CPU 更重；应用动画/CUA 更新反而从主线约 12.5%/10.2% 降到约 5.8%/4.3%，差异不是 broadcast_error 动画逻辑。


- [ ] **Filament 源码对照与优化验证（2026-09-15 新任务，需新会话继续）**：结合 hppc 主机源码定位 1.74.1 相对主线 1.38 的 CPU 增量，评估可落地优化并在 SS4 真机回归。
  - **源码位置（hppc）**：`/home/han/code/graphic/filamentdir/src/filament-v1.38.0-dev`（主线）与 `/home/han/code/graphic/filamentdir/src/filament-v1.74.1-dev`（1.74.1）；注意 `/home/han/code/graphic/filamentdir` 可能是外层工作区，源码对照应直接进入上述两个版本目录
  - **已锁定真机复现条件**：SS4 `localhost:5559`（经 `--remote hppc`）、`毛绒 3DGS 调试`/`FurBoy3dgsActivity2`、niuzai 模型、`broadcast_error` 循环；两版 FPS≈44.8、GPU 进程 busy≈11.6-11.9%，但渲染线程 XFW:Main 主线 **7.15%** vs 1.74.1 **12.21%**，进程 CPU **11.69%** vs **16.89%**，差异 **+5.06pp** 集中在 XFW:Main
  - **已确认热点（simpleperf，`.so` 含完整符号；children 百分比有包含关系，不能相加）**：主线 `VulkanCommands::flush` 8.67%、`VulkanBuffer::loadFromCpu` 8.55%、present 7.87%、`VulkanStagePool::gc` 5.70%、compute 4.57%；1.74.1 `FEngine::execute`/`FRenderer::renderInternal` 路径下，`VulkanDriver::finish` 28.46%、`VulkanCommands::flush` 22.95%、`VulkanCommandBuffer::submit` 22.71%、`vkQueueSubmit` 18.85%、`updateBufferObjectCommon` 17.81%、`updateBufferObject` 17.09%、compute 10.57%、`SurfaceRenderer::drop` 10.37%/`FRenderer::endFrame` 10.22%
  - **当前判断（待源码证实，不要直接当最终根因）**：1.74 backend 的每帧 command buffer flush/submit、buffer/descriptor 更新、fence 与资源生命周期管理路径 CPU 更重；应用层反而变轻（动画更新/CUA 约主线 12.5%/10.2% → 1.74 约 5.8%/4.3%）。需要源码 diff 证明是实现变化、调用次数变化还是对象生命周期/同步策略变化
  - **优先源码入口**：两版对应 backend Vulkan 实现（`backend/src/vulkan` 或实际目录）、`VulkanCommands::flush` / `VulkanCommandBuffer::submit` / `VulkanDriver::finish`、buffer upload（`VulkanBuffer::loadFromCpu` vs `VulkanBufferProxy::loadFromCpu` / `updateBufferObjectCommon`）、`VulkanStagePool`/`VulkanDisposer`/`VulkanCmdFence`、`FRenderer::endFrame`/`SurfaceRenderer::drop`；同时检查 `FEngine::execute`、`FRenderer::renderInternal`、FrameGraph/CommandStream 调度是否改变
  - **需要量化的源码问题**：每帧 command 数量与 flush 次数；每次 submit 的 command buffer/fence 数量；buffer upload 次数/字节数与 staging pool 回收次数；descriptor 更新/commit 次数；SurfaceRenderer drop 是否引发重复 endFrame、syncobj/fd 销毁；1.74 是否增加 FrameGraph pass、resource transition 或 Vulkan fence wait
  - **优化候选（调查后决定，不预设结论）**：合并/延迟 command flush；减少每帧 staging buffer 与 descriptor 更新；复用 command buffer/fence/syncobj；避免 SurfaceRenderer 每帧 drop 触发重型 endFrame；改 pipeline/cache key 使用兼容性标识而非 Vulkan handle 身份（仅针对 1.38 awaken2 病理，需验证 Vulkan 正确性）；任何优化必须保持 GPU/FPS 不退化
  - **验证要求**：源码静态 diff + `simpleperf` 函数/调用次数证据；必要时 Perfetto 帧级 trace；优化前后 SS4 `broadcast_error` 60s 对比（CPU、XFW:Main、FPS、GPU、RSS）以及 awaken2 对照（确认不重新引入 1.38 管线编译）；优化代码/补丁在对应 Filament 版本目录或明确记录为应用侧 workaround
  - **现有产物**：`/tmp/xperf/com.lixiang.facedemo/` 下有采样 CSV/图表；当前会话中 simpleperf 报告与 `.data` 已从设备清理，若需复盘应重新采集；APK/原始附件留在 hppc `/tmp/facedemo/`
  - **交接纪律**：先读两个版本源码的 git 状态、commit/tag 和相关文件，再做 diff；不要直接修改 hppc 源码；优化前必须复制/记录基线；children 百分比不得跨层相加；`broadcast_error` 的语音 binder/ART GC 与 XFW:Main backend 开销分开统计

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
