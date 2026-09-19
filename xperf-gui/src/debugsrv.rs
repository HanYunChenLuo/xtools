//! GUI 可编程调试接口（设计：`docs/DESIGN-gui-debug.md`）。
//!
//! **默认开启**（含 release；`XPERF_GUI_DEBUG=0` 关闭）：GUI 进程内嵌 axum HTTP
//! server，仅绑 `127.0.0.1` 随机端口，供脚本/coding agent 读取 UI 结构与监控
//! 状态、注入操作，替代脆弱的 CGEvent+截图目验。
//!
//! 安全模型：loopback + 每启动随机 token（请求须带 `X-Xperf-Token` 头）+ 发现
//! 文件 0600（`~/.config/xperf/gui-debug-<pid>.json` + 最新实例指针
//! `gui-debug.json`，均为 tmp+rename 原子写，退出清理、启动清扫陈旧条目）。
//!
//! 前端往返：需 DOM/JS 状态的请求经 Tauri 事件 `xperf-debug` 下发
//! `{id, op, params}`，main.js 调试钩子执行后经 `debug_respond` 命令回传，
//! 本模块 pending map 唤醒对应 HTTP 请求（每请求 [`FRONTEND_TIMEOUT`] 超时 → 504）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::oneshot;

/// 前端往返超时：超时回 504。前端钩子在 main.js 顶部注册，正常毫秒级应答；
/// 5s 未应答视为前端卡死/未加载。
const FRONTEND_TIMEOUT: Duration = Duration::from_secs(5);

/// 调试 server 状态（tauri manage 的是 `Mutex<Option<Arc<DebugState>>>` 槽位——
/// server 在 setup 阶段才拿得到 AppHandle，先放空槽位再由 setup 填充）。
pub(crate) struct DebugState {
    /// 鉴权 token（64 hex；发现文件 0600 存放，请求头 `X-Xperf-Token` 携带）
    token: String,
    /// 监听端口（127.0.0.1 随机分配）
    port: u16,
    /// server 启动时刻（uptime_s 计算）
    started: Instant,
    /// 启动时间戳字符串（发现文件 `started_at` 字段，RFC3339 本地时间）
    started_at: String,
    /// 本实例发现文件路径（`gui-debug-<pid>.json`）
    pid_file: PathBuf,
    /// 最新实例指针文件路径（`gui-debug.json`）
    latest_file: PathBuf,
    /// 前端就绪标志（main.js 调试钩子注册后经 `debug_frontend_ready` 命令置位；
    /// 就绪前依赖前端的端点大概率 504——消费方应轮询 `/api/status` 等 true）
    frontend_ready: AtomicBool,
    /// 前端往返 pending 表：请求 id → 应答通道（`debug_respond` 命令取出送达）
    pending: Mutex<HashMap<u64, oneshot::Sender<Value>>>,
    /// 请求 id 分配器
    next_id: AtomicU64,
}

/// tauri 托管的调试 server 槽位（`XPERF_GUI_DEBUG=0` 禁用或启动失败时为 None）
pub(crate) type DebugSlot = Mutex<Option<Arc<DebugState>>>;

/// axum handler 上下文
#[derive(Clone)]
struct Ctx {
    /// Tauri 句柄（emit 调试事件 + 读 AppState）；单测构造不了 AppHandle 为 None，
    /// 依赖它的端点回 503（单测只覆盖鉴权/参数校验/路由层）
    app: Option<AppHandle>,
    dbg: Arc<DebugState>,
}

/// 统一错误应答：`{ok:false, error}` + 对应 HTTP 状态码
fn err_json(status: StatusCode, msg: impl Into<String>) -> (StatusCode, Json<Value>) {
    (status, Json(json!({"ok": false, "error": msg.into()})))
}

/// 统一成功应答：`{ok:true, data}`
fn ok_json(data: Value) -> (StatusCode, Json<Value>) {
    (StatusCode::OK, Json(json!({"ok": true, "data": data})))
}

