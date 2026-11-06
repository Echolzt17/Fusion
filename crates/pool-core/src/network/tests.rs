use super::*;
use std::collections::BTreeMap;

fn fixture() -> Vec<u8> {
    br#"proxies:
  - {name: 'US one', type: http, server: example.invalid, port: 443, tls: true}
  - {name: 'Japan two', type: socks5, server: another.invalid, port: 1080}
rules: ['MATCH,DIRECT']
tun: {enable: true}
external-controller: '0.0.0.0:9090'
"#
    .to_vec()
}

#[test]
fn subscription_imports_only_explicit_nodes_and_stable_identity() {
    let parsed = parse_subscription(&fixture()).unwrap();
    assert_eq!(parsed.nodes.len(), 2);
    assert_eq!(parsed.nodes[0].region, "US");
    assert_eq!(parsed.nodes[1].region, "JP");
    assert!(parsed.nodes[0].config.get("tun").is_none());
    assert_eq!(parsed.nodes[0].config["skip-cert-verify"], false);
    let renamed = String::from_utf8(fixture())
        .unwrap()
        .replace("US one", "renamed");
    assert_eq!(
        parse_subscription(renamed.as_bytes()).unwrap().nodes[0].id,
        parsed.nodes[0].id
    );
}

#[test]
fn unsafe_or_unsupported_nodes_are_excluded_without_echoing_credentials() {
    let raw = br#"proxies:
  - {name: good, type: http, server: example.invalid, port: 80}
  - {name: plugin, type: ss, server: example.invalid, port: 80, cipher: aes-128-gcm, password: secret, plugin: unknown}
  - {name: insecure, type: trojan, server: example.invalid, port: 443, password: secret, skip-cert-verify: true}
  - {name: chain, type: http, server: example.invalid, port: 80, dialer-proxy: DIRECT}
  - {name: group, type: select, proxies: [DIRECT]}
  - {name: localfile, type: wireguard, private-key: secret}
"#;
    let parsed = parse_subscription(raw).unwrap();
    assert_eq!(parsed.nodes.len(), 1);
    assert_eq!(parsed.excluded, 5);
    let error = match parse_subscription(b"proxies: [secret-with-invalid-yaml") {
        Err(e) => e,
        Ok(_) => panic!("invalid YAML accepted"),
    };
    assert!(!error.contains("secret-with-invalid-yaml"));
}

#[test]
fn subscription_limits_and_dedup_are_enforced() {
    assert!(parse_subscription(&vec![b' '; MAX_SUBSCRIPTION_BYTES + 1]).is_err());
    let many = serde_json::json!({"proxies":(0..1001).map(|i| serde_json::json!({"name":format!("node {i}"),"type":"http","server":"example.invalid","port":80})).collect::<Vec<_>>()});
    assert!(parse_subscription(&serde_json::to_vec(&many).unwrap()).is_err());
    let duplicate = br#"proxies:
 - {name: a, type: http, server: example.invalid, port: 80}
 - {name: b, type: http, server: example.invalid, port: 80}
"#;
    let parsed = parse_subscription(duplicate).unwrap();
    assert_eq!(parsed.nodes.len(), 1);
    assert_eq!(parsed.excluded, 1);
}
#[test]
fn yaml_alias_expansion_and_duplicate_keys_are_bounded() {
    let mut raw = String::from("seed: &a0 [one, two, three]\n");
    for i in 1..12 {
        raw.push_str(&format!(
            "v{i}: &a{i} [*a{}, *a{}, *a{}]\n",
            i - 1,
            i - 1,
            i - 1
        ));
    }
    raw.push_str("proxies: []\n");
    assert!(parse_subscription(raw.as_bytes()).is_err());
    assert!(parse_subscription(b"proxies: []\nproxies: []\n").is_err());
}

#[test]
fn regions_are_annotations_and_can_be_corrected() {
    assert_eq!(infer_region("🇸🇬 Premium"), "SG");
    assert_eq!(infer_region("印度尼西亚 Jakarta"), "ID");
    assert_eq!(infer_region("unknown route"), "UNKNOWN");
    let nodes = parse_subscription(&fixture()).unwrap().nodes;
    let mut settings = NetworkSettings::default();
    settings
        .region_overrides
        .insert(nodes[0].id.clone(), "CA".into());
    assert_eq!(policy::region(&nodes[0], &settings), "CA");
    settings.allowed_regions = vec!["CA".into()];
    assert_eq!(
        policy::select_node(&nodes, &settings, &BTreeMap::new(), None, 10),
        Some(nodes[0].id.clone())
    );
    settings.allowed_regions = vec!["not-a-region".into()];
    assert!(policy::validate_settings(&settings, &nodes).is_err());
}

