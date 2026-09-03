# WORKSPACE.md — xtools 工作区待办与状态

> 本文件记录跨会话的待办事项（backlog）。每次会话的历史总结见 `SESSION.md`。
> 完成一项就把状态改为 ✅ 并注明完成的 commit；新增想法随时追加。
> 最后更新：2026-09-03

## 当前状态速览

- 采样架构：**设备端 agent（xperf-agent）为 CLI/GUI 唯一采样路径**；断连自动重连恢复；模块化布局（main/proc/mem/fps/thermal + gpu/ 五通道）
- 指标覆盖（9 项全实现）：CPU（单核口径）、内存、FPS、CPU 频率、温度/热降频、GPU、IO、网络、GPU 显存
- **平台抽象**：`xperf-core/src/platform/` trait + adb devices -l 自动检测（SS2MAX/SS2PRO/SS3/SS4/Android）；agent 加 `--platform`/`--qnx-host` 参数
- **GPU 五通道**（detect_gpu_path_ex 按平台选路）：kgsl sysfs（Android/SS2）/ QNX telnet（SS3，真 busy%/util%/频率+每进程）/ topgpu（SS2MAX）/ ligfxprofilerd logcat（SS4）/ dumpsys gpu 显存保底；全部补采 dumpsys gpu 显存
- **C 类验证能力**：阈值告警（--threshold，静止界面不误报）+ 退出验证报告 + 冷启动（--cold-start）+ 时间轴打点（Unix socket / GUI 按钮 + 图表竖线 + markers.csv）
- 落盘：CLI 流式 CSV（csv_escape 转义）+ 退出图表；GUI 完整历史 + CSV 导出
- GUI：9 张折线图 + 实时数值面板 + Top 线程 + 峰值 + 间隔档位下拉 + 实际周期标注 + 勾选即时生效（自动重启会话）+ 打点竖线
- agent 部署：自动尝试 adb root（IO 等需 root）；src 树内任一 .rs mtime 变化自动重建
- 测试：**60 全绿**（agent 23 个测试随模块迁移），clippy 零警告
- 设备：SS3 6eb792dfb0f（adbd root，QNX GPU 已验证；拆分后 QNX 通道待复验）；SS2MAX d1f39648c1f（adb root 可用，IO/kgsl/44 温度传感器已验证；gpubusy 计数器停走属数据源限制）

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

- [x] ~~agent 单文件拆分~~：main.rs 1848 行 → 10 文件（main 490 + proc/mem/fps/thermal + gpu/{mod,kgsl,qnx,topgpu,ligfx}），三份读线程骨架抽公共 `gpu::spawn_stream_parser`，四段相同的 gpumem 补采臂合并；测试 23 个随模块迁移全绿。真机回归：SS2MAX 新旧 agent 同机对比事件分布/wire 格式/smaps 值一致。附带修复 host 侧 `ensure_agent_built` 只盯 main.rs 的 mtime 检查（改扫 src 树，touch 子模块已验证触发重建）。**SS3 QNX 通道拆分后未真机回归**（设备不在线），接入时补验
- [x] ~~xperf-core 轮询参考实现删除~~（225d89b，-1653 行；保留 ThreadCpuInfo/MemoryDetails/FpsTimeSeriesData/PidStats/SampleEvent 等协议类型）

## E. 已知遗留（评估过，低风险不阻塞）

- SS2MAX GPU 显存无数据源（dumpsys gpu 无 Memory snapshot 段 + debugfs 不存在，平台限制）
- SS2MAX gpubusy 计数器恒 `0 0`（2026-09-03 实测 30 次采样全零，total_time 停走 → kgsl busy 通道无事件；gpuclk 正常 427MHz）——数据源限制，新旧 agent 行为一致
- SS3 QNX 流式通道（gpu/qnx.rs）拆分后未真机回归——解析函数有单测、spawn_stream_parser 逻辑与旧内联线程逐行对齐，SS3 设备接入时跑一次 `--gpu --platform ss3` 确认
- SS4 ligfx Frequency 单位待真机核实（Hz vs MHz）
- 多设备连接时所有 adb 命令不带 -s 会失败（单设备场景无影响）
- GUI add_marker 不写 markers.csv / 不进 export_csv（关窗丢失，仅图表竖线）
- marker 每连接线程无界（有 10s 读超时兜底）

---

## 已完成

- ✅ agent 模块化拆分：1848 行单文件 → 10 文件（proc/mem/fps/thermal/gpu 五通道），公共 spawn_stream_parser，host 侧 mtime 检查同步修复（本轮）
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
