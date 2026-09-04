# SESSION.md — 会话历史

> **约定**：每个会话结束前追加一条记录（最新在最上）。格式：
> 日期 / 任务目标 / 完成内容（commit 列表）/ 关键结论与基线 / 遗留问题。
> 待办事项（backlog）在 `WORKSPACE.md` 维护，本文件只做历史追溯。
> 新会话开始时可先读本文件了解近期上下文。

---

## 2026-09-04（四）下午二 — 基线对比 C-6 收官 + 多设备 adb 全局 `-s`（46bd161）+ 设备动态检测（951c0ab）

**任务线**：WORKSPACE C 类最后一项（基线对比）+ 会话中途手机接入实测触发 E 节候补（多设备 adb 不带 `-s` 全链路失效），同会话三项落地；末轮按用户要求补设备热插拔动态检测。

### Commits

| commit | 内容 |
|--------|------|
| 951c0ab | feat：GUI 设备动态检测——`diff_devices` 纯函数（core/utils，单测）+ `spawn_device_monitor` 线程（3s 轮询快照 diff，首轮只建快照；adb 暂不可用跳过）+ `devices-changed` 事件 + 前端下拉增量重建/断开占位/接入提示。真机 SS2MAX 物理插拔两轮验证事件对全捕捉 |
| 46bd161 | feat：①基线对比——xperf-core/src/baseline.rs（SessionSummary 汇总 + SummaryBuilder + 保存/读取 + compare 报告，单测 11 个）+ CLI `--save-baseline`/`--compare-baseline`（互斥，报告落盘 `<ts>/baseline_report.txt`，--cold-start 结果进汇总）+ GUI 侧栏「保存基线/对比基线」按钮 + 峰值区基线对比面板（collectSessionData 与导出 CSV 同源）+ GUI 命令级测试 2 个。②多设备 adb——utils TARGET_SERIAL 全局 + adb() 构造器 + run_adb_command 自动注入 `-s`（16 处调用点收敛）+ parse/list/pick_device + CLI `--device/-d` + GUI 设备下拉（list_devices/select_device）+ `--package` 自动启动设备前置解析（多台未指定确定性跳过，消除与前端下拉的竞态；--device 无效不静默换台）+ sampling-error 事件（构建/部署/启动失败前端可见）+ detect_platform_live 按 serial 过滤（**修表头 bug**）+ device_online 只认目标设备。13 文件 +1396/-69 |

### 完成内容

- **基线判定口径（先设计后编码）**：变化须**同时**超过相对 ±10% 与指标绝对地板值才判回归/改善（地板值抑制近零噪声：CPU 2pp/PSS 4MB/FPS 2/Jank 0.5 次每分/GPU 3pp/IO·网络 50KB/s/冷启动 150ms）；基线为 0 退化为纯绝对差；仅一侧采集如实标「⊘ 单侧未采集」；Jank 按两侧各自时长换算为次/分钟（时长 0 不可算→单侧标注）；重启次数用绝对差不用百分比
- **汇总口径**：多 PID 样本全量合并；时长取全部时序（含 GPU-only 的设备级指标）首末样本跨度——曾因只扫 pid_stats 致 GPU-only 会话时长 0，真机发现后修
- **基线存放**：`~/.local/share/xperf/baselines/<pkg>.json`（XDG 数据目录，用户数据语义，--clean-cache 不清理）；包名拼路径前校验；CLI/GUI 同一文件互通（GUI 保存的基线 CLI 可对比，反之亦然）
- **真机验证（SS3 svm，`--device` 全程指定）**：保存（20s，CPU 30.3 均值/PSS 462MB/FPS 29.7/Jank 5.98 次分/GPU 15.3%）→ 对比（同场景二次运行，持平 9 项 + 单侧 5 项，GPU 15.4 vs 15.3 不误报）→ 篡改基线制造回归（CPU 基线压到 10% → ⚠ 回归 4 项 + 指标名列表）→ 无基线包（"未找到基线（先用 --save-baseline 保存一次）"）→ 篡改后真数据重存恢复
- **多设备真机验证（SS3 + Redmi 1280da60 双连）**：CLI 不指定 → 报错列设备清单；`--device 6eb792dfb0f` → 平台 SS3 + QNX GPU 通道正常（15.3% busy 同单设备场景）；`--device 1280da60` → Android 平台 + com.miui.home 采样正常（pid 7424）；GUI `--package --device` 自动启动全链路（平台 SS3、CPU/Mem/GPU 事件流）；单台连接自动选择；`--device deadbeef` → "指定设备不在线"跳过自动启动
- **设备动态检测（951c0ab，用户要求）**：监视线程 3s 轮询 diff（core `diff_devices` 纯函数 + 单测）；首轮只建快照不通知（首屏 loadDevices 已填）；目标设备移除不清后端 serial（断连重连插回即恢复），下拉占位"（已断开）"+ 状态提示。**真机验证方法论**：kill-server/reconnect/wait-for-disconnect 均制造不出 diff（server 重启后设备枚举快于 3s 轮询窗、transport_id 变但 serial 不变）——热插拔验证必须物理插拔（SS2MAX d1f39648c1f 两轮插拔，接入/移除事件对全捕捉）
- **验证门槛**：99 测试全绿（agent 24 + core 59+2 ignored + xperformance 8 + GUI 4 + xrm 2；本会话新增 21：baseline 11 + platform 过滤 1 + utils 设备 3 + GUI 基线 4 + 既有回归），clippy 零警告，cargo doc 零 warning（默认 lint 集 + 3 crate missing_docs）

### 关键结论与基线

- **detect_platform 表头坑（真机发现）**：按 serial 过滤 `adb devices -l` 行时必须补回表头——`detect_platform` 用 `skip(1)` 跳表头，滤掉表头后 SS3 行被当表头跳过、平台误判 Android（QNX GPU 通道跟着选错）；单测 `test_filter_device_line_picks_target` 锁定
- **`adb -s X devices -l` 仍列全部设备**（-s 对 devices 无过滤作用），平台检测必须自己按 serial 行过滤
- GUI 自动启动与前端设备下拉存在天然竞态（前端 loadDevices 会写全局 serial）——解法是**启动前在 main() 前置解析**（确定性），线程内兜底只服务手动开始路径
- svm 稳态噪声远小于判定闸门（CPU ±0.5pp / PSS ±0.1MB / GPU ±0.1pp），判定参数有效
- 正确包名是 `com.lixiang.car.x.svm`（不是 `com.li.xiang.car.x.svm`——验证中打错包名致 CPU 零样本，GPU 设备级指标照常流导致一度误判；目标进程不在线时 Noproc 静默，注意区分）

### 遗留问题

