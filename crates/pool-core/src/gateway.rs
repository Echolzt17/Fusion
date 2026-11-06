use crate::diagnostics::{FailureStage, NetworkErrorKind, RequestDiagnostic, ResponseType};
use crate::{
    account::Accounts,
    scheduler::{binding_key, Lease, QueuePhase, Scheduler},
    RequestLog, Result,
};
use axum::{
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, RawQuery, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures_util::StreamExt;
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use tokio::{net::TcpListener, sync::Mutex, task::JoinHandle};
use tokio_util::sync::CancellationToken;

mod admission;
mod transport;
mod runtime;
type RequestTracking = Option<axum::Extension<runtime::RequestHandle>>;
pub(crate) mod continuity;
#[cfg(test)]
mod image_tests;
mod images;
#[cfg(test)]
mod risk_tests;
#[cfg(test)]
mod telemetry_tests;
#[cfg(test)]
mod network_tests;
#[cfg(test)]
mod connect_tests;
#[cfg(test)]
mod reliability_tests;
#[cfg(test)]
mod catalog_tests;

const MAX_EVENT_BYTES: usize = 64 * 1024 * 1024;
const IMAGE_TIMEOUT: Duration = Duration::from_secs(600);
// Queue 10s + authorization 50s + proxy 10s + headers 180s + body 180s.
// The same total applies to startup and ordinary model discovery, including a
// trickling body. Authorization rotation itself remains cancellation-safe.
const CATALOG_TOTAL: Duration = Duration::from_secs(430);
type CatalogProgress = Arc<dyn Fn(&'static str) + Send + Sync>;

#[derive(Clone)]
struct Context {
    requests: Arc<runtime::Registry>,
    admission: Arc<admission::Admission>,
    draining: CancellationToken,
    accounts: Arc<Accounts>,
    scheduler: Arc<Scheduler>,
    key: String,
    upstream: String,
    cancel: CancellationToken,
}
struct Running {
    context: Context,
    draining: CancellationToken,
    cancel: CancellationToken,
    task: JoinHandle<()>,
    port: u16,
}
pub struct Gateway {
    pub accounts: Arc<Accounts>,
    pub scheduler: Arc<Scheduler>,
    running: Mutex<Option<Running>>,
    #[cfg(test)]
    pub(crate) test_upstream: std::sync::Mutex<Option<String>>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceStatus {
    pub running: bool,
    pub draining: bool,
    pub active_requests: usize,
    pub base_url: String,
    pub storage_warning: Option<&'static str>,
    pub requests: Vec<runtime::ActiveRequest>,
}
impl Gateway {
    pub fn new(accounts: Arc<Accounts>) -> Arc<Self> {
        Arc::new(Self {
            scheduler: Scheduler::new(accounts.store.clone()),
            accounts,
            running: Mutex::new(None),
            #[cfg(test)]
            test_upstream: std::sync::Mutex::new(None),
        })
    }
    pub async fn status(&self) -> ServiceStatus {
        let r = self.running.lock().await;
        let port = r
            .as_ref()
            .map(|r| r.port)
            .unwrap_or_else(|| self.accounts.store.settings().unwrap_or_default().port);
        let requests = r.as_ref().map(|r| r.context.requests.snapshot()).unwrap_or_default();
        ServiceStatus {
            draining: r
                .as_ref()
                .is_some_and(|r| r.draining.is_cancelled() && !r.task.is_finished()),
            active_requests: requests.len(),
            running: r.as_ref().is_some_and(|r| !r.task.is_finished()),
            base_url: format!("http://127.0.0.1:{port}/v1"),
            storage_warning: self.accounts.store.persistence_warning(),
            requests,
        }
    }
    pub async fn start(&self) -> Result<ServiceStatus> {
        let mut running = self.running.lock().await;
        if running.as_ref().is_some_and(|r| !r.task.is_finished()) {
            return Err("服务已经运行".into());
        }
        let port = self.accounts.store.settings()?.port;
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .map_err(|_| format!("本机端口 {port} 被占用或无法监听；请修改端口后重试"))?;
        let cancel = CancellationToken::new();
        let draining = CancellationToken::new();
        let admission = Arc::new(admission::Admission::default());
        #[cfg(test)]
        let upstream = self
            .test_upstream
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| "https://chatgpt.com/backend-api/codex".into());
        #[cfg(not(test))]
        let upstream = "https://chatgpt.com/backend-api/codex".into();
        let ctx = Context {
            requests: Arc::new(runtime::Registry::default()),
            draining: draining.clone(),
            admission: admission.clone(),
            accounts: self.accounts.clone(),
            scheduler: self.scheduler.clone(),
            key: self.accounts.api_key()?,
            upstream,
            cancel: cancel.clone(),
        };
        let router = router(ctx.clone());
        let stop = draining.clone();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(stop.cancelled_owned())
                .await;
        });
        *running = Some(Running {
            context: ctx,
            cancel,
            draining,
            task,
            port,
        });
        drop(running);
        Ok(self.status().await)
    }
    pub async fn stop(&self) {
        let mut running = self.running.lock().await;
        if let Some(r) = running.take() {
            r.draining.cancel();
            r.cancel.cancel();
            let abort = r.task.abort_handle();
            if tokio::time::timeout(Duration::from_secs(3), r.task)
                .await
                .is_err()
            {
                abort.abort();
            }
        }
    }
    pub async fn drain(&self) {
        if let Some(r) = self.running.lock().await.as_ref() {
            r.draining.cancel();
        }
    }
    pub async fn cancel_request(&self, id: &str) -> Result<()> {
        let requests = self.running.lock().await.as_ref().map(|r| r.context.requests.clone());
        if let Some(requests) = requests { requests.cancel(id).await?; }
        Ok(())
    }
    pub async fn remove_account(&self, id: &str) -> Result<()> {
        let _reservation = self.scheduler.reserve_removal(id)?;
        self.accounts.remove(id).await
    }
    pub async fn check_models(&self, client_version: &str, progress: CatalogProgress) -> Result<()> {
        // Require the desktop layer's detected engine version. Never probe without
        // it, invent a fixed version or retry after an upstream rejection.
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("client_version", client_version).finish();
        catalog_version(Some(&query))?;
        let ctx = self.running.lock().await.as_ref().ok_or("服务未运行")?.context.clone();
        let mut h = HeaderMap::new();
        h.insert("authorization", format!("Bearer {}", ctx.key).parse().map_err(|_| "本地密钥无效")?);
        let response = catalog(ctx, h, Some(query), Some(progress)).await;
        if response.status().is_success() { return Ok(()); }
        let status = response.status().as_u16();
        // Read only our own finite error envelope, never an upstream body.
        let body = axum::body::to_bytes(response.into_body(), 16384).await.map_err(|_| "模型目录检查失败，未修改接入")?;
        let envelope = serde_json::from_slice::<Value>(&body).unwrap_or(Value::Null);
        let message = envelope["error"]["message"].as_str().unwrap_or("模型目录检查失败");
        let id = envelope["request_id"].as_str().unwrap_or("未知");
        Err(format!("模型目录检查失败（HTTP {status}，请求编号 {id}）：{message}；未修改接入"))
    }
}
fn router(ctx: Context) -> Router {
    Router::new()
        .route("/v1/models", get(models))
        .route("/v1/responses", post(responses))
        .route("/v1/responses/compact", post(compact))
        .route(
            "/v1/images/generations",
            post(images::generate).layer(DefaultBodyLimit::max(32 * 1024 * 1024)),
        )
        .route("/v1/images/edits", post(images::edit))
        .layer(DefaultBodyLimit::max(256 * 1024 * 1024))
        .route_layer(middleware::from_fn_with_state(
            ctx.clone(),
            authorize_before_body,
        ))
        .with_state(ctx)
}
fn failure(status: StatusCode, id: &str, code: &str, message: &str) -> Response {
    let mut r=(status,Json(json!({"error":{"message":message,"type":"codex_pool_error","code":code,"request_id":id,"retryable":false},"request_id":id}))).into_response();
    if let Ok(v) = id.parse() {
        r.headers_mut().insert("x-request-id", v);
    }
    r
}
fn authorized(ctx: &Context, h: &HeaderMap) -> bool {
    let supplied = h
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .unwrap_or("");
    bool::from(supplied.as_bytes().ct_eq(ctx.key.as_bytes()))
}
fn session(h: &HeaderMap) -> Option<String> {
    ["session_id", "x-codex-session-id", "conversation_id"]
        .iter()
        .find_map(|k| {
            h.get(*k)
                .and_then(|v| v.to_str().ok())
                .filter(|s| !s.is_empty())
                .map(|s| binding_key("session", s))
        })
}
fn opaque_keys(items: &Value) -> Vec<String> {
    items
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item["encrypted_content"].as_str())
        .map(|s| binding_key("opaque", s))
        .collect()
}
fn bind_output(ctx: &Context, value: &Value, account: &str) -> Result<()> {
    ctx.accounts
        .store
        .bind_many(&continuity::output_keys(value), account)
}
fn pinned(ctx: &Context, h: &HeaderMap, v: &Value) -> Result<Option<String>> {
    continuity::pin(ctx, h, v)
}
async fn authorize_before_body(
    State(ctx): State<Context>,
    mut req: axum::extract::Request,
    next: Next,
) -> Response {
    // For identity-preserving desktop access, never inspect its OpenAI bearer.
    // A supplied dedicated key is authoritative; invalid/duplicate keys cannot
    // fall back to Authorization. Old pool chats/checks retain local bearer auth.
    let valid = if req.headers().contains_key("x-fusion-client-key") {
        req.headers_mut().remove(http::header::AUTHORIZATION);
        let mut keys = req.headers().get_all("x-fusion-client-key").iter();
        keys.next().is_some_and(|v| bool::from(v.as_bytes().ct_eq(ctx.key.as_bytes())))
            && keys.next().is_none()
    } else { authorized(&ctx, req.headers()) };
    req.headers_mut().remove(http::header::AUTHORIZATION);
    req.headers_mut().remove("x-fusion-client-key");
    if !valid {
        return failure(
            StatusCode::UNAUTHORIZED,
            &uuid::Uuid::new_v4().to_string(),
            "invalid_local_key",
            "本地接入密钥无效",
        );
    }
    // Existing handlers recheck local authentication. Supply only Fusion's own
    // key after dropping both incoming credential headers above.
    let Ok(local) = http::HeaderValue::from_str(&format!("Bearer {}", ctx.key)) else {
        return failure(StatusCode::UNAUTHORIZED, &uuid::Uuid::new_v4().to_string(), "invalid_local_key", "本地接入密钥无效");
    };
    req.headers_mut().insert(http::header::AUTHORIZATION, local);
    admission::admit(&ctx, req, next).await
}
async fn request(
    audit: &mut Audit,
    lease: &Lease,
    t: &str,
    h: &HeaderMap,
    method: reqwest::Method,
    path: &str,
) -> std::result::Result<reqwest::RequestBuilder, NetworkFailure> {
    audit.phase("catalog_proxy");
    let endpoint = format!("{}{path}", audit.ctx.upstream);
    let (builder, source, retry_client) = tokio::select! {
        _ = audit.ctx.cancel.cancelled() => return Err(NetworkFailure { code: "service_stopped", stage: FailureStage::ServiceStopped, kind: None, message: "服务已停止，未发送上游请求".into() }),
        result = audit.ctx.accounts.http.gateway_request(method, &endpoint) => result.map_err(|message| NetworkFailure {
            code: "proxy_configuration", stage: FailureStage::ProxyResolution, kind: None, message,
        })?,
    };
    audit.diagnostic.proxy_source = source;
    audit.retry_client = Some(retry_client);
    let mut r = builder
        .bearer_auth(t)
        .header("ChatGPT-Account-ID", &lease.account.account_id)
        .header("originator", "codex_cli_rs")
        .header(
            "User-Agent",
            concat!("personal_codex_pool/", env!("CARGO_PKG_VERSION")),
        );
    for key in [
        "session_id",
        "conversation_id",
        "x-codex-turn-state",
        "x-codex-turn-metadata",
        "x-codex-image-turn-id",
        "x-codex-session-id",
        "x-codex-beta-features",
        "openai-beta",
    ] {
        if let Some(v) = h.get(key) {
            r = r.header(key, v);
        }
    }
    Ok(r)
}
#[cfg(test)]
async fn send(ctx: &Context, r: reqwest::RequestBuilder) -> std::result::Result<reqwest::Response, NetworkFailure> {
    send_timeout(ctx, r, Duration::from_secs(180)).await
}
#[cfg(test)]
async fn send_timeout(ctx: &Context, r: reqwest::RequestBuilder, timeout: Duration) -> std::result::Result<reqwest::Response, NetworkFailure> {
    transport::send_observed(ctx, r, None, timeout, Default::default(), None).await
}
fn transient_connect_failure(error: &reqwest::Error) -> bool {
    error.is_connect() && matches!(NetworkErrorKind::classify(error),
        NetworkErrorKind::ConnectionRefused | NetworkErrorKind::ConnectionReset
        | NetworkErrorKind::ConnectionAborted | NetworkErrorKind::ConnectionClosed
        | NetworkErrorKind::UnexpectedEof | NetworkErrorKind::Timeout)
}
#[derive(Debug)]
struct NetworkFailure {
    code: &'static str,
    stage: FailureStage,
    kind: Option<NetworkErrorKind>,
    message: String,
}
impl NetworkFailure {
    fn stopped() -> Self {
        Self { code: "service_stopped", stage: FailureStage::ServiceStopped, kind: None,
            message: "服务已停止，未自动重放请求".into() }
    }
    fn timeout(stage: FailureStage) -> Self {
        let reading = stage == FailureStage::ResponseRead;
        Self { code: if reading { "upstream_read_timeout" } else { "upstream_header_timeout" },
            stage, kind: Some(NetworkErrorKind::Timeout),
            message: match stage {
                FailureStage::Connect => "建立上游连接超时；未收到上游响应，执行状态未确认，未自动重放",
                FailureStage::ResponseRead => "读取上游响应超时；执行状态未确认，未自动重放",
                _ => "等待上游响应头超时；执行状态未确认，未自动重放",
            }.into() }
    }
    fn transport(error: &reqwest::Error, reading: bool) -> Self {
        let stage = if reading { FailureStage::ResponseRead } else if error.is_connect() {
            FailureStage::Connect
        } else { FailureStage::ResponseHeaders };
        if error.is_timeout() { return Self::timeout(stage) }
        let kind = NetworkErrorKind::classify(error);
        Self { code: if reading { "read_error" } else { "network_error" }, stage, kind: Some(kind),
            message: format!("{}（{}）；执行状态未确认，未回退直连、未自动重放请求",
                if reading { "读取上游响应失败" } else { "上游请求失败" }, kind.description()) }
    }
}
fn response_type(r: &reqwest::Response) -> ResponseType {
    match r
        .headers()
        .get("content-type")
        .and_then(|h| h.to_str().ok())
        .map(|s| {
            s.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        })
        .as_deref()
    {
        None => ResponseType::Missing,
        Some("text/event-stream") => ResponseType::Sse,
        Some("text/html" | "application/xhtml+xml") => ResponseType::Html,
        Some("application/json") => ResponseType::Json,
        Some(_) => ResponseType::Other,
    }
}
#[cfg(test)]
async fn read_budget(
    ctx: &Context,
    r: reqwest::Response,
    limit: usize,
    idle: Duration,
) -> std::result::Result<Bytes, NetworkFailure> {
    read_budget_activity(ctx, r, limit, idle, |_| {}).await
}
async fn read_budget_activity(
    ctx: &Context, r: reqwest::Response, limit: usize, idle: Duration, mut activity: impl FnMut(usize),
) -> std::result::Result<Bytes, NetworkFailure> {
    let mut stream = r.bytes_stream();
    let mut out = Vec::new();
    loop {
        let next = tokio::select! {_=ctx.cancel.cancelled()=>return Err(NetworkFailure::stopped()),r=tokio::time::timeout(idle,stream.next())=>r.map_err(|_|NetworkFailure::timeout(FailureStage::ResponseRead))?};
        match next {
            None => break,
            Some(Err(e)) => return Err(NetworkFailure::transport(&e, true)),
            Some(Ok(b)) => {
                if !b.is_empty() { activity(b.len()); }
                if out.len() + b.len() > limit {
                    return Err(NetworkFailure { code: "read_error", stage: FailureStage::ResponseRead,
                        kind: None, message: "上游响应超过大小限制".into() });
                }
                out.extend_from_slice(&b);
            }
        }
    }
    Ok(out.into())
}
struct Audit {
    handoff: Option<Arc<std::sync::atomic::AtomicBool>>,
    tracking: Option<runtime::RequestHandle>,
    catalog_progress: Option<CatalogProgress>,
    ctx: Context,
    id: String,
    account: Option<String>,
    conversation_key: Option<String>,
    started_at: i64,
    requested_model: Option<String>,
    response_model: Option<String>,
    operation: String,
    start: Instant,
    status: String,
    advice: String,
    usage: Option<crate::usage::TokenUsage>,
    transport: transport::Telemetry,
    retry_client: Option<reqwest::Client>,
    diagnostic: RequestDiagnostic,
    phase_name: &'static str,
    phase_at: Instant,
    upstream_at: Option<Instant>,
    read_at: Option<Instant>,
}
impl Audit {
    fn new(ctx: Context, op: &str) -> Self {
        let diagnostic = RequestDiagnostic {
            details: None,
            execution: Some(fusion_report_protocol::Execution { active_requests: Some(ctx.requests.snapshot().len() as u32), ..Default::default() }),
            app_version: env!("CARGO_PKG_VERSION").into(),
            upstream_status: None,
            response_type: None,
            detected_response_type: None,
            failure_stage: None,
            network_error_kind: None,
            proxy_source: crate::diagnostics::ProxySource::Unknown,
        };
        Self {
            handoff: None,
            tracking: None,
            catalog_progress: None,
            ctx,
            id: uuid::Uuid::new_v4().to_string(),
            account: None,
            conversation_key: None,
            started_at: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as i64,
            requested_model: None,
            response_model: None,
            operation: op.into(),
            start: Instant::now(),
            status: "cancelled".into(),
            advice: "请求被客户端取消或服务停止；上游任务是否停止无法保证，未自动重放".into(),
            usage: None,
            transport: Default::default(),
            retry_client: None,
            diagnostic,
            phase_name: "initial",
            phase_at: Instant::now(),
            upstream_at: None,
            read_at: None,
        }
    }
    async fn send(&mut self, r: reqwest::RequestBuilder, timeout: Duration) -> std::result::Result<reqwest::Response, NetworkFailure> {
        transport::send_observed(&self.ctx, r, self.retry_client.clone(), timeout, self.transport.clone(), self.tracking.clone()).await
    }
    fn observe_events(&mut self, obs: &Observer) {
        let mut t = self.transport.lock().unwrap();
        t.output_event_observed |= obs.output_event_observed;
        t.tool_call_observed |= obs.tool_call_observed;
    }
    fn finish_phase_timing(&mut self) {
        let elapsed = self.phase_at.elapsed().as_millis() as u64;
        if let Some(x) = self.diagnostic.execution.as_mut() {
            match self.phase_name {
                "ingress_queue" => x.ingress_queue_ms = Some(elapsed),
                "catalog_queue" => x.queue_ms = Some(elapsed),
                "catalog_auth" => x.authorization_ms = Some(elapsed),
                "catalog_headers" => x.headers_wait_ms = Some(elapsed),
                _ => {}
            }
        }
    }
    fn phase(&mut self, phase: &'static str) {
        self.finish_phase_timing();
        if phase == "catalog_headers" {
            self.upstream_at = Some(Instant::now());
            if let Some(x) = self.diagnostic.execution.as_mut() { x.upstream_attempted = true; }
        }
        if phase == "streaming" { self.body_activity(); }
        self.phase_name = phase;
        self.phase_at = Instant::now();
        if let Some(progress) = &self.catalog_progress { progress(phase); }
        if let Some(request) = &self.tracking {
            request.phase(match phase {
                "catalog_queue" => "queued", "catalog_auth" => "authorization",
                "catalog_proxy" => "proxy", "catalog_headers" => "headers",
                "catalog_body" => "reading", _ => phase,
            });
        }
    }
    fn tracked(ctx: Context, operation: &str, tracking: RequestTracking) -> Self {
        let mut audit = Self::new(ctx, operation);
        if let Some(axum::Extension(request)) = tracking {
            request.audit_claimed.store(true, std::sync::atomic::Ordering::Release);
            audit.id.clone_from(&request.id);
            audit.start=request.started;
            audit.started_at=request.started_at;
            if let Some(x)=audit.diagnostic.execution.as_mut() { x.ingress_queue_ms=request.ingress_queue_ms; }
            audit.tracking = Some(request);
        }
        audit
    }
    fn error(&mut self, status: StatusCode, code: &str, msg: &str) -> Response {
        if let Some(x) = self.diagnostic.execution.as_mut() { x.local_http_status = Some(status.as_u16()); }
        if let Some(request) = &self.tracking { request.finish_upload(); }
        self.status = code.into();
        self.advice = msg.into();
        if self.diagnostic.failure_stage.is_none() {
            self.diagnostic.failure_stage = Some(match code {
                "service_stopped" => FailureStage::ServiceStopped,
                "stream_invalid" | "unexpected_content_type" | "invalid_image_response" => {
                    FailureStage::EventValidation
                }
                "read_error" | "stream_interrupted" | "upstream_read_timeout" => {
                    FailureStage::ResponseRead
                }
                _ => FailureStage::Unknown,
            });
        }
        failure(status, &self.id, code, msg)
    }
    fn network_error(&mut self, e: NetworkFailure) -> Response {
        self.diagnostic.failure_stage = Some(e.stage);
        self.diagnostic.network_error_kind = e.kind;
        self.error(
            if e.code == "service_stopped" {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::BAD_GATEWAY
            },
            e.code,
            &e.message,
        )
    }
    fn received(&mut self, response: &reqwest::Response) {
        if let Some(x) = self.diagnostic.execution.as_mut() { x.headers_received = true; }
        self.read_at = Some(Instant::now());
        if let Some(request) = &self.tracking { request.finish_upload(); }
        self.phase("reading");
        self.diagnostic.upstream_status = Some(response.status().as_u16());
        self.diagnostic.response_type = Some(response_type(response));
    }
    fn read_error(&mut self, error: NetworkFailure) -> Response {
        self.diagnostic.failure_stage = Some(error.stage);
        self.diagnostic.network_error_kind = error.kind;
        self.error(StatusCode::BAD_GATEWAY, error.code, &error.message)
    }
    fn note_stream_failure(&mut self, kind: NetworkErrorKind) {
        self.status = if kind == NetworkErrorKind::Timeout { "upstream_read_timeout" }
            else { "stream_interrupted" }.into();
        self.diagnostic.failure_stage = Some(FailureStage::ResponseRead);
        self.diagnostic.network_error_kind = Some(kind);
        self.advice = format!("上游响应中断（{}）；执行状态无法确认，未自动重放", kind.description());
    }
    fn success(&mut self) {
        if let Some(x) = self.diagnostic.execution.as_mut() { x.terminal_received = true; x.local_http_status.get_or_insert(200); }
        self.phase("finishing");
        self.status = "success".into();
        self.advice.clear();
        self.diagnostic.failure_stage = None;
        self.diagnostic.network_error_kind = None;
    }
    fn body_activity(&mut self) {
        let now = Instant::now();
        if let Some(x) = self.diagnostic.execution.as_mut() {
            x.first_byte_received = true;
            if x.first_byte_ms.is_none() { x.first_byte_ms = self.upstream_at.map(|t| now.duration_since(t).as_millis() as u64); }
            if let Some(last) = self.read_at { x.max_read_idle_ms = Some(x.max_read_idle_ms.unwrap_or(0).max(now.duration_since(last).as_millis() as u64)); }
        }
        self.read_at = Some(now);
    }
    async fn read_response(&mut self, r: reqwest::Response, limit: usize, idle: Duration) -> std::result::Result<Bytes, NetworkFailure> {
        let ctx = self.ctx.clone();
        read_budget_activity(&ctx, r, limit, idle, |n| {
            self.transport.lock().unwrap().response_bytes += n as u64;
            self.body_activity();
        }).await
    }
}
impl Drop for Audit {
    fn drop(&mut self) {
        if self.handoff.as_ref().is_some_and(|claimed| claimed.load(std::sync::atomic::Ordering::Acquire)) { return; }
        // Finish the active phase, including timeouts and cancellation. No extra network calls.
        self.finish_phase_timing();
        if let (Some(last), Some(x)) = (self.read_at, self.diagnostic.execution.as_mut()) {
            x.max_read_idle_ms = Some(x.max_read_idle_ms.unwrap_or(0).max(last.elapsed().as_millis() as u64));
        }
        if self.status == "cancelled" {
            self.diagnostic.failure_stage = Some(if self.ctx.cancel.is_cancelled() {
                self.status = "service_stopped".into();
                FailureStage::ServiceStopped
            } else {
                FailureStage::Cancelled
            });
        }
        if let Some(x) = self.diagnostic.execution.as_mut() { x.transport = Some(self.transport.lock().unwrap().clone()); }
        let origin = crate::diagnostics::failure_origin(&self.status, &self.diagnostic);
        self.diagnostic.details.get_or_insert_with(Default::default).origin = Some(origin.into());
        let _ = self.ctx.accounts.store.log(&RequestLog {
            id: self.id.clone(),
            at: crate::now(),
            account: self.account.clone(),
            conversation_key: self.conversation_key.clone(),
            started_at: Some(self.started_at),
            requested_model: self.requested_model.clone(),
            response_model: self.response_model.clone(),
            operation: self.operation.clone(),
            status: self.status.clone(),
            elapsed_ms: self.start.elapsed().as_millis() as u64,
            advice: self.advice.clone(),
            usage: self.usage.clone(),
            diagnostic: Some(self.diagnostic.clone()),
        });
    }
}
async fn prepare(
    a: &mut Audit,
    h: &HeaderMap,
    v: &Value,
) -> std::result::Result<(Lease, String), Box<Response>> {
    if !authorized(&a.ctx, h) {
        return Err(Box::new(a.error(
            StatusCode::UNAUTHORIZED,
            "invalid_local_key",
            "本地接入密钥无效，请检查 Codex provider 配置",
        )));
    }
    // Keep the same hashed identity used for continuity; do not alter routing or retries.
    a.conversation_key = continuity::identities(h, v).ok().and_then(|keys| {
        keys.into_iter()
            .find(|key| key.starts_with("thread:") || key.starts_with("session:"))
    });
    a.requested_model = crate::usage::model_name(&v["model"]);
    if a.operation != "models" {
        a.ctx.accounts.store.note_request_started(crate::storage::history::RecentRequest {
            id: a.id.clone(), conversation_key: a.conversation_key.clone(), started_at: a.started_at, account: None,
        });
    }
    let pin = match pinned(&a.ctx, h, v) {
        Ok(p) => p,
        Err(e) => {
            return Err(Box::new(a.error(
                StatusCode::CONFLICT,
                "continuity_unavailable",
                &e,
            )))
        }
    };
    if let Some(request) = &a.tracking {
        request.identify(pin.as_deref(), a.conversation_key.as_deref());
    }
    let scheduler = a.ctx.scheduler.clone();
    let image = matches!(a.operation.as_str(), "image_generate" | "image_edit");
    a.phase("catalog_queue");
    let tracking = a.tracking.clone();
    let acquire = async {
        if a.operation == "models" {
            scheduler.acquire_catalog(pin.as_deref()).await
        } else {
            scheduler.acquire_observed(pin.as_deref(), image, move |phase| {
                if let Some(request) = &tracking {
                    request.phase(match phase {
                        QueuePhase::Capacity => "queued",
                        QueuePhase::Cooldown => "cooldown",
                    });
                }
            }).await
        }
    };
    let lease = tokio::select! {_=a.ctx.cancel.cancelled()=>return Err(Box::new(a.error(StatusCode::SERVICE_UNAVAILABLE,"service_stopped","服务已停止"))),r=acquire=>match r{Ok(l)=>l,Err(e)=>{a.diagnostic.failure_stage=Some(FailureStage::Queue);return Err(Box::new(a.error(StatusCode::SERVICE_UNAVAILABLE,"account_unavailable",&e)))}}};
    a.account = Some(lease.account.id.clone());
    if let Some(request) = &a.tracking {
        request.identify(a.account.as_deref(), a.conversation_key.as_deref());
    }
    a.ctx.accounts.store.note_request_account(&a.id, &lease.account.id);
    // A detached credential rotation may finish safely; cancellation must still release this request's lease.
    a.phase("catalog_auth");
    let token_request = async {
        if a.operation == "models" {
            tokio::time::timeout(Duration::from_secs(50), a.ctx.accounts.token(&lease.account.id)).await
                .map_err(|_| "模型目录授权等待超时，未发送目录请求".to_owned())?
        } else { a.ctx.accounts.token(&lease.account.id).await }
    };
    let token_result = tokio::select! {
        _ = a.ctx.cancel.cancelled() => return Err(Box::new(a.error(StatusCode::SERVICE_UNAVAILABLE, "service_stopped", "服务已停止"))),
        result = token_request => result,
    };
    let token = match token_result {
        Ok(t) => t.access_token,
        Err(e) => {
            return Err(Box::new(a.error(
                StatusCode::SERVICE_UNAVAILABLE,
                "authorization_unavailable",
                &e,
            )))
        }
    };
    let keys = match continuity::identities(h, v) {
        Ok(k) => k,
        Err(e) => {
            return Err(Box::new(a.error(
                StatusCode::CONFLICT,
                "continuity_unavailable",
                &e,
            )))
        }
    };
    if let Err(e) = a.ctx.accounts.store.bind_many(&keys, &lease.account.id) {
        return Err(Box::new(a.error(
            StatusCode::CONFLICT,
            "continuity_unavailable",
            &e,
        )));
    }
    Ok((lease, token))
}
async fn rejected(a: &mut Audit, lease: &Lease, r: reqwest::Response) -> Response {
    a.received(&r);
    a.diagnostic.failure_stage = Some(FailureStage::UpstreamRejected);
    let status = r.status();
    let delay = r
        .headers()
        .get("retry-after")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(60);
    let _ = a
        .ctx
        .accounts
        .mark_failure(&lease.account.id, status.as_u16(), delay)
        .await;
    let (code, msg) = match status.as_u16() {
        401 => (
            "reauthorization_required",
            "上游拒绝了授权，请重新登录该账号",
        ),
        429 => ("upstream_rate_limited", "账号已限流并进入冷却，请稍后重试"),
        400 if a.operation == "models" => (
            "upstream_bad_request",
            "上游拒绝模型目录请求，请检查客户端版本与目录接口兼容性",
        ),
        400 => (
            "upstream_bad_request",
            "上游不接受该请求，或上下文无法续接；请查看模型和输入设置",
        ),
        403 => (
            "upstream_forbidden",
            "访问被拒绝，具体来源未确认；可能涉及接入权限或网络出口策略，未自动重放",
        ),
        _ => ("upstream_error", "上游服务返回错误，未自动重放请求"),
    };
    let mut response = a.error(status, code, msg);
    copy_image_headers(r.headers(), response.headers_mut());
    response
}
fn copy_image_headers(source: &HeaderMap, target: &mut HeaderMap) {
    for (name, value) in source {
        let n = name.as_str();
        if matches!(
            n,
            "retry-after" | "x-codex-imagegen-request-id" | "x-codex-active-limit"
        ) || ((n.starts_with("x-codex-primary-") || n.starts_with("x-codex-secondary-"))
            && matches!(n.rsplit('-').next(), Some("percent" | "seconds" | "at"))
            && value
                .to_str()
                .ok()
                .is_some_and(|s| s.parse::<f64>().is_ok_and(f64::is_finite)))
        {
            target.insert(name.clone(), value.clone());
        }
    }
}
async fn models(State(ctx): State<Context>, h: HeaderMap, RawQuery(query): RawQuery, tracking: RequestTracking) -> Response {
    catalog_tracked(ctx, h, query, None, CATALOG_TOTAL, tracking).await
}
fn catalog_version(query: Option<&str>) -> Result<Option<String>> {
    let mut version = None;
    for (key, value) in url::form_urlencoded::parse(query.unwrap_or_default().as_bytes()) {
        if key != "client_version" { continue; }
        if version.is_some() || value.is_empty() || value.len() > 64
            || !value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'+')) {
            return Err("客户端版本参数无效".into());
        }
        version = Some(value.into_owned());
    }
    Ok(version)
}
async fn catalog(ctx: Context, h: HeaderMap, query: Option<String>, progress: Option<CatalogProgress>) -> Response {
    catalog_with_budget(ctx, h, query, progress, CATALOG_TOTAL).await
}
async fn catalog_with_budget(ctx: Context, h: HeaderMap, query: Option<String>, progress: Option<CatalogProgress>, budget: Duration) -> Response {
    catalog_tracked(ctx, h, query, progress, budget, None).await
}
async fn catalog_tracked(ctx: Context, h: HeaderMap, query: Option<String>, progress: Option<CatalogProgress>, budget: Duration, tracking: RequestTracking) -> Response {
    let mut a = Audit::tracked(ctx, "models", tracking);
    a.catalog_progress = progress;
    let version = match catalog_version(query.as_deref()) {
        Ok(v) => v,
        Err(e) => return a.error(StatusCode::BAD_REQUEST, "invalid_client_version", &e),
    };
    match tokio::time::timeout(budget, catalog_inner(&mut a, &h, version)).await {
        Ok(response) => response,
        Err(_) => a.error(StatusCode::GATEWAY_TIMEOUT, "catalog_timeout", "模型目录检查超过总时限，已取消等待，未自动重放"),
    }
}
async fn catalog_inner(a: &mut Audit, h: &HeaderMap, version: Option<String>) -> Response {
    let (lease, t) = match prepare(a, h, &Value::Null).await {
        Ok(v) => v,
        Err(r) => return *r,
    };
    a.phase("catalog_proxy");
    let r = match request(a, &lease, &t, h, reqwest::Method::GET, "/models").await {
        Ok(r) => if let Some(version) = version { r.query(&[("client_version", version)]) } else { r },
        Err(e) => return a.network_error(e),
    };
    a.phase("catalog_headers");
    let r = match a.send(r, Duration::from_secs(180)).await {
        Ok(r) => r,
        Err(e) => return a.network_error(e),
    };
    a.received(&r);
    if !r.status().is_success() {
        return rejected(a, &lease, r).await;
    }
    a.phase("catalog_body");
    let body = tokio::time::timeout(Duration::from_secs(180), a.read_response(r, 32 * 1024 * 1024, Duration::from_secs(180))).await
        .unwrap_or_else(|_| Err(NetworkFailure::timeout(FailureStage::ResponseRead)));
    let b = match body {
        Ok(b) => b,
        Err(e) => return a.read_error(e),
    };
    let v: Value = match serde_json::from_slice(&b) {
        Ok(v) => v,
        Err(_) => {
            return a.error(
                StatusCode::BAD_GATEWAY,
                "invalid_catalog",
                "上游模型目录格式不兼容",
            )
        }
    };
    a.diagnostic.detected_response_type = Some(ResponseType::Json);
    let items = v["models"].as_array().or_else(|| v["data"].as_array());
    let Some(items) = items else {
        return a.error(
            StatusCode::BAD_GATEWAY,
            "invalid_catalog",
            "上游模型目录缺少模型列表",
        );
    };
    let data: Vec<_> = items
        .iter()
        .filter_map(|m| m["slug"].as_str().or_else(|| m["id"].as_str()))
        .map(|id| json!({"id":id,"object":"model","created":0,"owned_by":"openai"}))
        .collect();
    a.success();
    Json(json!({"object":"list","data":data,"models":items})).into_response()
}
async fn compact(State(ctx): State<Context>, h: HeaderMap, tracking: RequestTracking, Json(v): Json<Value>) -> Response {
    let mut a = Audit::tracked(ctx, "compact", tracking);
    let (lease, t) = match prepare(&mut a, &h, &v).await {
        Ok(v) => v,
        Err(r) => return *r,
    };
    let r = match request(
        &mut a,
        &lease,
        &t,
        &h,
        reqwest::Method::POST,
        "/responses/compact",
    ).await {
        Ok(r) => r.json(&v),
        Err(e) => return a.network_error(e),
    };
    drop(v);
    a.phase("catalog_headers");
    let r = match a.send(r, Duration::from_secs(180)).await {
        Ok(r) => r,
        Err(e) => return a.network_error(e),
    };
    a.received(&r);
    if !r.status().is_success() {
        return rejected(&mut a, &lease, r).await;
    }
    let b = match a.read_response(r, 32 * 1024 * 1024, Duration::from_secs(180)).await {
        Ok(b) => b,
        Err(e) => return a.read_error(e),
    };
    let value: Value = match serde_json::from_slice(&b) {
        Ok(v) => v,
        Err(_) => {
            return a.error(
                StatusCode::BAD_GATEWAY,
                "invalid_compaction",
                "上游压缩响应格式错误",
            )
        }
    };
    a.diagnostic.detected_response_type = Some(ResponseType::Json);
    if let Err(e) = bind_output(&a.ctx, &value, &lease.account.id) {
        return a.error(StatusCode::INTERNAL_SERVER_ERROR, "storage_error", &e);
    }
    a.response_model = crate::usage::model_name(&value["model"]);
    a.usage = crate::usage::TokenUsage::from_response(&value);
    a.success();
    ([(http::header::CONTENT_TYPE, "application/json")], b).into_response()
}

