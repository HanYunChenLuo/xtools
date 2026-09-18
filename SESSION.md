# SESSION.md — 会话历史

> **约定**：每个会话结束前追加一条记录（最新在最上）。格式：
> 日期 / 任务目标 / 完成内容（commit 列表）/ 关键结论与基线 / 遗留问题。
> 待办事项（backlog）在 `WORKSPACE.md` 维护，本文件只做历史追溯。
> 新会话开始时可先读本文件了解近期上下文。

## 2026-09-18（傍晚）：J 节会话 2——CLI `--duration N` 限时采样

**任务**：WORKSPACE J 节会话 2——纯采样只能 Ctrl-C，agent 的 shell 调用必须有界（macOS 无 timeout），加 `--duration <SECONDS>`。

**commit**：`aac08d4`（feat：--duration + stop_window 纯函数 + 4 单测）、`75ada1c`（fix：限时打印改生效窗口值）、`d7bbf45`（--no-ff 合 main）。

**关键结论**：
- 实现极小：现有 `stop_after` 限时通路（`--trace/--stack/--record` 同窗口语义）已完备，`--duration` 并入窗口计算即可——抽纯函数 `stop_window(duration, trace, stack, record)` 取最长者（采样窗口覆盖所有录制）；启动打印生效窗口（max 后的值）；help 注明「纯深挖/纯录屏/镜像-only 模式自带边界不生效」
- **真机回归**（--remote hppc SS3 gltf viewer，全部 exit 0）：`--cpu --memory --duration 8`（8 样本+CSV+图表+峰值汇总）/ `--duration 3 --trace 6`（窗口取 6s，30MB trace+SQL 报告）/ `--duration 3 --stack 6`（窗口取 6s，.data+三视图）/ `--duration 5 --threshold cpu>5`（告警×4+验证报告）/ `--save-baseline`→`--compare-baseline`（保存+持平）/ 无 duration Ctrl-C 回归不变
- **测试**：stop_window 4 单测；全量 core 162+8 / CLI 9 / GUI 11 绿，clippy + cargo doc 零警告
- **踩坑**：首轮回归被测应用未运行（pidof 空）→ agent 无事件源，到点正常退出但零样本零 CSV（非缺陷，采样语义本就如此）；真机回归前先确认应用在跑
- macOS 数据根是 `$TMPDIR/xperf` 不是 `/tmp/xperf`（验证落盘别找错地方）

**遗留**：J 节会话 3（AGENTS.md + SKILL.md + README agent 节，含会话 1 review 两顺手项）；会话 4（summary.json，待用户拍板）；会话 5（整体 review + 三机回归）。

## 2026-09-18（下午）：J 节排期 + 会话 1——tarball CLI agent 解析链修复

**任务**：「向 Claude Code/Codex 等 agent 提供 xperf 能力」的代码与文档 review → WORKSPACE 新增 J 节排期（4 实施会话 + 1 review/真机回归会话）→ 会话 1 实施：修复 release tarball 安装的 xperf-cli 在非构建机找不到捆绑 agent（P0 阻断缺陷）。

**commit**：`64409d4`（fix，`fix/cli-agent-resolution` 合 main `3937993`）、`ead68be`（WORKSPACE J 节会话 1 勾销）。

**关键结论**：
- **Review 总判**：现状对 agent 不可用——① P0 缺陷：CLI agent 解析仅编译期 `CARGO_MANIFEST_DIR` 路径 + `cargo build`（均只构建机有效；v0.2.0 验证在构建机做被掩盖）；② P0 缺口：采样无 `--duration`（agent 的 shell 调用必须有界）；③ 文档载体缺失（无 AGENTS.md/SKILL.md，CLAUDE.md 是维护者向）。已具备基础：CLI 全非交互、结果全落盘、`--threshold`/基线即断言接口
- **会话 1 修复**：`resolve_agent_binary()` 三级链——`XPERF_AGENT_BIN` env（显式覆盖，指向不存在报错**不回退**）→ exe 旁 `agent/xperf-agent` sibling（tarball 布局）→ 开发检出 `ensure_agent_built()` 兜底（`workspace_root` 有 Cargo.toml 才走）；全落空报错指引布局。纯函数 `pick_agent_binary` 锁顺序，替换 CLI 预热/`ensure_daemon` 重推/`reconnect` 三处 None 分支，GUI 资源路径不动
- **真机验证**（--remote hppc SS3）：掩蔽 workspace agent 二进制 A/B——tarball CLI 从 /tmp 采样+trace 全通且**未触发 cargo 重建**（sibling 生效实证）；dev 回归、env 错误路径 exit 1；单测 5 新用例，全量 162+5+11 绿，clippy/doc 零警告
- **会话 1 review（二次走查）**：无严重问题。观察项 2 个并入会话 3：① `XPERF_AGENT_BIN=""` 空串按 Explicit 报空白路径（应视同未设置，一行 filter + 单测）；② CLAUDE.md「agent 部署」节补解析链一句。残余风险（接受）：开发机 `target/release/agent/` 意外遮蔽（env 可覆盖）；tarball mtime 各机不同首跑必推一次
- **方法论**：zsh 下 `cmd | tail; echo $?` 取的是 tail 退出码——验退出码必须 `pipestatus[1]` 或直接重定向（本会话初测误判 0，复测更正）

**遗留**：J 节会话 2（`--duration N`）新开会话进行；会话 3/4/5 按排期。

## 2026-09-18（上午）：OAuth 竞态单测 + GUI 按钮物理目验闭环

**任务**：两项遗留收口——① OAuth 并发刷新竞态恢复逻辑补单测（原记录「需 mock HTTP 层」）；② GUI 表单/按钮物理点击目验（AX 冻结遗留项）。

**commit**（`test/oauth-refresh-race`）：`0998475`（oauth 刷新动作闭包注入重构 + 8 竞态单测）。

