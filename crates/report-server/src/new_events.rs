//! Bounded immediate notices for newly inserted samples, never for overlapping uploads.
use super::*;
use fusion_report_protocol::ErrorRecord;
use std::collections::{BTreeMap, BTreeSet};

const GROUP_LIMIT: usize = 16;
pub(super) const EVENT_JSON_LIMIT: usize = 16 * 1024;
struct Group<'a> { errors: Vec<&'a ErrorRecord> }

/// Preserve samples that the legacy receiver had not yet included in a notice.
/// The old event boundary and all compensating notices commit together, once.
pub(super) fn initialize(db: &mut Connection, at: i64) -> Result<()> {
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|_| "database_write_failed")?;
    let existing: Option<i64> = tx.query_row("SELECT value FROM state WHERE key='immediate_errors_after_cursor'", [], |r| r.get(0))
        .optional().map_err(|_| "database_read_failed")?;
    if existing.is_some() { return Ok(()); }
    let boundary: i64 = tx.query_row("SELECT coalesce(max(id),0) FROM events", [], |r| r.get(0)).map_err(|_| "database_read_failed")?;
    {
        // Daily covers receive times through its committed cutoff. An early notice
        // covers only samples already received in its actual occurrence-time window;
        // historical or uncertain-time samples must not disappear with their signature.
        let mut statement = tx.prepare("SELECT e.report,e.record FROM errors e
            LEFT JOIN state early ON early.key='early:'||e.signature
            WHERE e.received>coalesce((SELECT value FROM state WHERE key='daily_cutoff'),0)
              AND NOT (early.value IS NOT NULL AND e.received<=early.value
                AND e.occurred>0 AND e.occurred>early.value-86400 AND e.occurred<=early.value)
            ORDER BY e.report,e.installation,e.request").map_err(|_| "database_read_failed")?;
        let rows = statement.query_map([], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).map_err(|_| "database_read_failed")?;
        let mut report_id = String::new();
        let mut errors = Vec::new();
        for row in rows {
            let (id, record) = row.map_err(|_| "database_read_failed")?;
            if report_id != id && !errors.is_empty() {
                emit_legacy(&tx, &report_id, &errors, at)?;
                errors.clear();
            }
            report_id = id;
            errors.push(serde_json::from_str::<ErrorRecord>(&record).map_err(|_| "database_read_failed")?);
        }
        if !errors.is_empty() { emit_legacy(&tx, &report_id, &errors, at)?; }
    }
    tx.execute("INSERT INTO state VALUES('immediate_errors_after_cursor',?1)", [boundary]).map_err(|_| "database_write_failed")?;
    tx.commit().map_err(|_| "database_write_failed")
}

fn emit_legacy(tx: &rusqlite::Transaction<'_>, report_id: &str, errors: &[ErrorRecord], at: i64) -> Result<()> {
    let body: String = tx.query_row("SELECT body FROM reports WHERE id=?1", [report_id], |r| r.get(0)).map_err(|_| "database_read_failed")?;
    let report: Report = serde_json::from_str(&body).map_err(|_| "database_read_failed")?;
    let summary = summary(&report, &errors.iter().collect::<Vec<_>>(), at)?;
    tx.execute("INSERT INTO events(created,kind,body) VALUES(?1,'new_errors',?2)", params![at,summary]).map_err(|_| "database_write_failed")?;
    Ok(())
}

