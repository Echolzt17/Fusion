use super::tests::{ctx, serve};
use super::*;
use crate::tests::{add, fixture};
use std::sync::Mutex as StdMutex;

fn turn(thread: &str, turn: &str) -> String {
    json!({"thread_id":thread,"turn_id":turn}).to_string()
}
fn completed(output: Value) -> String {
    format!("data: {}\n\n", json!({"type":"response.completed","response":{"status":"completed","output":output}}))
}
fn exhaust(accounts: &Accounts, id: &str) {
    let mut account = accounts.store.account(id).unwrap();
    account.quota = Some(json!({"updatedAt":crate::now(),"rateLimit":{"allowed":false,"limit_reached":true,
        "primary_window":{"used_percent":100,"reset_at":crate::now()+3600}}}));
    accounts.store.save_account(&account).unwrap();
}

#[tokio::test]
async fn string_user_turn_rolls_on_exhaustion_and_429_and_cannot_be_replayed_as_an_array() {
    for exhausted in [true, false] {
        let (_dir, accounts) = fixture(); add(&accounts, "a"); add(&accounts, "b");
        let chat = binding_key("thread", "string-chat");
        accounts.store.bind(&chat, "a").unwrap();
        let seen = Arc::new(StdMutex::new(Vec::new())); let captured = seen.clone();
        let (upstream, up) = serve(Router::new().route("/responses", post(move |h: HeaderMap, Json(v): Json<Value>| {
            let captured = captured.clone(); async move {
                let account = h["chatgpt-account-id"].to_str().unwrap().to_owned();
                captured.lock().unwrap().push(account.clone());
                assert_eq!(v["input"], json!([{"role":"user","content":[{"type":"input_text","text":"PUBLIC STRING TURN"}]}]));
                if account == "chatgpt-a" {
                    (StatusCode::TOO_MANY_REQUESTS, [("content-type", "text/event-stream")], String::new())
                } else {
                    (StatusCode::OK, [("content-type", "text/event-stream")], completed(json!([])))
                }
            }
        }))).await;
        let context = ctx(accounts.clone(), upstream);
        context.scheduler.select_account(Some("a".into())).unwrap();
        if exhausted { exhaust(&accounts, "a"); }
        let (base, gw) = serve(router(context)).await;
        let client = reqwest::Client::new();
        let url = format!("{base}/v1/responses");
        let headers = turn("string-chat", "new-string-turn");
        let response = client.post(&url).bearer_auth("test-key")
            .header("x-codex-turn-metadata", &headers)
            .json(&json!({"input":"PUBLIC STRING TURN","stream":false})).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK); response.bytes().await.unwrap();
        assert_eq!(accounts.store.binding(&chat).unwrap().as_deref(), Some("b"));
        for input in [json!("PUBLIC STRING TURN"), json!([{"role":"user","content":[{"type":"input_text","text":"PUBLIC STRING TURN"}]}])] {
            let repeated = client.post(&url).bearer_auth("test-key")
                .header("x-codex-turn-metadata", &headers)
                .json(&json!({"input":input,"stream":false})).send().await.unwrap();
            assert_eq!(repeated.status(), StatusCode::BAD_REQUEST);
        }
        assert_eq!(*seen.lock().unwrap(), if exhausted { vec!["chatgpt-b"] } else { vec!["chatgpt-a", "chatgpt-b"] });
        let logs = accounts.store.logs().unwrap();
        let routed = logs.iter().find(|log| log.status == "success").unwrap();
        assert_eq!(routed.diagnostic.as_ref().unwrap().routing, Some(crate::diagnostics::RoutingDecision::Portable));
        assert!(!serde_json::to_string(&logs).unwrap().contains("PUBLIC STRING TURN"));
        gw.abort(); up.abort();
    }
}

