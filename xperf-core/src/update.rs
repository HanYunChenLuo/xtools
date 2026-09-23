//! 版本更新检测：查询内部 GitLab 项目最新 Release，与当前版本比较。
//!
//! 只检测 + 引导——输出 Release 页链接与资产清单（package registry 直链，
//! 登录态浏览器可直接下载），升级动作留给用户（AppImage/DMG/tar 三形态
//! 自动替换复杂且危险，不做自替换）。CLI `--check-update` 与 GUI 顶栏徽标
//! 共用本模块。
//!
//! 与 [`crate::feedback`] 同走 `reqwest::blocking`（rustls 内置证书链），
//! 调用方须在阻塞线程调用（CLI `spawn_blocking` / GUI 命令同理）。

use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::time::Duration;

use crate::gitlab;

/// Release 资产（package registry 直链）
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReleaseAsset {
    /// 资产名（如 `xperf-v0.3.2-macos-arm64-cli.tar.gz`）
    pub name: String,
    /// 下载链接（登录态浏览器可直接下载）
    pub url: String,
}

/// 一次 Release 的结构化信息（取自 GitLab `GET /projects/:id/releases` 响应）
#[derive(Debug, Clone, Serialize)]
pub struct ReleaseInfo {
    /// 版本 tag（`vX.Y.Z`，tag 格式由 CI `validate:tag` 锁定）
    pub tag: String,
    /// 发布时间（ISO 8601 原文，如 `2026-09-23T08:00:00.000+08:00`）
    pub released_at: String,
    /// Release 页面 URL（web 链接，浏览器打开）
    pub url: String,
    /// Release 说明（markdown 原文；展示层按纯文本呈现，不渲染）
    pub description: String,
    /// 资产清单
    pub assets: Vec<ReleaseAsset>,
}

/// 拉取最新 Release（列表接口 + semver 选最新，不依赖接口排序约定）。
///
/// 超时 5s；项目为私有，无凭证时报错并指引配置（与 feedback 同凭证链）。
pub fn fetch_latest_release() -> Result<ReleaseInfo> {
    let auth = gitlab::resolve_auth()?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .context("构建 HTTP client 失败")?;
    let (hname, hval) = auth.header();
    // per_page=100：默认只回 20 条/页，Release 数超页后高版本可能落在后续页，
    // 「semver 选最新」会假报已是最新（review M1）；100 对本项目余量充足
    let url = format!(
        "{}/projects/{}/releases?per_page=100",
        gitlab::gitlab_api(),
        gitlab::gitlab_project_id()
    );
    let resp = client
        .get(&url)
        .header(hname, &hval)
        .send()
        .context("releases 请求发送失败")?;
    if !resp.status().is_success() {
        let status = resp.status();
        // 错误 body 截断：极端情况下大段 HTML 进状态栏/diag（diag 会被 feedback 打包）
        let text: String = resp.text().unwrap_or_default().chars().take(200).collect();
        bail!("查询 releases 失败: {} {}{}", status, text, gitlab::auth_hint(status));
    }
    let list: Vec<serde_json::Value> = resp.json().context("解析 releases 响应失败")?;
    let releases: Vec<ReleaseInfo> = list.iter().filter_map(parse_release).collect();
    pick_latest(releases).ok_or_else(|| anyhow::anyhow!("项目尚无任何 Release"))
}

/// 单条 Release JSON → [`ReleaseInfo`]（纯函数，单测注入样例）。
/// 无 `tag_name` 的条目返回 None 跳过。
fn parse_release(v: &serde_json::Value) -> Option<ReleaseInfo> {
    let tag = v.get("tag_name")?.as_str()?.to_string();
    let released_at = v
        .get("released_at")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let description = v
        .get("description")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    // Release 页 URL：优先响应自带的 _links.self，否则按 web 路径约定拼
    let url = v
        .pointer("/_links/self")
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| format!("{}/-/releases/{}", gitlab::project_web_url(), tag));
    let assets = v
        .pointer("/assets/links")
        .and_then(|x| x.as_array())
        .map(|links| {
            links
                .iter()
                .filter_map(|l| {
                    let name = l.get("name")?.as_str()?.to_string();
                    let url = l.get("url")?.as_str()?.to_string();
                    Some(ReleaseAsset { name, url })
                })
                .collect()
        })
        .unwrap_or_default();
    Some(ReleaseInfo { tag, released_at, url, description, assets })
}

/// 从多条 Release 中选最新者（semver 最大；tag 非 semver 的视为最旧——
/// 正常不存在，CI `validate:tag` 锁定 vX.Y.Z 格式）。
fn pick_latest(mut releases: Vec<ReleaseInfo>) -> Option<ReleaseInfo> {
    releases.sort_by(|a, b| {
        parse_version(&a.tag)
            .cmp(&parse_version(&b.tag))
            .then_with(|| a.released_at.cmp(&b.released_at))
    });
    releases.pop()
}

/// `latest_tag` 是否比 `current` 新（strip `v` 前缀后 semver 比较）。
/// 任一侧不是合法 semver 时返回 None（调用方如实提示，不猜测）。
pub fn is_newer(latest_tag: &str, current: &str) -> Option<bool> {
    let latest = parse_version(latest_tag)?;
    let cur = parse_version(current)?;
    Some(latest > cur)
}