pub(super) fn summary(report: &Report, errors: &[&ErrorRecord], at: i64) -> Result<String> {
    let mut groups: BTreeMap<String,Group<'_>> = BTreeMap::new();
    for error in errors { groups.entry(error.signature()).or_insert_with(|| Group { errors: vec![] }).errors.push(error); }
    let total_groups = groups.len();
    let mut output = Vec::new();
    for (signature,group) in groups.into_iter().take(GROUP_LIMIT) {
        let mut versions = BTreeSet::new();
        let mut uncertain = 0;
        let mut historical = 0;
        let mut occurred = Vec::new();
        for error in &group.errors {
            versions.insert(error.app_version.as_deref().unwrap_or("unknown"));
            if report.prepared_at > at + 300 || error.at > at + 300 { uncertain += 1; }
            else { occurred.push(error.at); historical += usize::from(error.at < at - 86400); }
        }
        let sample = group.errors.iter().map(|e| e.request_id.as_str()).min().unwrap_or_default();
        output.push(json!({"signature":signature,"count":group.errors.len(),"firstAt":occurred.iter().min(),"lastAt":occurred.iter().max(),
            "sampleRequestId":sample,"sampleReportId":report.report_id,
            "versions":versions.iter().take(4).copied().collect::<Vec<_>>().join(","),"versionsComplete":versions.len()<=4,
            "timeUncertainErrors":uncertain,"historicalErrors":historical}));
    }
    loop {
        let shown: u64 = output.iter().filter_map(|g|g["count"].as_u64()).sum();
        let value = json!({"source":"unverified_client_reports","reportId":report.report_id,
            "deviceAlias":format!("device-{:x}",Sha256::digest(report.installation_id.as_bytes())),"acceptedErrors":errors.len(),
            "totalGroups":total_groups,"omittedGroups":total_groups-output.len(),"omittedErrors":errors.len() as u64-shown,
            "summaryComplete":output.len()==total_groups && output.iter().all(|g|g["versionsComplete"]==true),"groups":output});
        let text = serde_json::to_string(&value).map_err(|_| "invalid_report")?;
        if text.len() <= EVENT_JSON_LIMIT { return Ok(text); }
        if output.pop().is_none() { return Err("invalid_report"); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn legacy(store: &Store, cutoff: i64) {
        store.db.execute("DELETE FROM events WHERE kind='new_errors'", []).unwrap();
        store.db.execute("DELETE FROM state WHERE key='immediate_errors_after_cursor'", []).unwrap();
        store.db.execute("INSERT OR REPLACE INTO state VALUES('daily_cutoff',?1)", [cutoff]).unwrap();
    }

    #[test]
    fn upgrade_notifies_single_uncovered_legacy_error_once_and_preserves_old_boundary() {
        let dir = tempfile::tempdir().unwrap(); let at = now();
        let mut store = Store::open(dir.path(),32*1024*1024).unwrap();
        let report = super::super::tests::report(at-10,1);
        store.ingest(&report,at-10).unwrap();
        legacy(&store,at-3600);
        store.db.execute("INSERT INTO events(created,kind,body) VALUES(?1,'daily','{}')", [at-3600]).unwrap();
        let boundary = store.events(0).unwrap()[0]["id"].as_i64().unwrap();
        let original = store.report(&report.report_id).unwrap();
        drop(store);
        let store = Store::open(dir.path(),32*1024*1024).unwrap();
        let events = store.events(0).unwrap();
        assert_eq!(events.len(),2); assert_eq!(events[0]["kind"],"daily");
        assert_eq!(events[1]["kind"],"new_errors");
        assert_eq!(events[1]["summary"]["acceptedErrors"],1);
        assert_eq!(events[1]["summary"]["groups"][0]["sampleRequestId"],report.errors[0].request_id);
        assert_eq!(store.health().unwrap()["immediateErrorsAfterCursor"],boundary);
        assert_eq!(store.report(&report.report_id).unwrap(),original);
        drop(store);
        let store = Store::open(dir.path(),32*1024*1024).unwrap();
        assert_eq!(store.events(0).unwrap(),events);
    }

    #[test]
    fn upgrade_excludes_daily_and_early_covered_but_keeps_late_historical_and_future_samples() {
        let dir = tempfile::tempdir().unwrap(); let at = now(); let early = at-600; let daily = at-7200;
        let mut store = Store::open(dir.path(),32*1024*1024).unwrap();
        let covered = super::super::tests::report(early-100,1);
        store.ingest(&covered,early-100).unwrap();
        let mut historical = super::super::tests::report(early-100,1);
        historical.errors[0].at = early-86401;
        store.ingest(&historical,early-100).unwrap();
        let future = super::super::tests::report(early+7200,1);
        store.ingest(&future,early-100).unwrap();
        let late = super::super::tests::report(early-10,1);
        store.ingest(&late,early+1).unwrap();
        let daily_covered = super::super::tests::report(daily-10,1);
        store.ingest(&daily_covered,daily-10).unwrap();
        legacy(&store,daily);
        store.db.execute("INSERT INTO state VALUES(?1,?2)",params![format!("early:{}",covered.errors[0].signature()),early]).unwrap();
        store.db.execute("INSERT INTO events(created,kind,body) VALUES(?1,'early','{}')", [early]).unwrap();
        new_events::initialize(&mut store.db,at).unwrap();
        let events = store.events(0).unwrap();
        assert_eq!(events.len(),4);
        let notices = events.iter().filter(|e|e["kind"]=="new_errors").collect::<Vec<_>>();
        let samples = notices.iter().map(|e|e["summary"]["groups"][0]["sampleRequestId"].as_str().unwrap()).collect::<BTreeSet<_>>();
        assert_eq!(samples,BTreeSet::from([historical.errors[0].request_id.as_str(),future.errors[0].request_id.as_str(),late.errors[0].request_id.as_str()]));
        assert_eq!(notices.iter().filter_map(|e|e["summary"]["groups"][0]["timeUncertainErrors"].as_u64()).sum::<u64>(),1);
        assert_eq!(notices.iter().filter_map(|e|e["summary"]["groups"][0]["historicalErrors"].as_u64()).sum::<u64>(),1);
    }

    #[test]
    fn upgrade_notice_or_marker_failure_rolls_back_all_notices_and_boundary() {
        let dir = tempfile::tempdir().unwrap(); let at = now();
        let mut store = Store::open(dir.path(),32*1024*1024).unwrap();
        for _ in 0..2 { store.ingest(&super::super::tests::report(at-10,1),at-10).unwrap(); }
        legacy(&store,at-3600);
        for trigger in [
            "CREATE TEMP TRIGGER fail_upgrade BEFORE INSERT ON events WHEN new.kind='new_errors' BEGIN SELECT raise(ABORT,'public-fake-failure'); END;",
            "CREATE TEMP TRIGGER fail_upgrade BEFORE INSERT ON state WHEN new.key='immediate_errors_after_cursor' BEGIN SELECT raise(ABORT,'public-fake-failure'); END;",
        ] {
            store.db.execute_batch(trigger).unwrap();
            assert_eq!(new_events::initialize(&mut store.db,at).unwrap_err(),"database_write_failed");
            assert!(store.events(0).unwrap().is_empty());
            assert_eq!(store.db.query_row("SELECT count(*) FROM state WHERE key='immediate_errors_after_cursor'", [], |r|r.get::<_,usize>(0)).unwrap(),0);
            assert_eq!(store.db.query_row("SELECT count(*) FROM errors", [], |r|r.get::<_,usize>(0)).unwrap(),2);
            store.db.execute_batch("DROP TRIGGER fail_upgrade;").unwrap();
        }
        new_events::initialize(&mut store.db,at).unwrap();
        assert_eq!(store.events(0).unwrap().len(),2);
    }

    #[test]
    fn immediate_summary_is_bounded_and_keeps_exact_totals_when_groups_are_truncated() {
        let at=now();let mut report=super::super::tests::report(at,2000);
        for (i,error) in report.errors.iter_mut().enumerate() {
            error.upstream_status=Some(100+(i%500) as u16);
            error.code=fusion_report_protocol::ERROR_CODES[i/500].into();
        }
        let encoded=summary(&report,&report.errors.iter().collect::<Vec<_>>(),at).unwrap();
        assert!(encoded.len()<=EVENT_JSON_LIMIT);
        let value:Value=serde_json::from_str(&encoded).unwrap();
        assert_eq!(value["acceptedErrors"],2000);assert_eq!(value["totalGroups"],2000);
        assert_eq!(value["omittedErrors"],1984);assert_eq!(value["summaryComplete"],false);
        assert_eq!(value["groups"].as_array().unwrap().len(),GROUP_LIMIT);
        assert!(!encoded.contains(&report.installation_id));
    }
}