/// 生成 64 字符 hex token（32 字节加密随机）
fn gen_token() -> String {
    let mut buf = [0u8; 32];
    getrandom::getrandom(&mut buf).expect("getrandom 不可用");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// 配置目录（`$XDG_CONFIG_HOME/xperf` 或 `~/.config/xperf`；与 remotes.json 同目录）
fn config_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("xperf"))
}

/// 原子写 JSON 文件（tmp+rename），0600 权限（token 在内，仅属主可读）
fn write_discovery_file(path: &Path, v: &Value) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension("json.tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    use std::io::Write;
    f.write_all(serde_json::to_string_pretty(v).unwrap_or_default().as_bytes())?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)?;
    // rename 保留 tmp 的 0600；兼容已存在旧文件被覆盖的场景，显式再设一次
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// 清扫陈旧发现文件：pid 已死的 `gui-debug-<pid>.json` 与指向死 pid 的
/// `gui-debug.json` 一并删除（崩溃/SIGKILL 的实例无清理机会，由下次启动收编）
fn sweep_stale(dir: &Path, self_pid: u32) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let pid: Option<u32> = name
            .strip_prefix("gui-debug-")
            .and_then(|s| s.strip_suffix(".json"))
            .and_then(|s| s.parse().ok());
        match pid {
            Some(p) if p != self_pid && !xperf_core::utils::pid_alive(p) => {
                let _ = std::fs::remove_file(e.path());
            }
            _ => {}
        }
    }
    // 最新实例指针指向死实例则删除（新实例随即覆写，先删防窗口期误导）
    let latest = dir.join("gui-debug.json");
    if let Ok(s) = std::fs::read_to_string(&latest) {
        if let Ok(v) = serde_json::from_str::<Value>(&s) {
            let dead = v.get("pid").and_then(Value::as_u64).map(|p| p != self_pid as u64 && !xperf_core::utils::pid_alive(p as u32)).unwrap_or(false);
            if dead {
                let _ = std::fs::remove_file(&latest);
            }
        }
    }
}

/// 写发现文件（本实例 + 最新实例指针）。失败不致命（接口仍可用，只是难发现）。
fn write_discovery(dbg: &DebugState) {
    let v = json!({
        "pid": std::process::id(),
        "port": dbg.port,
        "token": dbg.token,
        "version": env!("CARGO_PKG_VERSION"),
        "started_at": dbg.started_at,
    });
    if let Err(e) = write_discovery_file(&dbg.pid_file, &v) {
        xperf_core::utils::diag(&format!("debug 发现文件写入失败: {e}"));
    }
    if let Err(e) = write_discovery_file(&dbg.latest_file, &v) {
        xperf_core::utils::diag(&format!("debug 最新指针写入失败: {e}"));
    }
}

/// 退出清理（CloseRequested 调用）：删本实例发现文件；最新指针指向自己才删
pub(crate) fn cleanup_files(slot: &DebugSlot) {
    let Some(dbg) = slot.lock().ok().and_then(|g| g.clone()) else { return };
    let _ = std::fs::remove_file(&dbg.pid_file);
    if let Ok(s) = std::fs::read_to_string(&dbg.latest_file) {
        if let Ok(v) = serde_json::from_str::<Value>(&s) {
            if v.get("pid").and_then(Value::as_u64) == Some(std::process::id() as u64) {
                let _ = std::fs::remove_file(&dbg.latest_file);
            }
        }
    }
}

