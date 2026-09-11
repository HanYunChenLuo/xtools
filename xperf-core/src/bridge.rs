//! SS4（SA8797P）MindRT 网关自动桥接（设计 `docs/DESIGN-ss4-adb.md`）。
//!
//! SS4 是「MindRT（Linux PVM，USB 可见）+ Android（GVM，USB 不可见）」双系统：
//! `adb devices` 只能看到 MindRT，Android 须经 `forward tcp:<port> tcp:5557`
//! （MindRT 上的 adb 中继，S0 实测常驻）+ `adb connect localhost:<port>` 桥接成
//! 伪设备。本模块职责单一：**把 SS4 拓扑里的 Android 桥接进 `adb devices` 并维护
//! 其存活**——桥接后 Android 以 `localhost:<port>` 进入现有全链路，下游语义零变化。
//!
//! 集成点（全链路唯一四处，详见设计 §4.3）：[`crate::utils::list_adb_devices`]
//! 尾部 hook（本模块 [`crate::bridge::refresh()`]）、`device_online` 自愈
//! （[`crate::bridge::reconnect()`]）、root 链路 Ss4 分支、`pick_device` 网关过滤。
//!
//! S0 实测要点（设计 §8）：桥接免 root（MindRT adbd uid=2000 即可 forward+connect）；
//! GVM 重启 serial 不消失（offline ~24s 自动回 device）；**MindRT adbd 重启
//! （`adb root` 提权）会清掉其 forward 规则**——refresh/reconnect 必须按规则
//! 存在性重建。

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::utils::{run_adb, run_adb_command_for, AdbDevice};

/// MindRT 侧 adb 中继的监听目标（forward 规则的 remote 端；S0 实测为 MindRT 上
/// 一个 adb 进程 LISTEN `127.0.0.1:5557`，跨 GVM/MindRT adbd 重启存活）
const RELAY_TARGET: &str = "tcp:5557";

/// 桥接本机默认首选端口（对齐厂商 tool 与调试指南；被占用时顺延 5560/5561…）
const DEFAULT_PORT: u16 = 5559;

/// 未知设备 bootstrap 探测失败后的冷却时长（防 3s 轮询对无关设备反复探测；
/// 与「已知网关每轮 refresh 都重试 connect」是两类语义，勿混用）
const BOOTSTRAP_COOLDOWN: Duration = Duration::from_secs(60);

/// `adb forward --list` 输出中的一条规则：`<serial> <local> <remote>`
/// （如 `42087266b1f tcp:5559 tcp:5557`、`6eb792dfb0f tcp:38733 localabstract:xperf-agent`）
#[derive(Debug, Clone, PartialEq)]
pub struct ForwardRule {
    /// 规则所属设备 serial
    pub serial: String,
    /// host 侧端（如 `tcp:5559`）
    pub local: String,
    /// 设备侧端（如 `tcp:5557` / `localabstract:...`）
    pub remote: String,
}

/// 解析 `adb forward --list` 输出为规则列表（无规则时输出为空串，返回空 vec）
pub fn parse_forward_list(output: &str) -> Vec<ForwardRule> {
    output
        .lines()
        .filter_map(|line| {
            let mut f = line.split_whitespace();
            match (f.next(), f.next(), f.next()) {
                (Some(serial), Some(local), Some(remote)) => Some(ForwardRule {
                    serial: serial.to_string(),
                    local: local.to_string(),
                    remote: remote.to_string(),
                }),
                _ => None,
            }
        })
        .collect()
}

/// 规则 local 端的 TCP 端口号（`tcp:5559` → `Some(5559)`；`localabstract:...` 等
/// 非 TCP 端为 `None`）
fn local_tcp_port(local: &str) -> Option<u16> {
    local.strip_prefix("tcp:")?.parse().ok()
}

/// 伪设备 serial 不变量：桥接/connect 目标恒为字面 `localhost:<port>`
/// （**不用** `127.0.0.1:<port>`——`adb connect` 的目标串即伪设备 serial，
/// 换写法会导致 GUI tab 键与 `<ts>-<serial>` 目录名跨会话漂移）
pub fn android_serial(port: u16) -> String {
    format!("localhost:{port}")
}