#[tokio::test]
async fn quota_refusal_then_backup_queue_cancellation_releases_only_the_unsent_claim() {
    let (_dir, accounts) = fixture(); add(&accounts, "a"); add(&accounts, "b");
    let seen = Arc::new(StdMutex::new(Vec::new())); let captured = seen.clone();
    let (upstream, up) = serve(Router::new().route("/responses", post(move |h: HeaderMap| {
        let captured = captured.clone(); async move {
            let account = h["chatgpt-account-id"].to_str().unwrap().to_owned();
            captured.lock().unwrap().push(account.clone());
            if account == "chatgpt-a" {
                (StatusCode::TOO_MANY_REQUESTS, [("content-type", "text/event-stream")], String::new())
            } else {
                (StatusCode::OK, [("content-type", "text/event-stream")], completed(json!([])))
            }
        }
    }))).await;
    let context = ctx(accounts, upstream);
    context.scheduler.save_service_settings(18765, 1).unwrap();
    context.scheduler.select_account(Some("a".into())).unwrap();
    let occupied = context.scheduler.acquire(Some("b")).await.unwrap();
    let (base, gw) = serve(router(context.clone())).await;
    let client = reqwest::Client::new(); let url = format!("{base}/v1/responses");
    let metadata = turn("cancel-backup-chat", "cancel-backup-turn");
    let body = json!({"input":[{"role":"user","content":"PUBLIC BACKUP QUEUE TASK"}],"stream":false});
    let (pending_client, pending_url, pending_metadata, pending_body) = (client.clone(), url.clone(), metadata.clone(), body.clone());
    let pending = tokio::spawn(async move {
        pending_client.post(pending_url).bearer_auth("test-key")
            .header("x-codex-turn-metadata", pending_metadata).json(&pending_body).send().await.unwrap()
    });
    let id = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if seen.lock().unwrap().len() == 1 {
                if let Some(request) = context.requests.snapshot().into_iter().find(|r| r.phase == "queued") { break request.id; }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("explicit refusal must reach the actual backup queue");
    context.requests.cancel(&id).await.unwrap();
    assert_eq!(pending.await.unwrap().status().as_u16(), 499);
    drop(occupied);
    let retry = client.post(&url).bearer_auth("test-key").header("x-codex-turn-metadata", metadata)
        .json(&body).send().await.unwrap();
    assert_eq!(retry.status(), StatusCode::OK); retry.bytes().await.unwrap();
    assert_eq!(*seen.lock().unwrap(), vec!["chatgpt-a", "chatgpt-b"]);
    gw.abort(); up.abort();
}

#[tokio::test]
async fn encrypted_agent_history_reports_its_actual_blocker_without_sending_or_stripping_it() {
    let (_dir, accounts) = fixture(); add(&accounts, "a"); add(&accounts, "b");
    let chat = binding_key("thread", "encrypted-agent-chat");
    accounts.store.bind(&chat, "a").unwrap();
    accounts.store.bind(&binding_key("opaque", "PRIVATE_AGENT_CIPHER"), "a").unwrap();
    exhaust(&accounts, "a");
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0)); let count = calls.clone();
    let (upstream, up) = serve(Router::new().route("/responses", post(move || {
        let count = count.clone(); async move {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            ([("content-type", "text/event-stream")], completed(json!([])))
        }
    }))).await;
    let (base, gateway) = serve(router(ctx(accounts.clone(), upstream))).await;
    let response = reqwest::Client::new().post(format!("{base}/v1/responses")).bearer_auth("test-key")
        .header("x-codex-turn-metadata", turn("encrypted-agent-chat", "new-turn"))
        .json(&json!({"input":[{"role":"user","content":"PUBLIC ORIGINAL TASK"},
            {"type":"agent_message","author":"public-agent","recipient":"all","content":[
                {"type":"input_text","text":"PUBLIC READABLE PART"},
                {"type":"encrypted_content","encrypted_content":"PRIVATE_AGENT_CIPHER"}]},
            {"role":"user","content":"PUBLIC FOLLOWUP"}],"stream":false})).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let error = response.json::<Value>().await.unwrap();
    assert!(error["error"]["message"].as_str().unwrap().contains("账号专属加密内容"));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(accounts.store.binding(&chat).unwrap().as_deref(), Some("a"));
    let logs = accounts.store.logs().unwrap();
    assert_eq!(logs[0].diagnostic.as_ref().unwrap().routing, Some(crate::diagnostics::RoutingDecision::EncryptedHistory));
    let serialized = serde_json::to_string(&logs).unwrap();
    assert!(!serialized.contains("PRIVATE_AGENT_CIPHER") && !serialized.contains("PUBLIC FOLLOWUP"));
    gateway.abort(); up.abort();
}

