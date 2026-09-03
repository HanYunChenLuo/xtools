# WORKSPACE.md — xtools 工作区待办与状态

> 本文件记录跨会话的待办事项（backlog）。每次会话的历史总结见 `SESSION.md`。
> 完成一项就把状态改为 ✅ 并注明完成的 commit；新增想法随时追加。
> 最后更新：2026-09-03

## 当前状态速览

- 采样架构：**设备端 agent（xperf-agent，src/ 布局）为 CLI/GUI 唯一采样路径**；断连自动重连恢复
- 指标覆盖（9 项全实现）：CPU（单核口径）、内存、FPS、CPU 频率、温度/热降频、GPU、IO、网络、GPU 显存
- **平台抽象**：`xperf-core/src/platform/` trait + adb devices -l 自动检测（SS2MAX/SS2PRO/SS3/SS4/Android）；agent 加 `--platform`/`--qnx-host` 参数
- **GPU 五通道**（detect_gpu_path_ex 按平台选路）：kgsl sysfs（Android/SS2）/ QNX telnet（SS3，真 busy%/util%/频率+每进程）/ topgpu（SS2MAX）/ ligfxprofilerd logcat（SS4）/ dumpsys gpu 显存保底；全部补采 dumpsys gpu 显存
- **C 类验证能力**：阈值告警（--threshold，静止界面不误报）+ 退出验证报告 + 冷启动（--cold-start）+ 时间轴打点（Unix socket / GUI 按钮 + 图表竖线 + markers.csv）
- 落盘：CLI 流式 CSV（csv_escape 转义）+ 退出图表；GUI 完整历史 + CSV 导出
- GUI：9 张折线图 + 实时数值面板 + Top 线程 + 峰值 + 间隔档位下拉 + 实际周期标注 + 勾选即时生效（自动重启会话）+ 打点竖线
- agent 部署：自动尝试 adb root（IO 等需 root）；源码 mtime 变化自动重建
- 测试：**60 全绿**（死代码删除后从 107 精简），clippy 零警告
- 设备：SS3 6eb792dfb0f（adbd root，QNX GPU 已验证）；SS2MAX d1f39648c1f（adb root 可用，IO/kgsl/44 温度传感器已验证）

---

## A. 已知缺陷

（无——两轮 code review 的严重/一般问题已全部修复）

## B. 指标覆盖

- [x] 全部完成（b732d55 + 5cff92e + 58ceae5）

## C. 验证能力

- [x] ~~阈值告警 / 退出验证报告~~（a56a6f5 + 02ee9bc 静止误报修复）
- [x] ~~时间轴事件标记~~（0f7ad9d：CLI Unix socket + GUI 按钮 + 图表竖线）
- [x] ~~冷启动时间~~（a56a6f5：am start -W）
- [ ] **perfetto `--trace N` 深挖模式**：已端到端验证可行（perfetto v15.0，trace 须写 `/data/misc/perfetto-traces/`；此车机 frametimeline 无 layer 归属，与 agent 图层方案互补）
- [ ] simpleperf 调用栈采样（回答"CPU 高在哪个函数"）
- [ ] 基线对比（两次运行 diff）

## D. 结构改进（下轮候补）

- [ ] **agent 单文件拆分**：src/main.rs 仍 1848 行单文件，src 布局已就位（737ed45），待拆 `proc.rs`（/proc 读取）/ `fps.rs`（SurfaceFlinger）/ `mem.rs`（meminfo）/ `gpu/mod.rs`（五通道，读线程骨架三份重复可抽公共 spawn_stream_parser）。**拆完必须真机全指标回归**
- [x] ~~xperf-core 轮询参考实现删除~~（225d89b，-1653 行；保留 ThreadCpuInfo/MemoryDetails/FpsTimeSeriesData/PidStats/SampleEvent 等协议类型）

## E. 已知遗留（评估过，低风险不阻塞）

- SS2MAX GPU 显存无数据源（dumpsys gpu 无 Memory snapshot 段 + debugfs 不存在，平台限制）
- SS4 ligfx Frequency 单位待真机核实（Hz vs MHz）
- 多设备连接时所有 adb 命令不带 -s 会失败（单设备场景无影响）
- GUI add_marker 不写 markers.csv / 不进 export_csv（关窗丢失，仅图表竖线）
- marker 每连接线程无界（有 10s 读超时兜底）

---

## 已完成

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
