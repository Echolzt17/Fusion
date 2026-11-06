use super::tests::{ctx, serve};
use super::*;
use crate::tests::{add, fixture};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn raw_server(responses: Vec<Vec<u8>>) -> (String, Arc<AtomicUsize>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let task = tokio::spawn(async move {
        for response in responses {
            let (mut stream, _) = listener.accept().await.unwrap();
            count.fetch_add(1, Ordering::SeqCst);
            let mut scratch = [0u8; 8192];
            let _ = stream.read(&mut scratch).await;
            stream.write_all(&response).await.unwrap();
            stream.shutdown().await.unwrap();
        }
    });
    (url, calls, task)
}

#[tokio::test]
async fn connection_and_response_header_deadlines_remain_distinct_without_replay() {
    let (_dir, accounts) = fixture();
    for connecting in [true, false] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let stop = CancellationToken::new();
        let done = stop.clone();
        let upstream = tokio::spawn(async move {
            loop {
                let (mut socket, _) = tokio::select! {
                    _ = done.cancelled() => break,
                    result = listener.accept() => result.unwrap(),
                };
                let mut scratch = [0u8; 8192];
                if socket.read(&mut scratch).await.unwrap_or(0) > 0 {
                    count.fetch_add(1, Ordering::SeqCst);
                }
                // TLS can reconnect before dispatch; a sent HTTP request cannot.
                tokio::select! {
                    _ = done.cancelled() => break,
                    _ = socket.read(&mut scratch) => {},
                }
            }
        });
        let client = reqwest::Client::builder().no_proxy()
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_millis(100)).build().unwrap();
        let url = format!("{}://{address}/private-path", if connecting { "https" } else { "http" });
        let context = ctx(accounts.clone(), format!("http://{address}"));
        let failure = send_timeout(&context, client.post(url).body("private-prompt"),
            if connecting { Duration::from_secs(7) } else { Duration::from_millis(100) })
            .await.unwrap_err();
        assert_eq!(failure.kind, Some(NetworkErrorKind::Timeout));
        assert_eq!(failure.stage, if connecting { FailureStage::Connect } else { FailureStage::ResponseHeaders });
        assert!(failure.message.contains(if connecting { "建立上游连接超时" } else { "等待上游响应头超时" }));
        assert!(failure.message.contains("未自动重放"));
        assert!(!failure.message.contains("private-"));
        assert_eq!(calls.load(Ordering::SeqCst), if connecting { 2 } else { 1 });
        stop.cancel(); upstream.await.unwrap();
    }
}

#[tokio::test]
async fn missing_content_type_stream_survives_sixteen_seconds_before_first_event() {
    // Incident 5ac69404: 200 + untyped prefix, then about 15s until EOF.
    // Exercise the real TCP path past that interval: the ingress upload timer
    // must not close an upstream stream waiting for its first valid SSE event.
    let (_dir, accounts) = fixture();
    add(&accounts, "a");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0u8; 8192];
        assert!(socket.read(&mut request).await.unwrap() > 0);
        count.fetch_add(1, Ordering::SeqCst);
        let prefix = ": keepalive\r\n\r\n";
        let event = "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"fixture-delayed-event\",\"status\":\"completed\",\"output\":[]}}\r\n\r\n";
        let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{prefix}", prefix.len()+event.len());
        socket.write_all(head.as_bytes()).await.unwrap();
        tokio::time::sleep(Duration::from_secs(16)).await;
        socket.write_all(event.as_bytes()).await.unwrap();
        socket.shutdown().await.unwrap();
    });
    let context = ctx(accounts.clone(), address);
    let (base, gateway) = serve(router(context.clone())).await;
    let response = accounts.local_http.post(format!("{base}/v1/responses"))
        .bearer_auth("test-key").json(&json!({"input":"fixture", "stream":true}))
        .timeout(Duration::from_secs(25)).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.text().await.unwrap().contains("response.completed"));
    server.await.unwrap();
    let logs = accounts.store.logs().unwrap();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].status, "success");
    let diagnostic = logs[0].diagnostic.as_ref().unwrap();
    assert_eq!(diagnostic.response_type, Some(ResponseType::Missing));
    assert_eq!(diagnostic.detected_response_type, Some(ResponseType::Sse));
    assert!(diagnostic.execution.as_ref().unwrap().max_read_idle_ms.unwrap() >= 15_000);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(!context.scheduler.is_active("a"));
    gateway.abort();
}