#[tokio::test]
async fn unknown_reasoning_in_a_readable_user_turn_is_never_dropped_to_enable_rollover() {
    let (_dir, accounts) = fixture(); add(&accounts, "a"); add(&accounts, "b");
    let chat = binding_key("thread", "unknown-reasoning-chat");
    accounts.store.bind(&chat, "a").unwrap();
    exhaust(&accounts, "a");
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0)); let count = calls.clone();
    let (upstream, up) = serve(Router::new().route("/responses", post(move || {
        let count = count.clone(); async move {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            ([("content-type", "text/event-stream")], completed(json!([])))
        }
    }))).await;
    let (base, gateway) = serve(router(ctx(accounts.clone(), upstream))).await;
    let response = reqwest::Client::new().post(format!("{base}/v1/responses")).bearer_auth("test-key")
        .header("x-codex-turn-metadata", turn("unknown-reasoning-chat", "new-turn"))
        .json(&json!({"input":[{"role":"user","content":"PUBLIC ORIGINAL TASK"},
            {"type":"reasoning","encrypted_content":"UNOWNED_PRIVATE_REASONING"},
            {"role":"user","content":"PUBLIC FOLLOWUP"}],"stream":false})).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(accounts.store.binding(&chat).unwrap().as_deref(), Some("a"));
    assert!(accounts.store.binding(&binding_key("opaque", "UNOWNED_PRIVATE_REASONING")).unwrap().is_none());
    let logs = accounts.store.logs().unwrap();
    assert_eq!(logs[0].diagnostic.as_ref().unwrap().routing, Some(crate::diagnostics::RoutingDecision::EncryptedHistory));
    assert!(!serde_json::to_string(&logs).unwrap().contains("UNOWNED_PRIVATE_REASONING"));
    gateway.abort(); up.abort();
}

#[tokio::test]
async fn nested_account_cipher_cannot_follow_a_chat_preference_to_another_account() {
    for known in [false, true] {
        for nested in [
            json!({"type":"agent_message","author":"public-agent","recipient":"all","content":[
                {"type":"input_text","text":"PUBLIC HANDOFF"},
                {"type":"encrypted_content","encrypted_content":"PRIVATE_NESTED_CIPHER"}]}),
            json!({"type":"function_call","name":"read","call_id":"old-call","arguments":"{}",
                "encrypted_function_args":["PRIVATE_NESTED_CIPHER"]}),
        ] {
            let (_dir, accounts) = fixture(); add(&accounts, "a"); add(&accounts, "b");
            let chat = binding_key("thread", "nested-cipher-chat");
            accounts.store.bind(&chat, "a").unwrap();
            accounts.store.switch_chat_bindings(std::slice::from_ref(&chat), "a", "b").unwrap();
            if known { accounts.store.bind(&binding_key("opaque", "PRIVATE_NESTED_CIPHER"), "a").unwrap(); }
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0)); let count = calls.clone();
            let (upstream, up) = serve(Router::new().route("/responses", post(move || {
                let count = count.clone(); async move {
                    count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    ([("content-type", "text/event-stream")], completed(json!([])))
                }
            }))).await;
            let context = ctx(accounts.clone(), upstream);
            let item = json!({"output":[nested.clone()]});
            let keys = continuity::output_keys(&item);
            assert!(keys.contains(&binding_key("opaque", "PRIVATE_NESTED_CIPHER")), "output and input must share the same ownership extraction");
            let (base, gateway) = serve(router(context)).await;
            let response = reqwest::Client::new().post(format!("{base}/v1/responses")).bearer_auth("test-key")
                .header("x-codex-turn-metadata", turn("nested-cipher-chat", "new-turn"))
                .json(&json!({"input":[{"role":"user","content":"PUBLIC TASK"},nested,
                    {"role":"user","content":"PUBLIC FOLLOWUP"}],"stream":false})).send().await.unwrap();
            assert_eq!(response.status(), StatusCode::CONFLICT);
            assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
            assert_eq!(accounts.store.binding(&chat).unwrap().as_deref(), Some("b"));
            assert!(!serde_json::to_string(&accounts.store.logs().unwrap()).unwrap().contains("PRIVATE_NESTED_CIPHER"));
            gateway.abort(); up.abort();
        }
    }
}

