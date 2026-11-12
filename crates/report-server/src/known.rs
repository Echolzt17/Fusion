//! A caller can check only random identifiers/fingerprints it already possesses.
//! No administrator credential, discovery query or report data is exposed here.
use super::*;
use fusion_report_protocol::{reliability_bucket_fingerprint, KnownRequest, KnownResponse, ReliabilityHour, ReliabilitySummary, KNOWN_SCHEMA_VERSION, KNOWN_JSON_LIMIT};

struct ReadBudget<'a>(&'a Connection);
impl Drop for ReadBudget<'_> {
    fn drop(&mut self) { self.0.progress_handler(0,None::<fn()->bool>); }
}
impl Store {
    fn known(&self, request: &KnownRequest) -> Result<KnownResponse> {
        request.validate()?;
        // Public checks must not monopolize the receiver's serialized connection.
        // The guard removes the deadline even if a query fails or is interrupted.
        let started=std::time::Instant::now();
        self.db.progress_handler(10_000,Some(move || started.elapsed()>std::time::Duration::from_secs(5)));
        let _budget=ReadBudget(&self.db);
        let mut response = KnownResponse { schema_version: KNOWN_SCHEMA_VERSION, installation_id: request.installation_id.clone(), request_ids: vec![], bucket_indexes: vec![] };
        let mut find = self.db.prepare("SELECT EXISTS(SELECT 1 FROM errors WHERE installation=?1 AND request=?2)").map_err(|_| "database_read_failed")?;
        for id in &request.request_ids {
            if find.query_row(params![request.installation_id, id], |r| r.get::<_, bool>(0)).map_err(|_| "database_read_failed")? {
                response.request_ids.push(id.clone());
            }
        }
        if !request.buckets.is_empty() {
            // Match the administrator reliability reader's latest-snapshot ordering,
            // but restrict every output to a supplied key and exact fingerprint.
            let candidates = serde_json::to_string(&request.buckets).map_err(|_| "invalid_query")?;
            let sql = "WITH candidates AS MATERIALIZED (
                SELECT cast(key AS INTEGER) candidate,json_extract(value,'$.hour') hour,
                    json_extract(value,'$.appVersion') app_version,json_extract(value,'$.operation') operation FROM json_each(?2)),
                snapshots AS (
                SELECT q.candidate,
                    json_extract(r.body,'$.reliability.coverageStart') coverage,
                    json_extract(r.body,'$.reliability.historyPartial') partial,
                    b.value bucket,
                    row_number() OVER (PARTITION BY q.candidate ORDER BY json_extract(r.body,'$.reliability.snapshotAt') DESC,
                        (json_extract(b.value,'$.success')+json_extract(b.value,'$.failure')+json_extract(b.value,'$.cancelled')+json_extract(b.value,'$.stopped')) DESC,r.received DESC,r.id DESC) rank
                FROM reports r CROSS JOIN json_each(r.body,'$.reliability.buckets') b
                JOIN candidates q ON json_extract(b.value,'$.hour')=q.hour
                    AND json_extract(b.value,'$.appVersion')=q.app_version AND json_extract(b.value,'$.operation')=q.operation
                WHERE json_extract(r.body,'$.installationId')=?1 AND json_extract(r.body,'$.preparedAt')<=?3)
                SELECT candidate,coverage,partial,bucket FROM snapshots WHERE rank=1 ORDER BY candidate";
            let mut statement = self.db.prepare(sql).map_err(|_| "database_read_failed")?;
            let rows = statement.query_map(params![request.installation_id,candidates,now()+300], |r| Ok((r.get::<_,usize>(0)?, r.get::<_,i64>(1)?, r.get::<_,bool>(2)?, r.get::<_,String>(3)?))).map_err(|_| "database_read_failed")?;
            for row in rows {
                let (index,coverage_start,history_partial,bucket) = row.map_err(|_| "database_read_failed")?;
                let bucket: ReliabilityHour = serde_json::from_str(&bucket).map_err(|_| "database_read_failed")?;
                let summary = ReliabilitySummary { coverage_start, history_partial, snapshot_at: 0, buckets: vec![] };
                if reliability_bucket_fingerprint(&summary,&bucket) == request.buckets[index].fingerprint {
                    response.bucket_indexes.push(index);
                }
            }
        }
        Ok(response)
    }
}

