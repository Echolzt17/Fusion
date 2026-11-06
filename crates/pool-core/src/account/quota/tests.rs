use super::*;
use crate::tests::{add, fixture};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::get,
    Json,
};
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn ten_minute_due_boundary_and_clock_rollback() {
    assert!(!due(Some(1000), 1599));
    assert!(due(Some(1000), 1600));
    assert!(due(None, 1000));
    assert!(due(Some(1001), 1000));
}

#[tokio::test]
async fn quota_scan_discovers_accounts_skips_fresh_and_manual_resets_due_time() {
    let count = Arc::new(AtomicUsize::new(0));
    let handler_count = count.clone();
    let server = axum::Router::new().route(
        "/usage",
        get(move || {
            let count = handler_count.clone();
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                Json(
                    json!({"plan_type":"plus","rate_limit":{"primary_window":{"used_percent":25}}}),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (_dir, mut accounts) = fixture();
    Arc::get_mut(&mut accounts).unwrap().quota_endpoint =
        format!("http://{}/usage", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, server).await.unwrap() });
    add(&accounts, "a");
    let mut account = accounts.store.account("a").unwrap();
    account.quota = Some(json!({"updatedAt":now(),"rateLimit":{}}));
    accounts.store.save_account(&account).unwrap();
    accounts.refresh_due_quotas().await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 0);
    account.quota.as_mut().unwrap()["updatedAt"] = json!(now() - 600);
    accounts.store.save_account(&account).unwrap();
    accounts.refresh_due_quotas().await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
    accounts.quota("a").await.unwrap(); // Manual refresh bypasses the interval.
    accounts.refresh_due_quotas().await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 2);
    add(&accounts, "new");
    for id in ["reauth", "deleting"] {
        add(&accounts, id);
        let mut account = accounts.store.account(id).unwrap();
        account.state = id.into();
        accounts.store.save_account(&account).unwrap();
    }
    accounts.refresh_due_quotas().await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 3);
    assert!(accounts.store.account("new").unwrap().quota.is_some());
    server.abort();
}