/// 设备在桥接拓扑中的分类
#[derive(Debug, Clone, PartialEq)]
pub enum DeviceClass {
    /// 确定是 SS4 网关（存在 target 为 `tcp:5557` 的 forward 规则），带规则端口
    Gateway {
        /// 该网关桥接规则的本机端口
        port: u16,
    },
    /// 候选网关（无 product 字段的直连设备——S0 实测 MindRT 形态），待 bootstrap 探测
    Candidate,
    /// 普通设备（带 product 的 Android、或已是 TCP 伪设备），不参与桥接
    Regular,
}

/// 设备分类判据（按优先级）：① 存在 target `tcp:5557` 的规则 → 确定网关
/// （无论规则是谁建的——本工具/用户手工/厂商 tool，一律识别复用）；
/// ② serial 不含 `:`（排除 localhost:*/IP:* 等 TCP 伪设备）且 product 为空 → 候选；
/// ③ 其余 → 普通设备。
pub fn classify_device(device: &AdbDevice, rules: &[ForwardRule]) -> DeviceClass {
    if let Some(rule) = rules
        .iter()
        .find(|r| r.serial == device.serial && r.remote == RELAY_TARGET)
    {
        if let Some(port) = local_tcp_port(&rule.local) {
            return DeviceClass::Gateway { port };
        }
    }
    if !device.serial.contains(':') && device.product.is_empty() {
        return DeviceClass::Candidate;
    }
    DeviceClass::Regular
}

/// 分配桥接本机端口：同一网关 serial 的既有 5557 规则优先复用原端口
/// （serial 稳定性不变量：重启/重插后 `localhost:<port>` 不变，GUI tab 数据延续）；
/// 否则从 `preferred` 起顺延找未被任何规则占用的端口（多 SS4 / 模拟器端口冲突）。
pub fn allocate_port(rules: &[ForwardRule], gateway_serial: &str, preferred: u16) -> u16 {
    if let Some(rule) = rules
        .iter()
        .find(|r| r.serial == gateway_serial && r.remote == RELAY_TARGET)
    {
        if let Some(p) = local_tcp_port(&rule.local) {
            return p;
        }
    }
    let used: HashSet<u16> = rules.iter().filter_map(|r| local_tcp_port(&r.local)).collect();
    (preferred..=u16::MAX)
        .find(|p| !used.contains(p))
        .unwrap_or(preferred)
}

/// bootstrap 探测失败冷却查询（纯函数，状态机测试可注入时钟）
fn cooldown_active(failed: &HashMap<String, Instant>, serial: &str, now: Instant) -> bool {
    failed
        .get(serial)
        .is_some_and(|t| now.duration_since(*t) < BOOTSTRAP_COOLDOWN)
}

/// 一台已识别网关的桥接信息
#[derive(Debug, Clone, PartialEq)]
pub struct GatewayInfo {
    /// 桥接出的 Android 伪设备 serial（`localhost:<port>`）
    pub android_serial: String,
    /// 桥接本机端口
    pub port: u16,
}

/// 桥接进程内状态（跨会话持久化靠 adb server 侧事实：forward 规则 + connect
/// 存活状态——进程重启后 refresh 从 `forward --list` 恢复映射，见设计 §4.5）
struct BridgeState {
    /// mindrt_serial → 桥接信息
    gateways: HashMap<String, GatewayInfo>,
    /// bootstrap 探测失败的 serial → 失败时刻（冷却截止判定）
    failed: HashMap<String, Instant>,
}

static STATE: Mutex<Option<BridgeState>> = Mutex::new(None);

fn with_state<R>(f: impl FnOnce(&mut BridgeState) -> R) -> R {
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let state = guard.get_or_insert_with(|| BridgeState {
        gateways: HashMap::new(),
        failed: HashMap::new(),
    });
    f(state)
}

