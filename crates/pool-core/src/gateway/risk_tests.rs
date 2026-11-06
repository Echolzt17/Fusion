use super::tests::{ctx, serve};
use super::*;
use crate::tests::{add, fixture};
use std::sync::atomic::{AtomicUsize, Ordering};

#[tokio::test]
async fn short_cooldown_is_visible_and_recovers_or_cancels_before_any_upstream_send() {
    for ending in ["recovered", "cancelled", "stopped"] {
        let (_d, accounts) = fixture(); add(&accounts, "a"); add(&accounts, "b");
        accounts.store.bind(&binding_key("session", "fixture-cooling"), "a").unwrap();
        accounts.mark_failure("a", 429, 10).await.unwrap();
        let calls = Arc::new(AtomicUsize::new(0)); let count = calls.clone();
        let (upstream, up) = serve(Router::new().route("/responses", post(move |headers: HeaderMap| {
            let count = count.clone(); async move {
                count.fetch_add(1, Ordering::SeqCst);
                assert_eq!(headers["chatgpt-account-id"], "chatgpt-a");
                ([("content-type", "text/event-stream")],
                    "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[]}}\n\n")
            }
        }))).await;
        let context = ctx(accounts.clone(), upstream);
        let (base, gateway) = serve(router(context.clone())).await;
        let pending = tokio::spawn(async move {
            reqwest::Client::new().post(format!("{base}/v1/responses")).bearer_auth("test-key")
                .header("session_id", "fixture-cooling").json(&json!({"stream":false}))
                .send().await.unwrap()
        });
        let id = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(request) = context.requests.snapshot().into_iter().find(|r| r.phase == "cooldown") {
                    assert_eq!(request.account.as_deref(), Some("a"));
                    break request.id;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        assert!(!pending.is_finished());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(!context.scheduler.is_active("a") && !context.scheduler.is_active("b"));
        match ending {
            "recovered" => {
                let mut account = accounts.store.account("a").unwrap();
                account.cooldown_until = crate::now() - 1;
                accounts.store.save_account(&account).unwrap();
            }
            "cancelled" => context.requests.cancel(&id).await.unwrap(),
            _ => context.cancel.cancel(),
        }
        let response = tokio::time::timeout(Duration::from_secs(3), pending).await.unwrap().unwrap();
        assert_eq!(response.status().as_u16(), match ending { "recovered" => 200, "cancelled" => 499, _ => 503 });
        response.bytes().await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while !context.requests.snapshot().is_empty() { tokio::task::yield_now().await; }
        }).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), usize::from(ending == "recovered"));
        assert_eq!(context.admission.slots.available_permits(), 8);
        assert!(!context.scheduler.is_active("a") && !context.scheduler.is_active("b"));
        assert_eq!(accounts.store.binding(&binding_key("session", "fixture-cooling")).unwrap().as_deref(), Some("a"));
        gateway.abort(); up.abort();
    }
}

