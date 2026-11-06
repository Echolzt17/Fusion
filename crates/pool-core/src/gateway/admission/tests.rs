use super::*;
use crate::{
    gateway::tests::{ctx, serve},
    tests::{add, fixture},
};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn pre_handler_rejections_have_one_matching_reportable_audit_without_upstream() {
    let (_dir, accounts)=fixture();
    let calls=Arc::new(AtomicUsize::new(0));let count=calls.clone();
    let (upstream,up)=serve(Router::new().route("/responses",post(move || {let count=count.clone();async move {count.fetch_add(1,Ordering::SeqCst);StatusCode::OK}}))).await;
    let mut context=ctx(accounts.clone(),upstream);
    context.admission=Arc::new(Admission::new(2,1024,Duration::from_millis(100),Duration::from_secs(1)));
    let (base,gateway)=serve(router(context.clone())).await;
    let client=reqwest::Client::builder().no_proxy().build().unwrap();
    let url=format!("{base}/v1/responses");
    assert_eq!(client.post(&url).body("private-body").send().await.unwrap().status(),401);
    assert!(accounts.store.logs().unwrap().is_empty());
    for (index,code) in ["request_too_large","invalid_request","ingress_busy"].into_iter().enumerate() {
        let waiting=if code=="ingress_busy" {Some(context.admission.waiting.clone().acquire_many_owned(32).await.unwrap())} else {None};
        let response=client.post(&url).bearer_auth("test-key").header("content-type","application/json")
            .body(if code=="request_too_large" {"x".repeat(1025)} else {"private-invalid-json".into()}).send().await.unwrap();
        let status=response.status().as_u16();
        let header=response.headers()["x-request-id"].to_str().unwrap().to_owned();
        let body:Value=response.json().await.unwrap();
        assert_eq!(body["request_id"],header);assert_eq!(body["error"]["code"],code);
        drop(waiting);
        let logs=accounts.store.logs().unwrap();assert_eq!(logs.len(),index+1);
        let log=logs.iter().find(|l|l.id==header).unwrap();assert_eq!(log.status,code);
        let d=log.diagnostic.as_ref().unwrap();let x=d.execution.as_ref().unwrap();
        assert_eq!(x.local_http_status,Some(status));assert!(!x.upstream_attempted);assert_eq!(d.upstream_status,None);
        assert!(log.account.is_none() && log.usage.is_none());
        assert!(crate::error_reports::is_failure(log));
        assert!(!serde_json::to_string(&logs).unwrap().contains("private-"));
    }
    let (logs,limited)=accounts.store.error_report_records(crate::now()).unwrap();
    let environment=fusion_report_protocol::Environment {app_version:"1.0.0".into(),os:"Windows 10".into(),os_build:None,architecture:"x64".into(),webview2_version:None,desktop_version:None,engine_version:None,uptime_seconds:1};
    let report=crate::error_reports::prepare(&logs,&uuid::Uuid::new_v4().to_string(),environment,crate::now(),limited).unwrap();
    assert_eq!(report.schema_version,3);assert_eq!(report.errors.len(),3);assert!(report.encode().is_ok());
    assert_eq!(accounts.store.request_summaries(None).unwrap().0.failed,3);
    assert_eq!(calls.load(Ordering::SeqCst),0);assert!(context.requests.snapshot().is_empty());
    gateway.abort();up.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "Set FUSION_NATIVE_ENGINE to the installed desktop engine; public fake login and temporary paths only"]
