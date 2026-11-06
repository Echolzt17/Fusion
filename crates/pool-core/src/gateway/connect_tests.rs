use super::tests::{ctx, serve};
use super::*;
use crate::tests::fixture;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

// Stall the SOCKS greeting before any HTTP bytes can be sent. Later connections
// reach a local fake upstream. No system proxy, real account or external service.
async fn flaky_proxy(
    destination: SocketAddr,
    failures: usize,
) -> (reqwest::Client, Arc<AtomicUsize>, Arc<tokio::sync::Notify>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = format!("socks5h://{}", listener.local_addr().unwrap());
    let attempts = Arc::new(AtomicUsize::new(0));
    let observed = attempts.clone();
    let failed = Arc::new(tokio::sync::Notify::new());
    let signal = failed.clone();
    let task = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut greeting = [0; 2];
            socket.read_exact(&mut greeting).await.unwrap();
            assert_eq!(greeting[0], 5);
            let mut methods = vec![0; greeting[1] as usize];
            socket.read_exact(&mut methods).await.unwrap();
            let attempt = observed.fetch_add(1, Ordering::SeqCst) + 1;
            if attempt <= failures {
                // Wait for the client's connect deadline to close this attempt.
                let mut extra = [0];
                assert_eq!(socket.read(&mut extra).await.unwrap_or(0), 0);
                signal.notify_one();
                continue;
            }
            socket.write_all(&[5, 0]).await.unwrap();
            let mut connect = [0; 5];
            socket.read_exact(&mut connect).await.unwrap();
            assert_eq!(&connect[..4], &[5, 1, 0, 3]);
            let mut target = vec![0; connect[4] as usize + 2];
            socket.read_exact(&mut target).await.unwrap();
            assert_eq!(&target[..target.len() - 2], b"fixture.invalid");
            let mut upstream = TcpStream::connect(destination).await.unwrap();
            socket.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 80]).await.unwrap();
            let _ = tokio::io::copy_bidirectional(&mut socket, &mut upstream).await;
        }
    });
    let client = reqwest::Client::builder().no_proxy()
        .proxy(reqwest::Proxy::all(proxy).unwrap())
        .retry(reqwest::retry::never()).pool_max_idle_per_host(0)
        .connect_timeout(Duration::from_millis(100)).build().unwrap();
    (client, attempts, failed, task)
}

#[tokio::test]
async fn transient_handshake_failure_reconnects_once_and_sends_identical_business_request_once() {
    let (_dir, accounts) = fixture();
    let business = Arc::new(AtomicUsize::new(0));
    let hits = business.clone();
    let (upstream, server) = serve(Router::new().route("/responses", post(
        move |headers: HeaderMap, Json(body): Json<Value>| {
            hits.fetch_add(1, Ordering::SeqCst);
            async move {
                assert_eq!(headers["authorization"], "Bearer fake-token");
                assert_eq!(headers["chatgpt-account-id"], "fake-account");
                assert_eq!(headers["session_id"], "fake-session");
                assert_eq!(body, json!({"input":"fake-prompt", "stream":true}));
                // A successful connection's generation may outlive its 100ms
                // connector deadline; only the original send deadline applies.
                tokio::time::sleep(Duration::from_millis(250)).await;
                "done"
            }
        }
    ))).await;
    let (client, attempts, _, proxy) = flaky_proxy(upstream[7..].parse().unwrap(), 1).await;
    let context = ctx(accounts, upstream);
    let telemetry:transport::Telemetry=Default::default();
    let response = transport::send_observed(&context, client.post("http://fixture.invalid/responses")
        .bearer_auth("fake-token").header("chatgpt-account-id", "fake-account")
        .header("session_id", "fake-session").json(&json!({"input":"fake-prompt", "stream":true})), Some(client.clone()), Duration::from_secs(180), telemetry.clone(), None)
        .await.unwrap();
    assert_eq!(response.text().await.unwrap(), "done");
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(business.load(Ordering::SeqCst), 1);
    let t=telemetry.lock().unwrap();
    assert!(t.reconnect_started && t.reconnect_recovered && t.valid());
    assert_eq!(t.attempts.len(),2);
    assert_eq!(t.attempts[0].body_bytes_yielded,0);
    assert_eq!(t.attempts[1].body_bytes_yielded,t.request_bytes.unwrap());
    assert!(!serde_json::to_string(&*t).unwrap().contains("fake-"));
    proxy.abort(); server.abort();
}

