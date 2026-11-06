//! Per-client, per-source-revision loopback gateways. Native API payloads are not
//! normalized using the ChatGPT/Codex subscription gateway rules.
use crate::{
    api_sources::{context_hash, ClientKind, Protocol, Snapshot, Sources},
    now,
    usage::TokenUsage,
    Result,
};
use axum::{
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use tokio::{
    sync::{Notify, OwnedSemaphorePermit, Semaphore},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

const REQUEST_LIMIT: usize = 16 * 1024 * 1024;
const RESPONSE_LIMIT: usize = 64 * 1024 * 1024;
const EVENT_LIMIT: usize = 1024 * 1024;
const MAX_LOGS: usize = 2000;
const UPLOAD_IDLE: Duration = Duration::from_secs(15);
const UPLOAD_TOTAL: Duration = Duration::from_secs(60);
const HEADER_TIMEOUT: Duration = Duration::from_secs(180);
const RESPONSE_IDLE: Duration = Duration::from_secs(600);

// This value is intentionally not serializable or printable through Debug.
pub struct Endpoint {
    base_url: String,
    local_key: String,
}
impl Endpoint {
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
    pub fn local_key(&self) -> &str {
        &self.local_key
    }
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayStatus {
    pub client: ClientKind,
    pub source_id: String,
    pub source_revision: u64,
    pub running: bool,
    pub draining: bool,
    pub active_requests: usize,
    pub history_incomplete: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiRequestLog {
    pub id: String,
    pub at: i64,
    pub client: ClientKind,
    pub source_id: String,
    pub source_revision: u64,
    pub operation: String,
    pub model: Option<String>,
    pub response_model: Option<String>,
    pub status: String,
    pub upstream_status: Option<u16>,
    pub elapsed_ms: u64,
    pub request_bytes: u64,
    pub response_bytes: u64,
    pub usage: Option<TokenUsage>,
    pub usage_partial: bool,
    pub tool_call_observed: bool,
    #[serde(skip)]
    usage_input_known: bool,
    #[serde(skip)]
    usage_output_known: bool,
}
struct Runtime {
    running: bool,
    draining: bool,
    active: usize,
    logs: VecDeque<ApiRequestLog>,
    history_incomplete: bool,
}
struct Context {
    sources: Arc<Sources>,
    snapshot: Snapshot,
    client: ClientKind,
    key: String,
    runtime: Mutex<Runtime>,
    changed: Notify,
    slots: Arc<Semaphore>,
    cancel: CancellationToken,
}
pub struct ApiGateway {
    context: Arc<Context>,
    endpoint: Endpoint,
    task: tokio::sync::Mutex<Option<JoinHandle<()>>>,
}
impl ApiGateway {
    pub async fn start(sources: Arc<Sources>, client: ClientKind, id: &str) -> Result<Arc<Self>> {
        let snapshot = sources.snapshot(id)?;
        if !matches!(
            (client, snapshot.source.protocol),
            (ClientKind::Codex, Protocol::Responses) | (ClientKind::ClaudeCode, Protocol::Messages)
        ) {
            return Err("此来源协议与客户端不兼容；首版仅支持 Codex/Responses 和 Claude Code/Messages 原生接入".into());
        }
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|_| "API 本机监听失败")?;
        let port = listener
            .local_addr()
            .map_err(|_| "API 本机地址读取失败")?
            .port();
        let key = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        // Each application gets a new identity. No old provider definition can silently
        // resolve to another source, revision, local port or credential.
        let prefix = format!(
            "/fusion-api/{}/{}/{}/v1",
            snapshot.source.id,
            snapshot.source.revision,
            uuid::Uuid::new_v4().simple()
        );
        let endpoint = Endpoint {
            base_url: format!("http://127.0.0.1:{port}{prefix}"),
            local_key: key.clone(),
        };
        let context = Arc::new(Context {
            sources,
            snapshot,
            client,
            key,
            runtime: Mutex::new(Runtime {
                running: true,
                draining: false,
                active: 0,
                logs: VecDeque::new(),
                history_incomplete: false,
            }),
            changed: Notify::new(),
            slots: Arc::new(Semaphore::new(4)),
            cancel: CancellationToken::new(),
        });
        let api = Router::new()
            .route("/models", get(forward))
            .route("/responses", post(forward))
            .route("/responses/compact", post(forward))
            .route("/messages", post(forward))
            .route("/messages/count_tokens", post(forward));
        let router = Router::new().nest(&prefix, api).with_state(context.clone());
        let cancel = context.cancel.clone();
        let status_context = context.clone();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(cancel.cancelled_owned())
                .await;
            if let Ok(mut runtime) = status_context.runtime.lock() {
                runtime.running = false;
            }
            status_context.changed.notify_waiters();
        });
        Ok(Arc::new(Self {
            context,
            endpoint,
            task: tokio::sync::Mutex::new(Some(task)),
        }))
    }
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }
    pub fn status(&self) -> GatewayStatus {
        let runtime = self
            .context
            .runtime
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        GatewayStatus {
            client: self.context.client,
            source_id: self.context.snapshot.source.id.clone(),
            source_revision: self.context.snapshot.source.revision,
            running: runtime.running,
            draining: runtime.draining,
            active_requests: runtime.active,
            history_incomplete: runtime.history_incomplete,
        }
    }
    pub fn logs(&self) -> Vec<ApiRequestLog> {
        self.context
            .runtime
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .logs
            .iter()
            .rev()
            .cloned()
            .collect()
    }
    pub fn begin_drain(&self) {
        self.context
            .runtime
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .draining = true;
        self.context.changed.notify_waiters();
    }
    pub async fn wait_drained(&self) {
        loop {
            let notified = self.context.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self
                .context
                .runtime
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .active
                == 0
            {
                return;
            }
            notified.await;
        }
    }
    pub async fn stop(&self) {
        self.begin_drain();
        self.wait_drained().await;
        self.context.cancel.cancel();
        if let Some(task) = self.task.lock().await.take() {
            let abort = task.abort_handle();
            if tokio::time::timeout(Duration::from_secs(3), task)
                .await
                .is_err()
            {
                abort.abort();
            }
        }
        self.context
            .runtime
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .running = false;
    }
}
impl Drop for ApiGateway {
    fn drop(&mut self) {
        self.context.cancel.cancel();
    }
}

