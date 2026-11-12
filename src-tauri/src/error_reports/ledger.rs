//! Private durable acknowledgements. This file never becomes an IPC DTO.
use super::{atomic, read_legacy_history, valid_receipt};
use fusion_report_protocol::{reliability_bucket_fingerprint, KnownRequest, KnownResponse, Receipt, Report};
use pool_core::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::{BTreeMap, HashSet}, path::Path};

const VERSION: u32 = 1;
const FILE: &str = "acknowledgements.json";
const FILE_LIMIT: u64 = 6 * 1024 * 1024;
const ERROR_LIMIT: usize = 16384;
const BUCKET_LIMIT: usize = 32768;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Completed {
    digest: String,
    error_count: usize,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Ledger {
    version: u32,
    installation_id: String,
    errors: BTreeMap<String, i64>,
    buckets: BTreeMap<String, i64>,
    history: Vec<Receipt>,
    completed: BTreeMap<String, Completed>,
}

fn hash_ok(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(super) fn digest(bytes: &[u8]) -> String { format!("{:x}", Sha256::digest(bytes)) }

impl Ledger {
    pub(super) fn load(root: &Path, installation_id: &str) -> Result<Self> {
        let path = root.join(FILE);
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self { version: VERSION, installation_id: installation_id.into(),
                    errors: BTreeMap::new(), buckets: BTreeMap::new(),
                    history: read_legacy_history(root)?, completed: BTreeMap::new() });
            }
            Err(_) => return Err("无法读取内部上报状态，原文件已保留".into()),
        };
        if metadata.len() > FILE_LIMIT { return Err("内部上报状态超过限制，原文件已保留".into()); }
        let ledger: Self = serde_json::from_slice(&std::fs::read(path).map_err(|_| "无法读取内部上报状态")?)
            .map_err(|_| "内部上报状态损坏，原文件已保留")?;
        ledger.validate(installation_id)?;
        Ok(ledger)
    }
    fn validate(&self, installation_id: &str) -> Result<()> {
        if self.version > VERSION { return Err("内部上报状态来自更新版本，未覆盖原文件".into()); }
        let mut ids = HashSet::new();
        if self.version != VERSION || !fusion_report_protocol::uuid_ok(&self.installation_id)
            || self.installation_id != installation_id || self.errors.len() > ERROR_LIMIT
            || self.buckets.len() > BUCKET_LIMIT || self.history.len() > 10 || self.completed.len() > 10
            || self.errors.iter().any(|(id, at)| !fusion_report_protocol::uuid_ok(id) || *at <= 0)
            || self.buckets.iter().any(|(fingerprint, hour)| !hash_ok(fingerprint) || *hour <= 0 || hour % 3600 != 0)
            || self.history.iter().any(|receipt| !valid_receipt(receipt) || !ids.insert(&receipt.report_id))
            || self.completed.iter().any(|(id, entry)| !fusion_report_protocol::uuid_ok(id) || !hash_ok(&entry.digest)
                || !self.history.iter().any(|r| &r.report_id == id
                    && r.accepted_errors.checked_add(r.duplicate_errors) == Some(entry.error_count))) {
            return Err("内部上报状态无效或安装标识不匹配，原文件已保留".into());
        }
        Ok(())
    }
    pub(super) fn save(&self, root: &Path) -> Result<()> {
        self.validate(&self.installation_id)?;
        let bytes = serde_json::to_vec(self).map_err(|_| "无法保存内部上报状态")?;
        if bytes.len() > FILE_LIMIT as usize { return Err("内部上报状态已达容量上限，未丢弃已确认记录".into()); }
        atomic(&root.join(FILE), &bytes)
    }
    pub(super) fn history(&self) -> Vec<Receipt> { self.history.clone() }
    pub(super) fn prune(&mut self, at: i64) {
        let earliest = at.saturating_sub(7 * 86400);
        self.errors.retain(|_, time| *time >= earliest);
        self.buckets.retain(|_, hour| *hour >= earliest.div_euclid(3600) * 3600);
    }
    pub(super) fn filter(&self, report: &mut Report) {
        report.errors.retain(|error| !self.errors.contains_key(&error.request_id));
        if let Some(summary) = &mut report.reliability {
            let fingerprints: Vec<_> = summary.buckets.iter()
                .map(|bucket| reliability_bucket_fingerprint(summary, bucket)).collect();
            let mut index = 0;
            summary.buckets.retain(|_| { let known = self.buckets.contains_key(&fingerprints[index]); index += 1; !known });
        }
    }
    pub(super) fn confirm_known(&mut self, report: &Report, request: &KnownRequest, response: &KnownResponse) -> Result<()> {
        response.validate_for(request).map_err(|_| "已上报核对结果与当前预览不匹配，未上传")?;
        for error in &report.errors {
            if response.request_ids.contains(&error.request_id) { self.errors.insert(error.request_id.clone(), error.at); }
        }
        for index in &response.bucket_indexes {
            let bucket = &request.buckets[*index];
            self.buckets.insert(bucket.fingerprint.clone(), bucket.hour);
        }
        self.validate(&self.installation_id)
    }
    pub(super) fn confirm_report(&mut self, report: &Report, bytes: &[u8], receipt: &Receipt) -> Result<()> {
        if report.installation_id != self.installation_id || receipt.report_id != report.report_id
            || !valid_receipt(receipt)
            || receipt.accepted_errors.checked_add(receipt.duplicate_errors) != Some(report.errors.len()) {
            return Err("上报回执与待提交报告不匹配".into());
        }
        for error in &report.errors { self.errors.insert(error.request_id.clone(), error.at); }
        if let Some(summary) = &report.reliability {
            for bucket in &summary.buckets {
                self.buckets.insert(reliability_bucket_fingerprint(summary, bucket), bucket.hour);
            }
        }
        self.history.retain(|r| r.report_id != receipt.report_id);
        self.history.push(receipt.clone());
        self.history.sort_by(|a, b| b.received_at.cmp(&a.received_at).then_with(|| b.report_id.cmp(&a.report_id)));
        self.history.truncate(10);
        self.completed.insert(report.report_id.clone(), Completed { digest: digest(bytes), error_count: report.errors.len() });
        self.completed.retain(|id, _| self.history.iter().any(|receipt| &receipt.report_id == id));
        self.validate(&self.installation_id)
    }
    pub(super) fn completed(&self, report: &Report, bytes: &[u8]) -> Result<Option<Receipt>> {
        let Some(entry) = self.completed.get(&report.report_id) else { return Ok(None) };
        if report.installation_id != self.installation_id || entry.digest != digest(bytes)
            || entry.error_count != report.errors.len() { return Err("待收尾报告与内部回执不匹配，原文件已保留".into()); }
        Ok(self.history.iter().find(|receipt| receipt.report_id == report.report_id).cloned())
    }
}

pub(super) fn exists(root: &Path) -> bool { root.join(FILE).exists() }