#[tokio::test]
async fn refused_connection_keeps_only_category_and_releases_account_slot() {
    let (_dir, accounts) = fixture();
    add(&accounts, "a");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let context = ctx(accounts.clone(), upstream);
    let (base, gateway) = serve(router(context.clone())).await;
    let response = accounts
        .local_http
        .post(format!("{base}/v1/responses"))
        .bearer_auth("test-key")
        .json(&json!({"input":"private-prompt"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert!(response.text().await.unwrap().contains("连接被拒绝"));
    let logs = accounts.store.logs().unwrap();
    assert_eq!(logs.len(), 1);
    let diagnostic = logs[0].diagnostic.as_ref().unwrap();
    assert_eq!(diagnostic.failure_stage, Some(FailureStage::Connect));
    assert_eq!(
        diagnostic.network_error_kind,
        Some(NetworkErrorKind::ConnectionRefused)
    );
    assert_eq!(diagnostic.upstream_status, None);
    let x = diagnostic.execution.as_ref().unwrap();
    assert_eq!(x.local_http_status, Some(502));
    assert!(x.upstream_attempted);
    assert!(!x.headers_received && !x.first_byte_received && !x.terminal_received);
    assert!(x.queue_ms.is_some() && x.authorization_ms.is_some() && x.headers_wait_ms.is_some());
    assert!(!context.scheduler.is_active("a"));
    assert!(!serde_json::to_string(&logs)
        .unwrap()
        .contains("private-prompt"));
    gateway.abort();
}

#[tokio::test]
async fn proxy_tunnel_and_tls_failures_are_classified_without_replaying_or_leaking_urls() {
    let (_dir, accounts) = fixture();
    for (status, expected) in [
        (
            "407 Proxy Authentication Required",
            NetworkErrorKind::ProxyAuthentication,
        ),
        ("502 Bad Gateway", NetworkErrorKind::ProxyTunnel),
    ] {
        let raw = format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .into_bytes();
        let (url, count, task) = raw_server(vec![raw]).await;
        let client = reqwest::Client::builder()
            .no_proxy()
            .retry(reqwest::retry::never())
            .proxy(reqwest::Proxy::all(&url).unwrap())
            .build()
            .unwrap();
        let context = ctx(accounts.clone(), url);
        let failure = send(
            &context,
            client
                .post("https://fixture.invalid/private-url?key=private-secret")
                .body("private-prompt"),
        )
        .await
        .unwrap_err();
        assert_eq!(failure.stage, FailureStage::Connect);
        assert_eq!(failure.kind, Some(expected));
        assert!(!failure.message.contains("private-"));
        assert_eq!(count.load(Ordering::SeqCst), 1);
        task.await.unwrap();
    }
    // An HTTPS client receiving plaintext exercises an actual typed rustls handshake error.
    let (url, count, task) = raw_server(vec![
        b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n".to_vec(),
    ])
    .await;
    let context = ctx(accounts.clone(), url.clone());
    let failure = send(
        &context,
        accounts
            .local_http
            .get(url.replacen("http://", "https://", 1)),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.kind, Some(NetworkErrorKind::Tls));
    assert_eq!(count.load(Ordering::SeqCst), 1);
    task.await.unwrap();
}

#[tokio::test]
async fn orderly_stream_end_and_truncated_transfer_stay_distinct_and_never_move_or_replay() {
    let (_dir, accounts) = fixture();
    add(&accounts, "a");
    add(&accounts, "b");
    let body = "data: {\"type\":\"response.created\",\"response\":{\"model\":\"fixture-model\"}}\n\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"private-output\"}\n\n";
    let cases = [(true, false), (true, true), (false, false), (false, true)];
    let responses = cases
        .iter()
        .map(|(_, truncated)| {
            // Missing Content-Type reproduces the user's accepted-but-interrupted SSE path.
            let length = if *truncated {
                format!("Content-Length: {}\r\n", body.len() + 50)
            } else {
                String::new()
            };
            format!("HTTP/1.1 200 OK\r\n{length}Connection: close\r\n\r\n{body}").into_bytes()
        })
        .collect();
    let (upstream, calls, task) = raw_server(responses).await;
    let context = ctx(accounts.clone(), upstream);
    let key = binding_key("session", "private-session");
    accounts.store.bind(&key, "a").unwrap();
    let (base, gateway) = serve(router(context.clone())).await;
    let mut seen = std::collections::HashSet::new();
    for (index, (stream, truncated)) in cases.into_iter().enumerate() {
        let response = accounts
            .local_http
            .post(format!("{base}/v1/responses"))
            .bearer_auth("test-key")
            .header("session_id", "private-session")
            .json(&json!({"stream":stream,"input":"private-prompt"}))
            .send()
            .await;
        if let Ok(response) = response {
            let _ = response.bytes().await;
        }
        for _ in 0..100 {
            if accounts.store.logs().unwrap().len() == index + 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let logs = accounts.store.logs().unwrap();
        assert_eq!(logs.len(), index + 1);
        let log = logs.iter().find(|log| !seen.contains(&log.id)).unwrap();
        seen.insert(log.id.clone());
        assert_eq!(
            log.status,
            if stream || truncated {
                "stream_interrupted"
            } else {
                "response_incomplete"
            }
        );
        let diagnostic = log.diagnostic.as_ref().unwrap();
        assert_eq!(
            diagnostic.network_error_kind,
            Some(if truncated {
                NetworkErrorKind::UnexpectedEof
            } else {
                NetworkErrorKind::StreamEnded
            })
        );
        assert_eq!(diagnostic.upstream_status, Some(200));
        assert_eq!(diagnostic.response_type, Some(ResponseType::Missing));
        assert_eq!(diagnostic.detected_response_type, Some(ResponseType::Sse));
        assert_eq!(diagnostic.failure_stage, Some(FailureStage::ResponseRead));
        assert!(!context.scheduler.is_active("a"));
        assert_eq!(accounts.store.binding(&key).unwrap().as_deref(), Some("a"));
        let serialized = serde_json::to_string(&logs).unwrap();
        for secret in ["private-output", "private-prompt", "private-session"] {
            assert!(!serialized.contains(secret));
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    task.await.unwrap();
    gateway.abort();
}