/// 前端往返：emit `xperf-debug` 事件并等待 `debug_respond` 回传（超时 504）。
/// 前端应答 `{ok, data|error, kind?}` 原样透传为 HTTP 应答
/// （kind=not_found → 404，其余前端执行错误 → 400）。
async fn frontend_call(ctx: &Ctx, op: &str, params: Value) -> (StatusCode, Json<Value>) {
    let Some(app) = &ctx.app else {
        return err_json(StatusCode::SERVICE_UNAVAILABLE, "无 AppHandle（单测环境）");
    };
    if !ctx.dbg.frontend_ready.load(Ordering::SeqCst) {
        return err_json(
            StatusCode::SERVICE_UNAVAILABLE,
            "前端未就绪（轮询 /api/status 待 frontend_ready=true）",
        );
    }
    let id = ctx.dbg.next_id.fetch_add(1, Ordering::SeqCst);
    let (tx, rx) = oneshot::channel();
    ctx.dbg.pending.lock().unwrap().insert(id, tx);
    if let Err(e) = app.emit("xperf-debug", json!({"id": id, "op": op, "params": params})) {
        ctx.dbg.pending.lock().unwrap().remove(&id);
        return err_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("emit xperf-debug 失败: {e}"),
        );
    }
    match tokio::time::timeout(FRONTEND_TIMEOUT, rx).await {
        Ok(Ok(v)) => {
            let ok = v.get("ok").and_then(Value::as_bool).unwrap_or(false);
            if ok {
                ok_json(v.get("data").cloned().unwrap_or(Value::Null))
            } else {
                let msg = v
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("前端执行失败")
                    .to_string();
                let status = match v.get("kind").and_then(Value::as_str) {
                    Some("not_found") => StatusCode::NOT_FOUND,
                    _ => StatusCode::BAD_REQUEST,
                };
                err_json(status, msg)
            }
        }
        Ok(Err(_)) => err_json(StatusCode::INTERNAL_SERVER_ERROR, "前端应答通道关闭"),
        Err(_) => {
            ctx.dbg.pending.lock().unwrap().remove(&id);
            err_json(
                StatusCode::GATEWAY_TIMEOUT,
                format!("前端应答超时（{}s）", FRONTEND_TIMEOUT.as_secs()),
            )
        }
    }
}

/// token 鉴权中间件：`X-Xperf-Token` 头缺失/不符 → 401
async fn auth(State(dbg): State<Arc<DebugState>>, req: Request, next: Next) -> Response {
    let ok = req
        .headers()
        .get("x-xperf-token")
        .and_then(|v| v.to_str().ok())
        == Some(dbg.token.as_str());
    if ok {
        next.run(req).await
    } else {
        err_json(
            StatusCode::UNAUTHORIZED,
            "缺少或错误的 X-Xperf-Token 头（见 ~/.config/xperf/gui-debug-<pid>.json）",
        )
        .into_response()
    }
}

/// `GET /api/status`：后端进程内真相——版本/运行时长/remote/设备列表/每设备会话状态
async fn api_status(State(ctx): State<Ctx>) -> (StatusCode, Json<Value>) {
    let Some(app) = &ctx.app else {
        return err_json(StatusCode::SERVICE_UNAVAILABLE, "无 AppHandle（单测环境）");
    };
    let remote = crate::remote_status();
    let st = app.state::<crate::AppState>();
    let sessions: Vec<Value> = {
        let map = st.sessions.lock().unwrap();
        let building = st.agent_building.lock().unwrap();
        map.iter()
            .map(|(serial, s)| {
                json!({
                    "serial": serial,
                    "package": s.package,
                    "sampling": *s.running.lock().unwrap(),
                    "trace": *s.trace_running.lock().unwrap(),
                    "stack": *s.stack_running.lock().unwrap(),
                    "mirror": s.mirror.lock().unwrap().is_some(),
                    "recording": s.recorder.lock().unwrap().is_some(),
                    "logcat": s.logcat.lock().unwrap().is_some(),
                    "agent_building": building.get(serial).copied().unwrap_or(0) > 0,
                    "csv_dir": s.csv_dir.lock().unwrap().as_ref().map(|(_, d)| d.display().to_string()),
                })
            })
            .collect()
    };
    // adb 枚举含 bridge refresh，可能秒级——放 blocking 池，不占 runtime 线程
    let devices = tokio::task::spawn_blocking(xperf_core::list_adb_devices)
        .await
        .unwrap_or_else(|_| Ok(Vec::new()))
        .unwrap_or_default()
        .into_iter()
        .map(|d| {
            json!({
                "serial": d.serial,
                "model": d.model,
                "product": d.product,
                "android_version": d.android_version,
                "is_gateway": d.is_gateway,
            })
        })
        .collect::<Vec<_>>();
    ok_json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "pid": std::process::id(),
        "uptime_s": ctx.dbg.started.elapsed().as_secs(),
        "frontend_ready": ctx.dbg.frontend_ready.load(Ordering::SeqCst),
        "remote": remote,
        "devices": devices,
        "sessions": sessions,
    }))
}