- GUI 多台未指定 `--device` 的自动启动跳过路径未双机复现（验证时手机恰断开，只剩 SS3 单台走了自动路径）；行为由 pick_device 单测 + CLI 同逻辑真机覆盖
- GUI 基线按钮/设备下拉的点击渲染为人工目验项（后端命令级测试已锁定链路）
- GUI 侧基线对比不含 restarts/cold-start（前端序列无这两路数据，如实单侧标注）
- 手机（1280da60）为标准 Android 平台——kgsl/thermalservice 真数据路径未完整采样验证（仅 CPU/平台检测），后续可在手机上验证 Android 平台 GPU/温度通道
- 设备热插拔的前端下拉/提示渲染为人工目验项（后端事件链路真机插拔验证通过：日志 `[devices] 热插拔` 四条 + devices-changed emit）

---

## 2026-09-04（四） — simpleperf 函数热点 --stack N：C 类"CPU 高在哪个函数"落地（CLI + GUI）

**任务线**：WORKSPACE C 类候选项——simpleperf 调用栈采样；同时确立新工作流规则（cargo doc 门槛写入 CLAUDE.md）。

### Commits

| commit | 内容 |
|--------|------|
| 9ee99ad | feat：xperf-core/src/simpleperf.rs（录制 + 设备端三视图报告 + 解析渲染，单测 8 个）+ CLI `--stack N`（独立/并行两模式，与 --trace 可同给）+ GUI 函数热点独立 tab + `--stack N` 自动启动 |
| bd4d5a9 | review: ①device_report `2>/dev/null` 连真实报错一起吞（失败时错误信息为空）→ `2>&1` 带回报错 + 成功路径过滤 simpleperf W/I 日志行（filter_simpleperf_logs，真机验证报告 0 条日志行）②validate_package 补 `-`（与 CLI/GUI 校验口径一致）③三处 doc"两视图"残留改三视图④前端 trace/stack recording 事件统一禁用按钮（命令行自动启动时按钮此前保持可点）。测试 42+1 全绿 |
| 41ae32a + c652da8 | feat：simpleperf 浏览器火焰图（open_stack_in_browser：AOSP report_html.py 渲染 `.data` → 单文件 HTML，gitiles blob 逐文件引导脚本集 ~10MB 缓存离线，失败清半截/HTML 新于 data 复用）+ GUI 按钮 + trace/stack 录制进度（core record progress 回调每整秒触发 → GUI emit progress 事件 → 前端 status 绿色进度条 c652da8）。真机全链路验证 |
| 61535f7 | feat：GUI 录制时长共享下拉（recordSeconds）+ 按钮改名「simpleperf 分析」+ 进度漏报修复（progress 上报移到等待循环头：adb 启动开销推迟首秒 + try_wait=Some 轮次跳过末秒，曾致 10s 只显示 6s；真机 10s 完整 1..10/10s） |
| 2bd6bca | feat：采集数据统一落 `/tmp/xperf`（CLI utils data_root / GUI gui_data_root，替代 ./log，/tmp 重启自清）+ 缓存/数据清理（core clean_all_caches：~/.cache/xperf + /tmp/xperf，CLI `--clean-cache` 无需 --package、GUI 侧栏按钮 confirm 确认）。验证：清理 40.7MB/29 文件、CLI/GUI 采集均落新根 |
| 695946a | fix：清理确认改 tauri-plugin-dialog 原生对话框（webkit2gtk 的 JS confirm() 窗口标题是 "Javascript-taurixxx"） |
| e0eaf7e | review: 前端 UI 修复 9 项——M1 #package 补 type=text（属性选择器不匹配无 type 输入框→白底不随主题）/ M2 recorded 阶段退出进度态（原冻结在 100%"录制中"）/ M3 录制时长残留 flex 包裹改 label / M4 进度条颜色主题变量化 / L1-L2 死 CSS+旧注释 / L3 NoProcess 不抹进度条 / L5 recording 文案 / L7 内联样式收口 .sidebar-row / 后端 export_csv 补 validate_package |
| 57e84ba | review: UI 布局缺陷 6 项（逐项代码实证属实后修）——tab 内容区 3 处 inline flex 收口 .tab-content / #status flex-shrink 防窄窗截断 / 指标页 idleHint 空闲引导（开始后隐藏）/ 侧栏「深挖录制」「数据管理」分组标题 / 清理按钮幽灵样式降级（破坏性操作视觉区分）/ resize 50ms 硬编码收敛双 rAF |
| e8f75c9 | feat：`--package` 自动启动与 UI 手动启动统一流程——后端 startup_args 命令（AppState 记录包名/间隔/flags，spawn_sampling 统一写入）+ 前端初始化回填（包名/间隔/勾选 toggleCharts 同步/idleHint 隐藏/状态文案带包名）；深挖按钮 .flex-fill 等分（「Perfetto 分析」长文案曾把 simpleperf 按钮挤出侧栏）；顺带修 2 个 clippy 警告 |
| 90b57ed | chore：Cargo.lock 同步 tauri-plugin-dialog 依赖锁（695946a 引入依赖时漏提交） |
| （docs） | b7405c2 / 22445ae（61535f7 + 2bd6bca 收尾）、461b47f（e0eaf7e 补记）、c4e5835（57e84ba 补记）、c77e0f1（e8f75c9 补记）——CLAUDE.md 路径与行为同步 + WORKSPACE / SESSION 更新 |

### 完成内容

- **先实测后编码**（SS3，simpleperf 1.build.47，adbd root）：`--app <pkg> -g --duration N` 录制正常（svm 空闲 8s ≈ 8500 样本 / 0 丢失 / 3.3MB）；`--app` 对未运行应用输出 `Waiting for process of app …` **无限等待（--duration 拦不住，等待在采样开始前）** → pidof 前置拦截 + 主机侧超时（N+25s）兜底；三种 report 视图输出格式逐一定宽验证
- **模块设计**（xperf-core/src/simpleperf.rs，与 trace.rs 同构：目录调用方传、报告文本返回）：
  - record：包名防御性校验（防 shell 注入）→ which simpleperf 存在性 → pidof 拦截 → record（2>&1 合并解析 `Samples recorded/lost`）→ 三视图 report（**pull 前跑——.data 还在设备上**，单视图失败不中断）→ pull + 清理
  - 三视图：线程 CPU 分布（`--sort comm,pid,tid`）/ 函数热点 self（`--sort symbol,dso`——"CPU 高在哪个函数"的直接回答，热叶子函数如 `_raw_spin_unlock_irqrestore` 5.26%、libgsl memcpy/mutex）/ children（`--children --sort symbol,dso` 热点路径）；均 `--percent-limit 1`
  - 解析按「首/尾 token 锚定」（线程名/符号名可含空格、dso 恒无空格、children 与 self 视图按"次首 token 是否百分比"区分）；报告落盘前空格压缩（定宽 padding 去除，4.9MB → 566KB）
