# SS4（SA8797P）指标适配设计（H 节交接）

> **目标**：SS4 真机九项指标逐项落地 + C 类回归 + 文档收尾。
> **状态**：交接稿 v1.0（2026-09-10）。桥接（adb 自动桥接）已完成并合 main
> （`docs/DESIGN-ss4-adb.md` v1.2 + CLAUDE.md「SS4 adb 自动桥接」），本文是
> 下一步（指标层）的实施依据。**全部关键数据源已真机勘察完毕，结论均为实测，
> 新会话按任务 A/B/C/D 顺序实施即可，勿重复勘察**。
> **设备**：SS4 接 hppc（MindRT `42087266b1f`，Android GVM 桥接为
> `localhost:5559`，Android 16，12 核）；测试一律经 `--remote hppc`。

---

## 0. 已确证事实（勿重复勘察）

| 项 | 结论（实测） |
|---|---|
| 桥接 | `forward tcp:5559 tcp:5557` + `connect localhost:5559` 全自动（bridge.rs 已合入）；GVM 重启 ~24s 零干预自愈；MindRT adbd 重启清 forward 规则（refresh 重建） |
| root | 标准 adb `adb -s localhost:5559 root` 直连成功（主路径）；MindRT `rootandroid.sh` 兜底（`/system_ext/bin/`） |
| FPS `--latency` | **全图层失效**（QCM SDE 构建阉割，getprop 无可恢复开关）；agent 图层发现正常（A16 `RequestedLayerState{…}` 包装 1f74a73 已修）但缓冲恒空不发事件 |
| FPS frametimeline | **唯一可行源**：perfetto `android.surfaceflinger.frametimeline` 单数据源 ~9.5KB/s，595 帧/10s；与全配置 --trace 并发无冲突（双会话同窗口双完整） |
| GPU ligfx | ligfxprofilerd 在 **MindRT 侧**（pid 常驻，`/usr/bin/ligfxprofilerd`），**GVM logcat 无任何输出** → agent `gpu/ligfx.rs`（读 GVM logcat）永久不可用，须改 host 侧通道 |
| GPU 显存 | `dumpsys gpu` Memory snapshot **可用**（gltf 734.9MB/整机 4819MB）——保底通道已自动生效 |
| GPU 频率 | ligfx `Frequency: 1000 Hz` 恒值（空闲/负载不变；GPU VFIO 直通 GVM，MindRT/GVM 均无 kgsl/devfreq 节点可对照）——单位与语义待任务 B 核实 |
| CPU 频率 | hello `maxkhz` 全 0（12 核）——`scaling_max_freq` 读取失败，待任务 C 排查（可能路径不同或需 root 场景复核） |
| C 类 | perfetto `--trace` ✅（303 帧/5s 全报告）；冷启动 ✅（am start -W COLD 310ms）；simpleperf 待测（gltf 非 debuggable，root 下 --app 应可采） |
| 测试 APK | gltf viewer 反复崩溃 = **RemoteServer 8082 端口冲突**（onCreate 绑 ws 调试服务器，失败即 FATAL；SS4 Application Error 保进程形成崩溃循环）——**测前清场：`--force-stop`（c5d2cc2 已工具化，CLI/GUI 双入口）**；建议重建 APK 去 RemoteServer |

---

## 任务 A：FPS——frametimeline-only perfetto 流式化（SS4 专属兜底）

**选路**：`platform == Ss4` 时启用本通道；其余平台维持 agent `--latency` 路径零改动
（SS3 60.8 / SS2MAX 60.5 已回归）。预留泛化：agent「N 轮缓冲恒空」发 err 后
host 激活同一兜底（选配，首版不做）。

**已验证配置**（注意数据源名，写错被静默忽略）：

```pbtxt
buffers { size_kb: 1024 fill_policy: RING_BUFFER }
data_sources { config { name: "android.surfaceflinger.frametimeline" } }
duration_ms: <窗口>
write_into_file: true
file_write_period_ms: 1000
flush_period_ms: 1000
```

- 配置文件须推 `/data/misc/perfetto-configs/`（`/data/local/tmp` perfetto 无权限读，errno 13）；trace 落 `/data/misc/perfetto-traces/`（设备属 root，pull 无障碍）
- **开销实测**：10s = 94.5KB（~9.5KB/s，全配置 4.6MB/s 的 ~1/500）；FrameTimelineManager 常驻开销≈0；**与全配置 --trace 同窗口并发双完整**（A 916 帧/15s、B 609 帧/10s）——深挖录制时无需停 FPS
- **解析**：trace_processor SQL `actual_frame_timeline_slice`（`layer_name`/`dur`/`ts`）
- **归因粒度（实测 A/B 对照，勿踩坑）**：per-layer `TX - <层名>` 行**只覆盖非 BLAST 系统窗**（StatusBar/HUD/extra_window）；**BLAST 层（SurfaceView 直渲应用，含 gltf）帧折叠进 `layer_name IS NULL` 的 display 级合成流**（vsync 合并：gltf 动画时 359/6s≈60fps，杀掉后 122/6s≈20fps 系统底噪）⇒ **事件语义 = display 合成 FPS ≈ 被测应用 FPS（单动画源场景）**，UI/CLI 须按平台标注口径，不做 per-PID 归因
- **形态建议**（实施时定）：短窗循环（如 5s 窗）顺序「录 → pull → trace_processor 解析 → 合成 `SampleEvent::FpsUpdate`（layer 标 `(display)` 或 NULL 语义标注）→ 汇入 CLI/GUI 事件流」；jank 可由 NULL 流帧间隔/dur 复用现有阈值口径；与采样会话生命周期绑定（开始/停止/断连重连随 spawn_agent 会话）
- **防gap**：窗口边界帧去重按 ts 水印；录制间隙（pull+解析 ~1-2s）如实标注窗口覆盖率

