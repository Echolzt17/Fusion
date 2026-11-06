use super::*;
#[derive(Default)]
struct NoCredentials { preparing_gateway: AtomicBool }
impl pool_core::vault::Vault for NoCredentials {
    fn get(&self, key: &str) -> Result<Option<String>> {
        if self.preparing_gateway.load(Ordering::SeqCst) && key == "gateway-key" {
            return Ok(Some("fusion-public-fixture-key".into()));
        }
        panic!("update close must not read credentials")
    }
    fn set(&self, _: &str, _: &str) -> Result<()> { panic!("update close must not write credentials") }
    fn delete(&self, _: &str) -> Result<()> { panic!("update close must not delete credentials") }
}
impl NoCredentials {
    async fn fail_start(&self, connection: &pool_core::connection::Connection, error: &str) {
        self.preparing_gateway.store(true, Ordering::SeqCst);
        connection.start_failed(error.into()).await;
        self.preparing_gateway.store(false, Ordering::SeqCst);
    }
    async fn fail_close(&self, connection: &pool_core::connection::Connection, error: &str) {
        self.preparing_gateway.store(true, Ordering::SeqCst);
        connection.close_failed(error.into()).await;
        self.preparing_gateway.store(false, Ordering::SeqCst);
    }
}
fn connection_fixture() -> (tempfile::TempDir, Arc<pool_core::storage::Store>, Arc<pool_core::connection::Connection>, PathBuf, Arc<NoCredentials>) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    let exe = root.path().join("pool.exe");
    std::fs::write(&path, "model='original'\n").unwrap();
    let store = Arc::new(pool_core::storage::Store::open(&root.path().join("pool.sqlite")).unwrap());
    let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut settings = store.settings().unwrap();
    settings.port = reserved.local_addr().unwrap().port();
    store.save_settings(&settings).unwrap();
    let vault = Arc::new(NoCredentials::default());
    let accounts = Arc::new(pool_core::account::Accounts::new(store.clone(), vault.clone()).unwrap());
    let connection = pool_core::connection::Connection::new(pool_core::gateway::Gateway::new(accounts), path.clone(), exe.clone());
    let preview = config::preview(&store, &path, &exe, "fusion-public-fixture-key").unwrap();
    config::apply(&store, &path, &exe, "fusion-public-fixture-key", &preview.fingerprint).unwrap();
    (root, store, connection, path, vault)
}
#[tokio::test]
async fn detached_update_preserves_independent_codex_and_original_configuration() {
    let (_root, store, connection, path, _vault) = connection_fixture();
    close_connection(&connection, || Ok(())).await.unwrap();
    assert_eq!(connection.status().await.unwrap().mode, "off");
    // A stale reload hint alone does not authorize touching an independent
    // desktop. Neither client discovery nor credentials may be accessed.
    store.set("codex_client_reload_required", "original").unwrap();
    let before = std::fs::read(&path).unwrap();
    close_for_update(&connection, || panic!("detached update must not inspect, quit, or activate Codex")).await.unwrap();
    assert_eq!(std::fs::read(path).unwrap(), before);
    assert_eq!(store.get("codex_client_reload_required").unwrap().as_deref(), Some("original"));
    assert_eq!(connection.status().await.unwrap().mode, "off");
    assert!(!config::has_backup(&store).unwrap());
}
#[tokio::test]
async fn enabled_update_stops_client_once_and_leaves_pool_off_after_installation() {
    struct PublicGatewayKey;
    impl pool_core::vault::Vault for PublicGatewayKey {
        fn get(&self, key: &str) -> Result<Option<String>> {
            assert_eq!(key, "gateway-key");
            Ok(Some("fusion-public-fixture-key".into()))
        }
        fn set(&self, _: &str, _: &str) -> Result<()> { panic!("no credential writes") }
        fn delete(&self, _: &str) -> Result<()> { panic!("no credential deletes") }
    }
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    let exe = root.path().join("pool.exe");
    let store = Arc::new(pool_core::storage::Store::open(&root.path().join("pool.sqlite")).unwrap());
    let reserved = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut settings = store.settings().unwrap();
    settings.port = reserved.local_addr().unwrap().port();
    store.save_settings(&settings).unwrap();
    drop(reserved);
    std::fs::write(&path, "model='original'\n").unwrap();
    let accounts = Arc::new(pool_core::account::Accounts::new(store.clone(), Arc::new(PublicGatewayKey)).unwrap());
    let gateway = pool_core::gateway::Gateway::new(accounts);
    let connection = pool_core::connection::Connection::new(gateway.clone(), path.clone(), exe.clone());
    let preview = config::preview(&store, &path, &exe, "fusion-public-fixture-key").unwrap();
    config::apply(&store, &path, &exe, "fusion-public-fixture-key", &preview.fingerprint).unwrap();
    gateway.start().await.unwrap();
    assert_eq!(connection.status().await.unwrap().mode, "on");
    let pending = store.clone();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let stops = calls.clone();
    close_for_update(&connection, move || {
        stops.fetch_add(1, Ordering::SeqCst);
        assert!(config::has_backup(&pending)?);
        Ok(())
    }).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(connection.status().await.unwrap().mode, "off");
    assert!(!gateway.status().await.running);
    assert!(!config::has_backup(&store).unwrap());
    let update_root = root.path().join("updates");
    std::fs::create_dir(&update_root).unwrap();
    let updates = Updates::new(update_root);
    let resume = Resume {
        from: "1.0.6".into(), to: env!("CARGO_PKG_VERSION").into(),
        enabled: true, claimed: false,
        directory: std::env::current_exe().unwrap().parent().unwrap().into(),
    };
    updates.install_with_resume(&resume, || Ok(())).unwrap();
    updates.finish_update_record().unwrap();
    // A new Fusion process must also remain off until the user starts it.
    let reopened = pool_core::connection::Connection::new(gateway.clone(), path, exe);
    assert_eq!(reopened.status().await.unwrap().mode, "off");
    assert!(!gateway.status().await.running);
}
#[tokio::test]
async fn update_finishes_interrupted_close_without_reenabling_pool() {
    let (_root, store, connection, path, _vault) = connection_fixture();
    store.set("codex_restart_context", "public-fixture-pending").unwrap();
    let error = close_connection(&connection, || Err("fixture: client path unavailable".into())).await.unwrap_err();
    connection.close_failed(error).await;
    assert_eq!(connection.status().await.unwrap().mode, "recovery_required");
    let pending = store.clone();
    close_for_update(&connection, move || {
        assert!(config::has_backup(&pending)?);
        assert_eq!(pending.get("codex_restart_context")?.as_deref(), Some("public-fixture-pending"));
        Ok(())
    }).await.unwrap();
    assert_eq!(connection.status().await.unwrap().mode, "off");
    assert!(!config::has_backup(&store).unwrap());
    assert_eq!(store.get("codex_restart_context").unwrap().as_deref(), Some(""));
    assert!(std::fs::read_to_string(path).unwrap().starts_with("model='original'\n"));
    close_for_update(&connection, || panic!("off pool must not inspect or stop independent Codex")).await.unwrap();
}
#[tokio::test]
async fn update_recovers_failed_start_without_preserving_enable_intent() {
    let (_root, store, connection, _path, vault) = connection_fixture();
    store.set("codex_client_reload_required", "pool").unwrap();
    connection.begin_start().unwrap();
    vault.fail_start(&connection, "fixture: launch failed").await;
    assert_eq!(connection.status().await.unwrap().mode, "recovery_required");
    close_for_update(&connection, || Ok(())).await.unwrap();
    assert_eq!(connection.status().await.unwrap().mode, "off");
    assert!(!config::has_backup(&store).unwrap());
}
#[tokio::test]
async fn update_preserves_recovery_and_exposes_actual_close_failure_until_retry() {
    let (_root, store, connection, path, _vault) = connection_fixture();
    let error = close_for_update(&connection, || Err("fixture: client path unavailable".into())).await.unwrap_err();
    assert_eq!(error, "更新前恢复原接入未完成：fixture: client path unavailable");
    assert!(config::has_backup(&store).unwrap());
    assert_eq!(store.get("codex_client_reload_required").unwrap().as_deref(), Some("original"));
    assert_eq!(connection.status().await.unwrap().mode, "recovery_required");
    assert!(std::fs::read_to_string(path).unwrap().starts_with("model='original'\n"));
    close_for_update(&connection, || Ok(())).await.unwrap();
    assert_eq!(connection.status().await.unwrap().mode, "off");
}
#[tokio::test]
async fn update_does_not_bypass_conflicts_rescue_or_active_switches() {
    let (_root, store, connection, path, vault) = connection_fixture();
    connection.begin_start().unwrap();
    assert!(close_for_update(&connection, || panic!("active start must not close")).await.unwrap_err().contains("正在切换"));
    vault.fail_start(&connection, "fixture").await;
    let applied = std::fs::read_to_string(&path).unwrap();
    // Changed managed fields cannot be silently replaced with official access.
    let conflict = applied.replace("image_generation = true", "image_generation = false");
    assert_ne!(conflict, applied);
    std::fs::write(&path, &conflict).unwrap();
    let error = close_for_update(&connection, || panic!("conflict must not reload")).await.unwrap_err();
    assert!(error.contains("接入字段已被修改"), "{error}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), conflict);
    assert!(config::has_backup(&store).unwrap());
    std::fs::write(&path, applied).unwrap();
    assert!(connection.prepare_close().await.unwrap());
    assert!(close_for_update(&connection, || panic!("active close must not repeat")).await.unwrap_err().contains("正在切换"));
    vault.fail_close(&connection, "fixture").await;
    // A rescue journal always requires the separately confirmed recovery flow.
    let restored = std::fs::read_to_string(&path).unwrap();
    let journal = path.with_file_name("personal-codex-pool-recovery.json");
    std::fs::write(&journal, "public-fixture-invalid-journal").unwrap();
    let error = close_for_update(&connection, || panic!("rescue must require confirmation")).await.unwrap_err();
    assert!(error.contains("配置救援尚未完成"), "{error}");
    assert!(config::has_backup(&store).unwrap());
    assert_eq!(std::fs::read_to_string(path).unwrap(), restored);
    assert_eq!(std::fs::read_to_string(journal).unwrap(), "public-fixture-invalid-journal");
}
fn manifest(version: &str) -> Value {
    serde_json::json!({"version":version,"notes":"Public fixture", "platforms":{"windows-x86_64":{
        "url":format!("{REPOSITORY}/releases/download/v{version}/Fusion_{version}_x64-setup.exe"),
        "signature":include_str!("fixtures/payload.sig").trim()}}})
}
#[test]
fn only_versioned_fusion_assets_and_signed_versions_are_accepted() {
    let valid = manifest("99.0.0");
    assert!(validate_manifest(&valid).is_ok());
    let key = include_str!("fixtures/public.key").trim();
    let payload = include_bytes!("fixtures/payload");
    verify_signed_bytes(key, &valid, payload).unwrap();
    assert!(verify_signed_bytes(key, &valid, b"tampered").is_err());
    assert!(verify_signed_bytes(key, &manifest("100.0.0"), payload).is_err());
    for url in [
        "http://github.com/Echolzt17/Fusion/a.exe",
        "https://github.com/other/Fusion/a.exe",
        "https://example.invalid/a.exe",
    ] {
        let mut invalid = valid.clone();
        invalid["platforms"]["windows-x86_64"]["url"] = url.into();
        assert!(validate_manifest(&invalid).is_err());
    }
    for version in ["../escape", "v99.0.0", "99.0.0/escape", ""] {
        assert!(validate_manifest(&manifest(version)).is_err());
    }
}
#[test]
fn official_updater_rejects_tampered_or_mislabelled_cached_packages() {
    let app = tauri::test::mock_builder()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .build(tauri::generate_context!())
        .unwrap();
    let updater = app
        .updater_builder()
        .pubkey(include_str!("fixtures/public.key").trim())
        .version_comparator(|_, _| true)
        .build()
        .unwrap();
    let update = updater.restore_update(manifest("99.0.0")).unwrap().unwrap();
    update.verify(include_bytes!("fixtures/payload")).unwrap();
    assert!(update.verify(b"changed").is_err());
    let changed = updater
        .restore_update(manifest("100.0.0"))
        .unwrap()
        .unwrap();
    assert!(changed.verify(include_bytes!("fixtures/payload")).is_err());
}
#[test]
fn update_completion_ignores_legacy_enable_intent_and_only_retires_matching_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let updates = Updates::new(temp.path().to_path_buf());
    updates.finish_update_record().unwrap();
    let resume = Resume {
        from: "0.1.29".into(),
        to: env!("CARGO_PKG_VERSION").into(),
        enabled: true,
        claimed: false,
        directory: std::env::current_exe().unwrap().parent().unwrap().into(),
    };
    for enabled in [true, false] {
        for claimed in [true, false] {
            let legacy = Resume { enabled, claimed, ..resume.clone() };
            atomic_json(&temp.path().join("resume.json"), &legacy).unwrap();
            updates.finish_update_record().unwrap();
            assert!(!temp.path().join("resume.json").exists());
            updates.finish_update_record().unwrap();
        }
    }
    let mut wrong = resume;
    wrong.to = "99.0.0".into();
    atomic_json(&temp.path().join("resume.json"), &wrong).unwrap();
    assert!(updates.finish_update_record().is_err());
    assert!(temp.path().join("resume.json").exists());
    wrong.to = env!("CARGO_PKG_VERSION").into();
    wrong.directory = temp.path().join("different-install");
    atomic_json(&temp.path().join("resume.json"), &wrong).unwrap();
    assert!(updates.finish_update_record().is_err());
    assert!(temp.path().join("resume.json").exists());
    std::fs::write(temp.path().join("resume.json"), "corrupt public fixture").unwrap();
    assert!(updates.finish_update_record().is_err());
    assert_eq!(std::fs::read_to_string(temp.path().join("resume.json")).unwrap(), "corrupt public fixture");
}
#[test]
fn failed_installer_preserves_update_record_but_never_saves_enable_intent() {
    let root = tempfile::tempdir().unwrap();
    let updates = Updates::new(root.path().into());
    for enabled in [true, false] {
        let resume = Resume {
            from: "1.0.6".into(), to: env!("CARGO_PKG_VERSION").into(),
            enabled, claimed: false,
            directory: std::env::current_exe().unwrap().parent().unwrap().into(),
        };
        let error = updates.install_with_resume(&resume, || {
            let saved: Resume = serde_json::from_slice(&std::fs::read(root.path().join("resume.json")).unwrap()).unwrap();
            assert!(!saved.claimed);
            assert!(!saved.enabled);
            Err("fixture: installer cannot start".into())
        }).unwrap_err();
        assert!(error.contains("installer cannot start"));
        let saved: Resume = serde_json::from_slice(&std::fs::read(root.path().join("resume.json")).unwrap()).unwrap();
        assert!(!saved.claimed);
        assert!(!saved.enabled);
    }
}
#[tokio::test]
async fn update_gate_waits_for_existing_operations_and_rejects_new_mutations() {
    let temp = tempfile::tempdir().unwrap();
    let updates = Updates::new(temp.path().into());
    let existing = updates.enter_work().unwrap();
    updates.installing.store(true, Ordering::SeqCst);
    assert!(updates.enter_work().is_err());
    assert!(
        tokio::time::timeout(Duration::from_millis(30), updates.work.write())
            .await
            .is_err()
    );
    drop(existing);
    let exclusive = updates.work.write().await;
    assert!(updates.enter_work().is_err());
    drop(exclusive);
    updates.installing.store(false, Ordering::SeqCst);
    assert!(updates.enter_work().is_ok());
}
#[tokio::test]
async fn interrupted_download_never_replaces_an_existing_package() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let temp = tempfile::tempdir().unwrap();
    let previous = temp.path().join("setup.exe");
    std::fs::write(&previous, b"previous").unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0; 4096];
        let _ = stream.read(&mut request).await.unwrap();
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nshort")
            .await
            .unwrap();
    });
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .get(format!("http://{address}"))
        .send()
        .await
        .unwrap();
    assert!(download_to_file(response, temp.path(), |_, _| {})
        .await
        .is_err());
    server.await.unwrap();
    assert_eq!(std::fs::read(previous).unwrap(), b"previous");
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
}
#[test]
fn blocked_writes_and_unsafe_cache_paths_preserve_previous_files() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("record.json");
    atomic_bytes(&path, b"previous").unwrap();
    assert!(safe_directory(temp.path(), &temp.path().join("../outside")).is_err());
    assert!(atomic_bytes(&path.join("child"), b"bad").is_err());
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let locked = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        assert!(atomic_bytes(&path, b"replacement").is_err());
        drop(locked);
    }
    assert_eq!(std::fs::read(&path).unwrap(), b"previous");
    atomic_bytes(&path, b"replacement").unwrap();
    assert_eq!(std::fs::read(path).unwrap(), b"replacement");
}
#[test]
fn cache_cleanup_keeps_current_candidate_repair_and_unknown_files() {
    let temp = tempfile::tempdir().unwrap(); let updates = Updates::new(temp.path().into());
    for version in [env!("CARGO_PKG_VERSION"), "98.0.0", "97.0.0", "96.0.0", "repair"] {
        let path = temp.path().join(version); std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("setup.exe"), b"public fixture").unwrap();
        std::fs::write(path.join("manifest.json"), b"{}").unwrap();
    }
    std::fs::write(temp.path().join("96.0.0/unknown.txt"), b"preserve").unwrap();
    updates.prune_cache("98.0.0");
    for version in [env!("CARGO_PKG_VERSION"), "98.0.0", "96.0.0", "repair"] { assert!(temp.path().join(version).exists()); }
    assert!(!temp.path().join("97.0.0").exists());
}