- **CLI**：`--stack N` 独立（无指标 flag 时只录调用栈；与 `--trace` 同给时并行录制同窗口、报告 trace → stack 顺序输出；录制失败**非零退出**——脚本化验证门槛，分析失败不算）+ 并行（后台线程 + 采样限时同窗口；--trace/--stack 同给取 max）
- **GUI**：「函数热点」独立 tab（tabBar 三 tab，两个分析页均隐藏侧栏）+ 侧栏秒数/按钮 + `stack` 事件（stage/message/data_path，与 trace 事件同构）+ `--stack N` 自动启动
- **验证**（SS3 全场景）：独立 8s（三视图全出、self 视图热点函数命中）/ 并行 `--cpu --stack 6 --interval 500`（agent 采样同窗口限时结束 + stack 报告）/ 未运行包拦截（"应用无运行中的进程，请先启动"，exit=1，无 adb 挂起）/ 正常 exit=0 / GUI `--stack 5`（[stack] 启动→拉回 6458 样本→产物齐全）
- **验证关卡**：75 测试全绿（simpleperf 8 个）+ 1 ignored、clippy 零警告、`cargo doc` 零 warning（默认 lint 集 + 4 crate missing_docs 归零）
- **浏览器火焰图（41ae32a，先实测后编码）**：官方链路选定 AOSP `report_html.py`（本地手跑验证 3.3MB data → 7.8MB 单文件 HTML，含 flamegraph/Chart/Sample Table，~1.2s）。**AOSP 源获取坑**：gitiles `+archive` 不支持多级子路径（`+archive/main/simpleperf/scripts/xxx.tar.gz` 返回 INVALID_ARGUMENT；整仓 tarball 80MB 太重）；main 分支脚本已从 `scripts/simpleperf` 迁到根级 `simpleperf/scripts`（master 分支已不存在）；最终走 blob `?format=TEXT` base64 逐文件下载（~10MB，`python3 -m base64 -d` 解码跨平台）。**依赖闭包踩坑**：`etm_types.py`（report_lib 的 ETM 解析 import）与 `report_html.js`（write_script 内联的前端脚本，缺则生成半截 HTML 后失败）两个易漏件，靠真跑报错逐个补齐；主机 report 库仅 linux-x86_64（`.so`）/darwin-x86_64（`.dylib`）两种预编译（上游 get_host_binary_path 规则）。**半截 HTML 复用 bug**：python 崩溃会留下 1.2KB 残骸且 mtime 新于 `.data`，reuse 判定会误复用——生成失败时删除残留。ignored 真实链路测试锁定（首次引导下载 + 渲染 + xdg-open 全通；二次跑 0.00s 复用）
- GUI 侧栏按钮更名「simpleperf 分析」（与「Perfetto 分析」命名对齐）；录制时长合并为共享 `recordSeconds` 下拉（61535f7）。**进度漏报根因（61535f7 修）**：progress 上报原在等待循环 None 分支——adb 启动开销 ~0.5s 推迟首个整秒 + 最后一秒常落在 try_wait=Some 的轮次被跳过，10s 录制只显示到 ~6s；移到循环头后 10s 完整显示 1..10/10s（trace/stack 两处对称修，真机验证）

### 关键结论与基线

- **simpleperf `--app` 等待语义**：未运行应用无限等待（`--duration` 不约束等待期）——凡 `--app` 用法必须前置进程存在性检查
- 设备端应用 so 多 stripped：函数名显示 `libxxx.so[+偏移]`（偏移可用未剥离 so 离线符号化）；系统库（libc/libgsl）与 `[kernel.kallsyms]` 有符号；非 root 设备非 debuggable 应用被 run-as 拒（错误透传）
- report 定宽文本按列位置切片是脆的（Symbol 列 padding 达数百列宽），token 锚定 + 空格压缩是稳定做法
- 新规则（已入 CLAUDE.md 工作流约定）：**代码必须过 cargo doc（全量零 warning），所有 pub 项 doc 注释规范完整（含单位/语义/无值字段写明）**——本模块按此标准编写
- 与 perfetto 深挖的三级下钻关系：采样"什么时候高"→ perfetto"线程/调度/帧为什么高"→ simpleperf"哪个函数"

### 遗留问题

- SS2MAX/SS4 未真机验证 simpleperf（标准 Android 能力，SS2MAX adb root 后预期可用；非 root+非 debuggable 场景会被拒——错误信息透传可辨）
- 多设备连接 adb 不带 -s 的全局问题（E 节既有候补项，本模块同样不带 -s，与全工具链一致）
- GUI 前端按钮点击渲染为人工目验项（后端链路已由 ignored 真实测试 + diag log 验证）
- 火焰图 HTML 的样式/交互库（bootstrap/jquery/google-charts）走公网 CDN——离线打开时图表样式降级（数据/火焰图本体已内嵌，核心可看）
- trace/stack 并行录制共用单个 status 栏：文案/进度条互相覆盖，后完成者胜（单用户场景罕见，不修）
- 图表 series 色板在 JS 硬编码镜像 CSS 变量（mocha/latte 两套），主题色改动需双处同步（漂移风险，重构收益低）
- 深色主题下 tauri-plugin-dialog 原生对话框跟随系统主题而非应用主题（插件行为，无配置入口）

---

## 2026-09-03（三）晚二 ～ 09-04（四） — perfetto 深挖模式：浏览器全自动加载 + 配置对齐 + GUI 主题 + cargo doc 全覆盖

**任务线**：WORKSPACE C 类第二项——perfetto 深挖模式（2026-09-01 已验证可行性，2026-09-03 落地为产品功能；后续按用户反馈迭代：浏览器加载方案两轮演进 → trace 配置对齐团队口径 → GUI 布局与主题 → 多轮 code/doc review + cargo doc 全覆盖）。

### Commits

