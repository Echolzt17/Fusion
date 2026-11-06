use super::*;
use fusion_report_protocol::{SendAttempt, Transport};
use http_body::{Body as HttpBody, Frame, SizeHint};
use std::{
    pin::Pin,
    sync::Mutex as StdMutex,
    task::{Context as TaskContext, Poll},
};

pub(super) type Telemetry = Arc<StdMutex<Transport>>;

// Moves the existing body without copying its contents. Size hint and frames
// remain unchanged. Producer progress is never treated as dispatch evidence.
struct MeteredBody {
    inner: reqwest::Body,
    telemetry: Telemetry,
    index: usize,
    started: Instant,
}
impl HttpBody for MeteredBody {
    type Data = Bytes;
    type Error = reqwest::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<Option<std::result::Result<Frame<Bytes>, Self::Error>>> {
        let frame = Pin::new(&mut self.inner).poll_frame(cx);
        if let Poll::Ready(Some(Ok(frame))) = &frame {
            if let Some(bytes) = frame.data_ref().filter(|b| !b.is_empty()) {
                let mut t = self.telemetry.lock().unwrap();
                let a = &mut t.attempts[self.index];
                let ms = self.started.elapsed().as_millis() as u64;
                a.body_bytes_yielded = a.body_bytes_yielded.saturating_add(bytes.len() as u64);
                a.body_first_yield_ms.get_or_insert(ms);
                a.body_last_yield_ms = Some(ms);
            }
        }
        frame
    }
    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }
}
struct AttemptGuard {
    telemetry: Telemetry,
    index: usize,
    started: Instant,
}
impl Drop for AttemptGuard {
    fn drop(&mut self) {
        self.telemetry.lock().unwrap().attempts[self.index].elapsed_ms =
            self.started.elapsed().as_millis() as u64;
    }
}
async fn attempt(
    client: &reqwest::Client,
    mut request: reqwest::Request,
    telemetry: &Telemetry,
) -> std::result::Result<reqwest::Response, reqwest::Error> {
    let started = Instant::now();
    let index = {
        let mut t = telemetry.lock().unwrap();
        let index = t.attempts.len();
        t.attempts.push(SendAttempt {
            result: "cancelled".into(),
            ..Default::default()
        });
        index
    };
    let _guard = AttemptGuard {
        telemetry: telemetry.clone(),
        index,
        started,
    };
    if let Some(inner) = request.body_mut().take() {
        *request.body_mut() = Some(reqwest::Body::wrap(MeteredBody {
            inner,
            telemetry: telemetry.clone(),
            index,
            started,
        }));
    }
    let result = client.execute(request).await;
    let mut t = telemetry.lock().unwrap();
    let a = &mut t.attempts[index];
    a.result = match &result {
        Ok(_) => "headers",
        Err(e) if e.is_connect() => "connect_failure",
        Err(_) => "send_failure",
    }
    .into();
    a.network_error_kind = result
        .as_ref()
        .err()
        .and_then(|e| serde_json::to_value(NetworkErrorKind::classify(e)).ok())
        .and_then(|v| v.as_str().map(str::to_owned));
    result
}
pub(super) async fn send_observed(
    ctx: &Context,
    r: reqwest::RequestBuilder,
    retry_client: Option<reqwest::Client>,
    timeout: Duration,
    telemetry: Telemetry,
    tracking: Option<runtime::RequestHandle>,
) -> std::result::Result<reqwest::Response, NetworkFailure> {
    let (client, request) = r.build_split();
    let request = request.map_err(|e| NetworkFailure::transport(&e, false))?;
    telemetry.lock().unwrap().request_bytes =
        request.body().map_or(Some(0), |b| b.size_hint().exact());
    // Clone BEFORE wrapping: only existing reusable in-memory bodies qualify.
    let reconnect = request.try_clone();
    let sending = async {
        match (attempt(&client, request, &telemetry).await, reconnect) {
            (Err(error), Some(reconnect)) if transient_connect_failure(&error) => {
                telemetry.lock().unwrap().reconnect_started = true;
                if let Some(t) = &tracking {
                    t.phase("reconnecting");
                }
                let jitter = (uuid::Uuid::new_v4().as_u128() % 2001) as u64;
                tokio::time::sleep(Duration::from_millis(3000 + jitter)).await;
                let result = attempt(
                    retry_client.as_ref().unwrap_or(&client),
                    reconnect,
                    &telemetry,
                )
                .await;
                telemetry.lock().unwrap().reconnect_recovered = result.is_ok();
                result
            }
            (result, _) => result,
        }
    };
    let result = tokio::select! {
        biased;
        _ = ctx.cancel.cancelled() => Err(NetworkFailure::stopped()),
        result = tokio::time::timeout(timeout, sending) => match result {
            Ok(Ok(r)) => Ok(r),
            Ok(Err(e)) => Err(NetworkFailure::transport(&e, false)),
            Err(_) => {
                // During backoff the first connector already proved non-dispatch.
                let waiting = { let t=telemetry.lock().unwrap(); t.reconnect_started && t.attempts.len()==1 && t.attempts[0].result=="connect_failure" };
                Err(NetworkFailure::timeout(if waiting { FailureStage::Connect } else { FailureStage::ResponseHeaders }))
            },
        }
    };
    if result
        .as_ref()
        .is_err_and(|e| e.kind == Some(NetworkErrorKind::Timeout))
    {
        if let Some(a) = telemetry
            .lock()
            .unwrap()
            .attempts
            .last_mut()
            .filter(|a| a.result == "cancelled")
        {
            a.result = "deadline".into();
        }
    }
    result
}
