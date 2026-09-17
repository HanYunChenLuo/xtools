//! GitLab OAuth2 登录：授权码 + PKCE + 本机回环回调，把 issue 作者绑定为操作者本人。
//!
//! 流程（RFC 8252 桌面应用标准形态）：本机临时监听 `127.0.0.1:<port>` → 打开浏览器到
//! GitLab 授权页（用户走日常 SSO/域账号登录）→ 授权后浏览器携 code 跳回回环地址 →
//! code + PKCE verifier 换 token → 落盘 `~/.config/xperf/gitlab-oauth.json`（0600，
//! 原子写）。access_token 默认 2h 有效，过期前用 refresh_token 静默轮换（GitLab 每次
//! 刷新换发新 refresh_token，旧的立即作废——落盘失败即丢登录态，故原子写）。
//!
//! 认证头与 PAT 不同：OAuth token 用 `Authorization: Bearer`
//! （[`GitlabAuth::header`](crate::oauth::GitlabAuth::header) 统一封装，调用方无感）。
//! token 端点不换 JSON 时用 `application/x-www-form-urlencoded`。
//!
//! 无头环境（SSH/CI）无浏览器可用，PAT（env/文件）路径保留——见 [`crate::feedback`]。

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::Digest as _;
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// 默认 OAuth App client_id（ligraphic/xperf 反馈用，public client 无 secret）
const DEFAULT_CLIENT_ID: &str =
    "f9ca4b2331d11c659dcbe62603b4a19ff88e256b9eb9e6ed7c3452dcb9dae2b5";

/// 回环回调候选端口（注册 redirect_uri 须含其一；占用时顺延）
const CALLBACK_PORTS: &[u16] = &[39859, 39860, 39861];

/// 授权等待超时（用户在浏览器里登录+点授权的时间；人工操作留足余量）
const LOGIN_TIMEOUT: Duration = Duration::from_secs(600);

/// token 过期前提前刷新的余量（秒）
const REFRESH_MARGIN_SECS: i64 = 60;

/// 授权 scope：`api` 是覆盖「建 issue + 传附件」的最小 scope（GitLab 无 issue 级细粒度）
const SCOPE: &str = "api";

/// GitLab 凭证：PAT（env/文件）或 OAuth access token——仅认证头不同，调用方统一用
/// [`GitlabAuth::header`] 设置。
pub enum GitlabAuth {
    /// Personal Access Token（`PRIVATE-TOKEN` 头）
    Pat(String),
    /// OAuth access token（`Authorization: Bearer` 头）
    OAuth(String),
}

impl GitlabAuth {
    /// 认证头名与值（reqwest `.header(name, value)`）
    pub fn header(&self) -> (&'static str, String) {
        match self {
            GitlabAuth::Pat(t) => ("PRIVATE-TOKEN", t.clone()),
            GitlabAuth::OAuth(t) => ("Authorization", format!("Bearer {t}")),
        }
    }
}

/// 已登录的 OAuth 身份（状态展示用，不触网）
#[derive(Debug, Clone)]
pub struct OAuthIdentity {
    /// GitLab 用户名（@后面的）
    pub username: String,
    /// 显示名
    pub name: String,
}

/// 落盘的 token 对（`~/.config/xperf/gitlab-oauth.json`）
#[derive(Serialize, Deserialize)]
struct TokenStore {
    access_token: String,
    refresh_token: String,
    /// access_token 过期时刻（epoch 秒）
    expires_at: i64,
    username: String,
    name: String,
}

// ==================== 公共入口 ====================