| commit | 内容 |
|--------|------|
| ca01aa6 | xperformance/src/trace.rs 新模块：录制 + trace_processor 定位/引导 + SQL 分析 + 报告；main.rs 集成 `--trace N`（独立/并行两模式 + 限时采样）；xperf-core utils 加 `is_interrupted()`（含 CLAUDE/WORKSPACE/SESSION 文档） |
| 4b6a00d | code review 修复：①idle 口径 bug（实测发现 sched 表含 swapper 切片：utid=0 挂 upid=0 无名进程，不排除则空闲机器每核 busy 恒 ~99.7%、(内核线程) 桶被 idle 淹没 80%+ → top_procs/per_core 加 `utid != 0`，真机复测每核 3.4%~22.1% 合理、真实进程浮现）②cpufreq 去掉 limit 16 ③引导下载加超时（curl -m/wget -T 60）④trace_analysis.txt 写失败告警；单测锁定 idle 排除 |
| 944e854 | GUI 深挖支持：trace.rs 下沉 xperf-core（record 参数化输出目录、analyze_and_report 返回报告文本、去打印——CLI/GUI 共用）；GUI 加 `start_trace` 命令 + `spawn_trace` 线程（`trace` 事件推 recording/recorded/done/error 进度）+ 侧栏秒数/按钮 + 报告面板 + `--trace N` 命令行自动启动（脚本化验证路径）+ 关窗置中断标志；CLI 行为不变（下沉后独立/并行真机回归通过） |
| d9dd973 | GUI 交互按用户要求调整：①按钮改名"Perfetto 分析"②报告独立 tab 页（性能指标 / Perfetto 分析，done/error 自动切换，录制期间留指标页）③trace 一键浏览器分析——core 加 `open_in_perfetto_ui`（本地单文件 HTTP 服务 127.0.0.1 随机端口 + CORS/OPTIONS + ui.perfetto.dev `#!/viewer?url=` 深链；loopback mixed content 浏览器豁免；单测覆盖 GET/OPTIONS），GUI `open_perfetto_ui` 命令（xdg-open/open）+ 分析页"在浏览器打开 Perfetto UI 分析"按钮（trace 事件 payload 附 trace_path） |
| 5dfd362 | 浏览器加载方案按实测切换为文件拖拽（用户真机复现 TypeError: Failed to fetch，headless Chrome 复现并定位）：本地 HTTP + ?url= 深链被双重拦截——①ui.perfetto.dev CSP connect-src 白名单只放行 localhost:8080/127.0.0.1:9001 等固定口（随机端口全 block，错误只在 console 可见、netlog 零请求）②Chrome 152 LNA 权限拦 loopback fetch（公网页面无手势静默拒，带 PNA 头也过不了；本机 8080 又被其他服务占用）。改 `reveal_trace_and_open_ui`：打开 ui.perfetto.dev + dbus FileManager1.ShowItems 高亮 trace 文件（fallback xdg-open 目录 / macOS open -R），用户拖入浏览器即加载（File API 零网络请求，不受 CSP/LNA/mixed content 限制）；删本地 HTTP 服务实现与单测；dbus reveal 真机验证通过 |
| 3ee4ea2 | 浏览器**全自动加载**（用户要求免拖拽）：本地镜像 Perfetto UI + 同源深链——headless Chrome 跑 ui.perfetto.dev 提取 netlog 资源清单（~20 个，含 wasm 引擎）+ curl 镜像到 ~/.cache/xperf/perfetto_ui（跳过浏览器探测性 404 资源）+ 单例本地服务器（每连接一线程，serve 镜像 + 动态注册 trace，SW 固定 404）+ 深链 `#!/viewer?url=绝对URL`。同源 fetch 过 CSP 'self'/无 mixed content/无 LNA，零交互。**排查路上三个关键坑（教训已入 CLAUDE.md 验证方法论）**：①`?url=` 必须绝对 URL——SPA `new URL()` 不传 base，相对路径抛 Invalid URL 中断路由（console 才可见）；②曾用 `grep -c canvas` 验证被 Chrome 错误页内嵌 JS 误导判"成功"（"Sched"匹配了"scheduled"），且 python 对照 server 被自己清理脚本误杀（"对照成功"实为 connection refused 错误页），据此误推出"preconnect 死锁"并白改一版每连接线程（该修复本身更健壮，保留）；③`--virtual-time-budget` 不等 wasm 真实解析，dump-dom 总在解析前——最终以完整 console 证据链定案（WASM 引擎启动→trace 16MB@41MB/s 本地下载→local_cache_key 路由切换→WebGL 渲染活动）。失败自动回退拖拽。新增单测：netlog 资源提取/UI 服务器路由（含路径穿越防护）+ ignored 手动测试 test_real_ui_serve；测试 69 全绿 |
| cc76721 | trace 配置对齐团队 Performance_Tools general_debug.pbtxt（用户指定参考 /home/han/code/tools/Performance_Tools_Linux_V1.0_2024_7_10/config）：ftrace 全事件（sched 全家 + power/suspend_resume + cpu_frequency/cpu_idle/gpu_frequency + gpu_mem + task_newtask/rename + ftrace/print + ext4/f2fs/kmem/memory_bus/mmc 通配）+ atrace 17 类目 + atrace_apps "*" + android.log（kernel/default/system）+ packages_list + gpu.memory + process_stats(scan_all_processes_on_start) + frametimeline；双 buffer（128MB ftrace + 32MB stats/log RING）+ ftrace 内核缓冲 32MB/drain 2ms；write_into_file 流式落盘保留。SS3 真机实测：10s ≈ 46MB（原裸配置 11.8MB），atrace slice 17 万/10s、log 3425 条、cpuidle counter 生效、cpufreq 仍无（GVM）；不存在的 ftrace 事件 perfetto 静默忽略。端到端回归：录制→分析报告（svm 18.9% 负载、401 帧最差 27.2ms）正常 |
| dabb1e2 | GUI 布局与主题（用户反馈）：①Perfetto 分析 tab 自动隐藏左侧采样控制栏（报告占满全宽，切回指标页恢复）；status 栏从侧栏移至 tabBar 行右端（隐藏侧栏后状态仍可见）②暗/亮双主题——tabBar 右侧按钮切换 + localStorage 持久化；CSS 变量全量化（mocha/latte 双色板）+ `color-scheme`；canvas 图表（uiColors() 读 CSS 变量 + 双 series 色板）与实时面板（setLive 色参数语义化映射 CSS 变量）动态跟随主题；index.html 硬编码 inline 色全部收进 class。后补：checkbox 暗色白底修复（cddb84e）——webkit2gtk 对原生 checkbox 暗色渲染支持不全，改 appearance:none 自绘 |
| 29ce6e8 | code review 修复（用户要求 review 代码与文档）：①open_trace_in_local_ui 的 doc 注释过时（仍写"相对路径"，实现已改绝对 URL）②reveal_trace_and_open_ui doc 对比对象措辞澄清（"ui.perfetto.dev 页面 + 本地 HTTP"）③报告帧时间线文案去平台特化（"此平台无归属"写死 SS3 特性 → 通用"全局统计"，按图层归属深入指引浏览器 UI）④ensure_perfetto_ui_mirror 加进程内互斥（首次镜像 ~2min，并发调用会交错写坏缓存文件）⑤CLAUDE.md 修正三处过时/不准：实测基线补当前配置数据（46MB）、GUI 主题段更新 checkbox 自绘事实（webkit2gtk 不支持原生暗色）、报告段同③。测试 69 全绿 + clippy 零警告 |
| 288d385 | cargo doc 全覆盖（用户要求检查）：rustdoc -W missing_docs 检出 xperf-core 215 处 + xperformance/xperf-gui crate 级 2 处，全部补齐——crate/mod doc、AgentEvent 18 变体全字段（NDJSON 协议逐项注明单位语义）、SampleEvent 全变体/字段、MetricFlags/MemoryDetails/FpsSample/ThreadCpuInfo/Marker/ProcOutput/PlatformId/Platform trait、RecordedTrace/Analysis 等全字段；xperf-core 加 #![warn(missing_docs)] 常开（新增 pub 项漏 doc 构建即警告）。4 个 crate 检查均归零，69 测试全绿 + clippy 零警告 |
| 9c795c0 | review: 代码/注释/文档 review 修复——test_ui_server_routes 加单例组合边界注释（与 ignored 的 test_real_ui_serve 不能 --include-ignored 同进程混跑）；PidStats.start_time doc 精确化（历史字段恒为空）；SESSION 补记 doc 全覆盖轮；CLAUDE.md Commands 补文档覆盖检查命令 |
| fdfef14 | cargo doc 编译错误 + 大量 warning 修复（用户实测抓出）：完整 `cargo doc` 暴露三类 rustdoc 默认 lint 问题——①15 处 doc 裸尖括号（`/proc/<pid>/io` 等被 rustdoc 当未闭合 HTML 标签；xperformance 的 `#![deny(warnings)]` 将其升级为 **error 导致 cargo doc 失败**）→ 路径/参数包反引号（code span 不解析 HTML）②trace.rs 模块级 doc 裸 URL → markdown 链接③10 处日志样例 `[GPU0]` 被当 intra-doc 链接 → code block/code span 包裹。**教训（已入 CLAUDE.md Commands）**：上轮宣称"cargo doc 检查过"实际只跑了 `rustdoc -W missing_docs` 单 lint——默认 lint 集（invalid_html_tags/bare_urls/broken_intra_doc_links）根本没跑，验证不完整；以后必须跑完整 `cargo doc` 且 warning 归零。修后：cargo doc 零 warning 零 error、69 测试全绿、clippy 零警告、4 crate missing_docs 归零 |
| af65eed | review: 上轮 cargo doc 修复 diff 逐行核对（15 处 sed 补丁全部正确）后发现两处遗留——①agent 协议头注释（NDJSON 每行样例）裸排在 markdown 段落：`<wall_ms>` 含下划线不匹配 HTML 标签名模式恰好躲过 invalid_html_tags lint，但生成 HTML 缩进折叠逐行断段、渲染是坏的 → 整块包 text code block；②gui/agent 两处 usage 行包裹规则不一致（只包含尖括号的 token）→ 整条 usage 一个 code span。cargo doc 零 warning、69 测试全绿、clippy 零警告 |

