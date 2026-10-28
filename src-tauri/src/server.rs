//! 本地 HTTP 端点：只监听 127.0.0.1，所有请求需携带 token。

use agentsop_core::{
    AppError, DecisionAction, Evidence, Kernel, Snapshot,
};
use axum::{
    Json, Router,
    extract::{Path, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use tokio::net::TcpListener;

pub type KernelRef = Arc<Mutex<Kernel>>;

#[derive(Clone)]
pub struct AppState {
    pub kernel: KernelRef,
    pub token: Arc<String>,
    pub events: broadcast::Sender<Snapshot>,
}

impl AppState {
    pub fn new(kernel: KernelRef, token: String) -> Self {
        let (events, _) = broadcast::channel(64);
        AppState {
            kernel,
            token: Arc::new(token),
            events,
        }
    }

    fn emit(&self, snapshot: &Snapshot) {
        let _ = self.events.send(snapshot.clone());
    }
}

pub struct ApiError(pub AppError);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match self.0.code {
            "NOT_FOUND" => StatusCode::NOT_FOUND,
            "INVALID_TRANSITION" | "STATE_CONFLICT" => StatusCode::CONFLICT,
            "FORBIDDEN" | "UNAUTHORIZED" => StatusCode::UNAUTHORIZED,
            "INVALID_ARGUMENT" | "SOP_INVALID" | "BAD_JSON" => StatusCode::BAD_REQUEST,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, Json(self.0.to_json())).into_response()
    }
}

impl From<AppError> for ApiError {
    fn from(err: AppError) -> Self {
        ApiError(err)
    }
}

type ApiResult<T> = std::result::Result<T, ApiError>;

pub fn router(state: AppState) -> Router {
    let token = state.token.clone();
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/snapshot", get(snapshot))
        .route("/v1/runs", get(list_runs).post(create_run))
        .route("/v1/runs/{id}/pause", post(pause))
        .route("/v1/runs/{id}/resume", post(resume))
        .route("/v1/runs/{id}/abort", post(abort))
        .route("/v1/runs/{id}/session", post(bind_session))
        .route("/v1/context", get(context))
        .route("/v1/nodes/{id}/complete", post(complete_node))
        .route("/v1/decisions", post(decisions))
        .route("/v1/decisions/request", post(request_decision))
        .route("/v1/hooks", post(hook_generic))
        .route("/v1/hooks/{source}", post(hook_source))
        .route("/v1/sops", get(list_sops))
        .route("/v1/sops/{name}", get(get_sop).post(save_sop))
        .route("/v1/sops/{name}/validate", post(validate_sop))
        .with_state(state)
        .layer(middleware::from_fn_with_state(token, require_token))
}

async fn require_token(
    State(token): State<Arc<String>>,
    req: Request,
    next: Next,
) -> Response {
    let header = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let provided = header
        .strip_prefix("Bearer ")
        .or_else(|| req.headers().get("x-agentsop-token").and_then(|v| v.to_str().ok()))
        .unwrap_or("");
    if !constant_time_eq(provided.as_bytes(), token.as_bytes()) {
        return ApiError(AppError::new("UNAUTHORIZED", "缺少或错误的 token")).into_response();
    }
    next.run(req).await
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ---------------------------------------------------------------- handlers

async fn health(State(st): State<AppState>) -> Json<Value> {
    let kernel = st.kernel.lock().expect("kernel lock");
    Json(json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
        "pid": std::process::id(),
        "runs": kernel.run_ids().len(),
        "port": kernel.paths().port(),
    }))
}

async fn snapshot(
    State(st): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<SnapshotQuery>,
) -> ApiResult<Json<Snapshot>> {
    let kernel = st.kernel.lock().expect("kernel lock");
    let snap = resolve_snapshot(&kernel, q.run_id.as_deref())?;
    Ok(Json(snap))
}

#[derive(Deserialize)]
struct SnapshotQuery {
    run_id: Option<String>,
}

async fn list_runs(State(st): State<AppState>) -> ApiResult<Json<Value>> {
    let kernel = st.kernel.lock().expect("kernel lock");
    Ok(Json(json!({ "runs": kernel.all_snapshots() })))
}

#[derive(Deserialize)]
struct CreateRunBody {
    sop: String,
    #[serde(default)]
    repo: Option<String>,
    #[serde(default)]
    vars: std::collections::BTreeMap<String, String>,
    /// 只在这个 Agent 里跑；不给则按节点自带的 agent。
    #[serde(default)]
    agent: Option<String>,
    /// 直接绑定到这个会话；不给则等第一个 hook 自动接管。
    #[serde(default)]
    session_id: Option<String>,
}

