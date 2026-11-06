use super::*;
use crate::{
    account::{Accounts, Tokens},
    scheduler::Scheduler,
    storage::Store,
    vault::MemoryVault,
};
use std::sync::Arc;

pub fn fixture() -> (tempfile::TempDir, Arc<Accounts>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&dir.path().join("pool.sqlite")).unwrap());
    let vault = Arc::new(MemoryVault::default());
    let accounts = Arc::new(Accounts::new(store, vault).unwrap());
    (dir, accounts)
}
pub fn add(a: &Accounts, id: &str) {
    a.store
        .save_account(&Account {
            id: id.into(),
            email: format!("{id}@example.test"),
            account_id: format!("chatgpt-{id}"),
            state: "ready".into(),
            cooldown_until: 0,
            quota: None,
            quota_error: None,
        })
        .unwrap();
    a.vault
        .set(
            &format!("account/{id}"),
            &serde_json::to_string(&Tokens {
                access_token: "fake-access-token".into(),
                refresh_token: "fake-refresh-token".into(),
                id_token: "fake-id-token".into(),
                expires_at: now() + 3600,
            })
            .unwrap(),
        )
        .unwrap();
}

#[tokio::test]
async fn resets_bind_account_whitelist_and_reuse_request_after_uncertain_result() {
    use axum::{
        routing::{get, post},
        Json,
    };
    use serde_json::{json, Value};
    let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let captured = seen.clone();
    let app = axum::Router::new().route("/credits", get(|| async {
        Json(json!({"available_count":1,"credits":[{"id":"card-a","reset_type":"codex_rate_limits","status":"available","expires_at":null,"private":"discard"}],"private":"discard"}))
    })).route("/credits/consume", post(move |headers: axum::http::HeaderMap, Json(body): Json<Value>| {
        let captured = captured.clone();
        async move {
            assert_eq!(headers["chatgpt-account-id"], "chatgpt-a");
            assert_eq!(headers["authorization"], "Bearer fake-access-token");
            assert_eq!(body["credit_id"], "card-a");
            let mut seen = captured.lock().unwrap();
            seen.push(body["redeem_request_id"].as_str().unwrap().to_owned());
            if seen.len() == 1 { (axum::http::StatusCode::BAD_GATEWAY, Json(json!({"secret":"never expose"}))) }
            else { (axum::http::StatusCode::OK, Json(json!({"code":"reset","windows_reset":2}))) }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/credits", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (_dir, mut a) = fixture();
    Arc::get_mut(&mut a).unwrap().reset_endpoint = endpoint;
    add(&a, "a");
    add(&a, "b");
    a.store.bind("session:existing", "a").unwrap();
    a.mark_failure("a", 429, 60).await.unwrap();
    let details = a.reset_credits("a").await.unwrap();
    assert!(!serde_json::to_string(&details).unwrap().contains("private"));
    assert!(a.consume_reset("a", "missing").await.is_err());
    assert!(seen.lock().unwrap().is_empty());
    let error = a.consume_reset("a", "card-a").await.unwrap_err();
    assert!(error.contains("未确认"));
    assert!(!error.contains("secret"));
    assert_eq!(seen.lock().unwrap().len(), 1); // No automatic replay.
    assert!(a
        .consume_reset("a", "card-a")
        .await
        .unwrap()
        .contains("已重置"));
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0], seen[1]);
    assert_eq!(a.store.account("a").unwrap().cooldown_until, 0);
    assert_eq!(
        a.store.binding("session:existing").unwrap().as_deref(),
        Some("a")
    );
    server.abort();
}

#[tokio::test]
async fn reset_credit_query_failure_is_not_zero_and_never_consumes() {
    let (_dir, mut a) = fixture();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/credits", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            axum::Router::new().fallback(|| async { axum::http::StatusCode::FORBIDDEN }),
        )
        .await
        .unwrap();
    });
    Arc::get_mut(&mut a).unwrap().reset_endpoint = endpoint;
    add(&a, "a");
    assert!(a
        .reset_credits("a")
        .await
        .unwrap_err_string()
        .contains("不代表没有"));
    assert!(a.consume_reset("a", "card-a").await.is_err());
    assert!(a.store.get("reset-request/a/card-a").unwrap().is_none());
    server.abort();
}
#[test]
fn data_survives_reopen_and_credentials_are_not_in_db() {
    let (dir, a) = fixture();
    add(&a, "a");
    a.store.bind("session:a", "a").unwrap();
    let reopened = Store::open(&dir.path().join("pool.sqlite")).unwrap();
    assert_eq!(reopened.accounts().unwrap().len(), 1);
    assert_eq!(reopened.binding("session:a").unwrap().as_deref(), Some("a"));
    for e in std::fs::read_dir(dir.path()).unwrap() {
        let b = std::fs::read(e.unwrap().path()).unwrap();
        let s = String::from_utf8_lossy(&b);
        assert!(!s.contains("fake-access-token"));
        assert!(!s.contains("fake-refresh-token"));
    }
}
#[test]
fn session_binding_cannot_change_accounts() {
    let (_d, a) = fixture();
    add(&a, "a");
    add(&a, "b");
    a.store.bind("s", "a").unwrap();
    assert!(a.store.bind("s", "b").is_err());
    assert_eq!(a.store.binding("s").unwrap().as_deref(), Some("a"));
}
#[tokio::test]
async fn scheduler_stays_on_current_account_and_preserves_pin() {
    let (_d, a) = fixture();
    add(&a, "a");
    add(&a, "b");
    let s = Scheduler::new(a.store.clone());
    let first = s.acquire(None).await.unwrap();
    let second = s.acquire(None).await.unwrap();
    assert_eq!(first.account.id, second.account.id);
    drop(second);
    let pin = s.acquire(Some(&first.account.id)).await.unwrap();
    assert_eq!(pin.account.id, first.account.id);
    drop(pin);
    let id = first.account.id.clone();
    drop(first);
    assert!(!s.is_active(&id));
    a.set_state(&id, true).await.unwrap();
    assert!(s
        .acquire(Some(&id))
        .await
        .unwrap_err_string()
        .contains("原会话"));
}
trait ErrorText<T> {
    fn unwrap_err_string(self) -> String;
}
impl<T> ErrorText<T> for Result<T> {
    fn unwrap_err_string(self) -> String {
        match self {
            Err(e) => e,
            Ok(_) => panic!("expected error"),
        }
    }
}
#[tokio::test]
async fn paused_cooling_and_reauth_accounts_are_not_used() {
    let (_d, a) = fixture();
    add(&a, "a");
    add(&a, "b");
    a.set_state("a", true).await.unwrap();
    // Long cooldown remains an immediate refusal; short waits are tested separately.
    a.mark_failure("b", 429, 120).await.unwrap();
    let s = Scheduler::new(a.store.clone());
    assert!(s.acquire(None).await.is_err());
    a.mark_failure("b", 401, 0).await.unwrap();
    assert!(a.set_state("b", false).await.is_err());
    assert!(a.token("b").await.is_err());
}
#[tokio::test]
async fn fixed_account_is_respected() {
    let (_d, a) = fixture();
    add(&a, "a");
    add(&a, "b");
    let mut cfg = a.store.settings().unwrap();
    cfg.fixed_account = Some("b".into());
    a.store.save_settings(&cfg).unwrap();
    let s = Scheduler::new(a.store.clone());
    assert_eq!(s.acquire(None).await.unwrap().account.id, "b");
}
#[test]
fn refresh_response_can_omit_rotated_fields() {
    let old = Tokens {
        access_token: "old".into(),
        refresh_token: "refresh".into(),
        id_token: "id".into(),
        expires_at: 0,
    };
    let t = account::tokens_from_response(
        &serde_json::json!({"access_token":"new","expires_in":3600}),
        Some(&old),
    )
    .unwrap();
    assert_eq!(t.refresh_token, "refresh");
    assert_eq!(t.id_token, "id");
    assert!(t.expires_at > now());
}