**关键结论**：
- **① OAuth 竞态单测**：不引 mock HTTP 层——`current_access_token` 抽出 `current_access_token_with(path, refresh: &mut dyn FnMut)`（刷新动作闭包注入、store 路径显式传入并行安全，公共 API 不变）。8 测试锁定：未临期不刷/无态 None/刷新轮换落盘/无竞态 invalid_grant 清态/**竞态三变体**（赢家已轮换未过期→复用；赢家也临期→新 refresh_token 二刷；二刷仍拒→清态）/网络故障不清态。core 157+8 全绿（oauth 17）
- **② GUI 物理目验（键盘焦点链，全程截图 + diag 佐证，无一处 AX 点击）**：
  - **SSH 远程表单全链**：Tab×2 到「＋」Space 开表单 → 逐字段填写（name=xperftest/host=10.122.85.252/user=tester/ssh 端口 2299/密码）→ Space 取消勾选「保存到 ssh config」→ Tab×3 回车「保存并连接」→ **连接成功**（状态栏「已连接远程: tester@10.122.85.252」；diag：master spawn 密码 askpass → 远端预检 3.1s → probe → 设备枚举，总 3.29s；remotes.json 精确落盘 user@host 形式 + ssh_port 2299；**ssh config 零污染**）。测试靶机：hppc docker linuxserver/openssh-server @2299（tester/test12345，aliyun 源装 android-tools + AllowTcpForwarding yes——dl-cdn 内网不通）
  - **问题反馈按钮 → 浮层**：Tab×3 Space 开浮层，登出态（移走 oauth store）显示「未配置凭证」警示 + 「登录 GitLab」按钮可见
  - **登录 GitLab 按钮**：Shift-Tab Space 点击 → 浏览器打开授权页（xperf → 汪尽涵 @wangjinhan）+ diag `oauth: 等待浏览器回调（39859）`；恢复 store 重开浮层 → 「将以 汪尽涵（@wangjinhan）身份创建 issue」+ 退出登录按钮；取消按钮关闭浮层
  - 测试残留全清：GUI 退出/docker 容器移除/remotes.json 删除/known_hosts 测试条目 ssh-keygen -R；截图证据 `/tmp/gui_*.png`
- **AX/键盘目验方法论补充**：① **同名进程劫持**——周三安装的 DMG 版 `/Applications/xperf-gui.app` 还在跑（旧前端），System Events `process "xperf-gui"` 全被它截走（旧表单/副屏窗口/焦点错乱全是它）；目验前必查 `ps` 排除同名实例；② **keystroke 时序**——Tab 后 <0.5s 立即输入会丢步（本次实撞：Tab 丢失 → 相邻两字段串值、Space 落空），0.5~0.6s 延迟 + 每轮区域截图验证（`screencapture -R` 裁表单区，全屏转录噪声大）后全对；③ `click at {x,y}` 负坐标（副屏）行为异常会误击主屏其他应用窗口——先 `set position of window 1` 钉到主屏正坐标；④ Cmd+W 关浏览器标签后 Chrome 仍占前台，后续 keystroke 全进浏览器——每步后显式 `set frontmost`

**遗留**：无（两项原遗留全部闭环；v0.2.2 发版待用户排期）。

**追记（同日）**：用户指出代码不应含个人主机名 hppc——盘点分类后修用户可见面 5 处（README×2 示例/CLI `--remote` help/GUI 表单占位符/`RemoteConfig.name` doc → 通用别名 `myserver`，合 main `29c7cb6`）；保留 `#[ignore]` 集成测试（维护者真机回归资产，他人永不执行）与内部文档（SESSION/WORKSPACE/CLAUDE/DESIGN 工作笔记）；`.claude/settings.local.json` 未被 git 跟踪无泄漏。

**追记2（同日，多人维护启动）**：上述「集成测试保留硬编码」的判断随多人维护失效 → **参数化**：core `utils.rs` 新增 `#[cfg(test)] it_env` 模块（`XPERF_IT_SSH_HOST` 必填/`XPERF_IT_SSH_ADB` 可选默认自动探测/`XPERF_IT_DEVICE` 必填（设备类）/`XPERF_IT_PACKAGE` 可选默认 gltf），5 个集成测试（transport×3/agent/logcat）改注入 + 更名去 `_hppc` 后缀 + `#[ignore]` 原因写明变量；未设必填变量 panic 给指引（显式 `--ignored` 调起，静默跳过会误读为通过）。纯单测 fixture 字符串 hppc→myserver（transport 23/utils 1/gui 9 处）。**实证**：`XPERF_IT_SSH_HOST=hppc` 跑 tunnel ✓（adb 自动探测生效）；同测试 `XPERF_IT_DEVICE=localhost:5559`（SS4）跑 logcat restart ✓——同一测试二进制纯靠环境变量换主机/设备跑通；SS3 离线期间 logcat-on-SS3 失败为环境性（getprop 空输出）。CLAUDE.md Commands 节补集成测试环境变量说明；WORKSPACE 测试行刷新（core 157+8）。源代码（含测试）至此**零 hppc**，仅内部文档保留历史记录。

**追记3（同日，文档 review）**：多维护者视角复查全部文档——CLAUDE.md 两处**指令性内容**泛化：①「git 拓扑」改「通用流程（GitLab 主远端直推 + feature/fix 分支 --no-ff）为主，wangjinhan 个人 hppc 中转拓扑降为附注」；②「发版流程」tag 推送改「GitLab 直推（wangjinhan 经 hppc 两跳，其他维护者直推）」+ macOS 资产构建者改「Mac 维护者」；顺手泛化 SSH 远程节「为什么」与远端 adb 排查提示；WORKSPACE 速览 `--remote hppc` → `<远端机>`。保留不改：历史实测记录（握手收敛基线/真机回归数据——改了失真）、SESSION/DESIGN（日志与设计记录性质）、WORKSPACE 设备清单（当前实测环境事实）。README 双语结构镜像一致（含示例逐条对齐）。**用户拍板后新增 `CONTRIBUTING.md`**（入门导引：仓库结构/环境准备（NDK≥25.1 + LFS）/构建测试/集成测试环境变量/真机测试约定（gltf viewer + --remote）/工作流约定（分支--no-ff/WORKSPACE/SESSION/doc 规则）/git 拓扑；细节指向 CLAUDE.md 防两处失同步），README 双语「构建」节互链。

## 2026-09-17（晚，续3）：GUI `--release` 开发运行误报「缺少预编译 agent」修复

**任务**：用户报 bug——`cargo run --bin xperf-gui --release` 报「GUI 发布包缺少预编译 agent/xperf-agent；请重新安装完整 GUI 包」。

**commit**（`fix/gui-release-agent-fallback`）：core `agent::workspace_root()`（编译期固化绝对路径，`agent_binary_path` 改由它拼接 + 单测）→ GUI `bundled_agent_path` 回退判定改写 → docs（CLAUDE/WORKSPACE/SESSION）。

**关键结论**：
- 根因：`f4f285d`（GUI 发布打包）把 dev 路径的 `ensure_agent_built()` 换成 `bundled_agent_path()`，回退分支以 `cfg!(debug_assertions)` 判定开发运行——`cargo run --release` 是 release profile 的**开发运行**（无打包资源），被误判为发布包缺资源
- 修复：开发判定改「编译期 workspace 是否存在」——GUI 资源缺失且构建机 workspace 在 → 回退 `ensure_agent_built`（恢复 f4f285d 之前的自动构建行为，**任意 profile**）；发布包用户机器 workspace 必不存在 → 保持缺资源报错、不触发 Cargo/NDK（发布约束不变）；顺带修文案 typo（`agent/ xperf-agent` 多空格）
- **真机端到端**（`./target/release/xperf-gui --remote hppc --device 6eb792dfb0f --package com.google.android.filament.gltf --cpu --memory --fps` 自动启动）：agent 启动（8 核 root）+ CPU/内存/FPS 三 CSV 流式落盘（CPU ~25%/PSS 646MB 含 DMA-BUF 533MB）+ 退出后 daemon 空载自杀**零残留**
- **`cargo install` 场景实测可用**：`cargo install --path xperf-gui`（前端纯静态无 node 步骤，~51s）→ `~/.cargo/bin/xperf-gui` 从 home 目录（非 workspace CWD）`--remote` 自动启动采样全通（固化绝对路径解析 agent，CPU ~26%）→ 退出干净，已卸载复原。边界推演：`--git` 安装会在 cargo checkout 内构建 agent（可用）；registry 安装（无 workspace 根）落到缺资源报错——文案「请重新安装完整 GUI 包」对该场景略不贴切（内部 GitLab 工具本就无 registry 发布，不修）
- 验证：core 149+8 全绿（含新增 `test_workspace_root_points_at_real_workspace`）、cargo doc 默认 lint 集 + core/gui missing_docs 全零
- 勘察注意：① `adb shell "pgrep -f xperf-agent"` 会**自匹配 shell 命令行**出假 pid，验证残留用 `ps -A -o PID,NAME | grep xperf`；② macOS 上 CLI/GUI 共用的 `csvstream::data_root()` = `env::temp_dir()/xperf` 落在用户级 `$TMPDIR`（/var/folders/...）而非 /tmp（Linux 才是 /tmp），验证产物别看错目录

**遗留**：无（发布包行为未动，`validate_dmg`/CI 资源校验仍兜底）。

## 2026-09-17（晚，续2）：README 展示文档 review 与重排（中文主 + 英文版）

**任务**：review README.md 等展示文档是否需要修改/优化，并提供对应英文版本。

**commit**（`docs/readme-refresh` 合 main 57c8866 + main 直接提交）：`136022d`（README 重排 + 内容更新 + 打包引用同步）→ `4caeaa7`（CHANGELOG Unreleased 补记）→ `cd63eb1`（CLI `--remote` help 文案修正）→ `f6e88d5`（语言角色对调回英文默认）。

**关键结论**：
- 英文版其实已存在（README.md=英文 / README_zh.md=中文）但两份互不链接、内容落后 → 内容全面更新后**保持 README.md=英文（默认渲染）+ README_zh.md=中文，两份互链**（初版曾重排为中文主文档，用户拍板改回英文默认、中文走链接跳转，f6e88d5 对调）；`.gitlab-ci.yml` 与 `scripts/release-macos.sh` 的打包引用同步（CLI tar 内两份 README 都随包分发）
- 标题 `XTools` 为改名前残留，统一为 `xperf`
- 功能面补齐（v0.2.1 后合 main 的）：问题反馈（`--feedback`/`--gitlab-login`）、SSH 密码认证（GUI 表单/`XPERF_SSH_PASSWORD`）、工具命令三件；SSH 段落从修复说明口吻（DMG/Finder 细节）收敛为功能描述；Usage 选项 6 项 → 分组表（30+）+ 示例扩 6 场景；**GUI 章节重写**为实际功能面（多设备并行/四子 tab/应用管理/远程连接/反馈/主题——原文只写了三张图）；输出示例中性化（SVM 为旧测试对象）；新增 MIT License 章节
- CHANGELOG `[Unreleased]` 补记：问题反馈/OAuth/SSH 密码认证 + **AppImage harfbuzz 修复**（`git merge-base --is-ancestor` 核实 d75108f **不在 v0.2.1 tag 内**，即 v0.2.1 AppImage 在 Ubuntu 22.04 仍有启动 bug，修复随下版发布）
- CLI `--remote` help「需 ssh_config 免密配置」已过时 → 更新为密码认证说明
- 验证：`cargo build -p xperf-cli --release` + `--help` 渲染确认；`cargo doc` 默认 lint 集 + 三 crate missing_docs 全零；`README_zh` 无存活引用残留（仅 SESSION 历史条目，按约定不动）

**遗留**：GitLab 网页端渲染效果留用户一眼确认；README 描述的是 main 当前态（其中问题反馈/SSH 密码未随 v0.2.1 发布，下次 tag 生效）。

## 2026-09-17（傍晚，续）：密码错误即停 + 三轮 review 修复 + 测试覆盖补齐 + 版本号统一唯一出处

**任务**：SSH 密码功能收尾 review 链 + 版本号管理优化（用户提出：一次发版要改多处，能否统一出处）。

**commit**（均直接/分支合 main）：`ed2f353`（密码认证失败立即终止重试循环）→ `02bc095`（review 三修复：stderr 字节切片 panic 隐患/表单预填兜底/文案）→ `2f7ce7a`（ssh_port 补测 + pick_free_port flake）→ `a4977a5`（版本号统一）。

**关键结论**：
- **密码错误即停**（用户拍板）：`is_auth_failure` 命中即 kill master 返回——不占服务器 MaxAuthTries 配额；网络类失败保持 3 轮重试
- **review 三修复**：diag stderr 截断 `&s[..200]` 在多字节 UTF-8 边界 panic（错误处理路径最不该崩）→ `chars().take(200)`；ssh config 来源下拉项认证失败重开表单不预填 → 兜底预填目标本身；adb 路径 placeholder 与 NewSshHost 文档矛盾 → 对齐
- **测试覆盖补齐**：`master_ssh_args` 的 ssh_port `-p` 分支此前零单测（review 发现）；`pick_free_port` 两次裸取 OS 可复用同端口（实测偶发 flake）→ 持有监听再取
- **版本号统一**：5 出处（4 crate + tauri.conf.json）人肉同步 → 根 `[workspace.package]` 唯一出处。tauri.conf.json 删 version 后 Tauri 回落到 CARGO_PKG_VERSION——**tauri-codegen-2.2.0 context.rs:273-277 源码实锚**；GUI 新测试真调 `generate_context!` 锁整条回落链；validate:tag 加「tauri.conf 手写 version 即拦」防回归；CI/scripts 5 处 sed 收敛读根。发版从改 5 处变 1 处 + CHANGELOG
- 顺带修正文档偏差：CLAUDE.md 声称 validate:tag 校验「crate 版本一致」，旧实现只查 xperf-cli——workspace 化后物理不可能不一致

**遗留**：OAuth 并发刷新竞态恢复逻辑无单测（需 mock HTTP 层，已记录）；GUI 表单物理点击目验项同前（AX 冻结）；下个 tag 发版时自然实证 DMG productVersion 回落链。

## 2026-09-17（下午）：SSH 远程支持账号+IP+密码登录 + ssh config 保存

**任务**：GUI 远程登录此前只认 ssh 别名（须预先配免密）——支持 用户名+IP+密码 直连；可选把登录信息存进 ssh config 方便复用。

**用户边界拍板（两次收紧）**：密码不落盘；**xperf 专用公钥也不写入、不动远端**（不侵入主机，安全风险）——「保存到 ssh config」最终形态 = 纯 Host 条目（HostName/User/Port 零秘密），密码主机每个新会话重输一次（表单预填缓解）。

**commit**（feature/ssh-password-bootstrap）：`88e0cb1`（core：SSH_PASSWORD 内存静态 + askpass 注入 master + 认证失败分类 + save_ssh_config_host + 5 单测）→ `cfbbbdd`（GUI 表单 + add_ssh_host + connect_remote password 参数）→ `4b1bc5f`（语义澄清 + 重开表单预填 + ssh_port + accept-new 实踩修复）。

**关键结论**：
- askpass 机制：`SSH_ASKPASS_REQUIRE=force`（OpenSSH ≥8.4）+ 静态脚本（`printf '%s\n' "$XPERF_SSH_PW"`，密码走子进程环境变量 `/proc/<pid>/environ` 仅本人可读）；**`-f` 后台化与 askpass 兼容**（认证在前台完成后才 daemonize）
- **accept-new 必须加**：首次连接新主机的 host key 确认也走 askpass 通道，回密码≠yes → ssh 永久死等（真机实踩 180s 超时定位）；accept-new 自动接受新 key，已登记 key 变更仍拒绝（MITM 防护保留）
- BatchMode 与密码互斥（前者禁掉一切询问）——密码模式单独拼参数
- linuxserver/openssh-server 镜像坑：sshd 监听 **2222** 非 22；真实配置在 `/config/sshd/sshd_config`（改 `/etc/ssh/sshd_config` 无效——进程 `-f` 指定前者），`AllowTcpForwarding no` 默认关（hop 转发 administratively prohibited）；apk 并发锁残留须 `rm /lib/apk/db/lock`
- 真机回归（docker sshd @hppc:2299，tester/test12345）：错密码 →「密码认证失败（密码错误或服务器未启用密码登录）」；正确密码 → master 建立 + 预检（装 android-tools 后）+ 设备枚举「远程后端: xperftest（在线 0 台）」全链 ✅；GUI 表单 9 字段 id 注入校验 ✅
- **worktree LFS 陷阱**：`git add -A` 把 simpleperf_scripts 的 LFS 指针异常状态（checkout 后 smudge 未完成的删除态）一并提交——`reset --soft + restore --staged + checkout --` 剔除后重新提交；教训：worktree 里 stage 前 `git status` 必看

**遗留**：GUI 表单全链点击未目验（同前 AX 冻结，字段 id/命令注册/编译校验 + core 密码链真机全通）；测试容器与 ssh config 测试别名已清理零残留。

## 2026-09-17（午）：GitLab OAuth 登录（反馈身份绑定操作者本人）

**任务**：问题反馈的 issue 作者须为操作者本人——在 PAT 之外加 OAuth 登录（浏览器授权码+PKCE，兼容 SSO/2FA）。

**commit**（feature/gitlab-oauth）：core `oauth.rs`（login/logout/status/临期刷新+9 单测）→ feedback 凭证链接入 `GitlabAuth` → CLI `--gitlab-login/--gitlab-logout` → GUI 浮层身份行+登录/退出按钮。

**关键结论**：
- **GitLab 18.5.4 CE 实测**：`/oauth/authorize` 对未登录用户先 302 到登录页，redirect_uri 校验在其后——注册 URI 无法无登录探测；`scope=api` 是覆盖「建 issue+传附件」的最小 scope（无 issue 级细粒度）；labels 不存在自动创建
- **OAuth App 必须取消勾选「Confidential」**（默认勾上！）：机密客户端换 token 要 client_secret，公共客户端+PKCE 无 secret → `invalid_client` 401。真机实踩三轮（拒绝误点、Confidential 未改、改后成功），错误路径已加可操作指引
- **access_denied 不应判死**：授权页「取消」误点后用户可回退重点 Authorize——监听器应答后须继续等到超时（初版直接退出致浏览器拒连，真机实踩修复）
- 授权等待超时 180s→600s（人工读页+登录操作实测不够）
- **真机全链**：登录成功（汪尽涵 @wangjinhan，store 0600）→ 无 PAT 环境下 `--feedback` 走 Bearer 建 [issue #2](https://gitlab.chehejia.com/ligraphic/xperf/-/issues/2)（作者本人验证）→ 篡改 `expires_at` 强制过期 → 透明刷新轮换 + [issue #3](https://gitlab.chehejia.com/ligraphic/xperf/-/issues/3) ✅
- token 交换/刷新用 `application/x-www-form-urlencoded`（`.form()`），`expires_in` 缺省 7200

**遗留**：GUI 登录按钮的全链路点击未目验（同前——AX 树冻结；core 路径已实测、命令注册编译期校验、浮层身份行 invoke 经临时自动展开实测无错误）；issue #1/#2/#3 为自测件可关闭。

## 2026-09-17：问题反馈（一键收集日志 → GitLab issue）

**任务**：WORKSPACE I 节「问题反馈」——一键收集最近 1h xperf 证据打包上传内部 GitLab issue（project 39859）。

**commit**（feature/feedback）：`ddba464`（core feedback.rs 收集+打包+上传+9 单测）→ `48783c4`（CLI --feedback）→ `7bc71e5`（GUI 按钮+浮层+submit_feedback 命令）→ `30818f9`（真机回归修复×2）。

**实施期用户拍板**（相对 WORKSPACE 原方案的变更）：
- **采集范围收敛为 xperf 自身证据**：设备 logcat 不自动采集（非工具日志），issue 正文模板引导用户手动 `adb logcat -d` 附加；排除了原方案的 logcat -t 回放与 A11/A16 兼容性验证负担
- **打包/上传禁系统命令依赖**（跨平台稳定）：tar+flate2 纯 Rust 打包、reqwest::blocking+rustls（webpki 内置证书链，CI 免装 libssl-dev）上传；未引入系统 tar/curl 调用

**关键结论**：
- token 解析：`GITLAB_TOKEN` env > `~/.config/xperf/gitlab-token`（0600 建议）；**hppc `~/.git-credentials` 的 oauth2 token 实测可直接作 PRIVATE-TOKEN 完成 uploads+issues API**（本会话真实上传即走此路径）
- GitLab `POST /projects/:id/issues` 带 `labels: "feedback"` 会自动创建不存在的 label；上传附件返回 `markdown` 字段直接嵌正文；413（实例 max_attachment_size 默认 10MB）回退 package registry PUT
- 真机回归（--remote hppc）：本地无设备收集 ✅ / SSH 远程 3 设备 agent 日志（网关正确排除）✅ / SS3 gltf 采样后会话产物收集 ✅ / token 缺失 exit 1 + 归档路径提示 ✅ / **真实上传 issue #1 + 附件回下载可解包** ✅
- **AX 新教训（重要）**：本机 WKWebView 实例的 AX 子树**冻结在启动早期快照**——递归 `UI elements of` 遍历也只能拿到快照（新插入的设备页/浮层不暴露，`AXEnhancedUserInterface` 设置被拒），但主题按钮点击后的 label 变化又可见（交互过的分支才活化）。**替代验证法：`keystroke`/`key code` 走 DOM 焦点链**——textarea `focus()` 后直接键入、Tab 序遍历按钮、Space 点击，反馈浮层全链路以此实测通过（diag 日志佐证：submit→收集→token 错误路径）。AppleScript 另一坑：`repeat with c in UI elements of el` 在 `tell System Events` 块外编译不过（"UI elements" 是其术语），须包进 tell 或用 tell 包裹的 handler
- 真机回归顺带修复：无会话产物时 diag 日志暂存父目录未创建（报「读取失败」而非「不存在」）；CLI 归档大小恒显 MB 致小包显示 0.0

**遗留**：GUI 侧栏「问题反馈」按钮的物理点击未目验（AX 树冻结所致；按钮存在与事件绑定由 DeviceSession 构造器完成佐证——三设备 packages loaded），留给用户一眼确认；issue #1 为自测件，用户查阅后可关闭。

## 2026-09-16（深夜）：AppImage Ubuntu 22.04 harfbuzz 符号崩溃修复

**任务**：修复 WORKSPACE A 节「Linux AppImage 在某台 Ubuntu 22.04 启动失败」（libpango symbol lookup error）。

**commit**（已推 hppc + li）：`4fb79b7`（fix: AppDir 注入 libharfbuzz + 重打包）→ `8db22c1`（fix: appimagetool 来源修正）→ `d75108f`（--no-ff 合 main）。

**关键结论**：
- **根因实锤**（下载 v0.2.1 AppImage 解包审计，非推测）：linuxdeploy excludelist 排除 libharfbuzz（假定目标系统自带），但 bundle 内 bookworm 版 pango 1.50.12/webkit 引用 3 个 harfbuzz 3.3.0/4.0.0 新增符号（hb_font_set_synthetic_slant@3.3.0；hb_ot_layout_get_baseline_with_fallback / hb_ot_layout_get_horizontal_baseline_tag_for_script@4.0.0，经 upstream NEWS 核实）；运行时 LD_LIBRARY_PATH 内无 harfbuzz → 落系统 2.7.4（jammy）→ 缺符号启动即崩。24.04 自带 8.3.0 有符号 → 解释了「仅个别 22.04 机器触发」。SESSION 早前「harfbuzz 2.x 的较新 API」判读不准确——实为 3.3/4.0 API，2.7.4 全缺。
- **全量符号审计方法**（可复用）：对 bundle 全部 .so 取 undefined 符号 ∩ 系统侧排除库（Fc/FT_/fribidi_/hb_ 前缀），逐一比对 jammy deb（aliyun pool 直接拉）——仅 harfbuzz 3 个符号偏移，其余零偏移。glibc 侧审计：bundle 最高 GLIBC_2.36 仅 libcups 的 `arc4random`（PLT 惰性绑定不阻塞启动，jammy 上打印路径才炸；GUI 无打印入口，记 WORKSPACE E 节不修）。
- **修复形态**：gui:linux 在 tauri build 后向 `xperf-gui.AppDir/usr/lib` 注入构建侧 libharfbuzz.so.0（bookworm 6.0.0，glibc 需求 ≤2.33 兼容 jammy）+ nm 质量门断言崩溃符号 + 重打包。坑：tauri 构建期 linuxdeploy 的 `/tmp/appimage_extracted_*` 解包目录**随进程退出即删**不可复用；appimagetool 须从 `$HOME/.cache/tauri/linuxdeploy-x86_64.AppImage` 自行 `--appimage-extract` 取内嵌的 plugin（hppc 实机验证提取路径）。
- **A/B 验证**（hppc docker `ubuntu:22.04` + xvfb + jammy harfbuzz 2.7.4 复现环境，镜像 `xperf-jammy-test`）：旧包复现用户报错原文（symbol lookup error）；修复包（pipeline 1408622 产物）xvfb 下主进程 + WebKitNetworkProcess + WebKitWebProcess 完整启动、零错误输出、超时存活。
- **环境观察**：当日 mirrors.aliyun.com 从 hppc/CI 仅 ~137KB/s（平时 build 全 step 10min，当日 gui:linux 40min 主要耗在 apt）；docker 容器内 apt 同样受影响（慢但在前进，非卡死）。

**遗留**：修复随下次版本 tag 发布（v0.2.1 已发资产不含此修复）；libcups GLIBC_2.36 打印路径风险记 WORKSPACE E 节。

## 2026-09-16（晚）：DMG 远程连接慢根因修复 + v0.2.1 重发 + 全仓改名 xperf

**任务**：用户实测 fix/gui-dmg-ssh 的 DMG 远程可用但连接间歇 30~80s；随后要求删除 v0.2.1 tag/产物重发；中途 GitLab 仓库改名 `ligraphic/xtools` → `ligraphic/xperf`，全 workspace 同步改名。

**commit**（均已推 hppc + li）：
- `449c472` perf(ssh-remote)：establish 收敛为单次 SSH 握手 + `utils::diag` 阶段计时（fix/gui-dmg-ssh）
- `9f441d8` merge 入 main；`0cf8be7` WORKSPACE 核销；`4f04872` CHANGELOG 补记
- `b612ff7` rename：仓库/脚本/CI/文档 xtools→xperf（含 `tauri.conf.json` identifier → `com.xperf.xperf-gui`、package registry 路径 `generic/xperf`）

**关键结论**：
- "DMG 慢/本机快"是巧合——`utils::diag` 实锤 init_remote 旧实现顺序 5 次 SSH 握手 × 网络波动（单次握手 2.5~10s）→ 28s+。修复后全程 1 次握手（远端预检走 ControlMaster mux），健康网络 <1s（connect_remote 实测 841ms），DMG Finder 环境用户确认。
- DMG 一直是 release 构建（sha256 与 target/release 产物一致）。
- 改名后 `target/` 增量缓存引用旧绝对路径致 tauri 构建失败——`rm -rf target/*/release/{build,.fingerprint,incremental}` 后恢复。
- 目录改名：本机 `~/workspaces/tools/xtools`→`xperf`（`git worktree repair <wt>` 修复 3 个 worktree），hppc `~/code/tools/xtools`→`xperf`（li URL 改 `ligraphic/xperf.git`，本机 hppc remote 同步）。
- v0.2.1 重发：删旧 tag/Release/package（30095）→ 重打 b612ff7 → CI 1408481 成功（Linux CLI+AppImage）→ `release-macos.sh v0.2.1` 成功（macOS CLI 双架构 tar + DMG 双架构），Release 6 资产全在 `generic/xperf/v0.2.1/`；发布版 arm64 DMG 下载→签名验证→安装→启动实测通过。
- GitLab API token：hppc `~/.git-credentials` 的 oauth2 token 可作 PRIVATE-TOKEN（删 Release/包/tag、查流水线均验证）。

**遗留**：无（问题反馈功能、AppImage 22.04 启动失败仍在 WORKSPACE 待办）。


## 2026-09-16 — Linux AppImage 在某台 Ubuntu 22.04 启动失败（bug 立项，新会话修复）

**现象**（用户报错原文）：`xperf-gui: symbol lookup error: /tmp/.mount_xtoolsZA1ucA/usr/lib/libpango-1.0.so.0: undefined symbol: hb_ot_layout_get_horizontal_baseline_tag_for_script`

**初步判读（仅记录线索，结论以实施会话复现为准）**：报错库在 AppImage squashfs 内（`/tmp/.mount_*/usr/lib/libpango-1.0.so.0`），症状指向 **libpango 与 libharfbuzz 版本耦合断裂**——`hb_ot_layout_get_horizontal_baseline_tag_for_script` 是 harfbuzz 2.x 的较新 API，bundle 内 libpango 链接到了旧版 harfbuzz（或加载到了系统目录的旧副本）导致符号缺失。「某一台 22.04 失败」暗示该机系统 GTK/pango/harfbuzz 组合与 bundle 内副本发生混载（`LD_LIBRARY_PATH`/linuxdeploy 排布语义），并非所有 22.04 必现。

**产出**：`WORKSPACE.md` A 节（已知缺陷）新增条目；无代码改动。待验证方向：① 复现机用 `LD_DEBUG=libs`/`ldd` 定位 harfbuzz 实际加载来源；② 比对 AppDir 内 `libharfbuzz*` 版本与 libpango 符号需求；③ 视结论修 linuxdeploy 收集范围（`--exclude-library` 等）或升级 bundle 内 harfbuzz；④ 修复后在目标 22.04 机器回归启动 + WebKitGTK 渲染。

---

## 2026-09-16 — 「问题反馈」需求文档立项（新会话实施）

**任务**：用户要求新增问题反馈功能——自动抓取最近 1 小时内的 log，在 GitLab 仓库创建 issue 并上传，log 覆盖率须达标（能从 log 分析问题）；本轮只更新文档，新会话开工。

**产出**：`WORKSPACE.md` I 节新增 `[ ] 问题反馈（一键收集日志 → GitLab issue）` 条目，写明实施要点：入口（GUI 数据管理区）、采集范围（`/tmp/xperf` 会话数据按 1h mtime 过滤 + `/tmp/xperf_gui_diag.log` + 设备端 `xperf-agent.log` pull + logcat 回放窗口 + 环境信息）、覆盖率达标定义（打包前自检清单，缺失如实标注）、GitLab 链路（project 39859，uploads + issues API，`GITLAB_TOKEN` 认证）、隐私与失败边界。遗留决策（CLI 形态、token 提供方式、脱敏策略、logcat 回放 A11/A16 兼容性）已在条目内标注为实施会话确认项。无代码改动。


---
## 2026-09-16 — DMG 安装版 GUI SSH 远程连接失败（新会话）

**任务**：用户反馈 DMG 安装的 `xperf-gui` 连接其他机器失败，界面显示“已切回本机”；修复并更新相关文档。

**根因**：Finder/LaunchServices 启动的 `.app` 不加载 shell profile，环境 `PATH` 通常不含 `$HOME/Library/Android/sdk/platform-tools`。远程初始化的协议校验先执行本机 `adb version`，但代码用裸 `Command::new("adb")`，因此在建立 SSH 隧道前失败。与此同时前端 catch 分支再次调用 `connect_remote(null)`，把真实错误覆盖为“已切回本机”。SSH 本身也使用裸 `Command::new("ssh")`，存在同类 PATH 风险。

**修复**：host `adb` 统一按 `XPERF_ADB` → PATH → `ANDROID_HOME`/`ANDROID_SDK_ROOT` → macOS/Linux 常见 SDK 路径解析；host `ssh` 按 `XPERF_SSH` → PATH → `/usr/bin/ssh` 等固定路径解析。所有 SSH ControlMaster、forward/check/exit 和 adb 调用复用解析结果；`check_protocol_version` 改用同一 host adb；GUI 远程失败只刷新本机设备列表并保留原始错误，不再二次调用切回本机。

**验证**：`cargo check -p xperf-core -p xperf-gui` 通过；`cargo test -p xperf-core -p xperf-gui` 通过（core 123 passed/8 ignored，GUI 10 passed）；`cargo doc` 与三个 crate 的 `missing_docs` rustdoc 均零 warning。最小环境实测：CLI `env -i PATH=/usr/bin:/bin:/usr/sbin:/sbin ... xperf-cli --remote hppc` 成功建立 SSH 隧道并列出远端 4 台设备（多台仅因未指定 `--device` 停止，属预期）；修复版 arm64 DMG（`tauri.release.json` ad-hoc 签名 + `codesign --verify --deep --strict` 通过）安装到 `/Applications` 后经 Finder `open` 启动 `--remote hppc`，GUI 正常连接远程并加载三台设备（diag：`init devices: 6eb792dfb0f,d1f39648c1f,localhost:5559`），确认 Finder 环境远程链路恢复。分支 `fix/gui-dmg-ssh`（commit 7d7b245）已推送 hppc/GitLab。

---


## 2026-09-16 — GUI AppImage/DMG 发布与内置 agent（分支 `feature/gui-release-packaging`）

**任务**：用户要求 GUI 发布包内置预编译 `xperf-agent`，Linux 使用 AppImage，macOS 使用 DMG；运行时禁止再编译 agent。

**实现**：`xperf-core/src/agent.rs` 新增 `spawn_agent_with_binary` / `reconnect_agent_with_binary`，GUI 从 Tauri `resource_dir()/agent/xperf-agent` 取资源；仅 debug 构建允许回退 workspace target，release 缺资源直接报错。新增 `xperf-gui/tauri.release.json` 作为发布覆盖配置，资源源为 `release-resources/agent/xperf-agent`，目标为应用资源 `agent/xperf-agent`。`.gitlab-ci.yml` 新增 `gui:linux`：Ubuntu 22.04-class/WebKitGTK 4.1、先从 build artifact 注入 agent、`ARCH=x86_64` 构建 AppImage；release job 挂 Linux CLI + AppImage。`scripts/release-macos.sh` 构建 arm64/x86_64 DMG，`CI=true` 跳过无界面 Finder AppleScript，DMG 内置 agent；CLI tar 保持原有双架构。

**验证**：Linux GUI job 1407346 全绿：Rust/Tauri/WebKitGTK 编译、linuxdeploy、104MB AppImage artifact；解包确认 `usr/lib/xperf-gui/agent/xperf-agent` 为 Android aarch64 ELF。失败过的中间原因均已修复：Ubuntu 镜像 apt 源路径、bookworm `debian.sources`、linuxdeploy FUSE/CI 模式、AppDir 同时含 host x86_64 与 Android agent 架构需 `ARCH=x86_64`。本机 macOS arm64/x86_64 DMG 均构建成功（约 3.8/4.0MB），`.app/Contents/Resources/agent/xperf-agent` 架构正确。core/GUI 测试全绿（123+8 ignored / 10）。

**发布策略**：GUI 代码已完成并推送分支及 GitLab；当前 `v0.2.0` Release 不改写，GUI 资产在合并主线后新 tag 发布。Linux GUI 最低 Ubuntu 22.04，CLI 仍 Ubuntu 20.04+；DMG 未签名/公证，正式分发前需补 Apple Developer 签名/公证配置。

---


## 2026-09-15 — 软件发布打包落地（v0.2.0）

**任务**：实现 Linux + macOS 发布包、GitLab CI tag Release、CHANGELOG 驱动发布说明；用户选择 CLI + Android agent，不含 aarch64 Linux/GUI，macOS 由本机补传。

**实现**：`.gitlab-ci.yml` 完成 validate/test/build/release；内部 Artifactory 镜像 + Aliyun apt/crates sparse 镜像；NDK r25b（507MB）和 Android rust-std（26MB）vendor 到 GitLab package registry，build 首次实测 550s→106s。新增 `scripts/release_create.py`、`scripts/release_upload.py`、`scripts/release-macos.sh`；四 crate 统一 bump `0.2.0`，新增 `CHANGELOG.md`，删除 `.github/workflows/ci.yml`。

**验证**：分支流水线 1404810 在 prod01 宿主机内存故障后重试成功（test 99s/build 106s）；tag 流水线 1404834 的 validate/test/build/release 全绿。`v0.2.0` Release 已有 Linux x86_64、macOS arm64、macOS x86_64 三个 CLI + agent 资产，API 下载路径实测可用；`/-/package_files/<id>` web 路径实测 404，资产链接已改为 package registry API。macOS tar 使用 `COPYFILE_DISABLE=1` 去除 AppleDouble/xattr；三包结构与二进制架构已核验，CLI `--version` 冒烟通过。最终本地回归：core 123 passed + 8 ignored、CLI 5、GUI 10，cargo doc/clippy 零 warning。

**关键修复**：本 GitLab CE 的 generic package PUT 响应只有 `{"message":"201 Created"}`，不含官方文档中的 `package_files`，`release_upload.py` 改为不依赖 PUT 响应；后续资产 URL 改用 API 下载路径。NDK/rust-std 不再依赖 runner 外网大文件下载。

**遗留**：CLI/agent 原 v0.2.0 发布链路无阻塞；本分支新增 GUI AppImage/DMG 仍需合并主线并在新 tag 发布。命令行输入仍是 WORKSPACE I 节候补。

---

## 2026-09-15 晚(4) — 软件发布打包需求入 WORKSPACE + 文档 review 修复（a0eb296 + 本条）

**任务**：①用户要求支持 Linux/macOS 软件发布打包（结合 GitLab CI）——本会话只记录需求与勘察结论，新会话施工；②review 全部文档。

**产出①**：WORKSPACE I 节新条目「软件发布打包」——含现有 `.gitlab-ci.yml` 状态（test stage 空壳 / macOS job 全注释 / release description 硬编码）、最高风险前置项（内部 GitLab runner 可用性，尤其 macOS）、GUI 打包依赖缺口（webkit2gtk/Xcode）、agent 是否随包决策、打包形态与版本号细节、push 流程（已切 li）。

**产出②（文档 review 修复，README 双语版事实性过时）**：传输描述 `adb exec-out` → daemon+forward/TCP；数据根 `log/` → `/tmp/xperf`；root-only 要求 → 非 root 降级矩阵；linker 描述 `.cargo/config.toml`+env 覆盖 → `.cargo/ndk-clang.sh` 自动探测（NDK ≥25.1）；**`cargo build --release --workspace` → `cargo build --release`（--workspace 会触发 agent 主机目标 compile_error）**；`cargo test --workspace -- --test-threads=1` 的「global mock adb runner」理由已不存在（三件套 01b28ce 已删，测试可并行）→ `cargo test`；功能清单补全（B 类指标/深挖/捕获/验证/远程/多设备）。WORKSPACE 速览测试计数 core 122→123（e7d9bf9 漏更新）。工具坑：zsh 内联 python 字符串中的反引号会被 shell 吃掉（命令替换）——曾致 README 一行损坏，已修复；BSD od 对多字节 UTF-8 显示为 `**`、sed 输出重复均为终端伪影，文件字节以 python repr 验证为准。

---

## 2026-09-15 晚(3) — hppc 默认远端切换内部 GitLab（运维，无代码）

**任务**：用户要求 hppc 端默认远端从 GitHub 调整为内部 GitLab。

**改动**（全在 hppc 仓库配置）：remote `li` URL https → ssh（`git@gitlab.chehejia.com:ligraphic/xtools.git`，ssh -T 认证 @wangjinhan 已通，免 https 凭证）；`git push li main`（gitlab 侧 main fa1e63c 为本地祖先、落后 68 提交，fast-forward 推送 e7d9bf9，LFS 无新对象零传输）；`git branch --set-upstream-to=li/main main`——hppc 上裸 `git push`/`git pull` 默认走 GitLab。GitHub `origin` 保留，显式 `git push origin main` 仍可推。本机拓扑不变（推 hppc）。CLAUDE.md 拓扑段已同步。

**遗留**：无。（GitHub 与 GitLab 从 09-15 起开始分叉——若仍需镜像 GitHub，记得显式推 origin。）

---

## 2026-09-15 晚(2) — CLI 更名 xperf-cli（xperformance → xperf-cli）

**任务**：用户建议 CLI 更名为 `xperf-cli`（与其余成员 `xperf-core`/`xperf-gui`/`xperf-agent` 命名对齐）。

**改动**：目录 `git mv xperformance xperf-cli` + crate/bin 名（Cargo.toml package+[[bin]]）+ 根 workspace members/default-members + CI 产物路径（.github ci.yml ×2、.gitlab-ci.yml ×3，其中 2 处本已注释）+ 代码注释（main.rs 文件头、core trace/simpleperf doc 各 1）+ 活跃文档（CLAUDE/README×2/WORKSPACE）+ docs 设计文档命令示例（DESIGN-ssh-remote ×9、DESIGN-ss4-metrics ×1——命令名变了示例须跟随）。**顺手删除顶层 `src/main.rs`**（first commit 遗留 Hello world，根 Cargo.toml 是纯 [workspace] 从不构建它）。

**验证**：全量测试（core 122+8 ignored / GUI 10 / CLI 5）+ clippy/doc/rustdoc missing_docs 零警告；release 产物 `target/release/xperf-cli`；真机冒烟 `--remote hppc` SS3 采样正常。`git grep xperformance` 归零（SESSION 历史条目除外；target/ 构建缓存除外）。

---

## 2026-09-15 晚 — xrm 移出本仓库（残留清理）

**任务**：用户已删 xrm crate（Cargo.toml members/default-members + xrm/ 目录），检查遗漏。

**清理**：CLAUDE.md（Commands 两处 xrm 测试示例 + Workspace 成员行 + 整节「xrm 设计结构」）、README.md / README_zh.md（表格行 + `### xrm` 章节）、WORKSPACE.md 速览测试行（顺带把过时的测试计数更新为当前基线 core 122+8 / GUI 10 / CLI 5，并注明 xrm 已移出）。**SESSION.md 历史条目按约定不动**。

