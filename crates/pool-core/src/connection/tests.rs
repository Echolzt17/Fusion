use super::*;
use crate::tests::{add, fixture};
use axum::{
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio_util::sync::CancellationToken;

async fn setup(
    router: Router,
) -> (
    tempfile::TempDir,
    Arc<Connection>,
    tokio::task::JoinHandle<()>,
) {
    let (d, accounts) = fixture();
    add(&accounts, "a");
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream = format!("http://{}", socket.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(socket, router).await.unwrap();
    });
    let reserved = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut settings = accounts.store.settings().unwrap();
    settings.port = reserved.local_addr().unwrap().port();
    accounts.store.save_settings(&settings).unwrap();
    drop(reserved);
    let gateway = Gateway::new(accounts);
    *gateway.test_upstream.lock().unwrap() = Some(upstream);
    let path = d.path().join("config.toml");
    std::fs::write(
        &path,
        "model='keep-model'\n[features]\nimage_generation=false\nother=true\n",
    )
    .unwrap();
    std::fs::write(
        d.path().join("auth.json"),
        "public-fixture-official-login-do-not-touch",
    )
    .unwrap();
    let connection = Connection::new(gateway, path, d.path().join("pool.exe"));
    (d, connection, server)
}
fn models() -> Router {
    Router::new().route(
        "/models",
        get(|axum::extract::RawQuery(query): axum::extract::RawQuery| async move {
            assert_eq!(query.as_deref(), Some("client_version=0.166.0"));
            Json(json!({"models":[{"slug":"fixture"}]}))
        }),
    )
}
async fn enable(c: &Arc<Connection>) {
    let p = c.enable_preview().await.unwrap();
    c.enable(&p.fingerprint, "0.166.0").await.unwrap();
}
#[tokio::test]
async fn startup_sends_detected_engine_version_before_configuring_and_restart() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let (_d, c, server) = setup(Router::new().route("/models", get(move |axum::extract::RawQuery(query): axum::extract::RawQuery| {
        let count = count.clone();
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            if query.as_deref() != Some("client_version=0.166.0") {
                return (StatusCode::BAD_REQUEST, Json(json!({})))
            }
            (StatusCode::OK, Json(json!({"models":[{"slug":"fixture"}]})))
        }
    }))).await;
    let fingerprint = c.prepare_enable().await.unwrap();
    let client = crate::compatibility::ClientVersion {
        desktop: Some("26.1027.3384.0".into()), engine: Some("codex-cli 0.166.0-alpha.5.1".into()),
    };
    c.enable(&fingerprint, client.catalog_version().unwrap()).await.unwrap();
    assert!(config::is_applied(&c.gateway.accounts.store, &c.path).unwrap());
    assert_eq!(c.status().await.unwrap().reload_required.as_deref(), Some("pool"));
    c.wait_for_restart().await.unwrap();
    c.resume_gateway().await.unwrap();
    c.client_reloaded().await.unwrap();
    assert_eq!(c.status().await.unwrap().mode, "on");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    close(&c).await;
    server.abort();
}
async fn close(c: &Arc<Connection>) {
    c.prepare_close().await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), c.wait_drained())
        .await
        .unwrap()
        .unwrap();
    c.complete_close().await.unwrap();
}
#[tokio::test]
async fn startup_accepts_catalog_after_31_seconds_and_reports_waiting_phase() {
    let calls = Arc::new(AtomicUsize::new(0)); let n = calls.clone();
    let (_d, c, server) = setup(Router::new().route("/models", get(move || {
        let n = n.clone(); async move {
            n.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(31)).await;
            Json(json!({"models":[]}))
        }
    }))).await;
    let start = c.clone(); let task = tokio::spawn(async move { enable(&start).await; });
    while calls.load(Ordering::SeqCst) == 0 { tokio::time::sleep(Duration::from_millis(10)).await; }
    assert_eq!(c.status().await.unwrap().phase, Some("catalog_headers"));
    task.await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(config::is_applied(&c.gateway.accounts.store, &c.path).unwrap());
    close(&c).await; server.abort();
}
#[tokio::test]
async fn aborted_start_cleans_its_service_and_keeps_original_config() {
    let (_d, c, server) = setup(Router::new().route("/models", get(|| async { std::future::pending::<Json<Value>>().await }))).await;
    let before = std::fs::read(&c.path).unwrap(); let start = c.clone();
    let task = tokio::spawn(async move { enable(&start).await; });
    while c.status().await.unwrap().phase != Some("catalog_headers") { tokio::time::sleep(Duration::from_millis(10)).await; }
    task.abort(); let _ = task.await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while c.gateway.status().await.running { tokio::time::sleep(Duration::from_millis(10)).await; }
    }).await.unwrap();
    assert_eq!(std::fs::read(&c.path).unwrap(), before);
    assert!(!config::has_backup(&c.gateway.accounts.store).unwrap());
    assert!(!c.gateway.scheduler.is_active("a")); server.abort();
}

