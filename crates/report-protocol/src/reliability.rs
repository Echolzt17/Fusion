//! Closed numeric telemetry. No URLs, arbitrary error text, identities or payloads.
use serde::{Deserialize, Serialize};

pub const ORIGINS: &[&str] = &[
    "local",
    "transport",
    "upstream_service",
    "account",
    "unknown",
];
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Transport {
    pub request_bytes: Option<u64>,
    pub response_bytes: u64,
    pub output_event_observed: bool,
    pub tool_call_observed: bool,
    pub reconnect_started: bool,
    pub reconnect_recovered: bool,
    pub attempts: Vec<SendAttempt>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SendAttempt {
    /// Whole send-to-headers attempt; not an independent DNS/TLS measurement.
    pub elapsed_ms: u64,
    pub result: String,
    pub network_error_kind: Option<String>,
    /// Body producer progress, NOT socket delivery or upstream acknowledgement.
    pub body_bytes_yielded: u64,
    pub body_first_yield_ms: Option<u64>,
    pub body_last_yield_ms: Option<u64>,
}
impl Transport {
    pub fn valid(&self) -> bool {
        self.attempts.len() <= 2
            && (!self.reconnect_recovered
                || self.reconnect_started
                    && self.attempts.len() == 2
                    && self.attempts[1].result == "headers")
            && (!self.reconnect_started || !self.attempts.is_empty())
            && (self.attempts.len() < 2
                || self.reconnect_started && self.attempts[0].result == "connect_failure")
            && self.attempts.iter().all(|a| {
                [
                    "headers",
                    "connect_failure",
                    "send_failure",
                    "cancelled",
                    "deadline",
                ]
                .contains(&a.result.as_str())
                    && super::optional_category(&a.network_error_kind, super::NETWORKS)
                    && a.body_bytes_yielded <= self.request_bytes.unwrap_or(u64::MAX)
                    && match (
                        a.body_bytes_yielded,
                        a.body_first_yield_ms,
                        a.body_last_yield_ms,
                    ) {
                        (0, None, None) => true,
                        (n, Some(first), Some(last)) => {
                            n > 0 && first <= last && last <= a.elapsed_ms
                        }
                        _ => false,
                    }
            })
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReliabilitySummary {
    pub coverage_start: i64,
    pub snapshot_at: i64,
    pub history_partial: bool,
    pub buckets: Vec<ReliabilityHour>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReliabilityHour {
    pub hour: i64,
    pub app_version: String,
    pub operation: String,
    pub success: u64,
    pub failure: u64,
    pub cancelled: u64,
    pub stopped: u64,
    pub reconnect_started: u64,
    pub reconnect_recovered: u64,
    pub stream_interrupted: u64,
    /// local, transport, upstream_service, account, unknown.
    pub failures_by_origin: [u64; 5],
    /// <5s, <15s, <60s, <180s, <600s, >=600s. Success/failure only.
    pub elapsed_histogram: [u64; 6],
    /// unknown, <64KiB, <1MiB, <8MiB, >=8MiB.
    pub success_by_size: [u64; 5],
    pub failure_by_size: [u64; 5],
}
impl ReliabilityHour {
    pub fn total(&self) -> u64 {
        self.success
            .saturating_add(self.failure)
            .saturating_add(self.cancelled)
            .saturating_add(self.stopped)
    }
    pub fn key(&self) -> (i64, &str, &str) {
        (self.hour, &self.app_version, &self.operation)
    }
}
fn sum(xs: &[u64]) -> u128 {
    xs.iter().map(|x| u128::from(*x)).sum()
}
impl ReliabilitySummary {
    pub fn valid(&self, prepared_at: i64) -> bool {
        let mut keys = std::collections::HashSet::new();
        self.coverage_start > 0
            && self.coverage_start <= self.snapshot_at
            && self.snapshot_at <= prepared_at
            && self.buckets.len() <= 4096
            && self.buckets.iter().all(|b| {
                b.hour >= (prepared_at - 7 * 86400).div_euclid(3600) * 3600
                    && b.hour <= self.snapshot_at
                    && b.hour % 3600 == 0
                    && keys.insert(b.key())
                    && super::version(&b.app_version).as_deref() == Some(b.app_version.as_str())
                    && ["responses", "compact", "image_generate", "image_edit"]
                        .contains(&b.operation.as_str())
                    && b.total() > 0
                    && b.total() <= 1_000_000
                    && b.reconnect_recovered <= b.reconnect_started
                    && b.reconnect_recovered <= b.success
                    && b.reconnect_started <= b.total()
                    && b.stream_interrupted <= b.failure
                    && sum(&b.failures_by_origin) == u128::from(b.failure)
                    && sum(&b.elapsed_histogram) == u128::from(b.success) + u128::from(b.failure)
                    && sum(&b.success_by_size) == u128::from(b.success)
                    && sum(&b.failure_by_size) == u128::from(b.failure)
            })
    }
}
