# SS4（SA8797P）adb 桥接架构设计

> **目标**：SS4 真机（接 hppc）上，xtools 的设备枚举/agent 部署/采样/深挖全链路
> 无感支持 SS4 的「MindRT（Linux 主控）+ Android（GVM）」双系统拓扑。
>
> **状态**：设计稿 v1.2（2026-09-10）。自 review 修正（冷却语义统一 /
> GUI 网关过滤三处收敛 / serial 稳定性不变量 / §5.2 断言软化 / R8 并项）后，
> **S0 真机预验证已完成（hppc，结论见 §8 表「实测结论」列）**：R1/R2/R3/R4/R5/R7
> 全部成立（R3/R4 比假设更乐观）；**R8 推翻——ligfxprofilerd 在 MindRT 侧而非 GVM
> logcat**，GPU 通道须按 R8 fallback 改为 host 侧经网关读取（属 H 节 GPU 项）。
> 桥接主设计（方案 A）零改动，可进 S1-S6 实施。
> **依据**：飞书《SS4.0 (8797) USB ADB调试指南》
> （https://li.feishu.cn/docx/ID7JduPFEoM9G4xWH6ocbMZVnZe），
> 平台代码现状（`platform/ss4.rs` 桩 + `detect_platform` 的 SS4 单测），
> 以及 WORKSPACE H 节 bring-up 清单。
> **本文只做设计，不动代码**；带 ⚠ 标记的机制均为待真机验证假设。

---

## 目录

