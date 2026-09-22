# GUI 回归与压力测试计划（UI 全覆盖）

> 对应 WORKSPACE 待办「GUI 完整回归与压力测试」。回归部分见上（已完成，
> Mac/hppc 双环境全绿）；压力测试 2026-09-22 交付 `scripts/gui_tests/stress.py`。
> 驱动方式：`scripts/gui_tests/harness.py`（debug API 客户端，鉴权 token 自动发现）+
> `scripts/gui_tests/regression.py`（场景组）/ `stress.py`（压力场景）。不依赖
> 坐标模拟/AX 树——全部走 `xperf-gui/src/debugsrv.rs` 的可编程接口
> （设计见 `DESIGN-gui-debug.md`）。

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
| hppc AppImage（v0.3.1 发布产物） | 2026-09-22 实测：从 package registry 下载的 `xperf-v0.3.1-linux-x86_64-gui.AppImage`，FUSE 直跑 | 本机 adb（SS3+SS2MAX+SS4） | 发布二进制等效性——**Ubuntu 24.04**（`apparmor_restrict_unprivileged_userns=1`）顺带验证 v0.3.1 条件式沙箱 hook 生效（`/proc/<pid>/environ` 有 `WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1`、WebProcess 起、点击链路全通）；抓出 #10 发布产物缺陷。22.04 与「Linux 上的 SSH 远程模式」两格仍空（kong 侧设备未挂载，见 WORKSPACE K1/K2） |
| kong AppImage（CI 产物，含 A 节修复） | **22.04 覆盖已补齐**（2026-09-22 深夜，`xvfb-run -a` 本地模式） | 本机 adb（SS2PRO `ac889a71b1f` + SS4 桥接 `localhost:5559`） | **g1-g8 总计 FAIL 0**（g3 14/14+SKIP2、g4 16/16+SKIP 隧道）；22.04 无 userns 限制 → 条件式沙箱 hook 不触发（沙箱保留），与 hppc 24.04 互补；**火焰图在第二台非构建机 fresh+reuse 双路径出 3.6MB HTML**（A 节修复跨机验证） |
| hppc GUI `--remote kong`（Linux SSH 远程模式） | 直编 main 二进制 | SSH 隧道 → kong 的 adb server（SS2PRO + 桥接 SS4） | **g1-g8 全绿**（g1 29/29、g3 15/15、g4 16/16 含「杀 master→重建隧道→恢复采样」）；同时是「SSH + SS4 桥接」组合的首次覆盖；抓出 #12 传输切换设备缓存缺陷 |

同一套 `regression.py` 三环境通用（脚本在 GUI 宿主机上跑，读本地发现文件）；
`adb_on_remote` 在 ssh 模式经 hop#1 宿主执行（非交互 ssh 无 PATH，自动探测
`~/Android/Sdk/platform-tools/adb`，与 GUI 预检同套路）。**环境参数化**：
`XPERF_IT_PACKAGE`（kong 无 gltf viewer，用 `com.google.android.filament.hellotriangle`）、
`XPERF_TEST_EXTRA_SERIALS`（逗号分隔副设备，默认按 hppc 机队）、`XPERF_TEST_SSH`
（ssh 模式宿主，仅 G4 断连注入用）。**覆盖缺口（如实记录）**：kong→hppc 无密钥认证
（也无凭证注入渠道），AppImage 以本地模式测——「Linux 上的 SSH 远程模式」未覆盖
（已消解：2026-09-22 深夜 hppc `--remote kong` 与 kong 本机两条覆盖均已跑绿，见环境矩阵）。

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

8. **进程级采样的 macOS 陷阱（2026-09-22 压测实测）**：①BSD ps 多 pid 裸列表
   语义错乱（返回行数多于请求 pid）——必须 `-p a,b,c` 逗号连接（harness
   `proc_rss_cpu` 已修）；②WKWebView 的 WebKit 子进程是 XPC 服务挂 launchd
   （ppid=1、无 root 读不了 responsible），归因只能按「与 GUI 启动时刻相近」
   时间窗匹配（stress `webkit_pids_for_gui`）；③崩溃重 spawn 的 WebContent
   会逃逸时间窗归因——由 debug API 失联计数互补兜底。