#[test]
fn sticky_selection_waits_three_rounds_and_does_not_steal_back() {
    let nodes = parse_subscription(&fixture()).unwrap().nodes;
    let settings = NetworkSettings {
        allowed_regions: vec!["US".into(), "JP".into()],
        ..NetworkSettings::default()
    };
    let mut health = BTreeMap::new();
    let first = nodes[0].id.clone();
    let second = nodes[1].id.clone();
    health
        .entry(first.clone())
        .or_insert_with(NodeHealth::default)
        .record(Some(100), 10);
    health
        .entry(second.clone())
        .or_insert_with(NodeHealth::default)
        .record(Some(1), 10);
    assert_eq!(
        policy::select_node(&nodes, &settings, &health, Some(&first), 11),
        Some(first.clone())
    );
    for at in [11, 12] {
        health.get_mut(&first).unwrap().record(None, at);
        assert_eq!(
            policy::select_node(&nodes, &settings, &health, Some(&first), at),
            Some(first.clone())
        );
    }
    health.get_mut(&first).unwrap().record(None, 13);
    assert_eq!(health[&first].cooldown_until, Some(313));
    assert_eq!(
        policy::select_node(&nodes, &settings, &health, Some(&first), 13),
        Some(second.clone())
    );
    health.get_mut(&first).unwrap().record(Some(1), 314);
    assert_eq!(
        policy::select_node(&nodes, &settings, &health, Some(&second), 314),
        Some(second)
    );
}

#[test]
fn fixed_node_never_falls_through_to_another_region_or_node() {
    let nodes = parse_subscription(&fixture()).unwrap().nodes;
    let mut settings = NetworkSettings {
        mode: SelectionMode::Fixed,
        fixed_node_id: Some(nodes[0].id.clone()),
        allowed_regions: vec!["US".into(), "JP".into()],
        ..NetworkSettings::default()
    };
    let mut health = BTreeMap::new();
    let mut bad = NodeHealth::default();
    for at in 1..=3 {
        bad.record(None, at);
    }
    health.insert(nodes[0].id.clone(), bad);
    assert!(policy::select_node(&nodes, &settings, &health, None, 5).is_none());
    settings.allowed_regions = vec!["JP".into()];
    assert!(policy::select_node(&nodes, &settings, &BTreeMap::new(), None, 5).is_none());
}

#[test]
fn auto_requires_explicit_regions_and_prefers_same_region_then_saved_order() {
    let raw = br#"proxies:
 - {name: 'US primary', type: http, server: one.invalid, port: 80}
 - {name: 'JP fast', type: http, server: two.invalid, port: 80}
 - {name: 'US backup', type: http, server: three.invalid, port: 80}
 - {name: anonymous, type: http, server: four.invalid, port: 80}
"#;
    let nodes = parse_subscription(raw).unwrap().nodes;
    let mut settings = NetworkSettings::default();
    assert!(policy::select_node(&nodes, &settings, &BTreeMap::new(), None, 10).is_none());
    settings.enabled = true;
    assert!(policy::validate_settings(&settings, &nodes).is_err());
    settings.allowed_regions = vec!["JP".into(), "US".into(), "UNKNOWN".into()];
    settings.node_order = vec![
        nodes[3].id.clone(),
        nodes[1].id.clone(),
        nodes[2].id.clone(),
    ];
    let mut health = BTreeMap::new();
    let mut bad = NodeHealth::default();
    for at in 1..=3 {
        bad.record(None, at);
    }
    health.insert(nodes[0].id.clone(), bad);
    assert_eq!(
        policy::select_node(&nodes, &settings, &health, Some(&nodes[0].id), 10),
        Some(nodes[2].id.clone())
    );
    assert_eq!(
        policy::select_node(&nodes, &settings, &health, None, 10),
        Some(nodes[1].id.clone())
    );
}

