use super::*;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

// Charge actual buffered bytes. Serialize collection so partially buffered
// unknown-length uploads cannot deadlock one another under memory pressure.
// Upstream requests and responses remain concurrent.
pub(super) struct Admission {
    pub slots: Arc<Semaphore>,
    waiting: Arc<Semaphore>,
    bytes: Arc<Semaphore>,
    uploads: Mutex<()>,
    budget: usize,
    idle: Duration,
    total: Duration,
}
impl Default for Admission {
    fn default() -> Self {
        Self::new(
            8,
            256 * 1024 * 1024,
            Duration::from_secs(15),
            Duration::from_secs(60),
        )
    }
}
impl Admission {
    fn new(slots: usize, budget: usize, idle: Duration, total: Duration) -> Self {
        Self {
            slots: Arc::new(Semaphore::new(slots)),
            waiting: Arc::new(Semaphore::new(32)),
            bytes: Arc::new(Semaphore::new(budget)),
            uploads: Mutex::new(()),
            budget,
            idle,
            total,
        }
    }
}
struct Rejection(StatusCode, &'static str, &'static str);
impl Rejection {
    fn response(self, audit: &mut Audit) -> Response { audit.error(self.0, self.1, self.2) }
}
pub(super) async fn admit(ctx: &Context, req: axum::extract::Request, next: Next) -> Response {
    // This layer runs only after local authentication. One audit owns every accepted request,
    // including failures before an extractor/handler can take over.
    let operation = match req.uri().path() {
        "/v1/responses" => "responses", "/v1/responses/compact" => "compact",
        "/v1/images/generations" => "image_generate", "/v1/images/edits" => "image_edit",
        "/v1/models" => "models", _ => "request",
    };
    let mut audit = Audit::new(ctx.clone(), operation);
    if ctx.draining.is_cancelled() || ctx.cancel.is_cancelled() {
        return audit.error(StatusCode::SERVICE_UNAVAILABLE, "service_stopped", "服务正在停止接收请求，未发送至上游");
    }
    audit.diagnostic.failure_stage=Some(FailureStage::IngressQueue);
    audit.phase("ingress_queue");
    let admission = &ctx.admission;
    let Ok(waiting) = admission.waiting.clone().try_acquire_owned() else {
        return audit.error(StatusCode::SERVICE_UNAVAILABLE, "ingress_busy", "本机等待队列已满，未发送至上游");
    };
    let cap = if req.uri().path() == "/v1/images/generations" { 32*1024*1024 } else { 256*1024*1024 };
    let declared = req.headers().get("content-length").and_then(|v|v.to_str().ok()).and_then(|s|s.parse::<usize>().ok());
    let size = if req.method()==http::Method::GET { 0 } else { declared.unwrap_or(cap.min(admission.budget)) };
    if size > cap || size > admission.budget {
        audit.diagnostic.failure_stage=Some(FailureStage::RequestValidation);
        return audit.error(StatusCode::PAYLOAD_TOO_LARGE, "request_too_large", "请求超过大小限制，未发送至上游");
    }
    let (mut request, registration) = ctx.requests.register(operation, audit.id.clone(), audit.start, audit.started_at);
    request.phase("ingress_queue");
    let slot = tokio::select! {
        _ = ctx.cancel.cancelled() => Err(Rejection(StatusCode::SERVICE_UNAVAILABLE,"service_stopped","服务已停止，未发送至上游")),
        _ = request.cancel.cancelled() => Err(Rejection(StatusCode::from_u16(499).unwrap(),"cancelled","本机请求已取消，未发送至上游")),
        result = tokio::time::timeout(crate::scheduler::REQUEST_QUEUE_WAIT, admission.slots.clone().acquire_owned()) =>
            result.ok().and_then(|r|r.ok()).ok_or(Rejection(StatusCode::SERVICE_UNAVAILABLE,"ingress_timeout","入口排队超时，未发送至上游")),
    };
    drop(waiting);
    let slot = match slot { Ok(slot)=>slot, Err(error)=>return error.response(&mut audit) };
    audit.phase("uploading");
    request.ingress_queue_ms=audit.diagnostic.execution.as_ref().and_then(|x|x.ingress_queue_ms);
    audit.diagnostic.failure_stage=Some(FailureStage::Upload);
    request.phase("uploading");
    let (mut parts, body) = req.into_parts();
    let read = async {
        let _upload = admission.uploads.lock().await;
        let initial = if declared.is_some() { size } else { 0 };
        let mut bytes = admission.bytes.clone().acquire_many_owned(initial as u32).await
            .map_err(|_| Rejection(StatusCode::SERVICE_UNAVAILABLE,"service_stopped","服务已停止，未发送至上游"))?;
        let mut chunks = body.into_data_stream();
        let mut out = Vec::new();
        let mut uploading = Duration::ZERO;
        loop {
            let started = Instant::now();
            let remaining=admission.total.saturating_sub(uploading);
            let next = tokio::time::timeout(admission.idle.min(remaining), chunks.next()).await.map_err(|_| {
                if remaining<=admission.idle { Rejection(StatusCode::REQUEST_TIMEOUT,"upload_timeout","正文实际上传超时，未发送至上游") }
                else { Rejection(StatusCode::REQUEST_TIMEOUT,"upload_idle_timeout","等待正文数据超时，未发送至上游") }
            })?;
            uploading += started.elapsed();
            match next {
                None => return Ok((out, bytes)),
                Some(Err(_)) => return Err(Rejection(StatusCode::BAD_REQUEST,"body_read_error","正文传输读取失败，未发送至上游")),
                Some(Ok(b)) => {
                    if out.len().saturating_add(b.len()) > size {
                        return Err(Rejection(StatusCode::PAYLOAD_TOO_LARGE,"request_too_large","正文超过大小限制，未发送至上游"));
                    }
                    if declared.is_none() {
                        bytes.merge(admission.bytes.clone().acquire_many_owned(b.len() as u32).await
                            .map_err(|_| Rejection(StatusCode::SERVICE_UNAVAILABLE,"service_stopped","服务已停止，未发送至上游"))?);
                    }
                    out.extend_from_slice(&b);
                }
            }
        }
    };
    let data = tokio::select! {
        _ = ctx.cancel.cancelled() => Err(Rejection(StatusCode::SERVICE_UNAVAILABLE,"service_stopped","服务已停止，未发送至上游")),
        _ = request.cancel.cancelled() => Err(Rejection(StatusCode::from_u16(499).unwrap(),"cancelled","本机请求已取消，未发送至上游")),
        r = tokio::time::timeout(crate::scheduler::REQUEST_QUEUE_WAIT, read) => r.unwrap_or(Err(Rejection(StatusCode::REQUEST_TIMEOUT,"upload_wait_timeout","正文处理等待超时，未发送至上游"))),
    };
    let (data, bytes) = match data { Ok(data)=>data, Err(error)=>return error.response(&mut audit) };
    request.hold_upload(bytes);
    audit.diagnostic.failure_stage=Some(FailureStage::RequestValidation);
    let mut quoted=false;let mut escaped=false;let mut structure=0usize;
    for b in &data {
        if quoted {
            if escaped { escaped=false; } else if *b==b'\\' { escaped=true; } else if *b==b'"' { quoted=false; }
        } else if *b==b'"' { quoted=true; } else if matches!(*b,b'{'|b'['|b','|b':') { structure+=1; }
        if structure>250_000 {
            return audit.error(StatusCode::PAYLOAD_TOO_LARGE,"json_structure_limit","JSON 结构超过限制，未发送至上游");
        }
    }
    parts.extensions.insert(request.clone());
    audit.handoff=Some(request.audit_claimed.clone());
    let response = tokio::select! {
        biased;
        _ = request.cancel.cancelled() => return audit.error(StatusCode::from_u16(499).unwrap(),"cancelled","本机请求已取消，未自动重放"),
        response = next.run(axum::extract::Request::from_parts(parts, Body::from(data))) => response,
    };
    request.finish_upload();
    let response = if !request.audit_claimed.load(std::sync::atomic::Ordering::Acquire) {
        audit.error(response.status(),"invalid_request","请求格式或内容类型无效，未发送至上游")
    } else { response };
    hold(response, slot, request, registration, ctx.cancel.clone())
}
struct CancelOnDrop(CancellationToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) { self.0.cancel(); }
}
fn hold(response: Response, slot: OwnedSemaphorePermit,
    request: runtime::RequestHandle, registration: runtime::Registration, service_cancel: CancellationToken) -> Response {
    let (parts, body) = response.into_parts();
    // A bounded producer watches cancellation even when a slow/disconnected client
    // is no longer polling its response. At most one transport chunk is buffered.
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    let cancel_on_drop = CancelOnDrop(request.cancel.clone());
    tokio::spawn(async move {
        let mut stream = body.into_data_stream();
        loop {
            let chunk = tokio::select! {
                biased;
                _ = request.cancel.cancelled() => break,
                _ = service_cancel.cancelled() => break,
                _ = sender.closed() => break,
                chunk = stream.next() => match chunk { Some(chunk) => chunk, None => break },
            };
            tokio::select! {
                biased;
                _ = request.cancel.cancelled() => break,
                _ = service_cancel.cancelled() => break,
                result = sender.send(chunk) => if result.is_err() { break; },
            }
        }
        // Drop the handler/body (including its lease) before acknowledging release.
        drop(stream);
        drop(slot);
        drop(registration);
    });
    let stream = async_stream::stream! {
        let _cancel_on_drop = cancel_on_drop;
        while let Some(chunk) = receiver.recv().await { yield chunk; }
    };
    Response::from_parts(parts, Body::from_stream(stream))
}

#[cfg(test)]
mod tests;