9. **WebKit 唯一文本串无界驻留（macOS 平台缺陷，2026-09-22 压测定位+根治）**：
   WebCore 对**渲染/测量过的唯一字符串**有两层无淘汰 hash 驻留——fillText
   ~74KB/串（文本绘制缓存）+ measureText ~6.3KB/串（FontCascade 宽度缓存）。
   图表刻度标签原本每秒产新串（Y 轴连续浮点、X 轴 HH:MM:SS + tickStep 随窗口
   连续变），9 图 × ~7 帧/s ⇒ WebContent RSS ~3.7MB/min 线性不收敛（45min
   浸泡不平台化）。**修复** = 压灭唯一键产出：Y 轴上限 `niceCeil` 档位吸附
   （1/1.5/2/2.5/3/4/5/7.5×10^n）+ X 轴 `niceTimeStep` 时间档位 + tick 对齐
   绝对时间网格——实测唯一串 90s 450→21，dirty 内存持平、`phys_footprint`
   恒定 133MB、RSS 预热后平在 120MB（footprint 双轨 6min 验证）。
   勘察方法备忘：vmmap 分区定位（WebKit Malloc dirty 增长 vs owned unmapped
   graphics）→ 操作级二分（禁 fillText 0.12MB/min / 时间标签固定 0.11 /
   离屏唯一串 10/s 反向放大 43.65）→ fillText 包装器计数唯一串。
   **残余（非泄漏）**：修复后首分钟合成器 IOSurface 填池（purgeable，OS 压力
   可回收、不计 phys_footprint），ps RSS 判据须取后半程斜率（s1 已按此）。

10. **发布产物烤编译期路径（真缺陷，2026-09-22 hppc AppImage g3 抓出）**：火焰图
    脚本目录 `xperf-core/src/simpleperf.rs::scripts_dir()` = `env!("CARGO_MANIFEST_DIR")`
    + `simpleperf_scripts`，把**构建机目录**烤进二进制——CI 产物指向容器内
    `/builds/ligraphic/xperf/xperf-core/...`（用户机不存在且不可写，AOSP 引导下载同样
    失败），本机 `release-macos.sh` 产物烤成维护者仓库路径（**在构建机上恰好可用，
    本机/直编测试永远测不出**）。症状：状态栏「打开火焰图失败: 创建 <path>.dl-tmp
    失败」+ g3 判据「火焰图 HTML 产物」空列表。**已修**（解析链 + 随包分发 + bundle 质量门，
    见 WORKSPACE A 节）；修后在同一条 CI 产物上复验又抓出**第二层**：AppImage 的 AppRun
    （linuxdeploy python 插件）设 `PYTHONHOME=$APPDIR/usr/` + `PYTHONPATH=.../share/pyshared/`，
    宿主 `python3` 子进程继承 → `ModuleNotFoundError: No module named 'encodings'`（状态栏
    「report_html.py 生成失败」）——`python3_command()` 剥掉这两个变量后 g3 15/15 全绿。
    **纪律性结论**：① 凡 `env!("CARGO_MANIFEST_DIR")`/`concat!(env!(...))` 出现在运行时
    路径解析处，都必须在**发布产物 + 干净机器**上验一次，直编环境不构成证据；
    ② 发布产物里起的**宿主子进程**（python3/curl/xdg-open/trace_processor）会继承 bundle
    的打包环境（`PYTHONHOME`/`LD_LIBRARY_PATH`/`PATH`），排障先看 `/proc/<gui pid>/environ`
    再复现，别先怀疑脚本或数据本身。

11. **旧版产物上「通过」的用例不等于回归证据（2026-09-22 同轮）**：v0.3.1 tag（
    `80c2968`，9-20）不含 9-21/9-22 的修复，但本轮 g4「start 双击仅启动一次」与
    g3「连点不炸（冷却保护）」照样 PASS——前者是防重入修复缺失下的偶发通过（本机
    invoke 往返快，双击的第二击落在 disabled 之后），后者判据只查 GUI 存活不查新开
    标签页数（连点开 3 页由压测 s6 的 diag 计数才测得）。**发布产物回归要跑 s6 类
    带计数的判据**，或把这两条判据升级为可判定形态（候补）。

