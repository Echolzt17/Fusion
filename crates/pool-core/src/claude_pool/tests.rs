use super::*;
fn fixture() -> (tempfile::TempDir, Arc<Pool>) {
    let dir = tempfile::tempdir().unwrap();
    let p = Pool::new(
        Arc::new(Store::open(&dir.path().join("pool.sqlite")).unwrap()),
        Arc::new(crate::vault::MemoryVault::default()),
    )
    .unwrap();
    let mut d = Directory::default();
    for id in ["a", "b"] {
        d.accounts.insert(
            id.into(),
            Account {
                id: id.into(),
                state: "ready".into(),
                ..Default::default()
            },
        );
    }
    p.save(&d).unwrap();
    (dir, p)
}
#[test]
fn session_and_opaque_context_stay_with_original_account_after_preference_change_and_removal() {
    let (_dir, p) = fixture();
    let v = json!({"metadata":{"user_id":"device_account_fixture_session_thread-a"}});
    assert_eq!(p.bind(&v, &[], true).unwrap(), "a");
    p.bind_output("a", &["opaque-reference-hash".into()])
        .unwrap();
    p.select(Some("b".into())).unwrap();
    assert_eq!(p.bind(&v, &[], true).unwrap(), "a");
    let next = json!({"metadata":{"user_id":"{\"session_id\":\"thread-b\"}"}});
    assert_eq!(p.bind(&next, &[], true).unwrap(), "b");
    assert!(p
        .bind(&next, &["opaque-reference-hash".into()], true)
        .is_err());
    p.pause("a", true).unwrap();
    assert!(p.bind(&v, &[], true).is_err());
    assert!(p
        .bind(&json!({"metadata":{"user_id":"unrecognized"}}), &[], true)
        .is_err());
    assert!(p.bind(&next, &["unknown-reference".into()], true).is_err());
}
#[test]
fn free_and_missing_quotas_are_distinct_and_unknown_is_never_zero() {
    assert_eq!(
        profile_plan(&json!({"organization":{"organization_type":"claude_pro"}})),
        "Pro"
    );
    assert_eq!(
        profile_plan(&json!({"organization":{"rate_limit_tier":"default_claude_max_5x"}})),
        "Max"
    );
    assert_eq!(
        profile_plan(&json!({"account":{"has_claude_pro":true}})),
        "Pro"
    );
    assert_eq!(
        profile_plan(&json!({"organization":{"subscription_type":"free"}})),
        "Free"
    );
    assert!(quota(&json!({})).is_err());
    let q=quota(&json!({"five_hour":{"utilization":100,"resets_at":"2026-11-12T00:00:00Z"},"seven_day":null})).unwrap();
    assert_eq!(q["rateLimit"]["primary_window"]["used_percent"], 100.0);
    assert!(q["rateLimit"]["secondary_window"].is_null());
    assert!(quota(&json!({"five_hour":{"utilization":-1}})).is_err());
}

#[tokio::test]
async fn gateway_uses_pinned_oauth_accounts_records_real_usage_and_never_replays_failures() {
    use axum::{
        http::{HeaderMap, StatusCode},
        routing::post,
        Json,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (dir, mut pool) = fixture();
    let attempts = Arc::new(AtomicUsize::new(0));
    let captured = Arc::new(Mutex::new(Vec::new()));
    let (count, seen) = (attempts.clone(), captured.clone());
    let router=axum::Router::new().route("/v1/messages",post(move |headers:HeaderMap,Json(v):Json<Value>|{
        let (count,seen)=(count.clone(),seen.clone());async move {
            count.fetch_add(1,Ordering::SeqCst);
            let auth=headers.get("authorization").unwrap().to_str().unwrap().to_owned();
            assert!(headers.get("x-api-key").is_none());
            assert!(headers["anthropic-beta"].to_str().unwrap().contains(BETA));
            seen.lock().unwrap().push(auth.clone());
            if v["model"]=="failure-fixture" {return (StatusCode::TOO_MANY_REQUESTS,Json(json!({"error":{"type":"rate_limit_error"}})));}
            (StatusCode::OK,Json(json!({"id":format!("message-{}",count.load(Ordering::SeqCst)),"type":"message","role":"assistant","model":"fixture","content":[],"stop_reason":"end_turn","usage":{"input_tokens":3,"output_tokens":2}})))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    Arc::get_mut(&mut pool).unwrap().upstream = format!("http://{address}");
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    for id in ["a", "b"] {
        pool.vault
            .set(
                &format!("claude-pool/{id}"),
                &serde_json::to_string(&Tokens {
                    access: format!("public-fixture-{id}"),
                    refresh: "public-fixture-refresh".into(),
                    expires: now() + 3600,
                })
                .unwrap(),
            )
            .unwrap();
    }
    let sources = Arc::new(crate::api_sources::Sources::new(
        pool.store.clone(),
        Arc::new(crate::vault::MemoryVault::default()),
    ));
    let gateway = crate::api_gateway::ApiGateway::start_claude_pool(sources, pool.clone())
        .await
        .unwrap();
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let send = |session: &str, model: &str| {
        http.post(format!("{}/messages",gateway.endpoint().base_url())).bearer_auth(gateway.endpoint().local_key()).json(&json!({"model":model,"max_tokens":8,"messages":[{"role":"user","content":"public fixture"}],"metadata":{"user_id":format!("device_account_fixture_session_{session}")}}))
    };
    assert!(send("original", "fixture")
        .send()
        .await
        .unwrap()
        .status()
        .is_success());
    pool.select(Some("b".into())).unwrap();
    assert!(send("original", "fixture")
        .send()
        .await
        .unwrap()
        .status()
        .is_success());
    assert!(send("new", "fixture")
        .send()
        .await
        .unwrap()
        .status()
        .is_success());
    assert_eq!(
        &*captured.lock().unwrap(),
        &[
            "Bearer public-fixture-a",
            "Bearer public-fixture-a",
            "Bearer public-fixture-b"
        ]
    );
    assert_eq!(
        send("new", "failure-fixture")
            .send()
            .await
            .unwrap()
            .status(),
        429
    );
    assert_eq!(attempts.load(Ordering::SeqCst), 4);
    // The original account is cooling: its bound conversation cannot fall back.
    assert_eq!(send("new", "fixture").send().await.unwrap().status(), 409);
    assert_eq!(attempts.load(Ordering::SeqCst), 4);
    gateway.stop().await;
    assert_eq!(
        pool.directory().unwrap().accounts["a"].tokens.total_tokens,
        10
    );
    assert_eq!(
        pool.directory().unwrap().accounts["b"].tokens.total_tokens,
        5
    );
    assert_eq!(pool.summary().unwrap().tokens.total_tokens, 15);
    pool.remove("a").await.unwrap();
    assert_eq!(pool.summary().unwrap().tokens.total_tokens, 15);
    assert!(pool
        .bind(
            &json!({"metadata":{"user_id":"device_account_fixture_session_original"}}),
            &[],
            true
        )
        .is_err());
    let persisted = std::fs::read(dir.path().join("pool.sqlite")).unwrap();
    assert!(!persisted
        .windows(b"public-fixture-refresh".len())
        .any(|w| w == b"public-fixture-refresh"));
    server.abort();
}