/// 交互式登录：起回环监听 → 打开浏览器 → 等回调 → 换 token → 落盘。
/// 返回显示名（如「王金涵 (@wangjinhan)」）。须在阻塞线程调用（reqwest::blocking）。
pub fn login() -> Result<String> {
    let (listener, port) = bind_listener()?;
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");
    let verifier = pkce_verifier();
    let state = random_hex(16);
    let url = authorize_url(&redirect_uri, &state, &verifier);
    open_browser(&url);
    crate::utils::diag(&format!("oauth: 等待浏览器回调（{port}，超时 {}s）", LOGIN_TIMEOUT.as_secs()));
    let code = wait_for_code(&listener, &state, LOGIN_TIMEOUT)?;
    let client = http_client()?;
    let tokens = exchange_code(&client, &code, &redirect_uri, &verifier)?;
    let (username, name) = fetch_identity(&client, &tokens.access_token)
        .context("获取用户信息失败（token 已换到但未保存，请重新登录）")?;
    let store = TokenStore {
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        expires_at: now_epoch() + tokens.expires_in,
        username,
        name,
    };
    save_store(&store_path(), &store)?;
    Ok(format!("{} (@{})", store.name, store.username))
}

/// 退出登录（删除本地 token 文件；GitLab 侧的授权可在 profile → Applications 管理）
pub fn logout() -> Result<bool> {
    let path = store_path();
    if path.exists() {
        std::fs::remove_file(&path).with_context(|| format!("删除失败: {}", path.display()))?;
        return Ok(true);
    }
    Ok(false)
}

/// 当前 OAuth 登录身份（未登录返回 None；只读本地文件不触网）
pub fn status() -> Option<OAuthIdentity> {
    let store = load_store(&store_path())?;
    Some(OAuthIdentity { username: store.username, name: store.name })
}

/// 取当前可用 access token（临期/过期自动刷新并原子落盘；未登录返回 None）。
/// refresh 被 GitLab 拒绝（invalid_grant，如授权被撤销）时删除本地存储返回 None，
/// 由调用方引导重新登录。
pub fn current_access_token() -> Result<Option<String>> {
    let path = store_path();
    let Some(mut store) = load_store(&path) else { return Ok(None) };
    if now_epoch() + REFRESH_MARGIN_SECS < store.expires_at {
        return Ok(Some(store.access_token));
    }
    crate::utils::diag("oauth: access token 临期，刷新中");
    let client = http_client()?;
    match refresh_tokens(&client, &store.refresh_token) {
        Ok(tokens) => {
            store.access_token = tokens.access_token;
            store.refresh_token = tokens.refresh_token;
            store.expires_at = now_epoch() + tokens.expires_in;
            save_store(&path, &store)?;
            Ok(Some(store.access_token))
        }
        Err(RefreshError::InvalidGrant) => {
            let _ = std::fs::remove_file(&path);
            crate::utils::diag("oauth: refresh 被拒（授权可能已撤销），已清除本地登录态");
            Ok(None)
        }
        Err(RefreshError::Other(e)) => Err(e),
    }
}

// ==================== 配置解析 ====================

/// client_id（`XPERF_GITLAB_CLIENT_ID` 可覆盖，测试/自建实例用）
fn client_id() -> String {
    std::env::var("XPERF_GITLAB_CLIENT_ID")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_CLIENT_ID.into())
}

/// GitLab 站点根（由 `GITLAB_API` 推导：去掉尾部 `/api/v4`；默认内部实例）
fn server_root() -> String {
    let api = std::env::var("GITLAB_API")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "https://gitlab.chehejia.com/api/v4".into());
    api.trim_end_matches('/')
        .strip_suffix("/api/v4")
        .map(str::to_string)
        .unwrap_or(api)
}

/// token 存储路径（`~/.config/xperf/gitlab-oauth.json`，与 remotes.json 同目录约定）
fn store_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config/xperf/gitlab-oauth.json")
}

// ==================== 登录流程细节 ====================

/// 绑定回环监听（按候选端口顺延），返回监听器与实际端口
fn bind_listener() -> Result<(TcpListener, u16)> {
    for &port in CALLBACK_PORTS {
        match TcpListener::bind(("127.0.0.1", port)) {
            Ok(l) => return Ok((l, port)),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => continue,
            Err(e) => return Err(e).context("绑定回环端口失败"),
        }
    }
    bail!("回环端口 {:?} 均被占用", CALLBACK_PORTS)
}