## 任务 B：GPU——ligfx host 侧通道（经网关读 MindRT logcat）

- **行源**：`adb -s <mindrt> shell logcat -s ligfxprofilerd`（经 bridge 拿网关 serial：
  `bridge::gateway_for_android(localhost:5559)`；流式长连接）。**~5s 一个帧块**
  （`sampling_interval_ms=5000`；属性 `persist.vendor.ligfxprofiler.sampling_interval_ms`
  可调小提实时性——实测该属性存在，改后效果待验证）
- **行格式（真机样例）**：
  ```
  [GPU0] Frame 149556: Frequency: 1000 Hz, Tasks: 3 total, GSL Timestamp: 748015951, Global: Busy=29.28%, Queued=20.24%, Utilization=29.28%
  [GPU0]   GVM_d.filament.gltf-1572152: Busy=8.39%, Queued=5.98%, Utilization=8.39%
  ```
  agent `gpu/ligfx.rs::parse_line` 已能解析此格式——**解析逻辑移植 core**（agent crate
  私有；core 新增解析+单测，样例行即上两条）；**业务侧只看 Utilization**（平台文档口径）
- **归因**：`GVM_<comm 15字符截断>-<会话id>`（id 非 GVM pid、跨重启变化）——按 comm
  截断匹配 GVM pid（与 agent `lookup_pid` 同语义）
- **Frequency 单位核实**（E 节遗留）：恒 `1000 Hz`（空闲/负载不变）。`persist.vendor.ligfxprofiler.sampling_interval_ms` 调小后观察是否随负载变档；仍恒定则按「MHz 标注错误或定频占位」如实标注并核销 E 节条目
- **事件映射**：Sys 行 → `GpuUpdate{busy, util, mhz, maxmhz:0}`（maxmhz 未知）；Proc 行 → `GpuProcUpdate`；显存保底（dumpsys gpu）继续由 agent 补采
- **agent ligfx.rs 处置**：SS4 上 `available()` 恒 false（GVM logcat 无输出），当前落到 kgsl→失败→dumpsys 保底——host 通道上线后 agent 侧 ligfx 探测保留无害（永不命中），或按「退役或保留为探测」评估（设计 §8 R8 fallback 原话）；`gpu/mod.rs:66` 注释链同步更新
- **与 QNX 通道互斥语义对齐**：流式 GPU 通道全局独占（GPU_STREAM_BUSY）——host 侧 ligfx 同样须纳入该互斥（多 host 并行会话时后到访 err 禁用）

## 任务 C：九项指标逐项实测（root / 非 root 两态，对齐 G 节矩阵格式）

| 指标 | 预期路径 | 待核项 |
|---|---|---|
| CPU/线程 | /proc（root 已验证，500ms 样本+线程明细全通 ✅） | 非 root 复核（G 节矩阵预期可用） |
| 内存 | smaps_rollup（root）/ dumpsys meminfo 降级 | 低间隔降级路径复核；RSS 兜底 |
| FPS | 任务 A 通道 | A 完成后验收（NULL 流口径） |
| 频率 | scaling_cur_freq | **hello maxkhz 全 0 待查**（scaling_max_freq 读不到：路径差异/权限/GVM cpufreq 配置） |
| 温度 | thermalservice / sysfs zones | 数据源探测（是否有真 sensors，还是 SS3 同款 test HAL 假数据） |
| IO | /proc/pid/io（root） | root 验证；非 root 预期禁用（G 节矩阵） |
| 网络 | /proc/net/dev | 复核（含 GVM 虚拟网卡形态，排除规则是否误伤） |
| GPU | 任务 B 通道 + 显存保底 ✅ | B 完成后验收 |
| 显存 | dumpsys gpu ✅（已验证可用） | 无 |

## 任务 D：C 类回归

- perfetto `--trace` ✅（已验证 303 帧/5s 全报告，含 frametimeline 段）
- simpleperf `--stack`：gltf 非 debuggable → root 下 `--app` 预期可采（对齐 SS3 结论），实测确认；非 root 预期拒（如实提示已有）
- 冷启动 ✅（COLD 310ms）；**注意 gltf RemoteServer 崩溃坑——测前 `--force-stop` 清场**
- 基线对比：保存→对比全链路一次（口径同 SS3 基线流程）

## 任务 E：文档收尾

- `platform/ss4.rs` 桩补实（gpu_hint/description/数据源细节——ligfx host 通道 +
  显存保底 + FPS frametimeline 兜底 + 频率 maxkhz 结论）
- CLAUDE.md：平台抽象段补 SS4 特性（对齐 SS2MAX 特性格式）；FPS 采样流程节与
  B 类指标 GPU 表补 SS4 行（frametimeline 兜底 / ligfx host 通道）
- WORKSPACE：E 节遗留核销（ligfx Frequency 单位、SS4 FPS 条目状态更新）、
  H 节逐项勾选；SESSION 当日条目（真机基线数据入 SESSION）

---

## 环境备忘（交接时设备状态）

- MindRT 已 root（`adb root`）、GVM 已 root（rootandroid.sh）；gltf viewer 已装 SS4
- **gltf 崩溃循环的规避**：测前 `--force-stop` 清场（CLI `xperformance --remote hppc --device localhost:5559 --package com.google.android.filament.gltf --force-stop` 或 GUI「停止应用」按钮）；长期建议重建测试 APK 去掉 RemoteServer（filament 仓库 MainActivity.kt:154，try-catch 或删除即可）
- SS4 掉线自恢复：HU 休眠/插拔后 bridge 自动重桥接（已实测）；MindRT 从 adb 消失时即 HU 休眠/断开