#[test]
fn startup_prompt_is_process_scoped_and_failures_preserve_the_candidate() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("channel.json"), b"\"preview\"").unwrap();
    let updates = Updates::new(temp.path().into());
    assert!(updates.apply_manifest(&manifest("99.0.0"), true));
    assert!(updates.status.lock().unwrap().startup_prompt_pending);
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1, "metadata discovery must not download files");
    updates.dismiss_prompt();
    updates.apply_manifest(&manifest("99.0.0"), false);
    updates.apply_manifest(&manifest("100.0.0"), true);
    assert!(!updates.status.lock().unwrap().startup_prompt_pending);
    updates.fail("offline".into());
    assert_eq!(updates.status.lock().unwrap().version.as_deref(), Some("100.0.0"));
    assert_eq!(updates.status.lock().unwrap().notes.as_deref(), Some("Public fixture"));
    let restarted = Updates::new(temp.path().into());
    restarted.apply_manifest(&manifest("100.0.0"), true);
    assert!(restarted.status.lock().unwrap().startup_prompt_pending);
    assert!(!restarted.apply_manifest(&manifest(env!("CARGO_PKG_VERSION")), true));
    let state = restarted.status.lock().unwrap();
    assert_eq!(state.phase, "current");
    assert!(state.version.is_none() && !state.startup_prompt_pending);
}
#[test]
fn runtime_discovery_never_opens_a_startup_prompt_and_stable_versions_only() {
    let temp = tempfile::tempdir().unwrap();
    let updates = Updates::new(temp.path().into());
    updates.apply_manifest(&manifest("99.0.0"), false);
    assert!(!updates.status.lock().unwrap().startup_prompt_pending);
    assert_eq!(updates.status.lock().unwrap().phase, "available");
    for version in ["99.0.0-beta.1", "99.0.0+build.1"] {
        assert!(validate_manifest(&manifest(version)).is_err());
    }
}
#[test]
fn install_requires_a_discovered_confirmed_version_and_defer_stops_before_installing() {
    let temp = tempfile::tempdir().unwrap();
    let updates = Updates::new(temp.path().into());
    assert!(updates.confirmed_candidate("99.0.0").is_err());
    updates.apply_manifest(&manifest("99.0.0"), true);
    assert_eq!(updates.confirmed_candidate("99.0.0").unwrap(), manifest("99.0.0"));
    updates.apply_manifest(&manifest("100.0.0"), false);
    assert!(updates.confirmed_candidate("99.0.0").is_err());
    updates.phase("checking");
    assert!(updates.confirmed_candidate("100.0.0").is_err());
    for phase in ["checking", "downloading", "preparing"] {
        let (sender, mut receiver) = tokio::sync::oneshot::channel();
        *updates.cancellation.lock().unwrap() = Some(sender);
        updates.requested.store(true, Ordering::SeqCst);
        updates.phase(phase);
        updates.defer().unwrap();
        assert!(receiver.try_recv().is_ok());
        updates.cancelled();
        assert_eq!(updates.status.lock().unwrap().phase, "available");
        assert!(updates.enter_work().is_ok());
    }
    let (sender, mut receiver) = tokio::sync::oneshot::channel();
    *updates.cancellation.lock().unwrap() = Some(sender);
    updates.installing.store(true, Ordering::SeqCst);
    assert!(updates.defer().is_err());
    assert!(matches!(receiver.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)));
    assert!(!updates.status.lock().unwrap().startup_prompt_pending);
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
}
#[tokio::test]
async fn stalled_download_times_out_and_cleans_partial_file() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let temp = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0; 4096];
        assert!(stream.read(&mut request).await.unwrap() > 0);
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nshort").await.unwrap();
        let mut eof = [0; 1];
        let _ = stream.read(&mut eof).await;
    });
    let response = reqwest::Client::builder().no_proxy().read_timeout(Duration::from_millis(100))
        .build().unwrap().get(format!("http://{address}")).send().await.unwrap();
    let mut seen = vec![];
    let result = tokio::time::timeout(Duration::from_secs(3), download_to_file(response, temp.path(), |n, total| seen.push((n, total)))).await.unwrap();
    assert!(result.is_err());
    assert_eq!(seen.first(), Some(&(0, Some(100))));
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    server.abort();
}
#[tokio::test]
async fn deferring_a_live_download_cleans_partial_file_and_cannot_enter_installation() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let temp = tempfile::tempdir().unwrap();
    let updates = Updates::new(temp.path().into());
    updates.apply_manifest(&manifest("99.0.0"), true);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0; 4096]; assert!(stream.read(&mut request).await.unwrap() > 0);
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nshort").await.unwrap();
        let mut eof = [0; 1]; let _ = stream.read(&mut eof).await;
    });
    let response = reqwest::Client::builder().no_proxy().build().unwrap()
        .get(format!("http://{address}")).send().await.unwrap();
    let (sender, mut receiver) = tokio::sync::oneshot::channel();
    *updates.cancellation.lock().unwrap() = Some(sender);
    updates.phase("downloading");
    let task = download_to_file(response, temp.path(), |n, _| { if n > 0 { updates.defer().unwrap(); } });
    let result = tokio::time::timeout(Duration::from_secs(3), updates.prepare_or_cancel(&mut receiver, task)).await.unwrap().unwrap();
    assert!(result.is_none());
    assert!(!updates.installing.load(Ordering::SeqCst));
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    // Cancellation arriving just after preparation must also win the transition.
    let (sender, mut receiver) = tokio::sync::oneshot::channel();
    *updates.cancellation.lock().unwrap() = Some(sender);
    updates.defer().unwrap();
    assert!(!updates.begin_install(&mut receiver));
    let (sender, mut receiver) = tokio::sync::oneshot::channel();
    *updates.cancellation.lock().unwrap() = Some(sender);
    assert!(updates.begin_install(&mut receiver));
    assert!(updates.defer().is_err());
    server.abort();
}
#[test]
fn acceptance_transport_only_maps_fixed_resources_to_ipv4_loopback() {
    assert_eq!(acceptance_url("http://127.0.0.1:12345", STABLE_FEED).unwrap(), "http://127.0.0.1:12345/stable.json");
    assert_eq!(acceptance_url("http://127.0.0.1:12345", &format!("{REPOSITORY}/releases/download/v0.1.30/manifest.json")).unwrap(), "http://127.0.0.1:12345/v0.1.30/manifest.json");
    for origin in ["https://127.0.0.1:12345", "http://localhost:12345", "http://example.invalid:12345", "http://127.0.0.1", "http://user@127.0.0.1:12345", "http://127.0.0.1:12345/?x=1"] {
        assert!(acceptance_url(origin, STABLE_FEED).is_err());
    }
    assert!(acceptance_url("http://127.0.0.1:12345", "https://example.invalid/setup.exe").is_err());
    #[cfg(not(feature = "update-acceptance"))]
    assert_eq!(transport_url(STABLE_FEED).unwrap(), STABLE_FEED, "production builds have no runtime source override");
}

