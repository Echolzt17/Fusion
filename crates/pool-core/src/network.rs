//! Dedicated account-pool transport. Node credentials never enter desktop DTOs.
mod parse;
mod persistence;
mod policy;
mod types;

use crate::{storage::Store, upstream_http::UpstreamHttp, vault::Vault, Result};
use futures_util::{stream, StreamExt};
pub use parse::{infer_region, parse_subscription};
use persistence::{Index, Persistence};
use serde_json::json;
use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex, Weak,
    },
    time::Duration,
};
use tokio::sync::{OwnedRwLockReadGuard, RwLock};
pub use types::*;

pub trait KernelBackend: Send + Sync {
    fn start(&self, config: &str) -> Result<KernelEndpoint>;
    fn stop(&self) -> Result<()>;
    fn running(&self) -> bool;
}
#[derive(Clone)]
pub struct KernelEndpoint {
    pub proxy_url: String,
    pub controller_url: String,
    pub instance_id: String,
}
#[derive(Clone)]
struct Runtime {
    endpoint: KernelEndpoint,
    password: String,
    secret: String,
}
struct State {
    index: Index,
    snapshot: Option<Snapshot>,
    runtime: Option<Runtime>,
    phase: String,
    error: Option<String>,
    pending_update: bool,
    last_switch_reason: Option<String>,
    probe_due: BTreeSet<String>,
    restart_used: bool,
    restart_failed: bool,
    last_refresh_attempt: i64,
}

pub struct Network {
    persistence: Persistence,
    kernel: Arc<dyn KernelBackend>,
    state: Mutex<State>,
    routes: Arc<RwLock<()>>,
    lifecycle: tokio::sync::Mutex<()>,
    mutation: tokio::sync::Mutex<()>,
    checks: tokio::sync::Mutex<()>,
    active: AtomicUsize,
    checking: AtomicBool,
    refreshing: AtomicBool,
    stopping: AtomicBool,
    background: AtomicBool,
    system_http: UpstreamHttp,
    controller: reqwest::Client,
    wake_checks: tokio::sync::Notify,
    checks_paused: AtomicUsize,
    check_cancel: Mutex<tokio_util::sync::CancellationToken>,
    probe_slots: tokio::sync::Semaphore,
    probe_locks: Mutex<std::collections::BTreeMap<String, Arc<NodeProbe>>>,
}
#[derive(Default)]
struct NodeProbe {
    lock: tokio::sync::Mutex<()>,
    generation: AtomicUsize,
}

// Every attempt and response body of one logical operation retain this lease.
// Dropping it is the only way to release a generation for replacement/stop.
pub struct RouteLease {
    network: Weak<Network>,
    node_id: String,
    reported: AtomicBool,
    _guard: OwnedRwLockReadGuard<()>,
}
impl RouteLease {
    pub fn record_business(&self, success: bool, network_failure: bool) {
        if self.node_id.is_empty() {
            return;
        }
        if self.reported.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(network) = self.network.upgrade() {
            if let Ok(mut state) = network.state.lock() {
                let health = state.index.health.entry(self.node_id.clone()).or_default();
                if success {
                    health.business_successes = health.business_successes.saturating_add(1);
                } else {
                    health.business_failures = health.business_failures.saturating_add(1);
                }
                // Only confirmed transport failures can influence node health;
                // account/auth/quota errors and upstream HTTP errors cannot.
                if network_failure {
                    state.probe_due.insert(self.node_id.clone());
                    network.wake_checks.notify_one();
                }
                let _ = network.persistence.save_index(&state.index);
            }
        }
    }
}
impl Drop for RouteLease {
    fn drop(&mut self) {
        if let Some(network) = self.network.upgrade() {
            network.active.fetch_sub(1, Ordering::SeqCst);
        }
    }
}
pub struct ManagedRoute {
    pub proxy_url: String,
    pub username: String,
    pub password: String,
    pub cache_key: String,
    pub lease: RouteLease,
}