#[tokio::test]
async fn exhausted_chat_rolls_to_backup_and_tool_continuation_keeps_new_account() {
    let (_dir, accounts) = fixture(); add(&accounts,"a"); add(&accounts,"b");
    let chat = binding_key("thread","same-local-chat");
    accounts.store.bind(&chat,"a").unwrap();
    accounts.store.bind(&binding_key("opaque","OLD_PRIVATE_REASONING"),"a").unwrap();
    exhaust(&accounts,"a");
    let seen = Arc::new(StdMutex::new(Vec::new())); let captured = seen.clone();
    let (upstream, up) = serve(Router::new().route("/responses",post(move |h: HeaderMap, Json(v): Json<Value>| {
        let captured = captured.clone(); async move {
            assert_eq!(h["chatgpt-account-id"],"chatgpt-b");
            assert!(!v.to_string().contains("OLD_PRIVATE_REASONING"));
            let mut seen = captured.lock().unwrap();
            if seen.is_empty() { assert!(!h.contains_key("x-codex-turn-state")); }
            else { assert!(v.to_string().contains("NEW_PRIVATE_REASONING")); }
            seen.push(v);
            ([("content-type","text/event-stream")],completed(json!([
                {"type":"reasoning","encrypted_content":"NEW_PRIVATE_REASONING"},
                {"type":"function_call","id":"new-item","call_id":"local-read","name":"read_file","arguments":"{}"}
            ])))
        }
    }))).await;
    let context = ctx(accounts.clone(),upstream);
    let (base, gw) = serve(router(context)).await;
    let history = json!([
        {"role":"user","content":"original task"},
        {"type":"reasoning","encrypted_content":"OLD_PRIVATE_REASONING"},
        {"role":"assistant","content":"previous work is complete"},
        {"role":"user","content":"continue in the same local chat"}
    ]);
    let client = reqwest::Client::new();
    let first = client.post(format!("{base}/v1/responses")).bearer_auth("test-key")
        .header("x-codex-turn-metadata",turn("same-local-chat","new-turn"))
        .header("x-codex-turn-state","old-account-state")
        .json(&json!({"input":history,"stream":false})).send().await.unwrap();
    assert_eq!(first.status(),StatusCode::OK); first.bytes().await.unwrap();
    assert_eq!(accounts.store.binding(&chat).unwrap().as_deref(),Some("b"));
    assert_eq!(accounts.store.binding(&binding_key("opaque","OLD_PRIVATE_REASONING")).unwrap().as_deref(),Some("a"));
    let mut next = history.as_array().unwrap().clone();
    next.extend([
        json!({"type":"reasoning","encrypted_content":"NEW_PRIVATE_REASONING"}),
        json!({"type":"function_call","id":"new-item","call_id":"local-read","name":"read_file","arguments":"{}"}),
        json!({"type":"function_call_output","call_id":"local-read","output":"read exactly once"})
    ]);
    let second = client.post(format!("{base}/v1/responses")).bearer_auth("test-key")
        .header("x-codex-turn-metadata",turn("same-local-chat","new-turn"))
        .json(&json!({"input":next,"stream":false})).send().await.unwrap();
    assert_eq!(second.status(),StatusCode::OK); second.bytes().await.unwrap();
    assert_eq!(seen.lock().unwrap().len(),2);
    let logs = serde_json::to_string(&accounts.store.logs().unwrap()).unwrap();
    assert!(!logs.contains("PRIVATE_REASONING") && !logs.contains("original task") && !logs.contains("read exactly once"));
    gw.abort(); up.abort();
}

