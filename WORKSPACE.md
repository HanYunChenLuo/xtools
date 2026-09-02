# WORKSPACE.md — xtools 工作区待办与状态

> 本文件记录跨会话的待办事项（backlog）。每次会话的历史总结见 `SESSION.md`。
> 完成一项就把状态改为 ✅ 并注明完成的 commit；新增想法随时追加。
> 最后更新：2026-09-01

## 当前状态速览

- 采样架构：**设备端 agent（xperf-agent）为 CLI/GUI 唯一采样路径**，无 adb 轮询
- 指标覆盖：CPU（单核口径，与 adb top 一致）、内存（dumpsys meminfo / smaps_rollup）、FPS（SurfaceFlinger 图层，多图层分色）
- 测试：75 全绿，clippy 零警告
- 设备：车机 6eb792dfb0f（adbd 已 root），测试包 `com.lixiang.car.x.svm`

---

## A. 已知缺陷（优先级最高）

- [ ] **agent 低间隔 + FPS 互相拖累**：50ms 间隔下每轮跑 dumpsys SurfaceFlinger，实测约半数轮次 overrun。FPS 应在 agent 内限频（每 N 轮采一次，≥500ms 节奏，与 CPU/内存解耦）。
- [ ] **数据只在退出时落盘**：长测中途崩溃丢全部数据。应边采边追加写 CSV（或按 N 分钟滚动落盘）。
- [ ] **时序数据无上限**：`CpuTimeSeriesData`（及 agent 模式的 memory 时序）长测持续膨胀。需要容量上限或流式落盘后释放内存。
- [ ] **断连即终止**：agent EOF 直接退出，无 ADB 重连恢复。
- [ ] **GUI 能力缺口**：无线程视图（CLI 有 --thread）、无峰值面板、无数据导出、600 点窗口（约 10 分钟）无法回看历史。
- [ ] **死代码**：`LOG_FILE_PATH` 从未初始化，`append_to_log` 永远静默失败——要么接通要么删除。

## B. 指标覆盖扩展（root 设备上都很便宜）

- [ ] **CPU 频率**（`scaling_cur_freq`）：单核 15% 在 0.8GHz 和 2.5GHz 含义完全不同，是性能结论的必要上下文。
- [ ] 温度/热降频（thermal zones）。
- [ ] GPU 使用率（`/sys/class/kgsl/kgsl-3d0/gpubusy` 等 sysfs）。
- [ ] IO/网络（`/proc/<pid>/io`、`/proc/<pid>/net/dev`）。

## C. 验证能力（"监控"→"验证"的差距）

- [ ] **perfetto `--trace N` 深挖模式**：已端到端验证可行（设备 perfetto v15.0，trace 输出须写 `/data/misc/perfetto-traces/`，su 域写 `/data/local/tmp` 被 SELinux 拒；主机解析用 trace_processor 已验证可用）。注意：此车机 frametimeline 事件无 layer/进程归属（全 NULL），只能与 agent 图层方案互补，不能替代。
- [ ] simpleperf 调用栈采样（回答"CPU 高在哪个函数"）。
- [ ] 阈值告警 / 基线对比 / 退出时输出达标结论（验证报告）。
- [ ] 时间轴事件标记（如"开始倒车"打点，对齐指标变化）。
- [ ] 冷启动时间（`am start -W` / reportFullyDrawn）。

## D. 待决策

- [ ] xperf-core 的轮询参考实现（`Sampler`/`cpu`/`memory`/`fps`，约 1500 行含完整单测）已无二进制调用——**删除还是保留**作兜底参考？

---

## 已完成（本次迁移后）

- ✅ CPU 单核口径（×核数），与 adb top 对齐（0ff3432）
- ✅ FPS 采集：SurfaceFlinger 图层方案，多图层分色折线，GUI 接入（b551ae7、8c098e6、35f5f40）
- ✅ agent 设备端采样统一架构（f6fec7e、50627d6）
- ✅ 采样日志单行摘要（c7c6fe8）
