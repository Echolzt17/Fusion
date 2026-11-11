pub mod account;
pub mod claude_pool;
pub mod api_sources;
pub mod api_gateway;
pub mod api_config;
pub mod compatibility;
pub mod config;
pub mod connection;
pub mod gateway;
pub mod handoff;
pub mod oauth;
pub mod scheduler;
pub mod storage;
pub mod subscription;
pub mod usage;
pub mod local_usage;
pub mod vault;
pub mod upstream_http;
pub mod network;

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

pub type Result<T> = std::result::Result<T, String>;
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: String,
    pub email: String,
    pub account_id: String,
    pub state: String,
    pub cooldown_until: i64,
    pub quota: Option<serde_json::Value>,
    pub quota_error: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub port: u16,
    pub concurrency: usize,
    pub fixed_account: Option<String>,
    #[serde(default)]
    pub selection_policy: scheduler::SelectionPolicy,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            port: 18765,
            concurrency: 4,
            fixed_account: None,
            selection_policy: scheduler::SelectionPolicy::Auto,
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RequestLog {
    pub id: String,
    pub at: i64,
    pub account: Option<String>,
    /// Unix milliseconds; older logs have no start timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_model: Option<String>,
    /// Existing thread/session binding hash, never the raw client identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_key: Option<String>,
    pub operation: String,
    pub status: String,
    pub elapsed_ms: u64,
    pub advice: String,
    #[serde(default)]
    pub usage: Option<usage::TokenUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<diagnostics::RequestDiagnostic>,
}
pub mod diagnostics;
pub mod error_reports;

#[cfg(test)]
mod tests;

mod resets;
pub use resets::ResetCredits;