async fn actual_native_engine_four_conversations_through_gateway() {
    let engine = std::env::var_os("FUSION_NATIVE_ENGINE").expect("FUSION_NATIVE_ENGINE");
    let (_dir, accounts) = fixture();
    add(&accounts, "a");
    accounts.store.set("settings", r#"{"port":18765,"concurrency":2,"fixedAccount":null}"#).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(tokio::sync::Barrier::new(4));
    let (count, together) = (calls.clone(), barrier.clone());
    let upstream = Router::new()
        .route("/models", get(|| async { Json(json!({"models":[],"data":[]})) }))
        .route("/responses", post(move |h: HeaderMap, Json(_body): Json<Value>| {
            let (count, together) = (count.clone(), together.clone());
            async move {
                assert_eq!(h["chatgpt-account-id"], "chatgpt-a");
                let n = count.fetch_add(1, Ordering::SeqCst);
                // No response may complete before all four actual engines have
                // reached the same account concurrently through the real gateway.
                together.wait().await;
                let item = json!({"type":"message","id":format!("fixture-msg-{n}"),"role":"assistant","status":"completed",
                    "content":[{"type":"output_text","text":"fixture-parallel-ok","annotations":[]}]});
                let events = [
                    json!({"type":"response.created","response":{"id":format!("fixture-resp-{n}"),"object":"response","status":"in_progress"}}),
                    json!({"type":"response.output_item.added","output_index":0,"item":item}),
                    json!({"type":"response.output_item.done","output_index":0,"item":item}),
                    json!({"type":"response.completed","response":{"id":format!("fixture-resp-{n}"),"status":"completed","output":[item],"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}),
                ];
                ([("content-type", "text/event-stream")], events.iter().map(|v| format!("event: {}\ndata: {v}\n\n", v["type"].as_str().unwrap())).collect::<String>())
            }
        }));
    let (upstream, up) = serve(upstream).await;
    let mut context = ctx(accounts.clone(), upstream);
    context.key = "fusion-public-fixture-key".into();
    let (base, gateway) = serve(router(context.clone())).await;
    let address = base.trim_start_matches("http://").parse().unwrap();
    let mut roots = Vec::new();
    let mut children = Vec::new();
    for _ in 0..4 {
        let root = tempfile::Builder::new().prefix("fusion-parallel-native-").tempdir().unwrap();
        crate::compatibility::probe::configure(root.path(), address).unwrap();
        let mut command = tokio::process::Command::from(crate::compatibility::probe::isolated_command(std::path::Path::new(&engine), root.path()));
        command.args(["exec", "--skip-git-repo-check", "--ephemeral", "--ignore-rules", "--color", "never", "-C"])
            .arg(root.path()).arg("Fixture").stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).kill_on_drop(true);
        children.push(command.spawn().unwrap());
        roots.push(root);
    }
    tokio::time::timeout(Duration::from_secs(45), async {
        for child in &mut children { assert!(child.wait().await.unwrap().success(), "isolated native conversation must complete"); }
    }).await.expect("four native conversations must finish without settings changes or manual retries");
    assert_eq!(calls.load(Ordering::SeqCst), 4, "no native or gateway replay");
    let logs = accounts.store.logs().unwrap();
    let requests: Vec<_> = logs.iter().filter(|r| r.operation == "responses").collect();
    assert_eq!(requests.len(), 4);
    assert!(requests.iter().all(|r| r.status == "success" && r.account.as_deref() == Some("a")));
    assert!(!context.scheduler.is_active("a"));
    gateway.abort(); up.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn four_live_conversations_share_one_account_with_chunked_and_sized_uploads() {
    for chunked in [true, false] {
        let (_dir, accounts) = fixture();
        add(&accounts, "a");
        // Existing installations must work without asking the user to change settings.
        accounts.store.set("settings", r#"{"port":18765,"concurrency":2,"fixedAccount":null}"#).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let release = CancellationToken::new();
        let (count, gate) = (calls.clone(), release.clone());
        let (upstream, up) = serve(Router::new().route("/responses", post(move |h: HeaderMap, Json(_body): Json<Value>| {
            let (count, gate) = (count.clone(), gate.clone());
            async move {
                assert_eq!(h["chatgpt-account-id"], "chatgpt-a");
                count.fetch_add(1, Ordering::SeqCst);
                let stream = async_stream::stream! {
                    yield Ok::<_, std::io::Error>(Bytes::from_static(b"data: {\"type\":\"response.created\",\"response\":{\"status\":\"in_progress\"}}\n\n"));
                    gate.cancelled().await;
                    yield Ok(Bytes::from_static(b"data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[]}}\n\n"));
                };
                ([("content-type", "text/event-stream")], Body::from_stream(stream))
            }
        }))).await;
        let mut context = ctx(accounts.clone(), upstream);
        if !chunked {
            // Four uploads exceed the raw-byte budget together, but their uploads
            // have ended before the long-lived responses complete.
            context.admission = Arc::new(Admission::new(8, 128 * 1024, Duration::from_secs(2), Duration::from_secs(3)));
        }
        let (base, gateway) = serve(router(context.clone())).await;
        let mut pending = tokio::task::JoinSet::new();
        for n in 0..4 {
            let url = format!("{base}/v1/responses");
            pending.spawn(async move {
                let body = serde_json::to_vec(&json!({"input":"x".repeat(64 * 1024),"stream":true})).unwrap();
                let body = if chunked {
                    reqwest::Body::wrap_stream(futures_util::stream::iter(
                        body.chunks(8192).map(|b| Ok::<_, std::io::Error>(Bytes::copy_from_slice(b))).collect::<Vec<_>>()
                    ))
                } else { reqwest::Body::from(body) };
                reqwest::Client::builder().no_proxy().build().unwrap()
                    .post(url).bearer_auth("test-key").header("session_id", format!("fixture-four-{n}"))
                    .header("content-type", "application/json").body(body).send().await.unwrap()
            });
        }
        let mut responses = Vec::new();
        for _ in 0..4 {
            let response = tokio::time::timeout(Duration::from_secs(4), pending.join_next()).await
                .expect("all four conversations must start while the other three are still working")
                .unwrap().unwrap();
            assert_eq!(response.status(), StatusCode::OK, "chunked={chunked}");
            responses.push(response);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        assert_eq!(context.requests.snapshot().iter().filter(|r| r.account.as_deref() == Some("a")).count(), 4);
        assert_eq!(context.admission.bytes.available_permits(), context.admission.budget);
        release.cancel();
        for response in responses { assert!(response.text().await.unwrap().contains("response.completed")); }
        for n in 0..4 {
            assert_eq!(accounts.store.binding(&binding_key("session", &format!("fixture-four-{n}"))).unwrap().as_deref(), Some("a"));
        }
        gateway.abort(); up.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stress_ingress_bounds_bytes_and_slots_and_recovers_after_overload() {
    let (_d, accounts) = fixture();
    add(&accounts, "a");
    let calls = Arc::new(AtomicUsize::new(0));
    let release = CancellationToken::new();
    let (n, gate) = (calls.clone(), release.clone());
    let (upstream, up) = serve(Router::new().route("/responses", post(move |Json(_body): Json<Value>| {
        let (n, gate) = (n.clone(), gate.clone());
        async move {
            n.fetch_add(1, Ordering::SeqCst);
            gate.cancelled().await;
            ([("content-type", "text/event-stream")], "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"bounded\",\"status\":\"completed\",\"output\":[]}}\n\n")
        }
    }))).await;
    let mut c = ctx(accounts, upstream);
    c.admission = Arc::new(Admission::new(
        4,
        128 * 1024,
        Duration::from_secs(2),
        Duration::from_secs(3),
    ));
    let (base, server) = serve(router(c.clone())).await;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let body = format!("{{\"input\":\"{}\"}}", "x".repeat(64 * 1024 - 12));
    assert_eq!(body.len(), 64 * 1024);
    let mut tasks = tokio::task::JoinSet::new();
    // Keep two accepted bodies live while 100 additional clients contend for the byte budget.
    for _ in 0..2 {
        let (client, url, body) = (client.clone(), format!("{base}/v1/responses"), body.clone());
        tasks.spawn(async move {
            client
                .post(url)
                .bearer_auth("test-key")
                .header("content-type", "application/json")
                .body(body)
                .send()
                .await
                .unwrap()
                .status()
        });
    }
    tokio::time::timeout(Duration::from_secs(3), async {
        while calls.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let mut contending = tokio::task::JoinSet::new();
    for _ in 0..100 {
        let (client, url, body) = (client.clone(), format!("{base}/v1/responses"), body.clone());
        contending.spawn(async move {
            client
                .post(url)
                .bearer_auth("test-key")
                .header("content-type", "application/json")
                .body(body)
                .send()
                .await
        });
    }
    // Two body readers and 32 entrance waiters fit; any further load is bounded.
    tokio::time::timeout(Duration::from_secs(2), async {
        while c.requests.snapshot().len() != 36 { tokio::task::yield_now().await; }
    }).await.unwrap();
    assert_eq!(c.admission.bytes.available_permits(), 0);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(c.admission.waiting.available_permits(), 0);
    release.cancel();
    let mut accepted = 0;
    let mut rejected = 0;
    while let Some(result) = contending.join_next().await {
        // Early bounded-queue rejection may close an upload in progress.
        match result.unwrap() {
            Ok(r) if r.status() == StatusCode::OK => accepted += 1,
            Ok(r) => { assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE); rejected += 1; },
            Err(_) => rejected += 1,
        }
    }
    assert!(accepted >= 34 && rejected > 0);
    while let Some(r) = tasks.join_next().await {
        assert_eq!(r.unwrap(), StatusCode::OK);
    }
    tokio::time::timeout(Duration::from_secs(2), async {
        while c.admission.slots.available_permits() != 4 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(c.admission.bytes.available_permits(), 128 * 1024);
    let r = client
        .post(format!("{base}/v1/responses"))
        .bearer_auth("test-key")
        .json(&json!({"input":"recovered"}))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success());
    assert_eq!(calls.load(Ordering::SeqCst), accepted + 3);
    server.abort();
    up.abort();
}

#[tokio::test]
async fn body_idle_and_total_deadlines_release_reservations_before_parsing() {
    let (_d, accounts) = fixture();
    let mut c = ctx(accounts, "http://127.0.0.1:1".into());
    c.admission = Arc::new(Admission::new(
        2,
        1024,
        Duration::from_millis(100),
        Duration::from_millis(220),
    ));
    let (base, server) = serve(router(c.clone())).await;
    for trickle in [false, true] {
        let mut socket = tokio::net::TcpStream::connect(base.trim_start_matches("http://"))
            .await
            .unwrap();
        socket.write_all(b"POST /v1/responses HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer test-key\r\nContent-Type: application/json\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{").await.unwrap();
        if trickle {
            for _ in 0..4 {
                tokio::time::sleep(Duration::from_millis(40)).await;
                let _ = socket.write_all(b" ").await;
            }
        }
        let mut received = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(2), socket.read_to_end(&mut received))
            .await
            .unwrap();
        assert!(
            String::from_utf8_lossy(&received).contains("HTTP/1.1 408"),
            "{}",
            String::from_utf8_lossy(&received)
        );
        assert_eq!(c.admission.slots.available_permits(), 2);
        assert_eq!(c.admission.bytes.available_permits(), 1024);
    }
    let logs=c.accounts.store.logs().unwrap();assert_eq!(logs.len(),2);
    assert!(logs.iter().all(|l| matches!(l.status.as_str(),"upload_idle_timeout"|"upload_timeout") && !l.diagnostic.as_ref().unwrap().execution.as_ref().unwrap().upstream_attempted));
    server.abort();
}

#[tokio::test]
async fn waiting_for_entrance_or_body_budget_cancels_without_disturbing_live_work() {
    for phase in ["ingress_queue", "uploading"] {
        let (_dir, accounts) = fixture(); add(&accounts, "a");
        let release = CancellationToken::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let (gate, count) = (release.clone(), calls.clone());
        let (upstream, up) = serve(Router::new().route("/responses", post(move || {
            let (gate, count) = (gate.clone(), count.clone());
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                gate.cancelled().await;
                ([("content-type", "text/event-stream")], "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[]}}\n\n")
            }
        }))).await;
        let mut context = ctx(accounts, upstream);
        let slots = if phase == "ingress_queue" { 1 } else { 2 };
        context.admission = Arc::new(Admission::new(slots, 1024, Duration::from_secs(2), Duration::from_millis(100)));
        let (base, gateway) = serve(router(context.clone())).await;
        let url = format!("{base}/v1/responses");
        let first_url = url.clone();
        let first = tokio::spawn(async move {
            reqwest::Client::new().post(first_url).bearer_auth("test-key").header("content-type", "application/json")
                .body(format!("{{\"input\":\"{}\"}}", "x".repeat(1012))).send().await.unwrap()
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while calls.load(Ordering::SeqCst) != 1 { tokio::task::yield_now().await; }
        }).await.unwrap();
        let second = tokio::spawn(async move {
            reqwest::Client::new().post(url).bearer_auth("test-key").json(&json!({"input":"waiting"})).send().await
        });
        let id = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(r) = context.requests.snapshot().into_iter().find(|r| r.phase == phase) { break r.id; }
                tokio::task::yield_now().await;
            }
        }).await.unwrap();
        if phase == "uploading" {
            tokio::time::sleep(Duration::from_millis(150)).await;
            assert!(!second.is_finished(), "waiting for another request's memory must not count as a stalled upload");
        }
        context.requests.cancel(&id).await.unwrap();
        let _ = second.await.unwrap();
        let logs=context.accounts.store.logs().unwrap();
        let cancelled=logs.iter().find(|l| l.id==id).unwrap();
        assert_eq!(cancelled.status,"cancelled");assert!(!crate::error_reports::is_failure(cancelled));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(context.scheduler.is_active("a"));
        assert_eq!(context.requests.snapshot().len(), 1);
        assert_eq!(context.admission.bytes.available_permits(), 0);
        assert_eq!(context.admission.slots.available_permits(), slots - 1);
        assert_eq!(context.admission.waiting.available_permits(), 32);
        release.cancel();
        assert_eq!(first.await.unwrap().status(), StatusCode::OK);
        tokio::time::timeout(Duration::from_secs(2), async {
            while !context.requests.snapshot().is_empty() { tokio::task::yield_now().await; }
        }).await.unwrap();
        assert_eq!(context.admission.bytes.available_permits(), 1024);
        assert_eq!(context.admission.slots.available_permits(), slots);
        gateway.abort(); up.abort();
    }
}

#[tokio::test]
async fn unauthorized_and_oversized_bodies_are_rejected_without_upstream_or_leaks() {
    let (_d, accounts) = fixture();
    let mut c = ctx(accounts, "http://127.0.0.1:1".into());
    c.admission = Arc::new(Admission::new(
        2,
        1024,
        Duration::from_millis(100),
        Duration::from_secs(1),
    ));
    let (base, server) = serve(router(c.clone())).await;
    for (key, expected) in [
        ("bad-key", StatusCode::UNAUTHORIZED),
        ("test-key", StatusCode::PAYLOAD_TOO_LARGE),
    ] {
        let r = reqwest::Client::new()
            .post(format!("{base}/v1/responses"))
            .bearer_auth(key)
            .body(vec![b'x'; 1025])
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), expected);
        assert_eq!(c.admission.slots.available_permits(), 2);
        assert_eq!(c.admission.bytes.available_permits(), 1024);
    }
    server.abort();
}