#[tokio::test]
async fn rate_limited_request_returns_immediately_and_is_never_replayed() {
    let (_d, accounts) = fixture(); add(&accounts, "a"); add(&accounts, "b");
    let calls = Arc::new(AtomicUsize::new(0)); let count = calls.clone();
    let (upstream, up) = serve(Router::new().route("/responses", post(move || {
        let count = count.clone(); async move {
            count.fetch_add(1, Ordering::SeqCst);
            (StatusCode::TOO_MANY_REQUESTS, [("retry-after", "10")], "private-limit-body")
        }
    }))).await;
    let context = ctx(accounts.clone(), upstream);
    context.scheduler.select_account(Some("a".into())).unwrap();
    let (base, gateway) = serve(router(context.clone())).await;
    let response = tokio::time::timeout(Duration::from_secs(3), reqwest::Client::new()
        .post(format!("{base}/v1/responses")).bearer_auth("test-key")
        .header("session_id", "fixture-limit-once").json(&json!({"stream":false})).send()).await.unwrap().unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(!response.text().await.unwrap().contains("private-limit-body"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(!context.scheduler.is_active("a") && !context.scheduler.is_active("b"));
    assert!(accounts.store.account("a").unwrap().cooldown_until > crate::now());
    assert_eq!(accounts.store.binding(&binding_key("session", "fixture-limit-once")).unwrap().as_deref(), Some("a"));
    gateway.abort(); up.abort();
}

#[tokio::test]
async fn targeted_cancel_releases_queue_auth_headers_and_backpressured_stream_without_replay() {
    for phase in ["queued", "authorization", "headers", "streaming"] {
        let (_d, accounts) = fixture(); add(&accounts, "a"); add(&accounts, "b");
        accounts.store.bind(&binding_key("session", "fixture-cancel"), "a").unwrap();
        let calls = Arc::new(AtomicUsize::new(0)); let count = calls.clone();
        let (upstream, up) = serve(Router::new().route("/responses", post(move || {
            let count = count.clone(); async move {
                count.fetch_add(1, Ordering::SeqCst);
                if phase == "headers" { std::future::pending::<()>().await; }
                let stream = async_stream::stream! {
                    loop {
                        yield Ok::<_, std::io::Error>(Bytes::from_static(b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"private-cancel-payload\"}\n\n"));
                    }
                };
                ([("content-type", "text/event-stream")], Body::from_stream(stream))
            }
        }))).await;
        let context = ctx(accounts.clone(), upstream);
        context.scheduler.save_service_settings(18765, 1).unwrap();
        let other = context.scheduler.acquire(Some("b")).await.unwrap();
        let occupied = if phase == "queued" { Some(context.scheduler.acquire(Some("a")).await.unwrap()) } else { None };
        let lock = accounts.lock("a");
        let auth = if phase == "authorization" { Some(lock.lock().await) } else { None };
        let (base, gateway) = serve(router(context.clone())).await;
        let client = tokio::spawn(async move {
            let response = reqwest::Client::new().post(format!("{base}/v1/responses")).bearer_auth("test-key")
                .header("session_id", "fixture-cancel").json(&json!({"stream":true})).send().await;
            // Keep the body alive without reading it: cancellation must not need another client poll.
            if let Ok(response) = response {
                std::future::pending::<()>().await;
                drop(response);
            }
        });
        let id = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(request) = context.requests.snapshot().into_iter().find(|r| r.phase == phase) { break request.id; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        // The local "headers" phase starts before the async HTTP request has
        // reached the fixture. Exercise cancellation of an actual upstream wait,
        // rather than race cancellation against the first network send.
        if matches!(phase, "headers" | "streaming") {
            tokio::time::timeout(Duration::from_secs(3), async {
                while calls.load(Ordering::SeqCst) == 0 { tokio::task::yield_now().await; }
            }).await.expect("fixture upstream must receive the request before cancellation");
        }
        if phase == "streaming" { tokio::time::sleep(Duration::from_millis(100)).await; }
        context.requests.cancel(&id).await.unwrap();
        context.requests.cancel(&id).await.unwrap(); // completed request cancellation is idempotent
        assert!(context.requests.snapshot().is_empty());
        assert_eq!(context.admission.slots.available_permits(), 8);
        assert_eq!(context.scheduler.is_active("a"), phase == "queued");
        assert!(context.scheduler.is_active("b")); // never cancel an unrelated task
        assert_eq!(calls.load(Ordering::SeqCst), usize::from(matches!(phase, "headers" | "streaming")));
        assert_eq!(accounts.store.binding(&binding_key("session", "fixture-cancel")).unwrap().as_deref(), Some("a"));
        let logs = accounts.store.logs().unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].status, "cancelled");
        assert!(!serde_json::to_string(&logs).unwrap().contains("private-cancel-payload"));
        drop(auth); drop(occupied); drop(other);
        client.abort(); gateway.abort(); up.abort();
    }
}

#[tokio::test]
async fn busy_account_queues_past_ten_seconds_then_sends_once_on_original_account() {
    let (_d, accounts) = fixture(); add(&accounts, "a"); add(&accounts, "b");
    let calls = Arc::new(AtomicUsize::new(0)); let count = calls.clone();
    let (upstream, up) = serve(Router::new().route("/responses", post(move |headers: HeaderMap| {
        let count = count.clone(); async move {
            count.fetch_add(1, Ordering::SeqCst);
            assert_eq!(headers["chatgpt-account-id"], "chatgpt-a");
            ([("content-type", "text/event-stream")],
                "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[]}}\n\n")
        }
    }))).await;
    let context = ctx(accounts.clone(), upstream);
    context.scheduler.save_service_settings(18765, 1).unwrap();
    accounts.store.bind(&binding_key("session", "fixture-bound"), "a").unwrap();
    let held = context.scheduler.acquire(Some("a")).await.unwrap();
    let (base, gateway) = serve(router(context.clone())).await;
    let pending = tokio::spawn(async move {
        reqwest::Client::new().post(format!("{base}/v1/responses")).bearer_auth("test-key")
            .header("session_id", "fixture-bound").json(&json!({"stream":false}))
            .send().await.unwrap()
    });
    tokio::time::sleep(Duration::from_secs(11)).await;
    assert!(!pending.is_finished(), "normal work must keep queuing rather than fail at ten seconds");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    drop(held);
    let response = tokio::time::timeout(Duration::from_secs(2), pending).await.unwrap().unwrap();
    assert_eq!(response.json::<Value>().await.unwrap()["status"], "completed");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(!context.scheduler.is_active("a") && !context.scheduler.is_active("b"));
    gateway.abort(); up.abort();
}

#[tokio::test]
async fn completed_event_releases_account_before_client_drops_response_body() {
    let (_d, accounts) = fixture(); add(&accounts, "a");
    let (upstream, up) = serve(Router::new().route("/responses", post(|| async {
        ([("content-type", "text/event-stream")],
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[]}}\n\n")
    }))).await;
    let context = ctx(accounts, upstream);
    let mut headers = HeaderMap::new(); headers.insert("authorization", "Bearer test-key".parse().unwrap());
    let response = responses(State(context.clone()), headers, None, Json(json!({"stream":true}))).await;
    let mut body = response.into_body().into_data_stream();
    assert!(body.next().await.unwrap().unwrap().windows(18).any(|part| part == b"response.completed"));
    assert!(!context.scheduler.is_active("a"), "a completed operation must release its account before downstream body cleanup");
    drop(body); up.abort();
}

