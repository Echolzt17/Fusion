//! Failure-only projections. Never serialize RequestLog directly to the reporting service.
use crate::{RequestLog, Result};
use fusion_report_protocol::{Environment, ErrorRecord, Report, ERROR_CODES, OPERATIONS};
use std::collections::{BTreeMap, HashSet};

pub fn is_failure(log: &RequestLog) -> bool {
    OPERATIONS.contains(&log.operation.as_str())
        && !matches!(log.status.as_str(), "success" | "cancelled" | "service_stopped")
}
fn category<T: serde::Serialize>(v: &T) -> Option<String> {
    serde_json::to_value(v).ok()?.as_str().map(str::to_owned)
}
pub fn prepare(logs: &[RequestLog], installation_id: &str, environment: Environment, at: i64, limited: bool) -> Result<Report> {
    let mut accounts = BTreeMap::new();
    let mut conversations = BTreeMap::new();
    let mut seen = HashSet::new();
    fn alias(map: &mut BTreeMap<String, u16>, key: &Option<String>) -> Option<u16> {
        key.as_ref().map(|key| { let next = map.len() as u16 + 1; *map.entry(key.clone()).or_insert(next) })
    }
    let errors = logs.iter().filter(|l| is_failure(l) && l.at >= at - 7 * 86400 && l.at <= at)
        .filter(|l| fusion_report_protocol::uuid_ok(&l.id) && seen.insert(l.id.clone()))
        .take(fusion_report_protocol::RECORD_LIMIT).map(|l| {
            let d = l.diagnostic.as_ref();
            ErrorRecord {
                request_id: l.id.clone(), at: l.at, operation: l.operation.clone(),
                code: if ERROR_CODES.contains(&l.status.as_str()) { l.status.clone() } else { "other_failure".into() },
                elapsed_ms: l.elapsed_ms, account_alias: alias(&mut accounts, &l.account), conversation_alias: alias(&mut conversations, &l.conversation_key),
                app_version: d.and_then(|d| fusion_report_protocol::version(&d.app_version)),
                upstream_status: d.and_then(|d| d.upstream_status),
                failure_stage: d.and_then(|d| d.failure_stage).and_then(|s| category(&s)).filter(|s| fusion_report_protocol::STAGES.contains(&s.as_str())),
                network_error_kind: d.and_then(|d| d.network_error_kind).and_then(|s| category(&s)),
                proxy_source: d.and_then(|d| category(&d.proxy_source)),
                execution: d.and_then(|d| d.execution.clone()),
                details: d.map(|d| {
                    let mut details=d.details.clone().unwrap_or_default();
                    details.response_type=d.response_type.and_then(|v| category(&v));
                    details.detected_response_type=d.detected_response_type.and_then(|v| category(&v));
                    details
                }),
            }
        }).collect();
    let report = Report { reliability: None, schema_version: 3, report_id: uuid::Uuid::new_v4().to_string(), installation_id: installation_id.into(),
        prepared_at: at, range_from: at - 7 * 86400, range_to: at, history_limited: limited, environment, errors };
    if !report.errors.is_empty() { report.validate().map_err(|_| "脱敏错误报告校验失败")?; }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Store;
    fn log(status: &str, at: i64) -> RequestLog {
        RequestLog { id: uuid::Uuid::new_v4().to_string(), at, account: Some("private-account@example.com".into()), conversation_key: Some("private-thread".into()),
            started_at: None, requested_model: Some("private-prompt".into()), response_model: None, operation: "responses".into(), status: status.into(), elapsed_ms: 42,
            advice: "Bearer private-token; private-response".into(), usage: None, diagnostic: None }
    }
    fn environment() -> Environment { Environment { app_version: "0.1.34".into(), os: "Windows 11".into(), os_build: Some(26100), architecture: "x64".into(), webview2_version: None, desktop_version: None, engine_version: None, uptime_seconds: 2000 } }
    #[test]
    fn errors_only_closed_projection_and_frozen_encoding() {
        let at = crate::now();
        let logs: Vec<_> = ["success", "cancelled", "service_stopped", "network_error", "upstream_error", "stream_interrupted"].into_iter().map(|s| log(s, at)).collect();
        let report = prepare(&logs, &uuid::Uuid::new_v4().to_string(), environment(), at, false).unwrap();
        assert_eq!(report.errors.len(), 3);
        let encoded = report.encode().unwrap();
        let decoded = Report::decode(&encoded).unwrap();
        assert_eq!(decoded.report_id, report.report_id);
        let json = serde_json::to_string(&decoded).unwrap();
        for private in ["private-", "Bearer", "advice", "requestedModel", "usage"] { assert!(!json.contains(private)); }
        assert_eq!(decoded.errors[0].account_alias, Some(1));
        let mut invalid = serde_json::to_value(&report).unwrap(); invalid["errors"][0]["prompt"] = "private-prompt".into();
        assert!(serde_json::from_value::<Report>(invalid).is_err());
    }
    #[test]
    fn scans_beyond_two_hundred_and_never_uploads_successful_history() {
        let dir = tempfile::tempdir().unwrap(); let store = Store::open(&dir.path().join("pool.sqlite")).unwrap(); let at = crate::now();
        store.log(&log("network_error", at - 10)).unwrap();
        for _ in 0..210 { store.log(&log("success", at)).unwrap(); }
        assert!(store.logs().unwrap().iter().all(|l| l.status == "success"));
        let (logs, _) = store.error_report_records(at).unwrap(); assert_eq!(logs.len(), 1);
        store.log(&log("upstream_error", at - 8 * 86400)).unwrap(); assert_eq!(store.error_report_records(at).unwrap().0.len(), 1);
    }
    #[test]
    fn no_errors_does_not_create_a_wire_report() {
        let at = crate::now(); let report = prepare(&[log("success", at)], &uuid::Uuid::new_v4().to_string(), environment(), at, false).unwrap();
        assert!(report.errors.is_empty()); assert!(report.encode().is_err());
    }
}
