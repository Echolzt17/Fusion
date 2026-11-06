use super::tests::{ctx, serve};
use super::*;
use crate::tests::{add, fixture};
use std::sync::atomic::{AtomicUsize, Ordering};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires explicitly selected installed engine; uses isolated home and fake upstream only"]
async fn native_engine_through_real_gateway() {
    let engine = std::env::var("FUSION_NATIVE_ENGINE")
        .expect("set FUSION_NATIVE_ENGINE to the verified desktop engine");
    let (_d, a) = fixture();
    add(&a, "a");
    let models = Arc::new(AtomicUsize::new(0));
    let m = models.clone();
    let pictures = Arc::new(AtomicUsize::new(0));
    let p = pictures.clone();
    let mock=Router::new().route("/models",get(||async{Json(json!({"models":[]}))}))
      .route("/responses",post(move|h:HeaderMap,Json(v):Json<Value>|{let m=m.clone();async move{
        assert_eq!(h["authorization"],"Bearer fake-access-token");
        assert!(!h.contains_key("x-openai-actor-authorization"));
        let n=m.fetch_add(1,Ordering::SeqCst);
        let item=if n<2 {json!({"type":"function_call","id":format!("fc-{n}"),"call_id":format!("image-{n}"),"namespace":"image_gen","name":"imagegen","arguments":if n==0 {r#"{"prompt":"fixture"}"#} else {r#"{"prompt":"fixture edit","num_last_images_to_include":1}"#}})}
          else {json!({"type":"message","id":"fixture-message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"done","annotations":[]}]})};
        assert!(v["tools"].to_string().contains("imagegen") || v["input"].to_string().contains("imagegen"));
        let events=[json!({"type":"response.created","response":{"id":format!("resp-{n}")}}),
          json!({"type":"response.output_item.added","output_index":0,"item":item}),
          json!({"type":"response.output_item.done","output_index":0,"item":item}),
          json!({"type":"response.completed","response":{"id":format!("resp-{n}"),"object":"response","status":"completed","output":[item],"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}})];
        ([("content-type","text/event-stream")],events.iter().map(|e|format!("event: {}\ndata: {e}\n\n",e["type"].as_str().unwrap())).collect::<String>())
      }}));
    let image_handler = move |h: HeaderMap, Json(v): Json<Value>| {
        let p = p.clone();
        async move {
            assert_eq!(h["chatgpt-account-id"], "chatgpt-a");
            assert!(!h.contains_key("x-openai-actor-authorization"));
            assert!(h.contains_key("x-codex-image-turn-id"));
            let n = p.fetch_add(1, Ordering::SeqCst);
            if n == 1 {
                assert_eq!(v["images"].as_array().unwrap().len(), 1);
            }
            Json(
                json!({"created":1,"background":"opaque","data":[{"b64_json":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==","generation_id":"fixture-image"}]}),
            )
        }
    };
    let (upstream, up) = serve(
        mock.route("/images/generations", post(image_handler.clone()))
            .route("/images/edits", post(image_handler)),
    )
    .await;
    let mut c = ctx(a, upstream);
    c.key = "fusion-public-fixture-key".into();
    let (base, gw) = serve(router(c.clone())).await;
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/verify-native-images.mjs");
    let output = tokio::task::spawn_blocking(move || {
        std::process::Command::new("node")
            .arg(script)
            .arg(engine)
            .arg("--gateway")
            .arg(format!("{base}/v1"))
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(models.load(Ordering::SeqCst), 3);
    assert_eq!(pictures.load(Ordering::SeqCst), 2);
    assert!(!c.scheduler.is_active("a"));
    assert_eq!(
        c.accounts
            .store
            .logs()
            .unwrap()
            .iter()
            .filter(|l| l.operation.starts_with("image_") && l.status == "success")
            .count(),
        2
    );
    gw.abort();
    up.abort();
}

fn metadata(thread: &str, turn: &str) -> String {
    json!({"thread_id":thread,"turn_id":turn,"session_id":"true-session"}).to_string()
}
fn image() -> Value {
    json!({"created":1,"background":"transparent","output_format":"png","data":[{"b64_json":"Zml4dHVyZQ==","generation_id":"gen-fixture"}],"usage":{"input_tokens":9,"output_tokens":11,"total_tokens":20}})
}
#[tokio::test]
async fn image_payloads_ignore_mime_variants_and_invalid_payloads_never_replay() {
    for route in ["generations", "edits"] {
        for mime in [None, Some("Application/JSON; charset=UTF-8"), Some("application/json; charset=utf-8")] {
            for (index, payload) in [image().to_string(), "<html>failure</html>".into(), "{broken".into(), "{\"data\":[]}".into()].into_iter().enumerate() {
                let valid = index == 0;
                let (_d, a) = fixture(); add(&a, "a");
                a.store.bind(&binding_key("turn", "fixture-turn"), "a").unwrap();
                let calls = Arc::new(AtomicUsize::new(0)); let n = calls.clone();
                let (upstream, up) = serve(Router::new().route(&format!("/images/{route}"), post(move || {
                    let n = n.clone(); let payload = payload.clone();
                    async move {
                        n.fetch_add(1, Ordering::SeqCst);
                        let mut response = Response::new(Body::from(payload));
                        if let Some(mime) = mime { response.headers_mut().insert("content-type", mime.parse().unwrap()); }
                        response
                    }
                }))).await;
                let (base, gw) = serve(router(ctx(a.clone(), upstream))).await;
                let response = reqwest::Client::new().post(format!("{base}/v1/images/{route}"))
                    .bearer_auth("test-key").header("x-codex-image-turn-id", "fixture-turn")
                    .header("x-codex-turn-metadata", metadata("fixture-thread", "fixture-turn"))
                    .json(&json!({"model":"fixture", "prompt":"fixture", "images":[{"image_url":"data:image/png;base64,Zml4dHVyZQ=="}]}))
                    .send().await.unwrap();
                assert_eq!(response.status(), if valid { StatusCode::OK } else { StatusCode::BAD_GATEWAY });
                if valid { assert_eq!(response.json::<Value>().await.unwrap(), image()); }
                assert_eq!(calls.load(Ordering::SeqCst), 1);
                let logs = a.store.logs().unwrap();
                assert_eq!(logs.len(), 1);
                let diagnostic = logs[0].diagnostic.as_ref().unwrap();
                assert_eq!(diagnostic.response_type, Some(if mime.is_none() { ResponseType::Missing } else { ResponseType::Json }));
                assert_eq!(diagnostic.detected_response_type, if index == 0 || index == 3 { Some(ResponseType::Json) } else { None });
                gw.abort(); up.abort();
            }
        }
    }
}
#[tokio::test]
async fn images_preserve_native_protocol_bind_account_usage_and_never_store_content() {
    let (dir, a) = fixture();
    add(&a, "a");
    add(&a, "b");
    let calls = Arc::new(AtomicUsize::new(0));
    let n = calls.clone();
    let handler = move |h: HeaderMap, Json(v): Json<Value>| {
        let n = n.clone();
        async move {
            n.fetch_add(1, Ordering::SeqCst);
            assert_eq!(h["authorization"], "Bearer fake-access-token");
            assert_eq!(h["chatgpt-account-id"], "chatgpt-a");
            assert!(!h.contains_key("x-openai-actor-authorization"));
            assert_eq!(v["model"], "client-selected-image-model");
            assert_eq!(v["future_field"]["enabled"], true);
            assert!(v.get("store").is_none() && v.get("instructions").is_none());
            (
                [("x-codex-imagegen-request-id", "nested-request")],
                Json(image()),
            )
        }
    };
    let (upstream, up) = serve(
        Router::new()
            .route("/images/generations", post(handler.clone()))
            .route("/images/edits", post(handler)),
    )
    .await;
    let c = ctx(a.clone(), upstream);
    // Model turn identity arrives in Responses Lite body; header session_id is cache affinity.
    let v = json!({"client_metadata":{"x-codex-turn-metadata":metadata("thread-a","turn-a")}});
    let h = HeaderMap::new();
    let mut audit = Audit::new(c.clone(), "responses");
    let (lease, _) = prepare(
        &mut audit,
        &{
            let mut h = h;
            h.insert("authorization", "Bearer test-key".parse().unwrap());
            h
        },
        &v,
    )
    .await
    .unwrap_or_else(|_| panic!("prepare"));
    drop(lease);
    drop(audit);
    c.scheduler.select_account(Some("b".into())).unwrap();
    let (base, gw) = serve(router(c.clone())).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let mut payload = json!({"prompt":"private-image-prompt","model":"client-selected-image-model","background":"transparent","future_field":{"enabled":true}});
    for references in 0..=5 {
        let route = if references == 0 {
            "generations"
        } else {
            "edits"
        };
        if references > 0 {
            payload["images"] = json!(vec![
                json!({"image_url":"data:image/png;base64,cHJpdmF0ZS1pbWFnZS1ieXRlcw=="});
                references
            ]);
        }
        let r = client
            .post(format!("{base}/v1/images/{route}"))
            .bearer_auth("test-key")
            .header("x-openai-actor-authorization", "fusion-local")
            .header("x-codex-image-turn-id", "turn-a")
            .header("x-codex-turn-metadata", metadata("thread-a", "turn-a"))
            .json(&payload)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(r.headers()["x-codex-imagegen-request-id"], "nested-request");
        assert_eq!(r.json::<Value>().await.unwrap(), image());
    }
    assert_eq!(calls.load(Ordering::SeqCst), 6);
    assert_eq!(
        a.store
            .binding(&binding_key("image_generation", "gen-fixture"))
            .unwrap()
            .as_deref(),
        Some("a")
    );
    let reopened = crate::storage::Store::open(&dir.path().join("pool.sqlite")).unwrap();
    assert_eq!(
        reopened
            .binding(&binding_key("thread", "thread-a"))
            .unwrap()
            .as_deref(),
        Some("a")
    );
    let usage = a.store.usage().unwrap();
    let usage = usage.iter().find(|u| u.account == "a").unwrap();
    assert_eq!(usage.tokens.total_tokens, 120);
    let db = std::fs::read(dir.path().join("pool.sqlite-wal")).unwrap();
    for secret in [
        "private-image-prompt",
        "cHJpdmF0ZS1pbWFnZS1ieXRlcw==",
        "fake-access-token",
        "gen-fixture",
        "thread-a",
    ] {
        assert!(!String::from_utf8_lossy(&db).contains(secret));
    }
    gw.abort();
    up.abort();
}
#[tokio::test]
async fn image_references_and_conflicting_identity_fail_before_sending() {
    let (_d, a) = fixture();
    add(&a, "a");
    add(&a, "b");
    let c = ctx(a.clone(), "http://127.0.0.1:1".into());
    a.store.bind(&binding_key("thread", "t"), "a").unwrap();
    a.store.bind(&binding_key("turn", "r"), "b").unwrap();
    let mut h = HeaderMap::new();
    h.insert("x-codex-turn-metadata", metadata("t", "r").parse().unwrap());
    assert!(pinned(&c, &h, &json!({})).is_err());
    assert!(pinned(
        &c,
        &HeaderMap::new(),
        &json!({"images":[{"file_id":"unknown"}]})
    )
    .is_err());
    assert!(pinned(
        &c,
        &HeaderMap::new(),
        &json!({"images":[{"generation_id":"unknown"}]})
    )
    .is_err());
    assert!(pinned(
        &c,
        &HeaderMap::new(),
        &json!({"input":[{"type":"image_generation_call","id":"unknown"}]})
    )
    .is_err());
    h.clear();
    h.insert("session_id", "same-cache-key".parse().unwrap());
    let v = json!({"client_metadata":{"thread_id":"t","turn_id":"new-turn"}});
    assert_eq!(pinned(&c, &h, &v).unwrap().as_deref(), Some("a"));
    assert!(a
        .store
        .binding(&binding_key("session", "same-cache-key"))
        .unwrap()
        .is_none());
    // A multi-key conflict rolls back earlier inserts.
    assert!(a
        .store
        .bind_many(
            &[binding_key("turn", "fresh"), binding_key("turn", "r")],
            "a"
        )
        .is_err());
    assert!(a
        .store
        .binding(&binding_key("turn", "fresh"))
        .unwrap()
        .is_none());
}
#[tokio::test]
async fn image_http_failures_preserve_rate_headers_and_never_retry_or_leak_error_body() {
    for status in [400u16, 401, 403, 429, 500] {
        let (_d, a) = fixture();
        add(&a, "a");
        let count = Arc::new(AtomicUsize::new(0));
        let n = count.clone();
        let (upstream, up) = serve(Router::new().route(
            "/images/generations",
            post(move || {
                let n = n.clone();
                async move {
                    n.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::from_u16(status).unwrap(),
                        [
                            ("retry-after", "12"),
                            ("x-codex-active-limit", "codex"),
                            ("x-codex-primary-used-percent", "100"),
                        ],
                        "sensitive-upstream-body",
                    )
                }
            }),
        ))
        .await;
        let (base, gw) = serve(router(ctx(a.clone(), upstream))).await;
        let r = reqwest::Client::new()
            .post(format!("{base}/v1/images/generations"))
            .bearer_auth("test-key")
            .json(&json!({"model":"test","prompt":"private"}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status().as_u16(), status);
        assert_eq!(r.headers()["retry-after"], "12");
        assert!(!r.text().await.unwrap().contains("sensitive-upstream-body"));
        assert_eq!(count.load(Ordering::SeqCst), 1);
        if status == 401 {
            assert_eq!(a.store.account("a").unwrap().state, "reauth");
        }
        if status == 429 {
            assert!(a.store.account("a").unwrap().cooldown_until > crate::now());
        }
        assert!(!serde_json::to_string(&a.store.logs().unwrap())
            .unwrap()
            .contains("sensitive-upstream-body"));
        gw.abort();
        up.abort();
    }
}
#[test]
fn sse_large_frames_are_chunk_independent_bounded_and_not_retained_for_streaming() {
    let (_d, a) = fixture();
    add(&a, "a");
    let c = ctx(a, String::new());
    let event = json!({"type":"response.completed","response":{"id":"large","output":[{"type":"image_generation_call","id":"img-call","result":"a".repeat(5*1024*1024)}]}});
    let frame = format!("data: {event}\n\n");
    for chunk in [70_001, frame.len()] {
        let mut obs = Observer {
            streaming: true,
            ..Observer::default()
        };
        for b in frame.as_bytes().chunks(chunk) {
            obs.push(b, &c, "a").unwrap();
        }
        assert!(obs.terminal);
        assert!(obs.completed.is_none());
        assert!(obs.buffer.is_empty());
    }
    let mut obs = Observer {
        buffer: vec![b'a'; MAX_EVENT_BYTES],
        ..Observer::default()
    };
    assert!(obs.push(b"b\n\n", &c, "a").is_err());
}
#[tokio::test]
async fn authentication_precedes_large_or_invalid_request_body() {
    let (_d, a) = fixture();
    add(&a, "a");
    let (base, gw) = serve(router(ctx(a, "http://127.0.0.1:1".into()))).await;
    let r = reqwest::Client::new()
        .post(format!("{base}/v1/images/edits"))
        .header("content-type", "application/json")
        .body("not-json")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    gw.abort();
}

#[tokio::test]
async fn native_image_disconnect_releases_pending_upstream_and_lease() {
    use tokio::io::AsyncWriteExt;
    let (_d, a) = fixture();
    add(&a, "a");
    let started = Arc::new(tokio::sync::Notify::new());
    let notification = started.clone();
    let (upstream, up) = serve(Router::new().route(
        "/images/generations",
        post(move || {
            let n = notification.clone();
            async move {
                n.notify_one();
                std::future::pending::<Json<Value>>().await
            }
        }),
    ))
    .await;
    let c = ctx(a, upstream);
    let (base, gw) = serve(router(c.clone())).await;
    let mut socket = tokio::net::TcpStream::connect(base.trim_start_matches("http://"))
        .await
        .unwrap();
    let body = r#"{"model":"test","prompt":"private"}"#;
    let req=format!("POST /v1/images/generations HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer test-key\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",body.len(),body);
    socket.write_all(req.as_bytes()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), started.notified())
        .await
        .unwrap();
    assert!(c.scheduler.is_active("a"));
    drop(socket);
    tokio::time::timeout(Duration::from_secs(2), async {
        while c.scheduler.is_active("a") {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(c.accounts.store.logs().unwrap()[0].status, "cancelled");
    gw.abort();
    up.abort();
}

#[tokio::test]
async fn image_header_body_timeouts_and_oversize_are_finite_without_replay() {
    let (_d, a) = fixture();
    add(&a, "a");
    let (upstream,up)=serve(Router::new()
        .route("/headers",get(||async{std::future::pending::<String>().await}))
        .route("/body",get(||async{
            let s=async_stream::stream! {yield Ok::<_,std::io::Error>(Bytes::from_static(b"{\"data\":"));std::future::pending::<()>().await;};
            Response::new(Body::from_stream(s))
        }))
        .route("/size",get(||async{"0123456789"}))).await;
    let c = ctx(a, upstream.clone());
    let client = reqwest::Client::new();
    let error = send_timeout(
        &c,
        client.get(format!("{upstream}/headers")),
        Duration::from_millis(20),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "upstream_header_timeout");
    assert_eq!(error.stage, FailureStage::ResponseHeaders);
    assert_eq!(error.kind, Some(NetworkErrorKind::Timeout));
    let r = client.get(format!("{upstream}/body")).send().await.unwrap();
    let error = read_budget(&c, r, 100, Duration::from_millis(20)).await.unwrap_err();
    assert_eq!(error.kind, Some(NetworkErrorKind::Timeout));
    assert_eq!(error.stage, FailureStage::ResponseRead);
    assert!(error.message.contains("超时"));
    let r = client.get(format!("{upstream}/size")).send().await.unwrap();
    assert!(read_budget(&c, r, 5, Duration::from_secs(1))
        .await
        .unwrap_err()
        .message.contains("大小限制"));
    up.abort();
}
