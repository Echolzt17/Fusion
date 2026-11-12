use super::tests::{ctx, serve};
use super::*;
use crate::{account::Tokens, tests::{add, fixture}};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex as StdMutex;

fn expire(accounts: &Accounts, id: &str) {
    let saved = accounts.vault.get(&format!("account/{id}")).unwrap().unwrap();
    let mut tokens: Tokens = serde_json::from_str(&saved).unwrap();
    tokens.expires_at = 0;
    tokens.refresh_token = format!("public-fixture-refresh-{id}");
    accounts.vault.set(&format!("account/{id}"), &serde_json::to_string(&tokens).unwrap()).unwrap();
}

fn metadata(chat: &str, turn: &str) -> String {
    json!({"thread_id":chat,"turn_id":turn}).to_string()
}

fn completion() -> String {
    format!("data: {}\n\n", json!({"type":"response.completed","response":{"status":"completed","output":[]}}))
}

#[tokio::test]
async fn readable_chat_authorization_failure_uses_backup_before_any_business_dispatch() {
    for failure in ["expired_rejected", "missing_credentials"] {
        let (_dir, mut accounts) = fixture();
        add(&accounts, "a"); add(&accounts, "b");
        let refreshes = Arc::new(AtomicUsize::new(0)); let refreshed = refreshes.clone();
        let (token_base, token_server) = serve(Router::new().route("/token", post(move || {
            let refreshed = refreshed.clone(); async move {
                refreshed.fetch_add(1, Ordering::SeqCst);
                StatusCode::BAD_REQUEST
            }
        }))).await;
        Arc::get_mut(&mut accounts).unwrap().use_test_token_endpoint(format!("{token_base}/token"));
        if failure == "expired_rejected" { expire(&accounts, "a"); }
        else { accounts.vault.delete("account/a").unwrap(); }
        let chat = binding_key("thread", "auth-chat");
        accounts.store.bind(&chat, "a").unwrap();
        let dispatches = Arc::new(AtomicUsize::new(0)); let dispatched = dispatches.clone();
        let (upstream, up) = serve(Router::new().route("/responses", post(move |headers: HeaderMap| {
            let dispatched = dispatched.clone(); async move {
                assert_eq!(headers["chatgpt-account-id"], "chatgpt-b");
                dispatched.fetch_add(1, Ordering::SeqCst);
                ([("content-type", "text/event-stream")], completion())
            }
        }))).await;
        let context = ctx(accounts.clone(), upstream);
        let (base, gateway) = serve(router(context.clone())).await;
        let response = reqwest::Client::new().post(format!("{base}/v1/responses"))
            .header("x-fusion-client-key", "test-key")
            .header("x-codex-turn-metadata", metadata("auth-chat", "new-turn"))
            .json(&json!({"input":[{"role":"user","content":"Public fixture first task"},
                {"role":"assistant","content":"Public fixture completed result"},
                {"role":"user","content":"Public fixture follow-up"}],"stream":false}))
            .send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{failure}");
        response.bytes().await.unwrap();
        assert_eq!(dispatches.load(Ordering::SeqCst), 1);
        assert_eq!(refreshes.load(Ordering::SeqCst), usize::from(failure == "expired_rejected"));
        assert_eq!(accounts.store.binding(&chat).unwrap().as_deref(), Some("b"));
        assert_eq!(accounts.store.binding(&binding_key("turn", "new-turn")).unwrap().as_deref(), Some("b"));
        assert!(!context.scheduler.is_active("a") && !context.scheduler.is_active("b"));
        let log = accounts.store.logs().unwrap().into_iter().find(|log| log.operation == "responses").unwrap();
        assert_eq!(log.account.as_deref(), Some("b"));
        let serialized = serde_json::to_string(&log).unwrap();
        assert!(!serialized.contains("Public fixture") && !serialized.contains("fake-refresh-token"));
        gateway.abort(); up.abort(); token_server.abort();
    }
}

#[tokio::test]
async fn authorization_failures_are_request_local_and_bounded_without_repeating_refresh() {
    for count in [2, 10] {
        let (_dir, mut accounts) = fixture();
        for index in 0..count { let id = format!("a{index:02}"); add(&accounts, &id); expire(&accounts, &id); }
        let refreshes = Arc::new(AtomicUsize::new(0)); let refreshed = refreshes.clone();
        let seen = Arc::new(StdMutex::new(std::collections::HashSet::new()));
        let (token_base, token_server) = serve(Router::new().route("/token", post(move |Json(body): Json<Value>| {
            let refreshed = refreshed.clone(); let seen = seen.clone(); async move {
                assert!(seen.lock().unwrap().insert(body["refresh_token"].as_str().unwrap().to_owned()),
                    "the same fake subscription must not be refreshed twice for one request");
                refreshed.fetch_add(1, Ordering::SeqCst);
                StatusCode::SERVICE_UNAVAILABLE
            }
        }))).await;
        Arc::get_mut(&mut accounts).unwrap().use_test_token_endpoint(format!("{token_base}/token"));
        let chat = binding_key("thread", "all-auth-failed-chat");
        accounts.store.bind(&chat, "a00").unwrap();
        let dispatches = Arc::new(AtomicUsize::new(0)); let dispatched = dispatches.clone();
        let (upstream, up) = serve(Router::new().route("/responses", post(move || {
            let dispatched = dispatched.clone(); async move {
                dispatched.fetch_add(1, Ordering::SeqCst);
                ([("content-type", "text/event-stream")], completion())
            }
        }))).await;
        let context = ctx(accounts.clone(), upstream);
        let (base, gateway) = serve(router(context.clone())).await;
        let response = tokio::time::timeout(Duration::from_secs(5), reqwest::Client::new()
            .post(format!("{base}/v1/responses")).header("x-fusion-client-key", "test-key")
            .header("x-codex-turn-metadata", metadata("all-auth-failed-chat", "new-turn"))
            .json(&json!({"input":[{"role":"user","content":"Public fixture task"}],"stream":false})).send())
            .await.unwrap().unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.json::<Value>().await.unwrap()["error"]["code"], "authorization_unavailable");
        assert_eq!(refreshes.load(Ordering::SeqCst), count.min(8));
        assert_eq!(dispatches.load(Ordering::SeqCst), 0);
        assert_eq!(accounts.store.binding(&chat).unwrap().as_deref(), Some("a00"));
        assert!(accounts.store.binding(&binding_key("turn", "new-turn")).unwrap().is_none());
        for index in 0..count {
            let id = format!("a{index:02}");
            assert_eq!(accounts.store.account(&id).unwrap().state, "ready");
            assert!(!context.scheduler.is_active(&id));
        }
        gateway.abort(); up.abort(); token_server.abort();
    }
}

