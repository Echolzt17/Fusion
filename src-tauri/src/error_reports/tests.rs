use super::*;
use fusion_report_protocol::{Environment, ErrorRecord, ReliabilityHour, ReliabilitySummary};
use serde_json::json;
use std::{io::Read, net::TcpListener, thread::JoinHandle};

fn setup() -> (tempfile::TempDir, String) {
    let root=tempfile::tempdir().unwrap();
    let id=Reports::new(root.path().into()).install_id().unwrap();
    (root,id)
}
fn error(at: i64) -> ErrorRecord {
    ErrorRecord {request_id:uuid::Uuid::new_v4().to_string(),at,operation:"responses".into(),code:"network_error".into(),elapsed_ms:123,
        account_alias:None,conversation_alias:None,app_version:Some("1.0.13".into()),upstream_status:None,
        failure_stage:Some("connect".into()),network_error_kind:Some("connect".into()),proxy_source:None,execution:None,details:None}
}
fn report(id: &str) -> Report {
    let at=pool_core::now();
    Report {schema_version:3,report_id:uuid::Uuid::new_v4().to_string(),installation_id:id.into(),prepared_at:at,
        range_from:at-7*86400,range_to:at,history_limited:false,
        environment:Environment {app_version:"1.0.13".into(),os:"Windows 11".into(),os_build:None,architecture:"x64".into(),
            webview2_version:None,desktop_version:None,engine_version:None,uptime_seconds:10},
        errors:vec![error(at-5)],reliability:Some(ReliabilitySummary {coverage_start:at-3600,snapshot_at:at,history_partial:false,
            buckets:vec![ReliabilityHour {hour:at.div_euclid(3600)*3600,app_version:"1.0.13".into(),operation:"responses".into(),
                success:1,elapsed_histogram:[1,0,0,0,0,0],success_by_size:[1,0,0,0,0],..Default::default()}]})}
}
fn draft(report: Report, retry_existing: bool) -> Draft {
    let bytes=report.encode().unwrap();Draft {report,bytes,retry_existing}
}
fn receipt(report: &Report) -> Receipt {
    Receipt {report_id:report.report_id.clone(),received_at:pool_core::now(),accepted_errors:report.errors.len(),duplicate_errors:0}
}
fn client() -> reqwest::Client { reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(3)).build().unwrap() }

/// One bounded local request, with fake diagnostic data only. Tests can inspect
/// exact transmitted bytes without involving the user's processes or settings.
fn serve_once(status: u16, reply: impl FnOnce(&[u8])->Vec<u8> + Send + 'static) -> (String,JoinHandle<Vec<u8>>) {
    let listener=TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url=format!("http://{}",listener.local_addr().unwrap());
    let handle=std::thread::spawn(move || {
        let until=Instant::now()+Duration::from_secs(5);
        let mut stream=loop {
            match listener.accept() {
                Ok((stream,_))=>break stream,
                Err(error) if error.kind()==std::io::ErrorKind::WouldBlock=>{
                    assert!(Instant::now()<until,"expected local HTTP request");
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error)=>panic!("local test server: {error}"),
            }
        };
        stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let mut request=Vec::new();let (header_end,len)=loop {
            let mut buffer=[0;4096];let count=stream.read(&mut buffer).unwrap();assert!(count>0);
            request.extend_from_slice(&buffer[..count]);assert!(request.len()<1024*1024);
            if let Some(at)=request.windows(4).position(|w|w==b"\r\n\r\n") {
                let headers=String::from_utf8_lossy(&request[..at]).to_lowercase();
                let len:usize=headers.lines().find_map(|line|line.strip_prefix("content-length:").map(str::trim)).unwrap().parse().unwrap();
                break (at+4,len);
            }
        };
        while request.len()<header_end+len {
            let mut buffer=[0;4096];let count=stream.read(&mut buffer).unwrap();assert!(count>0);
            request.extend_from_slice(&buffer[..count]);assert!(request.len()<1024*1024);
        }
        let body=request[header_end..header_end+len].to_vec();let response=reply(&body);
        write!(stream,"HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",response.len()).unwrap();
        stream.write_all(&response).unwrap();body
    });
    (url,handle)
}
fn no_known(body: &[u8]) -> Vec<u8> {
    let request:KnownRequest=serde_json::from_slice(body).unwrap();
    serde_json::to_vec(&KnownResponse {schema_version:KNOWN_SCHEMA_VERSION,installation_id:request.installation_id,request_ids:vec![],bucket_indexes:vec![]}).unwrap()
}

