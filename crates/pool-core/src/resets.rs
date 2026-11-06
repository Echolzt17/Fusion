use crate::{account::Accounts, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

// Protocol reference: openai/codex backend-client/client/rate_limit_resets.rs.
// Only this whitelist crosses IPC; upstream bodies and credentials are never logged.
#[derive(Serialize, Deserialize)]
pub struct ResetCredit {
    pub id: String,
    pub reset_type: String,
    pub status: String,
    pub expires_at: Option<String>,
}
#[derive(Serialize, Deserialize)]
pub struct ResetCredits {
    pub credits: Vec<ResetCredit>,
    pub available_count: u64,
}
impl Accounts {
    pub async fn reset_credits(self: &Arc<Self>, id: &str) -> Result<ResetCredits> {
        let t = self.token(id).await?;
        let a = self.store.account(id)?;
        let r = self
            .http
            .get(&self.reset_endpoint).await?
            .bearer_auth(t.access_token)
            .header("ChatGPT-Account-ID", a.account_id)
            .timeout(std::time::Duration::from_secs(20))
            .send()
            .await
            .map_err(|_| "重置卡查询网络失败，请手动刷新")?;
        if !r.status().is_success() {
            return Err("重置卡查询失败，不代表没有重置卡".into());
        }
        r.json().await.map_err(|_| "重置卡响应格式不兼容".into())
    }

    pub async fn consume_reset(self: &Arc<Self>, id: &str, credit_id: &str) -> Result<String> {
        if credit_id.is_empty() || credit_id.len() > 256 {
            return Err("请选择有效重置卡".into());
        }
        let lock = self
            .reset_locks
            .lock()
            .unwrap()
            .entry(id.into())
            .or_default()
            .clone();
        let _guard = lock.lock().await;
        let details = self.reset_credits(id).await?;
        if !details.credits.iter().any(|c| {
            c.id == credit_id && c.status == "available" && c.reset_type == "codex_rate_limits"
        }) {
            return Err("这张重置卡已不可用，请刷新后重新选择".into());
        }
        let t = self.token(id).await?;
        let a = self.store.account(id)?;
        // Retain the same idempotency key across timeouts/restarts and manual retries.
        // No automatic replay, account switching, or paid-reset endpoint.
        let key = format!("reset-request/{id}/{credit_id}");
        let request = self
            .store
            .get(&key)?
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        self.store.set(&key, &request)?;
        let uncertain =
            "重置结果未确认；请先刷新额度和重置卡。若仍可用，可手动重试同一张卡（沿用原请求编号）";
        let r = self
            .http
            .post(&format!("{}/consume", self.reset_endpoint)).await?
            .bearer_auth(t.access_token)
            .header("ChatGPT-Account-ID", a.account_id)
            .json(&json!({"redeem_request_id": request, "credit_id": credit_id}))
            .timeout(std::time::Duration::from_secs(20))
            .send()
            .await
            .map_err(|_| uncertain)?;
        if !r.status().is_success() {
            return Err(uncertain.into());
        }
        #[derive(Deserialize)]
        struct Outcome {
            code: String,
        }
        let outcome: Outcome = r.json().await.map_err(|_| uncertain)?;
        let message = match outcome.code.as_str() {
            "reset" => "额度已重置，请查看刷新后的额度",
            "nothing_to_reset" => "当前额度无需重置，重置卡未消耗",
            "no_credit" => "当前没有可用重置卡，请刷新查询",
            "already_redeemed" => "该请求已兑换，请查看刷新后的额度",
            _ => return Err(uncertain.into()),
        };
        if matches!(outcome.code.as_str(), "nothing_to_reset" | "no_credit") {
            self.store.set(&key, "")?;
        }
        // Keep successful request IDs too: a stale read must never spend another card.
        if matches!(outcome.code.as_str(), "reset" | "already_redeemed") {
            let lock = self.lock(id);
            let _account_guard = lock.lock().await;
            let mut a = self.store.account(id)?;
            a.cooldown_until = 0;
            self.store.save_account(&a)?;
        }
        Ok(message.into())
    }
}
