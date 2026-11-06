use super::*;
use super::tests::{ctx, serve};
use crate::tests::{add, fixture};
use std::sync::{atomic::{AtomicUsize, Ordering}, Mutex as StdMutex};

#[tokio::test]
async fn dedicated_local_key_discards_desktop_auth_and_rejects_fallback() {
    let (_d, a) = fixture(); add(&a, "a");
    let calls = Arc::new(AtomicUsize::new(0)); let n = calls.clone();
    let (upstream, up) = serve(Router::new().route("/models", get(move |h: HeaderMap| {
        let n = n.clone(); async move {
            n.fetch_add(1, Ordering::SeqCst);
            assert_eq!(h["authorization"], "Bearer fake-access-token");
            assert!(!h.contains_key("x-fusion-client-key"));
            Json(json!({"models":[]}))
        }
    }))).await;
    let (base, gw) = serve(router(ctx(a.clone(), upstream))).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    for key in ["wrong-key", ""] {
        let r = client.get(format!("{base}/v1/models")).bearer_auth("test-key")
            .header("x-fusion-client-key", key).send().await.unwrap();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    }
    let r = client.get(format!("{base}/v1/models")).header("x-fusion-client-key", "test-key")
        .header("x-fusion-client-key", "test-key").send().await.unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let r = client.get(format!("{base}/v1/models")).header("x-fusion-client-key", "test-key")
        .bearer_auth("public-desktop-credential-must-be-discarded").send().await.unwrap();
    assert!(r.status().is_success());
    // Existing pool chats and internal checks still use the original local bearer.
    assert!(client.get(format!("{base}/v1/models")).bearer_auth("test-key").send().await.unwrap().status().is_success());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(!serde_json::to_string(&a.store.logs().unwrap()).unwrap().contains("public-desktop-credential"));
    gw.abort(); up.abort();
}

#[tokio::test]
async fn internal_catalog_check_requires_version_and_preserves_upstream_rejection() {
    let (_d, a) = fixture(); add(&a, "a");
    let reserved = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut settings = a.store.settings().unwrap();
    settings.port = reserved.local_addr().unwrap().port();
    a.store.save_settings(&settings).unwrap();
    let queries = Arc::new(StdMutex::new(Vec::new())); let q = queries.clone();
    let (upstream, up) = serve(Router::new().route("/models", get(move |RawQuery(query): RawQuery| {
        let q = q.clone(); async move { q.lock().unwrap().push(query); StatusCode::BAD_REQUEST }
    }))).await;
    let gateway = Gateway::new(a);
    *gateway.test_upstream.lock().unwrap() = Some(upstream);
    // Invalid versions are rejected even before a listener or authorization is needed.
    for version in ["", "codex-cli 0.166.0", "0.166.0&extra=bad", "bad/version"] {
        assert!(gateway.check_models(version, Arc::new(|_| {})).await.unwrap_err().contains("版本参数无效"));
    }
    drop(reserved);
    gateway.start().await.unwrap();
    let error = gateway.check_models("0.166.0-alpha.5.1+build", Arc::new(|_| {})).await.unwrap_err();
    assert!(error.contains("HTTP 400") && error.contains("请求编号") && error.contains("模型目录"));
    assert!(!error.contains("上下文") && !error.contains("输入设置"));
    assert_eq!(*queries.lock().unwrap(), vec![Some("client_version=0.166.0-alpha.5.1%2Bbuild".into())]);
    assert!(!gateway.scheduler.is_active("a"));
    gateway.stop().await; up.abort();
}

#[tokio::test]
async fn catalog_forwards_only_valid_version_and_omits_missing_version() {
    let (_d, a) = fixture(); add(&a, "a");
    let queries = Arc::new(StdMutex::new(Vec::new())); let q = queries.clone();
    let (upstream, up) = serve(Router::new().route("/models", get(move |RawQuery(query): RawQuery| {
        let q = q.clone(); async move { q.lock().unwrap().push(query); Json(json!({"models":[]})) }
    }))).await;
    let c = ctx(a.clone(), upstream);
    for (query, success) in [(Some("client_version=0.166.0-alpha.5.1&secret=ignored"), true), (None, true), (Some("client_version=bad%2Fversion"), false), (Some("client_version=a&client_version=b"), false)] {
        let mut h = HeaderMap::new(); h.insert("authorization", "Bearer test-key".parse().unwrap());
        let response = catalog(c.clone(), h, query.map(str::to_owned), None).await;
        assert_eq!(response.status().is_success(), success);
    }
    assert_eq!(*queries.lock().unwrap(), vec![Some("client_version=0.166.0-alpha.5.1".into()), None]);
    for log in a.store.logs().unwrap().iter().filter(|log| log.status == "success") {
        assert_eq!(log.diagnostic.as_ref().unwrap().detected_response_type, Some(ResponseType::Json));
    }
    up.abort();
}

#[tokio::test]
async fn cancelling_catalog_releases_lease_without_replay() {
    let (_d, a) = fixture(); add(&a, "a");
    let calls = Arc::new(AtomicUsize::new(0)); let n = calls.clone();
    let (upstream, up) = serve(Router::new().route("/models", get(move || {
        let n = n.clone(); async move { n.fetch_add(1, Ordering::SeqCst); std::future::pending::<Json<Value>>().await }
    }))).await;
    let c = ctx(a, upstream); let request = c.clone();
    let task = tokio::spawn(async move {
        let mut h = HeaderMap::new(); h.insert("authorization", "Bearer test-key".parse().unwrap());
        catalog(request, h, None, None).await
    });
    while calls.load(Ordering::SeqCst) == 0 { tokio::time::sleep(Duration::from_millis(10)).await; }
    task.abort(); let _ = task.await;
    assert!(!c.scheduler.is_active("a")); assert_eq!(calls.load(Ordering::SeqCst), 1);
    up.abort();
}
#[tokio::test]
async fn catalog_total_budget_bounds_trickling_body_and_releases_account() {
    let (_d, a) = fixture(); add(&a, "a");
    let calls = Arc::new(AtomicUsize::new(0)); let n = calls.clone();
    let (upstream, up) = serve(Router::new().route("/models", get(move || {
        let n = n.clone(); async move {
            n.fetch_add(1, Ordering::SeqCst);
            let stream = async_stream::stream! {
                loop {
                    yield Ok::<_, std::io::Error>(Bytes::from_static(b" "));
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            };
            Body::from_stream(stream)
        }
    }))).await;
    let c = ctx(a, upstream); let mut h = HeaderMap::new(); h.insert("authorization", "Bearer test-key".parse().unwrap());
    let response = catalog_with_budget(c.clone(), h, None, None, Duration::from_millis(250)).await;
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    assert!(!c.scheduler.is_active("a")); assert_eq!(calls.load(Ordering::SeqCst), 1);
    up.abort();
}