#[tokio::test]
async fn reconnect_is_bounded_and_streaming_bodies_are_not_retried() {
    let (_dir, accounts) = fixture();
    for cloneable in [true, false] {
        let (client, attempts, _, proxy) = flaky_proxy("127.0.0.1:1".parse().unwrap(), usize::MAX).await;
        let context = ctx(accounts.clone(), "http://fixture.invalid".into());
        let body = if cloneable { reqwest::Body::from("fake-prompt") } else {
            reqwest::Body::wrap_stream(futures_util::stream::once(async {
                Ok::<_, std::io::Error>(Bytes::from_static(b"fake-prompt"))
            }))
        };
        let failure = send(&context, client.post("http://fixture.invalid/responses").body(body)).await.unwrap_err();
        assert_eq!(failure.stage, FailureStage::Connect);
        assert_eq!(attempts.load(Ordering::SeqCst), if cloneable { 2 } else { 1 });
        assert!(!failure.message.contains("fake-"));
        proxy.abort();
    }
}

#[tokio::test]
async fn cancellation_and_original_deadline_stop_the_reconnect_delay() {
    let (_dir, accounts) = fixture();
    for cancel in [true, false] {
        let (client, attempts, failed, proxy) = flaky_proxy("127.0.0.1:1".parse().unwrap(), usize::MAX).await;
        let context = ctx(accounts.clone(), "http://fixture.invalid".into());
        let token = context.cancel.clone();
        let sending = tokio::spawn(async move {
            send_timeout(&context, client.post("http://fixture.invalid/responses").body("fake-prompt"),
                if cancel { Duration::from_secs(3) } else { Duration::from_millis(100) }).await
        });
        tokio::time::timeout(Duration::from_secs(2), failed.notified()).await.unwrap();
        if cancel { token.cancel(); }
        let failure = tokio::time::timeout(Duration::from_secs(1), sending).await.unwrap().unwrap().unwrap_err();
        assert_eq!(failure.code, if cancel { "service_stopped" } else { "upstream_header_timeout" });
        // Wait past the backoff to catch a detached retry after cancellation.
        tokio::time::sleep(Duration::from_millis(5100)).await;
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        proxy.abort();
    }
}

#[tokio::test]
async fn sent_request_without_response_headers_and_http_rejections_are_never_replayed() {
    let (_dir, accounts) = fixture();
    for response in [None, Some("429 Too Many Requests"), Some("502 Bad Gateway")] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (client, attempts, _, proxy) = flaky_proxy(listener.local_addr().unwrap(), 0).await;
        let upstream = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            assert!(socket.read(&mut request).await.unwrap() > 0);
            if let Some(status) = response {
                socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
            }
            // EOF before any response headers is still uncertain execution.
            socket.shutdown().await.unwrap();
        });
        let context = ctx(accounts.clone(), "http://fixture.invalid".into());
        let result = send(&context, client.post("http://fixture.invalid/responses").body("fake-prompt")).await;
        match response {
            Some("429 Too Many Requests") => assert_eq!(result.unwrap().status(), StatusCode::TOO_MANY_REQUESTS),
            Some(_) => assert_eq!(result.unwrap().status(), StatusCode::BAD_GATEWAY),
            None => assert_eq!(result.unwrap_err().stage, FailureStage::ResponseHeaders),
        }
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        upstream.await.unwrap(); proxy.abort();
    }
}