### 完成内容

- **先实测后编码**：SS3 真机先录 10s trace 逐条验证 SQL（sched/thread_state 状态码 R/R+/Running/S/D、process 表 full name、cpu_counter_track、actual_frame_timeline_slice、多语句 -q 文件的输出格式），全部通过才写代码
- 录制链路：`adb shell perfetto -c - --txt -o /data/misc/perfetto-traces/xperf_<ts>.pftrace`（stdin 喂 text proto，v15.0 实测可行）；`write_into_file: true` + 2s 刷盘（实测录制中文件持续增长，长录制内存有界）；录完 pull 到 `log/<pkg>/<ts>/trace/` 并清理设备端
- trace_processor 定位链：官方缓存 `~/.local/share/perfetto/prebuilts/trace_processor_shell*` → PATH → /tmp 自举脚本 → 均无从 get.perfetto.dev 下载引导；分析失败不致命（trace 已保存，提示 ui.perfetto.dev）
- SQL 分析：单文件多语句单次执行（trace 只加载一次）+ marker 分段 + `===END===` 哨兵；**实测查询出错会中止整个文件后续语句** → 按"表必然存在 → 可能缺失"排序，帧时间线殿后；[NULL] 值、quote-aware CSV 解析、空段登记（区分"空数据"与"未执行到"）均有单测
- 报告段：trace 窗口（boot 基线）、包 CPU 总量（单核口径 % 窗口）、包线程 CPU top15、抢占/调度延迟（thread_state R/R+）、系统 CPU top（upid=0 桶标"内核线程"）、每核 busy/切换、CPU 频率（SS3 GVM 无 cpufreq ftrace 事件，如实标注）、帧时间线全局统计 + 最差 5 帧；产物 .pftrace + trace_analysis.txt + trace_queries.sql 同目录留存
- CLI 两模式：`--trace 10` 独立（无指标 flag 时只录 trace，注册 Ctrl-C handler）；`--cpu --trace 10` 并行（后台线程录制 + 采样限时同窗口，到点自动结束，采样/图表/CSV 正常产出后 join 分析）
- Ctrl-C 修复（真机发现）：SIGINT 杀整个前台进程组 → adb 一并死，原先误报"录制失败: [659 Connected...]"且前缀重复两遍 → 区分信号中断（ExitStatusExt::signal）与失败退出码，消息改为"录制被中断 + 设备残留路径（traced TTL 后停止写入，可手动 pull）"；等待循环查 `is_interrupted()` 提前放弃
- 验证（SS3 全 6 场景）：独立 10s（11.8MB，包线程毫秒级明细全出）、并行 10s（限时结束/同目录/图表共存）、并行 Ctrl-C、独立 Ctrl-C、不存在的包（"无调度事件"如实上报）、1s 极限窗口；60s 长录制的超时/中断路径已有兜底（duration+25s 手动超时）
- 测试 67 全绿（新增 6：config 字段/SQL 转义与排序/CSV 解析/sections 解析/[NULL]/报告渲染），clippy 零警告

### 关键结论与基线

