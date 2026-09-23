//! GitLab 实例公共访问：API base / 项目 ID / 凭证解析链。
//!
//! 供 [`crate::feedback`]（问题反馈上传）与 [`crate::update`]（版本更新检测）
//! 共用。项目为私有，一切 API 访问都需凭证，优先级：`GITLAB_TOKEN` env >
//! `~/.config/xperf/gitlab-token` PAT 文件 > OAuth 登录态（[`crate::oauth`]，
//! 临期自动刷新）。issue/Release 的属主身份即凭证属主。

use anyhow::{anyhow, Result};
use std::path::PathBuf;

/// GitLab 项目 ID（ligraphic/xperf）
const DEFAULT_PROJECT_ID: &str = "39859";

/// GitLab API base（与 `scripts/release_upload.py` 同源）
const DEFAULT_GITLAB_API: &str = "https://gitlab.chehejia.com/api/v4";

/// GitLab 项目 web 路径（拼 Release 页面 URL 用；与 [`DEFAULT_PROJECT_ID`] 指同一项目）
const DEFAULT_PROJECT_PATH: &str = "ligraphic/xperf";

/// GitLab API base（`GITLAB_API` 环境变量可覆盖，与 `scripts/release_upload.py` 同源）
pub fn gitlab_api() -> String {
    std::env::var("GITLAB_API")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_GITLAB_API.into())
}

/// GitLab 项目 ID（`GITLAB_PROJECT_ID` 环境变量可覆盖）
pub fn gitlab_project_id() -> String {
    std::env::var("GITLAB_PROJECT_ID")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_PROJECT_ID.into())
}

/// 项目 web 页 base URL（由 API base 剥掉 `/api/v4` 后缀 + 项目路径拼成；
/// 项目路径可用 `GITLAB_PROJECT_PATH` 环境变量覆盖）
pub fn project_web_url() -> String {
    let web_base = gitlab_api()
        .trim_end_matches('/')
        .trim_end_matches("/api/v4")
        .to_string();
    let path = std::env::var("GITLAB_PROJECT_PATH")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_PROJECT_PATH.into());
    format!("{web_base}/{path}")
}

/// 401/403 时给错误信息追加凭证失效指引（PAT 过期 / OAuth 需重新登录）
pub fn auth_hint(status: reqwest::StatusCode) -> &'static str {
    match status {
        reqwest::StatusCode::UNAUTHORIZED => {
            "（凭证失效：PAT 过期或 OAuth 授权被撤销——重新登录：GUI 浮层「登录 GitLab」/ CLI --gitlab-login）"
        }
        reqwest::StatusCode::FORBIDDEN => "（凭证权限不足：需 api scope + 项目 Reporter 及以上角色）",
        _ => "",
    }
}

/// 解析 GitLab 凭证（优先级：`GITLAB_TOKEN` env > `~/.config/xperf/gitlab-token` PAT 文件
/// > OAuth 已登录 token——临期自动刷新）。三者皆无时报错并指引三条路径。
pub fn resolve_auth() -> Result<crate::oauth::GitlabAuth> {
    if let Some(t) = resolve_token_from(
        std::env::var("GITLAB_TOKEN").ok().as_deref(),
        &token_file_path(),
    ) {
        return Ok(crate::oauth::GitlabAuth::Pat(t));
    }
    if let Some(t) = crate::oauth::current_access_token()? {
        return Ok(crate::oauth::GitlabAuth::OAuth(t));
    }
    Err(anyhow!(
        "未找到 GitLab 凭证，三条路径任选：① GITLAB_TOKEN 环境变量（api 权限 PAT）；\
         ② 将 PAT 写入 {}（建议 chmod 600）；③ OAuth 登录（GUI 反馈浮层「登录 GitLab」\
         或 CLI `xperf-cli --gitlab-login`）",
        token_file_path().display()
    ))
}

/// token 文件路径（`~/.config/xperf/gitlab-token`，与 remotes.json 同目录约定）
fn token_file_path() -> PathBuf {
    // HOME 空串视同未设置（空基座会拼成相对路径落进程 CWD）
    std::env::var_os("HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config/xperf/gitlab-token")
}

/// PAT 解析的纯函数内核（单测可注入）：env > 文件
fn resolve_token_from(env: Option<&str>, token_file: &std::path::Path) -> Option<String> {
    if let Some(t) = env.map(str::trim).filter(|t| !t.is_empty()) {
        return Some(t.to_string());
    }
    if let Ok(content) = std::fs::read_to_string(token_file) {
        let t = content.trim();
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_token_from_priority() {
        let dir = std::env::temp_dir().join(format!("xperf-gltest-tok-{}", std::process::id()));
        let token_file = dir.join("gitlab-token");
        std::fs::create_dir_all(&dir).unwrap();
        // env 优先
        std::fs::write(&token_file, b"file-token\n").unwrap();
        assert_eq!(
            resolve_token_from(Some(" env-token "), &token_file).as_deref(),
            Some("env-token")
        );
        // 文件兜底（去空白）
        assert_eq!(
            resolve_token_from(None, &token_file).as_deref(),
            Some("file-token")
        );
        assert_eq!(
            resolve_token_from(Some("  "), &token_file).as_deref(),
            Some("file-token")
        );
        // 均无 → None（由 resolve_auth 统一报错指引）
        assert!(resolve_token_from(None, &dir.join("nonexist")).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_project_web_url_derivation() {
        // 默认 API base 剥 /api/v4 后缀
        //（不依赖环境变量的形态经默认值路径验证）
        assert!(DEFAULT_GITLAB_API.ends_with("/api/v4"));
        assert!(project_web_url().ends_with(DEFAULT_PROJECT_PATH));
        assert!(!project_web_url().contains("/api/v4"));
    }

    #[test]
    fn test_auth_hint_messages() {
        assert!(auth_hint(reqwest::StatusCode::UNAUTHORIZED).contains("凭证失效"));
        assert!(auth_hint(reqwest::StatusCode::FORBIDDEN).contains("权限不足"));
        assert!(auth_hint(reqwest::StatusCode::NOT_FOUND).is_empty());
    }
}
