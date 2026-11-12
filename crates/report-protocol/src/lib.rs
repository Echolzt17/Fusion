//! Shared, closed diagnostic wire format. No arbitrary messages, model names, paths or credentials.
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
mod reliability;
pub use reliability::*;
mod known;
pub use known::*;
pub const WIRE_LIMIT: usize = 4 * 1024 * 1024;
pub const JSON_LIMIT: usize = 16 * 1024 * 1024;
pub const RECORD_LIMIT: usize = 2000;
pub const ERROR_CODES: &[&str] = &[
    "upstream_header_timeout", "upstream_read_timeout", "upstream_empty_response",
    "invalid_event_stream", "upstream_response_too_large", "upstream_forbidden",
    "reauthorization_required", "upstream_bad_request", "response_incomplete",
    "invalid_image_response", "unexpected_content_type", "network_error", "read_error",
    "stream_interrupted", "stream_invalid", "upstream_rate_limited", "account_unavailable",
    "authorization_unavailable", "response_failed", "continuity_unavailable", "upstream_error",
    "proxy_configuration", "request_too_large", "other_failure",
    "ingress_busy", "ingress_timeout", "upload_idle_timeout", "upload_timeout",
    "upload_wait_timeout", "body_read_error", "json_structure_limit", "invalid_request",
];
pub const STAGES: &[&str] = &["queue", "proxy_resolution", "connect", "response_headers", "response_read", "event_validation", "upstream_rejected", "unknown", "ingress_queue", "upload", "request_validation"];
pub const NETWORKS: &[&str] = &["dns", "connection_refused", "connection_reset", "connection_aborted", "connection_closed", "connect", "tls", "proxy_authentication", "proxy_tunnel", "timeout", "http_protocol", "body_read", "unexpected_eof", "stream_ended", "unknown"];
pub const PROXIES: &[&str] = &["environment_configured", "windows_static_configured", "windows_automatic_configured", "loopback_direct", "none_configured", "unknown"];
pub const OPERATIONS: &[&str] = &["responses", "compact", "image_generate", "image_edit", "models"];

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Execution {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<Transport>,
    pub local_http_status: Option<u16>,
    pub upstream_attempted: bool,
    pub headers_received: bool,
    pub first_byte_received: bool,
    pub terminal_received: bool,
    pub queue_ms: Option<u64>,
    pub authorization_ms: Option<u64>,
    /// Includes connection setup; never claim an independent DNS/TLS measurement.
    pub headers_wait_ms: Option<u64>,
    /// From upstream attempt to first response body bytes.
    pub first_byte_ms: Option<u64>,
    pub max_read_idle_ms: Option<u64>,
    pub active_requests: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ingress_queue_ms: Option<u64>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FailureDetails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    pub response_type: Option<String>,
    pub detected_response_type: Option<String>,
    pub terminal_event: Option<String>,
    pub upstream_failure: Option<String>,
}
impl FailureDetails {
    pub fn valid(&self) -> bool {
        let types = &["sse", "html", "json", "missing", "other"];
        optional_category(&self.origin, ORIGINS) && optional_category(&self.response_type, types) && optional_category(&self.detected_response_type, types)
            && optional_category(&self.terminal_event, &["completed", "failed", "incomplete", "error"])
            && optional_category(&self.upstream_failure, &["authorization", "rate_limit", "server", "max_output_tokens", "content_filter", "unknown"])
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Environment {
    pub app_version: String,
    pub os: String,
    pub os_build: Option<u32>,
    pub architecture: String,
    pub webview2_version: Option<String>,
    pub desktop_version: Option<String>,
    pub engine_version: Option<String>,
    pub uptime_seconds: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ErrorRecord {
    pub request_id: String,
    pub at: i64,
    pub operation: String,
    pub code: String,
    pub elapsed_ms: u64,
    pub account_alias: Option<u16>,
    pub conversation_alias: Option<u16>,
    pub app_version: Option<String>,
    pub upstream_status: Option<u16>,
    pub failure_stage: Option<String>,
    pub network_error_kind: Option<String>,
    pub proxy_source: Option<String>,
    pub execution: Option<Execution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<FailureDetails>,
}
impl ErrorRecord {
    pub fn signature(&self) -> String {
        let base = format!("{}|{}|{}|{}|{}", self.operation, self.code,
            self.failure_stage.as_deref().unwrap_or("unknown"),
            self.network_error_kind.as_deref().unwrap_or("unknown"),
            self.upstream_status.map(|n| n.to_string()).unwrap_or_else(|| "unknown".into()));
        // Preserve old signatures; new evidence separates terminal failures with different causes.
        match &self.details {
            Some(d) if d.terminal_event.is_some() || d.upstream_failure.is_some() => format!("{base}|{}|{}",d.terminal_event.as_deref().unwrap_or("unknown"),d.upstream_failure.as_deref().unwrap_or("unknown")),
            _ => base,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Report {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reliability: Option<ReliabilitySummary>,
    pub schema_version: u8,
    pub report_id: String,
    pub installation_id: String,
    pub prepared_at: i64,
    pub range_from: i64,
    pub range_to: i64,
    pub history_limited: bool,
    pub environment: Environment,
    pub errors: Vec<ErrorRecord>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Receipt {
    pub report_id: String,
    pub received_at: i64,
    pub accepted_errors: usize,
    pub duplicate_errors: usize,
}
pub fn uuid_ok(s: &str) -> bool { uuid::Uuid::parse_str(s).is_ok_and(|id| id.to_string() == s) }
pub fn version(s: &str) -> Option<String> {
    // Numeric versions and bounded, known prerelease tags only; no arbitrary command output.
    s.split_whitespace().find(|part| {
        let (base, prerelease) = part.split_once('-').map_or((*part, None), |(b, p)| (b, Some(p)));
        let tag_ok = prerelease.is_none_or(|tag| {
            let mut parts = tag.split('.');
            matches!(parts.next(), Some("alpha" | "beta" | "rc")) && parts.clone().count() <= 3
                && parts.all(|n| !n.is_empty() && n.len() <= 8 && n.bytes().all(|b| b.is_ascii_digit()))
        });
        let mut count = 0;
        part.len() <= 64 && tag_ok && base.split('.').all(|n| { count += 1; !n.is_empty() && n.len() <= 8 && n.bytes().all(|c| c.is_ascii_digit()) }) && (2..=4).contains(&count)
    }).map(str::to_owned)
}
fn optional_category(s: &Option<String>, choices: &[&str]) -> bool { s.as_deref().is_none_or(|s| choices.contains(&s)) }
fn valid_status(n: Option<u16>) -> bool { n.is_none_or(|n| (100..=599).contains(&n)) }
impl Report {
    pub fn validate(&self) -> Result<(), &'static str> {
        let e = &self.environment;
        if !matches!(self.schema_version, 1..=3) || !uuid_ok(&self.report_id) || !uuid_ok(&self.installation_id)
            || self.prepared_at <= 0 || self.range_from < self.prepared_at - 7 * 86400
            || self.range_to > self.prepared_at || self.range_from > self.range_to
            || (self.errors.is_empty() && self.reliability.as_ref().is_none_or(|s| s.buckets.is_empty())) || self.errors.len() > RECORD_LIMIT
            || self.reliability.as_ref().is_some_and(|s| self.schema_version < 3 || !s.valid(self.prepared_at))
            || version(&e.app_version).as_deref() != Some(&e.app_version)
            || !["Windows 10", "Windows 11", "Windows (unknown)"].contains(&e.os.as_str())
            || !["x64", "x86", "arm64", "unknown"].contains(&e.architecture.as_str())
            || [&e.webview2_version, &e.desktop_version, &e.engine_version].into_iter().any(|s| s.as_ref().is_some_and(|s| version(s).as_deref() != Some(s))) {
            return Err("invalid_report");
        }
        let mut ids = std::collections::HashSet::new();
        for r in &self.errors {
            if !uuid_ok(&r.request_id) || !ids.insert(&r.request_id) || r.at < self.range_from || r.at > self.range_to
                || !OPERATIONS.contains(&r.operation.as_str()) || !ERROR_CODES.contains(&r.code.as_str())
                || !optional_category(&r.failure_stage, STAGES) || !optional_category(&r.network_error_kind, NETWORKS)
                || !optional_category(&r.proxy_source, PROXIES) || !valid_status(r.upstream_status)
                || r.account_alias.is_some_and(|n| n == 0 || n > RECORD_LIMIT as u16)
                || r.conversation_alias.is_some_and(|n| n == 0 || n > RECORD_LIMIT as u16)
                || r.app_version.as_ref().is_some_and(|s| version(s).as_deref() != Some(s))
                || r.execution.as_ref().is_some_and(|x| !valid_status(x.local_http_status))
                || r.details.as_ref().is_some_and(|d| !d.valid())
                || r.execution.as_ref().and_then(|x| x.transport.as_ref()).is_some_and(|t| self.schema_version < 3 || !t.valid())
                || self.schema_version < 3 && r.details.as_ref().is_some_and(|d| d.origin.is_some())
                || self.schema_version == 1 && (r.details.is_some()
                    || r.execution.as_ref().is_some_and(|x| x.ingress_queue_ms.is_some())
                    || matches!(r.code.as_str(),"ingress_busy"|"ingress_timeout"|"upload_idle_timeout"|"upload_timeout"|"upload_wait_timeout"|"body_read_error"|"json_structure_limit"|"invalid_request")
                    || matches!(r.failure_stage.as_deref(),Some("ingress_queue"|"upload"|"request_validation"))) { return Err("invalid_error"); }
        }
        Ok(())
    }
    pub fn encode(&self) -> Result<Vec<u8>, &'static str> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|_| "encode_failed")?;
        if bytes.len() > JSON_LIMIT { return Err("report_too_large"); }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&bytes).map_err(|_| "encode_failed")?;
        let bytes = gz.finish().map_err(|_| "encode_failed")?;
        if bytes.len() > WIRE_LIMIT { return Err("report_too_large"); }
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > WIRE_LIMIT { return Err("report_too_large"); }
        let mut json = Vec::new();
        flate2::read::GzDecoder::new(bytes).take(JSON_LIMIT as u64 + 1).read_to_end(&mut json).map_err(|_| "invalid_gzip")?;
        if json.len() > JSON_LIMIT { return Err("report_too_large"); }
        let value: Self = serde_json::from_slice(&json).map_err(|_| "invalid_report")?;
        value.validate()?;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn versions_cannot_carry_messages_or_paths() {
        assert_eq!(version("codex-cli 0.77.1"), Some("0.77.1".into()));
        assert_eq!(version("codex-cli 0.166.0-alpha.5.1"), Some("0.166.0-alpha.5.1".into()));
        for bad in ["user@example.com", "C:/secret", "Bearer-token", "1.2.3-secret"] { assert!(version(bad).is_none()); }
    }
    #[test]
    fn oversized_compressed_and_expanded_input_rejected() {
        assert!(Report::decode(&vec![0; WIRE_LIMIT + 1]).is_err());
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&vec![b' '; JSON_LIMIT + 1]).unwrap();
        assert_eq!(Report::decode(&gz.finish().unwrap()).unwrap_err(), "report_too_large");
    }
}