- v15.0 配置坑：ProcessStatsConfig 的 `scan_period_ms`/`proc_stats_poll_period_ms` 字段都不存在，省略子配置用默认值即可（进程/线程全名映射正常）
- SS3 GVM 无 `cpu_frequency` ftrace 事件（`/sys/kernel/tracing/events/cpu_frequency` 不存在，只有 devfreq）——trace 里频率段恒空，如实标注；实时频率走 agent `--freq`（sysfs 可读）；**power/cpu_idle 事件生效**（cpuidle counter 每核一条 track）
- trace_processor 多语句输出：结果集 = 表头 + 数据行 + 空行；查询失败 abort 整文件剩余语句且退出码非零，但已完成语句的 stdout 仍有效（只要非空就继续解析）
- **配置对齐团队 general_debug.pbtxt 后（cc76721）**：10s ≈ 46MB（~4.6MB/s，600s ≈ 2.8GB 落盘）、atrace slice 17 万/10s（系统进程有 app 层归因轨道）、android_logs 3425 条、sched 20 万；不存在的 ftrace 事件（memory_bus/\* 等）perfetto 静默忽略
- **浏览器加载（3ee4ea2）**：ui.perfetto.dev + 本地 HTTP + ?url= 深链被 CSP connect-src 白名单 + Chrome 152 LNA 双重拦截；可行方案是本地镜像 UI（~20 资源，缓存 ~/.cache/xperf/perfetto_ui）+ 同源深链（url 参数必须**绝对 URL**——SPA `new URL()` 不传 base）
- **rustdoc 三类坑（fdfef14/af65eed）**：doc 裸尖括号当未闭合 HTML（xperformance `#![deny(warnings)]` 升级为 error）、裸 URL、日志样例 `[GPU0]` 当 intra-doc 链接；`<wall_ms>` 含下划线恰好躲过 lint 但渲染坏——协议样例统一包 code block。**检查必须跑完整 `cargo doc`（默认 lint 集），单跑 `-W missing_docs` 不够**
- **webkit2gtk（Tauri webview）对原生控件暗色渲染支持不全**：color-scheme 有效但 checkbox 仍白底 → appearance:none 自绘（cddb84e）；Chrome 验证结论不能直接外推到 webkit2gtk
- adb pull root 0600 文件 OK（adbd root）

### 遗留问题

- 长录制（>60s）超时/中断路径未真机实测（有兜底逻辑）；600s 上限内 write_into_file 理论无内存问题
- 限时采样（--trace）+ 采集中途断连：主循环进 reconnect_agent 无限轮询等设备，deadline 不打断重连（Ctrl-C 可退；修复需给 reconnect 回调传 deadline，收益小改动大，记录不修）
- trace_processor 分析大 trace（600s ~2.8GB）加载耗时数分钟，无超时（Ctrl-C 可中断，深挖场景等得起，设计如此）
- 镜像的 Perfetto UI 版本与官方同步问题：无失效检测（UI 结构大改导致资源 404 时会报错回退拖拽，可接受；手动清 ~/.cache/xperf/perfetto_ui 重新镜像）
- simpleperf 调用栈采样（"CPU 高在哪个函数"）为下轮 C 类候选（见 WORKSPACE.md）

---

## 2026-09-03（三）下午 — SS3 QNX 通道真机回归：发现并修复 kgsl 统计链停滞

**任务线**：WORKSPACE 遗留项——SS3 设备在线，补 gpu/qnx.rs 拆分后的真机回归。

### Commits

| commit | 内容 |
|--------|------|
| 6e6f428 | qnx.rs：fd3 长活连接写入 + slog 行去重 + 看门狗自愈；gpu/mod.rs：spawn_stream_parser keepalive 改 Arc<Mutex<ChildStdin>>、parse 改闭包 |
| d2b8ba0 | review 修复：看门狗单次缺失改 3 连续缺失（实测窗口 ~1001.5ms > 检查周期 1000ms，单次缺失是相位漂移、长会话约每 11 分钟必现，原逻辑误触发自愈）；自愈改先写 stdin 成功才 emit（通道断开后不再产生误导 err） |
| 9310aa6 | 二轮 review：看门狗决策抽纯函数 watchdog_step + 单测锁定语义（阈值回退防护） |
| 4644e9a/7e0f4c5 | 三轮 review（用户质询"为什么不修"后复验）：agent 心跳——整轮零输出发空行探活（host next_event 本就跳空行，零协议改动），修复主机断连后 agent 残留；原"不修"判断的两个前提均错（SS3 --gpu 常规场景 gpumem 每秒兜底并非静默；心跳也无需新协议类型） |
| 94d5aba | 四轮 review（用户质询"不修项理由是否成立"）：发现 `echo>` 死写入者对 kgsl 链是 **toggle 语义**（流链→停、停链→复活，"无清理接口"与"干净态即时"两个不修理由均不成立）。落地三层链清理：agent 退出钩子（emit 失败先跑钩子再 exit）、spawn_agent 加 setsid（脱离 adbd 会话，断连走 EPIPE 而非信号直杀——实测 adbd 按进程树杀、setsid 后仍有竞态）、host qnx_stop_stats 条件兜底（纯观察探测≥2 帧才停链，防复活）；CLI/GUI 会话结束调用。真机验证：SIGINT 退出停链 ✓、下一会话 13 样本 0 自愈即起流（未清理时 8 样本+停滞）✓ |
| 84fc8d7 | 五轮 review（用户质询"再 review 不修事项"）：①多会话并发实锤——一方退出清理杀对方流（实测 5-8s 缺口+自愈），加 pgrep 并发保护（agent 钩子 other_agents_running>1 跳过；host 兜底轮询等自身收尸后 ≥1 跳过——初版立即探测把未收尸的自身误判为他人，单会话清理失效，实测修复）；②振荡项以 10 会话自愈全 ≤1 佐证成立；③--pid 项核实 host 均 --package 成立；④多设备 -s "单设备"理由过时（设备清单两台）已修正措辞列候补；⑤新记录双会话交互（后启动方致先启动方一次 ~7s 停走、事件密度 ~2×）不修 |

### 完成内容

- 回归发现真 bug：QNX frame 流"首条后停走"，跨会话交替通/停（A停→B通→C停→…），agent 会话 15-30s 只有 1 条 gpu 事件
- 五轮 review（用户三轮追问驱动，三轮翻案）沉淀的完整机制见 CLAUDE.md「QNX kgsl 统计链」：toggle 写入语义、fd3 活连接、行级去重、看门狗（3 连续缺失阈值 + watchdog_step 纯函数单测锁定）、心跳空行探活、三层链清理（退出钩子/setsid/host 条件兜底）+ 多会话并发保护（pgrep + 收尸等待）。方法论教训：**"罕见/无接口/代价高"类不修结论必须先做最小实验**——三轮翻案（心跳、链清理、并发保护）全是用实验推翻推演
- 黑盒定位（重启车机前后 10+ 组对照实验）：
  - **kgsl 统计链是驱动全局的**（开机自带 5000ms 链），会话死亡/fd 关闭都不清理（泄漏到整机重启；frame 计数持续递增证明）
  - `echo X > /dev/kgsl-control` 即开即死连接撞上存量链：只 flush 一个窗口（elapsed=5001ms 之类）即停
  - 长活连接（`exec 3>/dev/kgsl-control` + `>&3` 写入）撞存量链：全链重相位（计数归零、锁步）后持续输出
  - 多条链锁步时同一 slog 行重复 N 份（实测 4 链 ×4 拷贝）；`kgsl_driver_cleanup_full` 对已死进程清理失败（slog 有 WARNING）