#[test]
fn acknowledgements_survive_restart_without_hiding_a_new_failure_of_the_same_kind() {
    let (root,id)=setup();let report=report(&id);let receipt=receipt(&report);let bytes=report.encode().unwrap();
    let mut ledger=Ledger::load(root.path(),&id).unwrap();ledger.confirm_report(&report,&bytes,&receipt).unwrap();ledger.save(root.path()).unwrap();
    let reopened=Reports::new(root.path().into());assert_eq!(reopened.status.lock().unwrap().history.len(),1);
    let mut next=report.clone();next.prepared_at+=1;next.range_to+=1;next.reliability.as_mut().unwrap().snapshot_at+=1;
    let newer=error(next.prepared_at);next.errors.push(newer.clone());
    Ledger::load(root.path(),&id).unwrap().filter(&mut next);
    assert_eq!(next.errors.len(),1);assert_eq!(next.errors[0].request_id,newer.request_id);
    assert!(next.reliability.unwrap().buckets.is_empty(),"same numeric snapshot with a newer observation is not new data");
    let exposed=serde_json::to_string(&*reopened.status.lock().unwrap()).unwrap();
    assert!(!exposed.contains(&report.errors[0].request_id));assert!(!exposed.contains("fingerprint"));
}

#[test]
fn summary_only_does_not_acknowledge_errors_that_were_not_sent() {
    let (root,id)=setup();let full=report(&id);let mut summary=full.clone();summary.errors.clear();
    let mut ledger=Ledger::load(root.path(),&id).unwrap();
    ledger.confirm_report(&summary,&summary.encode().unwrap(),&receipt(&summary)).unwrap();ledger.save(root.path()).unwrap();
    let mut next=full.clone();Ledger::load(root.path(),&id).unwrap().filter(&mut next);
    assert_eq!(next.errors.len(),full.errors.len());assert!(next.reliability.as_ref().unwrap().buckets.is_empty());
    let bucket=next.reliability.as_mut().unwrap();bucket.buckets=full.reliability.unwrap().buckets;bucket.buckets[0].success+=1;
    bucket.buckets[0].elapsed_histogram[0]+=1;bucket.buckets[0].success_by_size[0]+=1;
    ledger.filter(&mut next);assert_eq!(next.reliability.unwrap().buckets.len(),1,"changed counts must be sent");
}

#[tokio::test]
async fn known_bootstrap_sends_only_new_candidates_then_freezes_the_approved_snapshot() {
    let (root,id)=setup();let mut approved=report(&id);let new=error(approved.prepared_at);approved.errors.push(new.clone());
    let known_error=approved.errors[0].request_id.clone();let expected=known_error.clone();
    let (url,server)=serve_once(200,move |body| {
        let request:KnownRequest=serde_json::from_slice(body).unwrap();
        assert_eq!(request.request_ids.len(),2);assert!(request.request_ids.contains(&expected));
        serde_json::to_vec(&KnownResponse {schema_version:1,installation_id:request.installation_id,request_ids:vec![expected],bucket_indexes:vec![0]}).unwrap()
    });
    let outcome=reconcile_and_freeze(root.path().into(),draft(approved.clone(),false),&client(),&url).await.unwrap();server.join().unwrap();
    let Frozen::Ready(frozen)=outcome else {panic!("new error should remain")};
    assert_eq!(frozen.report.report_id,approved.report_id);assert_eq!(frozen.report.errors.len(),1);
    assert_eq!(frozen.report.errors[0].request_id,new.request_id);assert!(frozen.report.reliability.as_ref().unwrap().buckets.is_empty());
    assert!(frozen.retry_existing);assert_eq!(std::fs::read(root.path().join("pending.json.gz")).unwrap(),frozen.bytes);
    let mut later=approved.clone();Ledger::load(root.path(),&id).unwrap().filter(&mut later);
    assert!(!later.errors.iter().any(|error|error.request_id==known_error));assert!(later.errors.iter().any(|error|error.request_id==new.request_id));
    let confirmed=receipt(&frozen.report);persist_confirmation(root.path(),&frozen,&confirmed).unwrap();
    assert!(!root.path().join("pending.json.gz").exists());
    let mut next=approved;Ledger::load(root.path(),&id).unwrap().filter(&mut next);assert!(!has_payload(&next));
}