#[tokio::test]
async fn restarted_app_exposes_unfinished_close_even_after_origin_is_restored() {
    let (_d, c, server) = setup(models()).await;
    enable(&c).await;
    c.prepare_close().await.unwrap();
    c.wait_drained().await.unwrap();
    c.gateway.stop().await;
    let restarted = Connection::new(c.gateway.clone(), c.path.clone(), c.exe.clone());
    let status = restarted.status().await.unwrap();
    assert_eq!(status.mode, "recovery_required");
    assert!(status.recovery_pending);
    assert!(restarted.prepare_close().await.unwrap());
    restarted.wait_drained().await.unwrap();
    restarted.complete_close().await.unwrap();
    assert_eq!(restarted.status().await.unwrap().mode, "off");
    server.abort();
}

#[tokio::test]
async fn automatic_close_restores_before_drain_keeps_recovery_until_client_reload() {
    let release = CancellationToken::new();
    let gate = release.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let (d, c, server) = setup(models().route(
        "/responses",
        post(move || {
            let (gate, count) = (gate.clone(), count.clone());
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                gate.cancelled().await;
                (
                    [("content-type", "text/event-stream")],
                    "data: {\"type\":\"response.completed\",\"response\":{\"output\":[]}}\n\n",
                )
            }
        }),
    ))
    .await;
    std::fs::write(&c.path, "model_provider='custom'\n[model_providers.custom]\nbase_url='https://example.invalid/v1'\n").unwrap();
    enable(&c).await;
    let url = format!("{}/responses", c.gateway.status().await.base_url);
    let key = c.gateway.accounts.api_key().unwrap();
    let http = c.gateway.accounts.local_http.clone();
    let request = tokio::spawn(async move {
        http.post(url)
            .bearer_auth(key)
            .json(&json!({"input":"fixture"}))
            .send()
            .await
            .unwrap()
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(c.prepare_close().await.unwrap());
    assert!(c.prepare_close().await.unwrap()); // joins the same transition
    assert!(config::detached(
        &c.path,
        &c.exe,
        c.gateway.accounts.store.settings().unwrap().port
    )
    .unwrap());
    assert!(std::fs::read_to_string(&c.path).unwrap().contains("custom"));
    assert!(config::has_backup(&c.gateway.accounts.store).unwrap());
    assert_eq!(c.status().await.unwrap().phase, Some("draining"));
    assert!(c.gateway.status().await.draining);
    assert!(c.enable_preview().await.is_err());
    release.cancel();
    assert_eq!(request.await.unwrap().status(), StatusCode::OK);
    tokio::time::timeout(Duration::from_secs(3), c.wait_drained())
        .await
        .unwrap()
        .unwrap();
    assert!(config::has_backup(&c.gateway.accounts.store).unwrap());
    c.close_failed("fixture desktop close blocked".into()).await;
    assert!(c.gateway.status().await.running);
    assert!(config::has_backup(&c.gateway.accounts.store).unwrap());
    assert_eq!(c.status().await.unwrap().phase, Some("failed"));
    assert!(c.prepare_close().await.unwrap());
    c.wait_drained().await.unwrap();
    c.complete_close().await.unwrap();
    assert_eq!(c.status().await.unwrap().mode, "off");
    assert!(c.status().await.unwrap().reload_required.is_none());
    assert!(!config::has_backup(&c.gateway.accounts.store).unwrap());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        std::fs::read_to_string(d.path().join("auth.json")).unwrap(),
        "public-fixture-official-login-do-not-touch"
    );
    server.abort();
}