/// 授权页 URL（response_type=code + PKCE S256 + state 防 CSRF）
fn authorize_url(redirect_uri: &str, state: &str, verifier: &str) -> String {
    let challenge = pkce_challenge(verifier);
    format!(
        "{}/oauth/authorize?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
        server_root(),
        client_id(),
        urlencoding(redirect_uri),
        SCOPE,
        state,
        challenge
    )
}

/// 打开浏览器（失败不致命——打印 URL 由用户手动打开）
fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let opener = "open";
    #[cfg(not(target_os = "macos"))]
    let opener = "xdg-open";
    if std::process::Command::new(opener).arg(url).spawn().is_err() {
        eprintln!("无法自动打开浏览器，请手动访问:\n{url}");
    }
}

/// 等待浏览器回调并解析 code（校验 state，防 CSRF）；超时/中断/授权被拒均报错。
/// 非阻塞轮询 accept（100ms 粒度），保证 Ctrl-C 中断标志可即时生效。
fn wait_for_code(listener: &TcpListener, expect_state: &str, timeout: Duration) -> Result<String> {
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + timeout;
    loop {
        if crate::utils::is_interrupted() {
            bail!("用户中断");
        }
        if Instant::now() > deadline {
            bail!("等待授权超时（{}s），请重试", timeout.as_secs());
        }
        match listener.accept() {
            Ok((mut conn, _)) => {
                let _ = conn.set_read_timeout(Some(Duration::from_secs(5)));
                let mut buf = [0u8; 8192];
                let n = conn.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                match parse_callback(&req) {
                    CallbackResult::Code { code, state } if state == expect_state => {
                        respond(&mut conn, "200 OK", "登录完成，请回到 xperf（本页可关闭）");
                        return Ok(code);
                    }
                    CallbackResult::Code { .. } => {
                        respond(&mut conn, "400 Bad Request", "state 校验失败（疑似 CSRF），请重试登录");
                        bail!("回调 state 不匹配");
                    }
                    CallbackResult::Denied { error } => {
                        // 拒绝可能是误点——应答后继续等到超时（用户可回退授权页重点 Authorize）
                        respond(
                            &mut conn,
                            "200 OK",
                            "你点击了拒绝授权。如属误操作，请回到浏览器授权页重新点「Authorize」；xperf 仍在等待。",
                        );
                        crate::utils::diag(&format!("oauth: 收到 access_denied（{error}），继续等待"));
                    }
                    CallbackResult::Unrecognized => {
                        // 浏览器 favicon 等杂项请求：应答后继续等真回调
                        respond(&mut conn, "404 Not Found", "not found");
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(e).context("回环监听失败"),
        }
    }
}

/// 回调解析结果
enum CallbackResult {
    /// 授权码 + state
    Code { code: String, state: String },
    /// 用户在授权页点拒绝（`error=access_denied`）
    Denied { error: String },
    /// 非回调请求（favicon 等）
    Unrecognized,
}

/// 解析 HTTP 请求行 `GET /callback?code=…&state=… HTTP/1.1`
fn parse_callback(request: &str) -> CallbackResult {
    let line = match request.lines().next() {
        Some(l) => l,
        None => return CallbackResult::Unrecognized,
    };
    let mut parts = line.split_whitespace();
    if parts.next() != Some("GET") {
        return CallbackResult::Unrecognized;
    }
    let target = parts.next().unwrap_or("");
    let query = match target.strip_prefix("/callback?") {
        Some(q) => q,
        None => return CallbackResult::Unrecognized,
    };
    let mut code = String::new();
    let mut state = String::new();
    let mut error = String::new();
    for kv in query.split('&') {
        let mut it = kv.splitn(2, '=');
        match (it.next(), it.next()) {
            (Some("code"), Some(v)) => code = percent_decode(v),
            (Some("state"), Some(v)) => state = percent_decode(v),
            (Some("error"), Some(v)) => error = percent_decode(v),
            _ => {}
        }
    }
    if !error.is_empty() {
        return CallbackResult::Denied { error };
    }
    if code.is_empty() {
        return CallbackResult::Unrecognized;
    }
    CallbackResult::Code { code, state }
}

/// 回环 HTTP 应答（一次性，纯文本；浏览器展示用）
fn respond(conn: &mut std::net::TcpStream, status: &str, body: &str) {
    let _ = conn.write_all(
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .as_bytes(),
    );
}

// ==================== token 交换与刷新 ====================

/// token 端点返回的字段
struct TokenPair {
    access_token: String,
    refresh_token: String,
    /// 有效时长（秒）
    expires_in: i64,
}

/// 授权码换 token（public client：无 secret，PKCE verifier 证明身份）
fn exchange_code(
    client: &reqwest::blocking::Client,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<TokenPair> {
    let resp = client
        .post(format!("{}/oauth/token", server_root()))
        .form(&[
            ("client_id", client_id()),
            ("code", code.to_string()),
            ("grant_type", "authorization_code".into()),
            ("redirect_uri", redirect_uri.to_string()),
            ("code_verifier", verifier.to_string()),
        ])
        .send()
        .context("token 交换请求失败")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().unwrap_or_default();
        // 公共客户端（无 secret）撞上机密应用注册时的典型错误：给出可操作指引
        if status == reqwest::StatusCode::UNAUTHORIZED && text.contains("invalid_client") {
            bail!(
                "token 交换被拒: invalid_client——OAuth 应用注册时「Confidential」须取消勾选\
                 （桌面工具是公共客户端，无 secret），请到应用设置修改后重试"
            );
        }
        bail!("token 交换被拒: {} {}", status, text);
    }
    parse_token_response(&resp.text().context("读 token 响应失败")?)
}

/// 刷新失败分类：invalid_grant（授权撤销/refresh 已轮换） vs 其他（网络等临时故障）
enum RefreshError {
    InvalidGrant,
    Other(anyhow::Error),
}

/// refresh_token 换新 token 对（GitLab 轮换语义：新响应里的 refresh_token 替换旧的）
fn refresh_tokens(
    client: &reqwest::blocking::Client,
    refresh_token: &str,
) -> std::result::Result<TokenPair, RefreshError> {
    let resp = client
        .post(format!("{}/oauth/token", server_root()))
        .form(&[
            ("client_id", client_id()),
            ("refresh_token", refresh_token.to_string()),
            ("grant_type", "refresh_token".into()),
        ])
        .send()
        .map_err(|e| RefreshError::Other(anyhow!("token 刷新请求失败: {e:#}")))?;
    if resp.status() == reqwest::StatusCode::BAD_REQUEST
        || resp.status() == reqwest::StatusCode::UNAUTHORIZED
    {
        return Err(RefreshError::InvalidGrant);
    }
    if !resp.status().is_success() {
        let s = resp.status();
        return Err(RefreshError::Other(anyhow!(
            "token 刷新失败: {} {}",
            s,
            resp.text().unwrap_or_default()
        )));
    }
    let text = resp.text().map_err(|e| RefreshError::Other(anyhow!("读刷新响应失败: {e:#}")))?;
    parse_token_response(&text).map_err(RefreshError::Other)
}

/// 解析 token 端点 JSON 响应
fn parse_token_response(text: &str) -> Result<TokenPair> {
    let v: serde_json::Value = serde_json::from_str(text).context("token 响应非 JSON")?;
    let get = |k: &str| -> Result<String> {
        v.get(k)
            .and_then(|x| x.as_str())
            .map(str::to_string)
            .ok_or_else(|| anyhow!("token 响应缺少字段 {k}"))
    };
    Ok(TokenPair {
        access_token: get("access_token")?,
        refresh_token: get("refresh_token")?,
        expires_in: v.get("expires_in").and_then(|x| x.as_i64()).unwrap_or(7200),
    })
}

/// `GET /user` 取当前身份（username + 显示名）
fn fetch_identity(client: &reqwest::blocking::Client, access_token: &str) -> Result<(String, String)> {
    let resp = client
        .get(format!("{}/api/v4/user", server_root()))
        .header("Authorization", format!("Bearer {access_token}"))
        .send()
        .context("user 请求失败")?;
    if !resp.status().is_success() {
        bail!("user 请求被拒: {}", resp.status());
    }
    let v: serde_json::Value = resp.json().context("user 响应非 JSON")?;
    let username = v.get("username").and_then(|x| x.as_str()).unwrap_or("?").to_string();
    let name = v.get("name").and_then(|x| x.as_str()).unwrap_or(&username).to_string();
    Ok((username, name))
}

// ==================== 存储 ====================

/// 读本地 token 存储（文件缺失/损坏均返回 None——损坏视同未登录，重新登录即可）
fn load_store(path: &Path) -> Option<TokenStore> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// 原子落盘（tmp+rename，0600）：refresh 轮换下旧 refresh_token 立即作废，写坏即丢登录态
fn save_store(path: &Path, store: &TokenStore) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    let text = serde_json::to_string_pretty(store)?;
    {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        f.write_all(text.as_bytes())?;
        f.sync_all().ok();
    }
    std::fs::rename(&tmp, path).with_context(|| format!("落盘失败: {}", path.display()))?;
    Ok(())
}

// ==================== 小工具 ====================

/// reqwest blocking client（60s 超时够 token 交换/刷新；大文件传输在 feedback.rs）
fn http_client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .context("构建 HTTP client 失败")
}

/// PKCE code_verifier：43 字符 base64url（RFC 7636，字符集 [A-Za-z0-9-._~]）
fn pkce_verifier() -> String {
    random_base64url(32)
}

/// PKCE code_challenge = BASE64URL-NOPAD(SHA256(verifier))
fn pkce_challenge(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier.as_bytes()))
}