- 修复（xperf-agent/src/gpu/qnx.rs）：
  1. 启动命令序列改 `exec 3>` 持 fd 写入（写入时连接存活是链持续输出的必要条件）
  2. 读线程 parse 闭包按"与上一行完全相同"去重（Sys/Proc 各记上一条，锁步重复行总相邻）
  3. 看门狗：frame 流静默超宽限（2×周期+3s）经同一 telnet 会话 fd3 重写 gpubusystats 自愈（≤3 次，恢复归零）
  4. spawn_stream_parser（gpu/mod.rs）keepalive 参数改 `Option<Arc<Mutex<ChildStdin>>>`（读线程保活 + 看门狗共享写入），parse 参数 fn 指针改 `impl Fn` 闭包（携带去重/计数状态）
- 验证：60 测试全绿、clippy 零警告；SS3 真机——4 条泄漏链硬场景下启动顿 ~5s + 1 次自愈后稳定 1/s（busy 10.1%/util 8.0%/507MHz）；gpu/gpuproc/gpumem 三类事件与 CSV（gpu_data / gpu_proc_9671 / gpumem）全通；连续 3 轮 kill/重跑稳定；CLI 端到端（--cpu --gpu）正常

### 关键结论与基线

- QNX slog 行格式不变（frame N: freq/busy/utilization；For process[PID] = 'comm 名'）；gpuproc 按 comm 归因验证通过（eid → pid 9671 @9.3%，svm 空闲时无 gpuproc 属正常）
- kgsl-control 无清理/查询接口，未知命令静默忽略；QNX 侧工具极简（/bin 仅 ksh/login/sh/camera，无 base64/od/tr/sort），二进制取证困难
- 真机包名教训：`com.lixiang.car.x.svm`（lixiang 无点）——ps 输出误读成 li.xiang 导致 noproc 排查弯路，od -c 字节级核对才定位
- run_cmd 输出偶发整段复制伪影（同时间戳同内容重复）：判断以文件落盘统计为准

### 遗留问题

（后续四/五轮 review 已推翻并修复下列两项——三层链清理 + 并发保护落地，最新状态见 WORKSPACE E 与 CLAUDE.md「QNX kgsl 统计链」；残余仅 SIGKILL 暴杀与 reboot 后首会话 ~8s 停走，均被下一会话看门狗兜住）

---

## 2026-09-03（三）晚 — agent 单文件模块化拆分 + mtime 检查修复 + SS2MAX 回归

**任务线**：WORKSPACE D 类第一项——xperf-agent 1848 行单文件拆分（src 布局已就位），拆完真机回归。

### Commits

| commit | 内容 |
|--------|------|
| 531798a | agent 模块化拆分（10 文件 + 公共 spawn_stream_parser + gpumem 臂合并）+ ensure_agent_built mtime 改扫 src 树 |
| 99d1b74 | review 修复：mtime 只认 .rs 扩展；read_smaps_rollup/GpuEvent 可见性收紧 |

### 完成内容

- `xperf-agent/src/` 拆为 10 文件：`main.rs`（493 行：协议头注释/Args/节拍循环/公共工具 emit·json_escape·now_ms·dumpsys）、`proc.rs`（/proc+sysfs 读取 + PidState::sample_cpu）、`mem.rs`（smaps/meminfo，sample_memory）、`fps.rs`（FpsState::sample_round）、`thermal.rs`（sample）、`gpu/{mod,kgsl,qnx,topgpu,ligfx}.rs`
- 三份重复读线程骨架（QNX/TopGpu/Ligfx）抽公共 `gpu::spawn_stream_parser`（keepalive stdin 保活 + eof_err 参数化）；三通道样本枚举归一为 `GpuEvent::Sys/Proc`，emit 按通道字段有无按需输出（wire 格式逐字节不变）
- main 循环里四段完全相同的 gpumem 补采臂（Qnx/TopGpu/Ligfx/DumpMem）合并为一
- 删除 `detect_gpu_path` 死代码包装
- **host 侧修复**：`ensure_agent_built` 原来只比较 `xperf-agent/main.rs` mtime——拆分后改子模块不会触发重建；改为 `newest_mtime_under(src)` 扫整棵 src 树取最新 .rs 的 mtime（review 后收紧：非 .rs 文件不触发）
- 测试 23 个随模块迁移（全绿，workspace 60）；clippy 零警告；交叉编译通过

### 真机回归（SS2MAX d1f39648c1f，SS3 不在线）

- CLI 全指标 500ms 跑 35s：CPU/内存全分类/FPS/频率/温度(sysfs 44 传感器)/IO/网络全流，图表全生成，SIGINT 优雅退出；touch 子模块触发自动重建验证通过
- **新旧 agent 同机对比**（git worktree 检出拆分前代码，adb 直跑同参数）：事件类型分布、wire 格式、数值量级完全一致（500ms：cpu 17/18、temp 4/4、overrun 14/15；100ms：smaps pss 194503/194439 KB、overrun 65/60）——**无回归**
- 新发现（数据源限制，非代码问题）：**SS2MAX gpubusy 计数器恒 `0 0`**（30 次采样全零，total_time 停走 → `dtotal>0` 永不成立 → kgsl 通道无事件；gpuclk 正常 427MHz）——新旧行为一致；SVM 的 smaps_rollup Pss ~194MB vs meminfo TOTAL PSS ~378MB（Graphics 220MB 不计入 smaps Pss），两路径差异固有

### 遗留问题

- SS3 QNX 流式通道拆分后未真机回归（解析有单测、读线程逻辑逐行对齐）；SS3 接入时跑 `--gpu --platform ss3` 补验
- SS2MAX 全指标 500ms 每轮 overrun 0.4-2s（FPS 图层重发现全量 dumpsys ~1.5s+ 是主因，新旧一致，设备算力限制）

---

## 2026-09-03（三）— 平台抽象 + 五平台 GPU + C 类验证 + SS2MAX 实测 + 两轮 review 修复

**任务线**：B 类收尾后的全面扩展——平台抽象（5 平台 trait）、各平台 GPU 通道、C 类验证能力、SS2MAX 真机实测、两轮 code review 全量修复、死代码清理。

### 完成内容（commits 旧→新）