/// 查询 `adb forward --list` 的当前规则（失败按无规则处理——下一轮 refresh 重试）
fn forward_rules() -> Vec<ForwardRule> {
    run_adb(&["forward", "--list"])
        .map(|o| parse_forward_list(&o.stdout))
        .unwrap_or_default()
}

/// `adb connect localhost:<port>`；输出含 "connected" 即成功
/// （"connected to" 与 "already connected to" 均算；"Connection refused" 不含）。
fn adb_connect(target: &str) -> bool {
    run_adb(&["connect", target])
        .map(|o| o.stdout.contains("connected"))
        .unwrap_or(false)
}

/// 首选桥接端口：`XPERF_SS4_BRIDGE_PORT` 环境变量覆盖，默认 [`DEFAULT_PORT`]
fn preferred_port() -> u16 {
    std::env::var("XPERF_SS4_BRIDGE_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_PORT)
}

/// 幂等收敛入口（[`crate::utils::list_adb_devices`] 尾部调用）。
///
/// 流程（设计 §4.1）：① 从 `forward --list` 恢复全部已知网关映射；② 已知网关的
/// Android 不在设备列表里 → `adb connect` 自愈（**每轮都试**，不受冷却限制——
/// 单次 connect 对本地拒绝是廉价快速失败，换取 GVM 恢复后一个轮询周期内接回）；
/// ③ 无规则的候选设备（无 product 直连设备）→ bootstrap 探测（失败者冷却 60s）；
/// ④ 给设备列表里的网关打 `is_gateway` 标记。
///
/// 返回「是否新建/接回了连接」——true 时调用方应重枚举一次把 `localhost:<port>`
/// 纳入返回值。
pub fn refresh(devices: &mut [AdbDevice]) -> bool {
    let rules = forward_rules();
    let mut created = false;

    // ①② 已知网关：恢复映射 + android 缺失时 connect 自愈
    for rule in rules.iter().filter(|r| r.remote == RELAY_TARGET) {
        let Some(port) = local_tcp_port(&rule.local) else {
            continue;
        };
        let android = android_serial(port);
        with_state(|s| {
            s.gateways.insert(
                rule.serial.clone(),
                GatewayInfo { android_serial: android.clone(), port },
            );
            s.failed.remove(&rule.serial);
        });
        if !devices.iter().any(|d| d.serial == android) && adb_connect(&android) {
            created = true;
        }
    }

    // ③ 候选设备 bootstrap（冷却内的跳过）
    let candidates: Vec<String> = devices
        .iter()
        .filter(|d| matches!(classify_device(d, &rules), DeviceClass::Candidate))
        .map(|d| d.serial.clone())
        .collect();
    for serial in candidates {
        if with_state(|s| cooldown_active(&s.failed, &serial, Instant::now())) {
            continue;
        }
        // 每轮重读规则：多网关同轮 bootstrap 时端口分配须看到上一台新建的规则
        // （否则两台撞同端口，connect 会错接到第一台网关的 Android）
        let fresh_rules = forward_rules();
        match bootstrap_gateway(&serial, &fresh_rules) {
            Some(info) => {
                with_state(|s| {
                    s.gateways.insert(serial.clone(), info);
                    s.failed.remove(&serial);
                });
                created = true;
            }
            None => with_state(|s| {
                s.failed.insert(serial.clone(), Instant::now());
            }),
        }
    }

    // ④ 网关标记
    for d in devices.iter_mut() {
        d.is_gateway = with_state(|s| s.gateways.contains_key(&d.serial));
    }
    created
}

/// 对候选设备做 bootstrap 探测：best-effort root（S0/R4 实测非必要，对齐文档
/// 方法 2 顺序；`XPERF_NO_AUTO_ROOT` 门控）→ 建 forward 规则 → connect。
/// 成功返回桥接信息；失败清掉规则（不留死规则）返回 None。
fn bootstrap_gateway(mindrt: &str, rules: &[ForwardRule]) -> Option<GatewayInfo> {
    if std::env::var_os("XPERF_NO_AUTO_ROOT").is_none() {
        let _ = run_adb_command_for(Some(mindrt), &["root"]);
        let _ = run_adb_command_for(Some(mindrt), &["wait-for-device"]);
    }
    let port = allocate_port(rules, mindrt, preferred_port());
    let local = format!("tcp:{port}");
    run_adb_command_for(Some(mindrt), &["forward", &local, RELAY_TARGET]).ok()?;
    let android = android_serial(port);
    if adb_connect(&android) {
        Some(GatewayInfo { android_serial: android, port })
    } else {
        let _ = run_adb_command_for(Some(mindrt), &["forward", "--remove", &local]);
        None
    }
}

/// `device_online` 自愈用：Android 伪设备掉线时查网关 → 按需重建 forward 规则
/// （S0 实测 MindRT adbd 重启会清规则）→ 重新 connect。serial 非
/// `localhost:<port>` 形态或找不到对应网关时返回 false。
pub fn reconnect(android_serial: &str) -> bool {
    let Some(port) = android_serial
        .strip_prefix("localhost:")
        .and_then(|p| p.parse::<u16>().ok())
    else {
        return false;
    };

    // 查网关：先进程内状态；状态丢失（进程重启）时从 forward --list 恢复
    let gateway = with_state(|s| {
        s.gateways
            .iter()
            .find(|(_, info)| info.android_serial == android_serial)
            .map(|(gw, _)| gw.clone())
    })
    .or_else(|| {
        let rule = forward_rules()
            .into_iter()
            .find(|r| r.remote == RELAY_TARGET && local_tcp_port(&r.local) == Some(port))?;
        with_state(|s| {
            s.gateways.insert(
                rule.serial.clone(),
                GatewayInfo { android_serial: self::android_serial(port), port },
            );
        });
        Some(rule.serial)
    });
    let Some(gateway) = gateway else {
        return false;
    };

    // 规则缺失则重建（MindRT adbd 重启清规则场景）
    let rules = forward_rules();
    if !rules
        .iter()
        .any(|r| r.serial == gateway && r.remote == RELAY_TARGET)
    {
        let local = format!("tcp:{port}");
        if run_adb_command_for(Some(&gateway), &["forward", &local, RELAY_TARGET]).is_err() {
            return false;
        }
    }
    adb_connect(android_serial)
}

/// serial 是否为已识别的 SS4 网关（MindRT）。`pick_device` 过滤与
/// `ensure_device_online` 拒绝对网关采样用。
pub fn is_gateway_serial(serial: &str) -> bool {
    with_state(|s| s.gateways.contains_key(serial))
}

/// 网关 serial → 其桥接出的 Android 伪设备 serial（root 链路经网关执行
/// `rootandroid.sh` 兜底时用）
pub fn gateway_android_serial(gateway: &str) -> Option<String> {
    with_state(|s| s.gateways.get(gateway).map(|i| i.android_serial.clone()))
}

/// Android 伪设备 serial → 其网关（MindRT）serial（Ss4 root 兜底经网关执行
/// `rootandroid.sh` 用；桥接未收敛/非桥接设备返回 None）
pub fn gateway_for_android(android_serial: &str) -> Option<String> {
    with_state(|s| {
        s.gateways
            .iter()
            .find(|(_, info)| info.android_serial == android_serial)
            .map(|(gw, _)| gw.clone())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gw_rule(serial: &str, port: u16) -> ForwardRule {
        ForwardRule {
            serial: serial.to_string(),
            local: format!("tcp:{port}"),
            remote: RELAY_TARGET.to_string(),
        }
    }

    fn agent_rule(serial: &str, port: u16) -> ForwardRule {
        ForwardRule {
            serial: serial.to_string(),
            local: format!("tcp:{port}"),
            remote: "localabstract:xperf-agent".to_string(),
        }
    }

    fn dev(serial: &str, product: &str) -> AdbDevice {
        AdbDevice {
            serial: serial.to_string(),
            product: product.to_string(),
            model: String::new(),
            android_version: String::new(),
            platform: crate::platform::PlatformId::Android,
            is_gateway: false,
        }
    }

    // ---- parse_forward_list ----

    #[test]
    fn test_parse_forward_list() {
        let out = "42087266b1f tcp:5559 tcp:5557\n6eb792dfb0f tcp:38733 localabstract:xperf-agent\n";
        let rules = parse_forward_list(out);
        assert_eq!(rules, vec![gw_rule("42087266b1f", 5559), agent_rule("6eb792dfb0f", 38733)]);
        assert!(parse_forward_list("").is_empty());
        // 残缺行跳过
        assert!(parse_forward_list("onlyserial tcp:1\n").is_empty());
    }

    // ---- classify_device ----

    #[test]
    fn test_classify_gateway_by_rule() {
        let rules = vec![gw_rule("42087266b1f", 5559)];
        // 有 5557 规则：即使 product 非空（极端形态）也优先判网关
        assert_eq!(
            classify_device(&dev("42087266b1f", ""), &rules),
            DeviceClass::Gateway { port: 5559 }
        );
        // localabstract 规则不误判
        assert_eq!(
            classify_device(&dev("6eb792dfb0f", ""), &[agent_rule("6eb792dfb0f", 38733)]),
            DeviceClass::Candidate
        );
    }

    #[test]
    fn test_classify_candidate_and_regular() {
        let no_rules: Vec<ForwardRule> = vec![];
        // 无 product 直连设备 → 候选（MindRT 形态）
        assert_eq!(classify_device(&dev("42087266b1f", ""), &no_rules), DeviceClass::Candidate);
        // 带 product 的 Android（SS3/手机）→ 普通
        assert_eq!(classify_device(&dev("6eb792dfb0f", "HU_SS3"), &no_rules), DeviceClass::Regular);
        // 无 product 但 serial 含 ':'（TCP 伪设备，如 localhost:5559 本身/网络直连）
        // → 普通，不参与候选（防对桥接产物反复探测）
        assert_eq!(classify_device(&dev("localhost:5559", ""), &no_rules), DeviceClass::Regular);
        assert_eq!(classify_device(&dev("172.31.2.51:5555", ""), &no_rules), DeviceClass::Regular);
    }

    // ---- allocate_port ----

    #[test]
    fn test_allocate_port_default() {
        assert_eq!(allocate_port(&[], "gw1", DEFAULT_PORT), 5559);
    }

    #[test]
    fn test_allocate_port_reuse_same_serial() {
        // 同一网关已有规则 → 复用原端口（serial 稳定性不变量）
        let rules = vec![gw_rule("gw1", 5560)];
        assert_eq!(allocate_port(&rules, "gw1", DEFAULT_PORT), 5560);
    }

    #[test]
    fn test_allocate_port_skip_occupied() {
        // 5559 被别的 serial 占用 → 顺延（含 localabstract 规则的端口也算占用）
        let rules = vec![gw_rule("gwA", 5559), agent_rule("devB", 5560)];
        assert_eq!(allocate_port(&rules, "gw1", DEFAULT_PORT), 5561);
    }

    // ---- android_serial 不变量 ----

    #[test]
    fn test_android_serial_literal() {
        assert_eq!(android_serial(5559), "localhost:5559");
        // 必须是 localhost 字面而非 127.0.0.1（serial 稳定性不变量）
        assert!(!android_serial(5559).contains("127.0.0.1"));
    }

    // ---- cooldown 状态机（注入时钟） ----

    #[test]
    fn test_cooldown_semantics() {
        let mut failed: HashMap<String, Instant> = HashMap::new();
        let t0 = Instant::now();
        failed.insert("dev1".to_string(), t0);
        // 冷却期内
        assert!(cooldown_active(&failed, "dev1", t0 + Duration::from_secs(30)));
        // 冷却过期
        assert!(!cooldown_active(&failed, "dev1", t0 + Duration::from_secs(61)));
        // 未知设备无冷却
        assert!(!cooldown_active(&failed, "dev2", t0));
    }
}
