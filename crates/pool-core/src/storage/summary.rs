//! Numeric widget summaries. No request content, credentials or raw conversation IDs.
use super::*;
use crate::usage::{AccountUsage, ConversationUsage};
use rusqlite::{OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const ALL: &str = "request-summary/v1/all";
const PREFIX: &str = "request-summary/v1/conversation/";

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCount {
    pub name: String,
    pub requested: bool,
    pub requests: u64,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestSummary {
    pub requests: u64,
    pub measured_requests: u64,
    pub total_tokens: u64,
    pub success: u64,
    pub failed: u64,
    pub other: u64,
    pub unclassified_requests: u64,
    pub history_partial: bool,
    pub models: Vec<ModelCount>,
    pub operations: BTreeMap<String, u64>,
}
impl RequestSummary {
    fn record(&mut self, log: &RequestLog) {
        self.requests = self.requests.saturating_add(1);
        if let Some(usage) = &log.usage {
            self.measured_requests = self.measured_requests.saturating_add(1);
            self.total_tokens = self.total_tokens.saturating_add(usage.total_tokens);
        }
        match log.status.as_str() {
            "success" => self.success = self.success.saturating_add(1),
            "upstream_header_timeout" | "upstream_read_timeout" | "upstream_empty_response"
            | "invalid_event_stream" | "upstream_response_too_large" | "upstream_forbidden"
            | "reauthorization_required" | "upstream_bad_request" | "response_incomplete"
            | "invalid_image_response" | "unexpected_content_type" | "network_error"
            | "read_error" | "stream_interrupted" | "stream_invalid" | "upstream_rate_limited"
            | "account_unavailable" | "authorization_unavailable" | "response_failed"
            | "continuity_unavailable" | "upstream_error" | "proxy_configuration" | "request_too_large"
            | "ingress_busy" | "ingress_timeout" | "upload_idle_timeout" | "upload_timeout"
            | "upload_wait_timeout" | "body_read_error" | "json_structure_limit" | "invalid_request" => self.failed = self.failed.saturating_add(1),
            _ => self.other = self.other.saturating_add(1),
        }
        let (name, requested) = if let Some(name) = &log.response_model {
            (name.clone(), false)
        } else {
            (log.requested_model.clone().unwrap_or_default(), log.requested_model.is_some())
        };
        if let Some(model) = self.models.iter_mut().find(|m| m.name == name && m.requested == requested) {
            model.requests = model.requests.saturating_add(1);
        } else {
            self.models.push(ModelCount { name, requested, requests: 1 });
            self.models.sort_by(|a, b| a.name.cmp(&b.name).then(a.requested.cmp(&b.requested)));
        }
        let count = self.operations.entry(log.operation.clone()).or_default();
        *count = count.saturating_add(1);
    }
    fn retain_totals(&mut self, requests: u64, measured: u64, tokens: u64, partial: bool) {
        self.requests = self.requests.max(requests);
        self.unclassified_requests = self.requests.saturating_sub(self.success.saturating_add(self.failed).saturating_add(self.other));
        self.history_partial |= partial || self.unclassified_requests > 0;
        self.measured_requests = self.measured_requests.max(measured);
        self.total_tokens = self.total_tokens.max(tokens);
    }
}
fn read(tx: &rusqlite::Connection, key: &str) -> Result<RequestSummary> {
    let raw: Option<String> = tx.query_row("SELECT value FROM kv WHERE key=?1", [key], |r| r.get(0))
        .optional().map_err(|_| "读取请求汇总失败")?;
    raw.map(|s| serde_json::from_str(&s).map_err(|_| "请求汇总数据损坏".into()))
        .unwrap_or_else(|| Ok(RequestSummary::default()))
}
fn save(tx: &Transaction<'_>, key: &str, summary: &RequestSummary) -> Result<()> {
    tx.execute("INSERT INTO kv VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![key, serde_json::to_string(summary).map_err(|_| "请求汇总编码失败")?])
        .map_err(|_| "保存请求汇总失败")?;
    Ok(())
}
pub(super) fn record(tx: &Transaction<'_>, log: &RequestLog) -> Result<()> {
    let mut all = read(tx, ALL)?;
    all.record(log);
    save(tx, ALL, &all)?;
    if let Some(key) = &log.conversation_key {
        let key = format!("{PREFIX}{key}");
        let mut summary = read(tx, &key)?;
        summary.record(log);
        save(tx, &key, &summary)?;
    }
    Ok(())
}
impl Store {
    pub(super) fn initialize_summaries(&self) -> Result<()> {
        let mut db = self.db.lock().map_err(|_| "数据库锁异常")?;
        let tx = db.transaction().map_err(|_| "初始化请求汇总失败")?;
        let initialized: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM kv WHERE key=?1)", [ALL], |r| r.get(0))
            .map_err(|_| "读取请求汇总失败")?;
        if !initialized {
            let mut all = RequestSummary::default();
            let mut unidentified = RequestSummary::default();
            let mut conversations: BTreeMap<String, RequestSummary> = BTreeMap::new();
            let mut accounts: BTreeMap<String, RequestSummary> = BTreeMap::new();
            {
                let mut st = tx.prepare("SELECT body FROM logs ORDER BY at,rowid").map_err(|_| "读取历史请求失败")?;
                let rows = st.query_map([], |r| r.get::<_, String>(0)).map_err(|_| "读取历史请求失败")?;
                for row in rows {
                    let log: RequestLog = serde_json::from_str(&row.map_err(|_| "读取历史请求失败")?).map_err(|_| "历史请求损坏")?;
                    if !history::is_request(&log) { continue; }
                    all.record(&log);
                    if let Some(key) = &log.conversation_key { conversations.entry(key.clone()).or_default().record(&log); }
                    else { unidentified.record(&log); }
                    if let Some(account) = &log.account { accounts.entry(account.clone()).or_default().record(&log); }
                }
            }
            // Account totals cover disjoint requests, including removed accounts. Only
            // add the proven difference from retained logs; never infer pruned results.
            {
                let mut st = tx.prepare("SELECT value FROM kv WHERE key >= 'usage/v1/' AND key < 'usage/v10'").map_err(|_| "读取历史用量失败")?;
                let rows = st.query_map([], |r| r.get::<_, String>(0)).map_err(|_| "读取历史用量失败")?;
                for row in rows {
                    let old: AccountUsage = serde_json::from_str(&row.map_err(|_| "读取历史用量失败")?).map_err(|_| "历史用量损坏")?;
                    let retained = accounts.get(&old.account).cloned().unwrap_or_default();
                    let missing = old.requests.saturating_sub(retained.requests);
                    all.requests = all.requests.saturating_add(missing);
                    all.unclassified_requests = all.unclassified_requests.saturating_add(missing);
                    all.measured_requests = all.measured_requests.saturating_add(old.measured_requests.saturating_sub(retained.measured_requests));
                    all.total_tokens = all.total_tokens.saturating_add(old.tokens.total_tokens.saturating_sub(retained.total_tokens));
                    all.history_partial |= missing > 0;
                }
            }
            {
                let mut st = tx.prepare("SELECT value FROM kv WHERE key >= 'conversation-usage/v1/' AND key < 'conversation-usage/v10'").map_err(|_| "读取历史对话失败")?;
                let rows = st.query_map([], |r| r.get::<_, String>(0)).map_err(|_| "读取历史对话失败")?;
                for row in rows {
                    let old: ConversationUsage = serde_json::from_str(&row.map_err(|_| "读取历史对话失败")?).map_err(|_| "历史对话损坏")?;
                    all.history_partial |= old.history_partial;
                    conversations.entry(old.conversation_key).or_default().retain_totals(old.requests, old.measured_requests, old.tokens.total_tokens, old.history_partial);
                }
            }
            // Identified conversations and unidentified retained requests are
            // disjoint. Their totals establish another proven lower bound.
            // Do not add that bound to the overlapping account totals.
            let floor = conversations.values().fold(unidentified, |mut total, summary| {
                total.requests = total.requests.saturating_add(summary.requests);
                total.measured_requests = total.measured_requests.saturating_add(summary.measured_requests);
                total.total_tokens = total.total_tokens.saturating_add(summary.total_tokens);
                total
            });
            all.retain_totals(floor.requests, floor.measured_requests, floor.total_tokens, floor.history_partial);
            for (key, summary) in conversations { save(&tx, &format!("{PREFIX}{key}"), &summary)?; }
            save(&tx, ALL, &all)?;
        }
        tx.commit().map_err(|_| "提交请求汇总失败".into())
    }
    pub fn request_summaries(&self, conversation: Option<&str>) -> Result<(RequestSummary, Option<RequestSummary>)> {
        let db = self.db.lock().map_err(|_| "数据库锁异常")?;
        let all = read(&db, ALL)?;
        let current = conversation.map(|key| read(&db, &format!("{PREFIX}{key}"))).transpose()?;
        Ok((all, current))
    }
}

#[cfg(test)]
mod tests;