| commit | 内容 |
|--------|------|
| 2058fcc | 平台抽象层：Platform trait + 5 平台文件 + adb devices -l 检测 + agent --platform/--qnx-host |
| 58ceae5 | 各平台 GPU：topgpu（SS2MAX）/ ligfxprofilerd（SS4）/ kgsl（Android）通道 + 单测 |
| a56a6f5 | C 类：--threshold 阈值告警+退出报告 / --cold-start 冷启动 |
| 0f7ad9d | C 类：时间轴打点（Unix socket + GUI 按钮 + 图表竖线 + markers.csv） |
| 178fd16 | SS2MAX 温度修复：thermalservice sensors 为空走 sysfs 兜底（44 传感器） |
| c3a2ee6 | SS2MAX 实测结论：IO/GPU 显存 SELinux 限制 + agent 提示 |
| 7f8fda1 | agent 自动 adb root（deploy 前 try_adb_root + id 验证） |
| 020e3ed | review 修复（严重 3 + 一般 6）：qnx-host OnceLock / FPS 基线误计 / GUI 关窗 / marker 阻塞 / QNX 泄漏 / adb root 文案等 |
| 14d514c | review 修复（一般 9）：alerts min / deploy mtime / json_escape / comm 截断 / aggs 清理 / coldstart 超时 / 退出码 / 路径遍历 |
| 02ee9bc | 二轮 review 修复：memory bail 文案 / lookup_pid UTF-8 panic / 重扫 exit 补发 / coldstart 30s / 包名校验前置 |
| d7eb4aa | marker 读超时 / agent 源码变更自动重建 / Tauri async / CSV 转义 |
| 225d89b | **删除 xperf-core 轮询参考实现**（-1653 行，D 类决策闭环） |
| 737ed45 | agent src 布局迁移（拆分第一步） |

### 关键结论与基线

- **GPU 五通道架构**：detect_gpu_path_ex(platform) 按平台选路——kgsl sysfs（有即用）/ QNX telnet（SS3）/ topgpu（SS2MAX，需工具）/ ligfxprofilerd logcat（SS4）/ dumpsys gpu 显存保底；所有通道补采显存。QNX/topgpu/ligfx 各自独立读线程（stdin 句柄须移交读线程持有，否则 drop 即 EOF 子进程退出）
- **SS2MAX 实测**（d1f39648c1f，SA8155P）：GPU 走 kgsl sysfs 直通（gpubusy+gpuclk 可读）；温度 44 sysfs 传感器（thermalservice HAL 有数据但 sensors 列表空——条件必须是 !sensors.is_empty() 而非 status.is_some()||...）；IO/GPU 显存被 SELinux 拦（agent shell uid 2000），**adb root 后 IO 可读**（try_adb_root 自动尝试 + id 验证）；GPU 显存无数据源（dumpsys gpu 无 Memory snapshot + debugfs 不存在）
- **SS3 QNX 通道细节**：telnet 172.31.101.52 root 免密；写 /dev/kgsl-control 开统计（gpu_set_log_level 4 + gpubusystats + gpu_per_process_busy）；slog2info -W（-w 会回放历史）；grep kgsl 挡 VHAL 刷屏；login:/# 提示符无换行须逐字节读；统计周期支持 50ms（clamp 100ms-1s 跟 interval）
- **code review 两轮修复 30+ 项**：最关键的三个是（1）qnx-host 双 OnceLock 静默失效（模块级 static 修）；（2）FPS None 基线全缓冲误计（127 帧全算新帧→虚高数倍，改为只计窗口时长内的帧）；（3）agent 重扫 retain 先于 per-pid 检测导致 exit 事件丢失（重扫补发 exit+清理 4 张 map）
- **死代码删除**：xperf-core 的 Sampler/sample_cpu/sample_memory/FpsPidState/get_all_processes 全删（与 agent 路径行为已分叉，留着误导）；保留 ThreadCpuInfo/MemoryDetails/FpsTimeSeriesData/PidStats/SampleEvent 协议类型。core 2600→1044 行
- **易踩坑**：`timeout cmd | head` 会让输出重复块（head 关管道后工具重复捕获），重定向文件即正常——非代码 bug；Tauri 非 async 命令阻塞主线程（list_packages/export_csv 已改 async）；`&name[..15]` 多字节 UTF-8 截断 panic（用 chars().take()）

### 遗留问题

见 WORKSPACE.md E 节。下轮优先：agent 单文件拆分（src 布局已就位，拆完须真机全指标回归）→ perfetto 深挖 → simpleperf。

---

## 2026-09-02（二）— B 类指标全覆盖：CPU 频率/温度/GPU/IO/网络

**任务线**：WORKSPACE.md B 类四项全部落地（agent+协议+CLI+GUI）。

### 完成内容（commits）

| commit | 内容 |
|--------|------|
| b732d55 | B 类五项采样：agent --freq/--thermal/--gpu/--io/--net + MetricFlags 重构 + CLI（打印/流式 CSV/退出图表）+ GUI（5 勾选框+折线图+导出扩展） |
| d811f73 | --gpu 保底路径：dumpsys gpu 每 PID GPU 显存 |
| 5cff92e | --gpu 新增 QNX 通道：hypervisor 平台（SS3/8295）真 GPU 利用率+频率+每进程 busy |

### 关键结论与基线

- **真机数据源普查（SS3/8295 车机）**：
  - `/sys/class/thermal`、`/sys/class/hwmon` **不存在** → 温度走 `dumpsys thermalservice`（限频 ≥2s），但本机是 test HAL 假数据（恒定 30.8°C），代码按标准接口实现，真手机有效
  - **GPU 由 QNX host 管理**（GVM 内无 kgsl sysfs/设备节点、ftrace 无 kgsl 事件、dma_fence 3s 0 事件、perfetto gpu.counters/gpu.memory 数据源注册但产 0 数据）→ 按平台文档走 QNX 侧：telnet 172.31.101.52（root 免密）→ /dev/kgsl-control 开统计 → slog2info 流。**真 GPU 数据已打通**：svm busy ~14.4%、系统 busy 16.8%/util 13.4% @ 506/635MHz，1/s 稳定
  - **per-app 网络无源**：qtaguid 不存在、eBPF maps 不便读、`/proc/<pid>/net/dev` 与整机一致（共享 netns）、被测包 uid=1000 系统聚合无意义 → `--net` 为整机口径（聚合物理口，排除 lo/sit/tun/gre/dummy/vti/ip6*），如实标注
- **QNX 通道踩坑**（已写入 CLAUDE.md）：login/# 提示符无换行须逐字节读；子进程 stdin drop 即 EOF；slog2info -w 回放历史（用 -W）；必须 grep kgsl 挡 VHAL 刷屏
- **协议**：hello 带 `maxkhz`；gpu 事件 QNX 路径带 util/maxmhz；gpuproc 进程行按 comm 归因 Android PID
- **重构**：`spawn_agent`/`reconnect_agent` 的逐 bool 参数收敛为 `MetricFlags` 结构体（8 指标开关），CLI/GUI 共用
- **验证**：50ms 间隔六指标同采 **0 overrun**；89 测试全绿、clippy 零警告
- **注意**：`timeout N cmd | head` 组合会让输出出现重复块（head 关闭管道后工具重复捕获），重定向文件即正常——非代码 bug

### 遗留问题

- 无（B 类清零）。下一步候选：C 类（perfetto 深挖 / simpleperf / 阈值告警 / 事件打点 / 冷启动）或 D 类决策（xperf-core 轮询参考实现去留）。

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
