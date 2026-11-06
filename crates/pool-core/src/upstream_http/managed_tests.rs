//! Pool-managed routing tests use only temporary stores and loopback fake servers.
//! Proxy identities are compared in memory; no header or credential is logged.
#![cfg(windows)]

use super::*;
use crate::{
    account::Accounts,
    network::{KernelBackend, KernelEndpoint, Network, SubscriptionInput},
    storage::Store,
    vault::MemoryVault,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Semaphore,
    task::JoinHandle,
};

const ENDPOINT: &str = "http://public-fixture.invalid/responses";

struct Captured {
    username: Option<String>,
}

struct FakeServer {
    url: String,
    hits: Arc<Mutex<Vec<Captured>>>,
    headers_sent: Arc<Semaphore>,
    finish_body: Arc<Semaphore>,
    task: JoinHandle<()>,
}
impl Drop for FakeServer {
    fn drop(&mut self) { self.task.abort(); }
}

async fn read_headers(socket: &mut TcpStream) -> Option<String> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 2048];
    while bytes.len() < 16 * 1024 {
        let count = socket.read(&mut buffer).await.ok()?;
        if count == 0 { return None; }
        bytes.extend_from_slice(&buffer[..count]);
        if bytes.windows(4).any(|part| part == b"\r\n\r\n") {
            return String::from_utf8(bytes).ok();
        }
    }
    None
}

async fn fake_proxy(hold_credit_body: bool) -> FakeServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let hits = Arc::new(Mutex::new(Vec::new()));
    let headers_sent = Arc::new(Semaphore::new(0));
    let finish_body = Arc::new(Semaphore::new(0));
    let (requests, headers, finish) = (hits.clone(), headers_sent.clone(), finish_body.clone());
    let task = tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let (requests, headers, finish) = (requests.clone(), headers.clone(), finish.clone());
            tokio::spawn(async move {
                let Some(raw) = read_headers(&mut socket).await else { return; };
                let username = raw.lines().find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    if !key.eq_ignore_ascii_case("proxy-authorization") { return None; }
                    let encoded = value.trim().strip_prefix("Basic ")?;
                    let decoded = STANDARD.decode(encoded).ok()?;
                    let decoded = String::from_utf8(decoded).ok()?;
                    decoded.split_once(':').map(|(name, _)| name.to_owned())
                });
                requests.lock().unwrap().push(Captured { username });
                if hold_credit_body && raw.lines().next().is_some_and(|line| line.contains("/credits")) {
                    let first = b"{\"available_count\":0,";
                    let last = b"\"credits\":[]}";
                    let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", first.len() + last.len());
                    if socket.write_all(response.as_bytes()).await.is_err() { return; }
                    if socket.write_all(first).await.is_err() { return; }
                    headers.add_permits(1);
                    let Ok(permit) = finish.acquire().await else { return; };
                    permit.forget();
                    let _ = socket.write_all(last).await;
                } else {
                    let _ = socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").await;
                }
            });
        }
    });
    FakeServer { url, hits, headers_sent, finish_body, task }
}

async fn fake_controller() -> FakeServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                if read_headers(&mut socket).await.is_none() { return; }
                let body = b"{\"delay\":12}";
                let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.write_all(body).await;
            });
        }
    });
    FakeServer { url, hits: Arc::new(Mutex::new(Vec::new())), headers_sent: Arc::new(Semaphore::new(0)), finish_body: Arc::new(Semaphore::new(0)), task }
}

struct FakeKernel {
    proxy_url: String,
    controller_url: String,
    running: AtomicBool,
    fail_start: AtomicBool,
    starts: AtomicUsize,
}
impl KernelBackend for FakeKernel {
    fn start(&self, config: &str) -> Result<KernelEndpoint> {
        if self.fail_start.load(Ordering::SeqCst) { return Err("公开假内核启动失败".into()); }
        let parsed: serde_json::Value = serde_yaml_ng::from_str(config).map_err(|_| "公开假内核配置无效")?;
        assert_eq!(parsed["listeners"][0]["listen"], "127.0.0.1");
        assert_eq!(parsed["rules"].as_array().unwrap().last().unwrap(), "MATCH,REJECT");
        let instance = self.starts.fetch_add(1, Ordering::SeqCst);
        self.running.store(true, Ordering::SeqCst);
        Ok(KernelEndpoint { proxy_url: self.proxy_url.clone(), controller_url: self.controller_url.clone(), instance_id: format!("public-test-{instance}") })
    }
    fn stop(&self) -> Result<()> { self.running.store(false, Ordering::SeqCst); Ok(()) }
    fn running(&self) -> bool { self.running.load(Ordering::SeqCst) }
}