1. [SS4 adb 拓扑（文档提炼）](#1-ss4-adb-拓扑文档提炼)
2. [现状差距分析](#2-现状差距分析)
3. [方案选型](#3-方案选型)
4. [模块设计](#4-模块设计)
5. [关键流程](#5-关键流程时序)
6. [root 链路设计](#6-root-链路设计)
7. [边界：不受影响的部分](#7-边界不受影响的部分)
8. [风险与真机预验证清单](#8-风险与真机预验证清单)
9. [实施计划](#9-实施计划)
10. [测试设计](#10-测试设计)

---

## 1. SS4 adb 拓扑（文档提炼）

SS4（SA8797P）是 hypervisor 双系统：**MindRT（Linux PVM，主控）** 直接挂 USB，
**Android（GVM）** 不在 USB 上——PC 的 `adb devices` 默认**只能看到 MindRT**。
Android 在 MindRT 侧的地址是 `172.31.101.51:5555`，MindRT 上有中继服务监听
`tcp:5557`（✅ S0 实测：常驻、无需额外启动命令；实为 MindRT 上一个 `adb` 进程
LISTEN `127.0.0.1:5557`，跨 GVM 重启存活、跨 MindRT adbd 重启也存活），
把 PC 侧流量转进 Android adbd。

指南给出的进 Android 三条路（均以 USB 连 MindRT 为前提）：

| 方法 | 命令（PC 端） | 前提 | 产物 serial |
|---|---|---|---|
| 1. 厂商魔改 adb tool | `adb shell-android` | 装 8255 同款 adb tool（**win/linux only**，mac 要自行打 patch gerrit/1259680 编译） | `localhost:5559` |
| 2. **标准 adb forward+connect**（**本设计采用**） | `adb root`（MindRT）→ `adb forward tcp:5559 tcp:5557` → `adb connect localhost:5559` | 无（标准 adb） | `localhost:5559` |
| 3. 进 MindRT 再进 Android | `adb shell` →（MindRT 内）`adbconnect.sh` 或 `adb connect 172.31.101.51:5555` → `adb -s 172.31.101.51:5555 shell` | MindRT shell | `172.31.101.51:5555`（**只在 MindRT 侧可见，PC 不可见**） |

root/remount Android 的方法（同表，节选与本项目相关的）：

- **标准 adb 路径**（方法 3.3/3.4）：`adb shell rootandroid.sh` / `remountandroid.sh`
  ——**脚本跑在 MindRT 上**（S0 实测在 `/system_ext/bin/`，PATH 内），由它把 Android
  的 adbd 提到 root。~~⚠ 隐含假设：`adb -s localhost:5559 root` 对标准 adb 不可用~~
  **✅ S0 实测推翻：标准 adb 直接 `root` 成功（无 TCP root 限制）**，rootandroid.sh
  降级为兜底路径（也实测成功）。
- 厂商 tool 路径（3.1/3.2）：`adb root-android` 或 connect 后 `adb -s localhost:5559 root`。

push 文件到 Android：root+remount 后经 `localhost:5559` push（推 `/system` 等分区才
需要 remount + 重启 HU；**本项目只推 `/data/local/tmp`，不需要**）。

网络调试路径（算力盒子）：Linux `172.31.2.52`、Android `172.31.2.51` 直连
（`adb connect 172.31.2.51:<port>`，⚠ 端口文档截图未给文字）。此路径无 MindRT
网关，Android 直接以 `172.31.2.51:*` serial 出现——xtools 天然可用（见 §7）。

**真机 `adb devices -l` 预期形态**（`platform/mod.rs` 现有单测 `test_detect_ss4`
已编码，⚠ 待真机核对）：

```
56df7065b0f          device usb:1-1.3 transport_id:1                    ← MindRT：无 product/model 字段
localhost:5559       device product:HU_SS4 model:HU_Smart_space_4_0 device:HU_SS4 transport_id:2   ← Android（桥接后）
```

注意 `172.31.101.51`（MindRT→Android 内网）与 `172.31.2.51`（算力盒子网络路径）
是两个不同地址，勿混。

---

## 2. 现状差距分析

现有架构的隐含假设：**目标 Android 设备直接出现在 `adb devices -l` 里**。
SS4 打破它的点及影响面：

| 现有机制 | SS4 下的表现 | 结论 |
|---|---|---|
| `list_adb_devices()` / GUI 设备枚举 | 只见 MindRT（无 product 字段，getprop 失败标 `?`）；Android 不存在 | **需桥接**（本文核心） |
| `pick_device`（CLI 无 `--device`） | 唯一设备是 MindRT → 自动选中它 → 后续全错（Linux 上没有 dumpsys/getprop/am） | 需过滤网关 + 桥接后自动选中 `localhost:5559` |
| `detect_platform` | MindRT 行无 product 跳过；桥接后 `localhost:5559` 行 `product:HU_SS4` → Ss4 ✅（单测已锁） | 零改动 |
| `try_adb_root` / `acquire_root` | `adb -s localhost:5559 root` ⚠ 疑似被拒（TCP root 限制）；MindRT 侧 `rootandroid.sh` 未接 | 需平台分支（§6） |
| `deploy_agent` / `ensure_daemon` | push /data/local/tmp、`forward tcp:0 localabstract:xperf-agent`、`setsid nohup` 全是标准 adb 语义——⚠ 唯一悬念：**localabstract forward 能否穿过 MindRT 5557 中继**（§8 风险 R1） | 大概率零改动，须实测 |
| `reconnect_agent` / `device_online` | Android GVM 重启 → `localhost:5559` 从 `adb devices` 消失；**无人重新 `adb connect`** → 永远等不回 | 需自愈钩子（§5.3） |
| GUI 设备 tab / 热插拔监视器 | MindRT 会被当一台「设备」建 tab（不可采样）；GVM 重启 tab 永久灰显 | 需网关标记 + 隐藏（§4.4） |
| trace / simpleperf / coldstart / 基线 | 全部经 `run_adb_command_for(serial)` → `-s localhost:5559` 透明 | 零改动 |
| SSH 远程后端（`--remote hppc`） | forward 监听与 connect 拨号都发生在 **adb server（hppc）侧**——桥接命令全经既有 hop#1 通道，与 DESIGN-ssh-remote §1 语义表完全一致 | 零改动（§7） |

---

## 3. 方案选型

**采用 方案 A：host 侧自动桥接（bridge 模块）**——桥接后 Android 以
`localhost:5559` **伪设备**进入 `adb devices`，下游一切按「普通 adb 设备」运转，
语义零变化；桥接本身收敛在新模块 + 少量集成点。

否决的替代方案：

- **B. 用户手动连接**（零代码）：按文档手工 forward+connect 后 xtools 才能工作。
  否决理由：UX 差（每次 HU 上电都要人肉）、易错（端口/顺序）、GUI 无法自动发现、
  与「SS4 接 hppc 远程调试」的工作流不匹配。
- **C. MindRT 内中转执行**：所有命令改写为 `adb -s <mindrt> shell adb -s 172.31.101.51:5555 …`
  两跳。否决理由：改动面遍布全部 adb 调用点；`localabstract` forward 无法表达；
  每命令双倍 adb 开销；push/pull 要先落 MindRT 再转推。方案 A 的「伪设备」天然绕开全部问题。
- **D. 依赖厂商魔改 adb tool**：win/linux 才有现成包，mac 要自行编译打 patch
  （gerrit/1259680），不可分发、不可控。方法 2（标准 adb）与之等价产物相同，无收益。

方案 A 的关键成立条件（全部可真机验证）：标准 adb 的 `forward tcp:5559 tcp:5557`
+ `connect localhost:5559` 在未装魔改 tool 的主机上可用（文档方法 2 明示可行）。

---

## 4. 模块设计

### 4.1 新模块 `xperf-core/src/bridge.rs`

职责单一：**把 SS4 拓扑里的 Android 桥接进 `adb devices`，并维护其存活**。
不含平台数据源逻辑（那是 `platform/ss4.rs` + agent 的事）。

```
pub struct BridgeState {                    // Mutex 包全局静态（线程安全：监视器/GUI/CLI 多线程调用）
    gateways: HashMap<String, GatewayInfo>, // mindrt_serial → { android_serial, port, probed_at }
    failed: HashMap<String, Instant>,       // 探测失败的 serial → 冷却截止（防 3s 轮询重试风暴）
}

pub fn refresh(devices: &mut Vec<AdbDevice>) -> bool
    // 幂等收敛入口（list_adb_devices 尾部调用，返回「是否新建了连接」供调用方重枚举）：
    // 1. 解析 `adb forward --list`，找 target 为 tcp:5557 的规则 → 这些 serial 是网关
    //    （无论规则是谁建的——我们自己建的、用户按文档手建的、厂商 tool 的 shell-android 建的，
    //     一律识别复用；这同时是「用户手动桥接过」场景的天然兼容路径）
    // 2. 对列表里 product 为空的 USB 设备且无规则者 → bootstrap（见下）；
    //    失败者记入 failed，冷却 60s 再试（只防「对无关设备反复探测」）
    // 3. 对已有规则的网关：若其 android serial（localhost:<port>）不在 devices 里
    //    → `adb connect localhost:<port>`（GVM 重启/连接掉线自愈；**每次 refresh 都试，
    //    不受 60s 冷却**——单次 connect 对本地拒绝是廉价快速失败，换取 GVM 恢复后
    //    一个轮询周期内接回）
    // 4. 给 devices 里识别出的网关打 is_gateway 标记

fn bootstrap_gateway(mindrt: &str) -> Option<(port, android_serial)>
    // XPERF_NO_AUTO_ROOT 未设时 best-effort `adb -s <mindrt> root`（文档方法 2 顺序；
    //   失败不阻断——⚠ 是否必要待 R4 验证，先对齐文档）
    // `adb -s <mindrt> wait-for-device`（短超时）
    // 分配 host 端口（默认 5559；`adb forward --list` 里 5559 已被**别的 serial** 占用时
    //   顺延 5560/5561…，支持多台 SS4；同 serial 已有规则则复用原端口——serial 稳定性优先）
    // `adb -s <mindrt> forward tcp:<port> tcp:5557` → `adb connect localhost:<port>`
    // connect 失败 → `adb forward --remove`（不留死规则）→ None

pub fn reconnect(android_serial: &str) -> bool
    // device_online 自愈用：serial 形如 localhost:<port> 时，查网关 → 重 forward + connect
```

端口策略：默认 **5559**（对齐厂商 tool 与文档，用户手动桥接场景直接复用既有规则）；
`XPERF_SS4_BRIDGE_PORT` 环境变量覆盖首选端口。5559 属 adb 模拟器端口段
（5555+2N），主机跑多模拟器时可能冲突 → 冲突时顺延分配并 stderr 提示。

网关识别的判据（按优先级）：
1. `adb forward --list` 存在 `<serial> tcp:<port> tcp:5557` 规则 → 确定是网关；
2. USB transport 且 `adb devices -l` 行**无 product 字段** → 候选，bootstrap 探测
   （forward+connect 成功 = 网关；失败 = 普通设备/中继不可达，冷却重试）。
   ⚠ R2：product 缺失是否为 MindRT 稳定特征，待真机核对（现有单测形态如此）。

**serial 稳定性不变量**：`adb connect` 的目标串就是伪设备 serial——桥接恒用字面
`localhost:<port>`（**不用** `127.0.0.1:<port>`，否则 serial 变成
`127.0.0.1:5559`，GUI tab 键/`<ts>-<serial>` 目录名跨会话漂移）。重枚举得到的
设备列表须再过一次 refresh（幂等——只补 is_gateway 标记，不会重复 connect）。

### 4.2 `AdbDevice` 扩展（`utils.rs`）

```rust
pub struct AdbDevice {
    ...,
    /// SS4 MindRT 网关（Linux 主控，非 Android 采样目标；桥接宿主）。
    /// true 时 CLI pick_device 跳过、GUI 前端隐藏，采样命令拒绝。
    pub is_gateway: bool,
}
```

`parse_adb_devices` 保持纯解析（默认 false），标记由 `bridge::refresh` 落。

### 4.3 集成点（全链路唯一四处）

1. **`list_adb_devices()`**（utils.rs）：解析后调 `bridge::refresh(&mut devices)`；
   返回 true（新建了连接）则**重枚举一次**（有界循环 ≤2 轮）把 `localhost:<port>`
   纳入返回值——CLI 启动、GUI `list_devices`、监视器（3s 轮询）、`ensure_device_online`
   全部自动获得桥接能力，无需各自记得调用。
   稳态开销：每轮 1 次 `adb forward --list` + 网关 getprop（快速失败 ~100ms）。
2. **`device_online()`**（agent.rs，重连轮询）：get-state 失败且 serial 形如
   `localhost:<port>` → `bridge::reconnect(serial)` 后再探活。CLI 单进程无人值守
   采样下 GVM 重启的自愈路径（GUI 侧监视器 3s 轮询也会经 1 兜到，双保险）。
3. **`try_adb_root` / `acquire_root`**（agent.rs）：Ss4 平台分支（§6）。
4. **`pick_device`**（utils.rs）：过滤 `is_gateway` 设备——单台 SS4 连接时
   `adb devices` 有两台（MindRT + localhost:5559），过滤后自动选中 Android；
   多台报错清单也只列 Android，避免误导。

### 4.4 GUI 呈现

- 网关过滤（**三处 payload + diff 统一收敛**）：`list_devices` 与 `devices_json`
  （`xperf-gui/src/main.rs`，后者供 connect_remote 用）过滤 `is_gateway`；
  `spawn_device_monitor` 在 **diff 之前**过滤（网关插拔不产生 devices-changed
  噪声、不进 added/removed，前端不会为 MindRT 建 tab——注意监视器的 payload
  是内联构造的第三处，不是复用 devices_json）。建议三处收敛到同一 helper。
- 网关在而 `localhost:5559` 不在（中继不可达/HU 半启动）：前端无 tab + 状态栏
  不额外提示（bring-up 阶段从简，排查靠文档 §1 的 ping/adbconnect.sh 流程）；
  若真机验证发现此态常见，再补「SS4 网关在线但 Android 未就绪」横幅。
- `ensure_device_online` 拒绝网关 serial（用户手输/边缘态兜底）：错误信息指引
  「MindRT 是 SS4 网关，请选择 localhost:5559（Android）」。

### 4.5 状态与生命周期

- `BridgeState` 进程内静态；**跨会话持久化靠 adb server 侧事实**
  （forward 规则 + connect 存活状态），重启 xtools/GUI 后 refresh 从
  `forward --list` 恢复映射——与 SSH 远程设计 R10「规则由 server 持有」同源。
- 不做退出清理（disconnect/forward --remove）：连接与规则常驻 adb server
  无害且可复用；`shutdown_remote` 亦不触碰（只管自己注册的 hop#2 映射）。

---

## 5. 关键流程（时序）

### 5.1 首次接入（HU 上电 → 可采样）

```
USB 插入 MindRT
  └─ GUI 监视器 3s 轮询 list_adb_devices
       └─ refresh：发现无 product 的 USB 设备 → bootstrap_gateway
            ├─ adb -s <mindrt> root（best-effort，NO_AUTO_ROOT 门控）
            ├─ adb -s <mindrt> wait-for-device（~1-2s，adbd 重启）
            ├─ adb -s <mindrt> forward tcp:5559 tcp:5557
            └─ adb connect localhost:5559        → devices-changed：新增 localhost:5559
                 └─ 前端建 tab（model: HU_Smart_space_4_0）；MindRT 被过滤不建 tab
用户开始监控
  └─ spawn_agent(localhost:5559)：ensure_daemon → deploy（try_adb_root Ss4 分支）→
     forward tcp:0 localabstract:xperf-agent（⚠ R1）→ 连流 → 采样
```

首次枚举多花 ~2-4s（MindRT adbd 重启 + connect），仅网关初次接入时发生。

### 5.2 HU 整机重启

MindRT USB 消失 → `localhost:5559` 消失 → GUI tab 灰显（数据保留，既有语义）。
MindRT 回来 → 监视器 diff 视作新设备 → bootstrap 重跑 → 同 port 5559 →
**serial 不变** → 前端插回恢复采样（`reconnect_agent` 既有路径）。
（MindRT 的 forward 规则随 transport 消亡还是残留不影响结局：refresh 按规则
存在性复用/重建，两种情况都收敛到同端口——S0 顺手实测确认即可。）

### 5.3 Android GVM 重启（MindRT 不动）

5557 中继断 → `localhost:5559` 掉线消失 → tab 灰显。
自愈双路径：① 监视器下一轮 `refresh` 步骤 3（网关在而 android 缺 → connect）；
② CLI 场景 `device_online` 探活失败 → `bridge::reconnect`。
GVM adbd 起来后 connect 成功 → serial 不变 → 采样恢复。
自愈节奏（与 §4.1 一致）：已知网关的 connect 重试**每次 refresh（3s）都发**——
单次失败是本地快速拒绝，代价可忽略，换取 GVM 起来后一个轮询周期内接回；
60s 冷却只用于**未知设备的 bootstrap 探测**（防对无关设备反复探测），两类
重试语义不同，勿混用。

### 5.4 多设备（SS4 + SS3 同连）

`-s` 路由各走各的（既有能力）；SS4 侧每网关独立 port（5559/5560…），
`localhost:5559` 与 SS3 的 USB serial 同列 `adb devices`，`pick_device`/
`detect_platform_live(serial)` 按行过滤互不干扰。已知边界：两台 SS4 的
port 分配按接入顺序，全部重插可能互换 serial（tab 数据错位）——多台 SS4
同连非 bring-up 目标，记录不解决。

SS2/SS3/手机与 SS4 混连（含本次 review 补充确认）：全部走 `-s` 路由既有机制，
网关探测只命中「无 product 的 USB 设备」（MindRT），其余设备零额外 adb 操作；
行为与已真机验证的「SS3+SS2MAX 双机并行」同构。S6 回归把「SS3+SS2MAX+SS4
三机同连采样不互扰」列为验证项。

---

## 6. root 链路设计

SS4 Android root 路径（**S0 已实测定稿**）：

```
try_adb_root(serial=localhost:5559) / acquire_root(serial) 的 Ss4 分支：
  ① 直接 adb -s localhost:5559 root → wait → shell id
     （✅ S0 实测成功——本机 adbd 无 TCP root 限制，① 即主路径）
  ② 失败 → 经网关：adb -s <mindrt> shell rootandroid.sh（/system_ext/bin/，PATH 内）
     → adb connect localhost:5559（adbd 提权重启会掉连接，重连）
     → 轮询 adb -s localhost:5559 shell id（复用 acquire_root 15s 轮询骨架）
     （✅ S0 实测成功，作为 ① 的兜底保留）
  验证口径不变：uid=0；XPERF_NO_AUTO_ROOT 门控 ①②。
```

- `should_auto_root(Ss4)=true` 已有单测锁定，不改。
- acquire_root（GUI「获取 root」按钮）报错文案区分 ①②（脚本不存在/中继不通各自
  透传原文）。
- MindRT 自身的 root：bootstrap 内 best-effort（§4.1），不进 acquire_root。
- **网络直连路径（算力盒子 `172.31.2.51`）无网关**：① 失败即止，非 root 降级
  （G 节矩阵），如实提示。

remount 本项目不需要（只写 /data/local/tmp）。

---

## 7. 边界：不受影响的部分

- **SSH 远程后端零改动**：桥接用的 `forward`/`connect`/`devices` 全部是
  adb server 侧语义（DESIGN-ssh-remote §1 表）；监听与拨号都发生在 hppc 的
  server 上，`-s localhost:5559` 经 hop#1 路由。agent NDJSON 流的
  `forward tcp:0 localabstract:...` 照旧走 hop#2 映射（`ensure_forward` 的
  规则复用按 serial 过滤，`localhost:5559` 作 serial 成立）。
- **trace / simpleperf / coldstart / 基线 / 阈值**：全经 `run_adb_command_for`，
  serial 透传，零改动。
- **agent（xperf-agent）零改动**：daemon/localabstract/NDJSON 协议与宿主怎么
  连进来无关。（若 R1 触发 fallback 才动 agent，见 §8。）
- **平台检测**：`test_detect_ss4` 已锁 `localhost:5559 + product:HU_SS4 → Ss4`；
  MindRT 行无 product 不参与匹配。`platform/ss4.rs` 桩的补实属 H 节第 6 项
  （GPU/数据源细节），与本设计正交。
- **用户手动桥接兼容**：用户已按文档（或厂商 tool）forward+connect 过 →
  refresh 从 forward --list 识别复用，零冲突。

---

## 8. 风险与真机预验证清单

**预验证先行**（S0，hppc 上纯 adb 手工跑，不动代码——假设不成立则回改设计）：

| # | 风险/假设 | 验证方法 | 不成立时的 fallback |
|---|---|---|---|
| R1 | **`forward tcp:0 localabstract:xperf-agent` 穿不过 5557 中继**（最高风险：中继可能只转 adb 协议特定通道） | 手工 forward+connect 后 `adb -s localhost:5559 forward tcp:15559 localabstract:xperf-agent`，另一终端 `nc 127.0.0.1 15559` 看 agent hello（先手工推 agent 起 daemon） | **F1**：agent 增加 `--tcp-port` 监听模式（bind 127.0.0.1:PORT，GVM 内无暴露面），host 侧 `forward tcp:0 tcp:PORT`。改动：agent 一个监听开关 + ensure_forward 目标串分支 |
| R2 | MindRT 的 product 字段缺失不是稳定特征（识别失效→不 bootstrap） | `adb devices -l` 看实际输出；多次重启 HU 复核 | 识别降级为「仅 forward --list 推断 + 端口探测」；或按 model/usb 路径匹配（真机拿到实际字段后定） |
| R3 | `adb -s localhost:5559 root` 行为（标准 adb 下成功/被拒/掉线） | 手工 connect 后执行，观察输出与 `shell id` | §6 已按 ② 设计成主路径，① 只是快路径——任一成立即通 |
| R4 | MindRT 不 root 则 5557 中继不可用 / forward 不可建 | 不 root MindRT 直接 forward+connect 试 | bootstrap 把 MindRT root 从 best-effort 升为必要步骤（文档方法 2 本就含 root） |
| R5 | GVM 重启后 `adb connect` 自愈节奏（中继何时恢复监听） | 重启 GVM，观察 5557 恢复时长与 connect 失败形态 | §5.3 节奏（每 3s 重试）真机不合适再引入冷却参数 |
| R6 | 5559 与模拟器端口冲突 | （低概率，Mac 跑模拟器 + SS4 同场景） | 端口顺延已内置（§4.1）；日志提示 |
| R7 | `localhost:5559` 上 `wait-for-device`/`getprop` 等 hidden 语义有厂商差异 | 预验证顺手跑 `getprop ro.build.version.release`、`shell echo`、`push/pull` 小文件 | 逐项适配（预期差异小：终归是 adbd 协议） |
| R8 | ligfxprofilerd 日志在 **GVM 的 logcat 不可见**（agent `gpu/ligfx.rs` 假设它在 GVM logcat 里；若它只在 MindRT/PVM 侧）——**含 ligfx Frequency 单位（Hz/MHz）核实，H 节遗留** | `adb -s localhost:5559 shell logcat -d -s ligfxprofilerd` vs `adb -s <mindrt> shell logcat -d -s ligfxprofilerd` | **本桥接反而提供新路径**：GPU 通道改为 host 侧 `adb -s <mindrt> shell logcat -s ligfxprofilerd` 流式读取后喂给采样会话（改动在 host，agent 的 ligfx 通道退役或保留为探测）。属 H 节 GPU 项，设计不展开 |

R1-R5 为 S0 预验证必做项；R7 顺手；R8 与 H 节第 3 项合并验证。

### S0 实测结论（2026-09-10，hppc：MindRT `42087266b1f` usb:1-12.3 + SS3 + SS2MAX 三机同连）

| # | 结论 | 实测证据 |
|---|---|---|
| R1 | ✅ **成立，F1 不需要** | 手工 forward+connect 后推 agent 起 daemon，`forward tcp:15559 localabstract:xperf-agent` + `nc 127.0.0.1 15559` 收到 hello（`ncores:12, version:2`）——localabstract 穿 5557 中继无阻碍 |
| R2 | ✅ **成立** | 真机 `adb devices -l` 与设计预期逐字一致：MindRT 行无 product 字段；桥接后 `localhost:5559 device product:HU_SS4 model:HU_Smart_space_4_0 device:HU_SS4`（`test_detect_ss4` 单测形态即真机形态） |
| R3 | ✅ **比假设乐观** | 标准 adb `adb -s localhost:5559 root` **直接成功**（uid=0，无 TCP root 限制）→ §6 ① 为主路径；② MindRT `rootandroid.sh`（`/system_ext/bin/`，PATH 内）也实测成功，留作兜底 |
| R4 | ✅ **比假设乐观** | MindRT adbd 为 uid=2000(adb)（未 root）时 forward+connect 即成功——bootstrap 的 MindRT root 保持 best-effort（对齐文档顺序），**非必要步骤** |
| R5 | ✅ **成立，零干预自愈** | `adb -s localhost:5559 reboot`（只重启 GVM）：serial **不从 `adb devices` 消失**（offline 态）→ **~24s 自动回 device**；forward 规则 / connect 对象 / 5557 中继全程存活，期间 `adb connect` 返回 "already connected"（无害 no-op）。另实测：**MindRT adbd 重启（`adb root` 提权）会清掉其 forward 规则**（relay 进程存活）→ refresh 的「按规则存在性重建」路径必需，§5.2 软化断言成立（消亡派） |
| R7 | ✅ **成立** | `getprop ro.build.version.release` = **Android 16**（FPS 图层名按 A12+ BLAST 形态）；shell/push/install/`am start -W`（gltf viewer COLD 310ms）全正常 |
| R8 | ⚠ **推翻，走 fallback** | ligfxprofilerd 跑在 **MindRT**（`/usr/bin/ligfxprofilerd` pid 8770），**GVM logcat 无任何 ligfx 输出** → agent `gpu/ligfx.rs`（读 GVM logcat）不成立，GPU 通道改 host 侧 `adb -s <mindrt> shell logcat -s ligfxprofilerd` 流式读取（R8 fallback，H 节 GPU 项实施）。实测数据形态：每 **~5s** 一个帧块；Sys 行 `Frequency: 1000 Hz, Tasks: N, GSL Timestamp, Global: Busy/Queued/Utilization`；Proc 行 `GVM_<comm 15字符截断>-<会话id>`（id **非 GVM pid 且跨重启变化**——reboot 后 surfaceflinger 7320→1573333，**归因只能按 comm**，与 agent `lookup_pid` 截断匹配语义一致）；负载对照：gltf viewer 渲染时 Global Busy 11%→29% 且出现 `GVM_d.filament.gltf` 行。**Frequency 恒 1000**（空闲/负载不变；GPU 经 VFIO 直通 GVM——MindRT 仅见 `vfio_kgsl*` 平台设备、GVM 无 `/sys/class/kgsl`，两侧均无频率节点可对照）——单位（Hz 标注疑似 MHz）与是否定频占位待 H 节 GPU 项结合厂商资料定论 |

**设备环境备忘**（S0 后状态）：MindRT 已被 `adb root`（提权后 adbd uid=0）；GVM 经 rootandroid.sh 提为 root；gltf viewer 已装 SS4（包名 `com.google.android.filament.gltf`）；测试 agent daemon 已随 GVM 重启清除。

---

## 9. 实施计划

分支 `feature/ss4-adb-bridge`，一步一 commit（对齐 ssh-remote 的实施风格）：

- **S0 预验证（无代码）**（✅ 2026-09-10 完成，结论见 §8「S0 实测结论」表：
  R1-R5/R7 成立、R8 推翻走 fallback；主设计零改动）。
- **S1 `bridge.rs` 核心**：BridgeState + refresh/bootstrap/reconnect + forward
  --list 解析/端口分配/网关判定（纯函数拆出可单测：`parse_forward_list`、
  `allocate_port`、`classify_gateway`）。
- **S2 utils 集成**：`AdbDevice.is_gateway` + `list_adb_devices` 尾部 hook（含重枚举
  有界循环）+ `pick_device` 过滤 + 相关单测更新。
- **S3 root 链路**：`try_adb_root`/`acquire_root` 的 Ss4 分支（§6），XPERF_NO_AUTO_ROOT
  门控 ①②。
- **S4 自愈**：`device_online` 的 localhost:* 分支 → `bridge::reconnect`。
- **S5 GUI**：三处 payload + 监视器 diff 前统一过滤网关（§4.4）+ `ensure_device_online`
  网关拒绝文案；**前端零改动（已核对 `main.js`）**——serial 只进 `dataset.serial` 与
  `pkgList-<serial>` 元素 id（`.id=`/`setAttribute` 赋值，无动态 id 的 querySelector，
  全部选择器为类名/固定 id），`localhost:5559` 的冒号在 HTML5 id/data 值中合法，安全。
- **S6 真机 bring-up 回归**：对齐 WORKSPACE H 1-2（环境/平台检测核对）+ 本设计
  §5.1-5.3 时序逐条跑通 + §5.4 多机项（SS3+SS2MAX+SS4 三机同连采样不互扰、
  SSH 远程 `--remote hppc` 全链路含 GVM 重启自愈）；文档收尾（CLAUDE.md 多设备/
  SS4 段补桥接说明，对齐 H 6）；九项指标与 C 类能力回归按 H 3-4 独立成会话
  （不塞进本设计）。

每步交付前跑 `cargo test` + `cargo doc` 零告警（workspace 规则）。

---

## 10. 测试设计

**单测（主机可跑）**：
- `parse_forward_list`：规则行解析、多网关、5557 与 localabstract 规则共存不误判
  （现 `ensure_forward` 的规则按 target 区分，桥接规则 target=tcp:5557 互不干扰）。
- `allocate_port`：默认 5559 / 占用顺延 / 同 serial 复用既有端口。
- `classify_gateway`：product 缺失 USB 行、带 product 行、已有规则行、localhost:* 行。
- `refresh` 状态机（注入时钟）：bootstrap 失败 60s 冷却 vs 已知网关每轮重试两类
  语义分离；connect 目标串恒 `localhost:<port>`（serial 稳定性不变量，§4.1）。
- `pick_device` 网关过滤：单 SS4（MindRT+localhost:5559）自动选 Android；全网关报错。
- `AdbDevice` 序列化兼容（is_gateway 新字段不破坏 GUI payload 契约——payload 由
  devices_json 显式挑字段，天然隔离）。

**集成（真机，S0/S6）**：§8 清单 + §5 三条时序（首接/整机重启/GVM 重启自动恢复）+
SSH 远程全链路（`--remote hppc` 采样 + GVM 重启自愈）。

**不可自动化项**（记录）：物理插拔/重启节奏、GUI tab 灰显-恢复目验（沿用既有
「后端命令级测试锁定 + 真机日志目验」惯例）。
