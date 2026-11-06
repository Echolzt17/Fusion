//! Manual, independent reporting transport. No upstream/local gateway credentials are used here.
use fusion_report_protocol::{Receipt, Report};
use pool_core::Result;
use serde::Serialize;
use std::{collections::BTreeMap, io::Write, path::{Path, PathBuf}, sync::Mutex, time::{Duration, Instant}};
use tauri::Manager;
const ENDPOINT: &str = "https://162.14.80.7/v1/error-reports";
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
        match std::fs::read_to_string(&path) {
            Ok(id) if fusion_report_protocol::uuid_ok(&id) => Ok(id),
            Ok(_) => Err("错误报告安装标识损坏，未覆盖原文件".into()),
            Err(e) if e.kind()==std::io::ErrorKind::NotFound => { let id=uuid::Uuid::new_v4().to_string();atomic(&path,id.as_bytes())?;Ok(id) },
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
}
fn read_history(root: &Path) -> Result<Vec<Receipt>> {
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
    if receipts.len()>10 || receipts.iter().any(|r| !fusion_report_protocol::uuid_ok(&r.report_id)
        || !ids.insert(&r.report_id) || r.received_at<=0
        || !r.accepted_errors.checked_add(r.duplicate_errors).is_some_and(|n| n<=2000)) {
        return Err("最近上报记录无效，原文件已保留".into());
    }
    Ok(receipts)
}
fn save_receipt(root: &Path, receipt: &Receipt) -> Result<Vec<Receipt>> {
    let mut history = read_history(root)?;
    history.retain(|r| r.report_id != receipt.report_id);
    history.push(receipt.clone());
    history.sort_by(|a,b| b.received_at.cmp(&a.received_at).then_with(||b.report_id.cmp(&a.report_id)));
    history.truncate(10);
    atomic(&root.join("receipts.json"), &serde_json::to_vec(&history).map_err(|_| "无法保存上报回执")?)?;
    Ok(history)
}
fn atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file=tempfile::NamedTempFile::new_in(path.parent().ok_or("错误报告路径无效")?).map_err(|_| "无法写入错误报告")?;
    file.write_all(bytes).and_then(|_| file.as_file().sync_all()).map_err(|_| "无法写入错误报告")?;
    file.persist(path).map_err(|_| "无法保存错误报告")?;Ok(())
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
        // A failed/unknown upload is retried with identical bytes, even across application restart.
        let pending=root.join("pending.json.gz");
        if pending.exists() {
            let meta=std::fs::metadata(&pending).map_err(|_| "无法读取待重试错误报告")?;
            if meta.len()>fusion_report_protocol::WIRE_LIMIT as u64 { return Err("待重试错误报告超过限制".into()); }
            let bytes=std::fs::read(&pending).map_err(|_| "无法读取待重试错误报告")?;
            let report=Report::decode(&bytes).map_err(|_| "待重试错误报告校验失败，未覆盖原文件")?;
            return Ok(r.set_draft(Draft {report,bytes,retry_existing:true}));
        }
        let at=pool_core::now();let (mut logs,limited)=accounts.store.error_report_records(at)?;
        if summary_only.unwrap_or(false) { logs.clear(); }
        let summary=accounts.store.reliability_summary(at)?;
        if logs.is_empty() && summary.as_ref().is_none_or(|s|s.buckets.is_empty()) {
            let status=Status {phase:"empty",message:Some("暂无可上报诊断记录".into()),preview:None,receipt:None,..r.status.lock().unwrap().clone()};
            *r.draft.lock().unwrap()=None;*r.status.lock().unwrap()=status.clone();return Ok(status);
        }
        let compatibility=pool_core::compatibility::status(&accounts.store,pool_core::config::codex_config_path()?,false,crate::codex_lifecycle::running_identity(false)).ok();
        let env=crate::windows_environment::report_environment(scale,compatibility.as_ref().and_then(|c| c.client.desktop.as_deref()),compatibility.as_ref().and_then(|c| c.client.engine.as_deref()),uptime);
        let mut report=pool_core::error_reports::prepare(&logs,&r.install_id()?,env,at,limited)?;
        report.reliability=summary;
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
    let (bytes,count)={let draft=reports.draft.lock().unwrap();let d=draft.as_ref().ok_or("请先查看错误报告摘要")?;
        if d.report.report_id!=draft_id { return Err("错误报告已变化，请重新查看摘要".into()); } (d.bytes.clone(),d.report.errors.len())};
    { let r=reports.clone();let bytes=bytes.clone();crate::blocking(move || atomic(&r.root.join("pending.json.gz"),&bytes)).await?; }
    { let mut s=reports.status.lock().unwrap();s.phase="uploading";s.message=None; }
    let result=upload(ENDPOINT,bytes,&draft_id,count).await;
    let persisted = if let Ok(receipt) = &result {
        let root=reports.root.clone();let receipt=receipt.clone();
        Some(crate::blocking(move || save_receipt(&root,&receipt)).await)
    } else { None };
    let mut status=reports.status.lock().unwrap();
    match result {
        Ok(receipt)=>{
            status.receipt=Some(receipt);
            match persisted.unwrap() {
                Ok(history)=>{status.history=history;status.history_error=None;},
                Err(error)=>{
                    status.phase="failed";status.history_error=Some(error);
                    status.message=Some("服务器已接收，但本机回执保存失败；原报告已保留，可重试同一报告".into());
                    return Ok(status.clone());
                }
            }
            // A durable receipt precedes cleanup; unknown submissions retain identical pending bytes.
            if std::fs::remove_file(reports.root.join("pending.json.gz")).is_err_and(|e| e.kind()!=std::io::ErrorKind::NotFound) {
                status.phase="failed";status.message=Some("服务器已接收，但本机回执收尾未完成；重试可取得同一回执".into());
            } else { status.phase="sent";status.message=Some("错误日志上报成功".into());*reports.draft.lock().unwrap()=None; }
        }
        Err(message)=>{status.phase="failed";status.message=Some(message);}
    }
    Ok(status.clone())
}
async fn upload(endpoint: &str, bytes: Vec<u8>, id: &str, count: usize) -> Result<Receipt> {
    let client=reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).https_only(true)
        .connect_timeout(Duration::from_secs(10)).timeout(Duration::from_secs(60)).build().map_err(|_| "无法初始化日志上报连接")?;
    let mut response=client.post(endpoint).header("content-type","application/gzip").body(bytes).send().await
        .map_err(|_| "日志上报连接失败或超时，接收状态未确认；可手动重试同一报告")?;
    if response.status()!=200 { return Err(match response.status().as_u16() { 413=>"错误报告超过接收限制",429=>"日志上报过于频繁，请稍后重试",507=>"日志服务器存储空间不足，请联系维护者",_=>"日志服务器未确认接收，请稍后重试" }.into()); }
    let mut body=Vec::new();while let Some(chunk)=response.chunk().await.map_err(|_| "回执读取中断，可手动重试同一报告")? {
        if body.len()+chunk.len()>16384 { return Err("上报回执超过限制".into()); } body.extend_from_slice(&chunk);
    }
    let receipt:Receipt=serde_json::from_slice(&body).map_err(|_| "上报回执无效，可手动重试同一报告")?;
    if receipt.report_id!=id || receipt.accepted_errors.checked_add(receipt.duplicate_errors)!=Some(count) || receipt.received_at<=0 { return Err("上报回执与当前报告不匹配".into()); }
    Ok(receipt)
}
#[tauri::command]
pub async fn save_error_report(a: tauri::State<'_, crate::App>, draft_id: String) -> Result<String> {
    let r=a.reports.clone();let _operation=r.operation.try_lock().map_err(|_| "错误报告正在处理中，请稍候")?;
    let bytes={let d=r.draft.lock().unwrap();let d=d.as_ref().ok_or("请先查看错误报告摘要")?;if d.report.report_id!=draft_id {return Err("错误报告已变化".into());}d.bytes.clone()};
    let path=r.root.join(format!("error-report-{draft_id}.json.gz"));
    crate::blocking(move || {atomic(&path,&bytes)?;Ok(path.display().to_string())}).await
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn receipts_survive_restart_deduplicate_bound_history_and_preserve_pending_on_failure() {
        let d=tempfile::tempdir().unwrap();let root=d.path();
        std::fs::write(root.join("pending.json.gz"), b"public-fake-pending").unwrap();
        let mut receipt=Receipt {report_id:uuid::Uuid::new_v4().to_string(),received_at:100,accepted_errors:1,duplicate_errors:0};
        save_receipt(root,&receipt).unwrap();save_receipt(root,&receipt).unwrap();
        assert_eq!(Reports::new(root.into()).status.lock().unwrap().history.len(),1);
        for i in 1..12 { receipt.report_id=uuid::Uuid::new_v4().to_string();receipt.received_at=100+i;save_receipt(root,&receipt).unwrap(); }
        let reopened=Reports::new(root.into());let state=reopened.status.lock().unwrap();
        assert_eq!(state.history.len(),10);assert_eq!(state.history[0].received_at,111);
        std::fs::write(root.join("receipts.json"), b"broken").unwrap();
        assert!(save_receipt(root,&receipt).is_err());
        assert!(Reports::new(root.into()).status.lock().unwrap().history_error.is_some());
        assert_eq!(std::fs::read(root.join("receipts.json")).unwrap(),b"broken");
        assert_eq!(std::fs::read(root.join("pending.json.gz")).unwrap(),b"public-fake-pending");
    }
    #[test] fn corrupt_installation_identity_is_not_replaced() {
        let d=tempfile::tempdir().unwrap();let reports=Reports::new(d.path().into());
        let id=reports.install_id().unwrap();assert_eq!(id,reports.install_id().unwrap());
        std::fs::write(d.path().join("installation-id"),"invalid").unwrap();assert!(reports.install_id().is_err());
        assert_eq!(std::fs::read_to_string(d.path().join("installation-id")).unwrap(),"invalid");
    }
}