#[test]
fn old_concurrency_default_upgrades_on_read_but_explicit_new_choices_survive() {
    let (_dir, accounts) = fixture();
    let old = r#"{"port":18901,"concurrency":2,"fixedAccount":"a","selectionPolicy":"auto"}"#;
    accounts.store.set("settings", old).unwrap();
    let mut settings = accounts.store.settings().unwrap();
    assert_eq!(settings.concurrency, 4);
    assert_eq!(settings.port, 18901);
    assert_eq!(settings.fixed_account.as_deref(), Some("a"));
    assert_eq!(accounts.store.get("settings").unwrap().as_deref(), Some(old), "reading performs no data migration");
    settings.concurrency = 2;
    accounts.store.save_settings(&settings).unwrap();
    assert_eq!(accounts.store.settings().unwrap().concurrency, 2);
    accounts.store.save_scheduling(&settings, Some("a")).unwrap();
    assert_eq!(accounts.store.settings().unwrap().concurrency, 2);
    for limit in [1, 3, 8, 16] {
        accounts.store.set("settings", &format!(r#"{{"port":18765,"concurrency":{limit},"fixedAccount":null}}"#)).unwrap();
        assert_eq!(accounts.store.settings().unwrap().concurrency, limit);
    }
}

#[tokio::test]
async fn token_refresh_is_serialized_and_survives_caller_cancellation() {
    use axum::{routing::post, Json, Router};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (_dir, mut a) = fixture();
    add(&a, "a");
    let raw = a.vault.get("account/a").unwrap().unwrap();
    let mut old: Tokens = serde_json::from_str(&raw).unwrap();
    old.expires_at = 0;
    a.vault
        .set("account/a", &serde_json::to_string(&old).unwrap())
        .unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let n = count.clone();
    let s = started.clone();
    let r = release.clone();
    let router=Router::new().route("/token",post(move||{let(n,s,r)=(n.clone(),s.clone(),r.clone());async move{n.fetch_add(1,Ordering::SeqCst);s.notify_one();r.notified().await;Json(serde_json::json!({"access_token":"rotated-access","refresh_token":"rotated-refresh","expires_in":3600}))}}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/token", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    Arc::get_mut(&mut a)
        .unwrap()
        .use_test_token_endpoint(endpoint);
    let first = {
        let a = a.clone();
        tokio::spawn(async move { a.token("a").await })
    };
    tokio::time::timeout(std::time::Duration::from_secs(3), started.notified())
        .await
        .unwrap();
    let second = {
        let a = a.clone();
        tokio::spawn(async move { a.token("a").await })
    };
    first.abort();
    assert!(tokio::time::timeout(std::time::Duration::from_millis(30), a.wait_background_idle()).await.is_err(), "update must wait for detached token rotation");
    release.notify_one();
    let token = tokio::time::timeout(std::time::Duration::from_secs(3), second)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(token.access_token, "rotated-access");
    assert_eq!(token.refresh_token, "rotated-refresh");
    assert_eq!(count.load(Ordering::SeqCst), 1);
    tokio::time::timeout(std::time::Duration::from_secs(3), a.wait_background_idle()).await.unwrap().unwrap();
    server.abort();
}
#[tokio::test]
async fn reauthorization_rejects_a_different_account() {
    use base64::Engine;
    let (_d, a) = fixture();
    let claim = serde_json::json!({"email":"user@example.test","https://api.openai.com/auth":{"chatgpt_account_id":"new-account"}});
    let jwt = format!(
        "header.{}.signature",
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&claim).unwrap())
    );
    let t = Tokens {
        access_token: "access".into(),
        refresh_token: "refresh".into(),
        id_token: jwt,
        expires_at: now() + 3600,
    };
    assert!(a.accept_login(t, Some("different-id")).await.is_err());
    assert!(a.store.accounts().unwrap().is_empty());
}
#[test]
fn config_apply_restore_preserves_unrelated_edits_and_has_conflict_detection() {
    let (dir, a) = fixture();
    let path = dir.path().join("config.toml");
    let exe = dir.path().join("pool.exe");
    let original="model_provider = \"custom\"\nmodel = \"existing-model\"\n# leave me\n[projects.hello]\ntrust_level = \"trusted\"\n[model_providers.custom]\nbase_url = \"https://example.test/v1\"\n";
    std::fs::write(&path, original).unwrap();
    let preview = config::preview(&a.store, &path, &exe, &a.api_key().unwrap()).unwrap();
    assert!(!preview.after.contains("api_key"));
    config::apply(&a.store, &path, &exe, &a.api_key().unwrap(), &preview.fingerprint).unwrap();
    let applied = std::fs::read_to_string(&path).unwrap();
    assert!(applied.contains("existing-model"));
    assert!(applied.contains("leave me"));
    assert!(applied.contains("request_max_retries = 0"));
    std::fs::write(
        &path,
        applied.replace("trust_level = \"trusted\"", "trust_level = \"untrusted\""),
    )
    .unwrap();
    recover(&a.store, &path).unwrap();
    let restored = std::fs::read_to_string(&path).unwrap();
    assert!(restored.contains("untrusted"));
    assert!(restored.contains("model_provider = \"custom\""));
    assert!(restored.contains("personal_codex_pool]"));
    let preview = config::preview(&a.store, &path, &exe, &a.api_key().unwrap()).unwrap();
    config::apply(&a.store, &path, &exe, &a.api_key().unwrap(), &preview.fingerprint).unwrap();
    let applied = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        applied.replace("supports_websockets = false", "supports_websockets = true"),
    )
    .unwrap();
    assert!(recover(&a.store, &path).is_err());
}
#[test]
fn config_refuses_stale_preview() {
    let (dir, a) = fixture();
    let path = dir.path().join("config.toml");
    let p = config::preview(&a.store, &path, dir.path(), &a.api_key().unwrap()).unwrap();
    std::fs::write(&path, "model = 'changed'").unwrap();
    assert!(config::apply(&a.store, &path, dir.path(), &a.api_key().unwrap(), &p.fingerprint).is_err());
    assert_eq!(std::fs::read_to_string(path).unwrap(), "model = 'changed'");
}
#[test]
fn usage_is_persistent_deduplicated_and_survives_log_pruning() {
    use crate::usage::TokenUsage;
    let (dir, a) = fixture();
    add(&a, "a");
    let mut log = RequestLog {
        started_at: None, requested_model: None, response_model: None,
        id: "measured".into(),
        at: now(),
        account: Some("a".into()),
        conversation_key: None,
        operation: "responses".into(),
        status: "success".into(),
        elapsed_ms: 1,
        advice: String::new(),
        usage: TokenUsage::from_response(
            &serde_json::json!({"usage":{"input_tokens":100,"output_tokens":20,"input_tokens_details":{"cached_tokens":80},"output_tokens_details":{"reasoning_tokens":10}}}),
        ),
        diagnostic: None,
    };
    a.store.log(&log).unwrap();
    a.store.log(&log).unwrap();
    log.id = "unknown".into();
    log.usage = None;
    log.status = "stream_interrupted".into();
    a.store.log(&log).unwrap();
    // Models do not consume inference request counts. Push measured logs out of retention.
    log.operation = "models".into();
    for i in 0..2001 {
        log.id = format!("catalog-{i}");
        a.store.log(&log).unwrap();
    }
    let reopened = Store::open(&dir.path().join("pool.sqlite")).unwrap();
    let u = reopened.usage().unwrap().remove(0);
    assert_eq!((u.requests, u.measured_requests), (2, 1));
    assert_eq!(
        (
            u.tokens.input_tokens,
            u.tokens.output_tokens,
            u.tokens.total_tokens
        ),
        (100, 20, 120)
    );
    assert_eq!(u.tokens.cached_tokens, Some(80));
    assert_eq!(u.days.len(), 1);
    assert!(!reopened.logs().unwrap().iter().any(|l| l.id == "measured"));
    assert!(TokenUsage::from_response(&serde_json::json!({"usage":null})).is_none());
    let old_log: RequestLog = serde_json::from_value(serde_json::json!({"id":"old","at":0,"account":null,"operation":"responses","status":"success","elapsedMs":1,"advice":""})).unwrap();
    assert!(old_log.usage.is_none());
}
#[test]
fn usage_preserves_unknown_details_and_bounds_daily_history() {
    use crate::usage::{AccountUsage, TokenUsage};
    let known = TokenUsage::from_response(
        &serde_json::json!({"usage":{"input_tokens":0,"output_tokens":0}}),
    )
    .unwrap();
    assert_eq!(known.cached_tokens, None);
    let mut u = AccountUsage::default();
    for day in 0..10 {
        u.record(day * 86400, Some(&known));
    }
    assert_eq!(u.days.len(), 7);
    assert_eq!(u.days[0].day, 3 * 86400);
    assert_eq!(u.measured_requests, 10);
    assert_eq!(u.tokens.reasoning_tokens, None);
}
#[test]
fn applied_status_detects_configuration_conflicts() {
    let (dir, a) = fixture();
    let path = dir.path().join("config.toml");
    let exe = dir.path().join("pool.exe");
    assert!(!config::is_applied(&a.store, &path).unwrap());
    let p = config::preview(&a.store, &path, &exe, &a.api_key().unwrap()).unwrap();
    config::apply(&a.store, &path, &exe, &a.api_key().unwrap(), &p.fingerprint).unwrap();
    assert!(config::is_applied(&a.store, &path).unwrap());
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        text.replace("supports_websockets = false", "supports_websockets = true"),
    )
    .unwrap();
    assert!(!config::is_applied(&a.store, &path).unwrap());
}
#[cfg(windows)]
#[test]
fn windows_vault_handles_large_tokens_and_replacement() {
    // A failed assertion must not accumulate native credentials across runs.
    // Match only this test's UUID namespace, and inspect target names, never blobs.
    struct FixtureCleanup(String);
    impl Drop for FixtureCleanup {
        fn drop(&mut self) {
            use windows_sys::Win32::{
                Foundation::{GetLastError, ERROR_NOT_FOUND},
                Security::Credentials::*,
            };
            let filter: Vec<u16> = format!("{}*", self.0).encode_utf16().chain(Some(0)).collect();
            let mut count = 0;
            let mut entries = std::ptr::null_mut();
            unsafe {
                if CredEnumerateW(filter.as_ptr(), 0, &mut count, &mut entries) == 0 {
                    let code = GetLastError();
                    if code != ERROR_NOT_FOUND { eprintln!("fixture cleanup enumerate error: {code}"); }
                    return;
                }
                let mut failures = 0usize;
                for entry in std::slice::from_raw_parts(entries, count as usize) {
                    let entry = &**entry;
                    if entry.Type != CRED_TYPE_GENERIC || entry.TargetName.is_null() { continue; }
                    let mut len = 0;
                    while len < 32768 && *entry.TargetName.add(len) != 0 { len += 1; }
                    if len == 32768 { continue; }
                    let Ok(target) = String::from_utf16(std::slice::from_raw_parts(entry.TargetName, len)) else { continue; };
                    let Some(key) = target.strip_suffix(".PersonalCodexPool") else { continue; };
                    let owned = key == self.0 || key.strip_prefix(&format!("{}/", self.0)).is_some_and(|rest| {
                        let Some((generation, part)) = rest.split_once('/') else { return false; };
                        uuid::Uuid::parse_str(generation).is_ok_and(|id| id.to_string() == generation)
                            && part.parse::<usize>().is_ok_and(|index| index < 128 && index.to_string() == part)
                    });
                    if owned && CredDeleteW(entry.TargetName, CRED_TYPE_GENERIC, 0) == 0 && GetLastError() != ERROR_NOT_FOUND { failures += 1; }
                }
                CredFree(entries.cast());
                if failures > 0 { eprintln!("fixture cleanup failed target count: {failures}"); }
            }
        }
    }
    let v = crate::vault::system_vault().unwrap();
    let key = format!("test/{}", uuid::Uuid::new_v4());
    let _cleanup = FixtureCleanup(key.clone());
    let secret = "凭据token".repeat(3000);
    v.set(&key, &secret).unwrap();
    assert!(v.get(&key).unwrap().as_deref() == Some(&secret));
    v.set(&key, "replacement").unwrap();
    assert!(v.get(&key).unwrap().as_deref() == Some("replacement"));
    v.delete(&key).unwrap();
    assert!(v.get(&key).unwrap().is_none());
}

pub(crate) fn recover(store: &Store, path: &std::path::Path) -> Result<()> {
    let exe = path.with_file_name("pool.exe");
    let p = config::recovery_preview(
        Some(store),
        path,
        &exe,
        Some(config::RecoveryStrategy::Normal),
    );
    if p.strategy != config::RecoveryStrategy::Normal {
        return Err("需要重新确认恢复策略".into());
    }
    config::apply_recovery(Some(store), path, &exe, p.strategy, &p.fingerprint)
}
