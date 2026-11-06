//! Read overlapping numeric snapshots without summing the same hour twice.
use super::*;
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct SummaryQuery {
    from: Option<i64>,
    until: Option<i64>,
    offset: Option<u64>,
    limit: Option<u64>,
}
impl Store {
    fn reliability_page(&self, q: SummaryQuery) -> Result<Value> {
        let until = q.until.unwrap_or_else(now);
        let from = q.from.unwrap_or(until - 7 * 86400).max(0);
        let limit = q.limit.unwrap_or(50);
        let offset = q.offset.unwrap_or(0);
        if from > until
            || until - from > 8 * 86400
            || limit == 0
            || limit > 200
            || offset > 1_000_000
        {
            return Err("invalid_query");
        }
        let sql="WITH snapshots AS (
            SELECT json_extract(r.body,'$.installationId') device,
                json_extract(r.body,'$.reliability.snapshotAt') snapshot,
                json_extract(r.body,'$.reliability.historyPartial') partial,
                b.value bucket, r.received received, r.id report_id,
                row_number() OVER (PARTITION BY json_extract(r.body,'$.installationId'),json_extract(b.value,'$.hour'),json_extract(b.value,'$.appVersion'),json_extract(b.value,'$.operation')
                  ORDER BY json_extract(r.body,'$.reliability.snapshotAt') DESC,
                    (json_extract(b.value,'$.success')+json_extract(b.value,'$.failure')+json_extract(b.value,'$.cancelled')+json_extract(b.value,'$.stopped')) DESC,r.received DESC,r.id DESC) rank
            FROM reports r,json_each(r.body,'$.reliability.buckets') b
            WHERE json_extract(b.value,'$.hour')>=?1 AND json_extract(b.value,'$.hour')<=?2
              AND json_extract(r.body,'$.preparedAt')<=?3)
            SELECT device,snapshot,partial,bucket,report_id FROM snapshots WHERE rank=1
            ORDER BY device,json_extract(bucket,'$.hour'),json_extract(bucket,'$.appVersion'),json_extract(bucket,'$.operation') LIMIT ?4 OFFSET ?5";
        let mut st = self.db.prepare(sql).map_err(|_| "database_read_failed")?;
        let rows=st.query_map(params![from.div_euclid(3600)*3600,until,now()+300,limit+1,offset],|r| {
            let device:String=r.get(0)?; let bucket:String=r.get(3)?;
            Ok(json!({"deviceAlias":format!("device-{:x}",Sha256::digest(device.as_bytes())),
                "snapshotAt":r.get::<_,i64>(1)?,"historyPartial":r.get::<_,bool>(2)?,
                "bucket":serde_json::from_str::<Value>(&bucket).unwrap_or(Value::Null),"reportId":r.get::<_,String>(4)?}))
        }).map_err(|_|"database_read_failed")?;
        let mut rows = rows
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|_| "database_read_failed")?;
        let more = rows.len() > limit as usize;
        rows.truncate(limit as usize);
        Ok(
            json!({"source":"unverified_client_reports","from":from,"until":until,"buckets":rows,"nextOffset":more.then_some(offset+limit)}),
        )
    }
}
pub(super) async fn list(
    State(s): State<Arc<Server>>,
    headers: HeaderMap,
    Query(q): Query<SummaryQuery>,
) -> Response {
    if !s.authorized(&headers) {
        return failure("unauthorized");
    }
    match tokio::task::spawn_blocking(move || {
        s.store
            .lock()
            .map_err(|_| "database_lock_failed")?
            .reliability_page(q)
    })
    .await
    {
        Ok(Ok(page)) => ([("cache-control", "no-store")], Json(page)).into_response(),
        Ok(Err(e)) => failure(e),
        Err(_) => failure("database_read_failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fusion_report_protocol::{ReliabilityHour, ReliabilitySummary};
    #[test]
    fn summary_only_reports_keep_old_receipts_and_merge_overlapping_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path(), 32 * 1024 * 1024).unwrap();
        let at = now();
        let mut r = super::super::tests::report(at, 0);
        r.schema_version = 3;
        let bucket = ReliabilityHour {
            hour: at.div_euclid(3600) * 3600,
            app_version: "1.0.6".into(),
            operation: "responses".into(),
            success: 1,
            success_by_size: [0, 1, 0, 0, 0],
            elapsed_histogram: [1, 0, 0, 0, 0, 0],
            ..Default::default()
        };
        r.reliability = Some(ReliabilitySummary {
            coverage_start: at,
            snapshot_at: at,
            history_partial: true,
            buckets: vec![bucket],
        });
        let receipt = store.ingest(&r, at).unwrap();
        assert_eq!(receipt.accepted_errors, 0);
        assert_eq!(store.ingest(&r, at + 1).unwrap().received_at, at);
        let old = r.clone();
        r.report_id = uuid::Uuid::new_v4().to_string();
        let b = &mut r.reliability.as_mut().unwrap().buckets[0];
        b.success = 2;
        b.success_by_size[1] = 2;
        b.elapsed_histogram[0] = 2;
        store.ingest(&r, at + 2).unwrap();
        let mut stale = old;
        stale.report_id = uuid::Uuid::new_v4().to_string();
        store.ingest(&stale, at + 3).unwrap();
        let page = store.reliability_page(SummaryQuery::default()).unwrap();
        assert!(page["nextOffset"].is_null());
        assert_eq!(page["buckets"].as_array().unwrap().len(), 1);
        assert_eq!(page["buckets"][0]["bucket"]["success"], 2);
        assert!(!page.to_string().contains(&r.installation_id));
        assert!(store.events(0).unwrap().is_empty());
        let mut old = super::super::tests::report(at, 1);
        let original = serde_json::to_vec(&old).unwrap();
        store.ingest(&old, at).unwrap();
        assert_eq!(
            original,
            serde_json::to_vec(&Report::decode(&old.encode().unwrap()).unwrap()).unwrap()
        );
        old.reliability = r.reliability.clone();
        assert!(old.encode().is_err());
        r.reliability.as_mut().unwrap().buckets[0].success_by_size[1] = 1;
        assert!(r.encode().is_err());
    }
}
