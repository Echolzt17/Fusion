use super::*;
use fusion_report_protocol::{Environment, ErrorRecord};
pub(super) fn report(at: i64, count: usize) -> Report {
    Report { reliability:None, schema_version:1, report_id:uuid::Uuid::new_v4().to_string(), installation_id:uuid::Uuid::new_v4().to_string(), prepared_at:at,
        range_from:at-7*86400,range_to:at,history_limited:false,
        // Keep the actual desktop's multipart prerelease in the HTTP/storage fixtures.
        // Numeric-only fixtures missed the stale receiver that rejected real client reports.
        environment:Environment {app_version:"0.1.34".into(),os:"Windows 11".into(),os_build:Some(26100),architecture:"x64".into(),webview2_version:Some("156.0.4314.58".into()),desktop_version:Some("26.1027.3384.0".into()),engine_version:Some("0.166.0-alpha.5.1".into()),uptime_seconds:2000},
        errors:(0..count).map(|_| ErrorRecord {request_id:uuid::Uuid::new_v4().to_string(),at,operation:"responses".into(),code:"upstream_error".into(),elapsed_ms:180000,account_alias:None,conversation_alias:None,app_version:None,upstream_status:Some(503),failure_stage:Some("upstream_rejected".into()),network_error_kind:None,proxy_source:None,execution:None,details:None}).collect() }
}
#[test]
fn idempotent_receipts_overlap_and_identity_conflict() {
    let dir=tempfile::tempdir().unwrap();let mut s=Store::open(dir.path(),32*1024*1024).unwrap();let at=now();let mut r=report(at,6);
    let a=s.ingest(&r,at).unwrap();assert_eq!(a.accepted_errors,6);
    assert_eq!(s.events(0).unwrap().len(),1);
    assert_eq!(s.events(0).unwrap()[0]["summary"]["acceptedErrors"],6);
    let b=s.ingest(&r,at+3).unwrap();assert_eq!(b.received_at,at);assert_eq!(b.accepted_errors,6);
    r.environment.uptime_seconds+=1;assert_eq!(s.ingest(&r,at).unwrap_err(),"report_id_conflict");
    r.report_id=uuid::Uuid::new_v4().to_string();let c=s.ingest(&r,at).unwrap();assert_eq!(c.accepted_errors,0);assert_eq!(c.duplicate_errors,6);
    assert_eq!(s.events(0).unwrap().len(),1);
    drop(s);let s=Store::open(dir.path(),32*1024*1024).unwrap();assert_eq!(s.report(&r.report_id).unwrap()["errors"].as_array().unwrap().len(),6);
}
#[test]
fn v1_receipts_remain_idempotent_and_v2_accepts_only_finite_new_diagnostics() {
    let dir=tempfile::tempdir().unwrap();let mut store=Store::open(dir.path(),32*1024*1024).unwrap();
    let mut r=report(now(),1);let original=serde_json::to_vec(&r).unwrap();
    store.ingest(&r,now()).unwrap();
    let old=Report::decode(&r.encode().unwrap()).unwrap();assert_eq!(serde_json::to_vec(&old).unwrap(),original);
    assert_eq!(store.ingest(&old,now()+1).unwrap().accepted_errors,1);
    r.report_id=uuid::Uuid::new_v4().to_string();r.errors[0].code="ingress_busy".into();
    r.errors[0].failure_stage=Some("ingress_queue".into());r.errors[0].upstream_status=None;
    r.errors[0].execution=Some(fusion_report_protocol::Execution {local_http_status:Some(503),ingress_queue_ms:Some(12),..Default::default()});
    assert!(r.encode().is_err());r.schema_version=2;
    let decoded=Report::decode(&r.encode().unwrap()).unwrap();assert_eq!(decoded.errors[0].code,"ingress_busy");
    store.ingest(&decoded,now()).unwrap();
    r.errors[0].details=Some(fusion_report_protocol::FailureDetails {upstream_failure:Some("private-message".into()),..Default::default()});
    assert!(r.encode().is_err());
    r.errors[0].details.as_mut().unwrap().upstream_failure=Some("server".into());assert!(r.encode().is_ok());
}
#[test]
fn immediate_errors_do_not_change_legacy_six_alert_or_daily_at_ten_beijing() {
    let dir=tempfile::tempdir().unwrap();let mut s=Store::open(dir.path(),32*1024*1024).unwrap();
    let cutoff=21000*86400+2*3600;let at=cutoff-1800;let mut r=report(at,5);
    s.sweep(at).unwrap();s.ingest(&r,at).unwrap();s.sweep(at+1).unwrap();
    assert_eq!(s.events(0).unwrap().len(),1);assert_eq!(s.events(0).unwrap()[0]["kind"],"new_errors");
    r.report_id=uuid::Uuid::new_v4().to_string();r.errors.push(report(at,1).errors.remove(0));s.ingest(&r,at+10).unwrap();s.sweep(at+11).unwrap();
    let first=s.events(0).unwrap();assert_eq!(first.len(),3);assert_eq!(first[2]["kind"],"early");assert_eq!(first[2]["summary"]["groups"][0]["count"],6);
    assert_eq!(first[1]["summary"]["acceptedErrors"],1);
    s.sweep(at+900).unwrap();assert_eq!(s.events(0).unwrap().len(),3);
    s.sweep(cutoff).unwrap();let events=s.events(0).unwrap();assert_eq!(events.len(),4);assert_eq!(events[3]["kind"],"daily");
    assert!(s.events(events[3]["id"].as_i64().unwrap()).unwrap().is_empty());
    s.sweep(cutoff+900).unwrap();s.sweep(cutoff+86400).unwrap();assert_eq!(s.events(0).unwrap().len(),4);
}
#[test]
fn historical_and_future_errors_do_not_trigger_early_and_backup_restores() {
    let dir=tempfile::tempdir().unwrap();let mut s=Store::open(dir.path(),32*1024*1024).unwrap();let at=now();
    s.ingest(&report(at-2*86400,6),at).unwrap();s.ingest(&report(at+7200,6),at).unwrap();s.sweep(at).unwrap();
    assert!(s.events(0).unwrap().iter().all(|e| e["kind"]!="early"));
    s.sweep(at+7201).unwrap();assert!(s.events(0).unwrap().iter().all(|e|e["kind"]!="early"));
    s.backup(at).unwrap();let copy=Connection::open(dir.path().join(format!("backups/backup-{at}.sqlite"))).unwrap();
    assert_eq!(copy.query_row("SELECT count(*) FROM errors",[],|r|r.get::<_,u64>(0)).unwrap(),12);
    assert_eq!(copy.query_row("PRAGMA quick_check",[],|r|r.get::<_,String>(0)).unwrap(),"ok");
    s.sweep(at+31*86400).unwrap();assert_eq!(s.db.query_row("SELECT count(*) FROM errors",[],|r|r.get::<_,u64>(0)).unwrap(),0);
}
#[tokio::test]
async fn http_real_loopback_upload_private_reads_and_rejected_success() {
    let dir=tempfile::tempdir().unwrap();let key="a".repeat(64);let server=Server::new(Store::open(dir.path(),32*1024*1024).unwrap(),key.clone(),0).unwrap();
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let url=format!("http://{}",listener.local_addr().unwrap());
    let task=tokio::spawn(async move {axum::serve(listener,router(server)).await.unwrap();});let client=reqwest::Client::builder().no_proxy().build().unwrap();let r=report(now(),1);
    let response=client.post(format!("{url}/v1/error-reports")).header("content-type","application/gzip").body(r.encode().unwrap()).send().await.unwrap();assert_eq!(response.status(),200);
    assert_eq!(response.json::<Receipt>().await.unwrap().accepted_errors,1);
    let known=fusion_report_protocol::KnownRequest {schema_version:1,installation_id:r.installation_id.clone(),request_ids:vec![r.errors[0].request_id.clone(),uuid::Uuid::new_v4().to_string()],buckets:vec![]};
    let response=client.post(format!("{url}/v1/error-reports/known")).json(&known).send().await.unwrap();assert_eq!(response.status(),200);
    let known_result=response.json::<fusion_report_protocol::KnownResponse>().await.unwrap();known_result.validate_for(&known).unwrap();
    assert_eq!(known_result.request_ids,vec![r.errors[0].request_id.clone()]);
    assert_eq!(client.get(format!("{url}/v1/error-reports/known")).send().await.unwrap().status(),405);
    assert_eq!(client.post(format!("{url}/v1/error-reports/known")).json(&serde_json::json!({"installationId":r.installation_id,"requestIds":[],"buckets":[],"schemaVersion":1,"arbitrary":"private"})).send().await.unwrap().status(),400);
    assert_eq!(client.post(format!("{url}/v1/error-reports/known")).header("content-type","application/json").body(vec![b' ';fusion_report_protocol::KNOWN_JSON_LIMIT+1]).send().await.unwrap().status(),413);
    let mut modern=report(now(),1);modern.schema_version=2;
    modern.errors[0].details=Some(fusion_report_protocol::FailureDetails {terminal_event:Some("failed".into()),upstream_failure:Some("server".into()),..Default::default()});
    assert_eq!(client.post(format!("{url}/v1/error-reports")).header("content-type","application/gzip").body(modern.encode().unwrap()).send().await.unwrap().status(),200);
    assert_eq!(client.get(format!("{url}/v1/admin/reports?limit=51")).bearer_auth(&key).send().await.unwrap().status(),400);
    for path in ["/v1/admin/reliability".to_owned(),"/v1/admin/events".to_owned(),"/v1/admin/health".to_owned(),"/v1/admin/reports".to_owned(),format!("/v1/admin/reports/{}",r.report_id)] {
        assert_eq!(client.get(format!("{url}{path}")).send().await.unwrap().status(),401);
        assert_eq!(client.get(format!("{url}{path}")).bearer_auth(&key).send().await.unwrap().status(),200);
    }
    let mut bad=r.clone();bad.errors[0].code="success".into();assert!(bad.encode().is_err());
    assert_eq!(client.post(format!("{url}/v1/error-reports")).header("content-type","application/gzip").body(vec![0;WIRE_LIMIT+1]).send().await.unwrap().status(),413);
    assert_eq!(client.post(format!("{url}/v1/error-reports")).header("content-type","application/gzip").body("private invalid data").send().await.unwrap().status(),400);
    task.abort();
}

