# WORKSPACE.md — xtools 工作区待办与状态

> 本文件记录跨会话的待办事项（backlog）。每次会话的历史总结见 `SESSION.md`。
> 完成一项就把状态改为 ✅ 并注明完成的 commit；新增想法随时追加。
> 最后更新：2026-09-04 下午二（基线对比 C-6 收官 + 多设备 adb -s 落地，46bd161，见 SESSION 当日条目）

## 当前状态速览

- 采样架构：**设备端 agent（xperf-agent）为 CLI/GUI 唯一采样路径**；断连自动重连恢复；模块化布局（main/proc/mem/fps/thermal + gpu/ 五通道）
- 指标覆盖（9 项全实现）：CPU（单核口径）、内存、FPS、CPU 频率、温度/热降频、GPU、IO、网络、GPU 显存
- **多设备 adb**：全局 `-s` 注入（utils 全部 adb 调用点收敛）；CLI `--device`/GUI 设备下拉（**热插拔动态检测**：3s 轮询 diff + devices-changed 事件，951c0ab）；单台自动、多台报错列清单（详见 CLAUDE.md「多设备 adb」）
- **平台抽象**：`xperf-core/src/platform/` trait + adb devices -l 自动检测（SS2MAX/SS2PRO/SS3/SS4/Android）；agent 加 `--platform`/`--qnx-host` 参数
- **GPU 五通道**（detect_gpu_path_ex 按平台选路）：kgsl sysfs（Android/SS2）/ QNX telnet（SS3，真 busy%/util%/频率+每进程）/ topgpu（SS2MAX）/ ligfxprofilerd logcat（SS4）/ dumpsys gpu 显存保底；全部补采 dumpsys gpu 显存
- **C 类验证能力**：阈值告警（--threshold，静止界面不误报）+ 退出验证报告 + 冷启动（--cold-start）+ 时间轴打点（Unix socket / GUI 按钮 + 图表竖线 + markers.csv）+ **simpleperf 函数热点**（--stack N：调用栈录制 + 线程/self/children 三视图报告，CLI 独立/并行两模式 + GUI 独立 tab）+ **基线对比**（--save-baseline/--compare-baseline：两次运行 diff 回归判定，CLI/GUI 共用，详见 CLAUDE.md「基线对比模式」）
- 落盘：数据根 `/tmp/xperf`（CLI 流式 CSV + 退出图表；GUI 完整历史 + CSV 导出，共用同根）；清理走 CLI `--clean-cache` / GUI 按钮（~/.cache/xperf + /tmp/xperf，2bd6bca）
- GUI：9 张折线图 + 实时数值面板 + Top 线程 + 峰值 + 间隔档位下拉 + 实际周期标注 + 勾选即时生效（自动重启会话）+ 打点竖线 + Perfetto 分析（独立 tab 报告 + 浏览器自动加载 + 每秒录制进度）+ **函数热点**（独立 tab + 每秒录制进度 + 浏览器火焰图：report_html.py 渲染 `.data` 为单文件 HTML 后打开，脚本集自动引导缓存）+ 暗/亮双主题（CSS 变量 + color-scheme；checkbox 自绘兼容 webkit2gtk；localStorage 持久化）
- agent 部署：自动尝试 adb root（IO 等需 root）；src 树内任一 .rs mtime 变化自动重建
- 测试：**100 全绿**（agent 24 个：解析 + watchdog_step 决策；core 62+2 ignored：协议/trace/simpleperf/baseline/设备 diff——baseline 14 个 + platform 多设备过滤 1 + utils 设备解析/diff 3；xperformance 8：alerts/coldstart；GUI 4：export_csv×2 + 基线命令级/汇总 2），clippy 零警告，**cargo doc 零 warning**（4 crate missing_docs 归零；xperf-core `#![warn(missing_docs)]` 常开）
- 设备：SS3 6eb792dfb0f（adbd root，QNX GPU 通道已真机回归，见 D-2；多设备 `-s` 真机回归 46bd161）；Redmi 手机 1280da60（标准 Android 平台，`-s` 路由验证通过）；SS2MAX d1f39648c1f（adb root 可用，IO/kgsl/44 温度传感器已验证；gpubusy 计数器停走属数据源限制）
- **测试对象（555ffab 起统一）**：`example/apk/filament-gltf-viewer-v1.76.0-android.apk`（git-lfs 管理，包名 `com.google.android.filament.gltf`，入口 `.MainActivity`）——真机测试一律用它，不再用 svm（车机应用场景不可控且与业务耦合）。已装 SS3 + SS2MAX。SS3 冒烟基线：CPU 均值 ~40%/峰 56%（JobSystem/AdrenoOsLib 渲染线程可见）、PSS ~650MB/峰 736MB、GPU busy ~25%（QNX 进程归因 11.7%）；静止场景 Filament 按需渲染不重绘 → FPS 如实为 0

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