**核对无遗漏**：`git grep -i xrm` 排除 SESSION/二进制后归零；Cargo.lock 无 xrm 残段；`.claude`/`.github` 无引用；`安全删除` 中文关键词仅剩刚清理的 README。构建/测试（core 122+8 ignored + GUI 10 + CLI 5）/cargo doc 零警告。

---

## 2026-09-15 — logcat 文本过滤（`feature/logcat-text-filter` 合 main）

**任务**：WORKSPACE I 节候补——按包过滤之外增加文本内容过滤（关键字/正则，下沉设备端），CLI 参数 + GUI 输入框，叠加生效，口径变更走热切换。

**commit**：e728b69（core：config/restart/build_args 增文本正则，`-e` 下沉 + 标记行 text= 口径 + 5 单测）→ e676d04（CLI --logcat-regex + 秒死 Error 透传 stderr）→ ca009c1（GUI 日志 tab 过滤输入框 + start/restart 命令 text 参数 + change 热切换联动）→ 文档收尾。

**形态决策**：设备端 `logcat -e <regex>`（消息体 POSIX ERE）——backlog 提示的「洪泛场景下沉设备端」直接采纳，不做 host 侧过滤；host 不校验正则语法（ERE 与 Rust regex 不等价，设备校验是权威），非法正则走既有秒死 Error 路径。

**关键实测结论**：
- **非法正则三平台一致**：A11/A12/A16 的 logcat 收到 `[invalid` 均立即 rc=134（`regex_error was thrown in -fno-exceptions mode` abort）→ 子进程秒死 ×3 → `LogcatEvent::Error`——CLI 新增事件回调把该错误打到 stderr（此前 CLI 传 None，参数不兼容类失败不可见；非法正则是该路径首个用户可达诱因）
- A11 的 `-e` 正常可用（有效正则过滤正确；此前仅验证过 `--uid` 不支持）
- SS4 多用户 uid 逗号列表与 `-e` 叠加正常（`--uid=10220,99910220 -e filament`）
- adb 参数转义可信：正则含 `|`/空格/中文经 `.args()` 由 adb client 转义，无 shell 注入面

**真机回归（--remote hppc）**：SS3 uid+`-e` 叠加（文件头 `filter=Uid("10136") | text=filament`，仅命中行+beginning 标记）✓；SS2MAX 全机+`-e` ✓；SS4 多用户 uid+`-e` ✓；非法正则 `[invalid` → ❌ 秒死放弃透传 ✓；热切换集成测试（restart 带 text=FATAL，标记行佐证同文件续写）✓；**GUI AX 全链**：日志 tab → 过滤输入 filament → 开始（文件头 text=filament）→ 改 SurfaceFlinger（respawn 标记 text=SurfaceFlinger）→ 改 ANR（restart 事件）→ 停止 ✓。

**测试**：core 122+8 ignored（+3 新单测：text_regex 叠加/空白忽略/display_regex 转义，集成测试 test_logcat_restart_hppc 更新带文本口径）、GUI 10、CLI 5、xrm 2 全绿；clippy/cargo doc/rustdoc missing_docs 零警告。

**勘察插曲（教训）**：① AX 验证一度误判「text 未生效」——实为**用户自己的 GUI 实例（旧镜像，UI 连接 hppc）同秒在同目录起了抓取**，diag/落盘路径共享致假象；判据须 lsof 文件持有者，多实例并存时留意。② WKWebView AX `set value` 异步生效（秒级）、合成 keystroke return 不一定派发 change，可靠触发改 `click` 他元素制造 blur；AX 元素引用跨流式渲染失效须重新遍历。③ AppleScript 保留字 `note`/`st` 不可作标识符；BSD sed 不认 `\b`。（均已记 CLAUDE.md AX 教训条）

**同日复核修复（review 第一轮）**：CLI 独立模式抓取线程异常终止后进程挂等 Ctrl-C 且退出码 0（非法正则是该路径首个用户可达诱因）——等待循环改 `!is_interrupted() && !is_done()`，异常终止退出等待并置 `capture_failed` → exit 1；真机复测：非法正则 ~10s 自退 exit 1、正常抓取 12s 不早退 SIGINT exit 0。SESSION 测试数笔误（+5→+3）一并修正。

**同日复核增强（review 第二轮，含一次自我纠错）**：①秒死放弃报文自解释——`diagnose_fast_death` 用同参数跑 `logcat -d` 探测 stderr。**第一版直接取 stderr 尾行在真机翻车**：A12（SS3）抓到 `-T 0 invalid` 无害告警（正常路径也有，误导死因）；改为**对照法**（跑两次 `-d`：完整参数一次、去 `-e` 对一次，取 full 有 base 无的差集行）——告警两侧同在即抵消。真机验证：A16 非法正则报文附 `regex_error was thrown in -fno-exceptions mode`、A12 回归原文案（静默 abort 诚实无附加段）、合法正则正常抓取零错误；**adb 客户端不透传设备端退出码（rc 恒 0），stderr 是唯一可信诊断源**。②`args.logcat_regex.clone()` 改 `take()`（唯一消费点）。core 123（+1 format_probe_diag 差集单测）全绿。

**遗留**：无。（GUI live 视图行渲染 AX 读值为旧已知项，不影响本功能。）

---
## 2026-09-15 — facedemo filament 版本性能调试（与本仓库无关，已清理）

facedemo 1.74 CPU 回归归因与排序管线优化（应用侧，未动 xtools 代码）：任务完成，真机验证通过。产物在 hppc `/tmp/facedemo/`（patch + APK），详细过程记录已从本文件移除。

---

## 2026-09-14 深夜(3) — logcat review 第二轮（0771155）

**任务**：继续 review（core 线程逻辑 / GUI 后端 / CLI / 文档）。修复 2 项：CLI 混合独立模式（`--screenshot --logcat`）logcat 失败补 `capture_failed` → exit 1（对齐 record/screenshot 语义；并行采样模式保持「失败只告警」不置码）；前端 `syncLogcatEvents` catch 静默吞错改 diag（IPC 掉线时后端暂停态失同步会致视图静默停滞，幂等重发自愈）。

**后续（7118091）**：用户质疑「记录不修」评估后重新推演，**项 ①（restart 竞态）评估不成立已修**——写线程 Eof 分支为「读 config → sleep(1s) → spawn」，restart 三步落在该窗口内时 kill 落空（slot 已 take）→ 旧口径 respawn 起来后无 Eof → `restarting` 标记滞留、新口径静默失效到下次断连，状态栏却已显示「已切换」；窗口 1s 非极窄，断连退避期间切级别高概率触发。修复：spawn 后补检 `restarting`，置位立即杀刚起的进程走 planned 路径按新口径重 spawn（全时序场景收敛，含 kill 落空/µs 窗口/正常路径）。项 ② ③ 维持不修（目录函数重复为风格项非正确性项；spawn 无限重试与断连重连同语义且 stop/关窗/Ctrl-C 退出路径全覆盖）。core 119 / CLI 5 / clippy / doc 零警告，集成测试回归通过。**教训：竞态「窗口窄」的判断必须对照真实代码时序（含 sleep），不能凭直觉——本例 sleep(1s) 恰是窗口主体。**

---

## 2026-09-14 深夜(2) — logcat UI review 修复（0ce8b45）

**任务**：用户要求 review 代码与文档（重点 UI）。发现并修复 4 项：**A 滚动失效**（`.logcat-view` 无 overflow，`view.scrollTop` 是 no-op，滚动实际发生在外层 trace-report-box——「暂停滚动/自动滚底」从未生效；改为 view 自滚动 overflow-y:auto+height:100%）；**B error 槽位死锁**（core 秒死放弃后 handle 滞留 GUI 槽位，再点开始永远「已在抓取」；core 加 `is_done()`，start 探测死句柄清槽接管 + 前端 error 兜底调 stop_logcat）；**C 会话隔离**（停止后再开始视图/buf 残留旧会话行；start 成功后清）；**D 重放判定**（设备页激活但日志 tab 隐藏时也重建 DOM；renderLogcatIfDirty 加可见性前置）。核对无误项：restart 持锁 adb 数百 ms（无死锁，可接受）、start 竞态（旧 handle drop 即 kill 无泄漏 + 前端 disabled 防抖）、stop/pending/Error 交互、CSS 色板主题跟随。core 119+GUI 10 全绿，AX 冒烟无回归。

---

## 2026-09-14 深夜 — logcat 性能开销分析与视图渲染节流（8b45830 + b2f04a4）

**任务**：用户问询 logcat 开销 + 观察「全机不过滤抓取时 xperf-gui + webkit 合计 20%+」是否正常。

**测量（SS3 全机洪泛 ~190 行/s，debug build，隔离变量：无采样/无镜像）**：CLI host ~1.0-1.4% / GUI Rust 后端 +1.3% / **WebKit 前端 +13.4%**（合计 ~15.5%——用户观察的 20%+ 含 ssh/adb 子进程，量级一致）。**根因：日志 tab 后台（用户在看指标页）时前端仍每 200ms append 38 行 DOM——WebKit 不可见 DOM 变更照样维护渲染/AX 树，13%+ 纯浪费**（对照：采样图表本身 webkit 开销 ≈0.1%）。

**修复（8b45830 + 后续 IPC 暂停）**：两层节流——① 非可见不碰 DOM（数据恒入 JS ring buffer + 脏标记，切回 DocumentFragment 重放，与图表「仅绘制激活页」同构）：后台 webkit 13.5%→3.4%；② `pause_events`/`resume_events`（core 攒 pending 上限 4000 尾部保留，恢复 ≤200ms 一次补发；`Error` 照发）+ `set_logcat_events` 命令 + 前端 `syncLogcatEvents`（switchTab/switchDevice/启动后同步）：**后台 webkit 归零 0.0%**。AX 实测：后台归零 / 切回 `bigbatch 4000` 补发（暂停 ~45s 攒满）/ 前台渲染恢复 8.2%。次要结论：core 逐行 flush ≈0.04% 单核不值得改；全机落盘 ~38KB/s（1h ≈137MB，/tmp 自清）；**纯暂停（丢事件）会让视图行缺口，必须攒批补发**。

**教训**：`ps aux` 的 %CPU 是进程生命周期平均（被启动期稀释），瞬时值须 `top -l 2` 取第二样本。

---
## 2026-09-14 晚 — logcat 支持（`feature/logcat` 合 main）

**任务**：WORKSPACE I 节新功能候补 ①——设备 logcat 抓取/查看接入工具链（要求 SSH 远程可用）。

**形态决策**（用户确认 A/A/A）：core 新模块 `logcat.rs`（spawn `adb logcat` 流式子进程，走 hop#1 零隧道改动）；按包过滤 UID 优先（A11 降级 pid）；GUI 每设备第 4 子 tab「日志」（live 视图 + 落盘），弃「仅侧栏落盘」；断连 1s 退避自动重连（对齐采样语义），弃「EOF 即停」。

**commit**：35a92f3（core logcat 模块 + 9 单测）→ f803a3b（CLI --logcat）→ 551d50c（GUI 日志 tab）→ a41c091（doc 链接修复）→ a4d4b0e（**过滤口径热切换**：restart 复用断连重连路径按新参数重 spawn 同文件续写，restarting 标记防计划内 kill 误计秒死；GUI restart_logcat 命令 + 前端级别/按包/包名 change 联动；集成测试 test_logcat_restart_hppc + AX 实测全机↔uid 往返）。

**实现要点**：
- core：读线程（blocking lines → mpsc）+ 写线程（`recv_timeout` 200ms 合帧 → 文件逐行 flush + 事件回调批量推 GUI）；`LogcatFilter::{All,Uid,Pid}`；`resolve_package_filter` 按 `ro.build.version.release` ≥12 选 `--uid`（`pm list package -U`，SS4 多用户 `10220,99910220` 逗号列表原样透传），否则 `pidof` 首 pid 降级（**A11 logcat 无 `--uid`，实测 `Unknown option`**）
- 行格式 `-v threadtime -v year -T 0`：threadtime 设备时钟与采样 CSV（agent 设备端 epoch）同源天然对齐；`-T 0` 不回放历史（A11 降级 1 行 backlog + stderr 告警，stdout 干净——实测锁定）
- 断连重连：EOF → 1s 退避重 spawn（文件内 `# xperf logcat reconnect` 标记行）；**设备在线但秒死（<3s）×3 → 判永久性失败**（参数错误类）`LogcatEvent::Error` 上报退出，防静默死循环
- CLI：并行（窗口覆盖采样全程）/独立（Ctrl-C）两模式；独立失败 exit 1、并行失败只告警（同截屏/录屏语义）；截屏「唯一目的」判定补充豁免 logcat
- GUI：`DeviceSession.logcat` 槽位（停止同步完成无监护线程）；`start_logcat/stop_logcat` 命令 + `logcat {serial, stage, lines|message}` 事件；前端 ring buffer 2000 行 + 级别着色（正则兼容 A11 `+0800 ` 时区前缀行首）+ 暂停滚动/清空；关窗 CloseRequested 统一 stop

**真机回归（均 --remote hppc）**：SS3 uid=10136 独立 ✓ / SS2MAX pid=1744 降级+限制提示 ✓ / SS4 多用户 uid 透传（冷启动 burst 111 行）✓ / `--cpu --logcat` 并行同目录（时间轴逐秒对齐）✓ / 无包名全机（1517 行/8s，device-<serial> 目录）✓ / `adb reconnect` 断连→重连标记行+继续 ✓ / A11 未运行与包未安装两错误路径 exit 1 文案清晰 ✓。**GUI AX 实测**：日志 tab 切换→包名填写→开始抓取→停止抓取全链（diag `logcat: started/stopped` + 落盘 uid 过滤正确佐证）；**热切换全链**（ax_final2 递归遍历脚本）：开始（uid）→取消按包过滤（diag `restarted (all)`，全机 3749 行）→勾回（`restarted (pkg=…)`）→停止，落盘文件两条 `# xperf logcat respawn … filter=All/Uid` 标记行佐证同文件续写。**关键教训**：① AX `set value` 写文本框须先 `set focused true`（否则值不落 DOM，按包过滤静默退化为全机——落盘目录 device-* 而非 pkg 名即为判据）；② 流式渲染期间 webkit AX **`entire contents` 枚举整体剪枝粘性不恢复，递归 `UI elements of` 逐层遍历仍完整可达**（AX 脚本须用后者）；③ run_cmd 超时杀整个进程组——被测 GUI 与脚本须分属不同 run_cmd 调用（一次 180s 超时把同组 GUI 一并杀掉，伪崩溃排查）。

**测试**：core 119（+9 logcat，+1 ignored 集成 test_logcat_restart_hppc）+ GUI 10 + CLI 5 + xrm 2 全绿；clippy/cargo doc/rustdoc missing_docs 零警告。

**遗留**：live 视图行渲染未能 AX 读出（上述剪枝问题）——代码走查 + 与 sample/trace 同事件模式，用户一眼确认即可。

---


## 2026-09-14 — 镜像+录屏并存缺陷核销（`feature/screen-capture` 收尾合 main）

**任务**：WORKSPACE A 节遗留——镜像+录屏同机并存失败（录屏卡死不产出 + 近同时启动镜像被杀）。

**结论（两个独立根因，均已修复/核销）**：

1. **近同时启动镜像被杀 = sweep 跨进程竞态**（本工具代码缺陷）。机理实锤链：录屏进程启动清扫 `sweep_scrcpy_rules` 会删掉镜像 scrcpy 注册后、建连完成前的 forward 规则（同进程由映射表保护，**跨进程无保护**）→ 镜像 connect 永远落空，10s 重试（100×100ms）耗尽 → "Server connection failed" 客户端死亡 → adbd 收尸设备端 server（"Killed"）。**killer 实验实锤**：hppc 上毫秒级轮询一删规则，镜像客户端必然死于连接失败。**修复**：sweep 第二层防护——远程模式下跳过本机端口被占用的规则（占用 = 另一进程活会话全程持有其 hop#2 本机监听；残留规则的宿主已死、无监听，照扫不误）。真机验证：持有本机监听的伪规则存活 ✓、无监听残留被清扫 ✓。
2. **录屏卡死不产出 = 设备端 scrcpy-server 启动期偶发中止**（平台级 flake，非本工具链路问题）。**勘察反转**：ADB wrapper（scrcpy 支持 `ADB=<wrapper>` 环境变量）挂日志证明 forward 注册从未失败（exit 0）；失败窗口内规则/监听器/双 server 进程都在；真凶是**录屏的 device server 在绑定 abstract socket 前死亡**（进程存活 ~0.5s，stderr 零输出，只见 shell 报 "Aborted"；镜像流并发下 SS3 实测 ~15% 发生率，单路启动未见）。客户端侧表现为 connect 重试耗尽。09-12 当天的 scrcpy 客户端 SIGSEGV 崩溃报告（`avformat_new_stream` ← `sc_recorder_video_packet_sink_open`）是连接失败 teardown 竞态的次生现象。**修复（自愈）**：录屏启动未建流（产物文件 10s 未出现）且非用户中断时**自动重试一次**（新端口+新 server 实例），CLI `spawn_record_thread` 与 GUI 监护线程同策略，GUI 前端加 `retrying` 状态（状态栏提示、按钮保持录制态）。

**验证**：
- 防护单测 + 真机（SS3 远程）：活规则存活/残留清扫双向正确；clippy/doc/missing_docs 零警告，全量测试 core 110 + GUI 10 + CLI 5 + xrm 2 全绿。
- CLI 重试：fail-once scrcpy shim →「⚠️ 自动重试一次…→ 录屏已保存」✓；always-fail shim → 双败后如实报错 exit 1 ✓。
- 共存回归矩阵全通：SS3 两进程（offset 0.5/1.5/2/5s）、SS3 同进程 ×11、SS4 两进程、同机双镜像，合计 20+ 次无失败。
- **GUI 按钮 AX 目验闭环**（A-2）：`--remote hppc` 启动 → AXPress 截屏 → PNG 落盘 ✓；录屏 toggle 启动/停止 → MP4 封盘 ✓；GUI 重试路径（fail-once shim 经 PATH 注入）→ diag `record retrying` → 重试后录制/停止/封盘全通 ✓。
- 中途观察：一次 AX 轮询窗口「window 1 无效索引」（GUI 进程活着、录制与清理均正常完成），疑似 AX 树瞬时剪枝/渲染层抖动，未复现，记录观察项不阻塞。

**勘察工具沉淀**：`ADB=/path/to/wrapper` 可完整记录 scrcpy 全部 adb 子调用（argv/exit/stdout/stderr/毫秒时标）；scrcpy 4.1 关键事实——forward 注册是 adb 二进制单次调用（`-p P` 单口即 range {P,P}，失败即 LOGE 退出不重试）；connect 重试 100×100ms 在注册之后；建连成功后 scrcpy 自撤 forward 规则；设备端 server 顺序 accept（首连接=视频流，任何探测性 connect 都是投毒）。**教训**：秒级日志无法分辨亚秒竞态，死亡判定须用进程级毫秒时间戳（perl Time::HiRes）+ 独立 death-watch。