#[tokio::test]
async fn entirely_known_preview_stops_before_creating_an_upload_or_a_receipt() {
    let (root,id)=setup();let approved=report(&id);
    let (url,server)=serve_once(200,|body| {
        let request:KnownRequest=serde_json::from_slice(body).unwrap();
        serde_json::to_vec(&KnownResponse {schema_version:1,installation_id:request.installation_id,
            request_ids:request.request_ids,bucket_indexes:(0..request.buckets.len()).collect()}).unwrap()
    });
    let outcome=reconcile_and_freeze(root.path().into(),draft(approved.clone(),false),&client(),&url).await.unwrap();server.join().unwrap();
    let Frozen::Empty(history)=outcome else {panic!("all known should stop")};assert!(history.is_empty());
    assert!(!root.path().join("pending.json.gz").exists());
    let mut next=approved;Ledger::load(root.path(),&id).unwrap().filter(&mut next);assert!(!has_payload(&next));
}

#[tokio::test]
async fn failed_or_unbound_known_response_never_falls_back_to_full_upload() {
    for status in [404,200] {
        let (root,id)=setup();let approved=report(&id);
        let (url,server)=serve_once(status,|_|serde_json::to_vec(&KnownResponse {schema_version:1,
            installation_id:uuid::Uuid::new_v4().to_string(),request_ids:vec![],bucket_indexes:vec![]}).unwrap());
        assert!(reconcile_and_freeze(root.path().into(),draft(approved,false),&client(),&url).await.is_err());server.join().unwrap();
        assert!(!root.path().join("pending.json.gz").exists());assert!(!ledger::exists(root.path()));
    }
}

#[tokio::test]
async fn legacy_pending_is_uploaded_with_identical_id_and_bytes_without_known_query() {
    for version in [1,2,3] {
    let (root,id)=setup();let mut old=report(&id);old.schema_version=version;
    if version<3 {old.reliability=None;}
    let original=draft(old,true);freeze_pending(root.path(),&original).unwrap();
    let Frozen::Ready(frozen)=reconcile_and_freeze(root.path().into(),original.clone(),&client(),"http://127.0.0.1:9/never-known").await.unwrap()
        else {panic!("legacy pending must be retained")};
    assert_eq!(frozen.bytes,original.bytes);assert_eq!(frozen.report.report_id,original.report.report_id);
    let response=receipt(&original.report);let expected=response.clone();
    let (url,server)=serve_once(200,move |_|serde_json::to_vec(&expected).unwrap());
    let received=upload(&client(),&url,frozen.bytes.clone(),&frozen.report.report_id,frozen.report.errors.len()).await.unwrap();
    assert_eq!(server.join().unwrap(),original.bytes);persist_confirmation(root.path(),&frozen,&received).unwrap();
    assert!(!root.path().join("pending.json.gz").exists());
    }
}

#[tokio::test]
async fn lost_receipt_survives_restart_and_retries_identical_frozen_bytes() {
    let (root,id)=setup();let original=draft(report(&id),true);freeze_pending(root.path(),&original).unwrap();
    // The fake receiver reads the complete upload, but provides no receipt.
    let (url,server)=serve_once(200,|_|vec![]);
    assert!(upload(&client(),&url,original.bytes.clone(),&original.report.report_id,original.report.errors.len()).await.is_err());
    assert_eq!(server.join().unwrap(),original.bytes);assert!(!ledger::exists(root.path()));
    let reopened=Reports::new(root.path().into());assert!(reopened.status.lock().unwrap().history.is_empty());
    let pending=read_pending(root.path(),&id).unwrap().unwrap();
    let Frozen::Ready(retry)=reconcile_and_freeze(root.path().into(),pending,&client(),"http://127.0.0.1:9/no-known").await.unwrap()
        else {panic!("unknown receipt must preserve the frozen retry")};
    assert_eq!(retry.bytes,original.bytes);assert_eq!(retry.report.report_id,original.report.report_id);
    let confirmed=receipt(&retry.report);let reply=confirmed.clone();
    let (url,server)=serve_once(200,move |_|serde_json::to_vec(&reply).unwrap());
    let received=upload(&client(),&url,retry.bytes.clone(),&retry.report.report_id,retry.report.errors.len()).await.unwrap();
    assert_eq!(server.join().unwrap(),original.bytes);persist_confirmation(root.path(),&retry,&received).unwrap();
    let mut next=original.report;Ledger::load(root.path(),&id).unwrap().filter(&mut next);assert!(!has_payload(&next));
}

