//! Discover existing reports without depending on notification thresholds or client receipts.
use super::*;

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ReportQuery {
    from: Option<i64>,
    until: Option<i64>,
    before: Option<i64>,
    before_id: Option<String>,
    limit: Option<u32>,
}
impl Store {
    fn list_reports(&self, query: ReportQuery) -> Result<Value> {
        let from = query.from.unwrap_or(0);
        let until = query.until.unwrap_or_else(now);
        let limit = query.limit.unwrap_or(50);
        if from < 0 || until < from || !(1..=50).contains(&limit)
            || query.before.is_some() != query.before_id.is_some()
            || query.before.is_some_and(|n| n < 0)
            || query.before_id.as_deref().is_some_and(|id| !fusion_report_protocol::uuid_ok(id)) {
            return Err("invalid_query");
        }
        let mut st = self.db.prepare("SELECT id,received,
            json_extract(body,'$.installationId'),json_extract(body,'$.preparedAt'),
            json_extract(body,'$.environment.appVersion'),json_extract(body,'$.environment.os'),
            json_array_length(body,'$.errors'),json_extract(receipt,'$.acceptedErrors'),
            json_extract(receipt,'$.duplicateErrors'),json_extract(body,'$.historyLimited')
            FROM reports WHERE received>=?1 AND received<=?2
            AND (?3 IS NULL OR received<?3 OR (received=?3 AND id<?4))
            ORDER BY received DESC,id DESC LIMIT ?5").map_err(|_| "database_read_failed")?;
        let rows = st.query_map(params![from, until, query.before, query.before_id, limit + 1], |r| {
            let installation: String = r.get(2)?;
            // Stable pseudonym of an existing random installation UUID, never a hardware identity.
            let device = format!("device-{:x}", Sha256::digest(installation.as_bytes()));
            Ok(json!({"reportId":r.get::<_,String>(0)?,"receivedAt":r.get::<_,i64>(1)?,
                "deviceAlias":device,"preparedAt":r.get::<_,i64>(3)?,"reportingVersion":r.get::<_,String>(4)?,
                "os":r.get::<_,String>(5)?,"errorCount":r.get::<_,u64>(6)?,
                "acceptedErrors":r.get::<_,u64>(7)?,"duplicateErrors":r.get::<_,u64>(8)?,
                "historyLimited":r.get::<_,bool>(9)?}))
        }).map_err(|_| "database_read_failed")?;
        let mut reports = rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|_| "database_read_failed")?;
        let more = reports.len() > limit as usize;
        reports.truncate(limit as usize);
        let next = if more { reports.last().map(|r| json!({"before":r["receivedAt"],"beforeId":r["reportId"]})) } else { None };
        Ok(json!({"reports":reports,"next":next,"from":from,"until":until}))
    }
}
pub(super) async fn list(State(s): State<Arc<Server>>, headers: HeaderMap, Query(query): Query<ReportQuery>) -> Response {
    if !s.authorized(&headers) { return failure("unauthorized"); }
    let result = tokio::task::spawn_blocking(move || s.store.lock().map_err(|_| "database_lock_failed")?.list_reports(query)).await;
    match result {
        Ok(Ok(page)) => ([("cache-control", "no-store")], Json(page)).into_response(),
        Ok(Err(e)) => failure(e), _ => failure("database_read_failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pages_find_old_reports_without_events_and_keep_device_and_unknown_versions() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path(), 32*1024*1024).unwrap();
        let at = now();
        let mut r = super::super::tests::report(at, 1);
        for id in ["00000000-0000-4000-8000-000000000001", "00000000-0000-4000-8000-000000000002", "00000000-0000-4000-8000-000000000003"] {
            r.report_id = id.into(); store.ingest(&r, at).unwrap();
        }
        assert!(store.events(0).unwrap().is_empty());
        // Emulate old cached version; summaries must still use original null.
        store.db.execute("UPDATE errors SET version='0.1.34'", []).unwrap();
        assert_eq!(store.groups(at-1, at, false).unwrap()[0]["versions"], "unknown");
        let first = store.list_reports(ReportQuery { limit:Some(2), ..Default::default() }).unwrap();
        assert_eq!(first["reports"].as_array().unwrap().len(), 2);
        assert_eq!(first["reports"][0]["deviceAlias"], first["reports"][1]["deviceAlias"]);
        assert_eq!(first["reports"][0]["duplicateErrors"], 1);
        let next = &first["next"];
        let last = store.list_reports(ReportQuery { before:next["before"].as_i64(), before_id:next["beforeId"].as_str().map(str::to_owned), ..Default::default() }).unwrap();
        assert_eq!(last["reports"].as_array().unwrap().len(), 1);
        assert_eq!(last["reports"][0]["acceptedErrors"], 1);
        assert!(last["next"].is_null());
        let text = first.to_string();
        for forbidden in [&r.installation_id, "requestId", "errors\":", "body", "accountAlias"] { assert!(!text.contains(forbidden)); }
        assert!(store.list_reports(ReportQuery { limit:Some(51), ..Default::default() }).is_err());
        assert!(store.list_reports(ReportQuery { before:Some(at), ..Default::default() }).is_err());
        assert!(store.list_reports(ReportQuery { from:Some(at+1), until:Some(at), ..Default::default() }).is_err());
        assert!(store.list_reports(ReportQuery { from:Some(at+1), until:Some(at+2), ..Default::default() }).unwrap()["reports"].as_array().unwrap().is_empty());
    }
}