#[tokio::test]
async fn hard_context_authorization_failure_never_uses_a_backup_subscription() {
    let (_dir, mut accounts) = fixture();
    add(&accounts, "a"); add(&accounts, "b"); expire(&accounts, "a");
    let refreshes = Arc::new(AtomicUsize::new(0)); let refreshed = refreshes.clone();
    let (token_base, token_server) = serve(Router::new().route("/token", post(move || {
        let refreshed = refreshed.clone(); async move {
            refreshed.fetch_add(1, Ordering::SeqCst);
            StatusCode::BAD_REQUEST
        }
    }))).await;
    Arc::get_mut(&mut accounts).unwrap().use_test_token_endpoint(format!("{token_base}/token"));
    let chat = binding_key("thread", "hard-auth-chat");
    let reference = binding_key("response", "public-fixture-account-reference");
    accounts.store.bind_many(&[chat.clone(), reference.clone()], "a").unwrap();
    let dispatches = Arc::new(AtomicUsize::new(0)); let dispatched = dispatches.clone();
    let (upstream, up) = serve(Router::new().route("/responses", post(move || {
        let dispatched = dispatched.clone(); async move {
            dispatched.fetch_add(1, Ordering::SeqCst);
            ([("content-type", "text/event-stream")], completion())
        }
    }))).await;
    let (base, gateway) = serve(router(ctx(accounts.clone(), upstream))).await;
    let response = reqwest::Client::new().post(format!("{base}/v1/responses"))
        .header("x-fusion-client-key", "test-key")
        .header("x-codex-turn-metadata", metadata("hard-auth-chat", "hard-turn"))
        .json(&json!({"input":[{"role":"user","content":"Public fixture task"}],
            "previous_response_id":"public-fixture-account-reference","stream":false}))
        .send().await.unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(dispatches.load(Ordering::SeqCst), 0);
    assert_eq!(accounts.store.binding(&chat).unwrap().as_deref(), Some("a"));
    assert_eq!(accounts.store.binding(&reference).unwrap().as_deref(), Some("a"));
    gateway.abort(); up.abort(); token_server.abort();
}

#[tokio::test]
async fn quota_fallback_retains_authorization_exclusions_for_the_entire_request() {
    let (_dir, mut accounts) = fixture();
    for id in ["a", "b", "c"] { add(&accounts, id); }
    expire(&accounts, "a");
    let refreshes = Arc::new(AtomicUsize::new(0)); let refreshed = refreshes.clone();
    let (token_base, token_server) = serve(Router::new().route("/token", post(move || {
        let refreshed = refreshed.clone(); async move {
            refreshed.fetch_add(1, Ordering::SeqCst);
            StatusCode::SERVICE_UNAVAILABLE
        }
    }))).await;
    Arc::get_mut(&mut accounts).unwrap().use_test_token_endpoint(format!("{token_base}/token"));
    let chat = binding_key("thread", "auth-then-quota-chat");
    accounts.store.bind(&chat, "a").unwrap();
    let dispatches = Arc::new(StdMutex::new(Vec::new())); let dispatched = dispatches.clone();
    let (upstream, up) = serve(Router::new().route("/responses", post(move |headers: HeaderMap| {
        let dispatched = dispatched.clone(); async move {
            let account = headers["chatgpt-account-id"].to_str().unwrap().to_owned();
            dispatched.lock().unwrap().push(account.clone());
            if account == "chatgpt-b" {
                (StatusCode::TOO_MANY_REQUESTS,
                    [("content-type", "text/event-stream"), ("retry-after", "3600")], String::new())
            } else {
                assert_eq!(account, "chatgpt-c");
                (StatusCode::OK,
                    [("content-type", "text/event-stream"), ("retry-after", "0")], completion())
            }
        }
    }))).await;
    let (base, gateway) = serve(router(ctx(accounts.clone(), upstream))).await;
    let response = reqwest::Client::new().post(format!("{base}/v1/responses"))
        .header("x-fusion-client-key", "test-key")
        .header("x-codex-turn-metadata", metadata("auth-then-quota-chat", "new-turn"))
        .json(&json!({"input":[{"role":"user","content":"Public fixture task"}],"stream":false}))
        .send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.bytes().await.unwrap();
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(*dispatches.lock().unwrap(), ["chatgpt-b", "chatgpt-c"]);
    assert_eq!(accounts.store.account("a").unwrap().state, "ready");
    assert_eq!(accounts.store.binding(&chat).unwrap().as_deref(), Some("c"));
    gateway.abort(); up.abort(); token_server.abort();
}