#[tokio::test]
async fn explicit_quota_refusal_selects_backup_but_server_error_is_not_replayed() {
    for status in [StatusCode::TOO_MANY_REQUESTS,StatusCode::INTERNAL_SERVER_ERROR] {
        let (_dir, accounts) = fixture(); add(&accounts,"a"); add(&accounts,"b");
        let seen = Arc::new(StdMutex::new(Vec::new())); let captured = seen.clone();
        let (upstream, up) = serve(Router::new().route("/responses",post(move |h: HeaderMap| {
            let captured = captured.clone(); async move {
                let account = h["chatgpt-account-id"].to_str().unwrap().to_owned();
                captured.lock().unwrap().push(account.clone());
                if account == "chatgpt-a" { (status,[("content-type","text/event-stream")],"private error body".to_owned()) }
                else { (StatusCode::OK,[("content-type","text/event-stream")],completed(json!([]))) }
            }
        }))).await;
        let context = ctx(accounts.clone(),upstream);
        context.scheduler.select_account(Some("a".into())).unwrap();
        let (base, gw) = serve(router(context)).await;
        let response = reqwest::Client::new().post(format!("{base}/v1/responses")).bearer_auth("test-key")
            .header("x-codex-turn-metadata",turn("quota-chat","first-turn"))
            .json(&json!({"input":[{"role":"user","content":"one task"}],"stream":false})).send().await.unwrap();
        assert_eq!(response.status(),if status==StatusCode::TOO_MANY_REQUESTS { StatusCode::OK } else { status });
        assert!(!response.text().await.unwrap().contains("private error body"));
        assert_eq!(*seen.lock().unwrap(),if status==StatusCode::TOO_MANY_REQUESTS { vec!["chatgpt-a","chatgpt-b"] } else { vec!["chatgpt-a"] });
        gw.abort(); up.abort();
    }
}

#[tokio::test]
async fn legacy_header_auth_discards_personal_credentials_and_rejects_fallback() {
    let (_dir, accounts) = fixture(); add(&accounts,"a");
    let (upstream, up) = serve(Router::new().route("/responses",post(|h: HeaderMap|async move {
        assert_eq!(h["authorization"],"Bearer fake-access-token");
        assert!(!h.contains_key("x-fusion-client-key"));
        ([("content-type","text/event-stream")],completed(json!([])))
    }))).await;
    let (base,gw) = serve(router(ctx(accounts.clone(),upstream))).await;
    let client = reqwest::Client::new();
    let url = format!("{base}/v1/responses");
    let denied = client.post(&url).bearer_auth("test-key").header("x-fusion-client-key", "wrong")
        .body("not json").send().await.unwrap();
    assert_eq!(denied.status(),StatusCode::UNAUTHORIZED);
    let accepted = client.post(&url).bearer_auth("PERSONAL_CREDENTIAL_MUST_BE_DISCARDED")
        .header("x-fusion-client-key", "test-key")
        .header("x-codex-turn-metadata", turn("authenticated-chat", "authenticated-turn"))
        .json(&json!({"input":[{"role":"user","content":"hello"}],"stream":false})).send().await.unwrap();
    assert_eq!(accepted.status(),StatusCode::OK); accepted.bytes().await.unwrap();
    assert!(!serde_json::to_string(&accounts.store.logs().unwrap()).unwrap().contains("PERSONAL_CREDENTIAL_MUST_BE_DISCARDED"));
    gw.abort(); up.abort();
}

#[tokio::test]
async fn rollover_late_original_turn_keeps_its_body_and_never_moves_to_new_account() {
    let (_dir, accounts) = fixture(); add(&accounts, "a"); add(&accounts, "b");
    let chat = binding_key("thread", "late-chat");
    accounts.store.bind(&chat, "a").unwrap();
    accounts.store.switch_chat_bindings(std::slice::from_ref(&chat), "a", "b").unwrap();
    accounts.store.bind(&binding_key("turn", "old-turn"), "a").unwrap();
    accounts.store.bind(&binding_key("opaque", "OLD_TURN_REASONING"), "a").unwrap();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0)); let count = calls.clone();
    let (upstream, up) = serve(Router::new().route("/responses", post(move || {
        let count = count.clone(); async move {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            ([("content-type", "text/event-stream")], completed(json!([])))
        }
    }))).await;
    let context = ctx(accounts.clone(), upstream);
    let mut headers = HeaderMap::new();
    headers.insert("x-codex-turn-metadata", turn("late-chat", "old-turn").parse().unwrap());
    headers.insert("x-codex-turn-state", "original-turn-state".parse().unwrap());
    let original = json!({"input":[{"role":"user","content":"old task"},
        {"type":"reasoning","encrypted_content":"OLD_TURN_REASONING"},
        {"type":"function_call","id":"original-item","call_id":"read","name":"read","arguments":"{}"},
        {"type":"function_call_output","call_id":"read","output":"already read"}],"stream":false});
    let mut preserved = original.clone();
    portable::sanitize_rolled_history(&context, &mut headers, &mut preserved).unwrap();
    assert_eq!(preserved, original);
    assert_eq!(headers["x-codex-turn-state"], "original-turn-state");
    let (base, gateway) = serve(router(context)).await;
    let response = reqwest::Client::new().post(format!("{base}/v1/responses"))
        .bearer_auth("test-key").headers(headers).json(&original).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(accounts.store.binding(&binding_key("turn", "old-turn")).unwrap().as_deref(), Some("a"));
    gateway.abort(); up.abort();
}