#[tokio::test]
async fn disconnected_client_releases_account_while_waiting_for_upstream_headers() {
    use tokio::io::AsyncWriteExt;
    let (_d, accounts) = fixture(); add(&accounts, "a");
    let calls = Arc::new(AtomicUsize::new(0)); let count = calls.clone();
    let (upstream, up) = serve(Router::new().route("/responses", post(move || {
        let count = count.clone(); async move {
            count.fetch_add(1, Ordering::SeqCst);
            std::future::pending::<Response>().await
        }
    }))).await;
    let context = ctx(accounts, upstream);
    let (base, gateway) = serve(router(context.clone())).await;
    let address = base.strip_prefix("http://").unwrap();
    let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
    let body = r#"{"input":"fixture","stream":true}"#;
    socket.write_all(format!("POST /v1/responses HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer test-key\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while calls.load(Ordering::SeqCst) == 0 { tokio::task::yield_now().await; }
    }).await.unwrap();
    assert!(context.scheduler.is_active("a"));
    drop(socket);
    tokio::time::timeout(Duration::from_secs(2), async {
        while context.scheduler.is_active("a") { tokio::time::sleep(Duration::from_millis(10)).await; }
    }).await.expect("disconnected client must release its local account slot");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    gateway.abort(); up.abort();
}