- [x] ~~agent 单文件拆分~~（531798a + 99d1b74 review 修复）：main.rs 1848 行 → 10 文件（main 493 + proc/mem/fps/thermal + gpu/{mod,kgsl,qnx,topgpu,ligfx}），三份读线程骨架抽公共 `gpu::spawn_stream_parser`，四段相同的 gpumem 补采臂合并；测试 23 个随模块迁移全绿。真机回归：SS2MAX 新旧 agent 同机对比事件分布/wire 格式/smaps 值一致。附带修复 host 侧 `ensure_agent_built` 只盯 main.rs 的 mtime 检查（改扫 src 树，touch 子模块已验证触发重建）
- [x] ~~SS3 QNX 通道真机回归 + kgsl 统计链停滞修复~~（2026-09-03）：回归发现 QNX frame 流"1 条后停走、跨会话交替通/停"。黑盒实验（重启车机前后共 10+ 组对照）定位根因：**kgsl 统计链是驱动全局的，会话/fd 关闭都不清理**（泄漏直到整机重启）；`echo >` 式即开即死连接写入撞存量链只 flush 一窗即停，长活连接（`exec 3>`）写入则全链重相位持续输出；多链锁步产生重复行。修复（qnx.rs）：① 启动命令改 `exec 3>` 持 fd 写入；② slog 行按"与上一行完全相同"去重（Sys/Proc 各一条）；③ 看门狗兜底（frame 静默超 3×周期经 fd3 重写自愈，≤3 次）。真机验证：4 条泄漏链硬场景下启动顿 ~5s + 1 次自愈后稳定 1/s；gpu/gpuproc/gpumem 三类事件与 CSV 全通（eid→pid 9671 归因正确）；连续多轮 kill/重跑稳定。已知残留：存量链的窗口 flush 会带来少量同值重复样本（数值正确，重启清零）
- [x] ~~xperf-core 轮询参考实现删除~~（225d89b，-1653 行；保留 ThreadCpuInfo/MemoryDetails/FpsTimeSeriesData/PidStats/SampleEvent 等协议类型）

## E. 已知遗留（评估过，低风险不阻塞）

- SS2MAX GPU 显存无数据源（dumpsys gpu 无 Memory snapshot 段 + debugfs 不存在，平台限制）
- SS3 kgsl 统计链（见 D-2/CLAUDE.md）：三层清理已落地（agent 退出钩子 + setsid + host 条件兜底，**均带 pgrep 多会话并发保护**），SIGINT/Ctrl-C/正常退出路径真机验证停链成功、下一会话零自愈即起流；残余风险仅 agent 被 SIGKILL 暴杀（无钩子机会）与 reboot 后首会话（开机 5000ms 链在流，走一次看门狗自愈 ~8s）
- SS2MAX gpubusy 计数器恒 `0 0`（2026-09-03 实测 30 次采样全零，total_time 停走 → kgsl busy 通道无事件；gpuclk 正常 427MHz）——数据源限制，新旧 agent 行为一致
- SS4 ligfx Frequency 单位待真机核实（Hz vs MHz）
- ~~多设备连接时所有 adb 命令不带 -s 会失败~~（**46bd161 已修**：全局 `-s` 注入 + CLI `--device` + GUI 设备下拉，SS3+手机双连真机回归；原候补转正，详见 CLAUDE.md「多设备 adb」）。GUI 多台未指定 `--device` 的自动启动跳过路径为逻辑验证 + 单测覆盖（验证时手机恰断开未双机复现，行为由 pick_device 单测锁定）
- QNX 双会话并发交互（五轮 review 实测）：①后启动会话的 fd3 写入给先启动方一次 ~7s GPU 停走（看门狗自愈恢复）；②各方 GPU 事件密度升至 ~2×（双方写入产生非锁步多链，行级全等去重不覆盖，值为真值仅密度偏高）；③退出清理已有并发保护（pgrep 检测其他 agent 跳过停链，agent 钩子 >1 / host 兜底 ≥1+收尸等待，真机验证）——并发监控本身罕见，记录不修
- GUI add_marker 不写 markers.csv / 不进 export_csv（关窗丢失，仅图表竖线）
- marker 每连接线程无界（有 10s 读超时兜底）
- GUI 基线按钮/设备下拉的点击渲染为人工目验项（后端链路由命令级测试锁定：save/compare 端到端 + build_summary 口径）

---

## 已完成

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
