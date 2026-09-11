//! 设备屏幕截取：`adb exec-out screencap -p` 二进制直写本机 PNG。
//!
//! - `exec-out` 是二进制安全通道（无 PTY 的 CRLF 转换），SSH 远程模式经 hop#1
//!   隧道天然承载（`adb_for` 注入 `ADB_SERVER_SOCKET`），零隧道改动；
//! - 零设备端文件残留（不落地 `/sdcard` 中转），非 root 可用（shell 权限足够）；
//! - 输出先过 PNG 魔数校验再落盘——设备 screencap 异常时 stdout 可能为空或
//!   是文本错误信息，直接写盘会产出不可见的坏文件。

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::Local;

/// PNG 文件魔数（8 字节固定头）
const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

/// 校验字节流确为 PNG（魔数比对；空输出/壳层错误文本在此拦截）
fn validate_png(bytes: &[u8]) -> Result<()> {
    if !bytes.starts_with(PNG_MAGIC) {
        let head: String = bytes
            .iter()
            .take(64)
            .map(|b| if b.is_ascii_graphic() || *b == b' ' { *b as char } else { '.' })
            .collect();
        bail!(
            "screencap 输出非 PNG（{} 字节，头部: {:?}）",
            bytes.len(),
            head
        );
    }
    Ok(())
}

/// 截取设备当前屏幕，保存为 `<dest_dir>/shot_<ts>.png`，返回产物路径。
///
/// `serial`：目标设备（`None` 回退全局选择——CLI 经 `select_device` 已写入）。
/// 失败即 Err（adb 非零退出附 stderr，PNG 校验失败附头部摘要）。
pub fn screenshot(serial: Option<&str>, dest_dir: &Path) -> Result<PathBuf> {
    let eff = crate::utils::resolve_serial(serial).context("未选择目标设备（先连接设备）")?;
    fs::create_dir_all(dest_dir)
        .with_context(|| format!("创建截屏目录失败: {}", dest_dir.display()))?;
    let out = crate::utils::adb_for(Some(&eff))
        .args(["exec-out", "screencap", "-p"])
        .output()
        .context("adb exec-out screencap 执行失败")?;
    if !out.status.success() {
        bail!(
            "screencap 失败（{}）: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    validate_png(&out.stdout)?;
    let path = dest_dir.join(format!("shot_{}.png", Local::now().format("%Y%m%d_%H%M%S_%3f")));
    fs::write(&path, &out.stdout)
        .with_context(|| format!("PNG 落盘失败: {}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_png() {
        // 合法 PNG 头通过
        assert!(validate_png(b"\x89PNG\r\n\x1a\n....").is_ok());
        // 空输出 / 文本错误信息 / 截断头均被拦截
        assert!(validate_png(b"").is_err());
        assert!(validate_png(b"error: closed").is_err());
        assert!(validate_png(b"\x89PNG\r\n\x1a").is_err());
        // 错误信息含可打印头部摘要（定位设备端报错用）
        let msg = validate_png(b"error: permission denied").unwrap_err().to_string();
        assert!(msg.contains("error: permission denied"), "应含头部摘要: {msg}");
    }
}