#[tokio::test]
async fn force_stop_releases_request_waiting_on_credential_rotation() {
    let (_d, a) = fixture();
    add(&a, "a");
    let lock = a.lock("a");
    let held = lock.lock().await;
    let c = ctx(a, "http://127.0.0.1:1".into());
    let (base, server) = serve(router(c.clone())).await;
    let task = tokio::spawn(async move {
        reqwest::Client::new()
            .post(format!("{base}/v1/responses"))
            .bearer_auth("test-key")
            .json(&json!({"input":"fixture"}))
            .send()
            .await
            .unwrap()
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while !c.scheduler.is_active("a") {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    c.cancel.cancel();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(!c.scheduler.is_active("a"));
    drop(held);
    server.abort();
}

#[tokio::test]
async fn expired_and_deleted_threads_never_rebind_even_after_restart_and_readd() {
    let (d, a) = fixture();
    add(&a, "a");
    add(&a, "b");
    let key = binding_key("thread", "old-thread");
    a.store.bind(&key, "a").unwrap();
    let db = rusqlite::Connection::open(d.path().join("pool.sqlite")).unwrap();
    db.execute("UPDATE bindings SET expires=0", []).unwrap();
    let c = ctx(a.clone(), String::new());
    c.scheduler.select_account(Some("b".into())).unwrap();
    let input = json!({"client_metadata":{"thread_id":"old-thread"},"input":"continue"});
    assert!(pinned(&c, &HeaderMap::new(), &input)
        .unwrap_err()
        .contains("过期"));
    assert!(a.store.bind(&key, "b").is_err());
    // Writing a different key must not prune expired identities.
    a.store.bind(&binding_key("thread", "other"), "b").unwrap();
    let reopened = crate::storage::Store::open(&d.path().join("pool.sqlite")).unwrap();
    assert!(reopened.binding(&key).is_err());
    a.store.delete_account("a").unwrap();
    add(&a, "a");
    assert!(reopened.binding(&key).unwrap_err().contains("删除"));
    assert!(a.store.bind(&key, "a").is_err());
}

#[tokio::test]
async fn removal_reserves_scheduler_before_waiting_for_account_lock() {
    let (_d, a) = fixture();
    add(&a, "a");
    add(&a, "b");
    let g = Gateway::new(a.clone());
    let lock = a.lock("a");
    let held = lock.lock().await;
    let task = {
        let g = g.clone();
        tokio::spawn(async move { g.remove_account("a").await })
    };
    tokio::time::timeout(Duration::from_secs(1), async {
        while g.scheduler.available(&a.store.account("a").unwrap()) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(g.scheduler.acquire(Some("a")).await.is_err());
    assert_eq!(g.scheduler.acquire(None).await.unwrap().account.id, "b");
    drop(held);
    task.await.unwrap().unwrap();
    assert!(a.store.account("a").is_err());
    let lease = g.scheduler.acquire(Some("b")).await.unwrap();
    assert!(g.remove_account("b").await.is_err());
    assert!(a.store.account("b").is_ok());
    drop(lease);
    g.remove_account("b").await.unwrap();
}

#[tokio::test]
async fn cancelling_pending_removal_releases_its_reservation() {
    let (_d, a) = fixture();
    add(&a, "a");
    let g = Gateway::new(a.clone());
    let lock = a.lock("a");
    let held = lock.lock().await;
    let task = {
        let g = g.clone();
        tokio::spawn(async move { g.remove_account("a").await })
    };
    tokio::time::timeout(Duration::from_secs(1), async {
        while g.scheduler.available(&a.store.account("a").unwrap()) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    task.abort();
    let _ = task.await;
    drop(held);
    assert!(g.scheduler.acquire(None).await.is_ok());
}

#[tokio::test]
async fn sse_whitelisted_errors_update_future_selection_without_replay_or_body_logs() {
    for (code, state, cool) in [
        ("usage_limit_reached", "ready", true),
        ("token_expired", "reauth", false),
        ("server_error", "ready", false),
        ("unknown_code", "ready", false),
    ] {
        for streaming in [false, true] {
            let (_d, a) = fixture();
            add(&a, "a");
            add(&a, "b");
            let calls = Arc::new(AtomicUsize::new(0));
            let n = calls.clone();
            let (upstream, up) = serve(Router::new().route("/responses", post(move || { let n = n.clone(); async move {
                n.fetch_add(1, Ordering::SeqCst);
                ([("content-type", "text/event-stream")], format!("data: {{\"type\":\"response.failed\",\"response\":{{\"error\":{{\"code\":\"{code}\",\"message\":\"private-failure-body\",\"retry_after_seconds\":123}}}}}}\n\n"))
            }}))).await;
            let c = ctx(a.clone(), upstream);
            let (base, server) = serve(router(c.clone())).await;
            let attempts = if code == "server_error" { 2 } else { 1 };
            for _ in 0..attempts {
                let r = reqwest::Client::new()
                    .post(format!("{base}/v1/responses"))
                    .bearer_auth("test-key")
                    .header("session_id", "original")
                    .json(&json!({"input":"private-input", "stream":streaming}))
                    .send()
                    .await
                    .unwrap();
                let _ = r.bytes().await.unwrap();
            }
            assert_eq!(calls.load(Ordering::SeqCst), attempts);
            let account = a.store.account("a").unwrap();
            assert_eq!(account.state, state);
            assert_eq!(account.cooldown_until > crate::now(), cool);
            assert_eq!(
                c.scheduler.available(&account),
                !(cool || state == "reauth")
            );
            assert_eq!(
                a.store
                    .binding(&binding_key("session", "original"))
                    .unwrap()
                    .as_deref(),
                Some("a")
            );
            let logs = serde_json::to_string(&a.store.logs().unwrap()).unwrap();
            assert!(!logs.contains("private-failure-body") && !logs.contains("private-input"));
            let rows=a.store.logs().unwrap();assert_eq!(rows.len(),attempts);
            for log in &rows {
                let d=log.diagnostic.as_ref().unwrap();
                assert_eq!(d.failure_stage,Some(FailureStage::UpstreamRejected));
                let details=d.details.as_ref().unwrap();assert_eq!(details.terminal_event.as_deref(),Some("failed"));
                assert_eq!(details.upstream_failure.as_deref(),Some(match code {"usage_limit_reached"=>"rate_limit","token_expired"=>"authorization","server_error"=>"server",_=>"unknown"}));
            }
            server.abort();
            up.abort();
        }
    }
}

#[tokio::test]
async fn draining_rejects_new_requests_but_allows_inflight_completion_and_restart() {
    let (_d, a) = fixture();
    add(&a, "a");
    let release = CancellationToken::new();
    let gate = release.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let n = calls.clone();
    let (upstream, up) = serve(Router::new().route("/responses", post(move || {
        let (gate, n) = (gate.clone(), n.clone());
        async move {
            n.fetch_add(1, Ordering::SeqCst); gate.cancelled().await;
            ([("content-type", "text/event-stream")], "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"drained\",\"output\":[]}}\n\n")
        }
    }))).await;
    let c = ctx(a.clone(), upstream);
    let g = Gateway::new(a.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut settings = a.store.settings().unwrap();
    settings.port = port;
    a.store.save_settings(&settings).unwrap();
    let stop = c.draining.clone();
    let router = router(c.clone());
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(stop.cancelled_owned())
            .await;
    });
    *g.running.lock().await = Some(Running {
        context: c.clone(),
        cancel: c.cancel.clone(),
        draining: c.draining.clone(),
        task: server,
        port,
    });
    let url = format!("http://127.0.0.1:{port}/v1/responses");
    let first = {
        let url = url.clone();
        tokio::spawn(async move {
            reqwest::Client::new()
                .post(url)
                .bearer_auth("test-key")
                .json(&json!({"input":"first"}))
                .send()
                .await
                .unwrap()
        })
    };
    tokio::time::timeout(Duration::from_secs(2), async {
        while calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    g.drain().await;
    assert!(g.status().await.draining);
    assert_eq!(g.status().await.active_requests, 1);
    assert!(!c.cancel.is_cancelled());
    // Listener may already be closed, or the middleware rejects an existing connection.
    if let Ok(r) = reqwest::Client::new()
        .post(url)
        .bearer_auth("test-key")
        .json(&json!({"input":"new"}))
        .send()
        .await
    {
        assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    release.cancel();
    assert_eq!(first.await.unwrap().status(), StatusCode::OK);
    tokio::time::timeout(Duration::from_secs(2), async {
        while g.status().await.running {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(a.store.logs().unwrap()[0].status, "success");
    assert!(g.start().await.unwrap().running);
    g.stop().await;
    up.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stress_sustained_requests_and_repeated_cancellation_do_not_leak_or_replay() {
    let (_d, a) = fixture();
    add(&a, "a");
    let calls = Arc::new(AtomicUsize::new(0));
    let n = calls.clone();
    let (upstream, up) = serve(Router::new().route("/responses", post(move |Json(v): Json<Value>| {
        let n = n.clone(); async move {
            n.fetch_add(1, Ordering::SeqCst);
            let stream = async_stream::stream! {
                yield Ok::<Bytes, std::io::Error>(Bytes::from_static(b"data: {\"type\":\"response.created\"}\n\n"));
                if v["input"][0]["content"][0]["text"] == "cancel" { std::future::pending::<()>().await; }
                yield Ok(Bytes::from_static(b"data: {\"type\":\"response.completed\",\"response\":{\"id\":\"sustained\",\"output\":[]}}\n\n"));
            };
            ([("content-type", "text/event-stream")], Body::from_stream(stream))
        }
    }))).await;
    let c = ctx(a.clone(), upstream);
    let (base, server) = serve(router(c.clone())).await;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap();
    for i in 0..600 {
        let cancel = i % 10 == 0;
        let r = client.post(format!("{base}/v1/responses")).bearer_auth("test-key").json(&json!({"input":if cancel { "cancel" } else { "hello" },"stream":cancel || i % 2 == 0})).send().await.unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        if cancel {
            let mut stream = r.bytes_stream();
            stream.next().await.unwrap().unwrap();
            drop(stream);
        } else {
            r.bytes().await.unwrap();
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            while c.scheduler.is_active("a") || c.admission.slots.available_permits() != 8 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 600);
    let logs = a.store.logs().unwrap(); // bounded UI view; full count is checked in SQLite below.
    assert!(logs
        .iter()
        .all(|l| matches!(l.status.as_str(), "success" | "cancelled")));
    assert_eq!(a.store.usage().unwrap()[0].requests, 600);
    assert!(c.scheduler.available(&a.store.account("a").unwrap()));
    server.abort();
    up.abort();
}

#[tokio::test]
async fn storage_failure_is_visible_without_replaying_or_changing_a_successful_response() {
    let (_d, accounts) = fixture();
    add(&accounts, "a");
    let calls = Arc::new(AtomicUsize::new(0));
    let (n, storage) = (calls.clone(), accounts.store.clone());
    let (upstream, up) = serve(Router::new().route("/responses", post(move || {
        let (n, storage) = (n.clone(), storage.clone());
        async move {
            n.fetch_add(1, Ordering::SeqCst); storage.reject_writes_for_test();
            ([("content-type", "text/event-stream")], "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[]}}\n\n")
        }
    }))).await;
    let gateway = Gateway::new(accounts.clone());
    let (base, server) = serve(router(ctx(accounts.clone(), upstream))).await;
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("{base}/v1/responses"))
        .bearer_auth("test-key")
        .json(&json!({"input":"public fixture"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.json::<Value>().await.unwrap()["status"],
        "completed"
    );
    assert!(gateway.status().await.storage_warning.is_some());
    assert!(accounts.store.logs().unwrap().is_empty());
    assert!(!serde_json::to_string(&gateway.status().await)
        .unwrap()
        .contains("sqlite"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    server.abort();
    up.abort();
}