#[tokio::test]
async fn automatic_close_failures_leave_listener_alive_and_never_choose_rescue() {
    for failure in ["corrupt", "missing", "conflict", "locked"] {
        let (_d, c, server) = setup(models()).await;
        enable(&c).await;
        let original = std::fs::read_to_string(&c.path).unwrap();
        let file_lock = if failure == "locked" {
            use std::os::windows::fs::OpenOptionsExt;
            Some(
                std::fs::OpenOptions::new()
                    .read(true)
                    .share_mode(1)
                    .open(&c.path)
                    .unwrap(),
            )
        } else {
            None
        };
        match failure {
            "corrupt" => std::fs::write(&c.path, "[invalid").unwrap(),
            "missing" => c
                .gateway
                .accounts
                .store
                .set("codex_config_backup", "")
                .unwrap(),
            "conflict" => std::fs::write(
                &c.path,
                original.replace("request_max_retries = 0", "request_max_retries = 4"),
            )
            .unwrap(),
            _ => {}
        }
        let before = std::fs::read(&c.path).unwrap();
        assert!(c.prepare_close().await.is_err(), "{failure}");
        assert!(c.gateway.status().await.running);
        assert!(!c.gateway.status().await.draining);
        assert_eq!(std::fs::read(&c.path).unwrap(), before);
        assert!(!config::recovery_transaction_pending(&c.path));
        drop(file_lock);
        c.gateway.stop().await;
        server.abort();
    }
}

#[tokio::test]
async fn automatic_close_noop_preserves_config_and_late_dependency_blocks_completion() {
    let (_d, c, server) = setup(models()).await;
    c.gateway
        .accounts
        .store
        .set("codex_client_reload_required", "original")
        .unwrap();
    let original = std::fs::read(&c.path).unwrap();
    assert!(!c.prepare_close().await.unwrap());
    c.wait_drained().await.unwrap();
    c.complete_close().await.unwrap();
    assert_eq!(std::fs::read(&c.path).unwrap(), original);
    enable(&c).await;
    let applied = std::fs::read(&c.path).unwrap();
    c.client_reloaded().await.unwrap();
    assert!(c.status().await.unwrap().reload_required.is_none());
    assert!(c.prepare_close().await.unwrap());
    c.wait_drained().await.unwrap();
    std::fs::write(&c.path, &applied).unwrap();
    assert!(c.complete_close().await.is_err());
    assert!(config::has_backup(&c.gateway.accounts.store).unwrap());
    c.close_failed("fixture late dependency".into()).await;
    assert!(c.gateway.status().await.running);
    c.gateway.stop().await;
    server.abort();
}

