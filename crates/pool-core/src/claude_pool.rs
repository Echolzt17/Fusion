//! Independent Claude Code subscription identities. No Codex accounts or API keys.
//! Protocol constants/usage fields: Cockpit Tools v1.3.75 (MIT), see notices.
use crate::{
    api_gateway::ApiRequestLog,
    api_sources::{ApiSource, NetworkMode, Protocol, Snapshot, Verification},
    now,
    storage::Store,
    upstream_http::UpstreamHttp,
    vault::Vault,
    Result,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
pub mod login;

pub const SOURCE_ID: &str = "claude-subscription-pool";
const DIRECTORY: &str = "claude-pool/v1/directory";
const BINDINGS: &str = "claude-pool/v1/bindings";
const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
pub(crate) const BETA: &str = "oauth-2025-04-20";

// Secret type: intentionally neither Debug nor exposed through IPC.
#[derive(Clone, Serialize, Deserialize)]
struct Tokens {
    access: String,
    refresh: String,
    expires: i64,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: String,
    pub email: String,
    pub plan_label: String,
    pub state: String,
    pub quota: Option<Value>,
    pub quota_error: Option<String>,
    pub cooldown_until: i64,
    pub tokens: crate::usage::TokenUsage,
    pub measured_requests: u64,
    pub requests: u64,
    #[serde(default)]
    pub available: bool,
    #[serde(default)]
    identity: String,
    #[serde(default)]
    last_quota_attempt: i64,
    #[serde(default)]
    revision: u64,
}
#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub tokens: crate::usage::TokenUsage,
    pub requests: u64,
    pub measured_requests: u64,
}
#[derive(Serialize, Deserialize)]
struct Directory {
    version: u32,
    accounts: BTreeMap<String, Account>,
    preferred: Option<String>,
    #[serde(default)]
    summary: Summary,
}
impl Default for Directory {
    fn default() -> Self {
        Self {
            version: 1,
            accounts: BTreeMap::new(),
            preferred: None,
            summary: Summary::default(),
        }
    }
}
pub struct Pool {
    store: Arc<Store>,
    vault: Arc<dyn Vault>,
    http: UpstreamHttp,
    lock: Mutex<()>,
    token_locks: Mutex<BTreeMap<String, Arc<tokio::sync::Mutex<()>>>>,
    quotas: tokio::sync::Semaphore,
    token_tasks: Arc<tokio::sync::watch::Sender<usize>>,
    #[cfg(test)]
    upstream: String,
}
struct TokenTask(Arc<tokio::sync::watch::Sender<usize>>);
impl Drop for TokenTask {
    fn drop(&mut self) {
        self.0.send_modify(|n| *n -= 1);
    }
}
impl Pool {
    pub fn new(store: Arc<Store>, vault: Arc<dyn Vault>) -> Result<Arc<Self>> {
        let pool = Arc::new(Self {
            store,
            vault,
            http: UpstreamHttp::new()?,
            lock: Mutex::new(()),
            token_locks: Mutex::new(BTreeMap::new()),
            quotas: tokio::sync::Semaphore::new(2),
            token_tasks: Arc::new(tokio::sync::watch::channel(0).0),
            #[cfg(test)]
            upstream: "https://api.anthropic.com".into(),
        });
        pool.directory()?;
        Ok(pool)
    }
    fn directory(&self) -> Result<Directory> {
        let d: Directory = self
            .store
            .get(DIRECTORY)?
            .filter(|s| !s.is_empty())
            .map(|s| serde_json::from_str(&s).map_err(|_| "Claude 账号目录损坏"))
            .transpose()?
            .unwrap_or_default();
        if d.version != 1 {
            return Err("Claude 账号目录来自较新版本，请升级 Fusion".into());
        }
        Ok(d)
    }
    fn save(&self, d: &Directory) -> Result<()> {
        self.store.set(
            DIRECTORY,
            &serde_json::to_string(d).map_err(|_| "Claude 账号编码失败")?,
        )
    }
    pub fn accounts(&self) -> Result<Vec<Account>> {
        Ok(self
            .directory()?
            .accounts
            .into_values()
            .map(|mut a| {
                a.available = eligible(&a);
                a
            })
            .collect())
    }
    pub fn preferred(&self) -> Result<Option<String>> {
        Ok(self.directory()?.preferred)
    }
    pub fn summary(&self) -> Result<Summary> {
        Ok(self.directory()?.summary)
    }
    pub fn select(&self, id: Option<String>) -> Result<()> {
        let _g = self.lock.lock().map_err(|_| "Claude 账号锁异常")?;
        let mut d = self.directory()?;
        if id.as_ref().is_some_and(|id| !d.accounts.contains_key(id)) {
            return Err("Claude 账号不存在".into());
        }
        d.preferred = id;
        self.save(&d)
    }
    pub fn pause(&self, id: &str, paused: bool) -> Result<()> {
        let _g = self.lock.lock().map_err(|_| "Claude 账号锁异常")?;
        let mut d = self.directory()?;
        let a = d.accounts.get_mut(id).ok_or("Claude 账号不存在")?;
        if a.state == "reauth" || a.state == "deleting" {
            return Err("请先完成重新授权或移除".into());
        }
        a.state = if paused { "paused" } else { "ready" }.into();
        self.save(&d)
    }
    pub async fn remove(&self, id: &str) -> Result<()> {
        let lock = self.token_lock(id);
        let _token = lock.lock().await;
        let _g = self.lock.lock().map_err(|_| "Claude 账号锁异常")?;
        let mut d = self.directory()?;
        d.accounts.get_mut(id).ok_or("Claude 账号不存在")?.state = "deleting".into();
        self.save(&d)?;
        self.vault.delete(&format!("claude-pool/{id}"))?;
        d.accounts.remove(id);
        if d.preferred.as_deref() == Some(id) {
            d.preferred = None;
        }
        self.save(&d)
    }
    fn token_lock(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.token_locks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(id.into())
            .or_default()
            .clone()
    }
    async fn json(&self, url: &str, token: &str) -> Result<Value> {
        let r = self
            .http
            .get(url)
            .await?
            .bearer_auth(token)
            .header("anthropic-beta", BETA)
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .map_err(|_| "Claude 查询网络失败")?;
        if !r.status().is_success() {
            return Err(format!(
                "Claude 查询失败（HTTP {}），已保留上次结果",
                r.status().as_u16()
            ));
        }
        let b = r.bytes().await.map_err(|_| "Claude 查询读取中断")?;
        if b.len() > 1024 * 1024 {
            return Err("Claude 查询响应过大".into());
        }
        serde_json::from_slice(&b).map_err(|_| "Claude 查询格式无效".into())
    }
    async fn accept(&self, value: Value, expected: Option<String>) -> Result<()> {
        let tokens = parse_tokens(&value, None)?;
        let profile = self.json(PROFILE_URL, &tokens.access).await?;
        let account = profile.get("account").unwrap_or(&profile);
        let uuid = account["uuid"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() < 256)
            .ok_or("Claude 授权缺少账号身份")?;
        let org = profile["organization"]["uuid"].as_str().unwrap_or("");
        let identity = format!("{:x}", Sha256::digest(format!("{uuid}/{org}")));
        let id = self
            .directory()?
            .accounts
            .values()
            .find(|a| a.identity == identity)
            .map(|a| a.id.clone())
            .unwrap_or_else(|| format!("claude-{}", uuid::Uuid::new_v4().simple()));
        if expected.as_ref().is_some_and(|v| v != &id) {
            return Err("请重新授权原 Claude 账号；添加其他账号请使用添加账号".into());
        }
        let email = account["email"]
            .as_str()
            .filter(|s| s.len() < 320 && !s.chars().any(char::is_control))
            .unwrap_or("Claude 账号");
        let plan = profile_plan(&profile);
        let token_lock = self.token_lock(&id);
        let _t = token_lock.lock().await;
        let _g = self.lock.lock().map_err(|_| "Claude 账号锁异常")?;
        let mut d = self.directory()?;
        if d.accounts.get(&id).is_some_and(|a| a.state == "deleting") {
            return Err("Claude 账号正在移除".into());
        }
        if expected.is_some() && !d.accounts.contains_key(&id) {
            return Err("Claude 原账号已移除，已丢弃重新授权结果".into());
        }
        let id = if !d.accounts.contains_key(&id) {
            format!("claude-{}", uuid::Uuid::new_v4().simple())
        } else {
            id
        };
        self.vault.set(
            &format!("claude-pool/{id}"),
            &serde_json::to_string(&tokens).map_err(|_| "Claude 凭据编码失败")?,
        )?;
        let a = d.accounts.entry(id.clone()).or_insert_with(|| Account {
            id,
            email: email.into(),
            ..Default::default()
        });
        a.email = email.into();
        a.identity = identity;
        a.plan_label = plan;
        a.quota_error = None;
        a.cooldown_until = 0;
        a.revision = a.revision.saturating_add(1);
        if a.state != "paused" {
            a.state = "ready".into();
        }
        self.save(&d)
    }
    async fn token(self: &Arc<Self>, id: &str) -> Result<Tokens> {
        // A rotating refresh completes even when its request/IPC caller goes away.
        let pool = self.clone();
        let id = id.to_owned();
        self.token_tasks.send_modify(|n| *n += 1);
        let task = TokenTask(self.token_tasks.clone());
        tokio::spawn(async move {
            let _task = task;
            pool.token_inner(&id).await
        })
        .await
        .map_err(|_| "Claude 续期任务异常")?
    }
    pub async fn wait_idle(&self) -> Result<()> {
        let _quotas = self
            .quotas
            .acquire_many(2)
            .await
            .map_err(|_| "Claude 额度任务未完成")?;
        self.token_tasks
            .subscribe()
            .wait_for(|n| *n == 0)
            .await
            .map_err(|_| "Claude 续期任务未完成")?;
        Ok(())
    }
    async fn token_inner(&self, id: &str) -> Result<Tokens> {
        let token_lock = self.token_lock(id);
        let _t = token_lock.lock().await;
        let d = self.directory()?;
        let a = d.accounts.get(id).ok_or("Claude 账号已移除")?;
        if matches!(a.state.as_str(), "deleting" | "reauth") {
            return Err("Claude 账号需要重新授权或完成移除".into());
        }
        let old: Tokens = serde_json::from_str(
            &self
                .vault
                .get(&format!("claude-pool/{id}"))?
                .ok_or("Claude 凭据缺失，请重新授权")?,
        )
        .map_err(|_| "Claude 凭据损坏，请重新授权")?;
        if old.expires > now() + 300 {
            return Ok(old);
        }
        let r=self.http.post(TOKEN_URL).await?.timeout(Duration::from_secs(30)).json(&json!({"grant_type":"refresh_token","refresh_token":old.refresh,"client_id":CLIENT_ID})).send().await.map_err(|_|"Claude 授权续期网络失败；不会自动重试")?;
        if !r.status().is_success() {
            if matches!(r.status().as_u16(), 400 | 401) {
                self.failure(id, 401)?;
            }
            return Err("Claude 授权续期未完成；不会自动重试".into());
        }
        let v = r
            .json::<Value>()
            .await
            .map_err(|_| "Claude 续期响应格式错误")?;
        let t = parse_tokens(&v, Some(&old))?;
        self.vault.set(
            &format!("claude-pool/{id}"),
            &serde_json::to_string(&t).map_err(|_| "Claude 凭据编码失败")?,
        )?;
        Ok(t)
    }
    pub async fn refresh(self: &Arc<Self>, id: &str) -> Result<()> {
        let _slot = self
            .quotas
            .acquire()
            .await
            .map_err(|_| "Claude 额度查询已关闭")?;
        let revision = {
            let _g = self.lock.lock().map_err(|_| "Claude 账号锁异常")?;
            let mut d = self.directory()?;
            let a = d.accounts.get_mut(id).ok_or("Claude 账号不存在")?;
            a.last_quota_attempt = now();
            let revision = a.revision;
            self.save(&d)?;
            revision
        };
        let result = async {
            let t = self.token(id).await?;
            let v = self.json(USAGE_URL, &t.access).await?;
            quota(&v)
        }
        .await;
        let _g = self.lock.lock().map_err(|_| "Claude 账号锁异常")?;
        let mut d = self.directory()?;
        let a = d
            .accounts
            .get_mut(id)
            .ok_or("Claude 账号已移除，已丢弃查询结果")?;
        if a.state == "deleting" || a.revision != revision {
            return Err("Claude 账号正在移除".into());
        }
        match &result {
            Ok(q) => {
                a.quota = Some(q.clone());
                a.quota_error = None;
            }
            Err(e) => a.quota_error = Some(e.clone()),
        }
        self.save(&d)?;
        result.map(|_| ())
    }
    pub(crate) fn source(id: &str) -> ApiSource {
        ApiSource {
            id: id.into(),
            name: "Claude 订阅账号池".into(),
            protocol: Protocol::Messages,
            base_url: "https://api.anthropic.com".into(),
            model: "sonnet".into(),
            network: NetworkMode::SystemProxy,
            enabled: true,
            has_key: true,
            revision: 1,
            verification: Verification::default(),
        }
    }
    pub fn client_source() -> ApiSource {
        Self::source(SOURCE_ID)
    }
    pub(crate) async fn snapshot(self: &Arc<Self>, id: &str) -> Result<Snapshot> {
        let t = self.token(id).await?;
        #[allow(unused_mut)]
        let mut source = Self::source(id);
        #[cfg(test)]
        {
            source.base_url = self.upstream.clone();
            source.network = NetworkMode::Direct;
        }
        Snapshot::claude(source, t.access)
    }
    pub fn quota_due(&self, id: &str) -> Result<bool> {
        Ok(self
            .directory()?
            .accounts
            .get(id)
            .is_some_and(|a| now() - a.last_quota_attempt >= 600))
    }
    pub(crate) fn placeholder() -> Result<Snapshot> {
        Snapshot::claude(Self::source(SOURCE_ID), String::new())
    }
    pub fn ready(&self) -> Result<bool> {
        Ok(self.accounts()?.iter().any(eligible))
    }
    pub(crate) fn bind(
        &self,
        value: &Value,
        refs: &[String],
        require_session: bool,
    ) -> Result<String> {
        let _g = self.lock.lock().map_err(|_| "Claude 账号锁异常")?;
        let d = self.directory()?;
        let mut bindings: BTreeMap<String, String> = self
            .store
            .get(BINDINGS)?
            .map(|s| serde_json::from_str(&s).map_err(|_| "Claude 会话绑定损坏"))
            .transpose()?
            .unwrap_or_default();
        let session = session(value)?;
        let mut keys = refs.to_vec();
        if let Some(s) = &session {
            keys.push(s.clone());
        }
        let assigned: std::collections::BTreeSet<_> = keys
            .iter()
            .filter_map(|k| bindings.get(k))
            .cloned()
            .collect();
        if assigned.len() > 1 {
            return Err("Claude 上下文包含不同账号，未发送请求".into());
        }
        if refs.iter().any(|r| !bindings.contains_key(r)) {
            return Err("Claude 上下文原账号无法确认，未发送请求".into());
        }
        let id = if let Some(id) = assigned.into_iter().next() {
            let a = d.accounts.get(&id).ok_or("Claude 原会话账号已移除")?;
            if !eligible(a) {
                return Err("Claude 原会话账号当前不可用；不能切换到其他账号".into());
            }
            id
        } else {
            if require_session && session.is_none() {
                return Err("Claude 请求缺少可靠的会话标识，未发送请求".into());
            }
            // New sessions only. Never reselect after an upstream attempt.
            d.preferred
                .as_ref()
                .and_then(|id| d.accounts.get(id))
                .filter(|a| eligible(a))
                .or_else(|| d.accounts.values().find(|a| eligible(a)))
                .ok_or("没有可用的 Claude 订阅账号")?
                .id
                .clone()
        };
        if let Some(s) = session {
            bindings.insert(s, id.clone());
        }
        if bindings.len() > 50000 {
            return Err("Claude 会话绑定已达到上限；保留现有绑定".into());
        }
        self.store.set(
            BINDINGS,
            &serde_json::to_string(&bindings).map_err(|_| "Claude 会话绑定编码失败")?,
        )?;
        Ok(id)
    }
    pub(crate) fn bind_output(&self, id: &str, refs: &[String]) -> Result<()> {
        let _g = self.lock.lock().map_err(|_| "Claude 账号锁异常")?;
        let mut b: BTreeMap<String, String> = self
            .store
            .get(BINDINGS)?
            .map(|s| serde_json::from_str(&s).map_err(|_| "Claude 会话绑定损坏"))
            .transpose()?
            .unwrap_or_default();
        if b.len().saturating_add(refs.len()) > 50000 {
            return Err("Claude 会话绑定已达上限".into());
        }
        for r in refs {
            if b.get(r).is_some_and(|v| v != id) {
                return Err("Claude 上下文账号冲突".into());
            }
            b.insert(r.clone(), id.into());
        }
        self.store.set(
            BINDINGS,
            &serde_json::to_string(&b).map_err(|_| "Claude 会话绑定编码失败")?,
        )
    }
    pub(crate) fn failure(&self, id: &str, status: u16) -> Result<()> {
        let _g = self.lock.lock().map_err(|_| "Claude 账号锁异常")?;
        let mut d = self.directory()?;
        if let Some(a) = d.accounts.get_mut(id) {
            if status == 401 {
                a.state = "reauth".into();
            } else if status == 429 {
                a.cooldown_until = now() + 60;
            }
            self.save(&d)?;
        }
        Ok(())
    }
    pub(crate) fn record(&self, log: &ApiRequestLog) -> Result<()> {
        let _g = self.lock.lock().map_err(|_| "Claude 账号锁异常")?;
        let mut d = self.directory()?;
        if let Some(a) = d.accounts.get_mut(&log.source_id) {
            a.requests = a.requests.saturating_add(1);
            if let Some(u) = &log.usage {
                a.measured_requests = a.measured_requests.saturating_add(1);
                a.tokens.input_tokens = a.tokens.input_tokens.saturating_add(u.input_tokens);
                a.tokens.output_tokens = a.tokens.output_tokens.saturating_add(u.output_tokens);
                a.tokens.total_tokens = a.tokens.total_tokens.saturating_add(u.total_tokens);
            }
            d.summary.requests = d.summary.requests.saturating_add(1);
            if let Some(u) = &log.usage {
                d.summary.measured_requests = d.summary.measured_requests.saturating_add(1);
                d.summary.tokens.input_tokens =
                    d.summary.tokens.input_tokens.saturating_add(u.input_tokens);
                d.summary.tokens.output_tokens = d
                    .summary
                    .tokens
                    .output_tokens
                    .saturating_add(u.output_tokens);
                d.summary.tokens.total_tokens =
                    d.summary.tokens.total_tokens.saturating_add(u.total_tokens);
            }
            self.save(&d)?;
        }
        Ok(())
    }
}
fn parse_tokens(v: &Value, old: Option<&Tokens>) -> Result<Tokens> {
    let access = v["access_token"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("Claude 授权缺少访问凭据")?
        .to_owned();
    let refresh = v["refresh_token"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| old.map(|t| t.refresh.clone()))
        .ok_or("Claude 授权缺少续期凭据")?;
    let seconds = v["expires_in"]
        .as_i64()
        .filter(|n| *n > 0 && *n <= 365 * 86400)
        .ok_or("Claude 授权有效期无效")?;
    Ok(Tokens {
        access,
        refresh,
        expires: now() + seconds,
    })
}
fn profile_plan(v: &Value) -> String {
    let org = &v["organization"];
    for raw in [
        &org["subscription_type"],
        &v["subscription_type"],
        &org["organization_type"],
        &org["rate_limit_tier"],
    ] {
        let plan = raw.as_str().unwrap_or("").trim().to_ascii_lowercase();
        let label = match plan.as_str() {
            "free" | "claude_free" => Some("Free"),
            "pro" | "claude_pro" | "default_claude_pro" => Some("Pro"),
            "max" | "claude_max" => Some("Max"),
            "team" | "claude_team" => Some("Team"),
            "enterprise" | "claude_enterprise" => Some("Enterprise"),
            _ if plan.starts_with("default_claude_max_") => Some("Max"),
            _ => None,
        };
        if let Some(label) = label {
            return label.into();
        }
    }
    if v["account"]["has_claude_max"].as_bool() == Some(true)
        || org["is_claude_max"].as_bool() == Some(true)
    {
        "Max"
    } else if v["account"]["has_claude_pro"].as_bool() == Some(true)
        || org["is_claude_pro"].as_bool() == Some(true)
    {
        "Pro"
    } else {
        "套餐待确认"
    }
    .into()
}
fn quota(v: &Value) -> Result<Value> {
    fn window(v: &Value, seconds: i64) -> Option<Value> {
        let used = v["utilization"]
            .as_f64()
            .filter(|n| n.is_finite() && (0.0..=100.0).contains(n))?;
        let reset = v["resets_at"]
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.timestamp());
        Some(json!({"used_percent":used,"limit_window_seconds":seconds,"reset_at":reset}))
    }
    let p = window(&v["five_hour"], 18000);
    let s = window(&v["seven_day"], 604800);
    if p.is_none() && s.is_none() {
        return Err("Claude 未提供可用的额度周期；不代表额度为零".into());
    }
    Ok(json!({"rateLimit":{"primary_window":p,"secondary_window":s},"updatedAt":now()}))
}
fn eligible(a: &Account) -> bool {
    a.state == "ready"
        && a.cooldown_until <= now()
        && !a.quota.as_ref().is_some_and(|q| {
            ["primary_window", "secondary_window"].iter().any(|w| {
                let v = &q["rateLimit"][w];
                v["used_percent"].as_f64().is_some_and(|n| n >= 100.0)
                    && v["reset_at"].as_i64().is_none_or(|at| at > now())
            })
        })
}
fn session(v: &Value) -> Result<Option<String>> {
    let raw = v["metadata"]["user_id"].as_str().unwrap_or("");
    if raw.len() > 4096 {
        return Err("Claude 会话标识无效".into());
    }
    let s = serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|v| v["session_id"].as_str().map(str::to_owned))
        .or_else(|| raw.rsplit_once("_session_").map(|(_, s)| s.to_owned()));
    Ok(s.filter(|s| !s.is_empty() && s.len() <= 256)
        .map(|s| crate::api_sources::context_hash("claude-session", &s)))
}

#[cfg(test)]
mod tests;