/// N 字节随机数的 base64url（无填充）编码
fn random_base64url(n: usize) -> String {
    let mut buf = vec![0u8; n];
    getrandom::getrandom(&mut buf).expect("getrandom 不可用");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

/// N 字节随机数的 hex 编码
fn random_hex(n: usize) -> String {
    let mut buf = vec![0u8; n];
    getrandom::getrandom(&mut buf).expect("getrandom 不可用");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// URL 查询参数编码（保留 unreserved 字符集）
fn urlencoding(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// 百分号解码（+ 号按空格处理，OAuth 回调参数惯例）
fn percent_decode(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                // 越界由 get() 的 None 兜住（非法 % 原样保留）
                if let (Some(h), Some(l)) = (hex_val(bytes.get(i + 1)), hex_val(bytes.get(i + 2))) {
                    out.push(h * 16 + l);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: Option<&u8>) -> Option<u8> {
    match b {
        Some(c @ b'0'..=b'9') => Some(c - b'0'),
        Some(c @ b'a'..=b'f') => Some(c - b'a' + 10),
        Some(c @ b'A'..=b'F') => Some(c - b'A' + 10),
        _ => None,
    }
}

fn now_epoch() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pkce_rfc7636_vector() {
        // RFC 7636 Appendix B 官方测试向量
        let v = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(pkce_challenge(v), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn test_pkce_verifier_charset() {
        let v = pkce_verifier();
        assert_eq!(v.len(), 43); // 32 字节 → base64url 无填充恒 43 字符
        assert!(v.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~')));
        assert_ne!(pkce_verifier(), pkce_verifier(), "两次生成不应相同");
    }

    #[test]
    fn test_authorize_url_params() {
        let url = authorize_url("http://127.0.0.1:39859/callback", "st123", "verifierX");
        assert!(url.contains("response_type=code"), "{url}");
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A39859%2Fcallback"), "{url}");
        assert!(url.contains("state=st123"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("scope=api"));
        assert!(url.contains(&format!("client_id={DEFAULT_CLIENT_ID}")));
        assert!(url.starts_with("https://gitlab.chehejia.com/oauth/authorize?"));
    }

    #[test]
    fn test_parse_callback() {
        let req = "GET /callback?code=abc123&state=st9 HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n";
        match parse_callback(req) {
            CallbackResult::Code { code, state } => {
                assert_eq!(code, "abc123");
                assert_eq!(state, "st9");
            }
            _ => panic!("应解析出 code"),
        }
        // 拒绝授权
        match parse_callback("GET /callback?error=access_denied&state=x HTTP/1.1\r\n\r\n") {
            CallbackResult::Denied { error } => assert_eq!(error, "access_denied"),
            _ => panic!("应解析出拒绝"),
        }
        // favicon / 非回调
        assert!(matches!(
            parse_callback("GET /favicon.ico HTTP/1.1\r\n\r\n"),
            CallbackResult::Unrecognized
        ));
        // 缺 code
        assert!(matches!(
            parse_callback("GET /callback?state=x HTTP/1.1\r\n\r\n"),
            CallbackResult::Unrecognized
        ));
    }

    #[test]
    fn test_percent_decode() {
        assert_eq!(percent_decode("a%20b+c%2Fd"), "a b c/d");
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(percent_decode("100%"), "100%"); // 非法 % 原样保留
        assert_eq!(percent_decode("%e4%b8%ad"), "中"); // UTF-8 多字节
    }

    #[test]
    fn test_parse_token_response() {
        let t = parse_token_response(
            r#"{"access_token":"at","refresh_token":"rt","expires_in":7200,"token_type":"Bearer"}"#,
        )
        .unwrap();
        assert_eq!(t.access_token, "at");
        assert_eq!(t.refresh_token, "rt");
        assert_eq!(t.expires_in, 7200);
        // 缺 expires_in 用默认 7200
        let t2 = parse_token_response(r#"{"access_token":"a","refresh_token":"r"}"#).unwrap();
        assert_eq!(t2.expires_in, 7200);
        assert!(parse_token_response(r#"{"error":"invalid_grant"}"#).is_err());
    }

    #[test]
    fn test_store_roundtrip_and_perms() {
        let dir = std::env::temp_dir().join(format!("xperf-oauth-test-{}", std::process::id()));
        let path = dir.join("gitlab-oauth.json");
        let store = TokenStore {
            access_token: "at".into(),
            refresh_token: "rt".into(),
            expires_at: 9999999999,
            username: "wangjinhan".into(),
            name: "王金涵".into(),
        };
        save_store(&path, &store).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let loaded = load_store(&path).unwrap();
        assert_eq!(loaded.access_token, "at");
        assert_eq!(loaded.username, "wangjinhan");
        // 损坏文件 → None（视同未登录）
        std::fs::write(&path, "{broken").unwrap();
        assert!(load_store(&path).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_gitlab_auth_header() {
        let (k, v) = GitlabAuth::Pat("tok".into()).header();
        assert_eq!((k, v.as_str()), ("PRIVATE-TOKEN", "tok"));
        let (k, v) = GitlabAuth::OAuth("tok".into()).header();
        assert_eq!((k, v.as_str()), ("Authorization", "Bearer tok"));
    }

    #[test]
    fn test_urlencoding() {
        assert_eq!(urlencoding("http://127.0.0.1:39859/callback"), "http%3A%2F%2F127.0.0.1%3A39859%2Fcallback");
        assert_eq!(urlencoding("a b"), "a%20b");
    }
}