**遗留**：无。（scrcpy 客户端 teardown 竞态 SEGV 为上游 bug，仅在已失败的连接路径上触发，不修。）

---


## 2026-09-12 — 截屏与录屏（`feature/screen-capture`，基本完成未合并：遗留镜像+录屏并存问题移交）

**任务**：WORKSPACE I 节新功能候补 ③——截屏（screencap）与录屏接入工具链，SSH 远程可用。

**形态决策**（用户确认方案 A）：录屏走 scrcpy `--no-window --record`（复用镜像隧道链路，无 180s 上限），弃设备端 screenrecord（180s 硬上限）；截屏走 `adb exec-out screencap -p` 直写本机。

**commit**（feature/screen-capture 分支，未合 main）：d8471f7（core：capture.rs + mirror.rs 录屏模式）→ e448b10（CLI --screenshot/--record N）→ 2947807（GUI 截屏按钮+录屏 toggle）→ 8939872（doc 链接）→ a2136b3（sweep 并发保护+mapped_remote_ports）→ e0c931e（录制倒计时起点修正+产物核验）。

**实现要点**：
- `mirror.rs` 抽 `spawn_scrcpy(serial, ScrcpyMode::{Mirror, Record(path)})` 公共件；录屏停止须 **SIGINT 优雅封盘**（MP4 finalize，SIGKILL 产坏文件）——`stop()` SIGINT + `sigint_at` 单次去重 + wait_exit 3s 超时 SIGKILL 兜底 + cleanup Drop 路径原地宽限
- 截屏 PNG 魔数**偏移定位**（非 starts_with）——SS4/A16 screencap 把 `[Warning] Multiple displays…` 打到 stdout 前缀，真机踩坑修复
- CLI `--record N` 倒计时从产物文件出现起算（wait_record_started）——spawn+推 server ~8s 延迟不吃进窗口；采样并行时 stop_after 取 max(trace, stack, record)
- 产物核验（CLI record 线程 + GUI 监护线程同口径）：进程优雅退出但 MP4 不存在 = 失败如实报，杜绝「已保存」假阳性
- sweep_scrcpy_rules 并发保护：跳过本进程 hop#2 映射表端口（`SshTunnel::mapped_remote_ports`）——录屏启动的清扫曾摘掉镜像建连中的规则致其 Device disconnected
- Ctrl-C handler 独立模式才自行注册（抢先注册会顶掉 monitor_process_agent 的 set_handler 报错）

**真机回归矩阵**（--remote hppc）：SS3 截屏 2880×1620 ✓ / SS3 录屏 5s→成片 5.35s ✓ / SS3 采样+截屏+录屏并行同会话目录 ✓ / Ctrl-C 提前封盘 exit 0 ✓ / SS4 截屏（前缀剥离后 PNG 有效）+ 录屏 6296×1740 ✓ / 同机双镜像两进程共存 12s ✓。

**遗留（移交下会话，详见 WORKSPACE A 节）**：**镜像+录屏同机并存失败**——录屏客户端卡在连接重试（forward 规则始终缺席），并发近同时启动时镜像 server 被 SIGKILL。已排除 sweep/端口/平台限制；勘察证据与下一步（ADB wrapper 挂日志抓注册返回值等）见 WORKSPACE A 节。

**测试**：core 109 + GUI 10 + CLI 5 + xrm 2 全绿；clippy/doc 零警告。教训：HashMap 无序断言 flaky（test_mapped_remote_ports 排序修复）；run_cmd 管道 exit code 是末命令的（`cmd | tail; echo $?` 测的是 tail）。

---



## 2026-09-11(7) — scrcpy 屏幕镜像集成（`feature/scrcpy-mirror` 合 main）

**任务**：WORKSPACE I 节新功能候补 ②——scrcpy 集成（屏幕镜像，要求 SSH 远程可用）。

**commit**：589335d（core mirror 模块 + transport 固定端口 hop#2）→ 94974ed（CLI --mirror）→ 2b3ad69（GUI 按钮）→ 7c583f4（fix：MirrorExit 三态）→ 5afd332（三态单测 + trace.rs 测试死变量清理 + CLAUDE.md）。

**形态决策**（用户确认）：拉起**外部 scrcpy 窗口**（解码/触控归 scrcpy 客户端），不做 GUI 内嵌视频（内嵌需自研 H.264 remux→MSE/WebCodecs + 触控注入，投入产出比低）。CLI `--mirror` 与 GUI 侧栏「屏幕镜像」按钮共用 core `mirror.rs`。

**关键实测结论**（Mac→hppc→SS3 真实链路）：
- scrcpy 的 adb 操作（push/设备发现/forward 注册）经 hop#1（`ADB_SERVER_SOCKET` 环境变量）全部正常——scrcpy 4.x 原生实现 adb 协议，读 `ADB_SERVER_SOCKET`。
- **默认 `adb reverse` 模式在远端 server 拓扑下不可用**：reverse 规则注册成功但设备侧连接回不到经隧道的 TCP 客户端（nc 对照实验：规则在、连接零到达）。
- **正解 = `--tunnel-port=P`**（隐含 `--force-adb-forward`）：scrcpy 把 forward 注册到远端 server，连本机 127.0.0.1:P，该端口由 hop#2 同号映射（新增 `SshTunnel::add_forward_pinned`）承载。端口池 27183..=27199 扫描**两侧同号空闲**（远端忙端口读 `forward --list` 全集——forward 端口是 server 全局资源跨 serial）。
- scrcpy 建立连接后会自己撤掉 forward 规则（运行中 `forward --list` 看不到属正常）；被 SIGKILL 时规则残留 → `sweep_scrcpy_rules` 按 serial 清扫。
- **scrcpy 正常运行也往 stderr 打启动日志**（tunnel 提示 + push 行）——退出归因不能凭 stderr 非空，须 exit status + 主动停止标志（`MirrorExit::{Stopped,Closed,Failed}`；GUI 事件三态分流，用户关窗不再误报「异常退出」）。
- scrcpy-server push 经隧道偶发变慢（0.005s 基线，一次 16s+）——wait_exit 150ms 轮询 + 宽限期设计不受影响。

**真机回归**（均 `--remote hppc`）：
- CLI 镜像-only：SS3 流 ESTABLISHED + SIGINT 2s 内优雅退出 + 零残留（进程/规则/hop#2）
- CLI 双设备并行：SS2MAX:27183 + SS3:27184 端口隔离，双流并行，退出全清理
- CLI SS4（localhost:5559 桥接 GVM）：流 ESTABLISHED，清理正常
- CLI 采样+镜像并行（gltf --cpu --mirror）：SIGINT 3s 退出，图表/CSV 正常，零残留
- GUI（用户实机点击，新二进制 diag 日志佐证）：SS3/SS4 按钮启动（视频+控制双连接 ESTABLISHED）、停止按钮（`mirror stopped`）、关 scrcpy 窗口（`mirror closed`）三态全通
- 本地模式：无本机设备（错误路径优雅报错「无 adb 设备在线」）；本地有设备的正向路径未真机（本机无设备可接）

**测试**：core 102（+mirror 5：forward 行解析/端口池三态/MirrorExit 三态）+ GUI 9 + CLI 5 + xrm 2 全绿；clippy/doc/missing_docs 零警告。

**遗留**：GUI 关窗时镜像清理路径已经用户真机目验闭环（开镜像→关主窗口→scrcpy 随之关闭 ✓，E 节已核销）。harness 注意：run_cmd 结束时其后台进程组会被收割（本次并行模式验证一度被此干扰）——后台验证须在同一命令内完成 SIGINT+等待闭环。

**同日补丁（a99dee7，用户实撞驱动）**：SS3 镜像在跑时 SS4 起不来（"Server connection failed"）。根因（scrcpy v4.1 源码 `adb_tunnel.c`/`server.c` 锁定）：**`--tunnel-port` 只钉本地 connect 口；`adb forward` 注册口由 `-p/--port`（port_range，默认 27183:27199）在 adb server 侧扫描**——只传 tunnel-port 时，注册口扫描结果（远端 27183 常空闲，因先跑镜像的规则建连后即撤）与连接口（27184）错配。修复：`-p P --tunnel-port=P` 双钉同号。勘察教训：**nc -z 探测 hop#2 是侵入性的**（会消费 scrcpy-server 的首个视频连接，污染实验）；scrcpy 的 `[server] INFO: Device:` 行是设备端进程启动日志，**不能作为连接成功判据**（真连接判据 = lsof ESTABLISHED / 保活超 10s 重试窗）。回归：SS3(27183)+SS4(27184) 并行双 ESTABLISHED、单镜像池首回归、退出零残留。

**独立 review 修复（341da84）**：①add_forward_pinned「同号复用」分支在并发双镜像下错误共享 hop#2 → 改 `Result<bool>` 占用信号 + start_mirror 候选端口循环（busy/本机占用/映射表已占/-O forward 失败均换下一口）；②双钉参数抽 `build_scrcpy_args` 纯函数 + 单测锚定；③sweep 双重解析清理。测试 103+9+5+2 三连跑全绿（注意：「释放后报闲」断言在并行测试下有抢端口 TOCTOU，只测忙向），真机并行复验通过。

---

## 2026-09-11(6) — QNX 「proc 链泄漏」核销：真凶是孤儿 tailer（协议 v9，`feat/qnx-orphan-reaper`）

**任务**：WORKSPACE E 节唯一未修缺陷——QNX `gpu_per_process_busy` 进程链无停止手段、疑似 ~20 条锁步洪泛挤死 frame 链。

**commit**：d6e7d6e（协议 v9）+ docs 收尾。

**黑盒勘察结论**（SS3 真机，busybox telnet 逐命令实验，事件级证据）：
- **命令全表**：`echo help > /dev/kgsl-control` 让解析器把完整命令表打进 slog（CRITICAL INFO 级）——无停止命令；`gpu_per_process_busy 0` 报 `Invalid sampling time interval 0msec, setting to default value of 1000msec`（钳位并启动，非停止）。
- **写入语义**（干净单 tailer 对照）：proc 写入 = 未跑则启动/在跑则重相位锁步，**不新增链**；开机 `/mnt/scripts/startup.sh` 写 `gpu_set_log_level 4` + `gpubusystats 5000` + `gpu_per_process_busy 5000`（frame/proc 各一条 @5000 是设计基态）。
- **真凶**：此前观察到的「链数 3→5→6→7 漂移」「~20 条锁步洪泛」全是**孤儿 slog2info tailer 重复打印**——telnet 断开只杀登录 shell，后台 `slog2info|grep` 管道不死（slay 列表实锤 10+ 个孤儿，含各实验/历史会话的 job pid），ttyp0 回收重用后孤儿把驱动行重印进新会话（同一行多份同时间戳，形似多链锁步）。孤儿挤占 QNX CPU 疑似才是 09-07 frame 链停走的根因。
- **KGSL 驱动形态**：QNX 用户态 resmgr 进程（pid 90154，slog 模块名 kgsl.90154），真身在 `GSLKernel.so`（pidin mem 确认映射）；`/ifs/bin/kgsl` 只是 17KB 启动 shim。kgsl-control 常驻持有者 = `ifs/bin/qcore`（系统核心守护，非统计写入源，strings 无相关命令）。
- 勘察方法教训：**QNX 上 `slog2info -W | grep … &` 实验必须收尸**（`kill $!`，管道末尾 grep 死后 slog2info 下次写触发 SIGPIPE 随退）；`slay` 是交互式的，须 `-f -Q` 非交互；秒级分桶计链数会被孤儿重印污染，须先清场。

**修复**（agent qnx.rs，三处）：
1. `login_and_start_stats` 启动先 `slay -f -Q slog2info` 清场孤儿（grep 随管道 EOF 自尽；同时收编 daemon 被 SIGKILL 时的残留——自愈路径）；
2. 会话 teardown 停链 echo> 后追加 `kill $!` 收本会话 tailer（真机验证 Broken pipe → pidin 计数归零）；
3. `--qnx-stop`（stop_once）观察前先清场（防孤儿重印 frame 行造成假阳性误停链）+ 两条退出路径均 `kill $!` 收观察 tailer。
协议 bump v9（无 wire 变化，仅为强制重推替换设备端旧 daemon）；行级全等去重保留作兜底。

**真机验证**（SS3，`--remote hppc`）：
- 改动前基线：v8 会话（20s --gpu @500ms）后 QNX slog2info 孤儿 **0→1**（泄漏实锤）；
- v9 会话 #1：启动即清掉 v8 遗留孤儿，32 GPU 样本零自愈零错误，退出后孤儿 0；frame 链已停（观察窗零 frame 行）、proc 常驻 ×1@500ms（会话写入重相位 1000→500，设计基态）；
- 注入孤儿（手工起 tailer 不收尸）→ `--qnx-stop`：清场孤儿 + 正确报「frame 未在跑，不动链」+ 自收观察 tailer → 孤儿 0；
- v9 会话 #2（--gpu --gpu-mem @500ms）：27 gpu + 25 gpuproc 样本零自愈，退出后孤儿 0。
- host 侧测试 96+9+5+2 全绿，clippy/cargo doc 零警告。

**遗留**：无。历史 E 节「QNX 双会话并发交互」条目中的密度 ×2 现象按旧模型归因（多链），新认知下应为互相重相位/重印 artifact——条目保留为历史观察记录，不阻塞。（gltf APK 的 RemoteServer 崩溃属 filament 样例自身问题，与本工具无关，移出 backlog——用户 2026-09-11 指示；规避方式仍为测前 `--force-stop` 清场。）

---

## 2026-09-11(5) — DMA-BUF 拆分实施落地（协议 v8，`feat/dmabuf-split` 合 main）

**任务**：按 WORKSPACE D 节交接条目（①-⑧）完成 Private Other 拆分 DMA-BUF 的剩余实施。

**commit**：758b5cc（前会话 WIP：agent 聚合函数）→ e5071da（本会话全链路）+ docs 收尾。

**完成内容**：
- **agent**（mem.rs）：`sample_memory` Full 模式（≥500ms）接线——root 下扫全量 smaps 按 VMA 名聚合 Pss 单列 `dmabuf` 随 mem 事件发出，`other = bd.other.saturating_sub(dmabuf)`；Smaps/DumpsysFallback/非 root（smaps 不可读）dmabuf=0。**修正 WIP 解析器**：头行从固定列切片（`line.get(73..)`，列宽随地址/inode 位数浮动不可靠）改为按字段解析（hex 开头 + 首 token 含 `-` 判头行，第 6 字段首 token 前缀匹配 `/dmabuf`/`[anon:dmabuf`）。
- **协议 v8**：mem 事件增 `dmabuf` 字段（纯增字段——core `AgentEvent::Mem`/`MemoryDetails` 均 `#[serde(default)]`，老 daemon 缺省可解析、老 host 忽略未知字段，双向兼容）；两侧版本常量同步 bump，版本契约注释更新为「v3 起向后兼容」。
- **CLI**：verbose 打印加 DMA-BUF + Private Other 改名 Other；退出内存图表加 DMA-BUF 序列（9 条）。
- **GUI**：`map_event` 透传；前端实时面板加 `└ DMA-BUF` 行、Private Other 改名「其他」（`setLive` 动态建行零 HTML 改动）。
- **CSV**：`mem_row` 加 `DMA-BUF (MB)` 列（Graphics 后），表头 `Private Other` 改 `Other`（10 列）。
- **测试**：agent `parse_dmabuf_pss` 真机片段单测 2 条（`/dmabuf:` 与 `[anon:dmabuf` 前缀/Pss 累计/非 dmabuf 不计/空输入）；core Mem 事件 v8 带字段与缺省两态解析。agent 测试经 scp→hppc→adb push 到 SS3 设备执行（主机 compile_error 限 Android 目标），32 全绿；host 95+8+5+2 全绿，clippy/cargo doc 零警告。

**真机验证**（均经 `--remote hppc`）：
- **SS4 gltf（root，500ms Full）**：稳态 `DMA-BUF 615.6MB / Other 9.0MB`，8 分类合计 728.3 ≈ PSS 728.4 ✓（与勘察 114 个 dmabuf VMA ≈614MB 吻合）。**发现并已录档**：场景加载尖峰期 dumpsys 与 smaps 两次快照不同步，dmabuf 可暂超 other（other saturating 归零、合计可暂超 PSS，如 939.9 total 时 DMA-BUF 887.1）——选择如实反映不钳制（钳制会在分配增长尖峰低报 dmabuf），CLAUDE.md 内存采样节已写明该口径。
- **SS3 gltf（root，500ms）**：DMA-BUF 533.9MB / Other 11.9MB，合计 649.4 = PSS ✓ 回归通过。
- **非 root SS2MAX（shell，XPERF_NO_AUTO_ROOT=1，500ms）**：dmabuf=0 不破坏，7 类合计 622.9 ≈ PSS 622.8 ✓（该机 gltf 大头 512.9MB 落 Graphics 桶——kgsl 节点名命中，与 SS4 GVM 无 kgsl 节点的分布差异互为印证）。验证后已 `adb root` 恢复设备状态。
- **低间隔（SS3 root，100ms Smaps 路径）**：48 样本分类全 0 含 DMA-BUF=0，PSS 正常流动，CSV 10 列表头正确。

**遗留**：gltf APK 去 RemoteServer、QNX proc 链停链命令两项旧遗留不变。

**同日复核（用户要求 review + 测试覆盖评估）**：
- **SS2MAX 双计风险实锤排除**（交接验证矩阵的缺口）：root 下 Graphics 512.9MB / DMA-BUF 0.0MB，8 类合计 623.4=PSS ✓——该机 gltf 大缓冲的 468 个 VMA 以 kgsl 设备节点命名进 Graphics 桶，仅存的 4 个 `/dmabuf:` VMA 的 Pss 均为 0（实测 smaps 原文），不存在"dumpsys Graphics 与 /dmabuf 前缀同时命中"的重叠；三平台（A11 kgsl / A12 QNX / A16 GVM）记账均不重叠。
- 补测：GUI `test_map_event_mem_dmabuf`（v8 透传+缺省 0 两态）+ core `test_mem_row_dmabuf_column`（表头/数据行 10 列对齐 + DMA-BUF 列位置锁）。
- 记录不修（低危/cosmetic）：①v7 host 连 v8 daemon 时 other 已扣减但无 dmabuf 显示行 → 分类合计观感 <PSS（纯增字段对老 host 的固有语义，daemon 60s 空闲自杀+版本收敛后绝迹）；②parse_dmabuf_pss 依赖 smaps 字段顺序（Pss 先于 "Anonymous:" 等 A-F 开头行——内核 ABI 稳定）；③JS↔Rust 命令参数 camelCase 契约无静态检查（gpuMem bug 类，Tauri 无校验基建，靠 review）。
- 附带修复（review 前用户实撞）：`4d3a421` GUI start_sampling 前端 `gpumem`→`gpuMem`（v7 引入的键名错误，所有设备开始采样均报错）。

**同日 GUI 目验闭环**（macOS 本机 GUI + `--remote hppc`，System Events AX 树读取 + AXPress 点击——screencapture 无屏幕录制权限改走 AX 树，顺带获得**可点击验证能力**）：
- **gpuMem 修复真机闭环**：手动「停止」→「开始监控」（走前端 currentFlags）→ 状态栏「监控中」，无 missing key 错
- **v8 面板渲染**：实时数值表 `└ DMA-BUF 619.2 MB` / `└ 其他 9.0 MB` 行渲染+数值正确（8 类合计 731.9 ≈ PSS 732.0）；SS2MAX tab 的 GPU 显存 checkbox `enabled=false` + tooltip「SS2MAX 平台无 GPU 显存数据源」
- **按钮端到端全通**：保存基线→对比基线（报告面板渲染，PSS 732.0/732.5 持平）；重启应用→冷启动面板两行实测（TotalTime 259/257ms，重启后 PID 11473→12716 全链路自动重解析）；获取 root（SS2MAX unroot + `XPERF_NO_AUTO_ROOT=1` 起 GUI：徽章 shell（非 root）→ 点击 → 状态栏「已获取 root 权限（uid=0）」→ 徽章翻 root → 设备侧 id=0 确认）；root 设备上按钮按设计 disabled（SS4 页点击为 no-op）
- 静态兜底：全部 36 个 `el()` class 引用在 index.html 模板均存在；全部命令的 invoke 参数 camelCase 核对无其他违例（`intervalMs`/`tracePath`/`dataPath` 正确）
- 工具沉淀：`/tmp` 下 press/findText/status 等 .scpt 已清理；方法可复用——System Events 遍历 AX 树读 static text 值 + `perform action "AXPress"` 点击按钮（窗口须在前台；tab 切换后偶发 AX 树剪枝只剩顶栏，重启 GUI 恢复）

---


## 2026-09-11(4) — GPU 缓冲记账原理勘察 + Private Other 拆分 DMA-BUF 方案（WIP 交接）

**任务**：用户问"为什么 GPU 缓冲不走 Android Graphics 记账"，随后提出 Private Other 名称可读性差、要更好的统计方案；用户要求交接新会话完成。

**勘察结论**（SS4 gltf pid 11473，smaps 全量聚合 4452 mapping）：
- **meminfo Graphics 行是按 VMA 名字匹配图形设备节点的启发式**（Qualcomm 主要 `/dev/kgsl-3d0` 等 = GPU 命令缓冲/memstore 小块），不是"应用 GPU 内存"的权威口径。
- gltf 的 **614MB PSS = 114 个 `/dmabuf:` VMA**（gralloc/dma-heap 缓冲 CPU mmap）——不匹配 Graphics 桶 → 落 Other mmap → App Summary **Private Other**（无语义兜底桶，占该应用 PSS 85%）。
- 内核不知道 dmabuf 的"图形用途"（语义在 userspace 分配器）；AOSP 正路是 `/proc/<pid>/dmabuf`（CONFIG_DMABUF_SYSFS_STATS，带 exporter 名）——**此 GVM 内核未编译**（实测不存在）→ 只能 root 下自扫 smaps 按 VMA 名拆。
- GPU 内存的权威口径 = `dumpsys gpu` Memory snapshot（`--gpu-mem` 采的 672MB）；两者本就该分开读。
- 排查工具沉淀：`adb shell cat /proc/<pid>/smaps` 拉回本地后 python 聚合（VMA 头行正则 `^[0-9a-f]+-[0-9a-f]+ \S+ \S+ \S+ \S+ *(.*)$` + Pss/Rss 行），比 dumpsys 全量表更能定位到 mapping 名。