#[tokio::test]
async fn rollover_unknown_current_reasoning_cannot_be_stripped_from_tool_continuation() {
    let (_dir, accounts) = fixture(); add(&accounts, "a"); add(&accounts, "b");
    let chat = binding_key("thread", "unknown-chat");
    accounts.store.bind(&chat, "a").unwrap();
    accounts.store.switch_chat_bindings(std::slice::from_ref(&chat), "a", "b").unwrap();
    accounts.store.bind(&binding_key("turn", "current-turn"), "b").unwrap();
    accounts.store.bind(&binding_key("opaque", "OLD_REASONING"), "a").unwrap();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0)); let count = calls.clone();
    let (upstream, up) = serve(Router::new().route("/responses", post(move || {
        let count = count.clone(); async move {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            ([("content-type", "text/event-stream")], completed(json!([])))
        }
    }))).await;
    let (base, gateway) = serve(router(ctx(accounts.clone(), upstream))).await;
    let response = reqwest::Client::new().post(format!("{base}/v1/responses")).bearer_auth("test-key")
        .header("x-codex-turn-metadata", turn("unknown-chat", "current-turn"))
        .json(&json!({"input":[{"role":"user","content":"original task"},
            {"type":"reasoning","encrypted_content":"OLD_REASONING"},
            {"role":"user","content":"new task"},
            {"type":"reasoning","encrypted_content":"UNKNOWN_CURRENT_REASONING"},
            {"type":"function_call","call_id":"read","name":"read","arguments":"{}"},
            {"type":"function_call_output","call_id":"read","output":"already read"}],"stream":false}))
        .send().await.unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(accounts.store.binding(&binding_key("opaque", "UNKNOWN_CURRENT_REASONING")).unwrap().is_none());
    gateway.abort(); up.abort();
}

#[tokio::test]
async fn rollover_compaction_stays_with_exhausted_original_account_without_sending_to_backup() {
    let (_dir, accounts) = fixture(); add(&accounts, "a"); add(&accounts, "b");
    let chat = binding_key("thread", "compact-chat");
    accounts.store.bind(&chat, "a").unwrap();
    accounts.store.bind(&binding_key("opaque", "ORIGINAL_COMPACTION"), "a").unwrap();
    exhaust(&accounts, "a");
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0)); let count = calls.clone();
    let (upstream, up) = serve(Router::new().route("/responses", post(move || {
        let count = count.clone(); async move {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            ([("content-type", "text/event-stream")], completed(json!([])))
        }
    }))).await;
    let (base, gateway) = serve(router(ctx(accounts.clone(), upstream))).await;
    let response = reqwest::Client::new().post(format!("{base}/v1/responses")).bearer_auth("test-key")
        .header("x-codex-turn-metadata", turn("compact-chat", "next-turn"))
        .json(&json!({"input":[{"role":"user","content":"original task"},
            {"type":"compaction","encrypted_content":"ORIGINAL_COMPACTION"},
            {"role":"user","content":"continue"}],"stream":false})).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(accounts.store.binding(&chat).unwrap().as_deref(), Some("a"));
    gateway.abort(); up.abort();
}