/// `vX.Y.Z` / `X.Y.Z` → semver 版本（非法格式返回 None）
fn parse_version(tag: &str) -> Option<semver::Version> {
    semver::Version::parse(tag.strip_prefix('v').unwrap_or(tag)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 脱敏的 GitLab releases 响应样例（字段子集对齐真实 schema）
    fn sample_releases_json() -> serde_json::Value {
        serde_json::json!([
            {
                "tag_name": "v0.3.1",
                "name": "v0.3.1",
                "released_at": "2026-09-20T10:00:00.000+08:00",
                "description": "## What's new\n- old",
                "assets": { "links": [] }
            },
            {
                "tag_name": "v0.3.2",
                "name": "v0.3.2",
                "released_at": "2026-09-23T08:00:00.000+08:00",
                "description": "## 修复\n- something",
                "assets": {
                    "links": [
                        {
                            "name": "xperf-v0.3.2-linux-x86_64-gui.AppImage",
                            "url": "https://gitlab.example.com/api/v4/projects/39859/packages/generic/xperf/v0.3.2/xperf-v0.3.2-linux-x86_64-gui.AppImage",
                            "link_type": "other"
                        },
                        {
                            "name": "xperf-v0.3.2-macos-arm64-cli.tar.gz",
                            "url": "https://gitlab.example.com/api/v4/projects/39859/packages/generic/xperf/v0.3.2/xperf-v0.3.2-macos-arm64-cli.tar.gz",
                            "link_type": "package"
                        }
                    ]
                },
                "_links": { "self": "https://gitlab.example.com/ligraphic/xperf/-/releases/v0.3.2" }
            }
        ])
    }

    #[test]
    fn test_parse_release_fields() {
        let list = sample_releases_json();
        let rel = parse_release(&list[1]).unwrap();
        assert_eq!(rel.tag, "v0.3.2");
        assert_eq!(rel.released_at, "2026-09-23T08:00:00.000+08:00");
        assert_eq!(rel.url, "https://gitlab.example.com/ligraphic/xperf/-/releases/v0.3.2");
        assert_eq!(rel.description, "## 修复\n- something");
        assert_eq!(rel.assets.len(), 2);
        assert_eq!(rel.assets[0].name, "xperf-v0.3.2-linux-x86_64-gui.AppImage");
        assert!(rel.assets[1].url.contains("packages/generic/xperf/v0.3.2/"));
    }

    #[test]
    fn test_parse_release_fallback_url_and_empty() {
        // 无 _links.self → 按约定拼 Release 页 URL；无 assets → 空清单
        let v = serde_json::json!({ "tag_name": "v9.9.9" });
        let rel = parse_release(&v).unwrap();
        assert!(rel.url.ends_with("/-/releases/v9.9.9"), "{}", rel.url);
        assert!(rel.assets.is_empty());
        assert!(rel.released_at.is_empty() && rel.description.is_empty());
        // 无 tag_name → 跳过
        assert!(parse_release(&serde_json::json!({ "name": "x" })).is_none());
    }

    #[test]
    fn test_pick_latest_semver_order() {
        let list = sample_releases_json();
        let releases: Vec<ReleaseInfo> = list.as_array().unwrap().iter().filter_map(parse_release).collect();
        let latest = pick_latest(releases).unwrap();
        assert_eq!(latest.tag, "v0.3.2");
        // 乱序 + 非 semver tag（视为最旧）
        let mut releases: Vec<ReleaseInfo> = ["v0.10.0", "nightly", "v0.9.9", "v0.3.2"]
            .iter()
            .map(|t| ReleaseInfo {
                tag: t.to_string(),
                released_at: String::new(),
                url: String::new(),
                description: String::new(),
                assets: vec![],
            })
            .collect();
        releases.reverse();
        assert_eq!(pick_latest(releases).unwrap().tag, "v0.10.0");
        assert!(pick_latest(vec![]).is_none());
    }

    #[test]
    fn test_is_newer() {
        assert_eq!(is_newer("v0.3.3", "0.3.2"), Some(true));
        assert_eq!(is_newer("v0.3.2", "0.3.2"), Some(false));
        assert_eq!(is_newer("v0.3.1", "0.3.2"), Some(false));
        assert_eq!(is_newer("0.10.0", "0.9.9"), Some(true));
        // 非法格式如实返回 None
        assert_eq!(is_newer("nightly", "0.3.2"), None);
        assert_eq!(is_newer("v0.3.3", "dev"), None);
    }

    /// 真机集成测试：经真实凭证链拉取最新 Release（无凭证时如实跳过）。
    /// 运行：`cargo test -p xperf-core -- --ignored fetch_latest_release`
    #[test]
    #[ignore]
    fn test_fetch_latest_release_real() {
        if gitlab::resolve_auth().is_err() {
            eprintln!("SKIP: 无 GitLab 凭证（GITLAB_TOKEN / gitlab-token 文件 / OAuth）");
            return;
        }
        let rel = fetch_latest_release().unwrap();
        assert!(rel.tag.starts_with('v'), "tag: {}", rel.tag);
        assert!(parse_version(&rel.tag).is_some(), "tag 应为 semver: {}", rel.tag);
        eprintln!("latest release: {} assets={}", rel.tag, rel.assets.len());
    }
}