**方案与进度**：见 WORKSPACE D 节「内存 Private Other 拆分 DMA-BUF 分类」——`feat/dmabuf-split` 分支（758b5cc，已推 hppc）含 agent 侧 `read_dmabuf_pss`/`parse_dmabuf_pss`（未接线）；剩余 ①-⑧ 步骤（接线/协议 v8/core 类型/GUI/CLI/CSV/真机验证/文档）在交接条目中逐条列明。

**本会话早些时候完成**：内存分类显示补全（5c21047，CLI/GUI 补全 7 类——解析无 bug，纯显示漏列）；GPU 显存独立开关 + SS2MAX 禁用 + FPS 默认勾选（96d875a，协议 v7）。

**遗留**：dmabuf 拆分待新会话按 D 节交接条目实施；gltf APK 去 RemoteServer、QNX proc 链停链命令两项旧遗留不变。

---

## 2026-09-11(3) — 内存分类显示补全（Private Other 大头漏列致"PSS 与分类之和差距大"）

**任务**：用户反馈 SS4 内存显示的分类之和与 PSS 差距很大。

**结论**：**解析无 bug**——SS4 App Summary 与解析器完全匹配（真机验证：7 分类相加 = TOTAL PSS 分毫不差）。差距来自**显示层漏列**：CLI 只打 Java/Native/Code/Graphics、GUI 面板只列 Native/Java/Code，而 gltf viewer 的大头在 **Private Other（~625MB，场景缓冲落在 Other mmap）**，Stack/System 也未列——SS4 上该应用 Private Other 占 PSS 的 ~85%，漏列后"分类合计 38MB vs PSS 729MB"。

**修复**：CLI verbose 打印与 GUI 实时面板补全 App Summary 全 7 分类（Native/Java/Graphics/Code/Stack/Private Other/System——构造上合计恒等于 PSS）。数据链路（agent 解析/协议/CSV/基线）零改动——一直是对的。

**真机验证**：SS4 gltf `738.6 MB (Java 22.9, Native 76.9, Graphics 0.0, Code 8.1, Stack 0.9, Private Other 624.9, System 4.8, RSS 895.2)`——7 项合计 = PSS。全量测试 94+8+5+2 绿。

---

## 2026-09-11(2) — GPU 显存独立开关 + SS2MAX 平台禁用 + FPS 默认勾选（协议 v7）

**任务**：用户要求——SS2MAX 无 GPU 显存源，其"GPU 显存"勾选框应不可勾选；性能指标默认勾选 FPS。

**commit**：见本次合并（`feat: GPU busy 与显存拆分独立开关（协议 v7）`）。

**实现**：
- **协议 v7**：GPU busy（`--gpu`）与 GPU 显存（`--gpu-mem`）拆分独立开关——此前 `--gpu` 隐含显存补采（DumpMem 保底路径），拆出后无源平台可按需禁用。agent：`GpuPath::DumpMem` 变体删除（detect 只探 busy 源，无源 err 禁用）；gpumem 补采臂独立（`--gpu-mem` 一次性探测 Memory snapshot，无则 err "平台无数据源"）。
- **平台到前端**：`AdbDevice.platform` 字段（product 推导，`platform_from_product` 纯函数与 detect_platform 共用；同一次 `adb devices -l` 输出内推导，零额外 adb 调用）；devices payload 携带 → 前端 `applyPlatformCaps` 按 platform 禁用 checkbox（SS2MAX → GPU 显存 disabled + 强制不勾 + title 提示"平台无 GPU 显存数据源"）。
- **前端**：GPU 显存独立 checkbox（图/开关与 busy 解耦）；FPS 默认勾选。
- CLI `--gpu-mem` flag 同步。

**真机验证**：SS2MAX `--gpu --gpu-mem` → busy 80%（kgsl 正常）+ 显存如实 err 禁用、零 mem 事件；SS3 双开 → QNX busy 25%/util 20% + GPU Mem 609MB/2366MB 并行；SS4 `--gpu-mem` 单开 → 9 事件 672MB/4952MB（无 ligfx busy 干扰）。全量测试 94+8+5+2 绿（+platform_from_product 单测），clippy/doc 零警告。

**遗留**：GUI 渲染目验（checkbox 禁用态/FPS 默认勾选）待用户 rebuild 后确认；SS2PRO 无真机未实测（同为 SS2 系列，前端禁用仅按 SS2MAX 收敛——如 SS2PRO 实测有源再放开）。

---

## 2026-09-11(1) — daemon bind 自愈 + 高版本替代低版本（协议 v6，`fix/daemon-upgrade`）

**任务**：用户要求从代码层解决双启动 bind 竞态（前一晚版本战曾致"无人监听"残留态，需手动清场），并要求 agent 具备完善的升级逻辑（高版本替代低版本）。

**commit**：9685438（agent bind 自愈 + host 版本契约）→ 本次（文档）。

**实现**：
- **daemon bind 失败自愈**（`run_daemon`）：撞上已有 daemon 时探在场者（连接读 hello，含版本）——健康且 ≥ 本版本 → 让位退出；健康但 < 本版本 → 清场升级接管；不可达/挂死（SIGSTOP 类持有 socket 不应答，2s 探活超时）→ 清场接管；重绑重试 5×200ms。任何启动交错收敛到「恰好一个健康 daemon 且为最高版本」。清场 = /proc cmdline 匹配 `xperf-agent`+`--daemon` 后 SIGKILL 除自身外。
- **host 版本契约变更**（`ensure_daemon`）：接受 daemon ≥ 自身版本（wire 自 v3 稳定，此后 bump 均为行为差异不改命令/事件格式），仅低于时 suicide+重推——消灭多宿主混跑降级战；wait_probe 8→10 次覆盖自愈接管耗时。

**真机验证（SS2MAX，四场景 + 端到端）**：① v5 daemon 在场 + v6 host → "协议版本过低…重推升级" + 采样通；② v5 持 socket + v6 直接启动（模拟并发竞态）→ "在场 daemon v5 低于本实例 v6，升级接管" → v6 监听；③ SIGSTOP 冻结 daemon → "无响应（死亡/挂死），清场接管" → 新 daemon 服务（单 daemon 单 socket）；④ 等版本 → 让位（daemon 数恒 1）；端到端 60fps。全量测试 94+8+5+2 绿，clippy/doc 零警告。

**遗留**：过渡期——exact-match 时代旧宿主（≤v5）连 v6 daemon 仍会自杀重推降级，各宿主升级一次 v6 后绝迹（已记 WORKSPACE E 节）。

---

## 2026-09-10(7) — SS4 FPS 根因反转：A16 --latency 图层名须带 hex 前缀，设备端路径恢复（agent v5）

**任务**：用户反馈编译运行后仍拿不到 gltf FPS；用户提供线索 `getfps -w "<图层全名>"` 可取 SS4 FPS。

**commit**：1486463（fix/ss4-fps-hex：agent fps.rs 查询名双轨 + 撤 SS4 短路 + 删 host frametimeline 通道，协议 v5）→ 本次（文档）。

**关键结论**：
- **v4 结论反转**：SS4 `--latency` 并非平台阉割。逆向 getfps 二进制（strings 见 `--latency-clear` 调用）发现其底层即 `--latency`，且传 `--list` 原始行的 `<hex> <name>` 全名——A/B 直测：带前缀 65 行真帧数据 vs 只传干净名 1 行刷新周期。此前所有 --latency 测试都用了剥壳后的名字，故恒空。
- **修复**：agent fps.rs `FpsLayerState` 拆 name（事件用）/query（--latency 查询用）双轨——A16 `RequestedLayerState` 包装行 query 保留 `<hex> ` 前缀，旧格式平台 query=干净名（零变化）；全量 dump 发现的层借 --list 同名行升级 query。SS4 --fps 短路撤销（v4 引入），hostchan frametimeline 通道删除（display 合成流口径全面劣于 per-layer），协议 bump v5。
- **"拿不到 FPS"用户侧根因**：①用户的 release GUI 从 main 编译（v5 未合入时 = v4 短路版）；②**v4 GUI 与 v5 CLI 并存打协议版本战**——各自 ensure_daemon 发现场上 daemon 版本不符即 suicide+重推，互相杀死对方 daemon 无限循环（实测设备 2 daemon 进程/6 socket 堆积、会话 hello 后流冻结）。协议 bump 升级时旧宿主必须退出（已记 WORKSPACE E 节）。
- getfps 输出口径（顺手勘察）：`count :N, max/min 帧间隔 ms`，内部 `--latency-clear` + 差值；`/system/etc/layer_mapping.cfg` 提供常用窗别名（svm/launcher/hud…）。

**真机验证（--remote hppc，单宿主干净环境）**：SS4 稳态 59-61fps per-layer（#367/#429/#519/#549/#579 逐次 Surface 重建）；杀进程链路 stdout 模式 exit=1/noproc=24/新 pid 续采，daemon 模式 #549→#579 新 pid 续采；--gpu --fps 双通道共存（37 fps + ligfx busy 34.2%/进程 10.9%）；SS2MAX 回归 60fps 旧格式不变、SS3 回归 60fps。全量测试 94+8+5+2 绿，clippy/doc 零警告。

**遗留**：无。

---

## 2026-09-10(6) — SS4 指标适配实施（WORKSPACE H 剩余，`feature/ss4-metrics`）

**任务**：按 `docs/DESIGN-ss4-metrics.md` 任务 A-E 实施 SS4 指标适配。

**commit**：7a3e9fb（任务 A：FPS frametimeline host 通道 + agent ss4 fps 短路 + 协议 v4）→ 9004f96（任务 B：GPU ligfx host 侧通道）→ b8754bf（任务 D 附带修复：simpleperf cpu-clock + 千分位解析）→ d9dc84a（任务 E 文档收尾）→ 7e0a748（review 修复：独立 review 2 严重 4 一般全修——ligfx EOF 热重连/独占登记重连竞态（宽限接管）/阻塞读 stop 看门狗/err 风暴上限；frametimeline 水位无条件推进/jank 门槛对齐；pull 30s 超时/僵尸回收等建议项）。

**实现**：
- **架构（任务 A/B 共用）**：host 侧线程合成 `AgentEvent` 经 mpsc 汇入 `AgentStream.extra_rx`，`next_event`/`next_event_batch` 统一取出（agent 心跳空行保证投递延迟 ≤ 一个采样间隔），**CLI/GUI 消费端零改动**；通道由 `spawn_agent` 按 SS4+指标开关启动，生命周期随流（析构→发送失败/`ping_stop` 退出），断连重连由 `reconnect_agent→spawn_agent` 重启。新模块 `xperf-core/src/hostchan.rs`。
- **任务 A（FPS）**：5s 窗循环「perfetto `-c -` stdin frametimeline-only 配置 → pull → trace_processor 单 SQL（`actual_frame_timeline_slice where layer_name is null`）→ 水位去重汇总 fps/jank（口径同 agent）」。pid 取 pidof 首进程（应用不在时不发事件），layer 固定 `(display)`。agent 侧 `--platform ss4` 下 `--fps` 短路（不再空转图层发现），协议 bump v4 强制重推。
- **任务 B（GPU）**：`bridge::gateway_for_android` 拿 MindRT 网关 → `adb shell logcat -T 0 -s ligfxprofilerd` 流式读（-T 0 不回放历史缓冲）→ Sys→Gpu / `GVM_<comm>-<id>`→GpuProc（pidof+/proc/pid/comm 15 字符截断归因，miss/60s 重建映射）。断流 2s 重连；进程内按 serial 独占（只读通道无 QNX 式写冲突）。显存保底仍由 agent dumpsys gpu 补采并存。
- **任务 D 附带**：SS4 GVM PMU 未虚拟化（cpu-cycles 8s 仅 6 样本）→ simpleperf 按平台自动 `-e cpu-clock`（RecordedStack 加 event 字段，报告头如实标注）；顺手修复 `parse_sample_stats` 千分位逗号解析（新版 simpleperf `4,016` 被旧解析截断为 4）。

**真机基线（SS4 localhost:5559 经 --remote hppc，gltf viewer）**：
- FPS frametimeline：稳态 59.8fps（299 帧/5s 窗，jank 0）；杀进程即停发、重启自动跟新 pid、**GVM reboot 后通道随重连自动恢复**；SS3 agent 路径回归 60fps 不受影响
- GPU ligfx：busy 33.8%（系统）/ 11.0%（gltf 进程归因正确）；显存 dumpsys 保底并存（721.8MB/整机 4653MB）
- 九项矩阵 root/非 root 两态：CPU/内存/FPS/GPU/显存/IO（root）/网络 全通；**freq/thermal 为 GVM 平台限制**（无 cpufreq sysfs——hello maxkhz 全 0 根因；无 thermal zones + HAL Ready=false），agent 探测 err 禁用符合预期；非 root 下 frametimeline（perfetto shell 可录）与 ligfx（MindRT root）均可用，IO 如期禁用
- C 类：trace/冷启动 246ms 复用既有结论；simpleperf cpu-clock 3908 样本/9s 三视图完整；基线保存→对比全链路（CPU/PSS/FPS/Jank/GPU 9 项持平）

**核销**：ligfx Frequency 单位（恒 1000 定频占位/标注存疑，原样透传；`sampling_interval_ms` 调小致停输出勿调）、hello maxkhz 全 0（GVM 无 cpufreq sysfs）。

**遗留**：无新阻塞项；GUI 侧 SS4 事件流经命令级验证（与 CLI 同一 AgentStream 路径），未跑 GUI 进程目验。

---

## 2026-09-10(5) — force-stop 工具化 + H 指标适配交接稿定稿

**任务**：杀进程清场功能落地；review 代码与文档，产出 H 节交接材料。

**commit**：c5d2cc2（feat: CLI --force-stop + GUI「停止应用」）→ 本次（交接稿 + 文档）。

**关键结论**：
- **gltf RemoteServer 崩溃根因（源码级）**：`MainActivity.onCreate`（MainActivity.kt:154）无条件 `RemoteServer(8082)`（filament-utils ws 调试服务器）→ Java 侧 `nCreate==0` 即 `IllegalStateException`（RemoteServer.java:43，无重试无降级）→ native CivetWeb bind 失败（典型 EADDRINUSE）；正常手机崩溃=进程死亡端口自愈，SS4 Application Error 保进程 → 端口不放 → 崩溃循环。修复建议：onCreate try-catch 置 null 或删除（viewer 本体不依赖）。
- **交接稿 `docs/DESIGN-ss4-metrics.md`（v1.0）**：任务 A FPS frametimeline 流式化（SS4 专属兜底；配置/pbtxt 权限目录/9.5KB/s 开销/并发无冲突/NULL 流归因语义全实测）→ 任务 B GPU ligfx host 侧通道（经网关读 MindRT logcat；真机行样例；parse_line 逻辑移植 core；`persist.vendor.ligfxprofiler.sampling_interval_ms` 调窗；Frequency 单位核实）→ 任务 C 九项矩阵（hello maxkhz 全 0 待查）→ 任务 D C 类（trace✅/冷启动✅/simpleperf 待测/基线）→ 任务 E 文档收尾。**数据源已全勘察，新会话勿重复**。
- **review 记录**：bridge 边缘态（bootstrap 失败冷却期 MindRT 漏进设备列表）入 WORKSPACE E 节不修项；agent `gpu/ligfx.rs::parse_line` 与真机行格式吻合可直接移植；`gpu/mod.rs:66` 注释链待任务 B 同步。

**遗留**：H 指标适配按交接稿任务 A-E 实施（新会话）。

---

## 2026-09-10(4) — SS4 FPS/GPU 为 0 排查：A16 图层发现修复 + 两项平台限制确证

**任务**：用户反馈 GUI 经 SSH 连 SS4 看 FPS 与 GPU busy 均 0，排查定位。

**commit**：1f74a73（agent fps.rs A16 包装格式修复）。文档（WORKSPACE E 节 + 本条）随提交。

**结论**（三层分离）：
- **FPS 根因①（已修，真 bug）**：Android 16 `dumpsys SurfaceFlinger --list` 行带 `RequestedLayerState{<hex> <name> parentId=… …}` 包装，agent `parse_list_layers` 未拆壳 → 整行作层名进 `--latency` 必无数据 → 图层发现全灭。修复 1f74a73（拆壳 + 元数据截断；旧格式逐字节兼容）；解析逻辑 rustc 独立断言验证（真机 A16 行，主机无法跑 agent 单测）；SS3 60.8 / SS2MAX 60.5 真机回归。
- **FPS 根因②（平台限制，未修）**：SS4 的 QCM SDE 定制 SF 构建 `--latency` 对**全部图层**恒空（全 BLAST 层实测只有刷新率行）；perfetto FrameTimeline 正常（`--trace 5` 实测 303 帧 ~60fps avg 15.44ms，顺带验证 C 类 trace 在 SS4 全通）。修复后 agent 图层发现正常但缓冲恒空 → 不发事件（不伪造 0）。SS4 FPS 需新数据源（perfetto frametimeline 流式化）或标不支持——入 WORKSPACE E 节 + H 节评估。
- **GPU busy=0（已知 R8 gap）**：ligfxprofilerd 在 MindRT 侧（S0 已证 GVM logcat 无输出）→ agent ligfx 通道起不来，自动降级 dumpsys gpu 显存（**SS4 显存可用**：gltf 734.9MB / 整机 4819MB，Memory snapshot 段存在）——busy 需 H 节的 host 侧 ligfx 通道。
- 附带发现：gltf viewer 在 SS4 曾崩溃（Application Error 窗，疑 GVM reboot 时应用被杀后遗留），force-stop 重启恢复；SS4 `--list` 图层号随 Surface 重建递增（#367→#418，agent 重发现机制覆盖）。

**遗留**：SS4 FPS 数据源选型 + ligfx host 侧通道（WORKSPACE H 剩余项）。

**补充（2026-09-10 晚，gltf 无 FPS 深挖）**：gltf 反复崩溃 = **RemoteServer 8082 端口冲突**（filament-utils 调试服务器，Application Error 窗遮挡 → 无上屏帧）；force-stop 清场重启后恢复。**frametimeline 归因粒度局限确证**：BLAST 层（SurfaceView 直渲应用）帧折叠进 layer_name=NULL 的 display 级合成流（vsync 合并），per-layer `TX -` 行仅覆盖非 BLAST 系统窗——A/B 对照：gltf 动画时 NULL 359 帧/6s≈60fps，杀掉后 122 帧/6s≈20fps（系统底噪）。**单动画源场景 NULL 流 ≈ 应用 FPS**（基线扣除系统动画）；真 per-layer 归因与 --latency 同被 QCM 构建阉割。

---

## 2026-09-10(3) — SS4 adb 自动桥接实施 S1-S6（WORKSPACE H，`feature/ss4-adb-bridge`）

**任务**：按 `docs/DESIGN-ss4-adb.md` §9 实施 bridge 模块（S1-S5）+ 真机回归（S6）。

**commit**：5018c11（S1 bridge.rs 核心）→ 03aa440（S2 utils 集成）→ e184ddc（S3+S4 root 链路与自愈）→ fa5cc6f（S5 GUI 过滤）→ f957d8c（review 修复：acquire_root 补 Ss4 兜底 + bootstrap 每轮重读规则防多网关端口撞车）。文档收尾（CLAUDE.md 桥接段/WORKSPACE/SESSION）随合并提交。

**实现要点**（详见 CLAUDE.md「SS4 adb 自动桥接」）：
- `bridge.rs`：refresh（forward --list 恢复映射/已知网关 connect 每轮都试/候选 bootstrap 60s 冷却/≤2 轮有界重枚举）+ reconnect（按规则存在性重建+connect）+ 纯函数 parse_forward_list/allocate_port/classify_device（9 单测）；serial 恒 `localhost:<port>` 字面（serial 稳定性不变量）
- 集成点恰好四处：list_adb_devices 尾部 hook、device_online localhost 分支、try_adb_root/acquire_root Ss4 兜底（rootandroid.sh）、pick_device 过滤 is_gateway
- GUI：`visible_devices` 统一收敛三处 payload（devices_json 内聚/list_devices/监视器 diff 前）；ensure_device_online 拒绝网关并指引；前端零改动

**S6 真机回归基线**（全部 --remote hppc，SS4+SS3+SS2MAX 三机同连）：
- 清桥接状态后 CLI 自动 bootstrap 成功：多台报错清单只列 3 台 Android（`localhost:5559 HU_Smart_space_4_0 Android 16` 自动桥接出现，MindRT 被过滤）
- `--device localhost:5559` 采样：auto-root ①直连成功、平台 SS4/12 核/hello root、CPU 500ms 样本（gltf ~10-14%）/线程明细/流式 CSV/退出图表全通
- **采样中 GVM reboot**：连接断开 → bridge 自愈 → 自动重连恢复；重连竞态中 root ①直连与 ②网关 rootandroid.sh 兜底**均真实触发成功**
- 三机并行采样（SS4+SS3+SS2MAX 各一 CLI 进程）：18/21/14 样本互不干扰
- 静态：全量测试 94 绿（core 90+7ignored/gui 8/cli 5/xrm 2 减重复计数）、clippy 0、cargo doc 0

**遗留**：九项指标逐项实测 + C 类回归 + platform/ss4.rs 桩补实（WORKSPACE H 剩余项，独立会话）——ligfx 通道须改 host 侧经网关读 MindRT logcat（S0/R8）；ligfx Frequency 恒 1000 单位存疑；hello maxkhz 全 0 待查。GUI 桥接路径为命令级验证（未跑 GUI 进程目验 tab 行为，payload 过滤由 devices_json 单点收敛）。

---

## 2026-09-10(2) — SS4 S0 真机预验证（WORKSPACE H，无代码）

