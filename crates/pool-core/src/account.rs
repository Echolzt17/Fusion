use crate::{now, storage::Store, vault::Vault, Account, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
mod quota;
pub use quota::QUOTA_REFRESH_INTERVAL_SECS;
#[cfg(test)]
mod deletion_tests;

// Never derive Debug or expose this type through the desktop IPC.
#[derive(Clone, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    pub id_token: String,
    pub expires_at: i64,
}

pub fn claims(token: &str) -> Result<Value> {
    let part = token.split('.').nth(1).ok_or("授权响应缺少账号信息")?;
    serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(part)
            .map_err(|_| "授权信息编码错误")?,
    )
    .map_err(|_| "授权信息格式错误".into())
}
pub fn tokens_from_response(v: &Value, old: Option<&Tokens>) -> Result<Tokens> {
    let field = |k: &str| v[k].as_str().filter(|s| !s.is_empty()).map(str::to_owned);
    let access_token = field("access_token").ok_or("授权响应缺少访问凭据")?;
    let refresh_token = field("refresh_token")
        .or_else(|| old.map(|t| t.refresh_token.clone()))
        .ok_or("授权响应缺少续期凭据")?;
    let id_token = field("id_token")
        .or_else(|| old.map(|t| t.id_token.clone()))
        .ok_or("授权响应缺少账号信息")?;
    let expires_at = v["expires_in"]
        .as_i64()
        .map(|n| now() + n)
        .or_else(|| claims(&access_token).ok().and_then(|c| c["exp"].as_i64()))
        .unwrap_or(now() + 3600);
    Ok(Tokens {
        access_token,
        refresh_token,
        id_token,
        expires_at,
    })
}

