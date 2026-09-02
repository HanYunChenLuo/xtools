# SESSION.md — 会话历史

> **约定**：每个会话结束前追加一条记录（最新在最上）。格式：
> 日期 / 任务目标 / 完成内容（commit 列表）/ 关键结论与基线 / 遗留问题。
> 待办事项（backlog）在 `WORKSPACE.md` 维护，本文件只做历史追溯。
> 新会话开始时可先读本文件了解近期上下文。

---

## 2026-09-02 — A 类缺陷全清：FPS 限频 / 流式落盘 / 内存上限 / 断连重连 / GUI 补齐

**任务线**：按 WORKSPACE.md A 类缺陷 1→2→3 推进，随后清掉剩余 3 项，A 类清零。

### 完成内容（commits 旧→新）

| commit | 内容 |
|--------|------|
| 93f6439 | agent FPS 限频解耦（≥500ms 周期）+ 启动预热 + 空发现节流重试 |
| df61bc4 | 并行测试数据竞争修复：ADB_TEST_LOCK 串行锁 |
| 02c18bd | CLI 边采边流式落盘 CSV + 时序抽稀上限（CHART_SERIES_CAP=30k） |
| cd9769b | 删除 LOG_FILE_PATH/append_to_log 死代码 |
| 1077e97 | agent 断连自动重连恢复（xperf-core reconnect_agent，CLI+GUI） |
| 3e388e3 | GUI 历史回看 + 峰值面板 + Top 线程视图 + CSV 导出 |

### 关键结论与基线

- **A1**：限频后 50ms 间隔 CPU+FPS 同采 10s **0 overrun**（修复前约半数轮次）。实测车机全量 `dumpsys SurfaceFlinger` 1.5s（图层发现）、`--latency` 每图层 ~10ms（稳态采样）、`--list` ~0ms；发现动作因此必须在节拍外预热或按阈值节流。顺带修复既有 bug：图层发现为空时 `zero_rounds` 不增长，FPS 会永久静默（进程重启时 Surface 未建即触发）。
- **测试竞争**：`ADB_RUNNER_OVERRIDE`/`MOCK_PHASE` 全局态在 cargo test 默认并行下互相覆盖，失败数 3-7 不等且非确定性；`--test-threads=1` 稳定全绿。修复后并行 5 轮 60/60。
- **A2**：CsvStream 逐行 flush，kill -9 实测崩溃前数据完整；运行中 CSV 即增长（6s 时 88 行 → 8s 时 128 行，20 行/s 与 50ms 间隔一致）。
- **A3**：内存时序超 2×30k 点每 2 取 1 抽稀（保时间范围、降分辨率），CSV 始终全量；MemoryTimeSeriesData 的 300 点丢头上限被抽稀替代（CLI 不再绕过 add_data_point）；`cpu_data.top_threads` 确认无读者后 CLI 不再写入。
- **A4 断连重连**：事件流 EOF/读错误 → `reconnect_agent` 每 500ms 轮询 `adb devices`，设备回来即重新部署+启动 agent，主机侧时序/峰值/流式 CSV 全部保留（CSV 继续追加同一时间戳目录）。实测 `pkill -f xperf-agent`（设备端死）与 `adb kill-server`（主机侧断）双路径均自动恢复；顺带覆盖 agent 崩溃场景。
- **A5 GUI**：图表 series 不再 600 点截断（完整会话历史），绘制按窗口裁剪+二分定位+stride 抽稀防卡顿；峰值面板/Top 线程表（500ms 节流渲染）；导出走 `export_csv` 命令写 `log/<pkg>/<导出时刻>/`。前端 JS 内存会话级无界（小时级约 10MB，可接受；无人值守长测是 CLI 的职责）。
- 测试 82 全绿（含并行、GUI 导出单测），clippy 零警告。

### 遗留问题

- A 类清零。B/C 类（CPU 频率/温度/GPU/IO 指标、perfetto/simpleperf/阈值告警等验证能力）与 D 类（xperf-core 轮询参考实现删留决策）见 WORKSPACE.md。


---

## 2026-09-01 — CPU 口径修复 + FPS 采集 + agent 统一采样架构

**任务线**：修复 GUI CPU 图表显示为 0 → 顺藤摸瓜完成三项大改动。

### 完成内容（commits 旧→新）

| commit | 内容 |
|--------|------|
| 0ff3432 | CPU% 改单核口径（×核数），与 adb top 一致 |
| c7c6fe8 | GUI 采样日志改单行摘要（原 Debug 全量每轮数千字符） |
| b551ae7 | 新增 FPS 采样（--fps），SurfaceFlinger 图层方案 |
| 8c098e6 | FPS 多图层逐层上报 + GUI 接入 FPS 图表 |
| 35f5f40 | GUI 图例色块 + 按文字宽度排布 |
| f6fec7e | 新增 agent 模式：设备端常驻采样器 |
| 50627d6 | 统一采样路径：CLI/GUI 全部走 agent，移除 adb 轮询 |
| 575a97e / 085f1e5 / 806a18f | 文档：CLAUDE.md、README 重写 |

### 关键结论与基线

- **GUI "CPU 显示为 0" 的根因**：不是 bug——svm 真实 CPU 是整机的 1.9%，在固定 0-100 纵轴上折线压底不可见。修复方式是改单核口径（15%），顺带与 adb top 对齐（实测均值 15.8% vs top 15.35%，差异为测量噪声）。
- **游戏/SurfaceView 直渲染的 FPS**：gfxinfo 无效，必须用 SurfaceFlinger 图层帧时间戳（`--latency`）。直渲染图层名常不含包名（"SVM Container"），按全量 dumpsys 的 ownerPID 归属匹配。
- **真机坑**：①`--latency-clear` 在此车机只清空不返回数据 → 用 `--latency` 差值法；②latency 缓冲末行有 `actualPresent=i64::MAX` 未上屏哨兵，需过滤；③meminfo 的 `TOTAL PSS:` 与分类行之间隔空行，须在 App Summary 区块外兜底解析；④jank 不能用 vsync 阈值（60Hz 屏上 30fps 相机流会被误判全卡），用间隔 > 2×窗口中位间隔。
- **agent 架构**：零依赖 Rust 静态二进制（568KB）交叉编译 aarch64-linux-android 推到 `/data/local/tmp/xperf-agent`，NDJSON 经 `adb exec-out` 长连接回传。50ms 间隔稳定（均值 15.03% 与 adb top 一致），可见 25-47% 瞬时毛刺。
- **perfetto 深挖模式已验证可行**：设备 v15.0 + traced 在跑；8s trace 13MB，ftrace 15 万事件；此车机 frametimeline 无进程归属（layer/upid 全 NULL）。主机侧 trace_processor 已下载验证（/tmp/trace_processor，v58.2）。trace 输出须写 `/data/misc/perfetto-traces/`（su 域写 /data/local/tmp 被 SELinux 拒）。

### 遗留问题

- 全部待办见 `WORKSPACE.md`（A 类缺陷优先：agent 低间隔 FPS 限频、边采边落盘、时序无上限）。
- HANDOFF.md 已被本工作流替代，删除。