// SSE frames are parsed as bytes so split UTF-8 and CRLF boundaries do not corrupt output.
#[derive(Default)]
struct Observer {
    buffer: Vec<u8>,
    validated_sse: bool,
    completed: Option<Value>,
    terminal: bool,
    failed: bool,
    usage: Option<crate::usage::TokenUsage>,
    response_model: Option<String>,
    streaming: bool,
    output_event_observed: bool,
    tool_call_observed: bool,
    failure_signal: Option<(u16, i64)>,
    terminal_event: Option<&'static str>,
    failure_class: Option<&'static str>,
}
impl Observer {
    fn push(&mut self, b: &[u8], ctx: &Context, account: &str) -> Result<()> {
        // Incremental delimiter scanning: bounded by one event, independent of transport chunks.
        for byte in b {
            self.buffer.push(*byte);
            if self.buffer.len() > MAX_EVENT_BYTES {
                return Err("上游单条流事件超过限制".into());
            }
            if !self.buffer.ends_with(b"\n\n") && !self.buffer.ends_with(b"\r\n\r\n") {
                continue;
            }
            let frame = std::mem::take(&mut self.buffer);
            let text = std::str::from_utf8(&frame).map_err(|_| "上游流包含无效编码")?;
            let data = text
                .lines()
                .filter_map(|l| l.strip_prefix("data:").map(str::trim_start))
                .collect::<Vec<_>>()
                .join("\n");
            if let Ok(mut v) = serde_json::from_str::<Value>(&data) {
                self.output_event_observed |= matches!(v["type"].as_str(), Some("response.output_text.delta" | "response.output_text.done" | "response.reasoning_text.delta" | "response.reasoning_summary_text.delta" | "response.output_audio.delta"))
                    || v["response"]["output"].as_array().is_some_and(|items| items.iter().any(|item| item["type"]=="message"))
                    || v["item"]["type"]=="message";
                self.tool_call_observed |= v["response"]["output"].as_array().is_some_and(|items| items.iter().any(|item|item["type"].as_str().is_some_and(|t| t.ends_with("_call"))))
                    || v["item"]["type"].as_str().is_some_and(|t| t.ends_with("_call"))
                    || matches!(v["type"].as_str(), Some("response.function_call_arguments.delta" | "response.function_call_arguments.done"));
                self.validated_sse |= v["type"].as_str()
                    .is_some_and(|t| t == "error" || t.starts_with("response."));
                if v["type"].as_str().is_some_and(|t| t.starts_with("response.")) {
                    if let Some(model) = crate::usage::model_name(&v["response"]["model"]) { self.response_model = Some(model); }
                }
                if matches!(
                    v["type"].as_str(),
                    Some("response.completed" | "response.failed" | "response.incomplete")
                ) {
                    self.usage = crate::usage::TokenUsage::from_response(&v["response"]);
                }
                if let Some(id) = v["response"]["id"].as_str() {
                    ctx.accounts
                        .store
                        .bind(&binding_key("response", id), account)?;
                }
                match v["type"].as_str() {
                    Some("response.completed") => {
                        self.terminal_event=Some("completed");
                        bind_output(ctx, &v["response"], account)?;
                        self.terminal = true;
                        if !self.streaming {
                            self.completed = Some(v["response"].take());
                        }
                    }
                    Some("response.output_item.done") => {
                        bind_output(ctx, &json!({"output":[v["item"].take()]}), account)?;
                    }
                    Some("response.failed" | "response.incomplete" | "error") => {
                        self.terminal_event=Some(match v["type"].as_str() { Some("response.failed")=>"failed",Some("response.incomplete")=>"incomplete",_=>"error" });
                        self.terminal = true;
                        self.failed = true;
                        let error = v.get("error").unwrap_or(&v["response"]["error"]);
                        let code = error["code"]
                            .as_str()
                            .or_else(|| v["code"].as_str())
                            .or_else(|| error["type"].as_str());
                        let status = match code {
                            Some("usage_limit_reached" | "rate_limit_exceeded") => Some(429),
                            Some(
                                "invalid_api_key"
                                | "invalid_authentication"
                                | "authentication_error"
                                | "token_expired",
                            ) => Some(401),
                            Some("server_error" | "internal_error" | "internal_server_error" | "server_is_overloaded" | "server_overloaded") => {
                                Some(500)
                            }
                            _ => None,
                        };
                        self.failure_signal = status.map(|status| {
                            (
                                status,
                                error["retry_after_seconds"]
                                    .as_i64()
                                    .unwrap_or(60)
                                    .clamp(1, 86400 * 7),
                            )
                        });
                        self.failure_class=Some(match status {
                            Some(401)=>"authorization", Some(429)=>"rate_limit", Some(500)=>"server",
                            _=>match v["response"]["incomplete_details"]["reason"].as_str() {
                                Some("max_output_tokens") if self.terminal_event==Some("incomplete")=>"max_output_tokens",
                                Some("content_filter") if self.terminal_event==Some("incomplete")=>"content_filter",
                                _=>"unknown",
                            }
                        });
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }
}
async fn observe_failure(a: &mut Audit, obs: &Observer) {
    a.diagnostic.failure_stage=Some(FailureStage::UpstreamRejected);
    a.diagnostic.details=Some(fusion_report_protocol::FailureDetails {
        terminal_event:obs.terminal_event.map(str::to_owned), upstream_failure:obs.failure_class.map(str::to_owned),
        ..Default::default()
    });
    if let Some((status, delay)) = obs.failure_signal {
        if let Some(id) = a.account.as_deref() {
            if matches!(status, 401 | 429) {
                let _ = a.ctx.accounts.mark_failure(id, status, delay).await;
            }
        }
    }
}
async fn responses(State(ctx): State<Context>, h: HeaderMap, tracking: RequestTracking, Json(mut v): Json<Value>) -> Response {
    let mut a = Audit::tracked(ctx, "responses", tracking);
    if !v.is_object() {
        return a.error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "请求必须是 JSON 对象",
        );
    }
    let stream = v["stream"].as_bool().unwrap_or(false);
    let (lease, t) = match prepare(&mut a, &h, &v).await {
        Ok(v) => v,
        Err(r) => return *r,
    };
    v["stream"] = json!(true);
    v["store"] = json!(false);
    if v.get("instructions").is_none() {
        v["instructions"] = json!("");
    }
    if let Some(text) = v["input"].as_str() {
        v["input"] = json!([{"role":"user","content":[{"type":"input_text","text":text}]}]);
    }
    let r = match request(&mut a, &lease, &t, &h, reqwest::Method::POST, "/responses").await {
        Ok(r) => r.header("accept", "text/event-stream").json(&v),
        Err(e) => return a.network_error(e),
    };
    drop(v);
    a.phase("catalog_headers");
    let r = match a.send(r, Duration::from_secs(180)).await {
        Ok(r) => r,
        Err(e) => return a.network_error(e),
    };
    a.received(&r);
    if !r.status().is_success() {
        return rejected(&mut a, &lease, r).await;
    }
    let turn_state = r.headers().get("x-codex-turn-state").cloned();
    // The official client parses SSE bytes even when the backend omits/mislabels
    // Content-Type. Validate an actual event before exposing that response as SSE.
    // Reference: openai/codex 3535c22, codex-api/src/sse/responses.rs.
    let labelled_sse = response_type(&r) == ResponseType::Sse;
    let mime = match response_type(&r) {
        ResponseType::Missing => "缺失",
        ResponseType::Html => "HTML",
        ResponseType::Json => "JSON",
        ResponseType::Sse => "事件流",
        ResponseType::Other => "其他",
    };
    let request_id = a.id.clone();
    let telemetry = a.transport.clone();
    let mut upstream = r.bytes_stream().inspect(move |chunk| {
        if let Ok(b) = chunk { telemetry.lock().unwrap().response_bytes += b.len() as u64; }
    }).boxed();
    if !labelled_sse {
        let mut prefix: Vec<u8> = Vec::new();
        let mut frame_start = 0;
        let mut failure_code = "unexpected_content_type";
        let mut failure_stage = FailureStage::EventValidation;
        let deadline = tokio::time::Instant::now() + IMAGE_TIMEOUT;
        let verified = loop {
            let next = tokio::select! {
                _ = a.ctx.cancel.cancelled() => { failure_code = "service_stopped"; failure_stage = FailureStage::ServiceStopped; break false; },
                r = tokio::time::timeout_at(deadline, upstream.next()) => r,
            };
            let chunk = match next {
                Ok(Some(Ok(chunk))) => chunk,
                Err(_) => {
                    failure_code = "upstream_read_timeout";
                    failure_stage = FailureStage::ResponseRead;
                    break false;
                }
                Ok(Some(Err(e))) => {
                    failure_code = "read_error";
                    failure_stage = FailureStage::ResponseRead;
                    a.diagnostic.network_error_kind = Some(NetworkErrorKind::classify(&e));
                    break false;
                }
                Ok(None) => {
                    failure_code = if prefix.is_empty() {
                        "upstream_empty_response"
                    } else if prefix
                        .iter()
                        .find(|b| !b.is_ascii_whitespace())
                        .is_some_and(|b| matches!(b, b'<' | b'{' | b'['))
                    {
                        "unexpected_content_type"
                    } else {
                        "invalid_event_stream"
                    };
                    break false;
                }
            };
            if prefix.len().saturating_add(chunk.len()) > MAX_EVENT_BYTES {
                failure_code = "upstream_response_too_large";
                break false;
            }
            if !chunk.is_empty() { a.body_activity(); }
            let mut decision = None;
            for (index, byte) in chunk.iter().enumerate() {
                prefix.push(*byte);
                let frame = &prefix[frame_start..];
                // HTML/JSON interception pages aren't SSE; never return or log their contents.
                if *byte == b'\n' {
                    let line = frame.rsplit(|b| *b == b'\n').nth(1).unwrap_or(frame);
                    let line = line.strip_suffix(b"\r").unwrap_or(line);
                    if !line.is_empty()
                        && ![b":".as_slice(), b"data:", b"event:", b"id:", b"retry:"]
                            .iter()
                            .any(|p| line.starts_with(p))
                    {
                        decision = Some(false);
                        break;
                    }
                }
                if frame.ends_with(b"\n\n") || frame.ends_with(b"\r\n\r\n") {
                    let text = std::str::from_utf8(frame).unwrap_or("");
                    let data = text
                        .lines()
                        .filter_map(|l| l.strip_prefix("data:").map(str::trim_start))
                        .collect::<Vec<_>>()
                        .join("\n");
                    if !data.is_empty() {
                        let valid = serde_json::from_str::<Value>(&data)
                            .ok()
                            .and_then(|v| {
                                v["type"]
                                    .as_str()
                                    .map(|s| s == "error" || s.starts_with("response."))
                            })
                            .unwrap_or(false);
                        decision = Some(valid);
                        if valid {
                            prefix.extend_from_slice(&chunk[index + 1..]);
                        }
                        break;
                    }
                    frame_start = prefix.len();
                }
            }
            if let Some(valid) = decision {
                break valid;
            }
        };
        if !verified {
            a.diagnostic.failure_stage = Some(failure_stage);
            if failure_code == "upstream_read_timeout" {
                a.diagnostic.network_error_kind = Some(NetworkErrorKind::Timeout);
            }
            let reason = match failure_code {
                "service_stopped" => "服务已停止",
                "upstream_read_timeout" => "读取上游响应超时",
                "read_error" => "读取上游响应时连接中断",
                "upstream_empty_response" => "上游返回空响应",
                "upstream_response_too_large" => "上游响应超过校验大小限制",
                "invalid_event_stream" => "上游事件流未完成有效事件",
                _ => "上游未返回有效事件流，具体来源未确认",
            };
            return a.error(
                if failure_code == "service_stopped" {
                    StatusCode::SERVICE_UNAVAILABLE
                } else {
                    StatusCode::BAD_GATEWAY
                },
                failure_code,
                &format!("{reason}（响应类型：{mime}{}）；执行状态无法确认，未自动重放",
                    a.diagnostic.network_error_kind.map(|kind| format!("；{}", kind.description())).unwrap_or_default()),
            );
        }
        upstream = futures_util::stream::once(async move { Ok(Bytes::from(prefix)) })
            .chain(upstream)
            .boxed();
        a.diagnostic.detected_response_type = Some(ResponseType::Sse);
    }
    let mut obs = Observer {
        streaming: stream,
        ..Observer::default()
    };
    if !stream {
        loop {
            let next = tokio::select! {_=a.ctx.cancel.cancelled()=>return a.error(StatusCode::SERVICE_UNAVAILABLE,"service_stopped","服务已停止"),r=tokio::time::timeout(IMAGE_TIMEOUT,upstream.next())=>r};
            match next {
                Ok(Some(Ok(b))) => {
                    a.phase("streaming");
                    let observed = obs.push(&b, &a.ctx, &lease.account.id);
                    a.observe_events(&obs);
                    if obs.validated_sse { a.diagnostic.detected_response_type = Some(ResponseType::Sse); }
                    a.response_model = obs.response_model.clone();
                    if let Err(e) = observed {
                        return a.error(StatusCode::BAD_GATEWAY, "stream_invalid", &e);
                    }
                    if let Some(x) = a.diagnostic.execution.as_mut() { x.terminal_received |= obs.terminal; }
                    a.usage = obs.usage.clone();
                    if obs.terminal {
                        break;
                    }
                }
                Ok(None) => {
                    a.diagnostic.network_error_kind = Some(NetworkErrorKind::StreamEnded);
                    a.diagnostic.failure_stage = Some(FailureStage::ResponseRead);
                    break;
                }
                next => {
                    let kind = match next { Ok(Some(Err(e))) => NetworkErrorKind::classify(&e),
                        _ => NetworkErrorKind::Timeout };
                    a.note_stream_failure(kind);
                    let code = a.status.clone();
                    let advice = a.advice.clone();
                    return a.error(StatusCode::BAD_GATEWAY, &code, &advice);
                }
            }
        }
        if obs.failed {
            observe_failure(&mut a, &obs).await;
        }
        if let Some(response) = obs.completed.filter(|_| !obs.failed) {
            a.success();
            return Json(response).into_response();
        }
        return a.error(
            StatusCode::BAD_GATEWAY,
            "response_incomplete",
            "上游响应未完成；未自动重放请求",
        );
    }
    if let Some(x) = a.diagnostic.execution.as_mut() { x.local_http_status = Some(200); }
    let output = async_stream::stream! {
        let mut a = a;
        let account = lease.account.id.clone();
        let mut lease = Some(lease);
        loop{
            let next=tokio::select!{_=a.ctx.cancel.cancelled()=>break,r=tokio::time::timeout(IMAGE_TIMEOUT,upstream.next())=>r};
            match next{
                Ok(Some(Ok(b)))=>{
                    a.phase("streaming");
                    let observed=obs.push(&b,&a.ctx,&account);
                    a.observe_events(&obs);
                    if obs.validated_sse { a.diagnostic.detected_response_type=Some(ResponseType::Sse); }
                    a.response_model=obs.response_model.clone();
                    if observed.is_err(){a.status="stream_invalid".into();a.diagnostic.failure_stage=Some(FailureStage::EventValidation);a.advice="上游事件格式错误，未自动重放".into();yield Err(std::io::Error::other("codex_pool: invalid upstream stream"));break;}
                    if let Some(x) = a.diagnostic.execution.as_mut() { x.terminal_received |= obs.terminal; }
                    a.usage=obs.usage.clone();
                    if obs.terminal{if obs.failed{observe_failure(&mut a, &obs).await;a.status="response_failed".into();a.diagnostic.failure_stage=Some(FailureStage::UpstreamRejected);a.advice="上游返回失败或未完成事件，未自动重放".into();}else{a.success();}}
                    if obs.terminal {
                        // The upstream operation has ended. Do not keep its account
                        // occupied while the client consumes the final buffered bytes.
                        drop(upstream);
                        drop(lease.take());
                        yield Ok::<Bytes,std::io::Error>(b);
                        break;
                    }
                    yield Ok::<Bytes,std::io::Error>(b);
                },
                next=>{
                    if a.ctx.cancel.is_cancelled() {
                        a.status="service_stopped".into();
                        a.diagnostic.failure_stage=Some(FailureStage::ServiceStopped);
                        a.advice="服务已停止，未自动重放".into();
                    } else {
                        let kind=match next {
                            Ok(Some(Err(e)))=>NetworkErrorKind::classify(&e),
                            Ok(None)=>NetworkErrorKind::StreamEnded,
                            _=>NetworkErrorKind::Timeout,
                        };
                        a.note_stream_failure(kind);
                    }
                    yield Err(std::io::Error::other("codex_pool: upstream stream interrupted; request not replayed"));break;
                }
            }
        }
    };
    let mut response = Response::new(Body::from_stream(output));
    response
        .headers_mut()
        .insert("content-type", "text/event-stream".parse().unwrap());
    response
        .headers_mut()
        .insert("cache-control", "no-cache".parse().unwrap());
    response
        .headers_mut()
        .insert("x-request-id", request_id.parse().unwrap());
    if let Some(v) = turn_state {
        response.headers_mut().insert("x-codex-turn-state", v);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn failure_diagnostics_keep_only_finite_metadata_and_never_replay() {
        let (_d, accounts) = crate::tests::fixture();
        crate::tests::add(&accounts, "a");
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = calls.clone();
        let (upstream, up) = serve(Router::new().route(
            "/responses",
            post(move || {
                let i = count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async move {
                    match i {
                        0 => (
                            StatusCode::FORBIDDEN,
                            [
                                ("content-type", "text/html"),
                                ("x-private-header", "private-secret"),
                            ],
                            "<html>private-secret</html>",
                        ),
                        1 => (
                            StatusCode::OK,
                            [
                                ("content-type", "application/json"),
                                ("x-private-header", "private-secret"),
                            ],
                            "{\"private-secret\":true}",
                        ),
                        _ => (
                            StatusCode::OK,
                            [
                                ("content-type", "text/plain"),
                                ("x-private-header", "private-secret"),
                            ],
                            "",
                        ),
                    }
                }
            }),
        ))
        .await;
        let context = ctx(accounts.clone(), upstream);
        let (base, gw) = serve(router(context.clone())).await;
        for code in [
            "upstream_forbidden",
            "unexpected_content_type",
            "upstream_empty_response",
        ] {
            let r = accounts
                .local_http
                .post(format!("{base}/v1/responses"))
                .bearer_auth("test-key")
                .json(&json!({"input":"private-secret"}))
                .send()
                .await
                .unwrap();
            let body = r.text().await.unwrap();
            assert!(body.contains(code), "{body}");
            assert!(!body.contains("private-secret"));
            if code == "upstream_forbidden" {
                assert!(body.contains("具体来源未确认"));
                assert!(!body.contains("没有该模型"));
            }
        }
        let logs = accounts.store.logs().unwrap();
        assert_eq!(logs.len(), 3);
        for log in &logs {
            let diagnostic = log.diagnostic.as_ref().unwrap();
            assert_eq!(diagnostic.app_version, env!("CARGO_PKG_VERSION"));
            assert_eq!(diagnostic.proxy_source, crate::diagnostics::ProxySource::LoopbackDirect);
            assert!(diagnostic.upstream_status.is_some());
        }
        let forbidden = logs
            .iter()
            .find(|l| l.status == "upstream_forbidden")
            .unwrap()
            .diagnostic
            .as_ref()
            .unwrap();
        assert_eq!(forbidden.upstream_status, Some(403));
        assert_eq!(forbidden.response_type, Some(ResponseType::Html));
        assert_eq!(
            forbidden.failure_stage,
            Some(FailureStage::UpstreamRejected)
        );
        assert!(!serde_json::to_string(&logs)
            .unwrap()
            .contains("private-secret"));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 3);
        let mut audit = Audit::new(context.clone(), "responses");
        audit.read_error(NetworkFailure::timeout(FailureStage::ResponseRead));
        assert_eq!(audit.status, "upstream_read_timeout");
        assert_eq!(
            audit.diagnostic.failure_stage,
            Some(FailureStage::ResponseRead)
        );
        context.cancel.cancel();
        let e = send(&context, accounts.local_http.get("http://127.0.0.1:1"))
            .await
            .unwrap_err();
        assert_eq!(e.stage, FailureStage::ServiceStopped);
        gw.abort();
        up.abort();
    }
    use crate::tests::{add, fixture};
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub(super) async fn serve(router: Router) -> (String, tokio::task::JoinHandle<()>) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let handle = tokio::spawn(async move { axum::serve(l, router).await.unwrap() });
        (url, handle)
    }
    pub(super) fn ctx(accounts: Arc<Accounts>, upstream: String) -> Context {
        Context {
            requests: Arc::new(runtime::Registry::default()),
            draining: CancellationToken::new(),
            admission: Arc::new(admission::Admission::default()),
            scheduler: Scheduler::new(accounts.store.clone()),
            accounts,
            key: "test-key".into(),
            upstream,
            cancel: CancellationToken::new(),
        }
    }
    fn events() -> String {
        concat!(
        "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_test\"}}\n\n",
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"你好\"}\n\n",
        "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_test\",\"object\":\"response\",\"status\":\"completed\",\"usage\":{\"input_tokens\":100,\"output_tokens\":20,\"total_tokens\":120,\"input_tokens_details\":{\"cached_tokens\":80},\"output_tokens_details\":{\"reasoning_tokens\":10}},\"output\":[{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"test\",\"arguments\":\"{}\"}]}}\n\n").into()
    }
    #[test]
    fn sse_handles_every_byte_boundary_and_binds_response() {
        let (_d, a) = fixture();
        add(&a, "a");
        let c = ctx(a.clone(), String::new());
        let mut o = Observer::default();
        for b in events().replace('\n', "\r\n").as_bytes() {
            o.push(&[*b], &c, "a").unwrap();
        }
        assert!(o.terminal && !o.failed);
        assert_eq!(o.usage.as_ref().unwrap().total_tokens, 120);
        assert_eq!(o.completed.unwrap()["output"][0]["type"], "function_call");
        assert_eq!(
            a.store
                .binding(&binding_key("response", "resp_test"))
                .unwrap()
                .as_deref(),
            Some("a")
        );
    }
    #[tokio::test]
    async fn mislabeled_sse_is_verified_without_replay_or_exposing_non_sse() {
        let (_d, a) = fixture();
        add(&a, "a");
        let calls = Arc::new(AtomicUsize::new(0));
        let n = calls.clone();
        let payload = format!(": keepalive\r\n\r\nevent: response.created\r\ndata: {{\"type\":\"response.created\",\"response\":{{\"id\":\"r\"}}}}\r\n\r\n{}", events());
        let wire = payload.clone();
        let (upstream, up) = serve(Router::new().route(
            "/responses",
            post(move || {
                let n = n.clone();
                let wire = wire.clone();
                async move {
                    let i = n.fetch_add(1, Ordering::SeqCst) % 6;
                    let mut response = Response::new(if i < 4 {
                        let bytes = wire.into_bytes();
                        Body::from_stream(futures_util::stream::iter(
                            bytes
                                .chunks(13)
                                .map(|b| Ok::<_, std::io::Error>(Bytes::copy_from_slice(b)))
                                .collect::<Vec<_>>(),
                        ))
                    } else {
                        Body::from("<html>PRIVATE interception content</html>\n")
                    });
                    if let Some(mime) = [
                        None,
                        Some("text/plain"),
                        Some("application/json"),
                        Some("Text/Event-Stream; charset=utf-8"),
                        Some("text/html"),
                        None,
                    ][i]
                    {
                        response
                            .headers_mut()
                            .insert("content-type", mime.parse().unwrap());
                    }
                    response
                }
            }),
        ))
        .await;
        let (base, gw) = serve(router(ctx(a.clone(), upstream))).await;
        let mut seen = std::collections::HashSet::new();
        for request in 0..12 {
            let i = request % 6;
            let stream = request < 6;
            let response = reqwest::Client::new()
                .post(format!("{base}/v1/responses"))
                .bearer_auth("test-key")
                .json(&json!({"model":"test-model","input":"hello","stream":stream}))
                .send()
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                if i < 4 {
                    StatusCode::OK
                } else {
                    StatusCode::BAD_GATEWAY
                }
            );
            let body = response.text().await.unwrap();
            if i < 4 {
                if stream { assert_eq!(body, payload); }
                else { assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["status"], "completed"); }
            } else {
                assert!(!body.contains("PRIVATE"));
                assert!(body.contains(if i == 4 { "HTML" } else { "缺失" }));
            }
            let logs = a.store.logs().unwrap();
            assert_eq!(logs.len(), request + 1);
            let log = logs.iter().find(|log| !seen.contains(&log.id)).unwrap();
            seen.insert(log.id.clone());
            let diagnostic = log.diagnostic.as_ref().unwrap();
            assert_eq!(diagnostic.detected_response_type, if i < 4 { Some(ResponseType::Sse) } else { None });
            assert_eq!(diagnostic.response_type, Some([
                ResponseType::Missing, ResponseType::Other, ResponseType::Json,
                ResponseType::Sse, ResponseType::Html, ResponseType::Missing,
            ][i]));
            assert_eq!(calls.load(Ordering::SeqCst), request + 1);
        }
        assert!(!serde_json::to_string(&a.store.logs().unwrap())
            .unwrap()
            .contains("PRIVATE"));
        gw.abort();
        up.abort();
    }
    #[tokio::test]
    async fn responses_stream_aggregate_and_compact_preserve_protocol() {
        let (_d, a) = fixture();
        add(&a, "a");
        let count = Arc::new(AtomicUsize::new(0));
        let n = count.clone();
        let mock=Router::new().route("/responses",post(move |h:HeaderMap,Json(v):Json<Value>|{let n=n.clone();async move{
            n.fetch_add(1,Ordering::SeqCst);assert_eq!(h["authorization"],"Bearer fake-access-token");assert_eq!(h["chatgpt-account-id"],"chatgpt-a");assert_eq!(v["store"],false);assert_eq!(v["stream"],true);
            if v.get("previous_response_id").is_some(){assert_eq!(v["input"][0]["type"],"function_call_output");}
            ([("content-type","text/event-stream")],events())
        }})).route("/responses/compact",post(|Json(v):Json<Value>|async move{assert_eq!(v["model"],"test-model");Json(json!({"object":"response.compaction","output":[{"type":"compaction","encrypted_content":"opaque"}]}))}));
        let (upstream, up) = serve(mock).await;
        let c = ctx(a.clone(), upstream);
        let (base, gw) = serve(router(c.clone())).await;
        let client = reqwest::Client::new();
        let r = client
            .post(format!("{base}/v1/responses"))
            .bearer_auth("test-key")
            .header("session_id", "s")
            .json(&json!({"model":"test-model","input":"hello","stream":true}))
            .send()
            .await
            .unwrap();
        assert!(r.status().is_success());
        assert_eq!(r.text().await.unwrap(), events());
        let r=client.post(format!("{base}/v1/responses")).bearer_auth("test-key").json(&json!({"model":"test-model","previous_response_id":"resp_test","input":[{"type":"function_call_output","call_id":"call_1","output":"ok"}]})).send().await.unwrap();
        assert_eq!(r.json::<Value>().await.unwrap()["status"], "completed");
        let r = client
            .post(format!("{base}/v1/responses/compact"))
            .bearer_auth("test-key")
            .json(&json!({"model":"test-model","input":[]}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            r.json::<Value>().await.unwrap()["output"][0]["encrypted_content"],
            "opaque"
        );
        assert_eq!(count.load(Ordering::SeqCst), 2);
        let usage = a.store.usage().unwrap();
        assert_eq!(usage[0].requests, 3); // stream, aggregate, and compact without usage
        assert_eq!(usage[0].measured_requests, 2);
        assert_eq!(usage[0].tokens.total_tokens, 240);
        assert_eq!(usage[0].tokens.cached_tokens, Some(160));
        assert_eq!(usage[0].tokens.reasoning_tokens, Some(20));
        assert!(a
            .store
            .logs()
            .unwrap()
            .iter()
            .all(|l| l.status == "success"));
        for log in a.store.logs().unwrap() {
            assert_eq!(log.diagnostic.unwrap().detected_response_type,
                Some(if log.operation == "responses" { ResponseType::Sse } else { ResponseType::Json }));
        }
        up.abort();
        gw.abort();
    }
    #[tokio::test]
    async fn request_notifications_use_only_hashed_conversation_identity() {
        let (_dir, accounts) = fixture();
        add(&accounts, "a");
        let mock = Router::new().route(
            "/responses",
            post(|| async {
                (
                    [("content-type", "text/html")],
                    "<html>not an event stream</html>",
                )
            }),
        );
        let (upstream, up) = serve(mock).await;
        let (base, gw) = serve(router(ctx(accounts.clone(), upstream))).await;
        for thread in ["private-thread-a", "private-thread-a", "private-thread-b"] {
            let response = reqwest::Client::new()
                .post(format!("{base}/v1/responses"))
                .bearer_auth("test-key")
                .header("session_id", "private-shared-session")
                .header(
                    "x-codex-turn-metadata",
                    json!({"thread_id": thread}).to_string(),
                )
                .json(&json!({"model": "test-model", "input": "private-prompt"}))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
            let _ = response.bytes().await.unwrap();
        }
        let logs = accounts.store.logs().unwrap();
        assert_eq!(logs.len(), 3);
        assert_eq!(
            logs.iter()
                .filter(|log| log.conversation_key.as_deref()
                    == Some(&binding_key("thread", "private-thread-a")))
                .count(),
            2
        );
        assert_eq!(
            logs.iter()
                .filter(|log| log.conversation_key.as_deref()
                    == Some(&binding_key("thread", "private-thread-b")))
                .count(),
            1
        );
        assert!(!serde_json::to_string(&logs).unwrap().contains("private-"));
        let legacy: RequestLog = serde_json::from_value(json!({"id":"old", "at":1, "account":null, "operation":"responses", "status":"success", "elapsedMs":1, "advice":""})).unwrap();
        assert!(legacy.conversation_key.is_none());
        up.abort();
        gw.abort();
    }
    #[tokio::test]
    async fn unknown_previous_response_and_bad_key_never_reach_upstream() {
        let (_d, a) = fixture();
        add(&a, "a");
        let (base, gw) = serve(router(ctx(a, "http://127.0.0.1:1".into()))).await;
        let client = reqwest::Client::new();
        let r = client
            .get(format!("{base}/v1/models"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
        let r = client
            .post(format!("{base}/v1/responses"))
            .bearer_auth("test-key")
            .json(&json!({"previous_response_id":"unknown"}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::CONFLICT);
        gw.abort();
    }
    #[tokio::test]
    async fn rate_limit_cools_account_without_retry_and_logs_are_redacted() {
        let (_d, a) = fixture();
        add(&a, "a");
        let calls = Arc::new(AtomicUsize::new(0));
        let n = calls.clone();
        let (upstream, up) = serve(Router::new().route(
            "/responses",
            post(move || {
                let n = n.clone();
                async move {
                    n.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::TOO_MANY_REQUESTS,
                        [("retry-after", "120")],
                        "secret prompt token must never be logged",
                    )
                }
            }),
        ))
        .await;
        let (base, gw) = serve(router(ctx(a.clone(), upstream))).await;
        let r = reqwest::Client::new()
            .post(format!("{base}/v1/responses"))
            .bearer_auth("test-key")
            .json(&json!({"input":"private prompt"}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(a.store.account("a").unwrap().cooldown_until >= crate::now() + 119);
        let logs = serde_json::to_string(&a.store.logs().unwrap()).unwrap();
        for s in [
            "private prompt",
            "secret prompt",
            "fake-access-token",
            "fake-refresh-token",
            "test-key",
        ] {
            assert!(!logs.contains(s));
        }
        up.abort();
        gw.abort();
    }
    #[tokio::test]
    async fn interrupted_stream_is_never_replayed_and_releases_lease() {
        let (_d, a) = fixture();
        add(&a, "a");
        let calls = Arc::new(AtomicUsize::new(0));
        let n = calls.clone();
        let (upstream, up) = serve(Router::new().route(
            "/responses",
            post(move || {
                let n = n.clone();
                async move {
                    n.fetch_add(1, Ordering::SeqCst);
                    (
                        [("content-type", "text/event-stream")],
                        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n",
                    )
                }
            }),
        ))
        .await;
        let c = ctx(a.clone(), upstream);
        let (base, gw) = serve(router(c.clone())).await;
        let r = reqwest::Client::new()
            .post(format!("{base}/v1/responses"))
            .bearer_auth("test-key")
            .json(&json!({"stream":true}))
            .send()
            .await;
        // Hyper can surface the deliberate body error before delivering response headers.
        if let Ok(r) = r {
            let _ = r.bytes().await;
        }
        for _ in 0..20 {
            if !a.store.logs().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(!c.scheduler.is_active("a"));
        assert_eq!(a.store.logs().unwrap()[0].status, "stream_interrupted");
        up.abort();
        gw.abort();
    }
    #[tokio::test]
    async fn service_cancellation_does_not_count_as_account_network_failure() {
        let (_d, a) = fixture();
        add(&a, "a");
        add(&a, "b");
        let c = ctx(a, String::new());
        c.cancel.cancel();
        for _ in 0..2 {
            let mut audit = Audit::new(c.clone(), "responses");
            audit.account = Some("a".into());
            audit.error(StatusCode::BAD_GATEWAY, "network_error", "服务已停止");
        }
        assert_eq!(c.scheduler.acquire(None).await.unwrap().account.id, "a");
    }
    #[tokio::test]
    async fn consecutive_server_failures_preserve_healthy_preference_and_bound_requests() {
        let (_d, a) = fixture();
        add(&a, "a");
        add(&a, "b");
        let calls = Arc::new(AtomicUsize::new(0));
        let n = calls.clone();
        let (upstream, up) = serve(Router::new().route(
            "/responses",
            post(move |h: HeaderMap| {
                let n = n.clone();
                async move {
                    n.fetch_add(1, Ordering::SeqCst);
                    if h["chatgpt-account-id"] == "chatgpt-a" {
                        (
                            StatusCode::BAD_GATEWAY,
                            [("content-type", "text/plain")],
                            "untrusted upstream secret".to_string(),
                        )
                    } else {
                        (
                            StatusCode::OK,
                            [("content-type", "text/event-stream")],
                            events(),
                        )
                    }
                }
            }),
        ))
        .await;
        let c = ctx(a.clone(), upstream);
        let (base, gw) = serve(router(c.clone())).await;
        let client = reqwest::Client::new();
        for attempt in 1..=2 {
            let r = client
                .post(format!("{base}/v1/responses"))
                .bearer_auth("test-key")
                .header("session_id", "original")
                .json(&json!({"input":"private input"}))
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), StatusCode::BAD_GATEWAY);
            assert_eq!(calls.load(Ordering::SeqCst), attempt); // One upstream call per client request.
        }
        let r = client
            .post(format!("{base}/v1/responses"))
            .bearer_auth("test-key")
            .header("session_id", "new")
            .json(&json!({"input":"hello"}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(
            c.scheduler.status().unwrap().current_account_id.as_deref(),
            Some("a")
        );
        // Both new and old sessions keep the healthy preferred account.
        let r = client
            .post(format!("{base}/v1/responses"))
            .bearer_auth("test-key")
            .header("session_id", "original")
            .json(&json!({"input":"continue"}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        assert_eq!(
            a.store
                .binding(&binding_key("session", "original"))
                .unwrap()
                .as_deref(),
            Some("a")
        );
        assert_eq!(
            a.store
                .binding(&binding_key("session", "new"))
                .unwrap()
                .as_deref(),
            Some("a")
        );
        let logs = serde_json::to_string(&a.store.logs().unwrap()).unwrap();
        assert!(!logs.contains("untrusted upstream secret") && !logs.contains("private input"));
        gw.abort();
        up.abort();
    }
    #[tokio::test]
    async fn service_rejects_occupied_port_and_can_restart() {
        let (_d, a) = fixture();
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let mut s = a.store.settings().unwrap();
        s.port = port;
        a.store.save_settings(&s).unwrap();
        let g = Gateway::new(a);
        assert!(g.start().await.is_err());
        drop(l);
        assert!(g.start().await.unwrap().running);
        g.stop().await;
        assert!(!g.status().await.running);
        assert!(g.start().await.unwrap().running);
        g.stop().await;
    }
    #[test]
    fn encrypted_context_is_pinned_and_unknown_context_rejected() {
        let (_d, a) = fixture();
        add(&a, "a");
        add(&a, "b");
        let c = ctx(a.clone(), String::new());
        let input =
            json!({"input":[{"type":"compaction","encrypted_content":"opaque-private-value"}]});
        assert!(pinned(&c, &HeaderMap::new(), &input).is_err());
        bind_output(
            &c,
            &json!({"output":[{"type":"compaction","encrypted_content":"opaque-private-value"}]}),
            "a",
        )
        .unwrap();
        assert_eq!(
            pinned(&c, &HeaderMap::new(), &input).unwrap().as_deref(),
            Some("a")
        );
        let mut headers = HeaderMap::new();
        headers.insert("session_id", "other-session".parse().unwrap());
        a.store
            .bind(&binding_key("session", "other-session"), "b")
            .unwrap();
        assert!(pinned(&c, &headers, &input).is_err());
    }
    #[tokio::test]
    async fn cancelling_stream_releases_upstream_and_concurrency_slot() {
        let (_d, a) = fixture();
        add(&a, "a");
        let dropped = Arc::new(AtomicUsize::new(0));
        let n = dropped.clone();
        struct OnDrop(Arc<AtomicUsize>);
        impl Drop for OnDrop {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let(upstream,up)=serve(Router::new().route("/responses",post(move||{let n=n.clone();async move{
            let stream=async_stream::stream!{let _guard=OnDrop(n);yield Ok::<Bytes,std::io::Error>(Bytes::from_static(b"data: {\"type\":\"response.created\",\"response\":{\"id\":\"cancel-test\"}}\n\n"));std::future::pending::<()>().await;};
            ([("content-type","text/event-stream")],Body::from_stream(stream))
        }}))).await;
        let c = ctx(a.clone(), upstream);
        let (base, gw) = serve(router(c.clone())).await;
        let r = reqwest::Client::new()
            .post(format!("{base}/v1/responses"))
            .bearer_auth("test-key")
            .json(&json!({"stream":true}))
            .send()
            .await
            .unwrap();
        let mut stream = r.bytes_stream();
        assert!(stream.next().await.unwrap().is_ok());
        drop(stream);
        for _ in 0..100 {
            if !c.scheduler.is_active("a") && dropped.load(Ordering::SeqCst) > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!c.scheduler.is_active("a"));
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        assert_eq!(a.store.logs().unwrap()[0].status, "cancelled");
        up.abort();
        gw.abort();
    }
}