struct Audit {
    context: Arc<Context>,
    log: ApiRequestLog,
    started: Instant,
    _slot: OwnedSemaphorePermit,
}
impl Audit {
    fn begin(context: Arc<Context>, operation: &str) -> std::result::Result<Self, Box<Response>> {
        let slot = context.slots.clone().try_acquire_owned().map_err(|_| {
            Box::new(failure(
                StatusCode::TOO_MANY_REQUESTS,
                "local_capacity",
                "此客户端并发已满，未发送上游",
            ))
        })?;
        {
            let mut runtime = context.runtime.lock().map_err(|_| {
                Box::new(failure(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "local_state",
                    "本机连接状态不可用",
                ))
            })?;
            if runtime.draining || !runtime.running {
                return Err(Box::new(failure(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "draining",
                    "此客户端连接正在关闭，未发送上游",
                )));
            }
            runtime.active += 1;
        }
        let log = ApiRequestLog {
            id: uuid::Uuid::new_v4().to_string(),
            at: now(),
            client: context.client,
            source_id: context.snapshot.source.id.clone(),
            source_revision: context.snapshot.source.revision,
            operation: operation.into(),
            model: None,
            response_model: None,
            status: "cancelled".into(),
            upstream_status: None,
            elapsed_ms: 0,
            request_bytes: 0,
            response_bytes: 0,
            usage: None,
            usage_partial: false,
            tool_call_observed: false,
            usage_input_known: false,
            usage_output_known: false,
        };
        Ok(Self {
            context,
            log,
            started: Instant::now(),
            _slot: slot,
        })
    }
    fn fail(&mut self, status: StatusCode, code: &'static str, message: &'static str) -> Response {
        self.log.status = code.into();
        failure(status, code, message)
    }
}
impl Drop for Audit {
    fn drop(&mut self) {
        self.log.elapsed_ms = self.started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        // Persistence happens before waking shutdown, so a normal stop cannot discard
        // its final completed request. A storage failure never changes/replays the API call.
        let persisted = self.context.sources.record_log(&self.log).is_ok();
        let mut runtime = self
            .context
            .runtime
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if !persisted {
            runtime.history_incomplete = true;
        }
        runtime.active = runtime.active.saturating_sub(1);
        runtime.logs.push_back(self.log.clone());
        while runtime.logs.len() > MAX_LOGS {
            runtime.logs.pop_front();
        }
        drop(runtime);
        self.context.changed.notify_waiters();
    }
}
fn failure(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        [("x-should-retry", "false")],
        Json(json!({"error":{"type":code,"code":code,"message":message}})),
    )
        .into_response()
}
fn authorized(headers: &HeaderMap, key: &str) -> bool {
    // A dedicated local header wins and never falls back to the client's own bearer.
    let selected = if headers.contains_key("x-fusion-client-key") {
        "x-fusion-client-key"
    } else if headers.contains_key("x-api-key") {
        "x-api-key"
    } else {
        "authorization"
    };
    let values: Vec<_> = headers.get_all(selected).iter().collect();
    if values.len() != 1 {
        return false;
    }
    let Some(raw) = values[0].to_str().ok() else {
        return false;
    };
    let supplied = if selected == "authorization" {
        raw.strip_prefix("Bearer ").unwrap_or("")
    } else {
        raw
    };
    supplied.as_bytes().ct_eq(key.as_bytes()).into()
}
async fn forward(State(context): State<Arc<Context>>, request: Request) -> Response {
    if !authorized(request.headers(), &context.key) {
        return failure(
            StatusCode::UNAUTHORIZED,
            "local_auth",
            "本机 API 连接密钥无效",
        );
    }
    let path = request.uri().path();
    let operation = if path.ends_with("/models") {
        "models"
    } else if path.ends_with("/responses/compact") {
        "responses/compact"
    } else if path.ends_with("/responses") {
        "responses"
    } else if path.ends_with("/messages/count_tokens") {
        "messages/count_tokens"
    } else {
        "messages"
    };
    let compatible = operation == "models"
        || match context.snapshot.source.protocol {
            Protocol::Responses => operation.starts_with("responses"),
            Protocol::Messages => operation.starts_with("messages"),
            Protocol::ChatCompletions => false,
        };
    if !compatible {
        return failure(
            StatusCode::NOT_FOUND,
            "protocol_route",
            "此连接不支持该协议路由",
        );
    }
    let mut audit = match Audit::begin(context.clone(), operation) {
        Ok(a) => a,
        Err(r) => return *r,
    };
    if context
        .sources
        .current(
            &context.snapshot.source.id,
            context.snapshot.source.revision,
        )
        .is_err()
    {
        return audit.fail(
            StatusCode::CONFLICT,
            "source_changed",
            "来源已停用、移除或变更，请重新应用此连接",
        );
    }
    let (parts, body) = request.into_parts();
    let raw = if parts.method == Method::GET {
        Vec::new()
    } else {
        if parts
            .headers
            .get("content-type")
            .and_then(|h| h.to_str().ok())
            .is_none_or(|h| h.split(';').next().unwrap_or("").trim() != "application/json")
        {
            return audit.fail(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "json_required",
                "此 API 连接仅接受原生 JSON 请求",
            );
        }
        if parts
            .headers
            .get("content-length")
            .and_then(|h| h.to_str().ok())
            .and_then(|h| h.parse::<u64>().ok())
            .is_some_and(|n| n > REQUEST_LIMIT as u64)
        {
            return audit.fail(
                StatusCode::PAYLOAD_TOO_LARGE,
                "request_too_large",
                "API 请求超过 16 MiB，未发送上游",
            );
        }
        let collect = async {
            let mut stream = body.into_data_stream();
            let mut bytes = Vec::new();
            loop {
                match tokio::time::timeout(UPLOAD_IDLE, stream.next()).await {
                    Ok(Some(Ok(chunk))) => {
                        if bytes.len().saturating_add(chunk.len()) > REQUEST_LIMIT {
                            return Err((
                                StatusCode::PAYLOAD_TOO_LARGE,
                                "request_too_large",
                                "API 请求超过 16 MiB，未发送上游",
                            ));
                        }
                        bytes.extend_from_slice(&chunk);
                    }
                    Ok(None) => return Ok(bytes),
                    Ok(Some(Err(_))) => {
                        return Err((
                            StatusCode::BAD_REQUEST,
                            "request_read",
                            "API 请求读取失败，未发送上游",
                        ))
                    }
                    Err(_) => {
                        return Err((
                            StatusCode::REQUEST_TIMEOUT,
                            "request_idle",
                            "API 请求上传空闲超时，未发送上游",
                        ))
                    }
                }
            }
        };
        match tokio::time::timeout(UPLOAD_TOTAL, collect).await {
            Ok(Ok(bytes)) => bytes,
            Ok(Err((s, c, m))) => return audit.fail(s, c, m),
            Err(_) => {
                return audit.fail(
                    StatusCode::REQUEST_TIMEOUT,
                    "request_timeout",
                    "API 请求上传超时，未发送上游",
                )
            }
        }
    };
    audit.log.request_bytes = raw.len() as u64;
    if !raw.is_empty() {
        let value: Value = match serde_json::from_slice(&raw) {
            Ok(v) => v,
            Err(_) => {
                return audit.fail(
                    StatusCode::BAD_REQUEST,
                    "invalid_json",
                    "API 请求 JSON 无效，未发送上游",
                )
            }
        };
        if !value.is_object() {
            return audit.fail(
                StatusCode::BAD_REQUEST,
                "invalid_json",
                "API 请求必须为 JSON 对象",
            );
        }
        audit.log.model = crate::usage::model_name(&value["model"]);
        if audit.log.model.is_none() && operation != "responses/compact" {
            return audit.fail(
                StatusCode::BAD_REQUEST,
                "invalid_model",
                "请求缺少有效模型名称",
            );
        }
        if validate_context(&context, &parts.headers, &value).is_err() {
            return audit.fail(
                StatusCode::CONFLICT,
                "context_source",
                "会话或上下文原来源无法确认，已阻止发送；请使用原连接或新建会话",
            );
        }
    }
    let mut builder = match context.snapshot.request(parts.method, operation).await {
        Ok(r) => r,
        Err(_) => {
            return audit.fail(
                StatusCode::BAD_GATEWAY,
                "network_route",
                "无法准备所选网络线路，未发送上游",
            )
        }
    };
    // Never forward incoming Authorization, x-api-key, account IDs or Codex subscription headers.
    if !raw.is_empty() {
        builder = builder.header("content-type", "application/json").body(raw);
    }
    if let Some(accept) = parts.headers.get("accept") {
        if accept.as_bytes().len() <= 128 {
            builder = builder.header("accept", accept);
        }
    }
    if context.snapshot.source.protocol == Protocol::Messages {
        if let Some(beta) = parts.headers.get("anthropic-beta") {
            if beta.as_bytes().len() <= 4096 {
                builder = builder.header("anthropic-beta", beta);
            }
        }
    }
    let response = tokio::select! {
        _ = context.cancel.cancelled() => return audit.fail(StatusCode::SERVICE_UNAVAILABLE,"stopped","API 连接已停止；请求状态未知，未自动重试"),
        result = tokio::time::timeout(HEADER_TIMEOUT, builder.send()) => match result {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => return audit.fail(StatusCode::BAD_GATEWAY,"transport_failure","API 传输失败；执行状态未知，未自动重试"),
            Err(_) => return audit.fail(StatusCode::GATEWAY_TIMEOUT,"upstream_timeout","API 等待响应超时；执行状态未知，未自动重试"),
        },
    };
    let status = response.status();
    audit.log.upstream_status = Some(status.as_u16());
    if !status.is_success() {
        return audit.fail(
            status,
            "upstream_rejected",
            "上游拒绝请求；未自动重试，请检查来源与权限",
        );
    }
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let is_sse = content_type
        .split(';')
        .next()
        .is_some_and(|m| m.trim().eq_ignore_ascii_case("text/event-stream"));
    let is_json = content_type
        .split(';')
        .next()
        .is_some_and(|m| m.trim().eq_ignore_ascii_case("application/json"));
    if !is_sse && !is_json {
        return audit.fail(
            StatusCode::BAD_GATEWAY,
            "invalid_response_type",
            "上游响应类型与所选原生协议不符",
        );
    }
    if is_sse {
        let stream_context = context.clone();
        let stream = async_stream::stream! {
            let mut audit = audit;
            let mut upstream = response.bytes_stream();
            let mut observer = Observer::new(stream_context.clone());
            loop {
                let next = tokio::select! { _ = stream_context.cancel.cancelled() => { audit.log.status="stopped".into(); yield Err(std::io::Error::other("API connection stopped")); break; }, r = tokio::time::timeout(RESPONSE_IDLE, upstream.next()) => r };
                match next {
                    Ok(Some(Ok(chunk))) => {
                        audit.log.response_bytes = audit.log.response_bytes.saturating_add(chunk.len() as u64);
                        if observer.feed(&chunk, &mut audit.log).is_err() { audit.log.status="invalid_stream".into(); yield Err(std::io::Error::other("API stream validation failed")); break; }
                        if observer.terminal {
                            audit.log.status=if observer.failed {"upstream_error"} else {"success"}.into();
                            // Do not retain an upstream keep-alive after its native terminal event.
                            // Record completion before delivering it, even if the consumer drops now.
                            drop(audit);
                            yield Ok::<_,std::io::Error>(chunk);
                            break;
                        }
                        yield Ok::<_,std::io::Error>(chunk);
                    },
                    Ok(None) => {
                        if observer.finish(&mut audit.log).is_err() { audit.log.status="stream_interrupted".into(); yield Err(std::io::Error::other("API stream ended before its terminal event")); }
                        else { audit.log.status=if observer.failed {"upstream_error"} else {"success"}.into(); }
                        break;
                    },
                    _ => { audit.log.status="stream_interrupted".into(); yield Err(std::io::Error::other("API stream interrupted; request was not replayed")); break; },
                }
            }
        };
        return Response::builder()
            .status(status)
            .header("content-type", "text/event-stream")
            .header("cache-control", "no-store")
            .body(Body::from_stream(stream))
            .unwrap_or_else(|_| {
                failure(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "local_response",
                    "本机响应构造失败",
                )
            });
    }
    let mut upstream = response.bytes_stream();
    let mut bytes = Vec::new();
    loop {
        let next = tokio::select! { _ = context.cancel.cancelled() => return audit.fail(StatusCode::SERVICE_UNAVAILABLE,"stopped","API 连接已停止；未自动重试"), r = tokio::time::timeout(RESPONSE_IDLE, upstream.next()) => r };
        match next {
            Ok(Some(Ok(chunk))) => {
                audit.log.response_bytes =
                    audit.log.response_bytes.saturating_add(chunk.len() as u64);
                if bytes.len().saturating_add(chunk.len()) > RESPONSE_LIMIT {
                    return audit.fail(
                        StatusCode::BAD_GATEWAY,
                        "response_too_large",
                        "API 响应超过大小限制，未自动重试",
                    );
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            _ => {
                return audit.fail(
                    StatusCode::BAD_GATEWAY,
                    "response_interrupted",
                    "API 响应中断；执行状态未知，未自动重试",
                )
            }
        }
    }
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => {
            return audit.fail(
                StatusCode::BAD_GATEWAY,
                "invalid_json_response",
                "上游未返回有效 JSON，未自动重试",
            )
        }
    };
    if record_output(&context, &value, &mut audit.log).is_err() {
        return audit.fail(
            StatusCode::BAD_GATEWAY,
            "context_storage",
            "无法保存来源上下文绑定，未自动重试",
        );
    }
    audit.log.status = if value.get("error").is_some() || value["status"] == "failed" {
        "upstream_error"
    } else {
        "success"
    }
    .into();
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .header("cache-control", "no-store")
        .body(Body::from(bytes))
        .unwrap_or_else(|_| {
            failure(
                StatusCode::INTERNAL_SERVER_ERROR,
                "local_response",
                "本机响应构造失败",
            )
        })
}

fn request_references(value: &Value, refs: &mut Vec<String>) {
    match value {
        Value::Array(values) => {
            for v in values {
                request_references(v, refs);
            }
        }
        Value::Object(object) => {
            for (field, kind) in [
                ("previous_response_id", "response"),
                ("encrypted_content", "encrypted"),
                ("signature", "signature"),
                ("tool_use_id", "tool"),
                ("call_id", "tool"),
                ("file_id", "file"),
            ] {
                if let Some(raw) = object
                    .get(field)
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    refs.push(context_hash(kind, raw));
                }
            }
            if object.get("type").and_then(Value::as_str) == Some("item_reference") {
                if let Some(raw) = object.get("id").and_then(Value::as_str) {
                    refs.push(context_hash("item", raw));
                }
            }
            if object.get("type").and_then(Value::as_str) == Some("redacted_thinking") {
                if let Some(raw) = object.get("data").and_then(Value::as_str) {
                    refs.push(context_hash("redacted", raw));
                }
            }
            // Never recurse into tool arguments, JSON schema or arbitrary metadata.
            for field in ["input", "messages", "content"] {
                if let Some(v) = object.get(field) {
                    request_references(v, refs);
                }
            }
        }
        _ => {}
    }
}
fn validate_context(context: &Context, headers: &HeaderMap, value: &Value) -> Result<()> {
    let scope = context.snapshot.scope();
    let mut refs = Vec::new();
    request_references(value, &mut refs);
    context.sources.contexts(&scope, &refs, false)?;
    let mut sessions = Vec::new();
    for name in ["session_id", "conversation_id", "x-codex-session-id"] {
        if let Some(raw) = headers
            .get(name)
            .and_then(|h| h.to_str().ok())
            .filter(|s| !s.is_empty() && s.len() <= 1024)
        {
            sessions.push(context_hash(&format!("{:?}-{name}", context.client), raw));
        }
    }
    for name in ["thread_id", "session_id"] {
        if let Some(raw) = value["client_metadata"][name]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 1024)
        {
            sessions.push(context_hash(&format!("{:?}-{name}", context.client), raw));
        }
    }
    for raw in [
        headers
            .get("x-codex-turn-metadata")
            .and_then(|h| h.to_str().ok()),
        value["client_metadata"]["x-codex-turn-metadata"].as_str(),
    ]
    .into_iter()
    .flatten()
    {
        if raw.len() > 64 * 1024 {
            return Err("会话元数据超过限制".into());
        }
        let metadata: Value = serde_json::from_str(raw).map_err(|_| "会话元数据无效")?;
        for name in ["thread_id", "session_id"] {
            if let Some(id) = metadata[name]
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 1024)
            {
                sessions.push(context_hash(&format!("{:?}-{name}", context.client), id));
            }
        }
    }
    sessions.sort();
    sessions.dedup();
    context.sources.contexts(&scope, &sessions, true)
}
fn output_refs(value: &Value, refs: &mut Vec<String>, tools: &mut bool) {
    match value {
        Value::Array(values) => {
            for value in values {
                output_refs(value, refs, tools);
            }
        }
        Value::Object(object) => {
            let kind = object.get("type").and_then(Value::as_str).unwrap_or("");
            if kind == "function_call" || kind == "tool_use" {
                *tools = true;
            }
            if let Some(raw) = object.get("id").and_then(Value::as_str) {
                if object.get("object").and_then(Value::as_str) == Some("response")
                    || raw.starts_with("resp_")
                {
                    refs.push(context_hash("response", raw));
                } else if kind == "tool_use" {
                    refs.push(context_hash("tool", raw));
                } else if !kind.is_empty() && kind != "message" {
                    refs.push(context_hash("item", raw));
                }
            }
            for (field, kind) in [
                ("call_id", "tool"),
                ("encrypted_content", "encrypted"),
                ("signature", "signature"),
            ] {
                if let Some(raw) = object
                    .get(field)
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    refs.push(context_hash(kind, raw));
                }
            }
            if kind == "redacted_thinking" {
                if let Some(raw) = object.get("data").and_then(Value::as_str) {
                    refs.push(context_hash("redacted", raw));
                }
            }
            for field in [
                "response",
                "output",
                "item",
                "content",
                "content_block",
                "delta",
                "message",
            ] {
                if let Some(v) = object.get(field) {
                    output_refs(v, refs, tools);
                }
            }
        }
        _ => {}
    }
}
fn record_output(context: &Context, value: &Value, log: &mut ApiRequestLog) -> Result<()> {
    let mut refs = Vec::new();
    output_refs(value, &mut refs, &mut log.tool_call_observed);
    refs.sort();
    refs.dedup();
    context
        .sources
        .contexts(&context.snapshot.scope(), &refs, true)?;
    let response = value.get("response").unwrap_or(value);
    if let Some(model) = crate::usage::model_name(&response["model"]) {
        log.response_model = Some(model);
    }
    if let Some(usage) = TokenUsage::from_response(response) {
        log.usage = Some(usage);
        log.usage_input_known = true;
        log.usage_output_known = true;
        log.usage_partial = false;
    }
    let message = value.get("message").unwrap_or(value);
    if let Some(model) = crate::usage::model_name(&message["model"]) {
        log.response_model = Some(model);
    }
    if let Some(usage) = message.get("usage").filter(|u| u.is_object()) {
        if context.snapshot.source.protocol == Protocol::Messages {
            let current = log.usage.get_or_insert_with(TokenUsage::default);
            if let Some(n) = usage["input_tokens"].as_u64() {
                current.input_tokens = n;
                log.usage_input_known = true;
            }
            if let Some(n) = usage["output_tokens"].as_u64() {
                current.output_tokens = n;
                log.usage_output_known = true;
            }
            current.cached_tokens = usage["cache_read_input_tokens"]
                .as_u64()
                .or(current.cached_tokens);
            current.total_tokens = current.input_tokens.saturating_add(current.output_tokens);
            log.usage_partial = !(log.usage_input_known && log.usage_output_known);
        }
    }
    Ok(())
}
struct Observer {
    context: Arc<Context>,
    buffer: Vec<u8>,
    terminal: bool,
    failed: bool,
    signatures: std::collections::BTreeMap<u64, String>,
}
impl Observer {
    fn new(context: Arc<Context>) -> Self {
        Self {
            context,
            buffer: Vec::new(),
            terminal: false,
            failed: false,
            signatures: std::collections::BTreeMap::new(),
        }
    }
    fn feed(&mut self, chunk: &[u8], log: &mut ApiRequestLog) -> Result<()> {
        // Chunk through input instead of copying an arbitrarily large network frame.
        for byte in chunk {
            self.buffer.push(*byte);
            if self.buffer.len() > EVENT_LIMIT {
                return Err("API 流事件超过限制".into());
            }
            if self.buffer.ends_with(b"\n\n") || self.buffer.ends_with(b"\r\n\r\n") {
                let data = std::mem::take(&mut self.buffer);
                self.event(&data, log)?;
            }
        }
        Ok(())
    }
    fn event(&mut self, event: &[u8], log: &mut ApiRequestLog) -> Result<()> {
        let event = std::str::from_utf8(event).map_err(|_| "API 流编码无效")?;
        let data = event
            .lines()
            .filter_map(|line| {
                line.strip_prefix("data:")
                    .map(|s| s.strip_prefix(' ').unwrap_or(s))
            })
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() {
            return Ok(());
        }
        if data == "[DONE]" {
            return Ok(());
        }
        let value: Value = serde_json::from_str(&data).map_err(|_| "API 流事件格式无效")?;
        let kind = value["type"].as_str().unwrap_or("");
        if self.context.snapshot.source.protocol == Protocol::Messages {
            if kind == "content_block_delta" && value["delta"]["type"] == "signature_delta" {
                let index = value["index"].as_u64().ok_or("签名事件缺少块索引")?;
                let part = value["delta"]["signature"]
                    .as_str()
                    .ok_or("签名事件缺少内容")?;
                if self.signatures.len() >= 128 && !self.signatures.contains_key(&index) {
                    return Err("签名块超过限制".into());
                }
                if self
                    .signatures
                    .values()
                    .map(String::len)
                    .sum::<usize>()
                    .saturating_add(part.len())
                    > EVENT_LIMIT
                {
                    return Err("签名缓冲超过限制".into());
                }
                let signature = self.signatures.entry(index).or_default();
                if signature.len().saturating_add(part.len()) > EVENT_LIMIT {
                    return Err("签名超过限制".into());
                }
                signature.push_str(part);
                // A signature_delta may be only one fragment. Never bind fragments.
                return Ok(());
            }
            if kind == "content_block_stop" {
                if let Some(signature) = value["index"]
                    .as_u64()
                    .and_then(|index| self.signatures.remove(&index))
                {
                    self.context.sources.contexts(
                        &self.context.snapshot.scope(),
                        &[context_hash("signature", &signature)],
                        true,
                    )?;
                }
            }
            if kind == "message_stop" && !self.signatures.is_empty() {
                return Err("签名块缺少结束事件".into());
            }
        }
        if matches!(
            kind,
            "response.completed"
                | "response.failed"
                | "response.incomplete"
                | "message_stop"
                | "error"
        ) {
            self.terminal = true;
        }
        if matches!(kind, "response.failed" | "response.incomplete" | "error") {
            self.failed = true;
        }
        record_output(&self.context, &value, log)
    }
    fn finish(&mut self, log: &mut ApiRequestLog) -> Result<()> {
        if !self.buffer.iter().all(u8::is_ascii_whitespace) {
            let data = std::mem::take(&mut self.buffer);
            self.event(&data, log)?;
        }
        if !self.terminal {
            return Err("API 流缺少结束事件".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