#[tokio::test]
async fn failed_quota_preserves_snapshot_and_waits_next_interval() {
    let calls = Arc::new(AtomicUsize::new(0));
    let handler_calls = calls.clone();
    let server = axum::Router::new().route(
        "/usage",
        get(move || {
            handler_calls.fetch_add(1, Ordering::SeqCst);
            async {
                (
                    StatusCode::BAD_GATEWAY,
                    "private response must not be stored",
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (_dir, mut accounts) = fixture();
    Arc::get_mut(&mut accounts).unwrap().quota_endpoint =
        format!("http://{}/usage", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, server).await.unwrap() });
    add(&accounts, "a");
    let mut account = accounts.store.account("a").unwrap();
    account.quota =
        Some(json!({"updatedAt":now()-1000,"rateLimit":{"primary_window":{"used_percent":13}},"subscription":{"expiresAt":1793750400,"checkedAt":1793678400}}));
    let original = account.quota.clone();
    accounts.store.save_account(&account).unwrap();
    assert!(accounts.quota("a").await.is_err());
    let failed = accounts.store.account("a").unwrap();
    assert_eq!(failed.quota, original);
    assert!(!failed.quota_error.unwrap().contains("private response"));
    accounts.refresh_due_quotas().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(!accounts.quota_refreshing("a"));
    // Authorization failures before HTTP also preserve the snapshot and expose an error.
    accounts.vault.delete("account/a").unwrap();
    assert!(accounts.quota("a").await.is_err());
    assert_eq!(accounts.store.account("a").unwrap().quota, original);
    assert!(accounts.store.account("a").unwrap().quota_error.is_some());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn quota_persists_only_matching_subscription_metadata_and_clears_missing_claims() {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    let app = axum::Router::new().route("/usage", get(|headers: HeaderMap| async move {
        assert_eq!(headers["chatgpt-account-id"], "chatgpt-a");
        assert_eq!(headers["authorization"], "Bearer fake-access-token");
        Json(json!({"plan_type":"plus","rate_limit":{"primary_window":{"used_percent":25}}}))
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (dir, mut accounts) = fixture();
    Arc::get_mut(&mut accounts).unwrap().quota_endpoint = format!("http://{}/usage", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    add(&accounts, "a");
    let raw = accounts.vault.get("account/a").unwrap().unwrap();
    let mut tokens: Tokens = serde_json::from_str(&raw).unwrap();
    let payload = json!({"https://api.openai.com/auth":{"chatgpt_account_id":"chatgpt-a","chatgpt_plan_type":"plus","chatgpt_subscription_active_until":"2026-11-04T00:00:00Z","chatgpt_subscription_last_checked":"2026-11-03T04:00:00Z","private":"discard"}});
    tokens.id_token = format!("fixture.{}.signature", URL_SAFE_NO_PAD.encode(payload.to_string()));
    accounts.vault.set("account/a", &serde_json::to_string(&tokens).unwrap()).unwrap();
    accounts.store.bind("session:existing", "a").unwrap();
    accounts.quota("a").await.unwrap();
    let reopened = Store::open(&dir.path().join("pool.sqlite")).unwrap();
    let quota = reopened.account("a").unwrap().quota.unwrap();
    assert_eq!(quota["subscription"], json!({"expiresAt":1793750400,"checkedAt":1793678400}));
    let saved = quota.to_string();
    for secret in ["fake-access-token", "signature", "private", "discard", "https://api.openai.com/auth"] {
        assert!(!saved.contains(secret));
    }
    tokens.id_token = "fixture-without-metadata".into();
    accounts.vault.set("account/a", &serde_json::to_string(&tokens).unwrap()).unwrap();
    accounts.quota("a").await.unwrap();
    assert!(accounts.store.account("a").unwrap().quota.unwrap()["subscription"].is_null());
    assert_eq!(accounts.store.binding("session:existing").unwrap().as_deref(), Some("a"));
    assert_eq!(accounts.store.account("a").unwrap().state, "ready");
    server.abort();
}

#[derive(Clone)]
struct Gate {
    permits: Arc<tokio::sync::Semaphore>,
    arrived: tokio::sync::mpsc::UnboundedSender<String>,
    active: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    calls: Arc<AtomicUsize>,
}
async fn gated(State(gate): State<Gate>, headers: HeaderMap) -> Json<Value> {
    let active = gate.active.fetch_add(1, Ordering::SeqCst) + 1;
    gate.peak.fetch_max(active, Ordering::SeqCst);
    gate.calls.fetch_add(1, Ordering::SeqCst);
    assert!(active <= 2);
    gate.arrived
        .send(headers["chatgpt-account-id"].to_str().unwrap().to_owned())
        .unwrap();
    gate.permits.acquire().await.unwrap().forget();
    gate.active.fetch_sub(1, Ordering::SeqCst);
    Json(json!({"rate_limit":{"primary_window":{"used_percent":42}}}))
}
async fn setup_gate() -> (
    tempfile::TempDir,
    Arc<Accounts>,
    Gate,
    tokio::sync::mpsc::UnboundedReceiver<String>,
    tokio::task::JoinHandle<()>,
) {
    let (arrived, receiver) = tokio::sync::mpsc::unbounded_channel();
    let gate = Gate {
        permits: Arc::new(tokio::sync::Semaphore::new(0)),
        arrived,
        active: Arc::new(AtomicUsize::new(0)),
        peak: Arc::new(AtomicUsize::new(0)),
        calls: Arc::new(AtomicUsize::new(0)),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (dir, mut accounts) = fixture();
    Arc::get_mut(&mut accounts).unwrap().quota_endpoint =
        format!("http://{}/usage", listener.local_addr().unwrap());
    let router = axum::Router::new()
        .route("/usage", get(gated))
        .with_state(gate.clone());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (dir, accounts, gate, receiver, server)
}

#[tokio::test]
async fn quota_single_flight_limits_two_and_survives_reader_cancellation_without_a_window() {
    let (_dir, accounts, gate, mut arrived, server) = setup_gate().await;
    for id in ["a", "b", "c", "d"] {
        add(&accounts, id);
    }
    let background = {
        let accounts = accounts.clone();
        tokio::spawn(async move { accounts.refresh_due_quotas().await })
    };
    let first = tokio::time::timeout(Duration::from_secs(3), arrived.recv())
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), arrived.recv())
        .await
        .unwrap()
        .unwrap();
    let id = first.strip_prefix("chatgpt-").unwrap().to_owned();
    let manual = {
        let accounts = accounts.clone();
        let id = id.clone();
        tokio::spawn(async move { accounts.quota(&id).await })
    };
    assert!(accounts.quota_refreshing(&id));
    assert!(
        tokio::time::timeout(Duration::from_millis(50), arrived.recv())
            .await
            .is_err()
    );
    background.abort(); // Independent tasks still persist their results.
    gate.permits.add_permits(4);
    manual.await.unwrap().unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while ["a", "b", "c", "d"]
            .iter()
            .any(|id| accounts.quota_refreshing(id))
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(gate.calls.load(Ordering::SeqCst), 4);
    assert_eq!(gate.peak.load(Ordering::SeqCst), 2);
    accounts.refresh_due_quotas().await.unwrap();
    assert_eq!(gate.calls.load(Ordering::SeqCst), 4);
    for account in accounts.store.accounts().unwrap() {
        assert!(account.quota.is_some());
    }
    server.abort();
}

#[tokio::test]
async fn quota_result_cannot_resurrect_a_removed_account() {
    let (_dir, accounts, gate, mut arrived, server) = setup_gate().await;
    add(&accounts, "a");
    let reader = {
        let accounts = accounts.clone();
        tokio::spawn(async move { accounts.quota("a").await })
    };
    tokio::time::timeout(Duration::from_secs(3), arrived.recv())
        .await
        .unwrap()
        .unwrap();
    accounts.remove("a").await.unwrap();
    gate.permits.add_permits(1);
    assert!(reader.await.unwrap().is_err());
    assert!(accounts.store.account("a").is_err());
    assert!(!accounts.quota_refreshing("a"));
    server.abort();
}

#[tokio::test]
async fn removed_then_readded_account_keeps_its_new_refresh_generation() {
    let (_dir, accounts, gate, mut arrived, server) = setup_gate().await;
    add(&accounts, "a");
    let old = {
        let accounts = accounts.clone();
        tokio::spawn(async move { accounts.quota("a").await })
    };
    tokio::time::timeout(Duration::from_secs(3), arrived.recv())
        .await
        .unwrap()
        .unwrap();
    accounts.remove("a").await.unwrap();
    add(&accounts, "a");
    let new = {
        let accounts = accounts.clone();
        tokio::spawn(async move { accounts.quota("a").await })
    };
    tokio::time::timeout(Duration::from_secs(3), arrived.recv())
        .await
        .unwrap()
        .unwrap();
    gate.permits.add_permits(1);
    assert!(old.await.unwrap().is_err());
    assert!(accounts.quota_refreshing("a"));
    assert!(accounts.store.account("a").unwrap().quota.is_none());
    gate.permits.add_permits(1);
    new.await.unwrap().unwrap();
    assert!(!accounts.quota_refreshing("a"));
    assert!(accounts.store.account("a").unwrap().quota.is_some());
    server.abort();
}