#[cfg(windows)]
#[test]
fn atomic_state_replace_failure_preserves_old_state_and_exact_pending() {
    use std::os::windows::fs::OpenOptionsExt;
    let (root,id)=setup();let pending=draft(report(&id),true);freeze_pending(root.path(),&pending).unwrap();
    Ledger::load(root.path(),&id).unwrap().save(root.path()).unwrap();
    let path=root.path().join("acknowledgements.json");let old=std::fs::read(&path).unwrap();
    // Allow reading the old state while denying Windows' atomic replacement.
    let guard=std::fs::OpenOptions::new().read(true).share_mode(1).open(&path).unwrap();
    assert!(persist_confirmation(root.path(),&pending,&receipt(&pending.report)).is_err());
    assert_eq!(std::fs::read(&path).unwrap(),old);
    assert_eq!(std::fs::read(root.path().join("pending.json.gz")).unwrap(),pending.bytes);
    let mut unconfirmed=pending.report.clone();Ledger::load(root.path(),&id).unwrap().filter(&mut unconfirmed);assert!(has_payload(&unconfirmed));
    drop(guard);
    persist_confirmation(root.path(),&pending,&receipt(&pending.report)).unwrap();
    assert!(!root.path().join("pending.json.gz").exists());
}

#[cfg(windows)]
#[tokio::test]
async fn pending_cleanup_failure_commits_once_and_retry_only_cleans_locally() {
    use std::os::windows::fs::OpenOptionsExt;
    let (root,id)=setup();let pending=draft(report(&id),true);freeze_pending(root.path(),&pending).unwrap();
    let path=root.path().join("pending.json.gz");
    let guard=std::fs::OpenOptions::new().read(true).share_mode(1).open(&path).unwrap();
    assert!(persist_confirmation(root.path(),&pending,&receipt(&pending.report)).is_err());
    assert_eq!(std::fs::read(&path).unwrap(),pending.bytes);
    let ledger=Ledger::load(root.path(),&id).unwrap();assert!(ledger.completed(&pending.report,&pending.bytes).unwrap().is_some());
    let mut next=pending.report.clone();ledger.filter(&mut next);assert!(!has_payload(&next));
    drop(guard);
    let Frozen::Complete(_,history)=reconcile_and_freeze(root.path().into(),pending,&client(),"http://127.0.0.1:9/no-network").await.unwrap()
        else {panic!("durably confirmed report must only need local cleanup")};
    assert_eq!(history.len(),1);assert!(!path.exists());
}

#[tokio::test]
async fn durable_confirmation_with_leftover_pending_only_cleans_up_locally() {
    let (root,id)=setup();let pending=draft(report(&id),true);freeze_pending(root.path(),&pending).unwrap();
    let mut ledger=Ledger::load(root.path(),&id).unwrap();ledger.confirm_report(&pending.report,&pending.bytes,&receipt(&pending.report)).unwrap();ledger.save(root.path()).unwrap();
    let Frozen::Complete(receipt,history)=reconcile_and_freeze(root.path().into(),pending.clone(),&client(),"http://127.0.0.1:9/no-network").await.unwrap()
        else {panic!("confirmed report must be completed locally")};
    assert_eq!(receipt.report_id,pending.report.report_id);assert_eq!(history.len(),1);assert!(!root.path().join("pending.json.gz").exists());
}

#[test]
fn receipt_confirmation_failure_keeps_the_exact_pending_and_does_not_acknowledge() {
    let (root,id)=setup();let pending=draft(report(&id),true);freeze_pending(root.path(),&pending).unwrap();
    let mut invalid=receipt(&pending.report);invalid.accepted_errors+=1;
    assert!(persist_confirmation(root.path(),&pending,&invalid).is_err());assert!(!ledger::exists(root.path()));
    assert_eq!(std::fs::read(root.path().join("pending.json.gz")).unwrap(),pending.bytes);
    let mut valid=receipt(&pending.report);valid.duplicate_errors=valid.accepted_errors;valid.accepted_errors=0;
    persist_confirmation(root.path(),&pending,&valid).unwrap();
    let mut next=pending.report;Ledger::load(root.path(),&id).unwrap().filter(&mut next);assert!(!has_payload(&next));
}

