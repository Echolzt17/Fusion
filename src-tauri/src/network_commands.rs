//! The main window is the only network-management IPC consumer.
use crate::{App, Result};
use pool_core::network::{NetworkSettings, NetworkStatus, SubscriptionInput};

#[tauri::command]
pub fn network_status(a: tauri::State<App>) -> Result<NetworkStatus> { a.network.status() }

#[tauri::command]
pub async fn network_import(a: tauri::State<'_, App>, input: SubscriptionInput) -> Result<NetworkStatus> {
    let _work = a.updates.enter_work()?;
    a.network.import_subscription(input).await
}
#[tauri::command]
pub async fn network_refresh(a: tauri::State<'_, App>, system_proxy: bool) -> Result<NetworkStatus> {
    let _work = a.updates.enter_work()?;
    a.network.refresh_subscription(system_proxy).await
}
#[tauri::command]
pub async fn network_remove(a: tauri::State<'_, App>) -> Result<NetworkStatus> {
    let _work = a.updates.enter_work()?;
    a.network.remove_subscription().await
}
#[tauri::command]
pub async fn network_save_settings(a: tauri::State<'_, App>, settings: NetworkSettings) -> Result<NetworkStatus> {
    let _work = a.updates.enter_work()?;
    a.network.save_settings(settings).await
}
#[tauri::command]
pub async fn network_select(a: tauri::State<'_, App>, id: String, fixed: bool) -> Result<NetworkStatus> {
    let _work = a.updates.enter_work()?;
    a.network.select_node(id, fixed).await
}
#[tauri::command]
pub async fn network_check(a: tauri::State<'_, App>, ids: Option<Vec<String>>) -> Result<NetworkStatus> {
    let _work = a.updates.enter_work()?;
    a.network.check_nodes(ids).await
}
#[tauri::command]
pub async fn network_set_enabled(a: tauri::State<'_, App>, enabled: bool) -> Result<NetworkStatus> {
    let _work = a.updates.enter_work()?;
    a.network.set_enabled(enabled).await
}
#[tauri::command]
pub async fn network_retry(a: tauri::State<'_, App>) -> Result<NetworkStatus> {
    let _work = a.updates.enter_work()?;
    a.network.retry().await
}