/// eval 请求体
#[derive(serde::Deserialize)]
struct EvalReq {
    /// 待执行 JS 表达式（async 表达式自动 await）
    expr: String,
}

/// `POST /api/eval`：任意 JS 逃逸舱（前端执行，结果 JSON 序列化回传）
async fn api_eval(
    State(ctx): State<Ctx>,
    body: Result<Json<EvalReq>, axum::extract::rejection::JsonRejection>,
) -> (StatusCode, Json<Value>) {
    let Ok(Json(req)) = body else {
        return err_json(StatusCode::BAD_REQUEST, "body 须为 JSON：{\"expr\": \"...\"}");
    };
    if req.expr.trim().is_empty() {
        return err_json(StatusCode::BAD_REQUEST, "expr 不能为空");
    }
    frontend_call(&ctx, "eval", json!({"expr": req.expr})).await
}

/// `GET /api/ping`：前端就绪探针（走完整事件往返）
async fn api_ping(State(ctx): State<Ctx>) -> (StatusCode, Json<Value>) {
    frontend_call(&ctx, "ping", json!({})).await
}

/// `/api/dom` 查询参数
#[derive(serde::Deserialize)]
struct DomQuery {
    /// CSS 选择器（默认 `body`）
    selector: Option<String>,
    /// 子树深度（默认 4，硬顶 12）
    depth: Option<u32>,
    /// 节点数上限（默认 400，硬顶 2000）
    max_nodes: Option<u32>,
}

/// `GET /api/dom`：DOM 子树快照（tag/attrs/text/bounding rect/visible；前端执行）
async fn api_dom(
    State(ctx): State<Ctx>,
    axum::extract::Query(q): axum::extract::Query<DomQuery>,
) -> (StatusCode, Json<Value>) {
    frontend_call(
        &ctx,
        "dom",
        json!({"selector": q.selector, "depth": q.depth, "max_nodes": q.max_nodes}),
    )
    .await
}

/// `/api/state` 查询参数
#[derive(serde::Deserialize)]
struct StateQuery {
    /// 目标设备 serial（缺省 = 前端当前激活设备页；无激活页时只回 app 级概况）
    serial: Option<String>,
    /// logcat 尾行数（默认 50，硬顶 2000）
    logcat_tail: Option<u32>,
}

/// `GET /api/state`：监控状态快照（状态栏/实时数值/峰值/图表 series 摘要/悬停
/// tooltip/冷启动/logcat 尾行；前端执行）
async fn api_state(
    State(ctx): State<Ctx>,
    axum::extract::Query(q): axum::extract::Query<StateQuery>,
) -> (StatusCode, Json<Value>) {
    frontend_call(&ctx, "state", json!({"serial": q.serial, "logcat_tail": q.logcat_tail})).await
}

/// `/api/series` 查询参数
#[derive(serde::Deserialize)]
struct SeriesQuery {
    /// 目标设备 serial（缺省 = 激活设备页）
    serial: Option<String>,
    /// 图表名：cpu/mem/fps/freq/temp/gpu/gpumem/io/net（必填）
    metric: Option<String>,
    /// 尾 N 点（默认 100，硬顶 1000；与 `at` 互斥）
    tail: Option<u32>,
    /// 取该 epoch_ms 时刻的最近点（二分，与悬停取数同口径；给了就忽略 tail）
    at: Option<f64>,
}

/// `GET /api/series`：单图表全分辨率读数（前端 series 全量历史的尾部/定点取值）
async fn api_series(
    State(ctx): State<Ctx>,
    axum::extract::Query(q): axum::extract::Query<SeriesQuery>,
) -> (StatusCode, Json<Value>) {
    let Some(metric) = q.metric else {
        return err_json(
            StatusCode::BAD_REQUEST,
            "metric 必填（cpu/mem/fps/freq/temp/gpu/gpumem/io/net）",
        );
    };
    frontend_call(
        &ctx,
        "series",
        json!({"serial": q.serial, "metric": metric, "tail": q.tail, "at": q.at}),
    )
    .await
}

