//! 时间轴事件标记（打点）：外部进程通过 Unix socket 发送标签，
//! CLI/GUI 监听后打印并写入 markers.csv，用于对齐指标变化。
//!
//! 用法：echo "开始倒车" | nc -U /tmp/xperf-marker.sock
//! 或：  echo "开始倒车" >> /tmp/xperf-marker （文件追加模式）

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::mpsc::{channel, Receiver};

/// 打点事件
#[derive(Debug, Clone)]
pub struct Marker {
    pub label: String,
    pub timestamp_ms: u64,
}

/// 启动 Unix stream socket 监听线程，返回 marker 接收端。
/// socket 路径：/tmp/xperf-marker.sock（外部用 `echo "标签" | nc -U /tmp/xperf-marker.sock` 发送）
pub fn start_marker_listener(sock_path: &str) -> Option<Receiver<Marker>> {
    let sock_path = sock_path.to_string();
    // 清理旧 socket
    let _ = std::fs::remove_file(&sock_path);
    let listener = UnixListener::bind(&sock_path).ok()?;
    let (tx, rx) = channel::<Marker>();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(s) = stream else { continue };
            let tx = tx.clone();
            // 每连接独立线程，避免空连接阻塞后续打点
            std::thread::spawn(move || {
                let mut reader = BufReader::new(s);
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() { return; }
                let label = line.trim().to_string();
                if label.is_empty() { return; }
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                let _ = tx.send(Marker { label, timestamp_ms: ts });
            });
        }
    });
    Some(rx)
}

/// 外部触发点：向 socket 发送打点标签（非阻塞，失败静默）
pub fn send_marker(sock_path: &str, label: &str) -> std::io::Result<()> {
    let mut sock = UnixStream::connect(Path::new(sock_path))?;
    writeln!(sock, "{}", label)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_marker_roundtrip() {
        let path = format!("/tmp/xperf-marker-test-{}.sock", std::process::id());
        let rx = start_marker_listener(&path).expect("bind 失败");
        send_marker(&path, "开始倒车").unwrap();
        let m = rx.recv_timeout(std::time::Duration::from_secs(2)).expect("超时");
        assert_eq!(m.label, "开始倒车");
        assert!(m.timestamp_ms > 0);
        let _ = std::fs::remove_file(&path);
    }
}
