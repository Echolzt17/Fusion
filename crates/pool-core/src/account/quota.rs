use super::*;
use futures_util::{stream::FuturesUnordered, StreamExt};
use std::time::Duration;
use tokio::sync::watch;

pub const QUOTA_REFRESH_INTERVAL_SECS: i64 = 600;
type InFlight = watch::Receiver<Option<Result<()>>>;

#[derive(Default)]
pub(super) struct Refresh {
    last_attempt: Option<i64>,
    pub(super) pending: Option<InFlight>,
}

fn due(last: Option<i64>, at: i64) -> bool {
    !last.is_some_and(|last| (0..QUOTA_REFRESH_INTERVAL_SECS).contains(&at.saturating_sub(last)))
}

impl Accounts {
    pub fn quota_refreshing(&self, id: &str) -> bool {
        self.quota_runtime
            .lock()
            .unwrap()
            .get(id)
            .is_some_and(|s| s.pending.is_some())
    }

    pub async fn quota(self: &Arc<Self>, id: &str) -> Result<()> {
        self.refresh_quota(id, true).await
    }

    // This is backend-owned: neither window visibility nor widget reads drive queries.
    // Each scan also discovers newly authorized accounts. Failures stay on their cards.
    pub async fn refresh_due_quotas(self: &Arc<Self>) -> Result<()> {
        let accounts = self.store.accounts()?;
        let mut reads = FuturesUnordered::new();
        for account in accounts {
            if account.state != "reauth" && account.state != "deleting" {
                let this = self.clone();
                reads.push(async move { this.refresh_quota(&account.id, false).await });
            }
        }
        while reads.next().await.is_some() {}
        Ok(())
    }

    async fn refresh_quota(self: &Arc<Self>, id: &str, force: bool) -> Result<()> {
        let account = self.store.account(id)?;
        if account.state == "deleting" || account.state == "reauth" {
            return Err("账号正在移除或需要重新授权，无法查询额度".into());
        }
        let mut receiver = {
            let mut states = self.quota_runtime.lock().unwrap();
            let state = states.entry(id.to_owned()).or_default();
            if let Some(pending) = &state.pending {
                pending.clone()
            } else {
                let last = state
                    .last_attempt
                    .or_else(|| account.quota.as_ref()?.get("updatedAt")?.as_i64());
                if !force && !due(last, now()) {
                    return Ok(());
                }
                let (sender, receiver) = watch::channel(None);
                state.pending = Some(receiver.clone());
                let this = self.clone();
                let id = id.to_owned();
                let flight = receiver.clone();
                // A cancelled IPC reader must not lose the persisted result or leave a
                // stuck refreshing flag. All callers join the same independent task.
                tokio::spawn(async move {
                    let result = this.query_quota(&id, &flight).await;
                    if let Some(state) = this.quota_runtime.lock().unwrap().get_mut(&id) {
                        if state
                            .pending
                            .as_ref()
                            .is_some_and(|pending| pending.same_channel(&flight))
                        {
                            state.last_attempt = Some(now());
                            state.pending = None;
                        }
                    }
                    let _ = sender.send(Some(result));
                });
                receiver
            }
        };
        loop {
            let result = receiver.borrow_and_update().clone();
            if let Some(result) = result {
                return result;
            }
            receiver.changed().await.map_err(|_| "额度查询任务异常")?;
        }
    }

    fn owns_quota_refresh(&self, id: &str, flight: &InFlight) -> bool {
        self.quota_runtime
            .lock()
            .unwrap()
            .get(id)
            .and_then(|state| state.pending.as_ref())
            .is_some_and(|pending| pending.same_channel(flight))
    }

    async fn query_quota(self: &Arc<Self>, id: &str, flight: &InFlight) -> Result<()> {
        let _slot = self
            .quota_slots
            .acquire()
            .await
            .map_err(|_| "额度查询已停止")?;
        let result: Result<Value> = async {
            if !self.owns_quota_refresh(id, flight) { return Err("账号已移除，已取消旧额度查询".into()); }
            let t = self.token(id).await?;
            let account = self.store.account(id)?;
            let response = self.http.get(&self.quota_endpoint).await?
                .bearer_auth(&t.access_token)
                .header("ChatGPT-Account-ID", &account.account_id)
                .timeout(Duration::from_secs(20))
                .send().await.map_err(|_| "额度查询网络失败".to_owned())?;
            if !response.status().is_success() {
                return Err("额度查询失败；不代表额度为零".into());
            }
            let value = response.json::<Value>().await.map_err(|_| "额度响应格式错误")?;
            if !value["rate_limit"].is_object() { return Err("额度响应缺少有效的周期信息".into()); }
            let subscription = crate::subscription::expiry(&t, &account.account_id, value["plan_type"].as_str().unwrap_or(""));
            Ok(json!({"rateLimit":value["rate_limit"],"planType":value["plan_type"],"updatedAt":now(),"subscription":subscription}))
        }.await;
        let lock = self.lock(id);
        let _guard = lock.lock().await;
        // Re-read under the account lock. Never resurrect a removed account or
        // overwrite the durable deletion intent / concurrent account state.
        let mut account = self.store.account(id)?;
        if !self.owns_quota_refresh(id, flight) {
            return Err("账号已移除，已丢弃旧额度查询结果".into());
        }
        if account.state == "deleting" {
            return Err("账号正在移除，已丢弃额度查询结果".into());
        }
        match &result {
            Ok(value) => {
                account.quota = Some(value.clone());
                account.quota_error = None;
            }
            Err(error) => account.quota_error = Some(error.clone()),
        }
        self.store.save_account(&account)?;
        result.map(|_| ())
    }
}

#[cfg(test)]
mod tests;