#[cfg(feature = "update-acceptance")]
static ACCEPTANCE_ENV: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
#[cfg(feature = "update-acceptance")]
struct AcceptanceEnvironment(Option<std::ffi::OsString>);
#[cfg(feature = "update-acceptance")]
impl AcceptanceEnvironment {
    fn set(origin: &str) -> Self {
        let previous = std::env::var_os("FUSION_ACCEPTANCE_UPDATE_ORIGIN");
        std::env::set_var("FUSION_ACCEPTANCE_UPDATE_ORIGIN", origin);
        Self(previous)
    }
}
#[cfg(feature = "update-acceptance")]
impl Drop for AcceptanceEnvironment {
    fn drop(&mut self) {
        if let Some(previous) = &self.0 { std::env::set_var("FUSION_ACCEPTANCE_UPDATE_ORIGIN", previous); }
        else { std::env::remove_var("FUSION_ACCEPTANCE_UPDATE_ORIGIN"); }
    }
}
#[cfg(feature = "update-acceptance")]
async fn metadata_server(version: &str) -> (String, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let body = serde_json::to_vec(&manifest(version)).unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0; 2048]; let size = stream.read(&mut request).await.unwrap();
        assert!(std::str::from_utf8(&request[..size]).unwrap().starts_with("GET /stable.json "));
        let header = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
        stream.write_all(header.as_bytes()).await.unwrap(); stream.write_all(&body).await.unwrap();
    });
    (format!("http://{address}"), task)
}
#[cfg(feature = "update-acceptance")]
#[tokio::test]
async fn actual_metadata_check_uses_only_one_request_and_never_downloads_a_package() {
    let _serial = ACCEPTANCE_ENV.lock().await;
    let (origin, server) = metadata_server("99.0.0").await;
    let _environment = AcceptanceEnvironment::set(&origin);
    let temp = tempfile::tempdir().unwrap(); let updates = Updates::new(temp.path().into());
    assert!(updates.check(true).await.unwrap().is_some()); server.await.unwrap();
    assert_eq!(updates.status.lock().unwrap().phase, "available");
    assert!(updates.status.lock().unwrap().startup_prompt_pending);
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    assert!(updates.enter_work().is_ok());
}