/// 组装路由（Ctx 注入；鉴权中间件经 `.layer` 挂整个 Router——route_layer 只覆盖
/// 已匹配路由，未知路径 404 会绕过鉴权，layer 则连 404 也先过 token 校验）
fn router(ctx: Ctx) -> Router {
    Router::new()
        .route("/api/status", get(api_status))
        .route("/api/ping", get(api_ping))
        .route("/api/eval", post(api_eval))
        .route("/api/dom", get(api_dom))
        .route("/api/state", get(api_state))
        .route("/api/series", get(api_series))
        .layer(middleware::from_fn_with_state(ctx.dbg.clone(), auth))
        .with_state(ctx)
}

/// 启动调试 server（`XPERF_GUI_DEBUG=0` 时返回 None 不启动）。
/// 独立 std 线程 + current_thread tokio runtime（不耦合 Tauri 内部 runtime）；
/// 绑定失败/发现文件失败均不致命（diag 记录），server 本身失败才返回 None。
pub(crate) fn start(app: &AppHandle, slot: &DebugSlot) {
    if std::env::var("XPERF_GUI_DEBUG").ok().as_deref() == Some("0") {
        xperf_core::utils::diag("debug api: XPERF_GUI_DEBUG=0，已禁用");
        return;
    }
    let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
        Ok(l) => l,
        Err(e) => {
            xperf_core::utils::diag(&format!("debug api: 绑定 loopback 失败: {e}"));
            return;
        }
    };
    if let Err(e) = listener.set_nonblocking(true) {
        xperf_core::utils::diag(&format!("debug api: set_nonblocking 失败: {e}"));
        return;
    }
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    let dir = config_dir().unwrap_or_else(|| PathBuf::from("."));
    let _ = std::fs::create_dir_all(&dir);
    let pid = std::process::id();
    sweep_stale(&dir, pid);
    let dbg = Arc::new(DebugState {
        token: gen_token(),
        port,
        started: Instant::now(),
        started_at: chrono::Local::now().to_rfc3339(),
        pid_file: dir.join(format!("gui-debug-{pid}.json")),
        latest_file: dir.join("gui-debug.json"),
        frontend_ready: AtomicBool::new(false),
        pending: Mutex::new(HashMap::new()),
        next_id: AtomicU64::new(1),
    });
    write_discovery(&dbg);
    *slot.lock().unwrap() = Some(dbg.clone());
    let ctx = Ctx { app: Some(app.clone()), dbg: dbg.clone() };
    let app_router = router(ctx);
    if let Err(e) = std::thread::Builder::new()
        .name("xperf-debug-api".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    xperf_core::utils::diag(&format!("debug api: runtime 构建失败: {e}"));
                    return;
                }
            };
            rt.block_on(async move {
                let listener = match tokio::net::TcpListener::from_std(listener) {
                    Ok(l) => l,
                    Err(e) => {
                        xperf_core::utils::diag(&format!("debug api: listener 转换失败: {e}"));
                        return;
                    }
                };
                if let Err(e) = axum::serve(listener, app_router).await {
                    xperf_core::utils::diag(&format!("debug api: serve 退出: {e}"));
                }
            });
        })
    {
        xperf_core::utils::diag(&format!("debug api: 线程启动失败: {e}"));
    }
    eprintln!("[debug-api] http://127.0.0.1:{port}（token 见 {}", dbg.pid_file.display());
    xperf_core::utils::diag(&format!("debug api: 127.0.0.1:{port}"));
}

