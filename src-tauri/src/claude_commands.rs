use crate::{App, Result};
use pool_core::{api_sources::ClientKind, claude_pool};
use serde::Deserialize;
use tauri::Manager;
use tauri_plugin_opener::OpenerExt;

#[tauri::command]
pub async fn claude_pool_status(
    a: tauri::State<'_, App>,
    logs: Option<bool>,
) -> Result<serde_json::Value> {
    let client = a
        .api_clients
        .statuses()
        .await?
        .into_iter()
        .find(|s| s.client == ClientKind::ClaudeCode)
        .ok_or("Claude 状态不可用")?;
    let records = if logs.unwrap_or(false) {
        a.api_clients
            .logs(ClientKind::ClaudeCode)
            .await?
            .into_iter()
            .filter(|l| l.source_id.starts_with("claude-"))
            .collect::<Vec<_>>()
    } else {
        vec![]
    };
    let pool = a.claude_pool.clone();
    let login = a.claude_login.status();
    crate::blocking(move||Ok(serde_json::json!({"accounts":pool.accounts()?,"preferred":pool.preferred()?,"summary":pool.summary()?,"client":client,"login":login,"logs":records}))).await
}
#[tauri::command]
pub async fn claude_pool_login(app: tauri::AppHandle, id: Option<String>) -> Result<()> {
    let a = app.state::<App>();
    let _work = a.updates.enter_work()?;
    let url = a.claude_login.begin(id).await?;
    if app.opener().open_url(url, None::<&str>).is_err() {
        a.claude_login.cancel();
        return Err("无法打开 Claude 登录浏览器，请重试".into());
    }
    Ok(())
}
#[tauri::command]
pub fn claude_pool_cancel_login(a: tauri::State<App>) {
    a.claude_login.cancel();
}
#[tauri::command]
pub async fn claude_pool_start(a: tauri::State<'_, App>) -> Result<()> {
    let _work = a.updates.enter_work()?;
    let _switch = a
        .lifecycle
        .try_lock()
        .map_err(|_| "客户端正在切换，请等待完成")?;
    if a.claude_login.active() {
        return Err("请先完成或取消 Claude 登录".into());
    }
    a.api_clients
        .start(ClientKind::ClaudeCode, claude_pool::SOURCE_ID)
        .await
}
#[tauri::command]
pub async fn claude_pool_stop(a: tauri::State<'_, App>) -> Result<()> {
    let _work = a.updates.enter_work()?;
    let _switch = a
        .lifecycle
        .try_lock()
        .map_err(|_| "客户端正在切换，请等待完成")?;
    a.api_clients.stop(ClientKind::ClaudeCode).await
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Action {
    Select,
    Pause,
    Resume,
    Remove,
    Refresh,
}
#[tauri::command]
pub async fn claude_pool_account(
    a: tauri::State<'_, App>,
    action: Action,
    id: Option<String>,
) -> Result<()> {
    let _work = a.updates.enter_work()?;
    if matches!(action, Action::Select) {
        let pool = a.claude_pool.clone();
        return crate::blocking(move || pool.select(id)).await;
    }
    let id = id.ok_or("请选择 Claude 账号")?;
    match action {
        Action::Refresh => a.claude_pool.refresh(&id).await,
        Action::Remove => {
            let _switch = a
                .lifecycle
                .try_lock()
                .map_err(|_| "客户端正在切换，请稍后移除账号")?;
            if a.api_clients.has_connection(ClientKind::ClaudeCode).await?
                || a.claude_login.active()
            {
                return Err(
                    "请先关闭 Claude 连接并完成登录，再移除账号；原会话绑定无法恢复".into(),
                );
            }
            a.claude_pool.remove(&id).await
        }
        Action::Pause | Action::Resume => {
            let pool = a.claude_pool.clone();
            crate::blocking(move || pool.pause(&id, matches!(action, Action::Pause))).await
        }
        Action::Select => unreachable!(),
    }
}

pub fn start_background(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            {
                let a = app.state::<App>();
                if let Ok(_work) = a.updates.enter_work() {
                    let accounts = a.claude_pool.accounts().unwrap_or_default();
                    for account in accounts {
                        if !matches!(account.state.as_str(), "reauth" | "deleting")
                            && a.claude_pool.quota_due(&account.id).unwrap_or(false)
                        {
                            let _ = a.claude_pool.refresh(&account.id).await;
                        }
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
    });
}
