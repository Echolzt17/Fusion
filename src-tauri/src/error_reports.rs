//! Manual, independent reporting transport. No upstream/local gateway credentials are used here.
use fusion_report_protocol::{KnownBucket, KnownRequest, KnownResponse, Receipt, Report, KNOWN_JSON_LIMIT, KNOWN_SCHEMA_VERSION};
use pool_core::Result;
use serde::Serialize;
use std::{collections::BTreeMap, io::Write, path::{Path, PathBuf}, sync::Mutex, time::{Duration, Instant}};
use tauri::Manager;
const ENDPOINT: &str = "https://162.14.80.7/v1/error-reports";
const KNOWN_ENDPOINT: &str = "https://162.14.80.7/v1/error-reports/known";
mod ledger;
use ledger::Ledger;
#[derive(Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    phase: &'static str,
    preview: Option<Preview>,
    receipt: Option<Receipt>,
    message: Option<String>,
    history: Vec<Receipt>,
    history_error: Option<String>,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    draft_id: String,
    error_count: usize,
    summary_requests: u64,
    summary_success: u64,
    summary_failure: u64,
    summary_recovered: u64,
    summary_partial: bool,
    retry_existing: bool,
    range_from: i64,
    range_to: i64,
    compressed_bytes: usize,
    history_limited: bool,
    destination: &'static str,
    categories: BTreeMap<String, usize>,
}
#[derive(Clone)]
struct Draft { report: Report, bytes: Vec<u8>, retry_existing: bool }
pub struct Reports {
    root: PathBuf,
    started: Instant,
    operation: tokio::sync::Mutex<()>,
    status: Mutex<Status>,
    draft: Mutex<Option<Draft>>,
}
impl Reports {
    pub fn new(root: PathBuf) -> Self {
        let (history, history_error) = match read_history(&root) { Ok(history)=>(history,None), Err(error)=>(vec![],Some(error)) };
        Self { root, started:Instant::now(),operation:tokio::sync::Mutex::new(()),status:Mutex::new(Status {phase:"idle",history,history_error,..Default::default()}),draft:Mutex::new(None) }
    }
    fn install_id(&self) -> Result<String> {
        std::fs::create_dir_all(&self.root).map_err(|_| "无法创建错误报告目录")?;
        let path=self.root.join("installation-id");
        if std::fs::metadata(&path).is_ok_and(|metadata| metadata.len()>64) {
            return Err("错误报告安装标识损坏，未覆盖原文件".into());
        }
        match std::fs::read_to_string(&path) {
            Ok(id) if fusion_report_protocol::uuid_ok(&id) => Ok(id),
            Ok(_) => Err("错误报告安装标识损坏，未覆盖原文件".into()),
            Err(e) if e.kind()==std::io::ErrorKind::NotFound => {
                if ledger::exists(&self.root) || self.root.join("pending.json.gz").exists()
                    || self.root.join("receipts.json").exists() {
                    return Err("错误报告安装标识缺失，未替换已有上报状态".into());
                }
                let id=uuid::Uuid::new_v4().to_string();atomic(&path,id.as_bytes())?;Ok(id)
            },
            Err(_) => Err("无法读取错误报告安装标识".into()),
        }
    }
    fn preview(draft: &Draft) -> Preview {
        let r=&draft.report;let mut categories=BTreeMap::new();for e in &r.errors { *categories.entry(e.code.clone()).or_insert(0)+=1; }
        Preview {draft_id:r.report_id.clone(),error_count:r.errors.len(),
            summary_requests:r.reliability.as_ref().map_or(0,|s|s.buckets.iter().map(|b|b.total()).sum()),
            summary_success:r.reliability.as_ref().map_or(0,|s|s.buckets.iter().map(|b|b.success).sum()),
            summary_failure:r.reliability.as_ref().map_or(0,|s|s.buckets.iter().map(|b|b.failure).sum()),
            summary_recovered:r.reliability.as_ref().map_or(0,|s|s.buckets.iter().map(|b|b.reconnect_recovered).sum()),
            summary_partial:r.reliability.as_ref().is_none_or(|s|s.history_partial),
            retry_existing:draft.retry_existing,
            range_from:r.range_from,range_to:r.range_to,compressed_bytes:draft.bytes.len(),history_limited:r.history_limited,destination:ENDPOINT,categories}
    }
    fn set_draft(&self, draft: Draft) -> Status {
        let status=Status {phase:"prepared",preview:Some(Self::preview(&draft)),receipt:None,message:None,..self.status.lock().unwrap().clone()};
        *self.draft.lock().unwrap()=Some(draft);*self.status.lock().unwrap()=status.clone();status
    }
    fn empty(&self, history: Vec<Receipt>) -> Status {
        let status=Status {phase:"empty",message:Some("暂无新增可上报诊断记录".into()),preview:None,receipt:None,
            history,history_error:None};
        *self.draft.lock().unwrap()=None;*self.status.lock().unwrap()=status.clone();status
    }
}
fn read_history(root: &Path) -> Result<Vec<Receipt>> {
    if ledger::exists(root) {
        let id=existing_installation_id(root)?;
        return Ok(Ledger::load(root,&id)?.history());
    }
    read_legacy_history(root)
}
fn existing_installation_id(root: &Path) -> Result<String> {
    if std::fs::metadata(root.join("installation-id")).map_err(|_| "无法核验错误报告安装标识")?.len()>64 {
        return Err("错误报告安装标识损坏，原文件已保留".into());
    }
    let id=std::fs::read_to_string(root.join("installation-id")).map_err(|_| "无法核验错误报告安装标识，原文件已保留")?;
    if !fusion_report_protocol::uuid_ok(&id) { return Err("错误报告安装标识损坏，原文件已保留".into()); }
    Ok(id)
}
fn valid_receipt(receipt: &Receipt) -> bool {
    fusion_report_protocol::uuid_ok(&receipt.report_id) && receipt.received_at>0
        && receipt.accepted_errors.checked_add(receipt.duplicate_errors).is_some_and(|count| count<=2000)
}
fn read_legacy_history(root: &Path) -> Result<Vec<Receipt>> {
    let path = root.join("receipts.json");
    let metadata = match std::fs::metadata(&path) {
        Ok(metadata)=>metadata,
        Err(e) if e.kind()==std::io::ErrorKind::NotFound=>return Ok(vec![]),
        Err(_)=>return Err("无法读取最近上报记录，原文件已保留".into()),
    };
    if metadata.len()>16384 { return Err("最近上报记录超过限制，原文件已保留".into()); }
    let receipts: Vec<Receipt> = serde_json::from_slice(&std::fs::read(path).map_err(|_| "无法读取最近上报记录")?)
        .map_err(|_| "最近上报记录损坏，原文件已保留")?;
    let mut ids = std::collections::HashSet::new();
    if receipts.len()>10 || receipts.iter().any(|r| !valid_receipt(r) || !ids.insert(&r.report_id)) {
        return Err("最近上报记录无效，原文件已保留".into());
    }
    Ok(receipts)
}
fn atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file=tempfile::NamedTempFile::new_in(path.parent().ok_or("错误报告路径无效")?).map_err(|_| "无法写入错误报告")?;
    file.write_all(bytes).and_then(|_| file.as_file().sync_all()).map_err(|_| "无法写入错误报告")?;
    file.persist(path).map_err(|_| "无法保存错误报告")?;Ok(())
}
fn has_payload(report: &Report) -> bool {
    !report.errors.is_empty() || report.reliability.as_ref().is_some_and(|summary| !summary.buckets.is_empty())
}
fn read_pending(root: &Path, installation_id: &str) -> Result<Option<Draft>> {
    let path=root.join("pending.json.gz");
    let metadata=match std::fs::metadata(&path) {
        Ok(metadata)=>metadata,
        Err(error) if error.kind()==std::io::ErrorKind::NotFound=>return Ok(None),
        Err(_)=>return Err("无法读取待重试错误报告，原文件已保留".into()),
    };
    if metadata.len()>fusion_report_protocol::WIRE_LIMIT as u64 { return Err("待重试错误报告超过限制，原文件已保留".into()); }
    let bytes=std::fs::read(path).map_err(|_| "无法读取待重试错误报告")?;
    let report=Report::decode(&bytes).map_err(|_| "待重试错误报告校验失败，未覆盖原文件")?;
    if report.installation_id!=installation_id { return Err("待重试报告的安装标识不匹配，原文件已保留".into()); }
    Ok(Some(Draft {report,bytes,retry_existing:true}))
}
fn remove_pending(root: &Path) -> Result<()> {
    std::fs::remove_file(root.join("pending.json.gz"))
        .or_else(|error| if error.kind()==std::io::ErrorKind::NotFound {Ok(())} else {Err(error)})
        .map_err(|_| "服务器已接收，但本机收尾未完成；重试将仅处理本机收尾".into())
}
fn freeze_pending(root: &Path, draft: &Draft) -> Result<()> {
    if let Some(pending)=read_pending(root,&draft.report.installation_id)? {
        if pending.bytes!=draft.bytes { return Err("另有待重试报告，未覆盖原文件".into()); }
        return Ok(());
    }
    atomic(&root.join("pending.json.gz"),&draft.bytes)
}
fn known_request(report: &Report) -> KnownRequest {
    KnownRequest {schema_version:KNOWN_SCHEMA_VERSION,installation_id:report.installation_id.clone(),
        request_ids:report.errors.iter().map(|error|error.request_id.clone()).collect(),
        buckets:report.reliability.as_ref().map_or_else(Vec::new,|summary|summary.buckets.iter()
            .map(|bucket| KnownBucket::from_summary(summary,bucket)).collect())}
}
enum Frozen {
    Ready(Box<Draft>),
    Empty(Vec<Receipt>),
    Complete(Receipt,Vec<Receipt>),
}
/// Only an explicitly submitted preview reaches this function. Reconciliation
/// removes approved records; it never adds newly arrived local records.
async fn reconcile_and_freeze(root: PathBuf, draft: Draft, client: &reqwest::Client, endpoint: &str) -> Result<Frozen> {
    let read_root=root.clone();
    let (mut ledger,mut draft)=crate::blocking(move || {
        let installation_id=existing_installation_id(&read_root)?;
        if draft.report.installation_id!=installation_id {return Err("预览的安装标识已变化，请重新准备".into());}
        let mut ledger=Ledger::load(&read_root,&installation_id)?;
        if draft.retry_existing {
            let pending=read_pending(&read_root,&installation_id)?.ok_or("待重试报告已不存在，请重新准备")?;
            if pending.bytes!=draft.bytes {return Err("待重试报告已变化，原文件已保留".into());}
        } else if read_pending(&read_root,&installation_id)?.is_some() {
            return Err("另有待重试报告，请重新准备；未覆盖原文件".into());
        }
        ledger.prune(pool_core::now());
        Ok((ledger,draft))
    }).await?;
    if draft.retry_existing {
        if let Some(receipt)=ledger.completed(&draft.report,&draft.bytes)? {
            crate::blocking(move || remove_pending(&root)).await?;
            return Ok(Frozen::Complete(receipt,ledger.history()));
        }
        return Ok(Frozen::Ready(Box::new(draft)));
    }
    ledger.filter(&mut draft.report);
    if !has_payload(&draft.report) {return Ok(Frozen::Empty(ledger.history()));}
    let request=known_request(&draft.report);
    let response=known(client,endpoint,&request).await?;
    ledger.confirm_known(&draft.report,&request,&response)?;
    ledger.filter(&mut draft.report);
    crate::blocking(move || {
        // Known records come from the server's durable store. Persisting them
        // never acknowledges errors omitted by a summary-only approved report.
        ledger.save(&root)?;
        if !has_payload(&draft.report) {return Ok(Frozen::Empty(ledger.history()));}
        draft.bytes=draft.report.encode().map_err(|_| "无法冻结新增脱敏报告")?;
        draft.retry_existing=true;
        freeze_pending(&root,&draft)?;
        Ok(Frozen::Ready(Box::new(draft)))
    }).await
}
#[tauri::command]
pub async fn prepare_error_report(a: tauri::State<'_, crate::App>, app: tauri::AppHandle, summary_only: Option<bool>) -> Result<Status> {
    let _work=a.updates.enter_work()?;
    let reports=a.reports.clone();
    let _operation=reports.operation.try_lock().map_err(|_| "错误报告正在处理中，请稍候")?;
    let root=reports.root.clone();let accounts=a.accounts.clone();let uptime=reports.started.elapsed().as_secs();
    let scale=app.get_webview_window("main").and_then(|w| w.scale_factor().ok());
    let r=reports.clone();
    crate::blocking(move || {
        let installation_id=r.install_id()?;
        let mut ledger=Ledger::load(&root,&installation_id)?;
        ledger.prune(pool_core::now());
        // A failed/unknown upload is retried with identical bytes, even across application restart.
        if let Some(pending)=read_pending(&root,&installation_id)? {
            if ledger.completed(&pending.report,&pending.bytes)?.is_some() {remove_pending(&root)?;}
            else {return Ok(r.set_draft(pending));}
        }
        let at=pool_core::now();let (mut logs,limited)=accounts.store.error_report_records(at)?;
        if summary_only.unwrap_or(false) { logs.clear(); }
        let summary=accounts.store.reliability_summary(at)?;
        if logs.is_empty() && summary.as_ref().is_none_or(|s|s.buckets.is_empty()) {
            return Ok(r.empty(ledger.history()));
        }
        let compatibility=pool_core::compatibility::status(&accounts.store,pool_core::config::codex_config_path()?,false,crate::codex_lifecycle::running_identity(false)).ok();
        let env=crate::windows_environment::report_environment(scale,compatibility.as_ref().and_then(|c| c.client.desktop.as_deref()),compatibility.as_ref().and_then(|c| c.client.engine.as_deref()),uptime);
        let mut report=pool_core::error_reports::prepare(&logs,&installation_id,env,at,limited)?;
        report.reliability=summary;
        ledger.filter(&mut report);
        if !has_payload(&report) {return Ok(r.empty(ledger.history()));}
        let bytes=report.encode().map_err(|_| "无法生成脱敏错误报告")?;
        Ok(r.set_draft(Draft {report,bytes,retry_existing:false}))
    }).await
}
#[tauri::command]
pub fn error_report_status(a: tauri::State<'_, crate::App>) -> Status { a.reports.status.lock().unwrap().clone() }
#[tauri::command]
pub async fn submit_error_report(a: tauri::State<'_, crate::App>, draft_id: String) -> Result<Status> {
    let _work=a.updates.enter_work()?;
    let reports=a.reports.clone();let _operation=reports.operation.try_lock().map_err(|_| "错误报告正在处理中，请稍候")?;
    let draft={let draft=reports.draft.lock().unwrap();let d=draft.as_ref().ok_or("请先查看错误报告摘要")?;
        if d.report.report_id!=draft_id { return Err("错误报告已变化，请重新查看摘要".into()); } d.clone()};
    { let mut s=reports.status.lock().unwrap();s.phase="uploading";s.message=None; }
    let client=match report_client() {Ok(client)=>client,Err(message)=>return Ok(failed(&reports,message))};
    let frozen=match reconcile_and_freeze(reports.root.clone(),draft,&client,KNOWN_ENDPOINT).await {
        Ok(frozen)=>frozen,Err(message)=>return Ok(failed(&reports,message)),
    };
    let draft=match frozen {
        Frozen::Empty(history)=>return Ok(reports.empty(history)),
        Frozen::Complete(receipt,history)=>return Ok(sent(&reports,receipt,history)),
        Frozen::Ready(draft)=>*draft,
    };
    // From this point every retry uses the newly frozen bytes. Never reconcile
    // an already dispatched report again or enlarge the approved snapshot.
    reports.set_draft(draft.clone());
    {let mut status=reports.status.lock().unwrap();status.phase="uploading";}
    let receipt=match upload(&client,ENDPOINT,draft.bytes.clone(),&draft.report.report_id,draft.report.errors.len()).await {
        Ok(receipt)=>receipt,Err(message)=>return Ok(failed(&reports,message)),
    };
    let root=reports.root.clone();let confirmed=receipt.clone();
    let persisted=crate::blocking(move || persist_confirmation(&root,&draft,&confirmed)).await;
    match persisted {
        Ok(history)=>Ok(sent(&reports,receipt,history)),
        Err(message)=>{
            let mut status=reports.status.lock().unwrap();status.phase="failed";status.receipt=Some(receipt);
            status.history_error=Some(message);
            status.message=Some("服务器已接收，但本机确认收尾未完成；原报告已保留，可重试同一报告".into());
            Ok(status.clone())
        }
    }
}
fn persist_confirmation(root: &Path, draft: &Draft, receipt: &Receipt) -> Result<Vec<Receipt>> {
    let installation_id=existing_installation_id(root)?;
    let mut ledger=Ledger::load(root,&installation_id)?;
    // Verify pending against the frozen bytes before recording any receipt.
    let pending=read_pending(root,&installation_id)?.ok_or("待确认报告已不存在，未改写确认状态")?;
    if pending.bytes!=draft.bytes {return Err("待确认报告已变化，未改写确认状态".into());}
    ledger.prune(pool_core::now());
    ledger.confirm_report(&draft.report,&draft.bytes,receipt)?;
    ledger.save(root)?;
    // Receipt, request ids and numeric bucket fingerprints are one atomic file.
    // If cleanup fails, a later retry validates this report id and byte digest
    // and cleans up locally without another HTTP submission.
    remove_pending(root)?;
    Ok(ledger.history())
}
fn sent(reports: &Reports, receipt: Receipt, history: Vec<Receipt>) -> Status {
    let mut status=reports.status.lock().unwrap();
    status.phase="sent";status.receipt=Some(receipt);status.history=history;status.history_error=None;
    status.message=Some("错误日志上报成功".into());*reports.draft.lock().unwrap()=None;status.clone()
}
fn failed(reports: &Reports, message: String) -> Status {
    let mut status=reports.status.lock().unwrap();status.phase="failed";status.message=Some(message);status.clone()
}
fn report_client() -> Result<reqwest::Client> {
    reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).https_only(true)
        .connect_timeout(Duration::from_secs(10)).timeout(Duration::from_secs(60)).build().map_err(|_| "无法初始化日志上报连接".into())
}
async fn known(client: &reqwest::Client, endpoint: &str, request: &KnownRequest) -> Result<KnownResponse> {
    request.validate().map_err(|_| "已上报核对参数无效，未上传")?;
    let body=serde_json::to_vec(request).map_err(|_| "无法准备已上报核对，未上传")?;
    if body.len()>KNOWN_JSON_LIMIT {return Err("已上报核对超过限制，未上传".into());}
    let mut response=client.post(endpoint).header("content-type","application/json").body(body).send().await
        .map_err(|_| "无法核对已上报记录，未上传；请稍后重试")?;
    if response.status()!=200 {return Err("日志服务器未完成增量核对，未上传；请稍后重试".into());}
    let mut bytes=Vec::new();
    while let Some(chunk)=response.chunk().await.map_err(|_| "增量核对读取中断，未上传")? {
        if bytes.len()+chunk.len()>KNOWN_JSON_LIMIT {return Err("增量核对结果超过限制，未上传".into());}
        bytes.extend_from_slice(&chunk);
    }
    let result: KnownResponse=serde_json::from_slice(&bytes).map_err(|_| "增量核对结果无效，未上传")?;
    result.validate_for(request).map_err(|_| "增量核对结果与当前预览不匹配，未上传")?;
    Ok(result)
}
async fn upload(client: &reqwest::Client, endpoint: &str, bytes: Vec<u8>, id: &str, count: usize) -> Result<Receipt> {
    let mut response=client.post(endpoint).header("content-type","application/gzip").body(bytes).send().await
        .map_err(|_| "日志上报连接失败或超时，接收状态未确认；可手动重试同一报告")?;
    if response.status()!=200 { return Err(match response.status().as_u16() { 413=>"错误报告超过接收限制",429=>"日志上报过于频繁，请稍后重试",507=>"日志服务器存储空间不足，请联系维护者",_=>"日志服务器未确认接收，请稍后重试" }.into()); }
    let mut body=Vec::new();while let Some(chunk)=response.chunk().await.map_err(|_| "回执读取中断，可手动重试同一报告")? {
        if body.len()+chunk.len()>16384 { return Err("上报回执超过限制".into()); } body.extend_from_slice(&chunk);
    }
    let receipt:Receipt=serde_json::from_slice(&body).map_err(|_| "上报回执无效，可手动重试同一报告")?;
    if receipt.report_id!=id || receipt.accepted_errors.checked_add(receipt.duplicate_errors)!=Some(count) || !valid_receipt(&receipt) { return Err("上报回执与当前报告不匹配".into()); }
    Ok(receipt)
}
#[tauri::command]
pub async fn save_error_report(a: tauri::State<'_, crate::App>, draft_id: String) -> Result<String> {
    let r=a.reports.clone();let _operation=r.operation.try_lock().map_err(|_| "错误报告正在处理中，请稍候")?;
    let bytes={let d=r.draft.lock().unwrap();let d=d.as_ref().ok_or("请先查看错误报告摘要")?;if d.report.report_id!=draft_id {return Err("错误报告已变化".into());}d.bytes.clone()};
    let path=r.root.join(format!("error-report-{draft_id}.json.gz"));
    crate::blocking(move || {atomic(&path,&bytes)?;Ok(path.display().to_string())}).await
}

#[cfg(test)] mod tests;
