use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const MAX_SUBSCRIPTION_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_NODES: usize = 1000;
pub const REFRESH_INTERVAL_SECS: i64 = 6 * 60 * 60;
pub const HEALTH_INTERVAL_SECS: u64 = 60;
pub const COOLDOWN_SECS: i64 = 5 * 60;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SelectionMode {
    #[default]
    Auto,
    Fixed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NetworkSettings {
    pub enabled: bool,
    pub mode: SelectionMode,
    #[serde(default)]
    pub allowed_regions: Vec<String>,
    #[serde(default)]
    pub node_order: Vec<String>,
    pub fixed_node_id: Option<String>,
    #[serde(default)]
    pub excluded_node_ids: Vec<String>,
    #[serde(default)]
    pub region_overrides: BTreeMap<String, String>,
}
impl Default for NetworkSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: SelectionMode::Auto,
            allowed_regions: vec![],
            node_order: vec![],
            fixed_node_id: None,
            excluded_node_ids: vec![],
            region_overrides: BTreeMap::new(),
        }
    }
}

// Contains node credentials. Never implement Debug or return through IPC.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredNode {
    pub id: String,
    pub name: String,
    pub region: String,
    pub protocol: String,
    pub(crate) config: Value,
}
impl StoredNode {
    pub fn core_name(&self) -> String {
        format!("fusion_{}", self.id)
    }
    pub fn proxy_config(&self) -> Value {
        let mut value = self.config.clone();
        value["name"] = self.core_name().into();
        value
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeHealth {
    pub checked_at: Option<i64>,
    pub latency_ms: Option<u64>,
    pub consecutive_failures: u32,
    pub cooldown_until: Option<i64>,
    pub successful_checks: u64,
    pub failed_checks: u64,
    #[serde(default)]
    pub business_successes: u64,
    #[serde(default)]
    pub business_failures: u64,
}
impl NodeHealth {
    pub fn record(&mut self, latency: Option<u64>, at: i64) {
        self.checked_at = Some(at);
        self.latency_ms = latency;
        if latency.is_some() {
            self.consecutive_failures = 0;
            self.cooldown_until = None;
            self.successful_checks = self.successful_checks.saturating_add(1);
        } else {
            self.failed_checks = self.failed_checks.saturating_add(1);
            self.consecutive_failures = self.consecutive_failures.saturating_add(1);
            if self.consecutive_failures >= 3 {
                self.cooldown_until = Some(at.saturating_add(COOLDOWN_SECS));
            }
        }
    }
    pub fn eligible(&self, at: i64) -> bool {
        self.cooldown_until.is_none_or(|until| until <= at) && self.consecutive_failures < 3
    }
    pub fn status(&self, at: i64) -> &'static str {
        if self.cooldown_until.is_some_and(|until| until > at) || self.consecutive_failures >= 3 {
            "unhealthy"
        } else if self.consecutive_failures > 0 {
            "degraded"
        } else if self.checked_at.is_some() {
            "healthy"
        } else {
            "unknown"
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeView {
    pub id: String,
    pub name: String,
    pub region: String,
    pub inferred_region: String,
    pub protocol: String,
    pub health: String,
    pub delay_ms: Option<u64>,
    pub last_checked_at: Option<i64>,
    pub cooldown_until: Option<i64>,
    pub failure_count: u32,
    pub excluded: bool,
    pub business_successes: u64,
    pub business_failures: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegionSummary {
    pub code: String,
    pub total: usize,
    pub healthy: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkStatus {
    pub selected_subscription_id: Option<String>,
    pub subscriptions: Vec<SubscriptionView>,
    pub settings: NetworkSettings,
    pub kernel_running: bool,
    pub current_node_id: Option<String>,
    pub nodes: Vec<NodeView>,
    pub subscription_name: Option<String>,
    pub subscription_updated_at: Option<i64>,
    pub subscription_kind: Option<String>,
    pub pending_update: bool,
    pub phase: String,
    pub error: Option<String>,
    pub last_switch_reason: Option<String>,
    pub active_requests: usize,
    pub skipped_nodes: usize,
    pub import_warnings: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionView {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub node_count: usize,
    pub updated_at: i64,
}

// URL and content are secret input, never Debug or returned to the UI.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubscriptionInput {
    pub url: Option<String>,
    pub yaml: Option<String>,
    pub name: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Snapshot {
    pub version: u32,
    pub label: String,
    pub updated_at: i64,
    pub kind: String,
    pub nodes: Vec<StoredNode>,
    pub excluded: usize,
}

pub struct ParsedSubscription {
    pub nodes: Vec<StoredNode>,
    pub excluded: usize,
}