#[tokio::test]
async fn automatic_close_waits_for_image_and_keeps_its_account_binding() {
    let release = CancellationToken::new();
    let gate = release.clone();
    let entered = Arc::new(tokio::sync::Notify::new());
    let notify = entered.clone();
    let (_d, c, server) = setup(models().route(
        "/images/generations",
        post(move || {
            let (gate, notify) = (gate.clone(), notify.clone());
            async move {
                notify.notify_one();
                gate.cancelled().await;
                Json(json!({"data":[{"b64_json":"cHVibGljLWZpeHR1cmU="}]}))
            }
        }),
    ))
    .await;
    enable(&c).await;
    let key = crate::scheduler::binding_key("turn", "fixture-image-turn");
    c.gateway.accounts.store.bind(&key, "a").unwrap();
    let http = c.gateway.accounts.local_http.clone();
    let auth = c.gateway.accounts.api_key().unwrap();
    let url = format!("{}/images/generations", c.gateway.status().await.base_url);
    let request = tokio::spawn(async move {
        http.post(url)
            .bearer_auth(auth)
            .header("x-codex-image-turn-id", "fixture-image-turn")
            .json(&json!({"prompt":"public fixture"}))
            .send()
            .await
            .unwrap()
    });
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    c.prepare_close().await.unwrap();
    assert_eq!(c.gateway.status().await.active_requests, 1);
    assert!(config::has_backup(&c.gateway.accounts.store).unwrap());
    release.cancel();
    assert_eq!(request.await.unwrap().status(), StatusCode::OK);
    c.wait_drained().await.unwrap();
    c.complete_close().await.unwrap();
    assert_eq!(
        c.gateway.accounts.store.binding(&key).unwrap().as_deref(),
        Some("a")
    );
    assert_eq!(
        c.gateway
            .accounts
            .store
            .logs()
            .unwrap()
            .iter()
            .filter(|l| l.operation == "image_generate")
            .count(),
        1
    );
    assert!(!c.gateway.status().await.running);
    server.abort();
}

#[tokio::test]
async fn review_does_not_start_or_write_and_stale_apply_never_starts() {
    let (d, c, server) = setup(models()).await;
    let original = std::fs::read_to_string(&c.path).unwrap();
    let p = c.enable_preview().await.unwrap();
    assert_eq!(c.status().await.unwrap().mode, "off");
    assert!(!c.gateway.status().await.running);
    assert!(!config::has_backup(&c.gateway.accounts.store).unwrap());
    assert_eq!(std::fs::read_to_string(&c.path).unwrap(), original);
    std::fs::write(&c.path, format!("# edited after review\n{original}")).unwrap();
    assert!(c.enable(&p.fingerprint, "0.166.0").await.is_err());
    assert!(!c.gateway.status().await.running);
    enable(&c).await;
    assert_eq!(c.status().await.unwrap().mode, "on");
    close(&c).await;
    let restored = std::fs::read_to_string(&c.path).unwrap();
    assert!(restored.contains("# edited after review"));
    assert!(restored.contains("personal_codex_pool"));
    assert!(restored.contains("keep-model"));
    assert!(restored.contains("other=true"));
    assert_eq!(
        std::fs::read_to_string(d.path().join("auth.json")).unwrap(),
        "public-fixture-official-login-do-not-touch"
    );
    server.abort();
}

