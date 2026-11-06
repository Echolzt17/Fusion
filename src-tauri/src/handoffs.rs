//! User-selected readable continuation; all native access is isolated behind main-window IPC.
mod rpc;
#[cfg(test)]
mod tests;
use crate::{App, Result};
use pool_core::{
    handoff::{self, PreparedHistory},
    storage::Store,
};
use rpc::Rpc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Mutex, time::Instant};
use tauri::State;

const RECORDS: &str = "handoff/v1/operations";
#[derive(Default)]
pub struct Handoffs {
    prepared: Mutex<HashMap<String, (Instant, PreparedHistory)>>,
    operation: tokio::sync::Mutex<()>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    id: String,
    source_id: String,
    target: String,
    account_id: Option<String>,
    target_id: Option<String>,
    phase: String,
    message: String,
    at: i64,
    #[serde(default)]
    recent_turns: usize,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    id: String,
    history: PreparedHistory,
}
fn records(store: &Store) -> Result<Vec<Record>> {
    store
        .get(RECORDS)?
        .map(|s| serde_json::from_str(&s).map_err(|_| "续接记录损坏，未自动重试".into()))
        .unwrap_or_else(|| Ok(Vec::new()))
}
fn save(store: &Store, record: &Record) -> Result<()> {
    let mut all = records(store)?;
    all.retain(|r| r.id != record.id);
    all.insert(0, record.clone());
    all.truncate(20);
    store.set(
        RECORDS,
        &serde_json::to_string(&all).map_err(|_| "无法保存续接进度")?,
    )
}
fn find(store: &Store, id: &str) -> Result<Record> {
    records(store)?
        .into_iter()
        .find(|r| r.id == id)
        .ok_or_else(|| "续接记录不存在，请重新选择聊天".into())
}
async fn connect() -> Result<Rpc> {
    let engine = tokio::task::spawn_blocking(|| {
        if crate::codex_window::find_any().is_some() {
            // An ambiguous or unverifiable running client must not be bypassed
            // by silently choosing a different installed kernel.
            crate::codex_lifecycle::running_identity(true).map(|i| i.engine_path)
        } else {
            crate::codex_window::installed_client().map(|i| i.engine())
        }
    })
    .await
    .map_err(|_| "客户端识别任务中断")??;
    let config = pool_core::config::codex_config_path()?;
    Rpc::start(&engine, config.parent().ok_or("Codex 数据目录无效")?).await
}
async fn route(app: &App, rpc: &mut Rpc, target: &str, account: Option<&str>) -> Result<String> {
    let status = app.connection.status().await?;
    let config = rpc
        .call("config/read", json!({"includeLayers":false}))
        .await?;
    let config = &config["config"];
    let provider = config["model_provider"].as_str().unwrap_or("openai");
    match target {
        "pool" if status.mode == "on" && provider == "personal_codex_pool" => {
            let account = app
                .accounts
                .store
                .account(account.ok_or("请选择目标池账号")?)?;
            if !app.gateway.scheduler.available(&account) {
                return Err("目标池账号当前不可用，请恢复该账号或选择其他账号".into());
            }
            Ok(provider.into())
        }
        "official"
            if status.mode == "off"
                && !status.recovery_pending
                && status.reload_required.is_none()
                && provider == "openai" =>
        {
            if config["model_providers"]["openai"]["base_url"]
                .as_str()
                .is_some_and(|url| url.trim_end_matches('/') != "https://api.openai.com/v1")
                || config["chatgpt_base_url"].as_str().is_some_and(|url| {
                    url.trim_end_matches('/') != "https://chatgpt.com/backend-api"
                })
            {
                return Err("当前官方 provider 配置了其他地址，请先检查接入配置".into());
            }
            Ok("openai".into())
        }
        _ => Err("请先将账号池切换到目标稳定接入；官方续接须关闭账号池并恢复官方接入".into()),
    }
}
async fn history(rpc: &mut Rpc, id: &str, recent_turns: usize) -> Result<Value> {
    if recent_turns > 1000 {
        return Err("交接轮次范围无效".into());
    }
    if !handoff::valid_id(id) {
        return Err("聊天标识无效".into());
    }
    let mut thread = rpc
        .call("thread/read", json!({"threadId":id,"includeTurns":false}))
        .await?["thread"]
        .take();
    let mut turns = Vec::new();
    let mut cursor = Value::Null;
    let mut bytes = 0usize;
    let mut partial = false;
    'pages: loop {
        let page = rpc
            .call(
                "thread/turns/list",
                json!({"threadId":id,"cursor":cursor,"limit":100,"sortDirection":"desc"}),
            )
            .await?;
        let page_turns = page["data"].as_array().ok_or("客户端轮次格式不兼容")?;
        for (index, turn) in page_turns.iter().enumerate() {
            let mut turn = turn.clone();
            // Paginated history keeps items behind a separate cursor. Never
            // mistake a metadata-only turn for a complete transcript.
            let mut items = Vec::new();
            let mut item_cursor = Value::Null;
            loop {
                let part = rpc.call("thread/items/list", json!({"threadId":id,"turnId":turn["id"],"cursor":item_cursor,"limit":100,"sortDirection":"asc"})).await?;
                bytes = bytes.saturating_add(part.to_string().len());
                if bytes > 16 * 1024 * 1024 {
                    return Err("可读历史超过读取预算，请改选更少的近期轮次；未静默截断".into());
                }
                items.extend(
                    part["data"]
                        .as_array()
                        .ok_or("客户端历史条目格式不兼容")?
                        .iter()
                        .cloned(),
                );
                if items.len() > 20_000 {
                    return Err("单轮历史条目过多，暂不能安全交接".into());
                }
                item_cursor = part["nextCursor"].clone();
                if item_cursor.is_null() {
                    break;
                }
            }
            turn["items"] = json!(items);
            turns.push(turn);
            if recent_turns > 0 && turns.len() >= recent_turns {
                partial = index + 1 < page_turns.len() || !page["nextCursor"].is_null();
                break 'pages;
            }
            if turns.len() > 1000 {
                return Err("历史超过 1000 轮，请在客户端建立近期续接聊天后再交接".into());
            }
        }
        cursor = page["nextCursor"].clone();
        if cursor.is_null() {
            break;
        }
    }
    turns.reverse();
    thread["turns"] = json!(turns);
    thread["handoffPartial"] = json!(partial);
    if thread["turns"]
        .as_array()
        .is_some_and(|turns| turns.iter().any(|t| t["status"] == "inProgress"))
    {
        return Err("原聊天存在进行中的轮次，请等待完成".into());
    }
    Ok(thread)
}
#[tauri::command]
pub async fn handoff_chats(cursor: Option<String>, archived: bool) -> Result<Value> {
    let mut rpc = connect().await?;
    let result = rpc.call("thread/list", json!({"limit":50,"cursor":cursor,"archived":archived,"modelProviders":[]})).await.map(|value| {
        let data: Vec<_> = value["data"].as_array().into_iter().flatten().filter(|t| handoff::valid_id(t["id"].as_str().unwrap_or(""))).map(|t| json!({"id":t["id"],"name":t["name"],"provider":t["modelProvider"],"updatedAt":t["updatedAt"]})).collect();
        json!({"data":data,"nextCursor":value["nextCursor"]})
    });
    rpc.close().await;
    result
}
#[tauri::command]
pub async fn handoff_prepare(
    a: State<'_, App>,
    source_id: String,
    target: String,
    account_id: Option<String>,
    recent_turns: usize,
) -> Result<Preview> {
    let _operation = a.handoffs.operation.lock().await;
    let mut rpc = connect().await?;
    let result = async {
        route(&a, &mut rpc, &target, account_id.as_deref()).await?;
        let mut prepared = handoff::prepare(
            &history(&mut rpc, &source_id, recent_turns).await?,
            recent_turns,
        )?;
        if prepared
            .attachments
            .iter()
            .any(|path| !local_attachment(path))
        {
            let note = "部分本地附件已缺失或不能安全引用，需要时请重新提供。";
            prepared.warnings.push(note.into());
            prepared.message.push_str(&format!("\n{note}"));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let record = Record {
            id: id.clone(),
            source_id,
            target,
            account_id,
            target_id: None,
            recent_turns,
            phase: "prepared".into(),
            message: "交接已准备；查看范围后创建聊天".into(),
            at: pool_core::now(),
        };
        save(&a.accounts.store, &record)?;
        let mut pending = a.handoffs.prepared.lock().map_err(|_| "续接准备状态异常")?;
        pending.retain(|_, (at, _)| at.elapsed().as_secs() < 600);
        if pending.len() >= 8 {
            return Err("准备中的续接过多，请完成或取消后再试".into());
        }
        pending.insert(id.clone(), (Instant::now(), prepared.clone()));
        Ok(Preview {
            id,
            history: prepared,
        })
    }
    .await;
    rpc.close().await;
    result
}
fn prepared(app: &App, id: &str) -> Result<PreparedHistory> {
    app.handoffs
        .prepared
        .lock()
        .map_err(|_| "续接准备状态异常")?
        .get(id)
        .filter(|(at, _)| at.elapsed().as_secs() < 600)
        .map(|(_, h)| h.clone())
        .ok_or_else(|| "交接预览已过期或 Fusion 已重启，请重新准备；未自动重发".into())
}
#[tauri::command]
pub async fn handoff_create(a: State<'_, App>, id: String) -> Result<Record> {
    let _operation = a.handoffs.operation.lock().await;
    let _lifecycle = a.lifecycle.lock().await;
    let mut record = find(&a.accounts.store, &id)?;
    if record.phase == "created" || record.phase == "complete" {
        return Ok(record);
    }
    if record.phase != "prepared" {
        return Err("本次创建已发起或结果未确认，请查看续接记录；未重复创建".into());
    }
    let prepared = prepared(&a, &id)?;
    let mut rpc = connect().await?;
    let result = async {
        let provider = route(&a, &mut rpc, &record.target, record.account_id.as_deref()).await?;
        let source = history(&mut rpc, &record.source_id, record.recent_turns).await?;
        if handoff::revision(&source) != prepared.source_revision { return Err("原聊天已变化，请重新准备交接".into()); }
        record.phase = "creating".into(); record.message = "创建已发起；结果不明时不会自动重试".into(); save(&a.accounts.store, &record)?;
        let target = rpc.call("thread/start", json!({"modelProvider":provider,"cwd":source["cwd"],"ephemeral":false,"serviceName":"fusion_handoff"})).await?;
        let target_id = target["thread"]["id"].as_str().filter(|id|handoff::valid_id(id)).ok_or("目标聊天标识无效，创建结果未确认")?.to_owned();
        record.target_id = Some(target_id.clone());
        // Save its identity before any other action, so even a binding failure
        // leaves a reviewable empty thread instead of causing a duplicate.
        save(&a.accounts.store, &record)?;
        if let Some(account) = record.account_id.as_deref().filter(|_| record.target == "pool") {
            a.accounts.store.bind(&pool_core::scheduler::binding_key("thread", &target_id), account)?;
        }
        let title = source["name"].as_str().unwrap_or("聊天").chars().take(60).collect::<String>();
        let _ = rpc.call("thread/name/set", json!({"threadId":target_id,"name":format!("{title} · 续接（{}）", if record.target == "pool" { "账号池" } else { "官方" })})).await;
        record.phase = "created".into(); record.message = "聊天已创建；点击发送交接才开始目标账号请求".into(); save(&a.accounts.store, &record)?;
        Ok(record.clone())
    }.await;
    rpc.close().await;
    result
}
#[tauri::command]
pub async fn handoff_send(a: State<'_, App>, id: String) -> Result<Record> {
    let _operation = a.handoffs.operation.lock().await;
    let _lifecycle = a.lifecycle.lock().await;
    let mut record = find(&a.accounts.store, &id)?;
    if record.phase == "complete" {
        return Ok(record);
    }
    if record.phase != "created" {
        return Err("交接已发送或结果未确认，请查看目标聊天；未重复发送".into());
    }
    let mut prepared = prepared(&a, &id)?;
    let target_id = record.target_id.clone().ok_or("目标聊天尚未创建")?;
    let mut rpc = connect().await?;
    let result = async {
        route(&a, &mut rpc, &record.target, record.account_id.as_deref()).await?;
        if handoff::revision(&history(&mut rpc, &record.source_id, record.recent_turns).await?) != prepared.source_revision { return Err("原聊天已变化，请重新准备交接；当前目标聊天尚未发送".into()); }
        let target_history = history(&mut rpc, &target_id, 1).await?;
        if !target_history["turns"].as_array().is_some_and(|turns| turns.is_empty()) { return Err("目标聊天已经开始使用，未插入交接或重复执行；请重新准备".into()); }
        let resumed = rpc.call("thread/resume", json!({"threadId":target_id,"excludeTurns":true})).await?;
        if resumed["thread"]["id"] != target_id { return Err("目标聊天核验失败".into()); }
        let mut input = vec![json!({"type":"text","text":prepared.message})];
        for path in &prepared.attachments {
            if local_attachment(path) { input.push(json!({"type":"localImage","path":path})); }
            else { input.push(json!({"type":"text","text":"[原聊天本地附件已缺失或不可安全引用；需要时请重新提供。]"})); }
        }
        record.phase = "sending".into(); record.message = "交接已发送或正在发送；结果不明时不会自动重发".into(); save(&a.accounts.store, &record)?;
        rpc.call("turn/start", json!({"threadId":target_id,"input":input})).await?;
        rpc.completed(&target_id).await?;
        record.phase = "complete".into(); record.message = "交接轮次已完成，请在目标聊天继续。工具使用仍以你的新要求为准。".into(); save(&a.accounts.store, &record)?;
        Ok(record.clone())
    }.await;
    prepared.message.clear();
    rpc.close().await;
    if result.is_ok() {
        if let Ok(mut pending) = a.handoffs.prepared.lock() {
            pending.remove(&id);
        }
    }
    result
}
fn local_attachment(raw: &str) -> bool {
    let path = std::path::Path::new(raw);
    if !path.is_absolute()
        || raw.starts_with(['\\', '/'])
        || !path.extension().and_then(|s| s.to_str()).is_some_and(|s| {
            ["png", "jpg", "jpeg", "webp", "gif"].contains(&s.to_ascii_lowercase().as_str())
        })
    {
        return false;
    }
    for ancestor in path.ancestors() {
        let Ok(metadata) = std::fs::symlink_metadata(ancestor) else {
            return false;
        };
        if metadata.file_type().is_symlink() {
            return false;
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return false;
            }
        }
    }
    path.is_file()
}
#[tauri::command]
pub fn handoff_status(a: State<'_, App>) -> Result<Vec<Record>> {
    let mut all = records(&a.accounts.store)?;
    if a.handoffs.operation.try_lock().is_ok() {
        for record in &mut all {
            if matches!(record.phase.as_str(), "creating" | "sending") {
                record.phase = "unknown".into();
                record.message =
                    "上次操作结果未确认，请查看目标聊天；不会自动重新创建或发送".into();
            }
        }
    }
    Ok(all)
}
#[tauri::command]
pub async fn handoff_cancel(a: State<'_, App>, id: String) -> Result<()> {
    let _operation = a.handoffs.operation.lock().await;
    let mut record = find(&a.accounts.store, &id)?;
    if !matches!(record.phase.as_str(), "prepared" | "created") {
        return Err("已发起的操作不能当作未发送取消；请查看目标聊天".into());
    }
    record.phase = "cancelled".into();
    record.message = "已取消交接；已创建的空聊天保留，原聊天未修改".into();
    save(&a.accounts.store, &record)?;
    a.handoffs
        .prepared
        .lock()
        .map_err(|_| "续接准备状态异常")?
        .remove(&id);
    Ok(())
}
#[tauri::command]
pub async fn handoff_open(app: tauri::AppHandle, a: State<'_, App>, id: String) -> Result<()> {
    use tauri_plugin_opener::OpenerExt;
    let record = find(&a.accounts.store, &id)?;
    let mode = a.connection.status().await?.mode;
    if (record.target == "pool" && mode != "on") || (record.target == "official" && mode != "off") {
        return Err("当前接入与目标聊天不一致，请先切换接入".into());
    }
    let target = record
        .target_id
        .filter(|id| handoff::valid_id(id))
        .ok_or("目标聊天尚未创建")?;
    app.opener()
        .open_url(format!("codex://threads/{target}"), None::<&str>)
        .map_err(|_| "无法打开目标聊天，请手动打开 Codex 查找续接聊天".into())
}
