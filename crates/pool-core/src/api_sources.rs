//! API sources are independent from ChatGPT subscription accounts.
//! Only configuration, credential references and hashed context identities are stored.
use crate::{now, storage::Store, upstream_http::UpstreamHttp, vault::Vault, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::Duration,
};
use url::Url;

const DIRECTORY: &str = "api-sources/v1/directory";
const BINDINGS: &str = "api-sources/v1/context-bindings";
const MAX_SOURCES: usize = 64;
const CHECK_LIMIT: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ClientKind {
    Codex,
    ClaudeCode,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Protocol {
    Responses,
    Messages,
    ChatCompletions,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum NetworkMode {
    SystemProxy,
    Direct,
}

// Never implement Debug: an input can contain a newly supplied credential.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceInput {
    pub id: Option<String>,
    pub name: String,
    pub protocol: Protocol,
    pub base_url: String,
    pub model: String,
    pub network: NetworkMode,
    pub enabled: bool,
    pub api_key: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckResult {
    pub status: String,
    pub at: Option<i64>,
    pub message: String,
}
impl Default for CheckResult {
    fn default() -> Self {
        Self {
            status: "unknown".into(),
            at: None,
            message: "尚未验证".into(),
        }
    }
}
impl CheckResult {
    fn result(passed: bool, message: &str) -> Self {
        Self {
            status: if passed { "passed" } else { "failed" }.into(),
            at: Some(now()),
            message: message.into(),
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Verification {
    pub basic: CheckResult,
    pub test: CheckResult,
    pub stream: CheckResult,
    pub tools: CheckResult,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiSource {
    pub id: String,
    pub name: String,
    pub protocol: Protocol,
    pub base_url: String,
    pub model: String,
    pub network: NetworkMode,
    pub enabled: bool,
    pub has_key: bool,
    pub revision: u64,
    #[serde(default)]
    pub verification: Verification,
}
#[derive(Clone, Serialize, Deserialize)]
struct SavedSource {
    source: ApiSource,
    credential: String,
}
#[derive(Default, Serialize, Deserialize)]
struct Directory {
    #[serde(default)]
    sources: BTreeMap<String, SavedSource>,
    #[serde(default)]
    retired: BTreeSet<String>,
    // reference -> source id. Persist before removing a credential; failed deletion is retryable.
    #[serde(default)]
    cleanup: BTreeMap<String, String>,
}
pub struct Sources {
    store: Arc<Store>,
    vault: Arc<dyn Vault>,
    lock: Mutex<()>,
}
impl Sources {
    pub fn new(store: Arc<Store>, vault: Arc<dyn Vault>) -> Self {
        Self {
            store,
            vault,
            lock: Mutex::new(()),
        }
    }
    fn directory(&self) -> Result<Directory> {
        self.store
            .get(DIRECTORY)?
            .map(|s| serde_json::from_str(&s).map_err(|_| "API 来源目录损坏，未修改数据".into()))
            .unwrap_or_else(|| Ok(Directory::default()))
    }
    fn persist(&self, directory: &Directory) -> Result<()> {
        self.store.set(
            DIRECTORY,
            &serde_json::to_string(directory).map_err(|_| "API 来源编码失败")?,
        )
    }
    pub fn list(&self) -> Result<Vec<ApiSource>> {
        let _lock = self.lock.lock().map_err(|_| "API 来源锁异常")?;
        Ok(self
            .directory()?
            .sources
            .into_values()
            .map(|s| s.source)
            .collect())
    }
    pub fn logs(&self, client: ClientKind) -> Result<Vec<crate::api_gateway::ApiRequestLog>> {
        let _lock = self.lock.lock().map_err(|_| "API 请求记录锁异常")?;
        self.read_logs(client)
    }
    fn read_logs(&self, client: ClientKind) -> Result<Vec<crate::api_gateway::ApiRequestLog>> {
        self.store
            .get(log_key(client))?
            .map(|s| serde_json::from_str(&s).map_err(|_| "API 请求记录损坏".into()))
            .unwrap_or_else(|| Ok(Vec::new()))
    }
    pub(crate) fn record_log(&self, log: &crate::api_gateway::ApiRequestLog) -> Result<()> {
        let _lock = self.lock.lock().map_err(|_| "API 请求记录锁异常")?;
        let mut logs = self.read_logs(log.client)?;
        logs.retain(|previous| previous.id != log.id);
        logs.insert(0, log.clone());
        logs.truncate(2000);
        self.store.set(
            log_key(log.client),
            &serde_json::to_string(&logs).map_err(|_| "API 请求记录编码失败")?,
        )
    }
    pub fn get(&self, id: &str) -> Result<ApiSource> {
        let _lock = self.lock.lock().map_err(|_| "API 来源锁异常")?;
        self.directory()?
            .sources
            .remove(id)
            .map(|s| s.source)
            .ok_or_else(|| "API 来源不存在或已移除".into())
    }
    pub fn save(&self, mut input: SourceInput) -> Result<ApiSource> {
        let _lock = self.lock.lock().map_err(|_| "API 来源锁异常")?;
        input.name = input.name.trim().to_owned();
        if input.name.is_empty()
            || input.name.chars().count() > 80
            || input.name.chars().any(char::is_control)
        {
            return Err("来源名称应为 1–80 个可见字符".into());
        }
        input.base_url = validate_base_url(&input.base_url)?;
        if crate::usage::model_name(&Value::String(input.model.clone())).is_none() {
            return Err("请填写有效的默认模型名称".into());
        }
        if let Some(key) = &input.api_key {
            if key.is_empty()
                || key.len() > 16384
                || key.chars().any(char::is_whitespace)
                || http::HeaderValue::from_str(key).is_err()
            {
                return Err("API Key 为空或格式无效".into());
            }
        }
        let mut directory = self.directory()?;
        let previous = match input.id.as_deref() {
            Some(id) => Some(
                directory
                    .sources
                    .get(id)
                    .cloned()
                    .ok_or("API 来源不存在或已移除，请新增来源")?,
            ),
            None => None,
        };
        if previous.is_none() && directory.sources.len() >= MAX_SOURCES {
            return Err("API 来源数量已达上限".into());
        }
        let id = previous
            .as_ref()
            .map(|s| s.source.id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let changed = previous.as_ref().is_none_or(|p| {
            p.source.protocol != input.protocol
                || p.source.base_url != input.base_url
                || p.source.model != input.model
                || p.source.network != input.network
                || input.api_key.is_some()
        });
        let revision = match &previous {
            Some(p) if changed => p
                .source
                .revision
                .checked_add(1)
                .ok_or("API 来源版本已达上限")?,
            Some(p) => p.source.revision,
            None => 1,
        };
        let credential = if let Some(key) = &input.api_key {
            let reference = format!("api-source/{id}/{revision}-{}", uuid::Uuid::new_v4());
            // Register a prospective orphan first, so even an interrupted save is cleanable.
            directory.cleanup.insert(reference.clone(), id.clone());
            self.persist(&directory)?;
            self.vault.set(&reference, key)?;
            reference
        } else {
            previous
                .as_ref()
                .map(|s| s.credential.clone())
                .ok_or("新增来源必须提供 API Key")?
        };
        let source = ApiSource {
            id: id.clone(),
            name: input.name,
            protocol: input.protocol,
            base_url: input.base_url,
            model: input.model,
            network: input.network,
            enabled: input.enabled,
            has_key: true,
            revision,
            verification: if changed {
                Verification::default()
            } else {
                previous
                    .as_ref()
                    .map(|p| p.source.verification.clone())
                    .unwrap_or_default()
            },
        };
        if let Some(p) = &previous {
            if p.credential != credential {
                directory.cleanup.insert(p.credential.clone(), id.clone());
            }
        }
        directory.cleanup.remove(&credential);
        directory.sources.insert(
            id,
            SavedSource {
                source: source.clone(),
                credential,
            },
        );
        self.persist(&directory)?;
        // Saving the source succeeded. A stale credential cleanup is retained for later retry.
        let _ = self.cleanup(&mut directory);
        Ok(source)
    }
    fn cleanup(&self, directory: &mut Directory) -> Result<()> {
        let keys: Vec<_> = directory.cleanup.keys().cloned().collect();
        let mut failed = false;
        for reference in keys {
            if directory
                .sources
                .values()
                .any(|s| s.credential == reference)
            {
                continue;
            }
            if self.vault.delete(&reference).is_ok() {
                directory.cleanup.remove(&reference);
            } else {
                failed = true;
            }
        }
        self.persist(directory)?;
        if failed {
            Err("来源已保存，旧凭据清理未完成；后续保存或移除时可重试".into())
        } else {
            Ok(())
        }
    }
    pub fn remove(&self, id: &str) -> Result<()> {
        let _lock = self.lock.lock().map_err(|_| "API 来源锁异常")?;
        let mut directory = self.directory()?;
        if let Some(source) = directory.sources.remove(id) {
            directory.retired.insert(id.to_owned());
            directory.cleanup.insert(source.credential, id.to_owned());
            self.persist(&directory)?;
        } else if !directory.retired.contains(id) {
            return Err("API 来源不存在".into());
        }
        self.cleanup(&mut directory)
    }
    pub(crate) fn snapshot(&self, id: &str) -> Result<Snapshot> {
        let _lock = self.lock.lock().map_err(|_| "API 来源锁异常")?;
        let saved = self
            .directory()?
            .sources
            .remove(id)
            .ok_or("API 来源不存在或已移除")?;
        if !saved.source.enabled {
            return Err("API 来源已停用".into());
        }
        let key = self
            .vault
            .get(&saved.credential)?
            .ok_or("API 来源凭据缺失，请重新保存 Key")?;
        Ok(Snapshot {
            source: saved.source,
            key,
            http: UpstreamHttp::new()?,
            direct: direct_client()?,
        })
    }
    pub(crate) fn current(&self, id: &str, revision: u64) -> Result<()> {
        let source = self.get(id)?;
        if !source.enabled || source.revision != revision {
            return Err("此连接的来源已停用或变更，请在客户端连接中重新应用".into());
        }
        Ok(())
    }
    fn record_check(
        &self,
        id: &str,
        revision: u64,
        test: bool,
        result: &CheckResult,
    ) -> Result<()> {
        let _lock = self.lock.lock().map_err(|_| "API 来源锁异常")?;
        let mut directory = self.directory()?;
        let saved = directory
            .sources
            .get_mut(id)
            .ok_or("API 来源已移除，未写入检查结果")?;
        if saved.source.revision != revision {
            return Err("API 来源已变更，请重新检查".into());
        }
        if test {
            saved.source.verification.test = result.clone();
        } else {
            saved.source.verification.basic = result.clone();
        }
        self.persist(&directory)
    }
    pub async fn basic_check(&self, id: &str) -> Result<CheckResult> {
        self.check(id, false).await
    }
    pub async fn test_request(&self, id: &str) -> Result<CheckResult> {
        self.check(id, true).await
    }
    async fn check(&self, id: &str, test: bool) -> Result<CheckResult> {
        let snapshot = self.snapshot(id)?;
        let operation = async {
            let mut request = snapshot
                .request(
                    if test {
                        http::Method::POST
                    } else {
                        http::Method::GET
                    },
                    if test { snapshot.operation() } else { "models" },
                )
                .await?;
            if test {
                let body = match snapshot.source.protocol {
                    Protocol::Responses => {
                        json!({"model":snapshot.source.model,"input":"Reply OK.","max_output_tokens":16,"stream":false,"store":false})
                    }
                    Protocol::Messages => {
                        json!({"model":snapshot.source.model,"max_tokens":16,"stream":false,"messages":[{"role":"user","content":"Reply OK."}]})
                    }
                    Protocol::ChatCompletions => {
                        json!({"model":snapshot.source.model,"max_tokens":16,"stream":false,"messages":[{"role":"user","content":"Reply OK."}]})
                    }
                };
                request = request.json(&body);
            }
            let response = request
                .send()
                .await
                .map_err(|_| "连接或请求失败，未自动重试")?;
            if !response.status().is_success() {
                return Err("上游拒绝检查请求；请检查地址、模型、凭据与权限".into());
            }
            let mut stream = response.bytes_stream();
            use futures_util::StreamExt;
            let mut body = Vec::new();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|_| "检查响应读取失败，未自动重试")?;
                if body.len().saturating_add(chunk.len()) > CHECK_LIMIT {
                    return Err("检查响应超过大小限制".into());
                }
                body.extend_from_slice(&chunk);
            }
            let value: Value =
                serde_json::from_slice(&body).map_err(|_| "上游没有返回可验证的 JSON 检查结果")?;
            let valid = if !test {
                value["data"].is_array() || value["models"].is_array()
            } else {
                match snapshot.source.protocol {
                    Protocol::Responses => {
                        value["object"] == "response"
                            && value["output"].is_array()
                            && value["status"] == "completed"
                    }
                    Protocol::Messages => {
                        value["type"] == "message"
                            && value["content"].is_array()
                            && value["stop_reason"].is_string()
                    }
                    Protocol::ChatCompletions => {
                        value["choices"].as_array().is_some_and(|a| !a.is_empty())
                    }
                }
            };
            if !valid {
                return Err("检查返回结构不符合所选协议".into());
            }
            Ok::<(), String>(())
        };
        let checked = tokio::time::timeout(Duration::from_secs(60), operation).await;
        let result = match checked {
            Ok(Ok(())) => CheckResult::result(
                true,
                if test {
                    "最小请求通过；流式输出、工具调用与真实客户端仍待验证"
                } else {
                    "模型目录通过；不代表流式输出、工具调用或客户端可用"
                },
            ),
            Ok(Err(message)) => CheckResult::result(false, &message),
            Err(_) => CheckResult::result(false, "检查超时，未自动重试；上游执行与计费状态未知"),
        };
        self.record_check(id, snapshot.source.revision, test, &result)?;
        Ok(result)
    }
    // Bindings are global across API sources so a known reference cannot change ownership.
    // Only SHA-256 values are retained. Never store client IDs, signatures or response content.
    pub(crate) fn contexts(&self, scope: &str, references: &[String], bind: bool) -> Result<()> {
        if references.is_empty() {
            return Ok(());
        }
        let _lock = self.lock.lock().map_err(|_| "API 上下文锁异常")?;
        let mut bindings: BTreeMap<String, String> = self
            .store
            .get(BINDINGS)?
            .map(|s| serde_json::from_str(&s).map_err(|_| "API 上下文记录损坏".to_owned()))
            .transpose()?
            .unwrap_or_default();
        for reference in references {
            match bindings.get(reference) {
                Some(owner) if owner != scope => {
                    return Err("此会话或上下文属于其他来源修订，已阻止发送".into())
                }
                None if !bind => {
                    return Err("无法确认此上下文的原来源，已阻止发送；请新建会话".into())
                }
                _ => {}
            }
        }
        if bind && !references.is_empty() {
            if bindings.len().saturating_add(references.len()) > 100_000 {
                return Err("API 上下文记录已达上限，未继续执行".into());
            }
            for reference in references {
                bindings.insert(reference.clone(), scope.to_owned());
            }
            self.store.set(
                BINDINGS,
                &serde_json::to_string(&bindings).map_err(|_| "API 上下文编码失败")?,
            )?;
        }
        Ok(())
    }
}
fn log_key(client: ClientKind) -> &'static str {
    match client {
        ClientKind::Codex => "api-sources/v1/request-logs/codex",
        ClientKind::ClaudeCode => "api-sources/v1/request-logs/claudeCode",
    }
}
pub(crate) fn context_hash(kind: &str, value: &str) -> String {
    format!("{kind}:{:x}", Sha256::digest(value.as_bytes()))
}
pub(crate) fn validate_base_url(raw: &str) -> Result<String> {
    let mut url = Url::parse(raw.trim()).map_err(|_| "API Base URL 格式错误")?;
    let loopback = match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        None => false,
    };
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
        || url.port() == Some(0)
        || url.path_segments().is_some_and(|mut segments| {
            segments.any(|s| {
                s.eq_ignore_ascii_case("%2e%2e")
                    || s.contains("%2f")
                    || s.contains("%2F")
                    || s.contains("%5c")
                    || s.contains("%5C")
            })
        })
    {
        return Err(
            "API Base URL 必须使用 HTTPS（本机回环可用 HTTP），且不能包含凭据、查询或片段".into(),
        );
    }
    let path = url.path().trim_end_matches('/').to_owned();
    url.set_path(&path);
    Ok(url.as_str().trim_end_matches('/').to_owned())
}
fn direct_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(20))
        .pool_max_idle_per_host(0)
        .build()
        .map_err(|_| "API 网络客户端初始化失败".into())
}
// Private to the core crate; deliberately no Debug/Serialize.
pub(crate) struct Snapshot {
    pub source: ApiSource,
    key: String,
    http: UpstreamHttp,
    direct: reqwest::Client,
}
impl Snapshot {
    pub fn scope(&self) -> String {
        format!("{}/{}", self.source.id, self.source.revision)
    }
    pub fn operation(&self) -> &'static str {
        match self.source.protocol {
            Protocol::Responses => "responses",
            Protocol::Messages => "messages",
            Protocol::ChatCompletions => "chat/completions",
        }
    }
    pub async fn request(
        &self,
        method: http::Method,
        operation: &str,
    ) -> Result<reqwest::RequestBuilder> {
        let base = self.source.base_url.trim_end_matches('/');
        let endpoint = if base.ends_with("/v1") {
            format!("{base}/{operation}")
        } else {
            format!("{base}/v1/{operation}")
        };
        let request = match self.source.network {
            NetworkMode::Direct => self.direct.request(method, endpoint),
            NetworkMode::SystemProxy => self.http.request(method, &endpoint).await?.0,
        };
        Ok(match self.source.protocol {
            Protocol::Messages => request
                .header("x-api-key", &self.key)
                .header("anthropic-version", "2023-06-01"),
            _ => request.bearer_auth(&self.key),
        })
    }
}

#[cfg(test)]
mod tests;