pub(super) async fn check(State(s): State<Arc<Server>>, Extension(permit): Extension<Arc<tokio::sync::OwnedSemaphorePermit>>, headers: HeaderMap, bytes: Bytes) -> Response {
    if headers.get("content-type").and_then(|v|v.to_str().ok()).and_then(|s|s.split(';').next()).map(str::trim) != Some("application/json") {
        return failure("invalid_query");
    }
    if bytes.len() > KNOWN_JSON_LIMIT { return failure("report_too_large"); }
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let request: KnownRequest = serde_json::from_slice(&bytes).map_err(|_| "invalid_query")?;
        request.validate()?;
        s.store.lock().map_err(|_| "database_lock_failed")?.known(&request)
    }).await;
    match result {
        Ok(Ok(value)) => ([ ("cache-control", "no-store") ], Json(value)).into_response(),
        Ok(Err(code)) => failure(code), _ => failure("database_read_failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fusion_report_protocol::KnownBucket;
    #[test]
    fn legacy_candidates_are_scoped_and_checks_never_write_or_enumerate() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path(),32*1024*1024).unwrap();
        let at = now(); let report = super::super::tests::report(at,2);
        store.ingest(&report,at).unwrap();
        let unknown = uuid::Uuid::new_v4().to_string();
        let mut request = KnownRequest { schema_version:1,installation_id:report.installation_id.clone(),request_ids:vec![unknown.clone(),report.errors[1].request_id.clone()],buckets:vec![] };
        let result = store.known(&request).unwrap();
        assert_eq!(result.request_ids,vec![report.errors[1].request_id.clone()]);
        result.validate_for(&request).unwrap();
        let text = serde_json::to_string(&result).unwrap();
        for private in [&report.errors[0].request_id,"environment","code","accountAlias","reportId"] { assert!(!text.contains(private)); }
        request.installation_id=uuid::Uuid::new_v4().to_string();
        assert!(store.known(&request).unwrap().request_ids.is_empty());
        request.installation_id=report.installation_id.clone(); request.request_ids.clear();
        assert!(store.known(&request).unwrap().request_ids.is_empty());
        assert_eq!(store.db.query_row("SELECT count(*) FROM reports",[],|r|r.get::<_,usize>(0)).unwrap(),1);
        assert_eq!(store.db.query_row("SELECT count(*) FROM errors",[],|r|r.get::<_,usize>(0)).unwrap(),2);
        assert_eq!(store.events(0).unwrap().len(),1);
    }
    #[test]
    fn only_latest_complete_bucket_fingerprints_match_not_stale_or_changed_coverage() {
        let dir = tempfile::tempdir().unwrap(); let mut store=Store::open(dir.path(),32*1024*1024).unwrap();
        let at=now(); let mut report=super::super::tests::report(at,0); report.schema_version=3;
        let bucket=ReliabilityHour {hour:at.div_euclid(3600)*3600,app_version:"1.0.13".into(),operation:"responses".into(),success:1,success_by_size:[1,0,0,0,0],elapsed_histogram:[1,0,0,0,0,0],..Default::default()};
        report.reliability=Some(ReliabilitySummary {coverage_start:at-10,snapshot_at:at,history_partial:true,buckets:vec![bucket]});
        store.ingest(&report,at).unwrap();
        let old=report.clone();
        let mut request=KnownRequest {schema_version:1,installation_id:report.installation_id.clone(),request_ids:vec![],buckets:vec![KnownBucket::from_summary(report.reliability.as_ref().unwrap(),&report.reliability.as_ref().unwrap().buckets[0])]};
        assert_eq!(store.known(&request).unwrap().bucket_indexes,vec![0]);
        report.report_id=uuid::Uuid::new_v4().to_string();
        let summary=report.reliability.as_mut().unwrap();summary.snapshot_at=at+1;report.prepared_at=at+1;report.range_from=report.prepared_at-7*86400;report.range_to=report.prepared_at;
        summary.buckets[0].success=2;summary.buckets[0].success_by_size[0]=2;summary.buckets[0].elapsed_histogram[0]=2;
        store.ingest(&report,at+1).unwrap();
        let mut stale=old;stale.report_id=uuid::Uuid::new_v4().to_string();store.ingest(&stale,at+2).unwrap();
        assert!(store.known(&request).unwrap().bucket_indexes.is_empty());
        let summary=report.reliability.as_ref().unwrap();request.buckets[0]=KnownBucket::from_summary(summary,&summary.buckets[0]);
        assert_eq!(store.known(&request).unwrap().bucket_indexes,vec![0]);
        let mut changed=summary.clone();changed.history_partial=false;request.buckets[0]=KnownBucket::from_summary(&changed,&changed.buckets[0]);
        assert!(store.known(&request).unwrap().bucket_indexes.is_empty());
    }
}
