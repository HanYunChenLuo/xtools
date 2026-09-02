# WORKSPACE.md — xtools 工作区待办与状态

> 本文件记录跨会话的待办事项（backlog）。每次会话的历史总结见 `SESSION.md`。
> 完成一项就把状态改为 ✅ 并注明完成的 commit；新增想法随时追加。
> 最后更新：2026-09-02

## 当前状态速览

- 采样架构：**设备端 agent（xperf-agent）为 CLI/GUI 唯一采样路径**，无 adb 轮询；断连自动重连恢复（EOF 后轮询等设备回来，状态保留）
- 指标覆盖：CPU（单核口径，与 adb top 一致）、内存（dumpsys meminfo / smaps_rollup）、FPS（SurfaceFlinger 图层，多图层分色，agent 内限频 ≥500ms 与 CPU/内存解耦）、**CPU 频率（每核 scaling_cur_freq）、温度/热降频（dumpsys thermalservice，≥2s 限频）、GPU（kgsl gpubusy，无 sysfs 平台自动降级）、IO（/proc/<pid>/io 逻辑+磁盘读写 KB/s）、网络（整机 /proc/net/dev 物理口聚合 KB/s）**
- 落盘：CLI 边采边流式写 CSV（逐行 flush，崩溃只丢尾部）；内存时序抽稀上限 2×30k 点，只服务退出图表；GUI 保留完整会话历史，可导出 CSV
- GUI：CPU/内存/FPS/频率/温度/GPU/IO/网络 折线（窗口跟随/全览）、Top 线程表、峰值面板、CSV 导出
- 测试：87 全绿（含并行），clippy 零警告
- 设备：车机 6eb792dfb0f（adbd 已 root），测试包 `com.lixiang.car.x.svm`
- **本机限制**：GPU 在 hypervisor 后无 kgsl sysfs（--gpu 自动禁用并提示）；thermalservice 是 test HAL 假数据（30.8°C 恒定）——两路代码按标准接口实现，真手机有效

---

## A. 已知缺陷（优先级最高）

（无——A 类已全部清零）

## B. 指标覆盖扩展（root 设备上都很便宜）

- [x] ~~**CPU 频率**（`scaling_cur_freq`）~~（b732d55）
- [x] ~~温度/热降频（thermal zones）~~（b732d55，实际走 dumpsys thermalservice——本机无 /sys/class/thermal）
- [x] ~~GPU 使用率~~（b732d55，kgsl gpubusy；本机 GPU 在 hypervisor 后无源，自动降级）
- [x] ~~IO/网络~~（b732d55，/proc/<pid>/io 每 PID；网络为整机口径 /proc/net/dev——per-app 无数据源）

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

- ✅ B 类指标全覆盖：CPU 频率/温度热降频/GPU/IO/网络（agent+CLI+GUI，MetricFlags 重构）（b732d55）
- ✅ CPU 单核口径（×核数），与 adb top 对齐（0ff3432）
- ✅ FPS 采集：SurfaceFlinger 图层方案，多图层分色折线，GUI 接入（b551ae7、8c098e6、35f5f40）
- ✅ agent 设备端采样统一架构（f6fec7e、50627d6）
- ✅ 采样日志单行摘要（c7c6fe8）
- ✅ agent FPS 限频解耦 ≥500ms + 启动预热 + 空发现节流重试，50ms 同采 0 overrun（93f6439）
- ✅ 并行测试数据竞争修复：ADB_TEST_LOCK 串行锁（df61bc4）
- ✅ CLI 边采边流式落盘 CSV + 时序抽稀上限（02c18bd）
- ✅ 删除 LOG_FILE_PATH 死代码（cd9769b）
- ✅ agent 断连自动重连恢复，CLI+GUI（1077e97）
- ✅ GUI 历史回看 + 峰值面板 + Top 线程 + CSV 导出（3e388e3）