#[test]
fn immediate_event_and_error_are_atomic_and_legacy_cutover_is_stable() {
    let dir=tempfile::tempdir().unwrap();let at=now();let store=Store::open(dir.path(),32*1024*1024).unwrap();
    // Simulate the exact existing schema before this receiver version was installed.
    store.db.execute("DELETE FROM state WHERE key='immediate_errors_after_cursor'",[]).unwrap();
    store.db.execute("INSERT INTO events(created,kind,body) VALUES(?1,'daily','{}')",[at]).unwrap();
    let legacy=store.events(0).unwrap();drop(store);
    let mut store=Store::open(dir.path(),32*1024*1024).unwrap();
    assert_eq!(store.health().unwrap()["immediateErrorsAfterCursor"],legacy[0]["id"]);
    store.db.execute_batch("CREATE TEMP TRIGGER fail_notice BEFORE INSERT ON events WHEN new.kind='new_errors' BEGIN SELECT raise(ABORT,'public-fake-failure'); END;").unwrap();
    let report=report(at,1);assert_eq!(store.ingest(&report,at).unwrap_err(),"database_write_failed");
    for table in ["errors","reports"] {assert_eq!(store.db.query_row(&format!("SELECT count(*) FROM {table}"),[],|r|r.get::<_,usize>(0)).unwrap(),0);}
    store.db.execute_batch("DROP TRIGGER fail_notice;").unwrap();store.ingest(&report,at).unwrap();
    assert_eq!(store.events(0).unwrap()[0],legacy[0]);drop(store);
    let store=Store::open(dir.path(),32*1024*1024).unwrap();
    assert_eq!(store.health().unwrap()["immediateErrorsAfterCursor"],legacy[0]["id"]);
    assert_eq!(store.events(0).unwrap()[1]["kind"],"new_errors");
    assert_eq!(store.db.query_row("PRAGMA user_version",[],|r|r.get::<_,u32>(0)).unwrap(),1);
}
#[test]
fn capacity_forward_schema_and_seven_backup_retention() {
    let dir=tempfile::tempdir().unwrap();let mut store=Store::open(dir.path(),128*1024).unwrap();let at=now();
    assert_eq!(store.ingest(&report(at,2000),at).unwrap_err(),"storage_limit");
    assert_eq!(store.db.query_row("SELECT count(*) FROM reports",[],|r|r.get::<_,u64>(0)).unwrap(),0);
    for offset in 0..9 {store.backup(at+offset).unwrap();}
    assert_eq!(std::fs::read_dir(dir.path().join("backups")).unwrap().count(),7);
    store.db.pragma_update(None,"user_version",2).unwrap();drop(store);
    assert!(matches!(Store::open(dir.path(),128*1024),Err("database_newer_than_server")));
}
