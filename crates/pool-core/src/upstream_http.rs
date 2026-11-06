//! All external AI traffic uses an explicit, current proxy. No implicit direct fallback.
use crate::{diagnostics::ProxySource, Result};
use reqwest::{Client, Method, RequestBuilder};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Semaphore;
use url::Url;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod managed_tests;
#[cfg(windows)]
mod windows;

const INVALID_PROXY: &str = "Fusion 无法解析当前代理配置，已阻止外部请求直连";
const MISSING_PROXY: &str =
    "Fusion 未检测到可用的外部服务代理，已阻止直连；开启系统代理后可直接重试";

// May contain a proxy credential supplied by the OS. Never implement Debug or serialize.
#[derive(Clone, PartialEq, Eq)]
struct Route {
    proxy: String,
    source: ProxySource,
}
type Resolver = Arc<dyn Fn(&Url) -> Result<Route> + Send + Sync>;
pub struct UpstreamHttp {
    local: Client,
    cached: Mutex<Option<(Route, Client, Client)>>,
    resolver: Resolver,
    resolving: Arc<Semaphore>,
    managed: Mutex<Option<Arc<crate::network::Network>>>,
    managed_cached: Mutex<Option<(String, Client, Client)>>,
}
impl UpstreamHttp {
    pub fn new() -> Result<Self> {
        Ok(Self {
            local: client_builder()
                .build()
                .map_err(|_| "网络客户端初始化失败")?,
            cached: Mutex::new(None),
            resolver: Arc::new(resolve),
            resolving: Arc::new(Semaphore::new(2)),
            managed: Mutex::new(None),
            managed_cached: Mutex::new(None),
        })
    }
    pub async fn get(&self, endpoint: &str) -> Result<RequestBuilder> {
        self.request(Method::GET, endpoint).await.map(|r| r.0)
    }
    pub async fn post(&self, endpoint: &str) -> Result<RequestBuilder> {
        self.request(Method::POST, endpoint).await.map(|r| r.0)
    }
    pub async fn request(
        &self,
        method: Method,
        endpoint: &str,
    ) -> Result<(RequestBuilder, ProxySource)> {
        self.gateway_request(method, endpoint).await.map(|(r, s, _)| (r, s))
    }
    /// Only the subscription pool receives this override. API-source clients
    /// and subscription bootstrap downloads keep their explicit system route.
    pub fn set_network(&self, network: Arc<crate::network::Network>) -> Result<()> {
        *self.managed.lock().map_err(|_| "专用网络状态不可用")? = Some(network);
        Ok(())
    }
    pub async fn pool_get(&self, endpoint: &str) -> Result<(RequestBuilder, Option<crate::network::RouteLease>)> {
        self.pool_gateway_request(Method::GET, endpoint).await.map(|(r, _, _, lease)| (r, lease))
    }
    pub async fn pool_post(&self, endpoint: &str) -> Result<(RequestBuilder, Option<crate::network::RouteLease>)> {
        self.pool_gateway_request(Method::POST, endpoint).await.map(|(r, _, _, lease)| (r, lease))
    }
    pub(crate) async fn pool_gateway_request(
        &self, method: Method, endpoint: &str,
    ) -> Result<(RequestBuilder, ProxySource, Client, Option<crate::network::RouteLease>)> {
        let url = Url::parse(endpoint).map_err(|_| "上游地址格式错误")?;
        if !matches!(url.scheme(), "http" | "https") { return Err("上游地址协议不受支持".into()); }
        let mut system_lease = None;
        if !loopback(&url) {
            let network = self.managed.lock().map_err(|_| "专用网络状态不可用")?.clone();
            if let Some(network) = network {
                let (managed, fallback_lease) = network.acquire_pool().await?;
                system_lease = fallback_lease;
                if let Some(route) = managed {
                    // Capture immutable per-node proxy credentials in BOTH clients.
                    // No selector is changed beneath a request during its backoff.
                    let mut cached = self.managed_cached.lock().map_err(|_| "专用代理客户端状态不可用")?;
                    if !cached.as_ref().is_some_and(|(key, _, _)| key == &route.cache_key) {
                        let proxy = reqwest::Proxy::all(&route.proxy_url).map_err(|_| INVALID_PROXY)?
                            .basic_auth(&route.username, &route.password);
                        let client = client_builder().proxy(proxy.clone()).pool_max_idle_per_host(0)
                            .tcp_keepalive(Duration::from_secs(30)).build().map_err(|_| "代理客户端初始化失败")?;
                        let retry = client_builder().proxy(proxy).pool_max_idle_per_host(0)
                            .tcp_keepalive(Duration::from_secs(30)).connect_timeout(Duration::from_secs(10))
                            .build().map_err(|_| "代理客户端初始化失败")?;
                        *cached = Some((route.cache_key, client, retry));
                    }
                    let (_, client, retry) = cached.as_ref().ok_or("专用代理客户端不可用")?;
                    return Ok((client.request(method, url), ProxySource::ManagedKernel, retry.clone(), Some(route.lease)));
                }
            }
        }
        self.gateway_request(method, endpoint).await.map(|(r, s, retry)| (r, s, retry, system_lease))
    }
    /// Both clients capture the same resolved route. Only the gateway may use
    /// the second client after a proven pre-dispatch connector failure.
    pub(crate) async fn gateway_request(
        &self, method: Method, endpoint: &str,
    ) -> Result<(RequestBuilder, ProxySource, Client)> {
        let url = Url::parse(endpoint).map_err(|_| "上游地址格式错误")?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err("上游地址协议不受支持".into());
        }
        if loopback(&url) {
            let retry = client_builder().connect_timeout(Duration::from_secs(10)).build()
                .map_err(|_| "网络客户端初始化失败")?;
            return Ok((self.local.request(method, url), ProxySource::LoopbackDirect, retry));
        }
        let resolver = self.resolver.clone();
        let permits = self.resolving.clone();
        let target = url.clone();
        // Native PAC resolution is blocking. Limit both its caller wait and outstanding tasks.
        let route = tokio::time::timeout(Duration::from_secs(10), async move {
            let permit = permits
                .acquire_owned()
                .await
                .map_err(|_| "代理解析已停止")?;
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                resolver(&target)
            })
            .await
            .map_err(|_| "代理解析任务失败")?
        })
        .await
        .map_err(|_| "Fusion 读取代理配置超时，已阻止外部请求直连")??;
        let mut cached = self.cached.lock().map_err(|_| "代理客户端状态不可用")?;
        if !cached
            .as_ref()
            .is_some_and(|(previous, _, _)| previous == &route)
        {
            let proxy = reqwest::Proxy::all(&route.proxy).map_err(|_| INVALID_PROXY)?;
            let client = client_builder()
                .proxy(proxy.clone())
                // Expired proxy keep-alive tunnels must not break a later turn.
                // Establish a fresh transport, never replay a business request.
                .pool_max_idle_per_host(0)
                .tcp_keepalive(Duration::from_secs(30))
                .build()
                .map_err(|_| "代理客户端初始化失败")?;
            let retry = client_builder().proxy(proxy).pool_max_idle_per_host(0)
                .tcp_keepalive(Duration::from_secs(30)).connect_timeout(Duration::from_secs(10))
                .build().map_err(|_| "代理客户端初始化失败")?;
            *cached = Some((route.clone(), client, retry));
        }
        let (_, client, retry) = cached.as_ref().ok_or("代理客户端不可用")?;
        Ok((client.request(method, url), route.source, retry.clone()))
    }
}
fn client_builder() -> reqwest::ClientBuilder {
    Client::builder()
        .no_proxy()
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
}
fn loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}
fn resolve(target: &Url) -> Result<Route> {
    // A current Windows user proxy wins over inherited/stale shell environment variables.
    #[cfg(windows)]
    if let Some(route) = windows::resolve(target)? {
        return Ok(route);
    }
    environment_route(target.scheme(), |name| std::env::var(name).ok())?
        .ok_or_else(|| MISSING_PROXY.into())
}
fn environment_route(scheme: &str, read: impl Fn(&str) -> Option<String>) -> Result<Option<Route>> {
    let keys = if scheme == "https" {
        ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"]
    } else {
        ["HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"]
    };
    for key in keys {
        if let Some(raw) = read(key).filter(|v| !v.trim().is_empty()) {
            return Ok(Some(Route {
                proxy: proxy_url(&raw, "http")?,
                source: ProxySource::EnvironmentConfigured,
            }));
        }
    }
    // NO_PROXY intentionally cannot send external AI requests directly.
    Ok(None)
}
fn proxy_url(raw: &str, default_scheme: &str) -> Result<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.eq_ignore_ascii_case("DIRECT") || raw.chars().any(char::is_whitespace) || raw.contains([';', ',']) {
        return Err(INVALID_PROXY.into());
    }
    let normalized = if raw.contains("://") {
        raw.to_owned()
    } else {
        format!("{default_scheme}://{raw}")
    };
    let mut url = Url::parse(&normalized).map_err(|_| INVALID_PROXY)?;
    if !matches!(
        url.scheme(),
        "http" | "https" | "socks4" | "socks4a" | "socks5" | "socks5h"
    ) || url.host_str().is_none()
        || url.port() == Some(0)
        || !matches!(url.path(), "" | "/")
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(INVALID_PROXY.into());
    }
    // SOCKS5 DNS belongs to the proxy, just as it does in the user's browser.
    if url.scheme() == "socks5" {
        url.set_scheme("socks5h").map_err(|_| INVALID_PROXY)?;
    }
    Ok(url.into())
}
fn static_proxy(raw: &str, scheme: &str) -> Result<String> {
    let mut specific = None;
    let mut fallback = None;
    for entry in raw.split(|c: char| c == ';' || c.is_whitespace()).filter(|s| !s.is_empty()) {
        if let Some((kind, value)) = entry.split_once('=') {
            if kind.eq_ignore_ascii_case(scheme) && specific.is_none() {
                specific = Some(proxy_url(value, "http")?);
            } else if kind.eq_ignore_ascii_case("socks") && fallback.is_none() {
                fallback = Some(proxy_url(value, "socks4")?);
            }
        } else if fallback.is_none() {
            fallback = Some(proxy_url(entry, "http")?);
        }
    }
    specific.or(fallback).ok_or_else(|| INVALID_PROXY.into())
}