#[tokio::test]
async fn failed_models_probe_rolls_back_listener_without_changing_official_config() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let (d, c, server) = setup(Router::new().route(
        "/models",
        get(move || {
            let count = count.clone();
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                StatusCode::BAD_REQUEST
            }
        }),
    ))
    .await;
    std::fs::write(&c.path, "model_provider='custom'\nmodel='keep-model'\n").unwrap();
    let original = std::fs::read_to_string(&c.path).unwrap();
    let p = c.enable_preview().await.unwrap();
    let error = c.enable(&p.fingerprint, "0.166.0").await.unwrap_err();
    assert!(error.contains("HTTP 400") && error.contains("模型目录"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(c.status().await.unwrap().mode, "off");
    assert!(!c.gateway.status().await.running);
    assert!(!config::has_backup(&c.gateway.accounts.store).unwrap());
    assert_eq!(std::fs::read_to_string(&c.path).unwrap(), original);
    assert!(d.path().join("auth.json").exists());
    server.abort();
}

#[tokio::test]
async fn drain_completes_and_restores_without_frontend_polling_or_replay() {
    let release = CancellationToken::new();
    let gate = release.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let (_d, c, server) = setup(models().route("/responses", post(move || {
        let (gate, count) = (gate.clone(), count.clone());
        async move {
            count.fetch_add(1, Ordering::SeqCst); gate.cancelled().await;
            ([("content-type", "text/event-stream")], "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"fixture-response\",\"output\":[]}}\n\n")
        }
    }))).await;
    enable(&c).await;
    c.gateway
        .accounts
        .store
        .bind("session:original-binding", "a")
        .unwrap();
    let url = format!("{}/responses", c.gateway.status().await.base_url);
    let key = c.gateway.accounts.api_key().unwrap();
    let http = c.gateway.accounts.local_http.clone();
    let request = tokio::spawn(async move {
        http.post(url)
            .bearer_auth(key)
            .json(&json!({"input":"public fixture"}))
            .send()
            .await
            .unwrap()
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    c.prepare_close().await.unwrap();
    assert_eq!(c.status().await.unwrap().mode, "closing");
    assert!(!config::is_applied(&c.gateway.accounts.store, &c.path).unwrap());
    assert!(c.enable_preview().await.is_err());
    let owner = c.clone();
    let closing = tokio::spawn(async move {
        owner.wait_drained().await.unwrap();
        owner.complete_close().await.unwrap();
    });
    release.cancel();
    assert_eq!(request.await.unwrap().status(), StatusCode::OK);
    // The backend task completes without any frontend status polling.
    tokio::time::timeout(Duration::from_secs(3), closing)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(c.status().await.unwrap().mode, "off");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        c.gateway
            .accounts
            .store
            .binding("session:original-binding")
            .unwrap()
            .as_deref(),
        Some("a")
    );
    assert!(!c.gateway.status().await.running);
    server.abort();
}

#[tokio::test]
async fn stale_close_does_not_stop_service_and_managed_conflicts_require_recovery() {
    let (_d, c, server) = setup(models()).await;
    enable(&c).await;
    let applied = std::fs::read_to_string(&c.path).unwrap();
    std::fs::write(&c.path, format!("# preserve later edit\n{applied}")).unwrap();
    // An unrelated edit is merged at apply time; a managed-field edit still requires a fresh decision.
    let mut changed = applied.parse::<toml_edit::DocumentMut>().unwrap();
    changed["features"]["image_generation"] = toml_edit::value(false);
    std::fs::write(&c.path, changed.to_string()).unwrap();
    assert!(c.prepare_close().await.is_err());
    assert!(c.gateway.status().await.running);
    assert_eq!(
        c.recovery_preview(None).await.unwrap().strategy,
        config::RecoveryStrategy::Official
    );
    assert_eq!(c.status().await.unwrap().mode, "recovery_required");
    assert!(c.gateway.status().await.running);
    assert!(c.save_service_settings(19000, 2).await.is_err());
    std::fs::write(&c.path, format!("# preserve later edit\n{applied}")).unwrap();
    close(&c).await;
    assert!(std::fs::read_to_string(&c.path)
        .unwrap()
        .contains("# preserve later edit"));
    server.abort();
}

#[tokio::test]
async fn restart_with_leftover_config_requires_explicit_restore_even_without_accounts() {
    let (_d, c, server) = setup(models()).await;
    enable(&c).await;
    c.gateway.stop().await; // simulate previous process ending; no real client involved
    let g = Gateway::new(c.gateway.accounts.clone());
    let restarted = Connection::new(g, c.path.clone(), c.exe.clone());
    assert_eq!(restarted.status().await.unwrap().mode, "recovery_required");
    assert!(config::has_backup(&c.gateway.accounts.store).unwrap());
    c.gateway.remove_account("a").await.unwrap();
    close(&restarted).await;
    assert!(restarted.status().await.unwrap().reload_required.is_none());
    server.abort();
}

#[tokio::test]
async fn custom_and_profile_origins_enable_directly_and_restore_without_copying_credentials() {
    let (d, c, server) = setup(models()).await;
    for content in [
        "model_provider='custom'\nmodel='keep-model'\n",
        "model_provider='custom'\n[model_providers.custom]\nbase_url='https://fixture.invalid/v1'\nsecret='public-fixture-sensitive'\n",
        "model_provider='custom'\nprofile='work'\n[profiles.work]\nmodel='keep-model'\nsecret='public-fixture-sensitive'\n",
        "model_provider='openai'\nprofile='work'\n[profiles.work]\nmodel_provider='custom'\nmodel='keep-model'\nsecret='public-fixture-sensitive'\n[profiles.unused]\nmodel_provider='other'\n",
        "profile='work'\n[profiles.work]\nmodel_provider='openai'\nother=true\n",
    ] {
        std::fs::write(&c.path, content).unwrap();
        let p = c.enable_preview().await.unwrap();
        assert!(!c.gateway.status().await.running);
        assert_eq!(std::fs::read_to_string(&c.path).unwrap(), content);
        assert!(!serde_json::to_string(&p).unwrap().contains("public-fixture-sensitive"));
        c.enable(&p.fingerprint, "0.166.0").await.unwrap();
        assert_eq!(c.status().await.unwrap().mode, "on");
        let applied: serde_json::Value = toml_edit::de::from_str(&std::fs::read_to_string(&c.path).unwrap()).unwrap();
        assert_eq!(applied["model_provider"], "personal_codex_pool");
        assert!(applied["profiles"]["work"]["model_provider"].is_null());
        assert!(!c.gateway.accounts.store.get("codex_config_backup").unwrap().unwrap().contains("public-fixture-sensitive"));
        let p = c.recovery_preview(None).await.unwrap();
        assert_eq!(p.strategy, config::RecoveryStrategy::Normal);
        close(&c).await;
        assert_eq!(c.status().await.unwrap().mode, "off");
        let mut restored: serde_json::Value = toml_edit::de::from_str(&std::fs::read_to_string(&c.path).unwrap()).unwrap();
        assert_eq!(restored["model_providers"]["personal_codex_pool"], applied["model_providers"]["personal_codex_pool"]);
        restored["model_providers"].as_object_mut().unwrap().remove("personal_codex_pool");
        let original: serde_json::Value = toml_edit::de::from_str(content).unwrap();
        for (key, value) in original.as_object().unwrap() {
            assert_eq!(&restored[key], value);
        }
        assert!(restored["model_providers"]["personal_codex_pool"].is_null());
        assert!(restored["features"]["image_generation"].is_null());
        assert!(!config::has_backup(&c.gateway.accounts.store).unwrap());
    }
    assert_eq!(
        std::fs::read_to_string(d.path().join("auth.json")).unwrap(),
        "public-fixture-official-login-do-not-touch"
    );
    server.abort();
}

#[tokio::test]
async fn active_profile_change_invalidates_review_and_enabled_routing() {
    let (_d, c, server) = setup(models()).await;
    let original = "profile='work'\n[profiles.work]\nmodel_provider='custom'\n[profiles.other]\nmodel_provider='openai'\n";
    std::fs::write(&c.path, original).unwrap();
    let p = c.enable_preview().await.unwrap();
    std::fs::write(
        &c.path,
        original.replace("profile='work'", "profile='other'"),
    )
    .unwrap();
    assert!(c.enable(&p.fingerprint, "0.166.0").await.is_err());
    assert!(!c.gateway.status().await.running);
    std::fs::write(&c.path, original).unwrap();
    enable(&c).await;
    let applied = std::fs::read_to_string(&c.path).unwrap();
    std::fs::write(
        &c.path,
        applied.replace("profile='work'", "profile='other'"),
    )
    .unwrap();
    assert_eq!(c.status().await.unwrap().mode, "recovery_required");
    assert!(c.enable_preview().await.is_err());
    std::fs::write(&c.path, applied).unwrap();
    close(&c).await;
    server.abort();
}

#[tokio::test]
async fn orphaned_pool_config_still_requires_recovery_before_enable() {
    let (_d, c, server) = setup(models()).await;
    std::fs::write(&c.path, "model_provider='personal_codex_pool'").unwrap();
    assert!(c.enable_preview().await.is_err());
    assert_eq!(c.status().await.unwrap().mode, "recovery_required");
    assert_eq!(
        c.recovery_preview(None).await.unwrap().strategy,
        config::RecoveryStrategy::Official
    );
    assert_eq!(
        std::fs::read_to_string(&c.path).unwrap(),
        "model_provider='personal_codex_pool'"
    );
    server.abort();
}

#[tokio::test]
async fn corrupt_configuration_blocks_automatic_close_but_allows_explicit_rescue() {
    let (_d, c, server) = setup(models()).await;
    enable(&c).await;
    std::fs::write(&c.path, "broken = [").unwrap();
    assert_eq!(c.status().await.unwrap().mode, "recovery_required");
    let p = c.recovery_preview(None).await.unwrap();
    assert_eq!(p.strategy, config::RecoveryStrategy::Rescue);
    assert!(c.prepare_close().await.is_err());
    assert!(c.gateway.status().await.running);
    assert_eq!(std::fs::read_to_string(&c.path).unwrap(), "broken = [");
    config::apply_recovery(
        Some(&c.gateway.accounts.store),
        &c.path,
        &c.exe,
        p.strategy,
        &p.fingerprint,
    )
    .unwrap();
    c.gateway.stop().await;
    server.abort();
}

#[tokio::test]
async fn interrupted_rescue_is_visible_even_when_missing_config_defaults_to_official() {
    let (_d, c, server) = setup(models()).await;
    let archive = c
        .path
        .with_file_name("config.fusion-recovery-public-fixture.toml");
    std::fs::rename(&c.path, &archive).unwrap();
    std::fs::write(c.path.with_file_name("personal-codex-pool-recovery.json"),serde_json::json!({"version":1,"archive":"config.fusion-recovery-public-fixture.toml","source_hash":"0".repeat(64)}).to_string()).unwrap();
    let status = c.status().await.unwrap();
    assert_eq!(status.mode, "recovery_required");
    assert!(status.recovery_pending);
    let p = c.recovery_preview(None).await.unwrap();
    assert_eq!(p.strategy, config::RecoveryStrategy::Rescue);
    config::apply_recovery(
        Some(&c.gateway.accounts.store),
        &c.path,
        &c.exe,
        p.strategy,
        &p.fingerprint,
    )
    .unwrap();
    c.gateway.stop().await;
    assert!(archive.exists());
    server.abort();
}

#[tokio::test]
async fn slow_enable_keeps_status_responsive_and_rechecks_after_display_cache() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = CancellationToken::new();
    let (e, r) = (entered.clone(), release.clone());
    let (_d, c, server) = setup(Router::new().route(
        "/models",
        get(move || {
            let (e, r) = (e.clone(), r.clone());
            async move {
                e.notify_one();
                r.cancelled().await;
                Json(json!({"models":[]}))
            }
        }),
    ))
    .await;
    assert_eq!(c.status().await.unwrap().mode, "off");
    let at = c.inspection.lock().await.as_ref().unwrap().0;
    for _ in 0..5 {
        c.status().await.unwrap();
    }
    assert_eq!(at, c.inspection.lock().await.as_ref().unwrap().0);
    let p = c.enable_preview().await.unwrap();
    let owner = c.clone();
    let enabling = tokio::spawn(async move { owner.enable(&p.fingerprint, "0.166.0").await });
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_millis(300), c.status())
            .await
            .unwrap()
            .unwrap()
            .mode,
        "enabling"
    );
    release.cancel();
    enabling.await.unwrap().unwrap();
    assert_eq!(c.status().await.unwrap().mode, "on");
    let applied = std::fs::read_to_string(&c.path).unwrap();
    std::fs::write(
        &c.path,
        applied.replace("personal_codex_pool", "external_change"),
    )
    .unwrap();
    // Displays may be cached; confirmation and mutations MUST see the live file.
    assert_eq!(c.status().await.unwrap().mode, "on");
    assert!(c.enable_preview().await.is_err());
    c.inspection.lock().await.as_mut().unwrap().0 = Instant::now() - Duration::from_secs(3);
    assert_eq!(c.status().await.unwrap().mode, "recovery_required");
    std::fs::write(&c.path, applied).unwrap();
    close(&c).await;
    server.abort();
}