async fn create_run(State(st): State<AppState>, Json(body): Json<CreateRunBody>) -> ApiResult<Json<Snapshot>> {
    let repo = body.repo.unwrap_or_else(|| {
        std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".into())
    });
    let mut kernel = st.kernel.lock().expect("kernel lock");
    let snap = kernel.start_run_bound(
        &body.sop,
        &repo,
        body.vars,
        body.agent.as_deref(),
        body.session_id.as_deref(),
    )?;
    drop(kernel);
    st.emit(&snap);
    Ok(Json(snap))
}

async fn pause(State(st): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Snapshot>> {
    let snap = st.kernel.lock().expect("kernel lock").pause(&id)?;
    st.emit(&snap);
    Ok(Json(snap))
}

async fn resume(State(st): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Snapshot>> {
    let snap = st.kernel.lock().expect("kernel lock").resume(&id)?;
    st.emit(&snap);
    Ok(Json(snap))
}

async fn abort(State(st): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Snapshot>> {
    let snap = st.kernel.lock().expect("kernel lock").abort(&id)?;
    st.emit(&snap);
    Ok(Json(snap))
}

#[derive(Deserialize)]
struct BindBody {
    session_id: String,
}

async fn bind_session(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<BindBody>,
) -> ApiResult<Json<Snapshot>> {
    let snap = st
        .kernel
        .lock()
        .expect("kernel lock")
        .bind_session(&id, &body.session_id)?;
    st.emit(&snap);
    Ok(Json(snap))
}

async fn context(
    State(st): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<SnapshotQuery>,
) -> ApiResult<Json<Value>> {
    let kernel = st.kernel.lock().expect("kernel lock");
    let snap = resolve_snapshot(&kernel, q.run_id.as_deref())?;
    let markdown = kernel.context_md(&snap.run_id)?;
    Ok(Json(json!({ "run_id": snap.run_id, "markdown": markdown })))
}

#[derive(Deserialize)]
struct CompleteBody {
    #[serde(default)]
    run_id: Option<String>,
    #[serde(default)]
    evidence: Vec<EvidenceBody>,
}

#[derive(Deserialize)]
struct EvidenceBody {
    #[serde(default = "kind_file")]
    kind: String,
    label: String,
    #[serde(default)]
    path: Option<String>,
}

fn kind_file() -> String {
    "file".into()
}

async fn complete_node(
    State(st): State<AppState>,
    Path(node_id): Path<String>,
    Json(body): Json<CompleteBody>,
) -> ApiResult<Json<Snapshot>> {
    let mut kernel = st.kernel.lock().expect("kernel lock");
    let run_id = resolve_run_id(&kernel, body.run_id.as_deref())?;
    let evidence = body
        .evidence
        .into_iter()
        .map(|e| Evidence {
            kind: e.kind,
            label: e.label,
            path: e.path,
            ts: agentsop_core::kernel::now_ts(),
        })
        .collect();
    let snap = kernel.complete_node(&run_id, &node_id, evidence)?;
    drop(kernel);
    st.emit(&snap);
    Ok(Json(snap))
}

#[derive(Deserialize)]
struct DecisionBody {
    #[serde(default)]
    run_id: Option<String>,
    node_id: String,
    action: DecisionAction,
    #[serde(default)]
    note: String,
}

async fn decisions(State(st): State<AppState>, Json(body): Json<DecisionBody>) -> ApiResult<Json<Snapshot>> {
    let mut kernel = st.kernel.lock().expect("kernel lock");
    let run_id = resolve_run_id(&kernel, body.run_id.as_deref())?;
    let snap = kernel.decide(&run_id, &body.node_id, body.action, &body.note)?;
    drop(kernel);
    st.emit(&snap);
    Ok(Json(snap))
}

#[derive(Deserialize)]
struct RequestDecisionBody {
    #[serde(default)]
    run_id: Option<String>,
    #[serde(default)]
    node_id: Option<String>,
    question: String,
    #[serde(default)]
    options: Option<Vec<String>>,
}

async fn request_decision(
    State(st): State<AppState>,
    Json(body): Json<RequestDecisionBody>,
) -> ApiResult<Json<Snapshot>> {
    let mut kernel = st.kernel.lock().expect("kernel lock");
    let run_id = resolve_run_id(&kernel, body.run_id.as_deref())?;
    let snap = kernel.request_decision(
        &run_id,
        body.node_id.as_deref(),
        &body.question,
        body.options,
    )?;
    drop(kernel);
    st.emit(&snap);
    Ok(Json(snap))
}

async fn hook_source(
    State(st): State<AppState>,
    Path(source): Path<String>,
    body: Option<Json<Value>>,
) -> ApiResult<Json<Value>> {
    let mut payload = body.map(|Json(v)| v).unwrap_or_else(|| json!({}));
    if let Some(map) = payload.as_object_mut() {
        map.insert("source".into(), Value::String(source));
    }
    run_hook(&st, payload)
}

async fn hook_generic(State(st): State<AppState>, Json(payload): Json<Value>) -> ApiResult<Json<Value>> {
    run_hook(&st, payload)
}

fn run_hook(st: &AppState, payload: Value) -> ApiResult<Json<Value>> {
    let mut kernel = st.kernel.lock().expect("kernel lock");
    let outcome = kernel.hook(payload)?;
    if let Some(snap) = outcome.snapshot.clone() {
        st.emit(&snap);
    }
    Ok(Json(outcome.response))
}

async fn list_sops(State(st): State<AppState>) -> ApiResult<Json<Value>> {
    let kernel = st.kernel.lock().expect("kernel lock");
    let names = kernel.paths().list_sops();
    let mut out = Vec::new();
    for name in names {
        if let Ok(sop) = kernel.paths().load_sop(&name) {
            out.push(json!({
                "name": sop.sop,
                "title": sop.display_title(),
                "version": sop.version,
                "nodes": sop.nodes.len(),
            }));
        }
    }
    Ok(Json(json!({ "sops": out })))
}

async fn get_sop(State(st): State<AppState>, Path(name): Path<String>) -> ApiResult<Json<Value>> {
    let kernel = st.kernel.lock().expect("kernel lock");
    let yaml = kernel.paths().read_sop_yaml(&name)?;
    let sop = kernel.paths().load_sop(&name)?;
    Ok(Json(json!({
        "name": sop.sop,
        "title": sop.display_title(),
        "version": sop.version,
        "yaml": yaml,
    })))
}

#[derive(Deserialize)]
struct SopBody {
    yaml: String,
}

async fn save_sop(
    State(st): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<SopBody>,
) -> ApiResult<Json<Value>> {
    let mut kernel = st.kernel.lock().expect("kernel lock");
    kernel.paths().save_sop(&name, &body.yaml)?;
    kernel.reload_sop(&name, &name)?;
    Ok(Json(json!({ "ok": true, "name": name })))
}

async fn validate_sop(Json(body): Json<SopBody>) -> ApiResult<Json<Value>> {
    match agentsop_core::sop::parse(&body.yaml) {
        Ok(sop) => Ok(Json(json!({
            "ok": true,
            "sop": sop.sop,
            "nodes": sop.nodes.len(),
            "edges": sop.edges.len(),
        }))),
        Err(err) => Err(ApiError(err)),
    }
}

// ---------------------------------------------------------------- 工具

fn resolve_run_id(kernel: &Kernel, explicit: Option<&str>) -> Result<String, AppError> {
    if let Some(id) = explicit {
        if kernel.run(id).is_ok() {
            return Ok(id.to_string());
        }
        return Err(AppError::not_found(format!("运行「{}」不存在", id)));
    }
    kernel
        .active_run_id()
        .ok_or_else(|| AppError::not_found("没有进行中的运行"))
}

fn resolve_snapshot(kernel: &Kernel, explicit: Option<&str>) -> Result<Snapshot, AppError> {
    let run_id = resolve_run_id(kernel, explicit)?;
    kernel.snapshot(&run_id)
}

/// 启动服务：优先使用端口文件里的端口，被占用时在 43117..43127 内顺延。
pub async fn serve(state: AppState, kernel_ref: KernelRef) -> Result<u16, AppError> {
    let preferred = {
        let kernel = kernel_ref.lock().expect("kernel lock");
        kernel.paths().port().unwrap_or(agentsop_core::store::DEFAULT_PORT)
    };
    let app = router(state);
    for offset in 0..agentsop_core::store::PORT_RANGE {
        let port = preferred.saturating_add(offset);
        let addr = format!("127.0.0.1:{}", port);
        match TcpListener::bind(&addr).await {
            Ok(listener) => {
                {
                    let kernel = kernel_ref.lock().expect("kernel lock");
                    let _ = kernel.paths().set_port(port);
                }
                axum::serve(listener, app).await.map_err(|e| {
                    AppError::new("SERVER_ERROR", format!("HTTP 服务退出：{}", e))
                })?;
                return Ok(port);
            }
            Err(_) => continue,
        }
    }
    Err(AppError::new("PORT_EXHAUSTED", "43117-43127 均被占用"))
}
