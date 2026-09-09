//! adb 传输后端：本机 adb server 或经 SSH 隧道的远端 adb server。
//!
//! 设计约束：本机恒为 adb **客户端**（经 `ADB_SERVER_SOCKET` 指向远端 server），
//! 故 `pull`/`push` 落点、分析工具链（trace_processor/report_html.py）、桌面打开
//! 全部留在本机零改动；只有 `adb forward` 的监听端口在远端 server 侧，
//! agent NDJSON 流需第二跳隧道映射回本机（见 `SshTunnel`，S2 引入）。
//!
//! 完整设计与实测依据见 `docs/DESIGN-ssh-remote.md`。

use std::sync::Mutex;

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
    /// 远端 adb 可执行路径。**不能假设在 PATH 中**（实测 hppc 即不在），
    /// 默认 `"adb"`。
    pub adb_path: String,
    /// 远端 adb server 监听端口，默认 [`SshTarget::DEFAULT_ADB_PORT`]。
    pub remote_port: u16,
}

impl SshTarget {
    /// 远端 adb server 默认端口（5037）
    pub const DEFAULT_ADB_PORT: u16 = 5037;

    /// 构造远端描述：adb 路径默认 `"adb"`，端口默认 5037。
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            adb_path: "adb".to_string(),
            remote_port: Self::DEFAULT_ADB_PORT,
        }
    }

    /// 指定远端 adb 可执行路径（远端 PATH 常不含 adb 时使用）
    pub fn with_adb_path(mut self, path: impl Into<String>) -> Self {
        self.adb_path = path.into();
        self
    }

    /// 指定远端 adb server 监听端口
    pub fn with_remote_port(mut self, port: u16) -> Self {
        self.remote_port = port;
        self
    }
}

/// 当前传输后端（进程级全局，与既有 `utils::TARGET_SERIAL` 同款模式）。
static TRANSPORT: Mutex<Transport> = Mutex::new(Transport::Local);

/// 当前传输后端（默认 [`Transport::Local`]）
pub fn transport() -> Transport {
    TRANSPORT
        .lock()
        .map(|g| g.clone())
        .unwrap_or(Transport::Local)
}

/// 设置传输后端。须在任何 adb 调用之前完成
/// （远程模式：`init_remote` 建隧道成功后才设置为 `Ssh`，见 S4）。
pub fn set_transport(t: Transport) {
    if let Ok(mut g) = TRANSPORT.lock() {
        *g = t;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ssh_target_defaults() {
        let t = SshTarget::new("hppc");
        assert_eq!(t.host, "hppc");
        assert_eq!(t.adb_path, "adb");
        assert_eq!(t.remote_port, 5037);
    }

    #[test]
    fn test_ssh_target_builders() {
        let t = SshTarget::new("user@192.168.1.10")
            .with_adb_path("~/Android/Sdk/platform-tools/adb")
            .with_remote_port(5038);
        assert_eq!(t.host, "user@192.168.1.10");
        assert_eq!(t.adb_path, "~/Android/Sdk/platform-tools/adb");
        assert_eq!(t.remote_port, 5038);
    }

    // 全局 TRANSPORT 的读写集中在单个测试函数内，避免同进程并行测试互踩
    #[test]
    fn test_transport_global_roundtrip() {
        assert_eq!(transport(), Transport::Local); // 默认本机
        let target = SshTarget::new("hppc");
        set_transport(Transport::Ssh(target.clone()));
        assert_eq!(transport(), Transport::Ssh(target));
        set_transport(Transport::Local); // 复位，不影响其他测试
        assert_eq!(transport(), Transport::Local);
    }
}