impl Network {
    pub fn new(
        store: Arc<Store>,
        vault: Arc<dyn Vault>,
        directory: PathBuf,
        kernel: Arc<dyn KernelBackend>,
    ) -> Result<Arc<Self>> {
        let persistence = Persistence::new(store, vault, directory);
        let (index, snapshot) = persistence.load()?;
        let last_refresh_attempt = snapshot.as_ref().map_or(0, |s| s.updated_at);
        let controller = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(12))
            .build()
            .map_err(|_| "节点检查客户端初始化失败")?;
        Ok(Arc::new(Self {
            persistence,
            kernel,
            state: Mutex::new(State {
                index,
                snapshot,
                runtime: None,
                phase: "idle".into(),
                error: None,
                pending_update: false,
                last_switch_reason: None,
                probe_due: BTreeSet::new(),
                restart_used: false,
                restart_failed: false,
                last_refresh_attempt,
            }),
            routes: Arc::new(RwLock::new(())),
            lifecycle: tokio::sync::Mutex::new(()),
            mutation: tokio::sync::Mutex::new(()),
            checks: tokio::sync::Mutex::new(()),
            active: AtomicUsize::new(0),
            checking: AtomicBool::new(false),
            refreshing: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            background: AtomicBool::new(false),
            system_http: UpstreamHttp::new()?,
            controller,
            wake_checks: tokio::sync::Notify::new(),
            checks_paused: AtomicUsize::new(0),
            check_cancel: Mutex::new(tokio_util::sync::CancellationToken::new()),
            probe_slots: tokio::sync::Semaphore::new(4),
            probe_locks: Mutex::new(std::collections::BTreeMap::new()),
        }))
    }
    pub fn status(&self) -> Result<NetworkStatus> {
        let state = self.state.lock().map_err(|_| "专用网络状态不可用")?;
        let settings = &state.index.settings;
        let nodes = state
            .snapshot
            .as_ref()
            .map(|s| s.nodes.as_slice())
            .unwrap_or(&[]);
        let at = crate::now();
        Ok(NetworkStatus {
            settings: settings.clone(), kernel_running: self.kernel.running(), current_node_id: state.index.current_node_id.clone(),
            nodes: nodes.iter().map(|node| {
                let health = state.index.health.get(&node.id).cloned().unwrap_or_default();
                NodeView { id: node.id.clone(), name: node.name.clone(), region: policy::region(node, settings).into(), inferred_region: node.region.clone(), protocol: node.protocol.clone(),
                    health: health.status(at).into(), delay_ms: health.latency_ms, last_checked_at: health.checked_at, cooldown_until: health.cooldown_until,
                    failure_count: health.consecutive_failures, excluded: settings.excluded_node_ids.contains(&node.id), business_successes: health.business_successes, business_failures: health.business_failures }
            }).collect(),
            subscription_name: state.snapshot.as_ref().map(|s| s.label.clone()), subscription_updated_at: state.snapshot.as_ref().map(|s| s.updated_at),
            subscription_kind: state.snapshot.as_ref().map(|s| s.kind.clone()), pending_update: state.pending_update,
            phase: if self.refreshing.load(Ordering::SeqCst) { "refreshing".into() } else if self.checking.load(Ordering::SeqCst) { "checking".into() } else { state.phase.clone() },
            error: state.error.clone(), last_switch_reason: state.last_switch_reason.clone(), active_requests: self.active.load(Ordering::SeqCst),
            skipped_nodes: state.snapshot.as_ref().map_or(0, |s| s.excluded),
            import_warnings: state.snapshot.as_ref().filter(|s| s.excluded > 0).map(|s| vec![format!("已跳过 {} 个不支持、重复或配置不安全的节点；仅导入独立节点，不加载订阅规则、脚本或链式依赖。", s.excluded)]).unwrap_or_default(),
        })
    }
    pub async fn acquire(self: &Arc<Self>) -> Result<Option<ManagedRoute>> {
        self.acquire_pool()
            .await
            .map(|(route, _system_lease)| route)
    }
    pub async fn acquire_pool(
        self: &Arc<Self>,
    ) -> Result<(Option<ManagedRoute>, Option<RouteLease>)> {
        let guard = self.routes.clone().read_owned().await;
        if self.stopping.load(Ordering::SeqCst) {
            return Err("专用网络正在关闭，未发送请求".into());
        }
        {
            let state = self.state.lock().map_err(|_| "专用网络状态不可用")?;
            if !state.index.settings.enabled {
                self.active.fetch_add(1, Ordering::SeqCst);
                return Ok((
                    None,
                    Some(RouteLease {
                        network: Arc::downgrade(self),
                        node_id: String::new(),
                        reported: AtomicBool::new(false),
                        _guard: guard,
                    }),
                ));
            }
        }
        let runtime = self.ensure_runtime().await?;
        let mut selected = {
            let mut state = self.state.lock().map_err(|_| "专用网络状态不可用")?;
            self.choose(&mut state, "原节点不可用，已选择后续请求节点")?
                .ok_or("所选地区或节点当前不可用；已阻止改走系统代理或直连")?
        };
        for attempt in 0..3 {
            let needs_check = {
                let state = self.state.lock().map_err(|_| "专用网络状态不可用")?;
                state.probe_due.contains(&selected)
                    || state.index.health.get(&selected).is_none_or(|h| {
                        h.consecutive_failures > 0
                            || h.checked_at.is_none_or(|t| t + 60 <= crate::now())
                    })
            };
            if !needs_check {
                break;
            }
            let delay = self.probe(&runtime, &selected).await;
            let mut state = self.state.lock().map_err(|_| "专用网络状态不可用")?;
            if delay.is_some() {
                break;
            }
            let next = self.choose(&mut state, "节点连续三轮探测失败，已选择后续请求节点")?;
            if next.as_deref() == Some(&selected) || attempt == 2 {
                return Err("节点本轮健康复核失败，业务尚未发送；连续三轮失败后才切换".into());
            }
            selected = next.ok_or("所选范围暂无健康节点，业务尚未发送")?;
        }
        self.active.fetch_add(1, Ordering::SeqCst);
        Ok((
            Some(ManagedRoute {
                proxy_url: runtime.endpoint.proxy_url.clone(),
                username: username(&selected),
                password: runtime.password,
                cache_key: format!("{}:{selected}", runtime.endpoint.instance_id),
                lease: RouteLease {
                    network: Arc::downgrade(self),
                    node_id: selected,
                    reported: AtomicBool::new(false),
                    _guard: guard,
                },
            }),
            None,
        ))
    }
    fn choose(&self, state: &mut State, reason: &str) -> Result<Option<String>> {
        let nodes = state
            .snapshot
            .as_ref()
            .map(|s| s.nodes.as_slice())
            .unwrap_or(&[]);
        let selected = policy::select_node(
            nodes,
            &state.index.settings,
            &state.index.health,
            state.index.current_node_id.as_deref(),
            crate::now(),
        );
        if selected != state.index.current_node_id {
            state.index.current_node_id = selected.clone();
            state.last_switch_reason = Some(reason.into());
            self.persistence.save_index(&state.index)?;
        }
        Ok(selected)
    }
    async fn ensure_runtime(self: &Arc<Self>) -> Result<Runtime> {
        // Complete registration even if an individual request is cancelled
        // while the desktop backend is starting its child process.
        let network = self.clone();
        tokio::spawn(async move { network.ensure_runtime_inner().await })
            .await
            .map_err(|_| "专用内核启动任务异常")?
    }
    async fn ensure_runtime_inner(self: &Arc<Self>) -> Result<Runtime> {
        let _lifecycle = self.lifecycle.lock().await;
        {
            let state = self.state.lock().map_err(|_| "专用网络状态不可用")?;
            if let Some(runtime) = &state.runtime {
                if self.kernel.running() {
                    return Ok(runtime.clone());
                }
            }
        }
        {
            let mut state = self.state.lock().map_err(|_| "专用网络状态不可用")?;
            if state.restart_failed {
                return Err("专用内核启动失败或再次退出，请点击重试；业务不会自动重放".into());
            }
            if state.runtime.is_some() {
                if state.restart_used {
                    state.phase = "failed".into();
                    state.error = Some("专用内核再次退出，请点击重试；业务不会自动重放".into());
                    return Err(state.error.clone().unwrap_or_default());
                }
                state.restart_used = true;
            }
        }
        let password = uuid::Uuid::new_v4().to_string();
        let secret = uuid::Uuid::new_v4().to_string();
        let config = {
            let mut state = self.state.lock().map_err(|_| "专用网络状态不可用")?;
            let snapshot = state.snapshot.as_ref().ok_or("请先导入订阅")?;
            let proxies: Vec<_> = snapshot
                .nodes
                .iter()
                .map(StoredNode::proxy_config)
                .collect();
            let users: Vec<_> = snapshot
                .nodes
                .iter()
                .map(|n| json!({"username":username(&n.id),"password":password}))
                .collect();
            let mut rules: Vec<String> = snapshot
                .nodes
                .iter()
                .map(|n| format!("IN-USER,{},{}", username(&n.id), n.core_name()))
                .collect();
            rules.push("MATCH,REJECT".into());
            let value = json!({"allow-lan":false,"bind-address":"127.0.0.1","mode":"rule","log-level":"silent","ipv6":false,"find-process-mode":"off",
                "external-controller":"127.0.0.1:0","secret":secret,"profile":{"store-selected":false,"store-fake-ip":false},"geo-auto-update":false,
                "listeners":[{"name":"fusion-http","type":"http","listen":"127.0.0.1","port":0,"users":users}],"proxies":proxies,"rules":rules});
            state.phase = "starting".into();
            state.error = None;
            serde_yaml_ng::to_string(&value).map_err(|_| "专用网络配置生成失败")?
        };
        let backend = self.kernel.clone();
        let result = tokio::task::spawn_blocking(move || backend.start(&config))
            .await
            .map_err(|_| "专用内核启动任务失败")?;
        let mut state = self.state.lock().map_err(|_| "专用网络状态不可用")?;
        match result {
            Ok(endpoint) => {
                validate_endpoint(&endpoint.proxy_url)?;
                validate_endpoint(&endpoint.controller_url)?;
                let runtime = Runtime {
                    endpoint,
                    password,
                    secret,
                };
                state.runtime = Some(runtime.clone());
                state.phase = "running".into();
                state.error = None;
                Ok(runtime)
            }
            Err(error) => {
                state.runtime = None;
                state.restart_failed = true;
                state.phase = "failed".into();
                state.error = Some(error.clone());
                Err(error)
            }
        }
    }
    pub async fn set_enabled(self: &Arc<Self>, enabled: bool) -> Result<NetworkStatus> {
        let _mutation = self.mutation.lock().await;
        self.stopping.store(false, Ordering::SeqCst);
        {
            let mut state = self.state.lock().map_err(|_| "专用网络状态不可用")?;
            state.restart_failed = false;
            state.restart_used = false;
        }
        let _pause = self.pause_checks();
        let _barrier = self.routes.write().await;
        let index = {
            let state = self.state.lock().map_err(|_| "专用网络状态不可用")?;
            let mut index = state.index.clone();
            index.settings.enabled = enabled;
            policy::validate_settings(
                &index.settings,
                state
                    .snapshot
                    .as_ref()
                    .map(|s| s.nodes.as_slice())
                    .unwrap_or(&[]),
            )?;
            index
        };
        if enabled {
            self.ensure_runtime().await?;
        } else {
            self.stop_runtime().await?;
        }
        let save = self.persistence.save_index(&index);
        if let Err(error) = save {
            if enabled
                && !self
                    .state
                    .lock()
                    .map_err(|_| "专用网络状态不可用")?
                    .index
                    .settings
                    .enabled
            {
                let _ = self.stop_runtime().await;
            }
            return Err(error);
        }
        {
            let mut state = self.state.lock().map_err(|_| "专用网络状态不可用")?;
            state.index = index;
            state.phase = if enabled { "running" } else { "disabled" }.into();
            state.error = None;
        }
        self.start_background();
        self.status()
    }
    pub async fn save_settings(
        self: &Arc<Self>,
        settings: NetworkSettings,
    ) -> Result<NetworkStatus> {
        let _mutation = self.mutation.lock().await;
        let mut state = self.state.lock().map_err(|_| "专用网络状态不可用")?;
        if settings.enabled != state.index.settings.enabled {
            return Err("请通过专用网络开关启用或关闭".into());
        }
        policy::validate_settings(
            &settings,
            state
                .snapshot
                .as_ref()
                .map(|s| s.nodes.as_slice())
                .unwrap_or(&[]),
        )?;
        let mut index = state.index.clone();
        index.settings = settings;
        self.persistence.save_index(&index)?;
        state.index = index;
        self.choose(&mut state, "已应用节点或地区选择")?;
        drop(state);
        self.status()
    }
    pub async fn select_node(self: &Arc<Self>, id: String, fixed: bool) -> Result<NetworkStatus> {
        let _mutation = self.mutation.lock().await;
        let mut state = self.state.lock().map_err(|_| "专用网络状态不可用")?;
        let mut index = state.index.clone();
        index.settings.mode = if fixed {
            SelectionMode::Fixed
        } else {
            SelectionMode::Auto
        };
        index.settings.fixed_node_id = if fixed { Some(id.clone()) } else { None };
        policy::validate_settings(
            &index.settings,
            state
                .snapshot
                .as_ref()
                .map(|s| s.nodes.as_slice())
                .unwrap_or(&[]),
        )?;
        let nodes = state
            .snapshot
            .as_ref()
            .map(|s| s.nodes.as_slice())
            .unwrap_or(&[]);
        if !nodes.iter().any(|n| n.id == id) || index.settings.excluded_node_ids.contains(&id) {
            return Err("节点不存在或已被排除".into());
        }
        let node = nodes.iter().find(|n| n.id == id).ok_or("节点不存在")?;
        if !index
            .settings
            .allowed_regions
            .iter()
            .any(|r| r == policy::region(node, &index.settings))
            || (!fixed && policy::region(node, &index.settings) == "UNKNOWN")
        {
            return Err("请先在允许地区中加入该节点的国家或地区".into());
        }
        index.current_node_id = Some(id);
        self.persistence.save_index(&index)?;
        state.index = index;
        state.last_switch_reason = Some("手动选择后续请求节点".into());
        drop(state);
        self.status()
    }
    pub async fn retry(self: &Arc<Self>) -> Result<NetworkStatus> {
        let _barrier = self.routes.read().await;
        if !self
            .state
            .lock()
            .map_err(|_| "专用网络状态不可用")?
            .index
            .settings
            .enabled
        {
            return Err("请先启用专用网络".into());
        }
        self.stopping.store(false, Ordering::SeqCst);
        self.resume_checks();
        {
            let mut state = self.state.lock().map_err(|_| "专用网络状态不可用")?;
            state.restart_used = false;
            state.restart_failed = false;
        }
        self.ensure_runtime().await?;
        self.start_background();
        self.status()
    }
    async fn stop_runtime(&self) -> Result<()> {
        let _lifecycle = self.lifecycle.lock().await;
        let kernel = self.kernel.clone();
        tokio::task::spawn_blocking(move || kernel.stop())
            .await
            .map_err(|_| "专用内核关闭任务失败")??;
        self.state.lock().map_err(|_| "专用网络状态不可用")?.runtime = None;
        Ok(())
    }
    pub async fn shutdown(self: &Arc<Self>) -> Result<()> {
        self.stopping.store(true, Ordering::SeqCst);
        let _pause = self.pause_checks();
        self.wake_checks.notify_one();
        let _barrier = self.routes.write().await;
        self.stop_runtime().await
    }

    pub async fn import_subscription(
        self: &Arc<Self>,
        input: SubscriptionInput,
    ) -> Result<NetworkStatus> {
        let _mutation = self.mutation.lock().await;
        if self.stopping.load(Ordering::SeqCst) {
            return Err("Fusion 正在退出".into());
        }
        if input.url.is_some() == input.yaml.is_some() {
            return Err("请提供一个订阅地址或一份 YAML 内容".into());
        }
        let name = input
            .name
            .unwrap_or_else(|| "我的订阅".into())
            .trim()
            .to_owned();
        if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
            return Err("订阅名称应为 1–256 字节且不能含控制字符".into());
        }
        let url = input
            .url
            .as_deref()
            .map(persistence::validate_url)
            .transpose()?;
        let bytes = if let Some(url) = url.as_deref() {
            self.download(url, true).await?
        } else {
            input.yaml.unwrap_or_default().into_bytes()
        };
        let parsed = parse_subscription(&bytes)?;
        let snapshot = Snapshot {
            version: 1,
            label: name,
            updated_at: crate::now(),
            kind: if url.is_some() { "url" } else { "file" }.into(),
            nodes: parsed.nodes,
            excluded: parsed.excluded,
        };
        self.commit_snapshot(snapshot, url.as_deref()).await?;
        self.status()
    }
    pub async fn refresh_subscription(
        self: &Arc<Self>,
        system_proxy: bool,
    ) -> Result<NetworkStatus> {
        let _mutation = self.mutation.lock().await;
        if self.stopping.load(Ordering::SeqCst) {
            return Err("Fusion 正在退出".into());
        }
        let _flag = Flag::new(&self.refreshing).ok_or("订阅正在更新")?;
        let (url, label) = {
            let mut state = self.state.lock().map_err(|_| "专用网络状态不可用")?;
            let url = self
                .persistence
                .url(&state.index)?
                .ok_or("文件订阅需要重新导入 YAML")?;
            state.last_refresh_attempt = crate::now();
            (
                url,
                state
                    .snapshot
                    .as_ref()
                    .map(|s| s.label.clone())
                    .unwrap_or_else(|| "我的订阅".into()),
            )
        };
        let result = async {
            let bytes = self.download(&url, system_proxy).await?;
            let parsed = parse_subscription(&bytes)?;
            let snapshot = Snapshot {
                version: 1,
                label,
                updated_at: crate::now(),
                kind: "url".into(),
                nodes: parsed.nodes,
                excluded: parsed.excluded,
            };
            self.commit_snapshot(snapshot, Some(&url)).await
        }
        .await;
        if let Err(error) = result {
            self.state.lock().map_err(|_| "专用网络状态不可用")?.error = Some(error.clone());
            return Err(error);
        }
        drop(_flag);
        self.status()
    }
    async fn download(self: &Arc<Self>, url: &str, system_proxy: bool) -> Result<Vec<u8>> {
        // The initial download deliberately uses the existing explicit system
        // route. Refreshes use an already selected node; never call acquire while
        // holding the exclusive generation barrier.
        let route = if system_proxy {
            None
        } else {
            self.acquire().await?
        };
        let request = if let Some(route) = &route {
            let proxy = reqwest::Proxy::all(&route.proxy_url)
                .map_err(|_| "专用网络监听信息无效")?
                .basic_auth(&route.username, &route.password);
            reqwest::Client::builder()
                .no_proxy()
                .proxy(proxy)
                .retry(reqwest::retry::never())
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(15))
                .build()
                .map_err(|_| "订阅下载客户端初始化失败")?
                .get(url)
        } else {
            self.system_http.get(url).await?
        };
        let response = request
            .timeout(Duration::from_secs(60))
            .send()
            .await
            .map_err(|_| "订阅下载失败，已保留原订阅")?;
        if !response.status().is_success() {
            return Err("订阅服务器未返回成功结果，已保留原订阅".into());
        }
        if response
            .content_length()
            .is_some_and(|size| size > MAX_SUBSCRIPTION_BYTES as u64)
        {
            return Err("订阅超过 8 MiB 限制".into());
        }
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| "订阅读取中断，已保留原订阅")?;
            if bytes.len().saturating_add(chunk.len()) > MAX_SUBSCRIPTION_BYTES {
                return Err("订阅超过 8 MiB 限制".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
    async fn commit_snapshot(
        self: &Arc<Self>,
        snapshot: Snapshot,
        url: Option<&str>,
    ) -> Result<()> {
        {
            self.state
                .lock()
                .map_err(|_| "专用网络状态不可用")?
                .pending_update = true;
        }
        let _pending = Pending(self.clone());
        let _pause = self.pause_checks();
        let result = async {
            let _barrier = self.routes.write().await;
            if self.stopping.load(Ordering::SeqCst) {
                return Err("Fusion 正在退出".into());
            }
            let (old_index, old_snapshot, was_running) = {
                let state = self.state.lock().map_err(|_| "专用网络状态不可用")?;
                (
                    state.index.clone(),
                    state.snapshot.clone(),
                    self.kernel.running(),
                )
            };
            self.stop_runtime().await?;
            {
                let mut state = self.state.lock().map_err(|_| "专用网络状态不可用")?;
                let mut index = state.index.clone();
                index
                    .settings
                    .node_order
                    .retain(|id| snapshot.nodes.iter().any(|n| n.id == *id));
                index
                    .settings
                    .excluded_node_ids
                    .retain(|id| snapshot.nodes.iter().any(|n| n.id == *id));
                index
                    .settings
                    .region_overrides
                    .retain(|id, _| snapshot.nodes.iter().any(|n| n.id == *id));
                if index
                    .settings
                    .fixed_node_id
                    .as_ref()
                    .is_some_and(|id| !snapshot.nodes.iter().any(|n| n.id == *id))
                {
                    // A removed fixed node remains a fail-closed choice until the
                    // user selects another node, never silently changing to auto.
                    index.current_node_id = None;
                }
                index
                    .health
                    .retain(|id, _| snapshot.nodes.iter().any(|n| n.id == *id));
                index.current_node_id = policy::select_node(
                    &snapshot.nodes,
                    &index.settings,
                    &index.health,
                    index.current_node_id.as_deref(),
                    crate::now(),
                );
                state.index = index;
                state.snapshot = Some(snapshot.clone());
                state.error = None;
                state.phase = "idle".into();
                state.restart_used = false;
                state.restart_failed = false;
            }
            // Test a fully isolated candidate runtime before committing its
            // encrypted snapshot or retiring any previously usable material.
            let validation = self.ensure_runtime().await.map(|_| ());
            let validation = if validation.is_ok() && !old_index.settings.enabled {
                self.stop_runtime().await
            } else {
                validation
            };
            let commit = validation.and_then(|_| {
                let mut state = self.state.lock().map_err(|_| "专用网络状态不可用")?;
                let mut index = state.index.clone();
                self.persistence
                    .replace(&old_index, &mut index, &snapshot, url)?;
                state.index = index;
                if let Ok(mut locks) = self.probe_locks.lock() {
                    locks.retain(|id, _| snapshot.nodes.iter().any(|n| n.id == *id));
                }
                state.last_switch_reason = Some("订阅已更新，仅影响后续请求".into());
                Ok(())
            });
            if let Err(error) = commit {
                let _ = self.stop_runtime().await;
                {
                    let mut state = self.state.lock().map_err(|_| "专用网络状态不可用")?;
                    state.index = old_index;
                    state.snapshot = old_snapshot;
                    state.runtime = None;
                    state.restart_used = false;
                    state.restart_failed = false;
                    state.error = Some("新订阅未通过，已保留上一份配置".into());
                    state.phase = "failed".into();
                }
                if was_running {
                    let _ = self.ensure_runtime().await;
                }
                return Err(error);
            }
            Ok(())
        }
        .await;
        result
    }
    pub async fn remove_subscription(self: &Arc<Self>) -> Result<NetworkStatus> {
        let _mutation = self.mutation.lock().await;
        let _pause = self.pause_checks();
        let _barrier = self.routes.write().await;
        self.stop_runtime().await?;
        {
            let mut state = self.state.lock().map_err(|_| "专用网络状态不可用")?;
            let index = Index {
                version: 1,
                ..Index::default()
            };
            self.persistence.remove(&state.index, &index)?;
            state.index = index;
            state.snapshot = None;
            state.phase = "disabled".into();
            state.error = None;
            state.last_switch_reason = None;
        }
        self.status()
    }
    pub async fn check_nodes(self: &Arc<Self>, ids: Option<Vec<String>>) -> Result<NetworkStatus> {
        if self.checks_paused.load(Ordering::SeqCst) != 0 {
            return self.status();
        }
        let Ok(_check) = self.checks.try_lock() else {
            return self.status();
        };
        let _flag = Flag::new(&self.checking).ok_or("节点检查正在进行")?;
        let _barrier = self.routes.read().await;
        if self.stopping.load(Ordering::SeqCst) {
            return Err("Fusion 正在退出".into());
        }
        if self.checks_paused.load(Ordering::SeqCst) != 0 {
            return self.status();
        }
        let cancellation = self
            .check_cancel
            .lock()
            .map_err(|_| "节点检查状态不可用")?
            .clone();
        let nodes = {
            let state = self.state.lock().map_err(|_| "专用网络状态不可用")?;
            let snapshot = state.snapshot.as_ref().ok_or("请先导入订阅")?;
            if !state.index.settings.enabled {
                return Err("请先启用专用网络再检查节点".into());
            }
            if ids.as_ref().is_some_and(|ids| {
                ids.len() > MAX_NODES
                    || ids
                        .iter()
                        .any(|id| !snapshot.nodes.iter().any(|node| node.id == *id))
            }) {
                return Err("待检查节点列表无效".into());
            }
            snapshot
                .nodes
                .iter()
                .filter(|node| {
                    ids.as_ref().is_none_or(|ids| ids.contains(&node.id))
                        && !state.index.settings.excluded_node_ids.contains(&node.id)
                        && state
                            .index
                            .health
                            .get(&node.id)
                            .and_then(|h| h.cooldown_until)
                            .is_none_or(|until| until <= crate::now())
                })
                .map(|n| n.id.clone())
                .collect::<Vec<_>>()
        };
        if nodes.is_empty() {
            drop(_flag);
            return self.status();
        }
        let runtime = self.ensure_runtime().await?;
        let this = self.clone();
        let checks = stream::iter(nodes)
            .map(move |id| {
                let this = this.clone();
                let runtime = runtime.clone();
                async move {
                    let delay = this.probe(&runtime, &id).await;
                    (id, delay)
                }
            })
            .buffer_unordered(4);
        tokio::pin!(checks);
        loop {
            let result = tokio::select! { biased; _ = cancellation.cancelled() => break, result = checks.next() => result };
            let Some((id, delay)) = result else {
                break;
            };
            if self.stopping.load(Ordering::SeqCst) {
                break;
            }
            let mut state = self.state.lock().map_err(|_| "专用网络状态不可用")?;
            let _ = (id, delay); // probe records a coalesced round exactly once.
            self.choose(&mut state, "节点连续检查失败，已为后续请求切换")?;
            self.persistence.save_index(&state.index)?;
        }
        drop(_flag);
        self.status()
    }
    fn pause_checks(self: &Arc<Self>) -> CheckPause {
        self.checks_paused.fetch_add(1, Ordering::SeqCst);
        if let Ok(token) = self.check_cancel.lock() {
            token.cancel();
        }
        CheckPause(self.clone())
    }
    /// Cancel credential-free probes before taking an application-wide work
    /// freeze. Business route leases remain untouched and drain normally.
    pub fn pause_background_checks(self: &Arc<Self>) -> CheckPause {
        self.pause_checks()
    }
    fn resume_checks(&self) {
        if let Ok(mut token) = self.check_cancel.lock() {
            if self.checks_paused.load(Ordering::SeqCst) == 0
                && !self.stopping.load(Ordering::SeqCst)
            {
                *token = tokio_util::sync::CancellationToken::new();
            }
        }
    }
    async fn probe(&self, runtime: &Runtime, id: &str) -> Option<u64> {
        let cancellation = self.check_cancel.lock().ok()?.clone();
        let entry = self
            .probe_locks
            .lock()
            .ok()?
            .entry(id.into())
            .or_default()
            .clone();
        let generation = entry.generation.load(Ordering::SeqCst);
        let _node = tokio::select! { biased; _ = cancellation.cancelled() => return None, node = entry.lock.lock() => node };
        if entry.generation.load(Ordering::SeqCst) != generation {
            return self
                .state
                .lock()
                .ok()?
                .index
                .health
                .get(id)
                .and_then(|h| h.latency_ms);
        }
        let round = async {
            match self
                .probe_url(runtime, id, "https://www.gstatic.com/generate_204")
                .await
            {
                Some(delay) => Some(delay),
                None => {
                    self.probe_url(runtime, id, "https://cp.cloudflare.com/generate_204")
                        .await
                }
            }
        };
        let delay = tokio::select! { biased; _ = cancellation.cancelled() => return None, delay = round => delay };
        if let Ok(mut state) = self.state.lock() {
            state.probe_due.remove(id);
            state
                .index
                .health
                .entry(id.into())
                .or_default()
                .record(delay, crate::now());
            if self.persistence.save_index(&state.index).is_err() {
                state.error = Some("节点健康结果未能保存，请检查数据目录".into());
            }
        }
        entry.generation.fetch_add(1, Ordering::SeqCst);
        delay
    }
    async fn probe_url(&self, runtime: &Runtime, id: &str, target: &str) -> Option<u64> {
        let _slot = self.probe_slots.acquire().await.ok()?;
        let mut endpoint = url::Url::parse(&runtime.endpoint.controller_url).ok()?;
        endpoint
            .path_segments_mut()
            .ok()?
            .extend(["proxies", &format!("fusion_{id}"), "delay"]);
        let response = self
            .controller
            .get(endpoint)
            .bearer_auth(&runtime.secret)
            .query(&[("url", target), ("timeout", "5000"), ("expected", "204")])
            .send()
            .await
            .ok()?;
        if !response.status().is_success()
            || response.content_length().is_some_and(|n| n > 16 * 1024)
        {
            return None;
        }
        let mut bytes = Vec::new();
        let mut body = response.bytes_stream();
        while let Some(part) = body.next().await {
            let part = part.ok()?;
            if bytes.len() + part.len() > 16 * 1024 {
                return None;
            }
            bytes.extend_from_slice(&part);
        }
        let data: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        data["delay"].as_u64().filter(|n| *n < 60_000)
    }
    pub fn start_background(self: &Arc<Self>) {
        if self.background.swap(true, Ordering::SeqCst) {
            return;
        }
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut timer = tokio::time::interval(Duration::from_secs(HEALTH_INTERVAL_SECS));
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                let Some(network) = weak.upgrade() else {
                    return;
                };
                let scheduled = tokio::select! { _ = timer.tick() => true, _ = network.wake_checks.notified() => false };
                if network.stopping.load(Ordering::SeqCst) {
                    network.background.store(false, Ordering::SeqCst);
                    return;
                }
                let (enabled, refresh) = match network.state.lock() {
                    Ok(state) => (
                        state.index.settings.enabled,
                        state.index.url_key.is_some()
                            && state
                                .last_refresh_attempt
                                .saturating_add(REFRESH_INTERVAL_SECS)
                                <= crate::now(),
                    ),
                    Err(_) => return,
                };
                if enabled {
                    let ids = if scheduled {
                        Some(network.background_due_nodes())
                    } else {
                        network
                            .state
                            .lock()
                            .ok()
                            .map(|s| s.probe_due.iter().cloned().collect())
                    };
                    network.background_check(ids, Duration::from_secs(55)).await;
                }
                if refresh {
                    let _ = network.refresh_subscription(!enabled).await;
                }
            }
        });
    }
    async fn background_check(self: &Arc<Self>, ids: Option<Vec<String>>, budget: Duration) {
        // A large subscription cannot monopolize the scheduler: completed
        // rounds are already saved; dropping this future cancels only unfinished
        // credential-free probes, which remain due for the next priority pass.
        let _ = tokio::time::timeout(budget, self.check_nodes(ids)).await;
    }
    fn background_due_nodes(&self) -> Vec<String> {
        let Ok(state) = self.state.lock() else {
            return vec![];
        };
        let Some(snapshot) = &state.snapshot else {
            return vec![];
        };
        let settings = &state.index.settings;
        let current = state.index.current_node_id.as_deref();
        let current_region = snapshot
            .nodes
            .iter()
            .find(|n| Some(n.id.as_str()) == current)
            .map(|n| policy::region(n, settings));
        let mut candidates: Vec<_> = snapshot
            .nodes
            .iter()
            .filter(|n| {
                Some(n.id.as_str()) != current
                    && !settings.excluded_node_ids.contains(&n.id)
                    && settings
                        .allowed_regions
                        .iter()
                        .any(|r| r == policy::region(n, settings))
                    && policy::region(n, settings) != "UNKNOWN"
            })
            .collect();
        candidates.sort_by_key(|n| {
            (
                !current_region.is_some_and(|r| r == policy::region(n, settings)),
                settings
                    .allowed_regions
                    .iter()
                    .position(|r| r == policy::region(n, settings))
                    .unwrap_or(usize::MAX),
                settings
                    .node_order
                    .iter()
                    .position(|id| id == &n.id)
                    .unwrap_or(usize::MAX),
            )
        });
        let backups: BTreeSet<_> = candidates
            .into_iter()
            .take(2)
            .map(|n| n.id.as_str())
            .collect();
        let at = crate::now();
        let mut due: Vec<_> = snapshot
            .nodes
            .iter()
            .filter(|n| {
                if settings.excluded_node_ids.contains(&n.id) {
                    return false;
                }
                let interval = if Some(n.id.as_str()) == current {
                    60
                } else if backups.contains(n.id.as_str()) {
                    180
                } else {
                    900
                };
                state
                    .index
                    .health
                    .get(&n.id)
                    .and_then(|h| h.checked_at)
                    .is_none_or(|t| t.saturating_add(interval) <= at)
            })
            .collect();
        due.sort_by_key(|n| {
            (
                Some(n.id.as_str()) != current,
                !backups.contains(n.id.as_str()),
            )
        });
        due.into_iter().map(|n| n.id.clone()).collect()
    }
}