#[tokio::test]
async fn one_click_start_stays_pending_until_client_verified_and_retry_keeps_original_login() {
    let (d, c, server) = setup(models()).await;
    let original = std::fs::read_to_string(&c.path).unwrap();
    assert!(c.begin_start().unwrap());
    assert!(!c.begin_start().unwrap());
    assert_eq!(c.status().await.unwrap().operation, Some("start"));
    let fingerprint = c.prepare_enable().await.unwrap();
    c.set_phase("configuring");
    c.enable(&fingerprint, "0.166.0").await.unwrap();
    assert_eq!(c.status().await.unwrap().mode, "enabling");
    c.gateway.accounts.store.set("codex_restart_context", "public-fixture-restart-record").unwrap();
    c.wait_for_restart().await.unwrap();
    c.start_failed("fixture: client failed to launch".into()).await;
    assert_eq!(c.status().await.unwrap().phase, Some("failed"));
    assert_eq!(c.status().await.unwrap().mode, "recovery_required");
    assert!(config::has_backup(&c.gateway.accounts.store).unwrap());
    assert_eq!(c.gateway.accounts.store.get("codex_restart_context").unwrap().as_deref(), Some("public-fixture-restart-record"));
    assert!(c.begin_start().unwrap());
    let fingerprint = c.prepare_enable().await.unwrap();
    c.enable(&fingerprint, "0.166.0").await.unwrap();
    c.wait_for_restart().await.unwrap();
    c.resume_gateway().await.unwrap();
    c.set_phase("verifying");
    assert_eq!(c.status().await.unwrap().mode, "enabling");
    c.client_reloaded().await.unwrap();
    assert_eq!(c.status().await.unwrap().mode, "on");
    assert_eq!(c.gateway.accounts.store.get("codex_restart_context").unwrap().as_deref(), Some(""));
    close(&c).await;
    let restored: toml_edit::DocumentMut = std::fs::read_to_string(&c.path).unwrap().parse().unwrap();
    let before: toml_edit::DocumentMut = original.parse().unwrap();
    assert_eq!(restored["model"].as_str(), before["model"].as_str());
    assert!(restored.get("model_provider").is_none());
    assert_eq!(std::fs::read_to_string(d.path().join("auth.json")).unwrap(), "public-fixture-official-login-do-not-touch");
    server.abort();
}

