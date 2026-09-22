# GUI 回归与压力测试计划（UI 全覆盖）

> 对应 WORKSPACE 待办「GUI 完整回归与压力测试」的 UI 覆盖部分（压力测试后置到独立会话）。
> 驱动方式：`scripts/gui_tests/harness.py`（debug API 客户端，鉴权 token 自动发现）+
> `scripts/gui_tests/regression.py`（场景组）。不依赖坐标模拟/AX 树——全部走
> `xperf-gui/src/debugsrv.rs` 的可编程接口（设计见 `DESIGN-gui-debug.md`）。

## 运行方式

```bash
# 前置：GUI 已启动（推荐 setsid nohup 脱离会话，见 CLAUDE.md SIGHUP 教训）、
# 目标设备在线、测试应用已安装（默认 gltf viewer）
python3 scripts/gui_tests/regression.py                       # 全部组
python3 scripts/gui_tests/regression.py --group g2,g5         # 指定组
python3 scripts/gui_tests/regression.py --group g8            # 破坏性（可能挂死 GUI）
python3 scripts/gui_tests/harness.py --check                  # harness 自检
```

- `--serial`：主测试设备（默认 SS3 `6eb792dfb0f`）。G4 会自动纳入同链路其余设备。
- 运行期间勿动物理鼠标/键盘（合成 hover 与真实输入竞争）。
- G1 会改写 `~/.config/xperf/remotes.json`（跑前备份、跑后还原，断言失败也还原）。
- G6 会**真实上传**一个 `[gui-test]` GitLab issue（验证后手动关闭）。
- G8 杀 WebKit 子进程——可能复现「窗口在、点击无响应」挂死，跑完可能需重启 GUI。

## 覆盖矩阵

| 组 | 面 | 覆盖点 | 判据要点 |
|----|----|--------|---------|
| g1 | 连接 | remotes.json 读写/ssh config 导入（`list_ssh_host_details` 展开、已保存过滤、选中填充）/表单保存并连接/临时连接零保存/侧栏 adb upsert 持久化（prev 合并）/切回本机/密码认证失败分类（表单预填重开）/网关过滤 | 后端 `list_remotes` + 落盘 JSON 双侧核验；失败保持 local |
| g2 | 采样 | 刷新包列表/打开应用（冷启动面板）/开始监控/后端会话/series 增长/liveData/hover tooltip（rAF 轮询）/`/api/series` tail+at 二分/导出 CSV/指标勾选重启（同目录 append、图表不重置）/峰值面板/停止冻结/停止后导出 | 会话目录复用、CSV 行数增长、frozen series |
| g3 | 深挖与捕获 | trace 5s（录制中→报告分段→浏览器按钮解锁→.pftrace 产物）/Perfetto UI 打开+连点冷却/stack 5s（三视图→.data 产物）/火焰图 HTML/截屏（PNG 魔数）/录屏（封盘+mp4）/镜像启停 | `/api/status` sessions 后端真相 + 落盘产物核验 |
| g4 | 多设备与故障 | 三机并行采样（series 隔离）/切设备页数据不串/start 双击竞态/10 次子 tab 快切/SS4 `adb disconnect` 断连自愈（bridge refresh 重连+采样恢复）/杀 ControlMaster 隧道重建（指数退避后设备回来）/全停 | 事件 payload 带 serial 分发正确性 |
| g5 | 日志 | 开始抓取/出行/落盘路径/按包→全机热切换/可见性暂停（隐藏 tail 冻结）/切回补发（pending 尾部保留）/级别热切换（respawn 标记 level=E）/文本过滤（自适应 token）/非法正则秒死中止/停止+标记行核验 | respawn 标记行**只落盘不进前端**——口径核验读文件 |
| g6 | 基线与反馈 | 短采样→保存基线（JSON 落盘）/二次采样→对比报告面板/`gitlab_auth_status`/反馈浮层→真实上传→issue URL→API 核验标题 | 基线文件跑前备份跑后还原 |
| g8 | WebKit 生命周期 | 杀单个 WebKit 子进程 SIGKILL→debug API/UI 交互存活度（如实记录） | 观察性结论，见「已知发现」 |

## 环境矩阵

| 环境 | GUI 形态 | 传输 | 观察项 |
|------|---------|------|--------|
| Mac 本机 | 直编 `target/release/xperf-gui` | SSH 远程（hppc 挂真机） | 主验证环境（本文档基线） |
| hppc 直编 | Linux 原生 GUI | 本机 adb | WebKit 进程树形态、X11 下 `resize_default` |
| kong AppImage | v0.3.1 发布产物 | 本机 adb（kong 自挂 SS2PRO + SS4 桥接） | 发布二进制等效性（v0.3.1 早于本会话修复，capabilities/async 修复不在内属预期） |

