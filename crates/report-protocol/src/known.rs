//! Non-enumerating candidate checks. These random identifiers are not account authentication.
use crate::{uuid_ok, version, ReliabilityHour, ReliabilitySummary, RECORD_LIMIT};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

pub const KNOWN_SCHEMA_VERSION: u8 = 1;
pub const KNOWN_JSON_LIMIT: usize = 1024 * 1024;
pub const KNOWN_BUCKET_LIMIT: usize = 4096;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KnownRequest {
    pub schema_version: u8,
    pub installation_id: String,
    pub request_ids: Vec<String>,
    pub buckets: Vec<KnownBucket>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KnownBucket {
    pub hour: i64,
    pub app_version: String,
    pub operation: String,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KnownResponse {
    pub schema_version: u8,
    pub installation_id: String,
    pub request_ids: Vec<String>,
    pub bucket_indexes: Vec<usize>,
}

/// Stable numeric snapshot identity; a fresh observation time alone is not new information.
pub fn reliability_bucket_fingerprint(summary: &ReliabilitySummary, bucket: &ReliabilityHour) -> String {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Fingerprint<'a> {
        coverage_start: i64,
        history_partial: bool,
        bucket: &'a ReliabilityHour,
    }
    // This fixed, closed DTO contains only integers, booleans, arrays and strings.
    let bytes = serde_json::to_vec(&Fingerprint {
        coverage_start: summary.coverage_start,
        history_partial: summary.history_partial,
        bucket,
    }).expect("closed reliability fingerprint is serializable");
    format!("{:x}", Sha256::digest(bytes))
}

impl KnownBucket {
    pub fn from_summary(summary: &ReliabilitySummary, bucket: &ReliabilityHour) -> Self {
        Self { hour: bucket.hour, app_version: bucket.app_version.clone(), operation: bucket.operation.clone(),
            fingerprint: reliability_bucket_fingerprint(summary, bucket) }
    }
}
impl KnownRequest {
    pub fn validate(&self) -> Result<(), &'static str> {
        let mut ids = HashSet::new();
        let mut keys = HashSet::new();
        if self.schema_version != KNOWN_SCHEMA_VERSION || !uuid_ok(&self.installation_id)
            || self.request_ids.len() > RECORD_LIMIT || self.buckets.len() > KNOWN_BUCKET_LIMIT
            || self.request_ids.iter().any(|id| !uuid_ok(id) || !ids.insert(id))
            || self.buckets.iter().any(|b| b.hour <= 0 || b.hour % 3600 != 0
                || version(&b.app_version).as_deref() != Some(b.app_version.as_str())
                || !["responses", "compact", "image_generate", "image_edit"].contains(&b.operation.as_str())
                || b.fingerprint.len() != 64 || !b.fingerprint.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                || !keys.insert((b.hour, &b.app_version, &b.operation))) {
            return Err("invalid_query");
        }
        Ok(())
    }
}
impl KnownResponse {
    pub fn validate_for(&self, request: &KnownRequest) -> Result<(), &'static str> {
        request.validate()?;
        let candidates: HashSet<_> = request.request_ids.iter().collect();
        let mut ids = HashSet::new();
        let mut indexes = HashSet::new();
        if self.schema_version != KNOWN_SCHEMA_VERSION || self.installation_id != request.installation_id
            || self.request_ids.len() > request.request_ids.len() || self.bucket_indexes.len() > request.buckets.len()
            || self.request_ids.iter().any(|id| !candidates.contains(id) || !ids.insert(id))
            || self.bucket_indexes.iter().any(|&i| i >= request.buckets.len() || !indexes.insert(i)) {
            return Err("invalid_known_response");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fingerprints_ignore_observation_time_but_include_all_values_and_coverage() {
        let bucket = ReliabilityHour { hour: 3600, app_version: "1.0.13".into(), operation: "responses".into(), success: 1, ..Default::default() };
        let mut summary = ReliabilitySummary { coverage_start: 1, snapshot_at: 3601, history_partial: true, buckets: vec![bucket.clone()] };
        let original = reliability_bucket_fingerprint(&summary, &bucket);
        summary.snapshot_at += 1;
        assert_eq!(original, reliability_bucket_fingerprint(&summary, &bucket));
        summary.history_partial = false;
        assert_ne!(original, reliability_bucket_fingerprint(&summary, &bucket));
        summary.history_partial = true; summary.coverage_start = 2;
        assert_ne!(original, reliability_bucket_fingerprint(&summary, &bucket));
        summary.coverage_start = 1;
        let mut changed = bucket.clone(); changed.elapsed_histogram[5] = 1;
        assert_ne!(original, reliability_bucket_fingerprint(&summary, &changed));
    }
    #[test]
    fn candidate_and_response_validation_rejects_enumeration_and_invalid_subsets() {
        let mut request = KnownRequest { schema_version: 1, installation_id: uuid::Uuid::new_v4().to_string(), request_ids: vec![uuid::Uuid::new_v4().to_string()], buckets: vec![] };
        let mut response = KnownResponse { schema_version: 1, installation_id: request.installation_id.clone(), request_ids: request.request_ids.clone(), bucket_indexes: vec![] };
        assert!(response.validate_for(&request).is_ok());
        response.request_ids.push(uuid::Uuid::new_v4().to_string()); assert!(response.validate_for(&request).is_err());
        response.request_ids = request.request_ids.clone(); response.bucket_indexes.push(0); assert!(response.validate_for(&request).is_err());
        response.bucket_indexes.clear(); response.installation_id = uuid::Uuid::new_v4().to_string(); assert!(response.validate_for(&request).is_err());
        request.request_ids.push(request.request_ids[0].clone()); assert!(request.validate().is_err());
        request.request_ids = vec![uuid::Uuid::new_v4().to_string(); RECORD_LIMIT + 1]; assert!(request.validate().is_err());
        let value = serde_json::json!({"schemaVersion":1,"installationId":uuid::Uuid::new_v4().to_string(),"requestIds":[],"buckets":[],"body":"private"});
        assert!(serde_json::from_value::<KnownRequest>(value).is_err());
    }
}