#[test]
fn corrupt_future_or_misbound_state_and_conflicting_pending_are_preserved() {
    let (root,id)=setup();let pending=draft(report(&id),true);freeze_pending(root.path(),&pending).unwrap();
    let mut ledger=Ledger::load(root.path(),&id).unwrap();ledger.confirm_report(&pending.report,&pending.bytes,&receipt(&pending.report)).unwrap();ledger.save(root.path()).unwrap();
    let changed=draft({let mut report=pending.report.clone();report.errors[0].elapsed_ms+=1;report},true);
    assert!(ledger.completed(&changed.report,&changed.bytes).is_err());assert!(freeze_pending(root.path(),&changed).is_err());
    let good=std::fs::read(root.path().join("acknowledgements.json")).unwrap();
    for replacement in [b"broken".to_vec(),{let mut v:serde_json::Value=serde_json::from_slice(&good).unwrap();v["version"]=json!(2);serde_json::to_vec(&v).unwrap()},
        {let mut v:serde_json::Value=serde_json::from_slice(&good).unwrap();v["installationId"]=json!(uuid::Uuid::new_v4().to_string());serde_json::to_vec(&v).unwrap()}] {
        std::fs::write(root.path().join("acknowledgements.json"),&replacement).unwrap();
        assert!(Ledger::load(root.path(),&id).is_err());assert!(Reports::new(root.path().into()).status.lock().unwrap().history_error.is_some());
        assert_eq!(std::fs::read(root.path().join("acknowledgements.json")).unwrap(),replacement);
        assert_eq!(std::fs::read(root.path().join("pending.json.gz")).unwrap(),pending.bytes);
    }
}

#[test]
fn legacy_history_is_preserved_but_never_guesses_which_requests_were_submitted() {
    let (root,id)=setup();let original=report(&id);let receipt=receipt(&original);
    let bytes=serde_json::to_vec(&vec![receipt]).unwrap();std::fs::write(root.path().join("receipts.json"),&bytes).unwrap();
    let ledger=Ledger::load(root.path(),&id).unwrap();assert_eq!(ledger.history().len(),1);
    let mut next=original;ledger.filter(&mut next);assert!(has_payload(&next));
    ledger.save(root.path()).unwrap();assert_eq!(std::fs::read(root.path().join("receipts.json")).unwrap(),bytes);
    assert_eq!(Reports::new(root.path().into()).status.lock().unwrap().history.len(),1);
}

#[test]
fn corrupt_or_missing_installation_identity_is_not_replaced() {
    let (root,id)=setup();let reports=Reports::new(root.path().into());assert_eq!(id,reports.install_id().unwrap());
    std::fs::write(root.path().join("installation-id"),"invalid").unwrap();assert!(reports.install_id().is_err());
    assert_eq!(std::fs::read_to_string(root.path().join("installation-id")).unwrap(),"invalid");
    std::fs::remove_file(root.path().join("installation-id")).unwrap();std::fs::write(root.path().join("pending.json.gz"),b"original").unwrap();
    assert!(reports.install_id().is_err());assert!(!root.path().join("installation-id").exists());
    let legacy=tempfile::tempdir().unwrap();
    std::fs::write(legacy.path().join("receipts.json"),serde_json::to_vec(&vec![receipt(&report(&id))]).unwrap()).unwrap();
    assert!(Reports::new(legacy.path().into()).install_id().is_err());
    assert!(!legacy.path().join("installation-id").exists());
}

#[tokio::test]
async fn no_known_submission_still_requires_a_valid_bound_receipt() {
    let (root,id)=setup();let approved=report(&id);let (url,server)=serve_once(200,no_known);
    let Frozen::Ready(pending)=reconcile_and_freeze(root.path().into(),draft(approved,false),&client(),&url).await.unwrap()
        else {panic!("unknown records remain")};server.join().unwrap();
    let (url,server)=serve_once(200,|_|serde_json::to_vec(&Receipt {report_id:uuid::Uuid::new_v4().to_string(),received_at:1,accepted_errors:1,duplicate_errors:0}).unwrap());
    assert!(upload(&client(),&url,pending.bytes.clone(),&pending.report.report_id,pending.report.errors.len()).await.is_err());server.join().unwrap();
    assert_eq!(std::fs::read(root.path().join("pending.json.gz")).unwrap(),pending.bytes);
    let mut next=pending.report;Ledger::load(root.path(),&id).unwrap().filter(&mut next);assert!(has_payload(&next));
}