struct Flag<'a>(&'a AtomicBool);
impl<'a> Flag<'a> {
    fn new(flag: &'a AtomicBool) -> Option<Self> {
        (!flag.swap(true, Ordering::SeqCst)).then_some(Self(flag))
    }
}
impl Drop for Flag<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}
struct Pending(Arc<Network>);
impl Drop for Pending {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.state.lock() {
            state.pending_update = false;
        }
    }
}
pub struct CheckPause(Arc<Network>);
impl Drop for CheckPause {
    fn drop(&mut self) {
        // A shutdown pause can overlap an in-flight subscription mutation.
        // Publish a fresh token only after the last owner releases its pause,
        // under the same lock used by probes to obtain the current token.
        if let Ok(mut token) = self.0.check_cancel.lock() {
            if self.0.checks_paused.fetch_sub(1, Ordering::SeqCst) == 1
                && !self.0.stopping.load(Ordering::SeqCst)
            {
                *token = tokio_util::sync::CancellationToken::new();
            }
        } else {
            self.0.checks_paused.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

fn username(id: &str) -> String {
    format!("n_{id}")
}
fn validate_endpoint(raw: &str) -> Result<()> {
    let url = url::Url::parse(raw).map_err(|_| "专用内核监听信息无效")?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || url.port().is_none_or(|p| p == 0)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return Err("专用内核必须仅监听本机回环地址".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