struct Fixture {
    _temporary: tempfile::TempDir,
    _controller: FakeServer,
    system: FakeServer,
    managed: FakeServer,
    kernel: Arc<FakeKernel>,
    network: Arc<Network>,
    http: UpstreamHttp,
    store: Arc<Store>,
    vault: Arc<MemoryVault>,
    system_resolutions: Arc<AtomicUsize>,
}
impl Fixture {
    async fn new(hold_credit_body: bool) -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let system = fake_proxy(false).await;
        let managed = fake_proxy(hold_credit_body).await;
        let controller = fake_controller().await;
        let kernel = Arc::new(FakeKernel {
            proxy_url: managed.url.clone(), controller_url: controller.url.clone(),
            running: AtomicBool::new(false), fail_start: AtomicBool::new(false), starts: AtomicUsize::new(0),
        });
        let store = Arc::new(Store::open(&temporary.path().join("pool.sqlite")).unwrap());
        let vault = Arc::new(MemoryVault::default());
        let network = Network::new(store.clone(), vault.clone(), temporary.path().join("network"), kernel.clone()).unwrap();
        network.import_subscription(SubscriptionInput {
            url: None, name: Some("公开集成测试订阅".into()), yaml: Some("proxies:\n - {name: 'Japan one', type: http, server: first.invalid, port: 80}\n - {name: 'US two', type: http, server: second.invalid, port: 80}\n".into()),
        }).await.unwrap();
        let mut settings = network.status().unwrap().settings;
        settings.allowed_regions = vec!["JP".into(), "US".into()];
        network.save_settings(settings).await.unwrap();
        network.set_enabled(true).await.unwrap();
        let system_resolutions = Arc::new(AtomicUsize::new(0));
        let resolutions = system_resolutions.clone();
        let route = system.url.clone();
        let mut http = UpstreamHttp::new().unwrap();
        http.resolver = Arc::new(move |_| { resolutions.fetch_add(1, Ordering::SeqCst); Ok(Route { proxy: route.clone(), source: ProxySource::WindowsStaticConfigured }) });
        http.set_network(network.clone()).unwrap();
        Self { _temporary: temporary, _controller: controller, system, managed, kernel, network, http, store, vault, system_resolutions }
    }
    fn accounts(&self) -> Arc<Accounts> {
        let mut accounts = Accounts::new(self.store.clone(), self.vault.clone()).unwrap();
        accounts.reset_endpoint = "http://public-fixture.invalid/credits".into();
        accounts.http.set_network(self.network.clone()).unwrap();
        crate::tests::add(&accounts, "managed-account");
        Arc::new(accounts)
    }
}

#[tokio::test]
async fn pool_override_does_not_change_plain_api_or_loopback_requests() {
    let fixture = Fixture::new(false).await;
    let (request, source, _, lease) = fixture.http.pool_gateway_request(Method::GET, ENDPOINT).await.unwrap();
    assert_eq!(source, ProxySource::ManagedKernel);
    assert!(lease.is_some());
    assert_eq!(request.send().await.unwrap().text().await.unwrap(), "ok");
    drop(lease);
    let (request, source) = fixture.http.request(Method::GET, ENDPOINT).await.unwrap();
    assert_eq!(source, ProxySource::WindowsStaticConfigured);
    assert_eq!(request.send().await.unwrap().text().await.unwrap(), "ok");
    let (request, source, _, lease) = fixture.http.pool_gateway_request(Method::GET, &fixture.system.url).await.unwrap();
    assert_eq!(source, ProxySource::LoopbackDirect);
    assert!(lease.is_none());
    request.send().await.unwrap().bytes().await.unwrap();
    assert_eq!(fixture.managed.hits.lock().unwrap().len(), 1);
    assert_eq!(fixture.system.hits.lock().unwrap().len(), 2);
    assert_eq!(fixture.system_resolutions.load(Ordering::SeqCst), 1);
    assert!(fixture.system.hits.lock().unwrap().iter().all(|hit| hit.username.is_none()));
    fixture.network.shutdown().await.unwrap();
}