12. **传输切换后设备列表停在旧机队（真缺陷，2026-09-22 K1 抓出并修）**：`/api/status` 的 devices 读热插拔监视器的 3s 快照缓存（#压测教训：adb 卡死时活查询会挂死 handler），但`connect_remote` 切换传输时既不作废也不回填该缓存，且切换瞬间的枚举失败被监视线程`Err(_) => continue` 静默吞掉 → 「切回本机」后设备 tab/侧栏远程区块仍显示远程机队 >15s（手工探测在干净态 3s 内翻转，只在切换窗口重现）。修 `dbef7fb`：切换前置空缓存、两条成功路径用新枚举 seed；连续枚举失败首次与每 10 次经 `utils::diag` 留痕。**教训**：任何「读缓存当后端真相」的路径，在改变真相的那一刻必须显式失效/回填。

13. **判据的环境门槛要显式建模（2026-09-22 K1/K2b 四条假 FAIL 的共性）**：① scrcpy 只查「装没装」不够——Ubuntu 22.04 apt 是 1.21，本工具按 4.x 设计，现按 `--version` 主版本 <2 SKIP；② trace 分析依赖 `trace_processor`（本地缓存→PATH→get.perfetto.dev 引导下载），离线机产品如实报「分析失败 + trace 已保存」是正确行为，判据须 SKIP 而非 FAIL（产物判据保留）；③ 慢机首轮火焰图渲染分钟级，判据改「产物 HTML 出现即通过」双轨；④ 并行采样判据必须自建前置（该机装了测试包 + 用例自己点「打开应用」），不能假设「用户已经开着它」。

## 压力测试（2026-09-22 交付）

```bash
# 前置同 regression.py（GUI 已启动/设备在线/测试应用已装）；全程 ~20min
python3 scripts/gui_tests/stress.py                       # 全部场景 s1..s7
python3 scripts/gui_tests/stress.py --scenario s1,s7      # 指定场景
python3 scripts/gui_tests/stress.py --s1-minutes 45       # 长浸泡
```

场景与判据（顺序执行，采样自 s1 起贯穿全程——叠加负载即压力）：

| 场景 | 内容 | 判据要点 |
|------|------|---------|
| s1 | 长时采样（默认 10min @1s） | 无 60s 停滞窗；DOM 恒定；**唯一文本串 <300**（WebKit 驻留泄漏的直接签名，稳态 ~140/10min、泄漏态 3000+）；series 增长；RSS 斜率信息项（≥30min 长浸泡才硬判 <1MB/min 平台验证——短窗预热 2-5MB/min 波动） |
| s2 | 50ms 高频数据（3min） | series 高速增长（~20 点/s/PID）；高频绘制下悬停读数可用 |
| s3 | logcat 洪泛（全机 V + 设备端 `log` 循环注入） | ring buffer 封顶 2000；隐藏段 buf 冻结（后端暂停事件）；切回补发（洪泛序号前进——序号跨调用单调防回绕） |
| s4 | trace 15s + stack 15s 并发（采样中） | 录制期间采样节拍不饿死；两报告产出 |
| s5 | 三机并行 + 0.4s×150 快切设备页 | switch 无错乱；各设备 series 隔离增长 |
| s6 | open-perf 按钮连点 ×10 | 首击后即禁用（防抖）；**连点仅开一页**（diag 计数差==1；冷却 5s 锚完成时刻——600ms 锚点击时刻实测 burst 开 3 页）；GUI 存活 |
| s7 | adb server SIGSTOP 45s（仅 ssh 模式） | 冻结期间 series 静止；恢复后看门狗→重连→数据流自愈 |

监测：全程 5s 粒度资源曲线落 JSONL（`/tmp/xperf-gui-stress-<ts>.jsonl`）——
GUI/WebKit 进程 RSS+CPU、DOM 节点数、series 总点数、debug API 可达性、
隧道/设备数快照；结束分段汇总基线表。WebKit 子进程归因：Linux 按 ppid；
macOS WKWebView XPC 挂 launchd，按 GUI 启动时刻窗口（±5s/90s）匹配。

压测产出的产品修复（2026-09-22，`feature/gui-stress-test`）：
- agent 事件流**入向静止看门狗**（半开信道自愈，s7 回归守卫）
- `/api/status` 设备列表读监视器缓存（adb 卡死不再拖垮调试接口）
- GUI 命令入口隧道自愈（空闲期 master 死亡后 adb 打死转发口挂起）
- 图表刻度标签档位化（WebKit 唯一文本串驻留泄漏根治，见 #9）
- open-perf/stack 冷却 5s 锚完成时刻（600ms 点击锚 burst 连点开 3 页）
- CloseRequested/隧道死亡边沿 diag 留痕