**任务**：按 `docs/DESIGN-ss4-adb.md` §8 清单在 hppc 上手工 adb 预验证 R1-R5/R7/R8，结论回填设计文档。

**环境**：SS4 接 hppc（MindRT `42087266b1f` usb:1-12.3，无 product 字段）+ SS3 + SS2MAX 三机同连；纯 ssh hppc 手工 adb，无代码改动。

**结论**（设计文档已回填 v1.2，**方案 A 主设计零改动，可进 S1-S6**）：
- **R1 ✅（最高风险排除）**：`forward tcp:15559 localabstract:xperf-agent` 穿 5557 中继成功，nc 收 agent hello（12 核/version 2）——F1（agent --tcp-port 模式）不需要。
- **R2 ✅**：真机 `adb devices -l` 形态与设计/单测逐字一致（桥接后 `localhost:5559 product:HU_SS4 model:HU_Smart_space_4_0`）。
- **R3 ✅ 比假设乐观**：标准 adb `adb -s localhost:5559 root` **直接成功**（无 TCP root 限制）→ root 主路径；MindRT `rootandroid.sh`（/system_ext/bin）兜底也实测成功。
- **R4 ✅ 比假设乐观**：MindRT 未 root（adbd uid=2000）即可 forward+connect——bootstrap 的 MindRT root 非必要，保持 best-effort。
- **R5 ✅**：`adb -s localhost:5559 reboot` 只重启 GVM；serial **不消失**（offline 态）→ **~24s 自动回 device**，forward 规则/connect/中继全程存活，零干预自愈（connect 返 "already connected" 为无害 no-op）。**另实测：MindRT adbd 重启（root 提权）清 forward 规则**（relay 是 MindRT 上一个 adb 进程 LISTEN 127.0.0.1:5557，跨重启存活）→ refresh 重建路径必需。
- **R7 ✅**：**Android 16**（FPS 图层按 A12+ BLAST 形态）；push/install/`am start -W`（gltf viewer COLD 310ms）正常。
- **R8 ⚠ 推翻**：ligfxprofilerd 在 **MindRT 侧**（/usr/bin/ligfxprofilerd），**GVM logcat 无 ligfx 输出** → agent `gpu/ligfx.rs` 通道不成立，走 fallback：host 侧 `adb -s <mindrt> shell logcat -s ligfxprofilerd` 流式读取（H 节 GPU 项实施）。数据形态：~5s/帧块；Sys=Frequency/Busy/Queued/Utilization；Proc=`GVM_<comm 15字符截断>-<会话id>`（id 非 GVM pid、跨重启变化，归因按 comm）；负载对照 gltf viewer Global Busy 11%→29%。**Frequency 恒 1000**（GPU VFIO 直通 GVM，MindRT/GVM 均无 kgsl/devfreq 节点可对照；Hz 标注疑似 MHz，单位与定频问题留 H 节 GPU 项）。

**遗留**：进 S1（bridge.rs 核心）→ S6，实施计划与测试设计见 `docs/DESIGN-ss4-adb.md` §9/§10；ligfx Frequency 单位核实并入 H 节 GPU 项。设备收尾状态：MindRT 已 root、GVM 已 root（rootandroid.sh）、gltf viewer 已装 SS4、测试 agent daemon 已随 GVM 重启清除。

---

## 2026-09-10 — SS4 adb 桥接方案设计（WORKSPACE H 前置，无代码）

**任务**：读飞书《SS4.0 (8797) USB ADB调试指南》，设计 SS4（SA8797P）双系统拓扑的 adb 接入方案；一轮自 review 后定稿，交接新会话实施。

**产出**：`docs/DESIGN-ss4-adb.md`（设计稿 v1.1）+ WORKSPACE H 节定案说明。无代码改动。

**关键结论**：
- SS4 = **MindRT（Linux PVM，USB 可见）+ Android（GVM，USB 不可见）**；Android 须经 `adb forward tcp:5559 tcp:5557`（建在 MindRT 上）+ `adb connect localhost:5559` 桥接成伪设备——标准 adb 即可（文档方法 2），厂商魔改 tool（mac 无现成包）非必需。
- **方案 A（host 侧自动桥接）定案**：新模块 `xperf-core/src/bridge.rs` + `AdbDevice.is_gateway` + 四个集成点（`list_adb_devices` 尾部 hook / `device_online` 自愈 / `try_adb_root`+`acquire_root` Ss4 分支 / `pick_device` 过滤）；Android 以 `localhost:5559` 进入现有全链路，下游语义零变化（SSH 远程 adb-server-侧语义 / 多设备 `-s` 路由 / trace/stack/冷启动天然兼容）。SS2/SS3/手机混连零影响（网关探测只命中无 product 的 USB 设备）。
- **root 链路**：`adb -s localhost:5559 root` 疑似被 TCP root 限制拒（⚠R3 待实测），主路径 = 经 MindRT `adb shell rootandroid.sh`；MindRT 自身 root 在 bootstrap 内 best-effort（XPERF_NO_AUTO_ROOT 门控）。
- **review 修正五处**：冷却语义分离（bootstrap 探测失败 60s 冷却 vs 已知网关每 3s 重试）、GUI 网关过滤实为**三处** payload 且监视器须在 diff 之前过滤、serial 稳定性不变量（connect 目标串恒 `localhost:<port>` 字面，勿用 127.0.0.1）、§5.2 forward 规则存亡断言软化（两种结局 refresh 均收敛）、R8 并入 ligfx Frequency 单位核实。
- **前端已核实零改动**：serial 只进 `dataset.serial` / `pkgList-<serial>` 元素 id（`.id=`/`setAttribute` 赋值，无动态 id 的 querySelector），`localhost:5559` 冒号安全。
- **最高风险 R1**：`localabstract` forward 能否穿 5557 中继——S0 预验证定生死；fallback F1 = agent 加 `--tcp-port` 监听模式 + `forward tcp:0 tcp:PORT`。

**遗留**：S0 预验证（hppc 手工 adb，R1-R5/R7/R8，结论回填设计文档）→ S1-S6 实施见设计文档 §9；九项指标/C 类回归按 WORKSPACE H 3-4 独立成会话。环境备忘：本机 lark-cli 不在 PATH（Doubao 沙盒内副本无 auth 子命令），可用版从 GitHub `larksuite/cli` releases 装并配 `~/.lark-cli`（appId cli_a943547e3e791bc8，keychain 存 secret）；用户 token 曾过期，本次已重新授权（docs 域）。

---

## 2026-09-09(3) — 非 root 设备支持全链路（WORKSPACE G 闭环）

**任务**：探索无 root 时各指标可用性并落地能力降级；加「获取 root」按钮（结果反馈）；auto-root 收敛为非车机不默认提权。

**环境**：SS3/SS2MAX 接 hppc（SSH 远程后端操作）；两台 `adb reboot` 掉 root（uid=2000 shell）作探索/回归环境，收尾已恢复 root。

**commit**：9d26384（矩阵文档）→ 19ecef9（auto-root 仅车机 + XPERF_NO_AUTO_ROOT）→ ea3986f（协议 v3 hello.root + 内存降级 + RSS 兜底）→ 9c34b23（QNX 内嵌 telnet 去 busybox + --qnx-stop）→ ec2de2b（GUI 徽章/root 按钮/err 透传）。`feature/non-root-support` 合 main。

**关键结论**（两机 shell 逐项实测，矩阵全表在 WORKSPACE G 节）：
- **shell 直接可用**：CPU/线程（/proc 挂 hidepid=2,gid=3009，shell 在 readproc 组）、dumpsys meminfo 全分类、FPS（SF --list/--latency/全量 dump）、scaling_cur_freq、thermalservice、/proc/net/dev、冷启动、perfetto（traced 代劳特权）、SS2MAX kgsl gpubusy、SS3 dumpsys gpu 显存。
- **shell 不可用**：smaps_rollup（PTRACE 拒）、/proc/<pid>/io（0400 owner-only，无兜底）、simpleperf（gltf viewer 非 debuggable）。
- **QNX 惊喜**：/vendor/bin/busybox 被 SELinux 拒执行，但 QNX telnet 端口网络 shell 可达——agent 内嵌极简 telnet client（纯 TCP + IAC 协商全拒）后 **SS3 GPU 全功能（busy/util/频率/每进程）非 root 可用**。
- 内存降级设计：低间隔 smaps 不可读 → decide_mode 探测一次 → dumpsys meminfo 限频 ≥500ms + err 告知；RSS 用 App Summary「TOTAL PSS:」同行的 TOTAL RSS 兜底（非 root 下 rss 不再为 0）。
- 部署零障碍：/data/local/tmp shell 可写可执行，daemon 模式本无 root 依赖。

**真机回归基线**（全部 --remote hppc）：
- 非 root SS3 @500ms 九项齐开：hello root=false + 降级提示；CPU 41%/FPS 60/内存明细/RSS 765MB/**QNX busy 20.3% util 16.2% 506/635MHz + gpuproc 归因**；IO err 一次后静默；退出 teardown 停链（--qnx-stop 复核「未在跑，不动链」守卫正确）。
- 非 root SS2MAX @50ms：内存 dumpsys 降级（PSS 629MB/RSS 741MB，~500ms 周期，dumpsys 开销致每 10 轮 2 轮 overrun——已知的降级代价）；kgsl 74.7%@585MHz shell 直读；IO err 禁用。
- root 回归 SS3 @50ms：无 XPERF_NO_AUTO_ROOT → 车机 auto-root 生效（hello root=true），smaps 50ms 快路 + IO + QNX 全恢复。
- GUI 冒烟（XPERF_NO_AUTO_ROOT + SS2MAX）：AgentHello{root:false} 到前端，徽章/灰显链路通（diag 佐证）。
- 静态：全量测试 96 绿（core 81+7ignored/gui 8/cli 5/xrm 2）、clippy 0、cargo doc 0。

**残留**：GUI「获取 root」按钮的 DOM 点击绑定为唯一未自动化部分（一行 addEventListener）；其后端 `acquire_root` 已真机闭合验证（bf50bbc：SS2MAX reboot 掉 root 后 `test_acquire_root_hppc` 3.46s 完成 shell→root 全迁移）；auto-root 平台守卫已抽纯函数 `should_auto_root` 单测锁定（泛型 Android 分支无非车机真机，策略由单测+detect 测试覆盖）；agent 单测无法在主机跑（compile_error 限 Android 目标，历史如此）。

---

## 2026-09-09(2) — Perfetto UI 镜像蓝屏修复 + GUI 性能优化 + 流式落盘统一

**任务**：修远程 Perfetto UI 蓝屏；审查并优化 GUI 热路径性能；series 内存无限增长治理。

**commit**：2c9c282（镜像修复）→ 6856010（性能批次 A）→ ca2bd11（批次 B）→ 本次（流式落盘统一）。

**关键结论**：
- **蓝屏根因**：镜像 curl 下载 frontend_bundle.js 截断 4%（exit 18 被当可跳过失败留半截文件，完整性检查只查存在性 → 缓存永久中毒）。修复=原子下载（.part+rename）+ `--retry 2 --retry-all-errors` + `_mirror_complete` 完成标记校验（缺标记自动重镜像自愈）。真机重镜像 83s 通过（bundle 与上游 Content-Length 一致）。
- **性能批次 A**：图表事件只标脏 150ms 合帧且仅绘激活页（原每事件全量重绘，极端 50ms×3PID 可达数百 draw/s/图）；updateTitle 从每事件 O(全设备点数) 改 5s 定时；uiColors 按主题缓存（原每 draw 两次 getComputedStyle）；面板 500ms 渲染加 dirty 检查；后端逐事件 eprintln 改 XPERF_DEBUG 门控。
- **性能批次 B**：折线 draw 窗口起点二分只算一次（vMax 扫描复用）；`AgentStream::next_event_batch` 抽干读缓冲合并一轮 burst 为一次 sample emit（{serial, events[]}）——IPC ~260/s → ~20/s。
- **流式落盘统一**：CsvStream 从 CLI 迁至 `xperf-core::csvstream`（CLI re-export 零改动，写文件改 append 模式+空文件才写表头），GUI 采样线程逐事件写 `<pkg>/<ts>-<serial>/`；目录策略与前端会话语义对齐（`start_sampling` 带 fresh 参数：手动开始=新目录，指标勾选重启=同包复用续写不丢前文，换包=新目录）；导出 CSV = 复制会话目录快照（不再前端回传全量）；前端 series/hist 加 pushCapped 抽稀（2×30000 每 2 取 1，同 CLI CHART_SERIES_CAP 口径）——GUI 内存有界，全分辨率数据在落盘 CSV。
- **验证**：全量 92 passed + 6 ignored 全绿，cargo doc 零告警；GUI --remote hppc 真机冒烟（SS3，cpu+memory@500ms）：CSV 逐行流式落盘，最后一行与事件流紧贴至停止。

**遗留**：macOS 数据根目录实为 `$TMPDIR/xperf`（temp_dir 语义，非字面 /tmp/xperf——行为一直如此，文档措辞 /tmp 指系统临时目录）；GUI 无线程 CSV（无 --thread 开关，面板内存态）。

---

## 2026-09-09 — SSH 远程后端全量落地（feature/ssh-remote → main）

**任务**：按 `docs/DESIGN-ssh-remote.md` v2 实现 SSH 远程后端（真机接 hppc，本机跑 CLI/GUI）。

### 完成内容（commit 链，均在 feature/ssh-remote 分支验收后合 main）

| 步 | commit | 内容 |
|---|---|---|
| 设计 | f45d44b | 设计文档 v2 + WORKSPACE F 节入库 |
| S1 | c156141 | `transport.rs`：Transport/SshTarget + 全局存取 |
| S2 | e520787 | SshTunnel establish/is_alive/Drop + 端口自选重试 + R9 无主 socket 清理 |
| S2a | 759d72d | hop#2 add_forward/remove_forward + 映射表复用 |
| S3 | cd81a37+b578b9f | utils.rs 唯一构造点 `adb_command()` 注入 `ADB_SERVER_SOCKET`；run_adb |
| S4 | ea61e05 | init_remote（R2 协议版本 banner 校验）/shutdown_remote（R10 逐条清规则） |
| S5+S6 | e4ae289 | CLI 三参数 + ensure_daemon 端口分流（Ssh → hop#2 映射） |
| S9 | b5c179a | reconnect_agent 隧道判别 + rebuild_tunnel（指数退避 1s→30s） |
| S10 | f2a58a9 | GUI 顶栏连接控件 + 5 命令 + remote-status 事件 + 监视器隧道死短路 |
| 追加 | （见 git log） | GUI 下拉读 `~/.ssh/config` Host 别名免录入 + 远端 adb 路径自动解析（默认 `adb` → 退 `~/Android/Sdk/platform-tools/adb`） |

### 关键结论与基线（真机 Mac←SSH→hppc→SS2MAX d1f39648c1f，gltf viewer）

- 远程采样 `--cpu --interval 500`：hello 8 核/maxkhz ✓，CPU ~42% 与本地口径一致，CSV 流式 500ms 节拍，退出图表正常
- `--trace 10`：44MB pftrace 落**本机**（pull 客户端侧语义），trace_processor SQL 报告正常；`--stack 10`：5.1MB .data + 三视图报告
- 断连重连：`-O exit` 杀隧道 → 「连接断开→SSH 隧道已重建→已重连，恢复采样」，~12s 断档后连续
- 并发：双 CLI 进程（各自隧道）采样+trace 并行，节拍无退化（佐证设计 #22-24 队头阻塞不成立）
- 退出零残留：优雅退出后 hppc `forward --list` 空、`~/.ssh/cm` 空；SIGTERM/SIGKILL 残留的 control socket 由下次 establish（R9 pid 判活）实测回收，远端 forward 规则由 ensure_forward 查 list 复用消化（不累积）
- hppc 集成测试 3 条（#[ignore] 手动跑）：隧道生命周期 / hop#2 生命周期 / init+shutdown 全绿

### 遗留问题

- **S8a 双真机远程并行未验证**：hppc 仅挂一台设备；已用「双进程并发会话（采样+trace）」替代验证，机制上与本地多设备同层（-s 路由在隧道之下）。待 hppc 挂两台设备后补验
- R11「首次 adb root 真重启 adbd」场景：SS2MAX adbd 已持久 root 无法制造；ensure_daemon 两轮重试结构理论上覆盖（root 后规则死→下轮 ensure_forward 重建），待自然出现时确认
- GUI 连接切换的点击交互为人工目验项（命令级测试覆盖配置 upsert/ssh config 解析；真机冒烟已验 --remote 自动启动）

### 工程备忘

- 本机测试工具 shell 对含全角字符命令输出偶发整段丢行/重复刷屏——重定向文件后 grep 判读，勿信空输出
- macOS 无 `timeout` 命令：用 `perl -e 'alarm N; exec @ARGV' ...` 限时

---

## 2026-09-08（一）午后 — perfetto 解析/浏览器打开 与 simpleperf 火焰图修复（Apple Silicon）

**现象**：GUI 上 perfetto 分析失败、perfetto UI 浏览器打开失败、simpleperf 火焰图浏览器打开失败——三连挂的根因各不相同，均为 **macOS（Apple Silicon）环境差异**。

### 1) perfetto 解析失败（真 bug，新 trace_processor 行为变更）

- 报错：`Result rows were returned for multiple queries. Ensure that only the final
  statement is a SELECT statement...`——新版 trace_processor（prebuilt
  `trace_processor_shell-64180e40f94fa8bd`）**禁止一次执行多条返回行的 SELECT**，
  而报告设计是单文件多段 SELECT（旧版本允许）
- **修复**：`analyze()` 改为**逐条执行**（`sql_statements()` 按 `;\n` 切分，marker 语句
  结果集天然存在，输出拼接后与旧版单次执行格式一致，`parse_sections` 无感）；
  `-q file trace` 旧语法新旧版本通吃（Linux 机旧二进制无需变动）；失败即停语义保留
  （其后段缺失 + notes 归因），首段失败保持整体报错
- **E2E 验证**：CLI `--trace 5` 全链路 ✓（报告落盘、频率/帧时间线段齐全）

### 2) perfetto UI 浏览器打开

- 镜像缓存实测健康（headless Chrome 走真实深链：`Opening trace using built-in WASM
  engine` → `Loading trace 22.07 MB (81.8 MB/s)`，console 判据）
- 加固：`ensure_perfetto_ui_mirror` 完整性检查改为**校验 index.html 声明的 stable
  版本目录**存在（含 frontend_bundle.js）——混合/半截缓存（多次镜像运行交叠）旧检查
  （任意 v 目录存在）认不出
- 用户当时的失败疑与 reboot 后 /tmp 清空/旧 GUI 进程相关，待 GUI 重启后复测

### 3) simpleperf 火焰图失败（真 bug，Apple Silicon 平台缺口）

- `host_report_lib()` 只认 `macos/x86_64`——本机 aarch64 直接命中兜底 bail
  （缓存目录不存在 = 下载从未发生，佐证）
- **实测发现上游 dylib 已是 universal 二进制（x86_64 + arm64）**，Apple Silicon 原生
  可加载——**修复 = `host_report_lib` 接受 `("macos","aarch64")` 走同一路径**，
  无需 Rosetta（中途实现的 `report_py_args` Rosetta 方案已删，最小正确实现）
- **E2E 验证**：gitiles 逐文件下载（6 文件，dylib 25MB universal）+ 原生 python3 渲染
  2.6MB data → 3.57MB HTML（1.4s）✓

### 工程备忘

- 工具 shell 捕获对含全角字符的输出偶发整段丢行（`od -c` 读文件绕过）；`/tmp` 在
  沙箱内行为不稳定，临时文件用 workspace 内路径
- CLAUDE.md 的 xdg-open 描述已过时（实为 open/xdg-open 分支），simpleperf 文档已同步

### GUI 布局重排（分析页控件 + 侧栏瘦身）

- 分析页（Perfetto/Simpleperf）toolbar 各加「录制时长下拉 + 录制并分析」——就地录制
  不切页，与侧栏入口共用 `start_trace`/`start_stack` 命令；禁用状态经
  `setTraceButtons`/`setStackButtons` 同步（命令行自动启动路径经 recording 事件覆盖）
- **侧栏瘦身**：仅保留公共模块、**三个 tab 常驻不再隐藏**（switchTab 去掉 sidebar 切换）——
  「应用管理」=包名输入/刷新包列表/打开应用/重启应用/Activity 输入（打开/重启与 Activity
  从 Package label 内拆出独立成组）；「数据管理」=导出 CSV/保存基线/对比基线/清理缓存
- 性能指标页新增 `perf-controls` 控制区：开始/停止、采样间隔、窗口、指标勾选横排
  （checkboxes 由两列 grid 改单行流式）、实际周期；侧栏「深挖录制」段删除（时长下拉归各分析页）
- 复用现有命令与事件流，无 Rust 逻辑改动；前端资产编译期嵌入（frontendDist），改前端须重建

### 火焰图脚本 vendor 进仓库 + 更新功能

- 脚本集从 `~/.cache/xperf/simpleperf_scripts/` **迁移到 `xperf-core/simpleperf_scripts/`**
  （git 管理，7 文件 ~31MB：5 个纯 Python + **双平台 report 库**——darwin dylib 25MB universal
  + linux .so 6MB ELF），文件齐全零网络；路径经 `env!("CARGO_MANIFEST_DIR")` 编译期锚定，
  与运行 cwd 无关
- **更新功能**：core `update_simpleperf_scripts()`（强制重拉、**双平台库都更新**——任一台
  机器执行即双端同步）→ CLI `--update-simpleperf-scripts`（维护 flag，无需 --package）+
  GUI 数据管理「更新火焰图脚本」按钮（`update_simpleperf_scripts` 命令）；覆盖后 git
  提交即分发
- ensure 行为：文件齐全直接用；缺项才逐文件补齐（下载函数 `download_scripts` 抽出共用）
- `--clean-cache` 语义变化：脚本集 vendor 在仓库**不受清理影响**（消息与 GUI confirm 文案已同步）
- Linux 侧适配（用户指正）：`bin/linux/x86_64/libsimpleperf_report.so`（ELF x86-64，6MB）
  一并 vendor；.gitignore 加 `__pycache__/`（火焰图脚本运行的 python 产物）
- E2E：CLI `--update-simpleperf-scripts` 7 文件重拉 ✓（git status 可见 vendor 目录待提交）
- **更新进度**：`update_simpleperf_scripts(progress: Option<&dyn Fn(&ScriptsDownloadProgress)>)`
  ——`ScriptsDownloadProgress`{index, files, rel, bytes, overall_bytes, overall_expected}；
  CLI 回调 stderr `\r` 单行原地刷新、GUI emit `scripts-update` 事件（progress/done +
  percent 字段）→ 前端 `setStatusProgress` 绿色进度条（done 退普通样式）
- **下载改 Rust 流式**：不再经 shell 管道（`curl | python3 -m base64 -d > dest` 的重定向
  会截断目标文件，失败留半截；且 python 退出码会掩盖 curl 失败）——spawn curl 读 stdout
  字节流 + 内置 `Base64StreamDecoder` 边下边解（~1MB 粒度回调），python3 不再是下载依赖
  （仍是 report_html.py 运行依赖）；`.dl-tmp` 原子替换 + update 两阶段（全下完才统一覆盖）
- 进度条基准 = 既有 vendor 文件大小之和（首装缺失 → 只显示字节计数无百分比，诚实不造假）
- E2E：进度 44 条流式（1MB 粒度）、`.dl-tmp` 零残留、重下 dylib md5 一致 ✓
- **LFS 管理大二进制**：.gitattributes 加 `bin/**/*.dylib|so`，`git add --renormalize`
  迁移（9686402）；ensure/download 对 report 库按 >1MB 判存在——LFS 未拉取时本地是
  ~130B 指针文本，自动回退 AOSP 下载（避免把指针文件当库加载）
- **历史重写**：`git lfs migrate import --everything` 把旧提交里的裸 blob（~31MB）也
  转为指针——注意 migrate 会因工作区脏（含未跟踪文件）弹 y/N 询问导致挂起（后台跑
  也会卡），须先清干净再跑；重写后 `git push --force-with-lease` 强推（LFS 对象
  同 oid 免重传）；**本地保险分支 `backup-pre-lfs-migrate` 保留旧历史**（未推送）；
  **Linux 机同步须 `git fetch && git reset --hard origin/main`（或重 clone），不能直接 pull**

### git 拓扑：hppc 作为中转远端（2026-09-08 晚）

- **远端结构**：本机（Mac）→ `hppc:/home/han/code/tools/xtools`（SSH 别名 hppc，
  Linux 开发机）→ GitHub（origin）。**向 GitHub 提交统一在 hppc 上执行**；
  本机 `git push`（upstream 已设 hppc/main）推到 hppc 即可
- hppc 侧 `receive.denyCurrentBranch=updateInstead`：推送到 checked-out 分支时
  自动更新其工作区（要求 hppc 工作区无未提交变更）；git-lfs 3.4.1 已装，
  LFS 对象经 SSH 直传 hppc（实测 `git lfs push hppc main` 握手可用；hppc 无锁 API，
  已按提示设 `lfs.<url>.locksverify=false` 消噪；真正的 LFS 内容传输待 dylib 变更时
  自然验证，失败兜底：ensure 按 >1MB 判缺自动从 AOSP 重下）
- LFS 内容已到位（dylib 25.4MB 实测在 hppc 工作区）；`git pull` 从 hppc 拉
  （GitHub 侧变更经 hppc：hppc 上 `git pull origin` 后本机再 pull hppc）

---

## 2026-09-08（一）上午 — GUI「打开应用」后 NoProcess 排查（RemoteServer 崩溃，非工具问题）

**现象**：GUI 采样 `com.google.android.filament.gltf`（SS2 MAX）持续 `NoProcess: 包名下无进程`；用户确认点击过「打开应用」。

### 结论（证据闭环）

- **xperf 工具链无异常**。「打开应用」正常执行（force-stop→800ms→`am start -W`），Activity 正常拉起，崩溃发生在应用自身 `onCreate`
- **崩溃机制**：样例的 `RemoteServer(8082)`（CivetWeb）`mg_start` 失败 → fork 源码链 `RemoteServer.cpp ctor(48-64)` graceful 返回 → JNI `isValid()==false→return 0`（`RemoteServer.cpp(JNI):25-28`）→ Java `RemoteServer.java:43` 抛 IllegalStateException → FATAL（crash buffer 09-08 09:37:13 pid1744 + 09-04 18:46 pid1742 两条同栈）。进程死 → agent 每轮重扫 → NoProcess 刷屏
- **排除端口占用**：3 次 `adb reboot` × 17 次早开机探测（最早 +25s），`/proc/net/tcp{,6}` 全程 8082(0x1F92) 仅应用自身监听；watcher 脚本（500ms 轮询 LISTEN + /proc/*/fd 定位持有者）从未见第三方进程
- **未定位**：mg_start 失败的确切 errno。CivetWeb Android 构建的 `mg_cry_internal_impl` 走 `__android_log_vprint(tag=civetweb)`（`third_party/civetweb/src/civetweb.c:3695`）——错误本会进 logcat，但 09:37 主 buffer 已被 veh-faas 日志刷滚；`error_log_file=civetweb.txt` 写 cwd=/（只读）丢失
- **复现条件**：两次失败均为**当天第一次冷开机**后 ~1 分钟内；`adb reboot`（温启动）3 次均无法复现

### 遗留与工具

- 已部署 `/data/local/tmp/catch8082.sh`：下次冷开机后 `adb root && adb shell sh /data/local/tmp/catch8082.sh` 自动拉起 6 轮 + 全量 logcat 落盘（catch.log）+ 8082 端口表（catch8082.log），失败即得 tag=civetweb 确切错误
- 彻底方案：样例 `error_log_file` 改绝对路径（如 `/data/local/tmp/civetweb.txt`）重编 APK
- 可选 feature（另立项）：GUI 检测「打开应用后进程立即消失」时提示应用启动崩溃（查 logcat -b crash），避免 NoProcess 误导

---

## 2026-09-07（日）夜 — Mac 主机构建修复（xperf-agent 收紧为 Android-only）

**任务**：macOS 上 `cargo build --release` 失败——xperf-agent 用了 Linux/Android 专属 API
`SocketAddr::from_abstract_name`（`std::os::linux|android::net::SocketAddrExt`），macOS 无此 API。

### 方案（按「agent 只有 Android 端二进制」语义收紧）

- **workspace 层**（`Cargo.toml`）：新增 `default-members`（core/CLI/GUI/xrm），主机上的
  build/test/check/doc 不再构建 agent；`-p xperf-agent` 交叉编译不受影响（member 仍在）
- **crate 层**（`xperf-agent/src/main.rs`）：`#[cfg(not(target_os = "android"))] compile_error!`
  拦截主机目标的显式构建，报错信息直接给出交叉编译命令；顺带清理 run_daemon 里的
  linux cfg 分支（crate 已 Android-only，linux 分支成死代码，`SocketAddrExt` import 无条件化）