#[tokio::test]
async fn changing_selection_cannot_move_an_existing_builder_or_its_retry_client() {
    let fixture = Fixture::new(false).await;
    let (old, _, retry_client, old_lease) = fixture.http.pool_gateway_request(Method::POST, ENDPOINT).await.unwrap();
    let first_id = fixture.network.status().unwrap().current_node_id.unwrap();
    let second_id = fixture.network.status().unwrap().nodes.into_iter().find(|node| node.id != first_id).unwrap().id;
    let (old_client, old_request) = old.body("public-fake-body").build_split();
    let old_request = old_request.unwrap();
    let retry_request = old_request.try_clone().unwrap();
    fixture.network.select_node(second_id.clone(), true).await.unwrap();
    let (new, _, _, new_lease) = fixture.http.pool_gateway_request(Method::POST, ENDPOINT).await.unwrap();
    old_client.execute(old_request).await.unwrap().bytes().await.unwrap();
    // Explicit execution models only the existing proven-pre-dispatch retry;
    // selecting a new node must not mutate that captured retry transport.
    retry_client.execute(retry_request).await.unwrap().bytes().await.unwrap();
    new.send().await.unwrap().bytes().await.unwrap();
    let identities: Vec<_> = fixture.managed.hits.lock().unwrap().iter().map(|hit| hit.username.clone()).collect();
    assert_eq!(identities, vec![Some(format!("n_{first_id}")), Some(format!("n_{first_id}")), Some(format!("n_{second_id}"))]);
    assert_eq!(fixture.network.status().unwrap().active_requests, 2);
    drop(old_lease); drop(new_lease);
    assert_eq!(fixture.network.status().unwrap().active_requests, 0);
    assert_eq!(fixture.system_resolutions.load(Ordering::SeqCst), 0);
    fixture.network.shutdown().await.unwrap();
}

#[tokio::test]
async fn disabling_waits_until_the_real_reset_reader_finishes_the_response_body() {
    let fixture = Fixture::new(true).await;
    let accounts = fixture.accounts();
    let read_accounts = accounts.clone();
    let reader = tokio::spawn(async move { read_accounts.reset_credits("managed-account").await });
    tokio::time::timeout(Duration::from_secs(3), fixture.managed.headers_sent.acquire()).await.unwrap().unwrap().forget();
    assert_eq!(fixture.network.status().unwrap().active_requests, 1);
    let network = fixture.network.clone();
    let mut disabled = tokio::spawn(async move { network.set_enabled(false).await });
    assert!(tokio::time::timeout(Duration::from_millis(75), &mut disabled).await.is_err());
    assert!(fixture.kernel.running());
    fixture.managed.finish_body.add_permits(1);
    let credits = tokio::time::timeout(Duration::from_secs(3), reader).await.unwrap().unwrap().unwrap();
    assert_eq!(credits.available_count, 0);
    tokio::time::timeout(Duration::from_secs(3), disabled).await.unwrap().unwrap().unwrap();
    assert!(!fixture.kernel.running());
    assert_eq!(fixture.network.status().unwrap().active_requests, 0);
    assert_eq!(fixture.managed.hits.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn cancelling_the_real_response_reader_releases_disable_without_replay() {
    let fixture = Fixture::new(true).await;
    let accounts = fixture.accounts();
    let reader = tokio::spawn(async move { accounts.reset_credits("managed-account").await });
    tokio::time::timeout(Duration::from_secs(3), fixture.managed.headers_sent.acquire()).await.unwrap().unwrap().forget();
    let network = fixture.network.clone();
    let mut disabled = tokio::spawn(async move { network.set_enabled(false).await });
    assert!(tokio::time::timeout(Duration::from_millis(75), &mut disabled).await.is_err());
    reader.abort();
    let Err(error) = reader.await else { panic!("a cancelled reader unexpectedly completed"); };
    assert!(error.is_cancelled());
    tokio::time::timeout(Duration::from_secs(3), disabled).await.unwrap().unwrap().unwrap();
    assert_eq!(fixture.network.status().unwrap().active_requests, 0);
    assert_eq!(fixture.managed.hits.lock().unwrap().len(), 1);
    assert!(fixture.system.hits.lock().unwrap().is_empty());
    fixture.managed.finish_body.add_permits(1);
}

#[tokio::test]
async fn managed_kernel_failure_is_fail_closed_and_does_not_resolve_the_system_route() {
    let fixture = Fixture::new(false).await;
    fixture.kernel.running.store(false, Ordering::SeqCst);
    fixture.kernel.fail_start.store(true, Ordering::SeqCst);
    assert!(fixture.http.pool_post(ENDPOINT).await.is_err());
    assert_eq!(fixture.system_resolutions.load(Ordering::SeqCst), 0);
    assert!(fixture.managed.hits.lock().unwrap().is_empty());
    assert!(fixture.system.hits.lock().unwrap().is_empty());
    assert_eq!(fixture.network.status().unwrap().active_requests, 0);
    fixture.network.shutdown().await.unwrap();
}