/// `debug_respond` 命令：前端钩子回传执行结果（id 配对 pending 表唤醒等待方）
#[tauri::command]
pub(crate) fn debug_respond(
    slot: tauri::State<'_, DebugSlot>,
    id: u64,
    result: Value,
) -> Result<(), String> {
    let dbg = slot.lock().unwrap().clone().ok_or("debug api 未启用")?;
    let tx = dbg.pending.lock().unwrap().remove(&id);
    match tx {
        Some(tx) => {
            let _ = tx.send(result);
            Ok(())
        }
        // 超时后应答才到达属正常（pending 已清），不视为错误
        None => Ok(()),
    }
}

/// `debug_frontend_ready` 命令：前端调试钩子注册完成上报（/api/status 可观测）
#[tauri::command]
pub(crate) fn debug_frontend_ready(slot: tauri::State<'_, DebugSlot>) -> Result<(), String> {
    if let Some(dbg) = slot.lock().unwrap().clone() {
        dbg.frontend_ready.store(true, Ordering::SeqCst);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_state() -> Arc<DebugState> {
        Arc::new(DebugState {
            token: "test-token".into(),
            port: 0,
            started: Instant::now(),
            started_at: String::new(),
            pid_file: PathBuf::from("/tmp/nonexistent-gui-debug-0.json"),
            latest_file: PathBuf::from("/tmp/nonexistent-gui-debug.json"),
            frontend_ready: AtomicBool::new(false),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        })
    }

    async fn body_string(r: Response) -> String {
        let b = axum::body::to_bytes(r.into_body(), usize::MAX).await.unwrap();
        String::from_utf8(b.to_vec()).unwrap()
    }

    /// 无 token → 401；错 token → 401；正确 token → 非 401（无 AppHandle → 503）
    #[tokio::test]
    async fn auth_middleware_enforced() {
        use tower::ServiceExt;
        let dbg = test_state();
        let app = router(Ctx { app: None, dbg: dbg.clone() });
        let mk = |tok: Option<&str>| {
            let mut b = axum::http::Request::builder().uri("/api/status");
            if let Some(t) = tok {
                b = b.header("x-xperf-token", t);
            }
            b.body(axum::body::Body::empty()).unwrap()
        };
        let r = app.clone().oneshot(mk(None)).await.unwrap();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
        let r = app.clone().oneshot(mk(Some("wrong"))).await.unwrap();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
        let r = app.oneshot(mk(Some("test-token"))).await.unwrap();
        assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
        let s = body_string(r).await;
        assert!(s.contains("\"ok\":false"), "{s}");
    }

    /// 未知路径也须先过鉴权（401 优先于 404）
    #[tokio::test]
    async fn unknown_route_still_authed() {
        use tower::ServiceExt;
        let app = router(Ctx { app: None, dbg: test_state() });
        let r = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/nope")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    }

    /// eval 参数校验：空 expr → 400；非法 JSON → 400（均需正确 token）
    #[tokio::test]
    async fn eval_param_validation() {
        use tower::ServiceExt;
        let app = router(Ctx { app: None, dbg: test_state() });
        let mk = |body: &str| {
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/eval")
                .header("x-xperf-token", "test-token")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(body.to_string()))
                .unwrap()
        };
        let r = app.clone().oneshot(mk("{\"expr\":\"\"}")).await.unwrap();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        let r = app.oneshot(mk("not json")).await.unwrap();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    }

    /// 发现文件原子写 + 0600 权限 + 陈旧清扫（死 pid 删、活 pid 留）
    #[test]
    fn discovery_write_and_sweep() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("xperf-debugsrv-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("gui-debug-999999.json");
        write_discovery_file(&f, &json!({"pid": 999999u32, "port": 1})).unwrap();
        let mode = std::fs::metadata(&f).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        // 活 pid（本进程）写一份，sweep 须保留；死 pid 须删
        let alive = dir.join(format!("gui-debug-{}.json", std::process::id()));
        write_discovery_file(&alive, &json!({"pid": std::process::id()})).unwrap();
        sweep_stale(&dir, 424242);
        assert!(!f.exists(), "死 pid 文件应被清扫");
        assert!(alive.exists(), "活 pid 文件应保留");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn token_format() {
        let t = gen_token();
        assert_eq!(t.len(), 64);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(t, gen_token());
    }
}