#[tokio::test]
async fn restart_drain_waits_for_inflight_request_without_replay_or_exit_timer() {
    let release = CancellationToken::new();
    let entered = Arc::new(tokio::sync::Notify::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let (gate, signal, count) = (release.clone(), entered.clone(), calls.clone());
    let (_d, c, server) = setup(models().route("/responses", post(move || {
        let (gate, signal, count) = (gate.clone(), signal.clone(), count.clone());
        async move {
            count.fetch_add(1, Ordering::SeqCst); signal.notify_one(); gate.cancelled().await;
            ([("content-type", "text/event-stream")], "data: {\"type\":\"response.completed\",\"response\":{\"output\":[]}}\n\n")
        }
    }))).await;
    enable(&c).await;
    let (http, key, url) = (c.gateway.accounts.local_http.clone(), c.gateway.accounts.api_key().unwrap(), format!("{}/responses", c.gateway.status().await.base_url));
    let request = tokio::spawn(async move { http.post(url).bearer_auth(key).json(&json!({"input":"public fixture"})).send().await.unwrap().bytes().await.unwrap() });
    tokio::time::timeout(Duration::from_secs(2), entered.notified()).await.unwrap();
    c.begin_start().unwrap();
    let owner = c.clone();
    let waiting = tokio::spawn(async move { owner.wait_for_restart().await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!waiting.is_finished());
    assert_eq!(c.status().await.unwrap().phase, Some("draining"));
    release.cancel(); request.await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), waiting).await.unwrap().unwrap().unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    c.start_failed("fixture stopped before desktop manipulation".into()).await;
    close(&c).await;
    server.abort();
}
