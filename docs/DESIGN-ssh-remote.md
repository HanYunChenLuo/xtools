# SSH 远程调试（Remote Backend）架构设计

> **目标**：真机连在远端 Linux 机（`hppc`）上，本机（Mac）跑 GUI/CLI，
> 经 SSH 完成采样、perfetto、simpleperf 全部功能。
>
> **状态**：设计稿 v2（2026-09-09），待实现。
> **文中所有机制结论均为 Mac↔hppc 真实拓扑实测**（设备物理接在 hppc），记录见附录 A。
>
> v1 曾据反向隧道误判「`forward` 端口在客户端侧监听」，导致 agent 通道设计错误；
> v2 已在真实拓扑复验并修正（见 4.4 第二跳隧道）。作废方案的评估结论见附录 B。

---

## 目录

1. [核心机制：adb 的执行侧语义](#1-核心机制adb-的执行侧语义)
2. [方案选型](#2-方案选型)
3. [系统架构总览](#3-系统架构总览)
4. [模块设计](#4-模块设计)
5. [关键流程](#5-关键流程时序)
6. [用户界面](#6-用户界面)
7. [边界：不受影响的部分](#7-边界不受影响的部分)
8. [风险与缓解](#8-风险与缓解)
9. [实施计划](#9-实施计划)
10. [测试设计](#10-测试设计)
- [附录 A：实测记录](#附录-a实测记录)
- [附录 B：已否决方案](#附录-b已否决方案)
- [附录 C：代码位置索引](#附录-c代码位置索引)

---

## 1. 核心机制：adb 的执行侧语义

整个设计建立在一个事实上：**adb 是 client/server 架构，不同子命令的「执行侧」不同。**
让本机 adb **客户端**连远端 adb **server**（`ADB_SERVER_SOCKET`），各子命令行为如下
（全部真实拓扑实测，附录 A #25-30）：

| 子命令 | 执行侧 | 对本项目的含义 |
|---|---|---|
| `pull <dev> <local>` | **client（Mac）文件系统** | trace/stack 产物**直接落本机** ⇒ `trace_processor`/`report_html.py` 零改动 ✅ |
| `push <local> <dev>` | **client（Mac）文件系统** | agent 二进制从本机直推 ⇒ `deploy_agent` 零改动 ✅ |
| `shell`（含 stdin 管道） | 透传 | perfetto config 灌 stdin 零改动 ✅ |
| `shell` 退出码 | 保真（实测 `exit 42` → 42） | Ctrl-C/信号判定零改动 ✅ |
| `devices -l` / `getprop` | 列远端设备 | 设备枚举天然指向 hppc ✅ |
| **`forward tcp:0 <target>`** | **server（hppc）监听** ⚠ | **本机连不上** ⇒ 需第二跳隧道（4.4） |

**这张表是全部设计的地基**：五条「零改动」让改造面收敛，一条例外催生了唯一的新机制。

> **为什么 `forward` 与众不同**：`pull`/`push` 的文件 I/O 由客户端进程完成（server 只转发 USB
> 字节流）；而 `forward` 是**在 server 上注册一条转发规则**，监听 socket 自然由 server 创建。
> 规则也因此由 server 持有 —— 这同时衍生出 R10（规则跨会话残留）。

---

## 2. 方案选型

**采用**：adb server 前移 + SSH 隧道（本机 adb 客户端连 hppc 的 adb server）。

三条替代方案已评估否决，完整论证见 [附录 B](#附录-b已否决方案)：

| 方案 | 一句话否决理由 |
|---|---|
| A. 命令包装（`adb X` ⇒ `ssh hppc adb X`） | 客户端跑到远端 ⇒ `pull`/`push` 落点全搬走，每处要补 scp；叠加多层 shell 转义 |
| C. 自研远端 server | 「远端 server + client 连接」**正是 adb 自身架构**，自研等于重写 `adb server`；且实测**带宽无收益**（只快 0.4s） |
| D. adb server 监听公网口 | 实测**比压缩隧道慢 4.5×**；且依赖「隔离网」这一运维状态而非代码不变量 |

**为什么 ssh 是对的传输**：实测 hppc 上真实安装的 VS Code Remote SSH ——
其 server **10+ 次启动无一例外绑 `127.0.0.1`**，并内嵌 `vscode-russh` **自带 SSH 栈做转发**
（附录 A #18-21）。业界成熟方案同样是「远端 server + ssh 传输」，而非用 server 规避 ssh。

---

## 3. 系统架构总览

### 3.1 分层视图

```
┌───────────────────── Mac（本机）─────────────────────┐
│                                                      │
│  表现层    CLI (xperformance)      GUI (xperf-gui)    │
│              │                        │              │
│  ────────────┴────────────────────────┴────────────   │
│  能力层    trace.rs   simpleperf.rs   coldstart.rs    │
│            baseline.rs   platform/    agent.rs        │
│              │                                        │
│  ────────────┴──────────────────────────────────────  │
│  传输层    utils.rs: adb_command() ◄── transport.rs   │
│            （唯一 adb 构造点）         Transport 枚举  │
│                    │                   SshTunnel      │
│  ──────────────────┼──────────────────────────────    │
│  本机 adb client ──┤                                  │
│                    │  ADB_SERVER_SOCKET=tcp:127.0.0.1:P_srv
│  本机资源（不变）： │                                  │
│   /tmp/xperf 落盘  │   trace_processor / python3      │
│   ~/.cache/xperf   │   open / Chrome / Perfetto UI    │
└────────────────────┼─────────────────────────────────┘
                     │
        ssh ControlMaster（单连接，多 channel 复用，Compression=yes）
                     │
     ┌───────────────┼──────── hppc（远端）─────────────┐
     │  hop#1: P_srv ─────► 127.0.0.1:5037  adb server  │
     │  hop#2: P_loc ─────► 127.0.0.1:P_fwd (adb forward)│
     │                              │                    │
     └──────────────────────────────┼────────────────────┘
                                    │ USB
                            ┌───────┴────────┐
                            │  真机（Android）│
                            │  xperf-agent    │
                            │  daemon         │
                            │  localabstract: │
                            │  xperf-agent    │
                            └─────────────────┘
```

### 3.2 两跳隧道的必要性

| 跳 | 本机端口 | 远端目标 | 承载 | 数量 |
|---|---|---|---|---|
| **hop#1** | `P_srv`（自选） | `127.0.0.1:5037` | adb 协议全部命令（devices/shell/pull/push/forward…） | 恒 1 条 |
| **hop#2** | `P_loc`（自选） | `127.0.0.1:P_fwd` | agent NDJSON 事件流（每设备一条） | **每设备 1 条** |

`P_fwd` 是 `adb forward tcp:0 localabstract:xperf-agent` 在 **hppc** 上分配的端口。
hop#2 把它映射到本机，使 `agent.rs:389` 的 `TcpStream::connect(("127.0.0.1", port))`
**连接代码完全不变** —— 变的只是 `ensure_daemon()` 返回哪个端口。

两跳共用**同一条 ssh 连接**（ControlMaster），经 `-O forward` 动态增删，不新起 ssh 进程
（实测，附录 A #31）。

### 3.3 数据流分类

| 流 | 方向 | 量级 | 路径 |
|---|---|---|---|
| 采样事件（NDJSON） | device → Mac | KB/s，持续 | hop#2，实时 |
| adb 控制命令 | Mac → device | 字节级，~0.12s/次往返 | hop#1 |
| trace/stack 产物 | device → Mac | **数十 MB～GB**，一次性 | hop#1（`pull`，压缩后 ~17MB/s） |
| agent 二进制 | Mac → device | ~900KB，偶发 | hop#1（`push`） |
| 分析产物（报告/图表/CSV） | 本机内 | — | **不过网**（本机落盘 + 本机工具链） |

---

## 4. 模块设计

### 4.1 新增：`xperf-core/src/transport.rs`

```rust
//! adb 传输后端：本机 adb server 或经 SSH 隧道的远端 adb server。
//!
//! 设计约束：本机恒为 adb **客户端**，故 `pull`/`push` 落点、分析工具链、
//! 桌面打开全部留在本机；只有 `adb forward` 的监听端口在远端，需第二跳隧道
//! （见 [`SshTunnel::add_forward`]）。

/// adb 服务端位置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    /// 本机 adb server（默认；行为与改造前逐字节一致）
    Local,
    /// 远端 adb server，经 SSH 本地端口转发抵达
    Ssh(SshTarget),
}

/// SSH 远端描述。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshTarget {
    /// ssh 目标：`ssh_config` 中的 Host 别名（如 `hppc`）或 `user@host`。
    /// 用别名可复用用户既有的免密/跳板/端口配置。
    pub host: String,
    /// 远端 adb 可执行路径。**不能假设在 PATH 中**（实测 hppc 即不在）。
    /// 默认 `"adb"`。
    pub adb_path: String,
    /// 远端 adb server 监听端口，默认 `5037`。
    pub remote_port: u16,
}

/// 当前传输后端（进程级全局，与既有 `TARGET_SERIAL` 同款模式）。
pub fn transport() -> Transport;
/// 设置传输后端。须在任何 adb 调用之前完成。
pub fn set_transport(t: Transport);

/// 初始化远程后端：建 hop#1 → 校验协议版本 → 返回远端设备列表。
pub fn init_remote(target: SshTarget) -> Result<Vec<AdbDevice>>;
/// 关闭远程后端：清理本工具注册的 forward 规则 → 关隧道 → 回落 `Local`。
pub fn shutdown_remote();
```

### 4.2 `SshTunnel`：隧道与端口映射

```rust
/// 一条 SSH ControlMaster 连接，承载 hop#1（adb server）与 N 条 hop#2（agent 通道）。
pub struct SshTunnel {
    host: String,
    control_path: PathBuf,
    /// hop#1 本机端口 → 远端 adb server
    server_port: u16,
    /// hop#2 映射表：远端 forward 端口 → 本机端口。复用防泄漏。
    forwards: Mutex<HashMap<u16, u16>>,
}

impl SshTunnel {
    /// 建立隧道（hop#1）。
    /// ① `ssh <host> '<adb_path> start-server'` 确保远端 server 在跑
    /// ② 自选本机空闲端口（`-L 0:` 不被支持，见下）
    /// ③ `ssh -M -S <ctl> -f -N -o … -L <P_srv>:127.0.0.1:<remote_port> <host>`
    /// ④ 探活：`host:version` 读协议版本
    pub fn establish(target: &SshTarget) -> Result<Self>;

    /// 为远端 forward 端口新增 hop#2，返回**本机**端口；已映射则复用。
    /// `ssh -S <ctl> -O forward -L <P_loc>:127.0.0.1:<remote> <host>`
    pub fn add_forward(&self, remote_port: u16) -> Result<u16>;

    /// 移除 hop#2（会话结束）。
    /// `ssh -S <ctl> -O cancel -L <P_loc>:127.0.0.1:<remote> <host>`
    pub fn remove_forward(&self, remote_port: u16) -> Result<()>;

    /// hop#1 本机端口（写 `ADB_SERVER_SOCKET` 用）
    pub fn server_port(&self) -> u16;
    /// 隧道存活性：`ssh -S <ctl> -O check <host>`
    pub fn is_alive(&self) -> bool;
}
// Drop: `ssh -S <ctl> -O exit <host>`（实测可靠）
```

**ssh 参数（每条均有实测依据）**：

```
ssh -M -S <control_path> -f -N \
    -o ControlPersist=<n> \
    -o ServerAliveInterval=15 -o ServerAliveCountMax=3 \
    -o ExitOnForwardFailure=yes \
    -o Compression=yes \
    -L <P_srv>:127.0.0.1:<remote_port> <host>
```

| 参数 | 依据 |
|---|---|
| `Compression=yes` | **实测 47MB 真实 trace 拉取 11.17s → 2.68s（4.2×）**，与 zstd 理论最优仅差 0.4s（附录 A #14-17）。链路 ~31Mbps 远未到 CPU 瓶颈 |
| `ExitOnForwardFailure=yes` | 端口占用时立即失败而非静默降级（实测有效） |
| `ServerAliveInterval/CountMax` | 网络黑洞时主动断连，触发重连逻辑（R3） |
| `-M -S`（ControlMaster） | 单连接多 channel 复用；`-O forward/cancel/check/exit` 动态管理（附录 A #11/#31） |

**实现要点**：

1. **本机端口必须自选** —— `ssh -L 0:…` 报 `Bad local forwarding specification`，
   带 bind 地址亦然（附录 A #10）。做法：`TcpListener::bind("127.0.0.1:0")` 取端口后 drop，
   复用 `trace.rs:1014` `ui_server` 同款手法；存在极小 TOCTOU 窗口 ⇒ 失败换端口重试 3 次。
2. **`control_path`** 用 `~/.ssh/cm/xperf-<host>-<pid>`（带 pid 防多实例互踩）；
   UNIX socket 路径有长度上限，超长退化到 `temp_dir()`。
3. **绝不 `adb kill-server`** —— 远端 server 可能被 hppc 上他人共用
   （实测该 server 已存活 1.6 天）。本工具只 `start-server`。

### 4.3 注入点：`utils.rs`（改造面极小）

现有全部 adb 调用只经两个构造点：`run_command`（`utils.rs:19`）与 `adb_for`（`utils.rs:72`）。

```rust
/// 构造 adb 命令。本机恒为 adb **客户端**；远程模式经环境变量指向远端 server。
fn adb_command() -> Command {
    let mut c = Command::new("adb");           // 恒本机 adb 二进制
    if let Transport::Ssh(_) = transport() {
        if let Some(p) = tunnel_server_port() {
            c.env("ADB_SERVER_SOCKET", format!("tcp:127.0.0.1:{p}"));
        }
    }
    c
}
```

- 新增 `run_adb(args)`，把 `run_command("adb", …)` 的调用点
  （`utils.rs:42/44/137/140`）改为它；`run_command` 保持通用语义**不变**。
- `adb_for`（`utils.rs:72`）改为基于 `adb_command()` 构造。

**为何用 `ADB_SERVER_SOCKET` 而非 `-H/-P`**（两者实测等价）：`adb_for` 返回**裸 `Command`**
供调用方追加参数（如 `agent.rs:554` 追加 `forward --list`），
环境变量不参与 argv 顺序，不会与调用方追加的 `-s`/子命令位置冲突。

### 4.4 agent 通道：第二跳隧道（v2 修正的核心）

**问题**：`ensure_forward()`（`agent.rs:551`）返回的端口监听在 **hppc**，
本机 `TcpStream::connect(("127.0.0.1", port))` **必然失败**（实测 `Connection refused`）。

**解法**：`ensure_daemon()` 按 Transport 分流端口来源，连接逻辑不动。

```rust
/// 确保 daemon 在跑，返回**本机可直连**的 agent 端口。
fn ensure_daemon(serial: Option<&str>) -> Result<u16> {
    let remote_port = ensure_forward(serial)?;   // 现有逻辑，零改动
    match transport() {
        // 本地：forward 监听在本机 server 上，端口直接可用
        Transport::Local => Ok(remote_port),
        // 远程：forward 监听在 hppc，叠 hop#2 映射到本机
        Transport::Ssh(_) => tunnel().add_forward(remote_port),
    }
}
```

其余全部沿用现有实现：`probe_daemon`（`:533`）、版本协商与 `suicide`（`:449`）、
ping 保活（`:413`）、`AgentStream`（`:236`）—— 它们只认「一个本机端口」，
不关心端口从哪来。

**已实测打通**（附录 A #28-29、#31）：
- 两跳链路读到真实 hello：`{"t":"hello","ncores":8,"maxkhz":[…],"version":2}`
- 下发 `start --package com.google.android.filament.gltf --interval 500 --cpu --freq`
  → 5s 收 **18 个事件**（`freq` 10 + `cpu` 8），CPU **40.40%**，pid 8620

**映射表复用是必需的**：实测 `adb forward tcp:0` **每次调用都新建一条规则**
（连调 3 次得到 3 个不同端口，附录 A #32）。因此：
- `ensure_forward` 既有的「先查 `forward --list` 复用同名规则」逻辑（`agent.rs:554`）
  是**防泄漏的关键**，远程模式下必须保留；
- `SshTunnel.forwards` 映射表同理，同一 `remote_port` 只建一条 hop#2。

### 4.5 断线重连

现有 `reconnect_agent`（`agent.rs:653`，每 500ms 轮询 `device_online`）需增加一层判别 ——
**隧道本身可能断**（网络抖动、hppc 重启）：

```
device_online(serial) 失败
├─ Transport::Local → 现有逻辑（等设备回来）
└─ Transport::Ssh   → tunnel.is_alive()?
     ├─ 否 → 重建隧道（指数退避 1s/2s/4s…上限 30s）
     │        └─ 成功后：清空 hop#2 映射表 + 本进程 forward 端口缓存
     │           ⇒ 让 ensure_daemon 走完整的 forward + add_forward + probe
     └─ 是 → 现有逻辑（设备侧问题，如拔插/重启）
```

**关键**：隧道重建后旧 `P_loc`/`P_fwd` 全部失效，**必须清空映射表**，否则会连到死端口。

`spawn_device_monitor`（`xperf-gui/src/main.rs:744`，每 3s 枚举设备）需加短路：
隧道死时暂停轮询并 emit 状态，避免每 3s 刷一次错误日志。

---

## 5. 关键流程时序

### 5.1 启动（远程模式）

```
CLI/GUI 解析 --remote hppc
  │
  ├─ init_remote(SshTarget)
  │    ├─ ssh hppc '<adb_path> start-server'        ← 远端 server 就绪
  │    ├─ 自选 P_srv → ssh -M -N -f -L P_srv:127.0.0.1:5037   ← hop#1
  │    ├─ set_transport(Ssh(target))
  │    ├─ host:version 校验协议版本（不一致 → 报错中止，见 R2）
  │    └─ list_adb_devices()                        ← 已指向 hppc
  │
  ├─ pick_device(--device, devices)                 ← 既有策略不变
  └─ 采样/深挖流程（与本地模式同一代码路径）
```

**顺序是硬约束**：`init_remote` 必须早于**任何** adb 调用，
特别是 `select_device`（`xperformance/src/main.rs:118`）与 GUI 的 `list_adb_devices()`
（`xperf-gui/src/main.rs:1159`）。

### 5.2 采样会话建立（每设备）

```
spawn_agent(pkg, interval, flags, platform, serial)
  ├─ ensure_agent_built()                    本机 cargo + NDK（不过网）
  ├─ deploy_agent(bin, serial)
  │    ├─ adb shell stat（size/mtime 快检）   hop#1
  │    └─ adb push <本机路径> <设备路径>       hop#1，源读**本机** ✅
  ├─ ensure_daemon(serial)
  │    ├─ ensure_forward → P_fwd（hppc 上）   hop#1
  │    ├─ [Ssh] add_forward(P_fwd) → P_loc    ← hop#2 新建/复用
  │    └─ probe_daemon(P_loc) 读 hello 校验版本
  ├─ TcpStream::connect(("127.0.0.1", P_loc))  ← 代码零改动
  ├─ 写 "start --package … --cpu …"
  └─ AgentStream（reader + 5s ping 线程）
```

### 5.3 深挖录制（trace/stack）

```
trace::record(seconds, out_dir, progress, serial)
  ├─ adb shell perfetto -c - --txt -o <设备路径>    hop#1，config 灌 stdin ✅
  ├─ （录制 N 秒，进度回调）
  ├─ adb pull <设备路径> <out_dir/…>               hop#1，落**本机** ✅
  ├─ adb shell rm -f <设备路径>                    hop#1
  └─ trace_processor_shell -q …                    **本机执行，不过网** ✅
```

simpleperf 同构：`record` → 设备端 `report` 三视图 → `pull .data` → 本机 `report_html.py` 出火焰图。

### 5.4 会话结束与清理

```
停止采样
  ├─ AgentStream::kill()（关 TCP → daemon 侧会话收尾）
  └─ [Ssh] tunnel.remove_forward(P_fwd)        清 hop#2
        + adb forward --remove tcp:P_fwd       清远端规则（R10）

退出 / 切回本机
  └─ shutdown_remote()
       ├─ 逐条清理本工具注册的 forward 规则（**禁用 --remove-all**，见 R10）
       ├─ ssh -O exit（带走 hop#1 与全部 hop#2）
       └─ set_transport(Local)
```

---

## 6. 用户界面

### 6.1 CLI

```bash
# 本地（默认，行为与改造前完全一致）
xperformance --package com.foo --cpu

# 远程
xperformance --remote hppc --package com.foo --cpu
xperformance --remote hppc --remote-adb ~/Android/Sdk/platform-tools/adb --package com.foo --cpu
xperformance --remote hppc --device d1f39648c1f --cpu --trace 10
```

| 参数 | 默认 | 说明 |
|---|---|---|
| `--remote <host>` | 无（本地模式） | ssh 目标；别名复用 `ssh_config` |
| `--remote-adb <path>` | `adb` | 远端 adb 路径（**PATH 常不含**，实测 hppc 即如此） |
| `--remote-adb-port <n>` | `5037` | 远端 adb server 端口 |

与 `--device` 组合：先建隧道，再在**远端**设备列表里按既有 `pick_device` 策略选台。

### 6.2 GUI

顶栏设备 tab 区左侧新增「连接」控件：

```
[● 本机] [○ hppc] ＋   │   设备 tab: [SS3] [SS2MAX] …
```

- 默认「本机」，行为与现在一致；
- 「＋」弹配置（host / adb 路径 / 端口），存 `~/.config/xperf/remotes.json`；
- 切换连接 = 停全部会话 → `shutdown_remote()` → `init_remote()` → 重建设备 tab；
- 隧道状态进 status 栏（连接中 / 已连接 / 断开重连中）。

新增 Tauri 命令（与既有 18 个并列，注册于 `xperf-gui/src/main.rs:1257`）：

| 命令 | 签名 | 说明 |
|---|---|---|
| `list_remotes` | `() -> Vec<RemoteConfig>` | 读配置 |
| `save_remote` | `(cfg: RemoteConfig) -> Result<()>` | 增改 |
| `connect_remote` | `(host: Option<String>) -> Result<Value>` | `None` = 切回本机；返回设备列表 |
| `remote_status` | `() -> RemoteStatus` | 隧道存活/端口/延迟 |

新增事件 `remote-status`：`{state, host, message}` → 前端 status 栏。

**启动顺序（易错点）**：`xperf-gui/src/main.rs:1143-1194` 现为
「解析 argv → `list_adb_devices()` → `pick_device`」，远程模式须插在枚举之前：

```
解析 argv → [--remote] init_remote() → list_adb_devices() → pick_device → 自动启动
```

### 6.3 多设备并行

**天然支持，无需额外设计** —— 隧道位于 adb client↔server 之间，比「设备」低一层。

| 现有机制 | 远程模式下 |
|---|---|
| `-s <serial>` 路由（`utils.rs:71`） | 无变化（serial 是 adb 协议内寻址） |
| GUI `HashMap<serial, DeviceSession>`（`main.rs:1198`） | 无变化 |
| 每设备 `forward tcp:0`（`agent.rs:566`） | 端口在 hppc 分配，各设备本机侧一条 hop#2 |
| trace/stack 并发 | 无变化（各自 `adb shell` + `pull`，落点本机） |
| 热插拔监视（`main.rs:744`） | 无变化（隧道死时短路） |
| QNX GPU 通道全局独占 | 无变化（**设备端/QNX 侧**约束，与 adb 位置无关） |

**队头阻塞实测不成立**（附录 A #22-24）：200ms 采样节拍在并发 3×47MB 拉取（141MB）下
p50 仍 **200ms** —— ssh channel 独立流控，小流量不被大流量饿死。24 条并发转发 channel 全通。

**唯一并行代价是带宽共享**：总吞吐守恒，N 台同时拉 trace ≈ N × 单台
（压缩后单个 46MB ~3s，3 台并发 ~14s）。链路 ~31Mbps 是硬约束，与 ssh 无关。

> **注**：`MaxSessions`（默认 10）约束的是**交互式 session/exec channel**，
> 不管 `-L` 的 `direct-tcpip` ⇒ **无需改 hppc 的 sshd 配置**。
> 但若将来实现里并发起多个 `ssh <host> <cmd>` exec，那些受其约束。

---

## 7. 边界：不受影响的部分

以下全部**留在本机、零改动** —— 这是方案 B 的核心收益（方案 A/C 下每一项都要改）：

| 类别 | 位置 |
|---|---|
| `trace_processor_shell` SQL 分析 | `trace.rs:466` |
| `python3 report_html.py` 火焰图 | `simpleperf.rs:739` |
| `host_report_lib()` 平台库选择 | `simpleperf.rs:345`（本机 macOS → dylib，正确） |
| 本地 Perfetto UI 镜像服务器 | `trace.rs:1014` |
| `open`/`xdg-open`/Chrome/Finder/dbus | `trace.rs:936/1125/1163/1173`、`simpleperf.rs:786` |
| `/tmp/xperf` 数据根、CSV/图表落盘 | `xperformance/utils.rs:41`、`xperf-gui/main.rs:321` |
| `~/.cache/xperf`、`~/.local/share/{xperf,perfetto}` | `trace.rs:833/273`、`baseline.rs:207` |
| `simpleperf_scripts` vendor 目录 | `simpleperf.rs:330` |
| marker Unix socket | `marker.rs:23` |
| agent 交叉编译（`cargo build` + NDK） | `agent.rs:317`（产物经 `adb push` 直达设备） |
| QNX telnet 通道 | `agent.rs:631`（**从设备端发起**，与 adb 位置无关） |
| 基线对比 / 阈值告警 / 图表 | `baseline.rs`、`xperformance/alerts.rs`、`utils.rs` |

---

## 8. 风险与缓解

| # | 风险 | 影响 | 缓解 |
|---|---|---|---|
| R1 | 隧道带宽瓶颈 | 46MB trace ~2.7s；600s trace（2.8GB）≈ 3min | 隧道必带 `Compression=yes`（实测 4.2×）；GUI 拉取阶段显示进度 |
| R2 | **adb 协议版本**不符会杀远端 server | 打断 hppc 上他人调试 | 判据是协议版本（`1.0.41`）**而非 platform-tools 版本**（实测 36.0.2 连 36.0.0 安全，因协议同为 1.0.41）。`host:version` 比对，不一致则**报错中止**并提示对齐版本；绝不主动 `kill-server` |
| R3 | 隧道静默半死（TCP 黑洞） | 采样卡住不报错 | `ServerAliveInterval=15` + `CountMax=3`；`is_alive()` 定期 `-O check` |
| R4 | 本机端口 TOCTOU 竞态 | 建隧道偶发失败 | 换端口重试 3 次 |
| R5 | 远端 adb 不在 PATH | 首次连接失败 | `--remote-adb` 可配；失败时提示并列出常见路径 |
| R6 | 远端 forward 端口冲突 | forward 失败 | `tcp:0` 由 adb 自选（现状即如此，`agent.rs:566`） |
| R7 | ControlMaster socket 路径过长 | 建隧道失败 | 超长退化到 `temp_dir()` |
| R8 | 多设备场景枚举出远端全部设备 | 误选设备 | 既有 `pick_device` 策略不变（多台须 `--device`） |
| R9 | 隧道 Drop 未执行（SIGKILL） | 残留 ssh master | `control_path` 带 pid；启动时清理无主的 `xperf-*` control socket |
| **R10** | **forward 规则残留在共享 server 上**（实测确认） | 规则由 **server** 持有，本机进程死亡也不消失；`tcp:0` 每次新建一条 ⇒ 跨会话累积，多人共用时互相可见 | ① 会话结束显式 `forward --remove <port>`；② 保留 `ensure_forward` 的「查 list 复用同名规则」逻辑（`agent.rs:554`）—— 同 serial+同 socket 只一条；③ **禁用 `forward --remove-all`**（全局生效，会踢掉他人规则）；④ `shutdown_remote` 逐条清理 |
| **R11** | `adb root` 致 adbd 重启，规则/隧道失效 | agent 通道断 | 实测 `adb root`（adbd 已 root）后 forward 规则与设备均存活；**未覆盖首次 root 真重启场景** ⇒ S6 复验 `try_adb_root`（`agent.rs:332`）路径，必要时 root 后重建 hop#2 |
| **R12** | 每次 adb 往返 ~0.12s（本机 ~0.01s） | 多步链路可感延迟（`deploy_agent` stat 双查、`coldstart` 多步） | 均非高频路径，可接受；`list_adb_devices` 的 N 次 `getprop` 可缓存（Android 版本不变） |

---

## 9. 实施计划

| 步 | 内容 | 验证 |
|---|---|---|
| S1 | `transport.rs`：`Transport`/`SshTarget` + 全局存取 + 单测 | `cargo test` |
| S2 | `SshTunnel::establish/is_alive/Drop` + 端口自选重试 | 集成验证隧道起停、`-O check` |
| **S2a** | **`add_forward`/`remove_forward` + 映射表**（hop#2） | 单测复用/清理；集成读到 agent `hello`（已预验 #28/#31） |
| S3 | `utils.rs` 注入：新增 `run_adb` + 改造 `adb_for` | **回归门槛：本地模式 105 测试全绿** |
| S4 | `init_remote`/`shutdown_remote` + 协议版本校验 + forward 清理 | 远端 `adb devices` 通；退出后 `forward --list` 干净 |
| S5 | CLI 三参数，接在 `select_device` 之前 | `--remote hppc --cpu` 端到端 |
| S6 | agent 链路（`ensure_daemon` 走 hop#2 / probe / daemon / 采样流）+ **R11 复验** | 远程 50ms 采样 CSV 与本地口径一致 |
| S7 | trace 远程 | `--remote hppc --trace 10`：pull 落本机 + 报告生成 |
| S8 | simpleperf 远程 | `--remote hppc --stack 10`：三视图报告 + 火焰图 HTML |
| S8a | **多设备并行远程** | 两台同时采样 + 一台 `--trace` + 另一台 `--stack`：口径一致、目录不撞名、节拍不退化 |
| S9 | 隧道断连重连（含映射表清空） | `-O exit` / 拔网线制造断连 |
| S10 | GUI 连接切换 UI + 4 命令 + `remote-status` 事件 | 手动目验 + 命令级测试 |
| S11 | 文档：CLAUDE.md 新增「SSH 远程后端」节 + README | **`cargo doc` 零 warning**（项目强制） |

---

## 10. 测试设计

### 单元测试（CI 可跑）
- `SshTarget` 解析（别名 / `user@host` / 带端口）
- `Transport` 默认为 `Local`；全局存取
- `adb_command()`：`Local` 下**不含** `ADB_SERVER_SOCKET`；`Ssh` 下含且格式正确
- `control_path` 生成（带 pid、超长退化）
- 端口自选重试（注入 bind 失败）
- hop#2 映射表：同 `remote_port` 复用、`remove` 后移除

### 集成测试（需 hppc，标 `#[ignore]`）
- 隧道建立 / 探活 / 关闭
- 远端 `adb devices` 与 `ssh hppc adb devices` 结果一致
- **落点断言**（核心不变量）：远程 `adb pull` 后 `local_path` 在**本机**存在
- **hop#2 断言**：`ensure_daemon` 返回端口在本机可 connect 并读到 `hello`
  —— 若 adb 改变 forward 监听侧语义，此测试立即失败
- **规则清理断言**（R10）：`shutdown_remote` 后 `forward --list` 不含本工具规则
- 协议版本一致时远端 server pid 不变

### 真机回归（人工）
按 S6-S9；重点「同设备先本地后远程各采 20s，数值口径一致」。

---

## 附录 A：实测记录

环境：Mac adb 36.0.2 / hppc adb 36.0.0（协议均 `1.0.41`）、hppc OpenSSH 9.6p1、
RTT avg 10.4ms、链路 ~31Mbps。**#25 起设备 `d1f39648c1f`（HU_SS2MAXF）物理接在 hppc**，
即真实目标拓扑。

### 第一轮：机制探查（#1-13，部分用反向隧道，见文末订正）

| # | 项 | 结果 |
|---|---|---|
| 1 | 本机客户端经 `-L` 用远端 adb server | ✅ 通 |
| 2 | 版本不符不杀远端 server | ✅ pid/etimes 连续（真因见 #30） |
| 3 | `-H/-P` 与 `ADB_SERVER_SOCKET` 等价 | ✅ 等价（选 env，见 4.3） |
| 4 | `pull` 落客户端侧 | ✅（#27 真实拓扑复验一致） |
| 5 | `push` 读客户端侧 | ✅（#26 真实拓扑复验一致） |
| 6 | ~~`forward` 在客户端侧监听~~ | ❌ **结论错误** → 见 #25 与文末订正 |
| 7 | stdin 管道透传 | ✅ `wc -l` = 3 |
| 8 | 退出码保真 | ✅ `exit 42` → rc=42 |
| 9 | 吞吐（48MB，**未压缩**） | 本地 1.339s vs 隧道 10.064s；当时「已达链路上限」结论**不完整**，见 #16 |
| 10 | `-L 0:` 动态端口 | ❌ `Bad local forwarding specification`（带 bind 地址亦然）⇒ Rust 侧自选 |
| 11 | ControlMaster `-O forward/cancel/check/exit` | ✅ 均可用 |
| 12 | 远端 adb 在 PATH | ❌ 不在（在 `~/Android/Sdk/platform-tools/adb`）⇒ `--remote-adb` 必需 |
| 13 | 远端环境 | python3 3.12.3、trace_processor 缓存均有（方案 B **不需要**，仅记录） |

### 第二轮：带宽与选型（真实 47MB pftrace，非 `/dev/zero`）

素材 47,074,325 B。压缩：gzip -6 → 12.2MB（0.96s）、**zstd -3 → 8.7MB（0.11s，5.4×）**。
所有传输均校验字节数一致。

| # | 路径 | 耗时 | 结论 |
|---|---|---|---|
| 14 | 直连 TCP（无 ssh 无压缩） | **12.18s** | 去掉 ssh 并不更快 |
| 15 | 隧道 `Compression=no` | **11.17s** | 与 #14 同量级 ⇒ **链路是瓶颈，ssh 开销测不出** |
| 16 | 隧道 `Compression=yes` | **2.68s** | 一参数换 4.2×；证明 `-C` 对 `-L` 转发流生效 |
| 17 | `ssh "zstd -3 -c"` 管道 | **2.24s** | 仅快 0.4s ⇒ 自研 server 无带宽理由 |

### 第三轮：VS Code Remote SSH（hppc 上真实安装）

| # | 项 | 证据 |
|---|---|---|
| 18 | server 绑定地址 | `.cli.*.log` 中 10+ 条 `Listening on 127.0.0.1:<随机端口>`，**无一条 `0.0.0.0`** |
| 19 | server 启动参数 | `--socket-path=`/`--host=`/`--port=`/`--connection-token=` ⇒ loopback + token |
| 20 | 传输层 | `src/tunnels/local_forwarding.rs`、`channel_open_confirmation`/`channel_data`/`channel_eof`、内嵌 `vscode-russh` ⇒ **自带 SSH 栈转发** |
| 21 | 版本化安装 | `cli/servers/Stable-<commit>/{server,pid.txt,log.txt}` + `lru.json` |

### 第四轮：并行与队头阻塞

方法：hppc 起两个 loopback 服务（trickle 每 200ms 一行 NDJSON；bulk 发 47MB pftrace），
本机**一条** ssh 连接带两个 `-L`（`Compression=yes`）。

| # | 场景 | 采样节拍（目标 200ms） | 拉取 |
|---|---|---|---|
| 22 | 基线（仅采样流） | p50 **199ms**，max 207ms | — |
| 22b | 并发 1×47MB | p50 **210ms**，max 255ms | 3.12s |
| 23 | **并发 3×47MB（141MB）** | p50 **200ms**，max 474ms | 13.68s wall |
| 24 | 24 条并发 `direct-tcpip` channel | — | 全通 0 失败，2.02s ⇒ `MaxSessions` 不约束转发 channel |

### 第五轮：真实拓扑复验（推翻 #6）

| # | 项 | 结果 | 影响 |
|---|---|---|---|
| 25 | **`forward` 监听侧** | 分配 42583 → **hppc `ss` 有，Mac `lsof` 无**；Mac 连之 `Connection refused` | ⚠ 推翻 #6 ⇒ 新增 hop#2（4.4） |
| 26 | `push` 读哪侧 | 源仅存于 Mac（hppc 已删）→ 设备内容 `src-on-mac-only` | ✅ 读 client |
| 27 | `pull` 落哪侧 | **Mac 有**，hppc 无 | ✅ 落 client |
| 28 | hop#2 打通 agent 通道 | hppc fwd 39503 → `-L 55177:127.0.0.1:39503` → 读到完整 `hello` | ✅ 方案可行 |
| 29 | 两跳链路端到端采样 | `start --package com.google.android.filament.gltf --cpu --freq` → 5s **18 事件**（freq 10 + cpu 8），CPU **40.40%**，pid 8620 | ✅ 真机真包跑通 |
| 30 | 协议版本 | 两端均 `Android Debug Bridge version 1.0.41` | ⇒ #2 真因是**协议**版本相同（R2 表述修正） |

### 第六轮：实现序列与幂等性

| # | 项 | 结果 | 影响 |
|---|---|---|---|
| 31 | 完整生命周期：`-M` 建 master → `adb forward` → **`-O forward` 动态加 hop#2** → 读 `hello` → `-O cancel` → 确认 refused → `-O exit` | ✅ 全通；`-O forward` **不新起 ssh 进程**（单连接多 channel） | 锁定 4.2 API 设计 |
| 32 | `adb forward tcp:0` 幂等性 | ❌ **非幂等**：连调 3 次得 3 条规则（39831/33915/35615） | ⇒ `ensure_forward` 的「查 list 复用」是**防泄漏关键**；R10 累积风险确认 |
| 33 | `adb root` 后规则存活 | ✅ `adb root`（已 root）+ `wait-for-device` 后 forward 规则与设备均在 | R11：首次 root 真重启场景待 S6 复验 |

### 订正说明（重要方法论教训）

#4/#5/#6 使用**反向隧道**（当时设备接在 Mac 上，故让 hppc 当客户端）。
反向拓扑下 **Mac 扮演的是 server 而非 client** —— #4/#5 看的是对侧（hppc=client）故结论正确；
**#6 看的是 Mac 侧，角色恰好翻转，于是把「server 侧监听」误读为「client 侧监听」**。

⇒ **角色对称的实验，必须逐项确认「这一侧此刻扮演什么角色」，不能整体套用结论。**

所有测试产物已清理（设备端 `/data/local/tmp/xt_*`、hppc forward 规则与临时脚本、
两侧 `/tmp/xt_*`、全部 ssh 隧道与 control socket），并二次确认。

---

## 附录 B：已否决方案

### 方案 A：命令包装（`adb X` ⇒ `ssh hppc adb X`）

把程序名从 `adb` 换成 `ssh`，参数前缀 `hppc adb`。**否决**——`adb` 的文件语义锚在
「客户端进程所在机器」，包装后客户端跑在 hppc：

| 破坏点 | 位置 | 后果 |
|---|---|---|
| `pull` 落 hppc | `trace.rs:204`、`simpleperf.rs:243` | 后续 `metadata()`、`trace_processor`、`report_html.py` 全部找不到文件 |
| `push` 源在 hppc | `agent.rs:366/494` | 本机交叉编译产物不可见，须先 scp |
| 引号/转义 | `agent.rs:635`（QNX telnet 复合命令） | 多层 shell 嵌套 |
| 退出码/信号 | `trace.rs:181`、`simpleperf.rs:191` | ssh 退出码语义叠加，Ctrl-C 判定失真 |

每处都要补一条 scp 回传路径。
（注：`forward` 一条对 A/B 是**共同**代价，非 A 独有劣势。）

### 方案 C：自研远端 server

**「远端 server + client 连接」正是 adb 自身架构** —— `adb server` 就是持有 USB、
对外提供转发的服务端。方案 B 已是该架构，区别只在 server 是**白捡的**还是**自研的**。

自研 server 跑在 hppc 上，则它自己就是 adb 客户端 ⇒ `pull`/`push` 落点问题原样重现。
为消化，必须实现：① 双向文件传输协议（重写 scp）；② agent 流二级代理 + 重做重连；
③ shell 代理且保 stdin 流式与退出码；④ 认证加密（否则裸网络控制 root 设备，
或跑在 ssh 上 ⇒ 未规避 ssh）；⑤ server 自身部署与版本协商。

**带宽不构成理由**（#14-17）：自研 server 上压缩的最优 2.24s，
对比 ssh `-C` 的 2.68s **仅快 0.4s**。
唯一独有收益是天然同版本（消化 R2），而这条的廉价解法是两端对齐 platform-tools。

### 方案 D：adb server 监听公网口（`adb -a -P 5037 nodaemon server`）

hppc 在完全可信隔离网，**安全前提成立**，故不因安全否决。否决理由是**没有收益**：
- 实测直连裸 TCP **12.18s** vs 压缩隧道 **2.68s** ⇒ 去掉 ssh 反而慢 4.5×
- 需改远端 server 启动方式，且该 server 可能被他人共用
- 丢掉 ssh 免密/别名复用，需自管地址端口
- 「隔离网」是**运维状态**而非代码不变量；设备哪天接到别的网段，工具即成裸奔的 root 通道

⇒ 记录备查，不实现。若将来 ssh 不可用再单独评估。

---

## 附录 C：代码位置索引

| 主题 | 位置 |
|---|---|
| adb 命令唯一构造点 | `utils.rs:19`（`run_command`）、`:72`（`adb_for`） |
| `run_command("adb", …)` 调用点 | `utils.rs:42/44`、`:137`、`:140` |
| serial 归一 / 全局 | `utils.rs:82` `resolve_serial`；`:53` `TARGET_SERIAL` |
| 设备枚举 | `utils.rs:136` `list_adb_devices`（1+N 次往返） |
| **agent forward（需 hop#2）** | `agent.rs:551` `ensure_forward`、`:441` `ensure_daemon` |
| agent TCP 连接（代码不变） | `agent.rs:389/449/536` |
| agent daemon 协商 | `agent.rs:533` `probe_daemon`、`:225` `AGENT_PROTOCOL_VERSION` |
| agent 部署 | `agent.rs:348` `deploy_agent`、`:304` `ensure_agent_built`、`:332` `try_adb_root` |
| 重连 | `agent.rs:653` `reconnect_agent`、`:580` `device_online` |
| trace | `trace.rs:136`（stdin 灌 config）、`:204`（pull）、`:466`（trace_processor） |
| simpleperf | `simpleperf.rs:147`（record）、`:243`（pull）、`:739`（report_html.py） |
| CLI 设备选择 | `xperformance/src/main.rs:118` `select_device`、`:1428` 调用点 |
| GUI 命令注册 / 启动参数 / 热插拔 | `xperf-gui/src/main.rs:1257` / `:1143-1194` / `:744` |
| GUI 多会话状态 | `xperf-gui/src/main.rs:1198` `AppState.sessions` |