#[test]
fn only_https_subscription_urls_without_authority_credentials_are_accepted() {
    assert!(persistence::validate_url("https://example.invalid/sub?token=fake").is_ok());
    for raw in [
        "http://example.invalid/a",
        "https://a:b@example.invalid/sub",
        "file:///c:/foo",
        "https://example.invalid/sub#fragment",
    ] {
        assert!(persistence::validate_url(raw).is_err());
    }
}

#[cfg(windows)]
mod runtime {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };
    struct FakeKernel {
        running: Arc<AtomicBool>,
        config: Mutex<Option<serde_json::Value>>,
        fail: AtomicBool,
        starts: AtomicUsize,
        reject_server: Mutex<Option<String>>,
        stall_probe: Arc<AtomicBool>,
    }
    impl FakeKernel {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                running: Arc::new(AtomicBool::new(false)),
                config: Mutex::new(None),
                fail: AtomicBool::new(false),
                starts: AtomicUsize::new(0),
                reject_server: Mutex::new(None),
                stall_probe: Arc::new(AtomicBool::new(false)),
            })
        }
    }
    impl KernelBackend for FakeKernel {
        fn start(&self, config: &str) -> Result<KernelEndpoint> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            if self.fail.load(Ordering::SeqCst) {
                return Err("假内核拒绝启动".into());
            }
            let parsed: serde_json::Value =
                serde_yaml_ng::from_str(config).map_err(|_| "假内核配置无效")?;
            if self
                .reject_server
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|server| {
                    parsed["proxies"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|n| n["server"] == *server)
                })
            {
                return Err("候选假内核配置不兼容".into());
            }
            *self.config.lock().unwrap() = Some(parsed);
            self.running.store(true, Ordering::SeqCst);
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            listener.set_nonblocking(true).unwrap();
            let running = self.running.clone();
            let stall_probe = self.stall_probe.clone();
            std::thread::spawn(move || {
                while running.load(Ordering::SeqCst) {
                    if let Ok((mut socket, _)) = listener.accept() {
                        socket
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let mut bytes = [0; 8192];
                        let _ = socket.read(&mut bytes);
                        if stall_probe.load(Ordering::SeqCst) {
                            while stall_probe.load(Ordering::SeqCst)
                                && running.load(Ordering::SeqCst)
                            {
                                std::thread::sleep(Duration::from_millis(5));
                            }
                            continue;
                        }
                        let body = b"{\"delay\":12}";
                        let header = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                        let _ = socket.write_all(header.as_bytes());
                        let _ = socket.write_all(body);
                    } else {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
            });
            Ok(KernelEndpoint {
                proxy_url: format!("http://{address}"),
                controller_url: format!("http://{address}"),
                instance_id: uuid::Uuid::new_v4().to_string(),
            })
        }
        fn stop(&self) -> Result<()> {
            self.running.store(false, Ordering::SeqCst);
            Ok(())
        }
        fn running(&self) -> bool {
            self.running.load(Ordering::SeqCst)
        }
    }
    fn network(temp: &tempfile::TempDir, kernel: Arc<FakeKernel>) -> Arc<Network> {
        Network::new(
            Arc::new(Store::open(&temp.path().join("pool.sqlite")).unwrap()),
            Arc::new(crate::vault::MemoryVault::default()),
            temp.path().join("network"),
            kernel,
        )
        .unwrap()
    }
    async fn import(network: &Arc<Network>) {
        network
            .import_subscription(SubscriptionInput {
                url: None,
                yaml: Some(String::from_utf8(fixture()).unwrap()),
                name: Some("公开假订阅".into()),
            })
            .await
            .unwrap();
        let mut settings = network.status().unwrap().settings;
        settings.allowed_regions = vec!["US".into(), "JP".into()];
        network.save_settings(settings).await.unwrap();
    }
    #[tokio::test]
    async fn persistence_encrypts_snapshot_and_never_exports_credentials() {
        let temporary = tempfile::tempdir().unwrap();
        let kernel = FakeKernel::new();
        let network = network(&temporary, kernel);
        import(&network).await;
        let status = serde_json::to_string(&network.status().unwrap()).unwrap();
        assert!(!status.contains("example.invalid"));
        assert!(!status.contains("password"));
        let files: Vec<_> = std::fs::read_dir(temporary.path().join("network"))
            .unwrap()
            .collect();
        assert_eq!(files.len(), 1);
        let raw = std::fs::read(files[0].as_ref().unwrap().path()).unwrap();
        assert!(!String::from_utf8_lossy(&raw).contains("example.invalid"));
        let reloaded = Network::new(
            network.persistence.store.clone(),
            Arc::new(crate::vault::MemoryVault::default()),
            temporary.path().join("network"),
            FakeKernel::new(),
        )
        .unwrap();
        assert_eq!(reloaded.status().unwrap().nodes.len(), 2);
    }
    #[tokio::test]
    async fn route_lease_pins_node_and_disable_waits_for_full_response() {
        let temporary = tempfile::tempdir().unwrap();
        let kernel = FakeKernel::new();
        let network = network(&temporary, kernel.clone());
        import(&network).await;
        network.set_enabled(true).await.unwrap();
        let first = network.acquire().await.unwrap().unwrap();
        let first_node = network.status().unwrap().current_node_id.unwrap();
        let second_node = network
            .status()
            .unwrap()
            .nodes
            .into_iter()
            .find(|n| n.id != first_node)
            .unwrap()
            .id;
        network
            .select_node(second_node.clone(), true)
            .await
            .unwrap();
        let second = network.acquire().await.unwrap().unwrap();
        assert_ne!(first.username, second.username);
        assert_ne!(first.cache_key, second.cache_key);
        assert_eq!(first.password, second.password);
        first.lease.record_business(false, true);
        first.lease.record_business(false, true);
        let first_health = network
            .status()
            .unwrap()
            .nodes
            .into_iter()
            .find(|n| n.id == first_node)
            .unwrap();
        assert_eq!(first_health.failure_count, 0);
        assert_eq!(first_health.business_failures, 1);
        drop(second);
        let off_network = network.clone();
        let mut off = tokio::spawn(async move { off_network.set_enabled(false).await });
        assert!(tokio::time::timeout(Duration::from_millis(50), &mut off)
            .await
            .is_err());
        assert!(kernel.running());
        drop(first);
        off.await.unwrap().unwrap();
        assert!(!kernel.running());
        assert!(network.acquire().await.unwrap().is_none());
    }
    #[tokio::test]
    async fn failed_initial_start_does_not_enable_or_bypass_the_switch() {
        let temporary = tempfile::tempdir().unwrap();
        let kernel = FakeKernel::new();
        let network = network(&temporary, kernel.clone());
        import(&network).await;
        let mut settings = network.status().unwrap().settings;
        settings.enabled = true;
        assert!(network.save_settings(settings).await.is_err());
        kernel.fail.store(true, Ordering::SeqCst);
        assert!(network.set_enabled(true).await.is_err());
        assert!(!network.status().unwrap().settings.enabled);
        kernel.fail.store(false, Ordering::SeqCst);
        network.set_enabled(true).await.unwrap();
        network.shutdown().await.unwrap();
        assert!(network.acquire().await.is_err());
        network.retry().await.unwrap();
        assert!(network.acquire().await.unwrap().is_some());
        network.set_enabled(false).await.unwrap();
    }
    #[tokio::test]
    async fn candidate_start_failure_preserves_previous_snapshot_and_restarts_it() {
        let temporary = tempfile::tempdir().unwrap();
        let kernel = FakeKernel::new();
        let network = network(&temporary, kernel.clone());
        import(&network).await;
        network.set_enabled(true).await.unwrap();
        let before = network.persistence.store.get("network/v1/state").unwrap();
        let old_ids: Vec<_> = network
            .status()
            .unwrap()
            .nodes
            .iter()
            .map(|n| n.id.clone())
            .collect();
        *kernel.reject_server.lock().unwrap() = Some("bad.invalid".into());
        let yaml = String::from_utf8(fixture())
            .unwrap()
            .replace("example.invalid", "bad.invalid");
        assert!(network
            .import_subscription(SubscriptionInput {
                url: None,
                yaml: Some(yaml),
                name: Some("不兼容的假候选".into())
            })
            .await
            .is_err());
        let status = network.status().unwrap();
        assert_eq!(
            status
                .nodes
                .iter()
                .map(|n| n.id.clone())
                .collect::<Vec<_>>(),
            old_ids
        );
        assert!(status.kernel_running);
        assert!(status.settings.enabled);
        assert!(!status.pending_update);
        let saved: serde_json::Value = serde_json::from_str(
            &network
                .persistence
                .store
                .get("network/v1/state")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        let prior: serde_json::Value = serde_json::from_str(&before.unwrap()).unwrap();
        assert_eq!(saved["snapshotFile"], prior["snapshotFile"]);
        assert!(network.acquire().await.unwrap().is_some());
        network.set_enabled(false).await.unwrap();
    }
    #[tokio::test]
    async fn disabling_cancels_the_unauthenticated_probe_batch() {
        let temporary = tempfile::tempdir().unwrap();
        let kernel = FakeKernel::new();
        let network = network(&temporary, kernel.clone());
        import(&network).await;
        network.set_enabled(true).await.unwrap();
        kernel.stall_probe.store(true, Ordering::SeqCst);
        let checking = network.clone();
        let check = tokio::spawn(async move { checking.check_nodes(None).await });
        tokio::time::sleep(Duration::from_millis(30)).await;
        tokio::time::timeout(Duration::from_secs(2), network.set_enabled(false))
            .await
            .unwrap()
            .unwrap();
        check.await.unwrap().unwrap();
        assert!(!kernel.running());
    }
    #[tokio::test]
    async fn system_pool_lease_also_blocks_enable_until_old_request_finishes() {
        let temporary = tempfile::tempdir().unwrap();
        let kernel = FakeKernel::new();
        let network = network(&temporary, kernel.clone());
        import(&network).await;
        let (route, system) = network.acquire_pool().await.unwrap();
        assert!(route.is_none());
        let on_network = network.clone();
        let mut on = tokio::spawn(async move { on_network.set_enabled(true).await });
        assert!(tokio::time::timeout(Duration::from_millis(50), &mut on)
            .await
            .is_err());
        assert!(!kernel.running());
        drop(system);
        on.await.unwrap().unwrap();
        assert!(kernel.running());
        network.set_enabled(false).await.unwrap();
    }
    #[tokio::test]
    async fn failed_controlled_restart_is_latched_until_explicit_retry() {
        let temporary = tempfile::tempdir().unwrap();
        let kernel = FakeKernel::new();
        let network = network(&temporary, kernel.clone());
        import(&network).await;
        let _checks = network.checks.lock().await;
        network.set_enabled(true).await.unwrap();
        kernel.running.store(false, Ordering::SeqCst);
        kernel.fail.store(true, Ordering::SeqCst);
        let starts = kernel.starts.load(Ordering::SeqCst);
        assert!(network.acquire().await.is_err());
        assert!(network.acquire().await.is_err());
        assert_eq!(kernel.starts.load(Ordering::SeqCst), starts + 1);
        kernel.fail.store(false, Ordering::SeqCst);
        network.retry().await.unwrap();
        assert!(network.acquire().await.unwrap().is_some());
        network.set_enabled(false).await.unwrap();
    }
    #[tokio::test]
    async fn concurrent_same_node_checks_share_one_round_and_use_global_slots() {
        let temporary = tempfile::tempdir().unwrap();
        let kernel = FakeKernel::new();
        let network = network(&temporary, kernel);
        import(&network).await;
        let _checks = network.checks.lock().await;
        network.set_enabled(true).await.unwrap();
        let runtime = network.state.lock().unwrap().runtime.clone().unwrap();
        let id = network.status().unwrap().nodes[0].id.clone();
        let permits = network.probe_slots.acquire_many(4).await.unwrap();
        let a = network.clone();
        let ar = runtime.clone();
        let ai = id.clone();
        let first = tokio::spawn(async move { a.probe(&ar, &ai).await });
        let b = network.clone();
        let br = runtime.clone();
        let bi = id.clone();
        let second = tokio::spawn(async move { b.probe(&br, &bi).await });
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!first.is_finished());
        assert!(!second.is_finished());
        drop(permits);
        assert_eq!(first.await.unwrap(), Some(12));
        assert_eq!(second.await.unwrap(), Some(12));
        assert_eq!(
            network.state.lock().unwrap().index.health[&id].successful_checks,
            1
        );
        network.set_enabled(false).await.unwrap();
    }
    #[test]
    fn foreign_vault_references_are_rejected_before_access_or_removal() {
        let temporary = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&temporary.path().join("pool.sqlite")).unwrap());
        store.set("network/v1/state", r#"{"version":1,"settings":{"enabled":false,"mode":"auto","allowedRegions":[],"nodeOrder":[],"fixedNodeId":null,"excludedNodeIds":[],"regionOverrides":{}},"snapshotFile":null,"urlKey":"account/do-not-delete","currentNodeId":null,"health":{}}"#).unwrap();
        assert!(Network::new(
            store,
            Arc::new(crate::vault::MemoryVault::default()),
            temporary.path().join("network"),
            FakeKernel::new()
        )
        .is_err());
    }
    #[tokio::test]
    async fn background_checks_current_each_minute_and_backups_each_three() {
        let temporary = tempfile::tempdir().unwrap();
        let network = network(&temporary, FakeKernel::new());
        import(&network).await;
        let current = network.status().unwrap().current_node_id.unwrap();
        {
            let mut state = network.state.lock().unwrap();
            let ids: Vec<_> = state
                .snapshot
                .as_ref()
                .unwrap()
                .nodes
                .iter()
                .map(|n| n.id.clone())
                .collect();
            for id in ids {
                state
                    .index
                    .health
                    .entry(id)
                    .or_default()
                    .record(Some(12), crate::now() - 100);
            }
        }
        assert_eq!(network.background_due_nodes(), vec![current]);
    }
    #[tokio::test]
    async fn nested_check_pauses_resume_only_after_last_guard_in_either_drop_order() {
        for outer_first in [false, true] {
            let temporary = tempfile::tempdir().unwrap();
            let network = network(&temporary, FakeKernel::new());
            let original = network.check_cancel.lock().unwrap().clone();
            let mut outer = Some(network.pause_background_checks());
            let mut inner = Some(network.pause_checks());
            assert_eq!(network.checks_paused.load(Ordering::SeqCst), 2);
            assert!(original.is_cancelled());

            if outer_first {
                drop(outer.take());
            } else {
                drop(inner.take());
            }
            assert_eq!(network.checks_paused.load(Ordering::SeqCst), 1);
            // Even the explicit retry-resume path cannot override a live pause.
            network.resume_checks();
            assert!(network.check_cancel.lock().unwrap().is_cancelled());
            // With no subscription this would fail if a prematurely resumed
            // check reached its ordinary validation or tried to start a kernel.
            assert!(network.check_nodes(None).await.is_ok());

            drop(outer.take());
            drop(inner.take());
            assert_eq!(network.checks_paused.load(Ordering::SeqCst), 0);
            assert!(!network.check_cancel.lock().unwrap().is_cancelled());
            assert!(original.is_cancelled());

            // Shutdown still releases the count without restarting probes;
            // a later explicit retry can then resume after stopping is cleared.
            let stopped = network.pause_background_checks();
            network.stopping.store(true, Ordering::SeqCst);
            drop(stopped);
            assert_eq!(network.checks_paused.load(Ordering::SeqCst), 0);
            assert!(network.check_cancel.lock().unwrap().is_cancelled());
            network.stopping.store(false, Ordering::SeqCst);
            network.resume_checks();
            assert!(!network.check_cancel.lock().unwrap().is_cancelled());
        }
    }
    #[tokio::test]
    async fn background_budget_cancels_unfinished_probes_without_consuming_health_rounds() {
        let temporary = tempfile::tempdir().unwrap();
        let kernel = FakeKernel::new();
        let network = network(&temporary, kernel.clone());
        import(&network).await;
        // Drive one pass directly so this test need not wait a production minute.
        network.background.store(true, Ordering::SeqCst);
        network.set_enabled(true).await.unwrap();
        kernel.stall_probe.store(true, Ordering::SeqCst);
        tokio::time::timeout(
            Duration::from_secs(2),
            network.background_check(None, Duration::from_millis(40)),
        )
        .await
        .unwrap();
        assert!(!network.checking.load(Ordering::SeqCst));
        assert!(network.checks.try_lock().is_ok());
        assert!(network.routes.try_write().is_ok());
        assert!(network
            .status()
            .unwrap()
            .nodes
            .iter()
            .all(|n| n.failure_count == 0 && n.last_checked_at.is_none()));
        assert_eq!(network.background_due_nodes().len(), 2);
        network.set_enabled(false).await.unwrap();
    }
}