#[tokio::test]
async fn legacy_replay_responses_block_completed_server_error_and_output_eof_retries() {
    for mode in ["completed", "server_error", "output_eof"] {
        let (_dir, accounts) = fixture(); add(&accounts, "a"); add(&accounts, "b");
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0)); let count = calls.clone();
        let (upstream, up) = serve(Router::new().route("/responses", post(move || {
            let count = count.clone(); async move {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let (status, body) = match mode {
                    "server_error" => (StatusCode::INTERNAL_SERVER_ERROR, "private upstream error".into()),
                    "output_eof" => (StatusCode::OK, format!("data: {}\n\n", json!({"type":"response.output_text.delta","delta":"PRIVATE_OUTPUT_CANARY"}))),
                    _ => (StatusCode::OK, completed(json!([]))),
                };
                (status, [("content-type", "text/event-stream")], body)
            }
        }))).await;
        let (base, gateway) = serve(router(ctx(accounts.clone(), upstream))).await;
        let url = format!("{base}/v1/responses");
        let body = json!({"input":[{"role":"user","content":"PRIVATE_INPUT_CANARY"}],"stream":true});
        let client = reqwest::Client::new();
        let first = client.post(&url).header("x-fusion-client-key", "test-key")
            .bearer_auth("PERSONAL_CREDENTIAL_MUST_BE_DISCARDED")
            .header("x-codex-turn-metadata", turn("legacy-chat", "same-turn"))
            .json(&body).send().await.unwrap();
        assert_eq!(first.status(), if mode == "server_error" { StatusCode::INTERNAL_SERVER_ERROR } else { StatusCode::OK });
        let _ = first.bytes().await;
        // An intervening quota refresh must not turn the repeated turn into a B dispatch.
        exhaust(&accounts, "a");
        let retry = client.post(&url).header("x-fusion-client-key", "test-key")
            .bearer_auth("PERSONAL_CREDENTIAL_MUST_BE_DISCARDED")
            .header("x-codex-turn-metadata", turn("legacy-chat", "same-turn"))
            .json(&body).send().await.unwrap();
        assert_eq!(retry.status(), StatusCode::BAD_REQUEST);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let logs = serde_json::to_string(&accounts.store.logs().unwrap()).unwrap();
        assert!(!logs.contains("PRIVATE_INPUT_CANARY") && !logs.contains("PRIVATE_OUTPUT_CANARY"));
        gateway.abort(); up.abort();
    }
}

#[tokio::test]
async fn replay_guard_cancelled_queued_turn_can_be_sent_once_after_capacity_recovers() {
    let (_dir, accounts) = fixture(); add(&accounts, "a");
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0)); let count = calls.clone();
    let (upstream, up) = serve(Router::new().route("/responses", post(move || {
        let count = count.clone(); async move {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            ([("content-type", "text/event-stream")], completed(json!([])))
        }
    }))).await;
    let context = ctx(accounts, upstream);
    context.scheduler.save_service_settings(18765, 1).unwrap();
    let occupied = context.scheduler.acquire(Some("a")).await.unwrap();
    let (base, gateway) = serve(router(context.clone())).await;
    let url = format!("{base}/v1/responses");
    let metadata = turn("queued-cancel-chat", "queued-cancel-turn");
    let body = json!({"input":[{"role":"user","content":"public queued task"}],"stream":false});
    let client = reqwest::Client::new();
    let queued_client = client.clone(); let queued_url = url.clone();
    let queued_metadata = metadata.clone(); let queued_body = body.clone();
    let pending = tokio::spawn(async move {
        queued_client.post(queued_url).header("x-fusion-client-key", "test-key")
            .header("x-codex-turn-metadata", queued_metadata).json(&queued_body).send().await.unwrap()
    });
    let id = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(request) = context.requests.snapshot().into_iter().find(|request| request.phase == "queued") {
                break request.id;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("request must enter the actual account-capacity queue before cancellation");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    context.requests.cancel(&id).await.unwrap();
    let cancelled = tokio::time::timeout(Duration::from_secs(3), pending).await.unwrap().unwrap();
    assert_eq!(cancelled.status().as_u16(), 499);
    assert!(context.requests.snapshot().is_empty());
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    drop(occupied);
    let retried = client.post(&url).header("x-fusion-client-key", "test-key")
        .header("x-codex-turn-metadata", metadata).json(&body).send().await.unwrap();
    assert_eq!(retried.status(), StatusCode::OK);
    retried.bytes().await.unwrap();
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    gateway.abort(); up.abort();
}