同一套 `regression.py` 三环境通用（脚本在 GUI 宿主机上跑，读本地发现文件）；
`adb_on_remote` 在 ssh 模式经 hop#1 宿主执行（非交互 ssh 无 PATH，自动探测
`~/Android/Sdk/platform-tools/adb`，与 GUI 预检同套路）。**环境参数化**：
`XPERF_IT_PACKAGE`（kong 无 gltf viewer，用 `com.google.android.filament.hellotriangle`）、
`XPERF_TEST_EXTRA_SERIALS`（逗号分隔副设备，默认按 hppc 机队）、`XPERF_TEST_SSH`
（ssh 模式宿主，仅 G4 断连注入用）。**覆盖缺口（如实记录）**：kong→hppc 无密钥认证
（也无凭证注入渠道），AppImage 以本地模式测——「Linux 上的 SSH 远程模式」未覆盖
（Mac --remote 已覆盖 ssh 通道逻辑本身，Linux 侧差异仅在 webkit2gtk 渲染，风险低）。

## 已知发现（本计划实施过程）

1. **start 双击竞态（已修复）**：`start()` 的 disabled 要等 `invoke` 返回才置位，
   双击的第二击落在 pending 窗口内会双发 `start_sampling` → 后端报「已在监控中」
   弹原生错误框，且第二击的 `resetSessionData()` 已清掉首次的图表数据。
   修复：`main.js start()` 加 `_startPending` 防重入（g4 覆盖回归）。
2. **open_perfetto_ui 冻结 GUI（已修复）**：同步 Tauri command 跑在主线程，
   首用镜像 Perfetto UI（headless Chrome 抓资源清单 + 逐个下载，实测 ~1 分钟）
   期间整个 GUI 无响应——webview 事件循环全停，debug API 前端应答全部超时，
   与「GUI 已挂」症状一致（2026-09-21 g3 实测锁定）。修复：改 async +
   `spawn_blocking`（与 `submit_feedback` 同模式，`xperf-gui/src/main.rs`）。
   g3 的 open-perf 步骤即回归判据（点击后 30s 内状态栏出结果且 debug API 不失联）。
3. **respawn 标记行只落盘不进前端**：logcat 热切换的口径标记不在 ring buffer 里，
   测试判据走落盘文件（产品行为本身合理——标记是文件侧续写证据）。
4. **测试脚本的密码认证注入**：GUI 从带 SSH agent 的 shell 启动时密钥认证先于密码
   路径，密码失败用 `nobody@hppc`（agent 密钥对该用户无授权）才能真测到密码分支。
5. **G8 预期**：WebKit 子进程被 SIGKILL 后主进程不自动恢复（与 SIGHUP 症状同构），
   属已知边界，如实记录不判产品缺陷。
6. **全遮挡/锁屏冻结 rAF（非缺陷，测试防护）**：macOS 对被完全遮挡（或锁屏——
   `lsappinfo` 见 loginwindow in front）的 NSWindow 暂停 WebKit 渲染——
   `requestAnimationFrame` 完全冻结（`document.visibilityState=hidden`），但
   `setInterval` 照常跑（图表绘制/plot 几何照常更新）。症状：悬停读数的 rAF
   合帧永不执行，hover tooltip 确定性无读数（两轮假 FAIL，2026-09-21 锁定；
   现场特征=canvas rect 非零但 `_hoverRaf` 悬停后不变、cssW=0——设备页首次
   resize 也是 rAF 门控）。非产品缺陷（遮挡暂停渲染是 macOS 正确行为，真实
   用户悬停时窗口必然可见）。测试防护（三层）：① capabilities 放行
   `core:window:allow-set-focus`，`ensure_visible()` 在 hover 前先 setFocus——
   覆盖「仅被其他窗口遮挡」场景；② 锁屏场景软件无法恢复（也不应碰），hover
   用例经 `Checker.skip()` 降级为 SKIP（环境受限，单列不计失败）；③ 无人值守
   回归（如过夜跑）出现 hover SKIP 属预期，解锁后重跑即真覆盖。注意：AX/
   System Events 在 Claude Code shell 上下文枚举不到任何窗口（无 Accessibility
   权限），`set frontmost` 也会无声失败——窗口状态以 webview 内
   `document.visibilityState` 与 Tauri window API 为准，前台判定用 `lsappinfo`。

7. **Linux dev 机 agent 自动构建的 cargo 版本地板（环境级，2026-09-21 hppc 实测）**：
   `ensure_agent_built` 触发重建时，cargo 经 `~/.cargo/bin/cargo`（rustup shim）走
   **默认工具链**——hppc 默认 1.75 解析不了 lockfile v4（`lock file version 4
   requires -Znext-lockfile-bump`），报错文案却归因为「需要 Android NDK」具有误导性。
   规避：GUI 启动环境带 `RUSTUP_TOOLCHAIN=<新版>`（hppc 用 1.97.0——还须装
   `aarch64-linux-android` target；1.93.0 无该 target 同样失败），或 `rustup default`
   升级。
   顺带确认：`agent_binary_needs_build` 按源码 mtime 判定正确触发（仓库含 9-11 后
   agent v9 源码 > 9-7 旧产物）。**候选产品改进（未做）**：构建失败时把 cargo 的
   stderr 尾行附进错误信息，替代固定 NDK 文案。

## 压力测试（后置）

harness 已备 `proc_rss_cpu`/`webkit_children`（进程级 CPU/RSS 采样），供后续会话：
长时采样、logcat 洪泛、trace/stack 并发、图表高频数据、多设备页切换、浏览器按钮
连点、CPU/内存/DOM 增长曲线。产出基线记录进 SESSION.md。