- **链接器跨平台**（`.cargo/config.toml` + 新增 `.cargo/ndk-clang.sh`）：原 config 硬编码
  Linux 机器路径（`/home/han/.../linux-x86_64/...`），本 Mac 交叉编译同样失败。改为相对路径
  指向包装脚本，按 `uname` 选 `darwin-x86_64`/`linux-x86_64` prebuilt，探测顺序
  `$ANDROID_NDK_HOME` → `~/Library/Android/sdk/ndk/25.1.8937393` → `~/Android/Sdk/ndk/...`
  （绑定 NDK 25.1.8937393 / API 26 不变）

### 验证

- `cargo build --release`（Mac 主机）✓；`cargo test` 77 passed 0 failed ✓
- `cargo check -p xperf-agent`（主机目标）→ compile_error 明确拦截 ✓
- `cargo build -p xperf-agent --target aarch64-linux-android --release` ✓
  （产物 `target/aarch64-linux-android/release/xperf-agent`，907KB）
- `cargo doc` + 三个 rustdoc missing_docs 检查全 0 warning ✓

### 关键结论

- cargo config 无宿主机条件语法（`cfg()` 只作用于编译目标），跨机器链接器解析只能经
  包装脚本；rustc 对含 `/` 的相对 `linker` 路径按工作目录（workspace 根）解析，实测可用
- `--workspace` 全量命令在主机上会触发 agent 的 compile_error（设计如此），
  主机工作流一律用默认成员集命令（CLAUDE.md Commands 节已同步）

### review 追加（NDK 版本策略统一）

- 版本策略改为「**>= 25.1.8937393 均可，多版本取最相近**（满足下限的最小版本），
  显式 ANDROID_NDK_HOME/ANDROID_NDK_ROOT/NDK_HOME 优先且不过滤」——不再 pin 具体版本
- **删除 core 的 `find_ndk_linker()`（~60 行）+ `CARGO_TARGET_..._LINKER` env 覆盖**，
  `.cargo/ndk-clang.sh` 成为唯一探测机制（补齐显式 env 分支与 SDK 根扫描）；
  附带删除其单测（65→64）
- 脚本验证：版本 key（major*1e12+minor*1e9+build）整型比较；假 wrapper 树实测
  多版本取相近/无满足版本报错列出已发现版本/显式 env 直用/真机三版本取 25.1 全通过
  （注：工具 shell 捕获对含全角字符的输出偶发丢行，用 od 读文件绕过）

---

## 2026-09-07（日）夜 — GitHub LFS 慢速下载修复（media 路线自定义传输通道）

**任务**：本仓库 `example/apk/*.apk`（LFS）拉取极慢，定位并修复网络瓶颈。

### 定位（实测数据）

LFS 流程两段速度悬殊：batch API 协商（`github.com`）正常（TLS 0.17s）；**对象下载走
`github-cloud.githubusercontent.com`（AWS S3 alambic 中转）仅 21KB/s**（29MB ≈ 24 分钟）。
而 GitHub 自家媒体边缘 `media.githubusercontent.com/media/<owner>/<repo>/<ref>/<path>`
直连 3.5MB/s（快 165 倍）。本机无代理（scutil/pgrep 确认），公共 gh 加速器对该 S3 签名
URL 全部不可用（ghfast 1.1KB/s / ghproxy 9.7KB/s / moeyy 超时）。

### 方案：git-lfs standalone 自定义传输通道

- **代理脚本**：`~/.local/bin/git-lfs-media-agent`（python3，git-lfs custom transfer
  协议：init 回 `{}`；download 时 oid→path 由 `git lfs ls-files` 解析（**注意输出是 10 位
  短 oid，须前缀匹配**）、`<owner>/<repo>` 从 remote URL 解析（兼容 SSH/HTTPS/LFS endpoint
  `/info/lfs` 后缀，字符集严格校验）、ref 取 HEAD——构造 media URL 下载，失败回退 batch
  href（仅 https）；全程 subprocess 列表参数无 shell 注入面）
- **接线（仓库 `.git/config` 本地生效，勿提交——脚本路径机器相关，提交会弄坏他人 clone）**：
  ```bash
  git config lfs.customtransfer.media-rewrite.path ~/.local/bin/git-lfs-media-agent
  git config lfs.customtransfer.media-rewrite.direction download
  git config lfs.standalonetransferagent media-rewrite
  ```
- **验证**：`git lfs env` 出现 `media-rewrite`；`git lfs pull` 13.7s 完成（原约 24 分钟），
  SHA256 与 pointer oid 一致，`git lfs ls-files` 状态 `*`

### 关键结论

- 「git 主站快但 LFS 慢」= 两段流量走不同域名，S3 中转域线路劣化是根因；换 media 域即修复
- **新 clone 首次 smudge 仍走慢速 basic**（clone 时仓库本地 config 尚未存在）：先
  `GIT_LFS_SKIP_SMUDGE=1 git clone`，接线后 `git lfs pull`。全局启用（对所有 GitHub 仓库
  生效，含 clone 时）把上面三条 `git config` 改 `git config --global` 即可
- git-lfs 只认 batch API 的 transfer 协商，但 `lfs.standalonetransferagent` 可无条件绕过
  （GitHub 不感知）；agent 回退 href 的设计保证 media 域 404 时行为退化为 basic，不丢功能

---

## 2026-09-07（日）晚 — agent daemon 化（socket 服务 + 多 host + 版本握手）

**任务线**：用户拍板重设计——「不要一条命令杀死一次 agent」：agent 常驻 daemon、心跳/无连接超时自杀、host 每会话连接（版本不符自杀/强杀重推，一致直连）、多 host 上限 10。

### Commits

| commit | 内容 |
|--------|------|
| （本轮） | feat(agent,core)：daemon 化。agent `--daemon` 监听 `localabstract:xperf-agent`（连接即 hello 含 version；命令 `start <argv 同款参数>`（复用 parse_args 零新依赖）/`stop`/`ping`/`suicide`；会话上限 10；0 会话 60s 自杀；会话=独立节拍线程 + TLS emitter（子模块 emit 零改动）；QNX/topgpu/ligfx 流式 GPU 通道全局独占（GPU_STREAM_BUSY，被占用发 err）+ teardown 槽（持有会话结束或进程退出执行一次，先停链后放读线程杀 telnet——顺序确定）。core：AgentStream 改 forward+TCP（reader 阻塞读 + ping 线程 5s）；`ensure_daemon` 全权管生命周期（forward 复用/probe 版本/suicide+pkill/强制重推/启动/探活）；`AgentEvent::Hello` 加 version；删 cleanup_orphan_agents（daemon 化根治孤儿泄漏，e692d4e 过渡方案移除） |

### 完成内容与真机验证

- **SS2MAX**：daemon 常驻跨会话复用（pid 不变）+ 第二次会话秒连；kill -9 宿主 → 会话即收零泄漏；**双 CLI 并行**（cpu 18 事件 + memory 9 事件同 daemon）；**版本不符**（v99 daemon → CLI v2：suicide+pkill+强制重推+重启，md5 与日志双重确证）；**max 10**（11 连接实测：前 10 个 hello、第 11 个收「会话数已满（10）」err）；空载 60s 自杀（daemon 日志「空载 60s，daemon 退出」）
- **SS3 QNX**：会话结束 frame 链即停（teardown 确定性）+ 次会话 0 自愈即起流（17 gpu 事件）；proc 链泄漏依旧（E 节已知未修项）
- **验证门槛**：105 测试全绿 + clippy 零警告 + cargo doc 零 warning

### 关键结论（踩坑）

- **同秒同尺寸重建会骗过 deploy_agent 的 size+mtime 快检**（v99/v2 测试时两构建同秒落地同尺寸，快检跳推 → 版本协商死循环）——daemon 重推路径改为**强制 push**（800KB ~6ms，不省这个）
- **`pkill -f` 与启动命令同 shell 会自杀**：组合命令文本里含 "xperf-agent --daemon" 字面量，`pkill -f 'xperf-age[n]t'` 的正则能匹配（`[n]` 只防模式自身，防不了同行其它字面量）——pkill 与 start 必须分开两个 adb 调用
- **会话结束 teardown 竞态**：stop 置位后 GPU 读线程杀 telnet vs 停链写 telnet 同资源竞争——拆两阶段 stop（会话 loop stop 先，GPU 读线程 gpu_stop 在 teardown 后）确定顺序
- **抽象 socket std 支持**：`SocketAddrExt::from_abstract_name` 在 `os::android` 与 `os::linux` 下各有一份（std 同 API，cfg 分开引）
- 连接上限计数按**连接**而非已开始会话（probe/idle 连接也占坑，瞬时可接受）

---

## 2026-09-07（日）下午 — 孤儿 exec-out 三层防御 + agent 传输改版（exec-out → shell）

**任务线**：用户拍板「两者都要」——agent 侧 liveness 看门狗 + host 侧孤儿清理，根治 E 节「孤儿 adb exec-out 泄漏」（GUI/CLI 非正常死亡 → adb 流孤儿化挂 init → 设备端 agent 永不退出，曾实测 7 组残留跑 4 天）。

### Commits

| commit | 内容 |
|--------|------|
| e692d4e | fix：三层防御。①agent **stdin EOF 监测**（主路径）——host 死亡 → adb stdin EOF → adbd 关设备端 stdin → read 0/Err 带钩子退出（秒级）；②agent **停滞看门狗**——`max(3×interval, 30s)` 无成功写出（`LAST_EMIT_OK`）带钩子退出（兜底）；③host **`cleanup_orphan_agents`**（spawn_agent 前调用）——`ps` 快照筛 agent 流（shell/exec-out 都认），父进程非存活 xperf 即孤儿 `kill -9`（不问 serial），杀过等 2s 让 EPIPE+钩子落地；设备侧兜底：目标设备无存活 host 流时 `pkill -f 'xperf-age[n]t'`（有存活流跳过，防误杀并行会话）。**传输 exec-out → `adb shell`**（exec-out stdin 不传数据不传 EOF，实测 `cat` 挂死；shell 字节完整——NDJSON 全量 JSON 校验通过）；`AgentStream` 增 `_stdin` 句柄（从不写数据，纯 liveness 信号）；`exit_with_hooks` 统一 EPIPE/EOF/停滞三路径。单测 core 65→66（孤儿流解析+判定），workspace 105 全绿 |

### 完成内容与真机验证（SS2MAX d1f39648c1f）

- **exec-out 不传 stdin  EOF（根因确证）**：`adb exec-out cat < /dev/null` 挂死（rc=124）vs `adb shell cat` 立即回显退出；fifo 控制实验：`adb shell setsid xperf-agent < fifo` 关写端 → 5s 内 agent 退出，host adb 一并消失——shell 传输的 stdin EOF 路径成立
- **三层各自验证**：①shell 会话关 stdin → agent 秒退（上）；②CLI 新会话（无孤儿场景）正常采样 + `timeout` 结束后零残留（cleanup 零误伤：无孤儿时只做 pkill 兜底且设备端本就干净）；③**孤儿场景**——setsid bash 起 exec-out 流 → `kill -9` 载体 bash（adb 流孤儿化挂 systemd，PPID 实拍 3579960）→ 跑 CLI 新会话 → host 孤儿流被查杀、设备端 agent 归零（`NONE`），新会话自身采样正常（CPU/内存事件流完整）→ 正常退出后零残留
- **停滞看门狗**：kill -STOP adb 进程模拟「写阻塞」（stop 期间 stdout 管道满后 agent emit 卡住）——最终由 host 侧 kill -CONT+kill 收场；此路径为兜底防御，正常超时 30s（interval 500ms × 3 取下限 30s）
- **验证门槛**：105 全绿 + clippy 零警告 + cargo doc/agent rustdoc missing_docs 零 warning

### 关键结论

- **孤儿化在 setsid bash 载体下 PPID 是 systemd --user（3579960）**，不是 pid 1——孤儿判定不能认死 pid 1，须「父进程不存在或非 xperf 工具」（is_orphan_stream 语义）
- `kill -STOP` 一个 adb 进程不够——adb 可能多进程协作（server/transport），STOP 单个后输出仍走（另测 STOP all 后 log 仍在涨：worker 线程在写）；停滞看门狗的真实验证需 STOP 全部 adb 进程并等管道满，本轮以 kill -CONT 收场不再深挖（该路径为兜底，主路径 stdin EOF 已实测）
- **agent 协议零变化**（stdin 不下发数据、空行心跳照旧）；GUI/CLI 调用点零改动（core 内部换传输）
- 手动直跑 agent 的新约束：stdin 须保持打开（`< /dev/null` 立即 EOF 退出）；fifo 控制实验是最可靠的 liveness 验证手段（真机可复现）

---

## 2026-09-07（日）— E 类修复：SS2MAX gpubusy 窗口语义 + 两项新发现

**任务线**：WORKSPACE E 节 SS2MAX 相关遗留修复（gpubusy 恒 0 0 / busy%>100% 不可信 + GPU 显存无数据源复核）。

### Commits

| commit | 内容 |
|--------|------|
| b72ef36 | fix(agent)：SS2MAX kgsl gpubusy 窗口语义自适应。根因：该厂商内核 gpubusy **非累计计数器**，读数为上一 ~1s 窗口的 busy/total µs（total 恒 ≈1e6——开机 4 天 16h 读数仍 ≈1e6 确证；与内核 `gpu_busy_percentage` 同刻值 74.8% vs 75% 互证）。累计差值解析在窗口边界产生 busy 暴增/total 回退 → 1662% 荒谬值。`GpuBusyCalc`（kgsl.rs）三判据命中即永久锁定窗口语义：①total < 60e6 µs 幅值（累计=开机至今 µs，开机 >60s 必超）②计数器回退（total 降或 busy 降）③差值占比 >100%；窗口语义直读 busy/total、读数全等=窗口未刷新不出数、空闲 "0 0" 不出数；累计语义零变化。单测 24→27（真机序列/累计序列/异常锁定三组） |

### 完成内容

- **SS2MAX gpubusy 修复（E 节转正）**：真机验证修复后 busy 68-80% 有界，同窗对照 `gpu_busy_percentage` 均值 75.5 vs 74.7 一致；空闲态（force-stop 被测应用）~22.05% 与直读窗口值 22.09% 一致（系统合成器基线）；mhz 随负载 427/500/585 正常跳变。原 09-03 "恒 0 0" 真相：GPU 空闲时窗口语义读数即 "0 0"，非计数器停走
- **SS2MAX GPU 显存无数据源复核（维持平台限制结论）**：root 下全路径——dumpsys gpu 无 Memory snapshot 段；/sys/kernel/debug 整个不存在（内核未编译 debugfs，mount 失败）；/proc/kgsl 不存在；kgsl sysfs 仅 usesgmem（开关非计量）
- **验证门槛**：workspace 104 全绿（agent 27 + core 65+2ignored + xperformance 5 + GUI 5 + xrm 2），clippy 零警告，cargo doc + rustdoc missing_docs 零 warning

### 关键结论与新发现（已记 WORKSPACE E 节）