pub struct Accounts {
    pub store: Arc<Store>,
    pub vault: Arc<dyn Vault>,
    pub http: crate::upstream_http::UpstreamHttp,
    pub local_http: reqwest::Client,
    token_endpoint: String,
    pub(crate) reset_endpoint: String,
    pub(crate) reset_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    quota_endpoint: String,
    quota_runtime: Mutex<HashMap<String, quota::Refresh>>,
    quota_slots: tokio::sync::Semaphore,
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    token_tasks: Arc<tokio::sync::watch::Sender<usize>>,
}
struct TokenTask(Arc<tokio::sync::watch::Sender<usize>>);
impl Drop for TokenTask {
    fn drop(&mut self) { self.0.send_modify(|n| *n -= 1); }
}
impl Accounts {
    pub fn new(store: Arc<Store>, vault: Arc<dyn Vault>) -> Result<Self> {
        let http = crate::upstream_http::UpstreamHttp::new()?;
        // Never send the local gateway key to an environment or system proxy.
        let local_http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "本地网络客户端初始化失败")?;
        Ok(Self {
            store,
            vault,
            http,
            local_http,
            token_endpoint: TOKEN_URL.into(),
            reset_endpoint: "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits".into(),
            reset_locks: Mutex::new(HashMap::new()),
            quota_endpoint: "https://chatgpt.com/backend-api/wham/usage".into(),
            quota_runtime: Mutex::new(HashMap::new()),
            quota_slots: tokio::sync::Semaphore::new(2),
            locks: Mutex::new(HashMap::new()),
            token_tasks: Arc::new(tokio::sync::watch::channel(0).0),
        })
    }
    #[cfg(test)]
    pub(crate) fn use_test_token_endpoint(&mut self, url: String) {
        self.token_endpoint = url;
    }
    pub fn lock(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.locks
            .lock()
            .unwrap()
            .entry(id.into())
            .or_default()
            .clone()
    }
    pub async fn accept_login(&self, tokens: Tokens, expected: Option<&str>) -> Result<Account> {
        let c = claims(&tokens.id_token)?;
        let auth = &c["https://api.openai.com/auth"];
        let account_id = auth["chatgpt_account_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("账号信息中缺少 ChatGPT 账号 ID")?
            .to_owned();
        let email = c["email"].as_str().unwrap_or("已授权账号").to_owned();
        let id = format!("{:x}", Sha256::digest(account_id.as_bytes()));
        if expected.is_some_and(|e| e != id) {
            return Err("登录了不同账号；请重新授权原账号，或用添加账号操作".into());
        }
        let lock = self.lock(&id);
        let _guard = lock.lock().await;
        let previous = self.store.account(&id).ok();
        if previous.as_ref().is_some_and(|a| a.state == "deleting") {
            return Err("账号正在移除，请先重试完成移除再添加".into());
        }
        let a = Account {
            id: id.clone(),
            email,
            account_id,
            state: if previous.as_ref().is_some_and(|a| a.state == "paused") {
                "paused"
            } else {
                "ready"
            }
            .into(),
            cooldown_until: 0,
            quota: previous.and_then(|a| a.quota),
            quota_error: None,
        };
        self.vault.set(
            &format!("account/{id}"),
            &serde_json::to_string(&tokens).map_err(|_| "凭据编码失败")?,
        )?;
        self.store.save_account(&a)?;
        Ok(a)
    }
    pub async fn token(self: &Arc<Self>, id: &str) -> Result<Tokens> {
        // Credential rotation must finish even if the client disconnects while refreshing.
        let this = self.clone();
        let id = id.to_owned();
        self.token_tasks.send_modify(|n| *n += 1);
        let task = TokenTask(self.token_tasks.clone());
        tokio::spawn(async move { let _task = task; this.token_inner(&id).await })
            .await
            .map_err(|_| "授权续期任务异常")?
    }
    /// Call after stopping new background/IPC work and draining the gateway.
    /// Detached refreshes retain their task registration even when their caller is cancelled.
    pub async fn wait_background_idle(&self) -> Result<()> {
        let pending: Vec<_> = self.quota_runtime.lock().unwrap().values()
            .filter_map(|state| state.pending.clone()).collect();
        for mut receiver in pending {
            receiver.wait_for(|result| result.is_some()).await.map_err(|_| "额度查询尚未安全结束")?;
        }
        self.token_tasks.subscribe().wait_for(|count| *count == 0).await
            .map_err(|_| "授权续期尚未安全结束")?;
        Ok(())
    }
    async fn token_inner(&self, id: &str) -> Result<Tokens> {
        let lock = self.lock(id);
        let _guard = lock.lock().await;
        let mut a = self.store.account(id)?;
        if a.state == "deleting" {
            return Err("账号正在移除，请重试移除".into());
        }
        if a.state == "reauth" {
            return Err("账号需要重新授权".into());
        }
        let raw = self
            .vault
            .get(&format!("account/{id}"))?
            .ok_or("账号凭据缺失，请重新授权")?;
        let old: Tokens = serde_json::from_str(&raw).map_err(|_| "账号凭据损坏，请重新授权")?;
        if old.expires_at > now() + 300 {
            return Ok(old);
        }
        let r = self.http.post(&self.token_endpoint).await?.timeout(std::time::Duration::from_secs(30))
            .json(&json!({"client_id":CLIENT_ID,"grant_type":"refresh_token","refresh_token":old.refresh_token})).send().await.map_err(|_| "授权续期网络失败，请检查网络后再试")?;
        if !r.status().is_success() {
            if r.status().as_u16() == 400 || r.status().as_u16() == 401 {
                a.state = "reauth".into();
                self.store.save_account(&a)?;
                return Err("授权已失效，请重新登录账号".into());
            }
            return Err("授权服务器暂时不可用，请稍后再试".into());
        }
        let t = tokens_from_response(
            &r.json::<Value>()
                .await
                .map_err(|_| "授权续期响应格式错误")?,
            Some(&old),
        )?;
        self.vault.set(
            &format!("account/{id}"),
            &serde_json::to_string(&t).unwrap(),
        )?;
        Ok(t)
    }
    pub async fn set_state(&self, id: &str, paused: bool) -> Result<()> {
        let lock = self.lock(id);
        let _guard = lock.lock().await;
        let mut a = self.store.account(id)?;
        if a.state == "deleting" {
            return Err("账号正在移除，请重试移除".into());
        }
        if !paused && a.state == "reauth" {
            return Err("请先重新授权该账号".into());
        }
        a.state = if paused { "paused" } else { "ready" }.into();
        self.store.save_account(&a)
    }
    pub(crate) async fn remove(&self, id: &str) -> Result<()> {
        let lock = self.lock(id);
        let _guard = lock.lock().await;
        // Durable intent precedes any irreversible Vault work. A crash or failure
        // leaves an unschedulable, visible record whose deletion can be retried.
        let mut a = self.store.account(id)?;
        if a.state != "deleting" {
            a.state = "deleting".into();
            self.store.save_account(&a)?;
        }
        self.vault.delete(&format!("account/{id}"))?;
        self.store.delete_account(id)?;
        self.quota_runtime.lock().unwrap().remove(id);
        Ok(())
    }
    pub async fn mark_failure(&self, id: &str, status: u16, retry_after: i64) -> Result<()> {
        let lock = self.lock(id);
        let _guard = lock.lock().await;
        let mut a = self.store.account(id)?;
        if status == 401 {
            a.state = "reauth".into();
        } else if status == 429 {
            a.cooldown_until = now() + retry_after.clamp(1, 86400 * 7);
        }
        self.store.save_account(&a)
    }
    pub fn api_key(&self) -> Result<String> {
        match self.vault.get("gateway-key")? {
            Some(k) => Ok(k),
            None => {
                let k = format!(
                    "cp_{}{}",
                    uuid::Uuid::new_v4().simple(),
                    uuid::Uuid::new_v4().simple()
                );
                self.vault.set("gateway-key", &k)?;
                Ok(k)
            }
        }
    }
}
