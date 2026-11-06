//! Main-window-only commands. API resources never enter the subscription account pool.
use crate::{blocking, close_connection, codex_lifecycle, App, Result};
use pool_core::api_sources::{ApiSource, CheckResult, ClientKind, SourceInput};
use tauri::Manager;

#[tauri::command]
pub async fn api_sources(a: tauri::State<'_, App>) -> Result<Vec<ApiSource>> {
    let sources = a.api_sources.clone();
    blocking(move || sources.list()).await
}

#[tauri::command]
pub async fn save_api_source(a: tauri::State<'_, App>, input: SourceInput) -> Result<ApiSource> {
    let _work = a.updates.enter_work()?;
    let _sources = a.api_sources_lock.write().await;
    if let Some(id) = input.id.as_deref() { a.api_clients.ensure_source_idle(id).await?; }
    let sources = a.api_sources.clone();
    blocking(move || sources.save(input)).await
}

#[tauri::command]
pub async fn remove_api_source(a: tauri::State<'_, App>, id: String) -> Result<()> {
    let _work = a.updates.enter_work()?;
    let _sources = a.api_sources_lock.write().await;
    a.api_clients.ensure_source_idle(&id).await?;
    let sources = a.api_sources.clone();
    blocking(move || sources.remove(&id)).await
}

#[tauri::command]
pub async fn check_api_source(a: tauri::State<'_, App>, id: String, actual: bool) -> Result<CheckResult> {
    let _work = a.updates.enter_work()?;
    let _sources = a.api_sources_lock.read().await;
    if actual { a.api_sources.test_request(&id).await } else { a.api_sources.basic_check(&id).await }
}

#[tauri::command]
pub async fn api_client_status(a: tauri::State<'_, App>) -> Result<Vec<crate::api_clients::ClientStatus>> {
    a.api_clients.statuses().await
}

#[tauri::command]
pub async fn start_api_connection(app: tauri::AppHandle, client: ClientKind, source_id: String) -> Result<()> {
    let a = app.state::<App>();
    let _work = a.updates.enter_work()?;
    let _switch = a.lifecycle.clone().try_lock_owned().map_err(|_| "客户端正在切换，请等待完成")?;
    let _sources = a.api_sources_lock.read().await;
    // Resolve the candidate before changing a working connection.
    let source = a.api_sources.get(&source_id)?;
    let compatible = matches!((client, source.protocol),
        (ClientKind::Codex, pool_core::api_sources::Protocol::Responses)
        | (ClientKind::ClaudeCode, pool_core::api_sources::Protocol::Messages));
    if !source.enabled || !compatible { return Err("来源已停用或协议不适合此客户端".into()); }
    if a.close.lock().map_err(|_| "关闭状态不可用")?.running { return Err("Fusion 正在关闭，请等待完成".into()); }
    if client == ClientKind::Codex {
        // Only close the subscription connection when it actually owns the client.
        // An API-owned provider intentionally lies outside its inspection model.
        if !a.api_clients.has_connection(ClientKind::Codex).await? {
            let before = a.connection.status().await?;
            if before.mode != "off" {
                if !matches!(before.mode, "on" | "recovery_required") { return Err("账号池正在切换，请等待完成".into()); }
                let (progress, store) = (a.connection.clone(), a.accounts.store.clone());
                if let Err(error) = close_connection(&a.connection, move || codex_lifecycle::stop_client(&store, |phase| progress.set_phase(phase)).map(|_| ())).await {
                    a.connection.close_failed(error.clone()).await;
                    return Err(error);
                }
            }
        }
        a.api_clients.start(client, &source_id).await
    } else { a.api_clients.start(client, &source_id).await }
}

#[tauri::command]
pub async fn stop_api_connection(a: tauri::State<'_, App>, client: ClientKind) -> Result<()> {
    let _work = a.updates.enter_work()?;
    let _switch = a.lifecycle.clone().try_lock_owned().map_err(|_| "客户端正在切换，请等待完成")?;
    a.api_clients.stop(client).await
}

#[tauri::command]
pub async fn api_request_logs(a: tauri::State<'_, App>, client: ClientKind) -> Result<Vec<pool_core::api_gateway::ApiRequestLog>> {
    a.api_clients.logs(client).await
}

#[tauri::command]
pub async fn open_claude_login(a: tauri::State<'_, App>) -> Result<()> {
    let _work = a.updates.enter_work()?;
    let _switch = a.lifecycle.clone().try_lock_owned().map_err(|_| "客户端正在切换，请等待完成")?;
    if a.close.lock().map_err(|_| "关闭状态不可用")?.running { return Err("Fusion 正在关闭，请等待完成".into()); }
    a.api_clients.official_claude().await
}