- **QNX `gpu_per_process_busy` 进程链无停止手段（预存缺陷，本轮发现）**：三层清理只写 `gpubusystats` 停 frame 链；proc 链实测死写入者 toggle（500）/写 0/`gpu_set_log_level 0` **均无效**——每 --gpu 会话泄漏一条，多日累积 ~20 条锁步洪泛（同一 proc 行每窗重复 20+ 份），疑似挤占资源致 frame 链无法启动（我两轮直跑会话 0 frame 事件 + 3 次看门狗自愈无效）。**恢复手段 = `adb reboot`：实测整 SoC 复位含 QNX host**（重启后回开机基线：5000ms frame 链 + 低频 proc 行）。已 reboot 恢复 SS3 并验证一次完整会话（EPIPE 退出路径：38 gpu + 43 gpuproc 事件、1 次自愈、frame 链退出钩子正常停止）
- **孤儿 adb exec-out 泄漏**：host 侧 7 个 9月04 起残留的 adb exec-out 孤儿进程（GUI/CLI 非正常死亡不带走）让设备端 7 个 agent 持续采样 4 天。杀 host adb → 设备端 agent EPIPE 自退（设计路径有效）。教训：**手动直跑 agent 验证勿用 `timeout`（SIGTERM 不触发退出钩子泄链），须杀 host 侧 adb 走 EPIPE**
- **终端复制伪影本轮依旧极重**（reboot 命令输出重复 100+ 行）——判据全走文件中转 + 逐条单跑确认

### 遗留问题

- QNX proc 链停链命令待挖掘（找到后补 agent 退出钩子 + host qnx_stop_stats）；在此之前 SS3 上 --gpu 会话每跑一次泄一条 proc 链，密度累积影响 gpuproc 去重外的 wire 冗余，必要时 adb reboot 清
- 孤儿 adb exec-out：缓解思路（agent 最大空闲看门狗 / host 启动 pgrep 清理）待决策

---

## 2026-09-04（四）晚 — GUI 多设备改版（设备 tab 并行）+ 应用操作/冷启动

**任务线**：用户需求——「根据 devices 增加页面，实现完整的性能调试功能」（每设备独立 tab：性能分析 / Perfetto / Simpleperf）+「支持打开指定包名应用、重启应用，顺带监控冷启动性能」。

### Commits

| commit | 内容 |
|--------|------|
| aacc499 | review（7 项）：①core 删死代码链 `adb()`/`run_adb_command()`/AdbRunner mock 三件套（coldstart 迁 core 后全仓零调用，doc 声称的 mock 注入点/ADB_TEST_LOCK 使用者均不存在——连矛盾注释一起删，-81 行）②GUI 删 `is_running` 命令（前端从未调用）③`stop_sampling` 幂等化（会话不存在静默成功不建空会话）④`restart_app` activity 留空时 resolve **前置**于 force-stop（解析失败不再杀应用未拉起）⑤前端重定向检测补完整类名形态 `pkg.MainActivity`（原仅短类名 `pkg/.MainActivity`，手填完整类名会误报重定向）⑥`updateTitle` 删废变量/重复 Set 构建⑦CLAUDE.md 同步实际（CLI 无直接 adb 调用、已删函数清单、空白两修复路径合并）。101 全绿 + release 真机冒烟 |
| b30d149 | fix（Alt+Tab 窗口空白）：`WEBKIT_DISABLE_COMPOSITING_MODE=1` 进程内禁用 webkit2gtk 加速合成（与 DMABUF renderer 禁用并排）——窗口失焦（切到浏览器等 GPU 重负载应用）再切回时内容有概率空白，与 DMABUF 启动空白同源不同触发路径；本机四轮窗口切换模拟（最小化×20/覆盖×20/长隐藏 3-30s×5/采样中覆盖×15）均 0 复现，按 DMABUF 同源场景推因修复，采样中真机回归 UI 全正常。顺带发现：SS2MAX kgsl 通道 busy 值可 >100%（实测 GpuUpdate busy=1662%，gpubusy 计数器停走 E 节已知限制的另一表现，补记 WORKSPACE） |
| 9bcb8ee | fix：打开/重启应用按钮报错——前端成功路径调 Rust 方法 `r.summary()`（serde 到 JS 只剩数据字段，`TypeError: r.summary is not a function` 进 catch，`JSON.stringify(TypeError)={}` 掩盖真实错误）；摘要前端自拼 + catch 打 `e.message`。release 构建走按钮同路径 SS2MAX 真机验证通过 |
| 01b28ce | feat：①core serial 参数化——`utils::{adb_for, run_adb_command_for, resolve_serial}`（`Some` 显式路由/空串视同 `None`/`None` 回退全局）；`spawn_agent`/`deploy_agent`/`reconnect_agent`/`qnx_stop_stats`/`trace::record`/`simpleperf::record`/`detect_platform_live` 全部加 `serial: Option<&str>`（CLI 调用点传 None 零行为变化）②GUI 多会话——`AppState.sessions: HashMap<serial, DeviceSession>`（running/trace_running/stack_running/package/startup_extra 设备内隔离）；命令全带 serial（start/stop_sampling、start_trace/stack、list_packages、is_running、launch_app/restart_app——前置 `ensure_device_online`+包名校验，失败回滚 running）；事件 payload 带 serial（sample={serial,event}、trace/stack/sampling-error）；`startup_args`→`startup_sessions`（回查全部活跃会话）；关窗遍历停全部；`select_device` 删除（GUI 脱离全局 serial）③前端设备页——`<template id="devicePageTpl">` 克隆 + `DeviceSession` 类（charts/pidData/peaks/liveData/hists/状态全设备内隔离；datalist id 按 serial 唯一化）+ App 管理器（serial 分发/顶栏 status 显示激活设备/主题遍历）；顶栏设备 tab（断开灰显「已断开」保留数据、插回 reconnect 自动恢复）；CSS 全 class 化（#sidebar→.sidebar 等）④冷启动下沉 `xperf-core/src/coldstart.rs`（CLI coldstart.rs 删除改用 core）——`resolve_activity`（留空自动解析主入口）、`force_stop`、`measure(package, activity, serial)`（activity 空自动 resolve；ColdStartResult derive Serialize）；GUI 侧栏「应用操作」（刷新包列表/打开应用/重启应用三按钮 + Activity 输入）+ 指标页「冷启动」面板（最近 5 次 TotalTime/WaitTime）；**启动被系统重定向（Activity 不属于目标包）状态栏警示**⑤GUI 深挖目录 `<pkg>/<ts>-<serial>/`（防双设备同秒撞目录）；`--package --device` 自动启动回填后**切到对应设备页**（修复：原先停留第一台 idle 页）。测试 101 全绿（+GUI 多会话隔离 1 + coldstart 迁移 3 归 core） |

### 完成内容

- **设计**：core 全局 `TARGET_SERIAL` 保留给 CLI（单会话语义不变）；GUI 用会话级参数——每台设备独立 adb 长连接，双机并行采样/深挖互不干扰（QNX 停链 pgrep 保护在目标设备执行，多设备天然隔离）
- **真机验证（SS3 6eb792dfb0f + SS2MAX d1f39648c1f 双车机同连）**：
  - `--package --device 6eb792dfb0f` 自动启动：SS3 平台检测（QNX 通道）/FPS 60/GPU busy 15%/PSS 650MB 全正常；截图确认双设备 tab（HU_SS3 激活+HU_SS2MAXF）、5 图表 + 面板
  - 用户 release 实例手动操作全链路（diag 日志实证）：手动开始（带 serial）→ 两次勾选切换重启会话（flags 序列化正确）→ Perfetto 10s 录制每秒进度 → recorded → done；产物落 `<pkg>/20260904_184020-6eb792dfb0f/trace/`（三件套齐全）
  - 双机并行：用户实例采 SS3 + 我的实例 `--device d1f39648c1f` 采 SS2MAX 同时进行（SS2MAX 平台检测正确走 serial 过滤路径；filament CPU ~50%/PSS 644MB）
  - 冷启动：CLI（走 core 新模块）SS2MAX `--cold-start .MainActivity`——COLD TotalTime 874ms（手动 am start -W 对照一致）；进程存活时热启动 0ms 如实上报；`resolve-activity --brief` 输出末行格式与解析逻辑实测匹配
- **发现并修复**：①`applyStartupArgs` 不切激活页（回填了 SS2MAX 页但显示 SS3 idle 页）→ 回填后 `switchDevice(serial)`；②车机熄屏状态 `am start -W` 被系统重定向到激活引导页（`Activity: com.lixiang.provision/...`、TotalTime 0）——解析如实上报 + 前端重定向警示（避免误导性 0ms）；③（次日早用户真机反馈「SS2 打开应用失败」）`launchOrRestart` 成功路径调 `r.summary()`——serde 序列化到 JS 的 ColdStartResult 只有数据字段无 Rust 方法 → TypeError 进 catch → `JSON.stringify(TypeError)={}` 掩盖真实错误（invoke 本身已成功，同一实例 start_sampling 正常是关键对照）；修：摘要前端自拼 + catch 打 `e.message`。**教训：JS 侧拿到的 Rust 返回值永远是纯数据对象（方法不可跨 IPC），错误日志须打 `e.message` 而非 stringify（Error 对象 stringify 只剩 {}）**
- **验证门槛**：101 测试全绿（agent 24 + core 65+2 ignored（coldstart 3 迁入）+ xperformance 5 + GUI 5（新增多会话隔离）+ xrm 2），clippy 零警告（修 2 处 `Iterator::last` on DoubleEndedIterator），cargo doc + rustdoc missing_docs 三 crate 零 warning

### 关键结论与基线

- **终端复制伪影在本轮极重**（cargo 输出整块重复 3+ 遍、grep 计数行重复）——判据一律改走文件中转（`> /tmp/x.log` 后 cat），单跑命令逐条确认
- **xdotool 点击 webkit2gtk 按钮多次未命中**（坐标换算物理/逻辑 + 标题栏偏移叠出来误差大；tab 切换/按钮均未触发）——GUI 交互验证优先靠用户实际操作 + diag 日志（本轮用户操作日志完整替代点击验证）；后端链路用 CLI/adb 直测
- **`resolve-activity --brief` 输出两行**（包名 + 完整组件），取末行 split('/').next_back()——SS2MAX 实测 `com.google.android.filament.gltf/.MainActivity`
- **force-stop 后立即 am start 会测到残留路径**：GUI restart_app 固定 800ms 缓冲（进程死透再测真 COLD）
- am start -W 语义：进程已运行时 ThisTime/TotalTime=0（热启动无新 Activity 开销），**不是 bug**——冷启动测量须先 force-stop（GUI「重启应用」语义）或进程确实未起（COLD）

### 遗留问题

- ~~GUI「打开/重启应用」按钮的点击渲染为人工目验项~~（**9bcb8ee 已修 + release 真机验证通过**：SS2MAX 完整按钮路径 invoke→recordColdStart→成功日志；根因 `r.summary()` 前端调用 Rust 方法）
- 设备热插拔的 tab 灰显/恢复为逻辑验证（devices-changed 事件链路 951c0ab 已真机验证；本轮前端新增灰显逻辑未物理插拔复现——与上轮同因：验证窗口内未动线缆）
- 双设备并行 trace/stack 同时录制未双机实测（目录隔离已按 serial 后缀设计；单机录制双机采样的并行组合已验证）

---

## 2026-09-04（四）下午二~四 — 基线对比 C-6 + 多设备 `-s` + 动态检测 + 窗口动态尺寸 + FPS BLAST 修复 + Android 版本检测

**任务线**：C 类收官（基线对比）+ 多设备 adb 全链路修复（E 节候补）+ 用户连续反馈迭代（热插拔动态检测 / 窗口尺寸 / 测试对象换 Filament demo / SS2 FPS 质疑引出 BLAST 图层 bug / Android 版本检测）。

### Commits

| commit | 内容 |
|--------|------|
| 084895b | review：S1（严重）基线判定方向 bug——Row::verdict 相对容差用带符号百分比致 delta<0（FPS 回归/CPU 改善）恒判持平，改 .abs() 对称判显著 +3 测试锁定；G1 全不可比误报无回归→⊘ 无可对比指标；G3 load_from 恢复+版本校验；G4 MetricSummary NaN 过滤；G2 loadDevices 不静默切设备；G3 devices-changed 兜底调 select_device；G4 renderPidList 过滤已退出 PID；G6 resize_default 小屏溢出+conf min 降低；G7 CLI 空基线跳过；M2 CSS 死规则清理；M3 resize_default 改 async。100 测试全绿 |
| 78f93a9 | feat：性能数据单位对齐——内存全链路统一 MB（CLI 打印/CSV/图表纵轴 + GUI 图表/峰值面板/导出 CSV；协议与基线 JSON 存储仍 KB）；GUI 删除打点功能（用户不需要：控件 + marker 事件 + 竖线绘制 + add_marker 命令全链路；CLI socket 打点保留）；PIDs 融入状态栏「监控中: pkg（PID xxx）」不单独占面板 |
| 8d01922 | ui：PIDs 列表迁主区指标页面板行（与实时数值/Top 线程/峰值并列，限高滚动）+「函数热点」tab 改名「Simpleperf 分析」（与侧栏按钮/Perfetto 命名对齐，状态文案同步） |
| 9cfa9a5 | feat：①Android 版本检测——AdbDevice.android_version（ro.build.version.release 逐台 getprop），CLI 打印/多台报错/GUI 下拉均展示；实测 SS3=Android 12（帧走 BLAST 层）、SS2MAX=Android 11（帧走 SurfaceView - 层）。②**SS3 FPS 恒 0 修复**——sf_discover_layers 从「ownerPID 非空跳过兜底」改为 ownerPID ∪ 包名并集：Android 12+ BLAST 合成层（app 直提 buffer）承载真实帧流而 ownerPID 只匹配到静止 Activity View 层；SS3 60fps 恢复、SS2MAX 回归无变化。③**webview 间歇空白修复**——进程内 set WEBKIT_DISABLE_DMABUF_RENDERER=1（webkit2gtk DMABUF 路径在 GPU 重负载占用时间歇空白，同代码两次启动一好一坏实测复现；此前归因 setup set_size 时序系误判，文档已修正） |
| 555ffab | chore：example/apk 增加测试用 Filament glTF viewer demo（git-lfs 管理，example/apk/*.apk）——**后续真机测试统一用它替代 svm**；包名 com.google.android.filament.gltf，已装 SS3+SS2MAX，基线保存→对比全链路冒烟通过 |
| 7cb4b4b | feat：GUI 默认窗口大小按屏幕动态计算——`resize_default` 命令（宽 72%/高 88% + clamp + 屏幕边距，前端 ready 后调用；setup 阶段 set_size 致 webkit2gtk 渲染空白，真机实测）+ 侧栏勾选两列 grid + PIDs 限高，默认大小下全量可见；conf 1400×1000 兜底 + min 1080×720 |
| 951c0ab | feat：GUI 设备动态检测——`diff_devices` 纯函数（core/utils，单测）+ `spawn_device_monitor` 线程（3s 轮询快照 diff，首轮只建快照；adb 暂不可用跳过）+ `devices-changed` 事件 + 前端下拉增量重建/断开占位/接入提示。真机 SS2MAX 物理插拔两轮验证事件对全捕捉 |
| 46bd161 | feat：①基线对比——xperf-core/src/baseline.rs（SessionSummary 汇总 + SummaryBuilder + 保存/读取 + compare 报告，单测 11 个）+ CLI `--save-baseline`/`--compare-baseline`（互斥，报告落盘 `<ts>/baseline_report.txt`，--cold-start 结果进汇总）+ GUI 侧栏「保存基线/对比基线」按钮 + 峰值区基线对比面板（collectSessionData 与导出 CSV 同源）+ GUI 命令级测试 2 个。②多设备 adb——utils TARGET_SERIAL 全局 + adb() 构造器 + run_adb_command 自动注入 `-s`（16 处调用点收敛）+ parse/list/pick_device + CLI `--device/-d` + GUI 设备下拉（list_devices/select_device）+ `--package` 自动启动设备前置解析（多台未指定确定性跳过，消除与前端下拉的竞态；--device 无效不静默换台）+ sampling-error 事件（构建/部署/启动失败前端可见）+ detect_platform_live 按 serial 过滤（**修表头 bug**）+ device_online 只认目标设备。13 文件 +1396/-69 |

### 完成内容

- **基线判定口径（先设计后编码）**：变化须**同时**超过相对 ±10% 与指标绝对地板值才判回归/改善（地板值抑制近零噪声：CPU 2pp/PSS 4MB/FPS 2/Jank 0.5 次每分/GPU 3pp/IO·网络 50KB/s/冷启动 150ms）；基线为 0 退化为纯绝对差；仅一侧采集如实标「⊘ 单侧未采集」；Jank 按两侧各自时长换算为次/分钟（时长 0 不可算→单侧标注）；重启次数用绝对差不用百分比
- **汇总口径**：多 PID 样本全量合并；时长取全部时序（含 GPU-only 的设备级指标）首末样本跨度——曾因只扫 pid_stats 致 GPU-only 会话时长 0，真机发现后修
- **基线存放**：`~/.local/share/xperf/baselines/<pkg>.json`（XDG 数据目录，用户数据语义，--clean-cache 不清理）；包名拼路径前校验；CLI/GUI 同一文件互通（GUI 保存的基线 CLI 可对比，反之亦然）
- **真机验证（SS3 svm，`--device` 全程指定）**：保存（20s，CPU 30.3 均值/PSS 462MB/FPS 29.7/Jank 5.98 次分/GPU 15.3%）→ 对比（同场景二次运行，持平 9 项 + 单侧 5 项，GPU 15.4 vs 15.3 不误报）→ 篡改基线制造回归（CPU 基线压到 10% → ⚠ 回归 4 项 + 指标名列表）→ 无基线包（"未找到基线（先用 --save-baseline 保存一次）"）→ 篡改后真数据重存恢复
- **多设备真机验证（SS3 + Redmi 1280da60 双连）**：CLI 不指定 → 报错列设备清单；`--device 6eb792dfb0f` → 平台 SS3 + QNX GPU 通道正常（15.3% busy 同单设备场景）；`--device 1280da60` → Android 平台 + com.miui.home 采样正常（pid 7424）；GUI `--package --device` 自动启动全链路（平台 SS3、CPU/Mem/GPU 事件流）；单台连接自动选择；`--device deadbeef` → "指定设备不在线"跳过自动启动
- **设备动态检测（951c0ab，用户要求）**：监视线程 3s 轮询 diff（core `diff_devices` 纯函数 + 单测）；首轮只建快照不通知（首屏 loadDevices 已填）；目标设备移除不清后端 serial（断连重连插回即恢复），下拉占位"（已断开）"+ 状态提示。**真机验证方法论**：kill-server/reconnect/wait-for-disconnect 均制造不出 diff（server 重启后设备枚举快于 3s 轮询窗、transport_id 变但 serial 不变）——热插拔验证必须物理插拔（SS2MAX d1f39648c1f 两轮插拔，接入/移除事件对全捕捉）
- **Android 版本检测 + SS3 FPS 恒 0 修复（9cfa9a5，用户质疑 SS2 FPS=0 引出）**：**方法论**——用户在 SS2MAX 上看到 FPS 正常（60fps，SurfaceView - 层）而 SS3 上恒 0（GPU busy 24% 明明在渲染），矛盾即 bug；逐层实证：SS3 `--list` 有 `SurfaceView[...](BLAST)#0` 层（方括号格式，SS2 是 `SurfaceView - `），`--latency` 该层帧时间戳 16.7ms 间隔（60fps 真帧流）；agent 的 ownerPID 匹配拿到静止 Activity View 层（非空）即跳过包名兜底 → BLAST 层漏掉。**修复**：图层发现改 ownerPID ∪ 包名并集（Android 12+ BLAST 合成 app 直提 buffer，真实帧流在 BLAST 层；无 buffer 辅助层并入无害——空缓冲层不建基线不发事件）。SS3 60fps 恢复、SS2MAX（Android 11 无 BLAST）回归无变化。**版本检测价值即刻印证**：SS3=12/SS2MAX=11 正好解释两机帧层差异
- **webview 间歇空白（9cfa9a5 修）**：同代码两次启动一好一坏（16:14 正常 / 16:28、16:30 连续空白），`WEBKIT_DISABLE_DMABUF_RENDERER=1` 手动启动即恢复 → webkit2gtk DMABUF 渲染路径在浏览器等 GPU 重负载占用时间歇失败；修复：main 最开头进程内 set_var。**此前把空白归因于 setup set_size 时序是误判**（当时恰好一次空白一次正常，恰逢 set_size 变更），CLAUDE.md 已修正——归因须多轮复现支撑
- **默认窗口尺寸（7cb4b4b，用户反馈"默认大小得调整，不要有内容无法显示"）**：resize_default 命令按屏幕比例 clamp；侧栏勾选两列 + PIDs 限高。验证法：xdotool 定位窗口 + import 截图 + 视觉转写核对侧栏全量内容（清理按钮/PIDs 区可见）
- **UI 布局调整（8d01922 + 78f93a9，用户连续反馈）**：PIDs 先迁主区面板行后进一步融入状态栏「监控中: pkg（PID xxx）」（用户：不用单独一栏）；「函数热点」tab 改名「Simpleperf 分析」；GUI 打点功能整体删除（用户：不需要——控件/marker 事件/竖线绘制/add_marker 命令全链路清掉，CLI socket 打点保留）
- **单位对齐（78f93a9，用户要求"同类数据单位统一"）**：内存统一 MB（CLI 打印/CSV/图表 + GUI 面板/图表/峰值/导出 CSV），协议与基线 JSON 存储仍 KB（字段名自带单位）；GUI 基线汇总把前端 MB 转回 KB 存（与 CLI 口径一致，已存基线兼容）。其余指标核对无混用（GPU 显存 MB/IO·网络 KB/s/频率 MHz/温度 °C）。**注意 memory CSV 数值从 KB 变 MB**（表头标注），旧会话 CSV 消费看表头。GUI 验证方法：截图受窗口遮挡/工作区不稳定 → 临时 _diag DOM 断言（读 memChart.title/unit + peakTable 表头）验后删——diag_log 落 `/tmp/xperf_gui_diag.log` 非 stderr
- **验证门槛**：100 测试全绿（agent 24 + core 62+2 ignored + xperformance 8 + GUI 4 + xrm 2；review 后 +3：S1 负 delta 方向×2 + G1 全不可比），clippy 零警告，cargo doc 零 warning（默认 lint 集 + 3 crate missing_docs）

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
