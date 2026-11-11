#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use pool_core::{
    account::Accounts,
    config,
    connection::Connection,
    gateway::{Gateway, ServiceStatus},
    oauth::{LoginStatus, OAuth},
    scheduler::{SchedulingStatus, SelectionPolicy},
    storage::Store,
    subscription, vault, RequestLog, Result, Settings,
};
use std::sync::Arc;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager,
};
use tauri_plugin_opener::OpenerExt;
#[cfg(windows)]
mod codex_window;
mod codex_lifecycle;
#[cfg(test)]
mod permissions_tests;
mod recovery_ui;
mod quota_refresh;
mod usage_dashboard;
mod widget;
mod handoffs;
mod api_clients;
mod api_commands;
mod widget_input;
mod window_layout;
mod windows_environment;
mod uninstall_data;
mod updates;
mod pricing;
mod error_reports;
mod network_kernel;
mod network_commands;
mod claude_commands;

struct App {
    network: Arc<pool_core::network::Network>,
    pricing: pricing::Pricing,
    api_sources: Arc<pool_core::api_sources::Sources>,
    api_clients: Arc<api_clients::ApiClients>,
    claude_pool: Arc<pool_core::claude_pool::Pool>,
    claude_login: Arc<pool_core::claude_pool::login::Login>,
    api_sources_lock: tokio::sync::RwLock<()>,
    handoffs: Arc<handoffs::Handoffs>,
    usage_dashboard: Arc<usage_dashboard::UsageDashboard>,
    reports: Arc<error_reports::Reports>,
    updates: Arc<updates::Updates>,
    accounts: Arc<Accounts>,
    oauth: Arc<OAuth>,
    gateway: Arc<Gateway>,
    connection: Arc<Connection>,
    lifecycle: Arc<tokio::sync::Mutex<()>>,
    close: Arc<std::sync::Mutex<CloseIntent>>,
}
#[derive(Default)]
struct CloseIntent {
    running: bool,
    exit_app: bool,
}
impl CloseIntent {
    fn request(&mut self, exit_app: bool) -> bool {
        self.exit_app |= exit_app;
        if self.running {
            return false;
        }
        self.running = true;
        true
    }
}
fn request_close(app: tauri::AppHandle, exit_app: bool) {
    let a = app.state::<App>();
    let mut close = a.close.lock().unwrap();
    if a.updates.installing.load(std::sync::atomic::Ordering::SeqCst) { return; }
    if !close.request(exit_app) {
        return;
    }
    drop(close);
    let (connection, oauth, lifecycle, intent) = (
        a.connection.clone(),
        a.oauth.clone(),
        a.lifecycle.clone(),
        a.close.clone(),
    );
    tauri::async_runtime::spawn(async move {
        let _switch = lifecycle.lock().await;
        let mut network_checks = if exit_app {
            Some(app.state::<App>().network.pause_background_checks())
        } else { None };
        let mut exit_work = if exit_app {
            Some(app.state::<App>().updates.freeze_work().await)
        } else { None };
        oauth.cancel();
        if exit_app {app.state::<App>().claude_login.cancel();app.state::<App>().claude_login.wait_idle().await;}
        let progress = connection.clone();
        let store = app.state::<App>().accounts.store.clone();
        let mut result = async {
            let clients = app.state::<App>().api_clients.clone();
            let api_owned = clients.has_connection(pool_core::api_sources::ClientKind::Codex).await?;
            if exit_app { clients.stop_all().await?; }
            else { clients.stop(pool_core::api_sources::ClientKind::Codex).await?; }
            if api_owned && !config::has_backup(&store)? { return Ok(()); }
            let result = close_connection(&connection, move || {
                codex_lifecycle::stop_client(&store, |phase| progress.set_phase(phase)).map(|_| ())
            }).await;
            if let Err(error) = &result { connection.close_failed(error.clone()).await; }
            result
        }.await;
        let exit_app = {
            let mut intent = intent.lock().unwrap();
            let exit_app = intent.exit_app;
            intent.running = false;
            intent.exit_app = false;
            exit_app
        };
        // A tray exit may upgrade a close already in progress. Drain the other
        // managed client before honoring that later exit intent.
        if exit_app {
            if network_checks.is_none() { network_checks = Some(app.state::<App>().network.pause_background_checks()); }
            if exit_work.is_none() { exit_work = Some(app.state::<App>().updates.freeze_work().await); }
            app.state::<App>().claude_login.cancel();
            app.state::<App>().claude_login.wait_idle().await;
            let closed = app.state::<App>().api_clients.stop_all().await;
            if result.is_ok() { result = closed; }
            if result.is_ok() { result = app.state::<App>().accounts.wait_background_idle().await; }
            if result.is_ok() { result = app.state::<App>().claude_pool.wait_idle().await; }
            if result.is_ok() { result = app.state::<App>().network.shutdown().await; }
        }
        // Keep the work barrier alive through the final outcome below.
        let _exit_work = exit_work;
        let _network_checks = network_checks;
        match result {
            Ok(()) if exit_app => app.exit(0),
            Ok(()) => {
                let _ = app.emit("close-complete", ());
            }
            Err(error) => {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.emit("main-visible", true);
                    let _ = w.set_focus();
                }
                let _ = app.emit(
                    "close-failed",
                    serde_json::json!({"message":error,"exitApp":exit_app}),
                );
            }
        }
    });
}
async fn close_connection(
    connection: &Connection,
    stop_client: impl FnOnce() -> Result<()> + Send + 'static,
) -> Result<()> {
    let needs_stop = connection.prepare_close().await?;
    connection.wait_drained().await?;
    // Only a pool-owned switch requires touching the client. An independent
    // desktop remains running when Fusion is already detached.
    if needs_stop {
        blocking(stop_client).await?;
    }
    connection.complete_close().await
}

#[cfg(test)]
mod close_tests {
    use super::*;
    struct NoCredentials;
    impl vault::Vault for NoCredentials {
        fn get(&self, _: &str) -> Result<Option<String>> { panic!("close must not read credentials") }
        fn set(&self, _: &str, _: &str) -> Result<()> { panic!("close must not write credentials") }
        fn delete(&self, _: &str) -> Result<()> { panic!("close must not delete credentials") }
    }
    #[tokio::test]
    async fn ordinary_exit_skips_client_lookup_and_stop_failure_keeps_recovery_until_retry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let exe = dir.path().join("pool.exe");
        std::fs::write(&path, "model='original'\n").unwrap();
        let store = Arc::new(Store::open(&dir.path().join("pool.sqlite")).unwrap());
        let accounts = Arc::new(Accounts::new(store.clone(), Arc::new(NoCredentials)).unwrap());
        let connection = Connection::new(Gateway::new(accounts), path.clone(), exe.clone());
        store.set("codex_client_reload_required", "original").unwrap();
        close_connection(&connection, || panic!("an off pool must not inspect Codex windows")).await.unwrap();
        assert_eq!(connection.status().await.unwrap().mode, "off");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "model='original'\n");

        let preview = config::preview(&store, &path, &exe, "fusion-public-fixture-key").unwrap();
        config::apply(&store, &path, &exe, "fusion-public-fixture-key", &preview.fingerprint).unwrap();
        let error = close_connection(&connection, || Err("fixture: no operable Codex window".into())).await.unwrap_err();
        assert!(error.contains("no operable"));
        connection.close_failed(error).await;
        assert!(config::has_backup(&store).unwrap());
        assert_eq!(store.get("codex_client_reload_required").unwrap().as_deref(), Some("original"));
        assert_eq!(connection.status().await.unwrap().mode, "recovery_required");
        let pending = store.clone();
        close_connection(&connection, move || {
            // A verified stop suffices. There is no launch/window callback.
            assert!(config::has_backup(&pending)?);
            assert_eq!(pending.get("codex_client_reload_required")?.as_deref(), Some("original"));
            Ok(())
        }).await.unwrap();
        assert!(!config::has_backup(&store).unwrap());
        assert_eq!(connection.status().await.unwrap().mode, "off");
    }
    #[tokio::test]
    async fn config_change_after_client_exit_never_clears_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let exe = dir.path().join("pool.exe");
        std::fs::write(&path, "model='original'\n").unwrap();
        let store = Arc::new(Store::open(&dir.path().join("pool.sqlite")).unwrap());
        let accounts = Arc::new(Accounts::new(store.clone(), Arc::new(NoCredentials)).unwrap());
        let connection = Connection::new(Gateway::new(accounts), path.clone(), exe.clone());
        let preview = config::preview(&store, &path, &exe, "fusion-public-fixture-key").unwrap();
        config::apply(&store, &path, &exe, "fusion-public-fixture-key", &preview.fingerprint).unwrap();
        let applied = std::fs::read_to_string(&path).unwrap();
        let error = close_connection(&connection, move || {
            std::fs::write(path, applied).unwrap();
            Ok(())
        }).await.unwrap_err();
        connection.close_failed(error).await;
        assert!(config::has_backup(&store).unwrap());
        assert_eq!(store.get("codex_client_reload_required").unwrap().as_deref(), Some("original"));
        assert_eq!(connection.status().await.unwrap().mode, "recovery_required");
    }
    #[test]
    fn concurrent_close_requests_share_one_run_and_exit_escalates() {
        let intent = Arc::new(std::sync::Mutex::new(CloseIntent::default()));
        let barrier = Arc::new(std::sync::Barrier::new(12));
        let handles: Vec<_> = (0..12)
            .map(|i| {
                let (intent, barrier) = (intent.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    intent.lock().unwrap().request(i == 7)
                })
            })
            .collect();
        assert_eq!(
            handles
                .into_iter()
                .map(|h| usize::from(h.join().unwrap()))
                .sum::<usize>(),
            1
        );
        assert!(intent.lock().unwrap().exit_app);
    }
}
fn data_dir() -> Result<std::path::PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(|v| std::path::PathBuf::from(v).join("PersonalCodexPool"))
        .ok_or("无法找到 Windows 用户数据目录".into())
}
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|_| "本机数据任务异常")?
}
#[tauri::command]
fn main_visible(window: tauri::WebviewWindow) -> bool {
    window.is_visible().unwrap_or(false) && !window.is_minimized().unwrap_or(true)
}
#[tauri::command]
async fn ai_pricing(a: tauri::State<'_, App>) -> std::result::Result<pricing::PricingSnapshot, String> {
    a.pricing.load().await
}
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
enum PricingWebsite { Comparison, Recharge }
#[tauri::command]
fn open_pricing_website(app: tauri::AppHandle, site: PricingWebsite) -> std::result::Result<(), String> {
    let url = match site {
        PricingWebsite::Comparison => "https://opentherank.com/zh/ai-pricing/",
        PricingWebsite::Recharge => "https://priceai.cc/products/chatgpt-codex-service",
    };
    app.opener().open_url(url, None::<&str>).map_err(|_| "无法打开系统浏览器".into())
}
#[tauri::command]
async fn accounts(a: tauri::State<'_, App>) -> Result<Vec<serde_json::Value>> {
    let accounts = a.accounts.clone();
    let scheduler = a.gateway.scheduler.clone();
    blocking(move || {
        accounts
            .store
            .accounts()?
            .into_iter()
            .map(|account| {
                let mut value = serde_json::to_value(&account).map_err(|_| "账号编码失败")?;
                let fallback = accounts.store.get(&format!("account-plan/v1/{}", account.id))?.unwrap_or_default();
                value["planLabel"] = serde_json::json!(if subscription::code(&account).is_empty() { subscription::label_code(&fallback) } else { subscription::label(&account) });
                value["available"] = serde_json::json!(scheduler.available(&account));
                value["quotaRefreshing"] = serde_json::json!(accounts.quota_refreshing(&account.id));
                Ok(value)
            })
            .collect()
    })
    .await
}
#[tauri::command]
async fn settings(a: tauri::State<'_, App>) -> Result<Settings> {
    let store = a.accounts.store.clone();
    blocking(move || store.settings()).await
}
#[tauri::command]
async fn save_settings(a: tauri::State<'_, App>, settings: Settings) -> Result<()> {
    let _work = a.updates.enter_work()?;
    a.connection
        .save_service_settings(settings.port, settings.concurrency)
        .await
}
#[tauri::command]
async fn login(
    app: tauri::AppHandle,
    a: tauri::State<'_, App>,
    account_id: Option<String>,
) -> Result<()> {
    let _work = a.updates.enter_work()?;
    let url = a.oauth.start(account_id).await?;
    if app.opener().open_url(url, None::<&str>).is_err() {
        a.oauth.cancel();
        return Err("无法打开浏览器；请检查默认浏览器设置后重试".into());
    }
    Ok(())
}
#[tauri::command]
fn login_status(a: tauri::State<App>) -> LoginStatus {
    a.oauth.status()
}
#[tauri::command]
fn cancel_login(a: tauri::State<App>) {
    a.oauth.cancel();
}
#[tauri::command]
fn uninstall_requested() -> bool {
    std::env::args().any(|s| s == "--prepare-uninstall")
}
#[tauri::command]
async fn pause_account(a: tauri::State<'_, App>, id: String, paused: bool) -> Result<()> {
    let _work = a.updates.enter_work()?;
    a.accounts.set_state(&id, paused).await
}
#[tauri::command]
async fn remove_account(a: tauri::State<'_, App>, id: String) -> Result<()> {
    let _work = a.updates.enter_work()?;
    a.gateway.remove_account(&id).await
}
#[tauri::command]
async fn refresh_quota(a: tauri::State<'_, App>, id: String) -> Result<()> {
    let _work = a.updates.enter_work()?;
    a.accounts.quota(&id).await
}
#[tauri::command]
async fn reset_credits(a: tauri::State<'_, App>, id: String) -> Result<pool_core::ResetCredits> {
    let _work = a.updates.enter_work()?;
    a.accounts.reset_credits(&id).await
}
#[tauri::command]
async fn consume_reset(a: tauri::State<'_, App>, id: String, credit_id: String) -> Result<String> {
    let _work = a.updates.enter_work()?;
    let message = a.accounts.consume_reset(&id, &credit_id).await?;
    let _ = a.accounts.quota(&id).await;
    Ok(message)
}
#[tauri::command]
async fn service_status(a: tauri::State<'_, App>) -> Result<ServiceStatus> {
    Ok(a.gateway.status().await)
}
#[tauri::command]
async fn cancel_request(a: tauri::State<'_, App>, id: String) -> Result<()> {
    a.gateway.cancel_request(&id).await
}
#[tauri::command]
async fn connection_status(a: tauri::State<'_, App>) -> Result<pool_core::connection::Status> {
    if a.api_clients.has_connection(pool_core::api_sources::ClientKind::Codex).await? {
        return Ok(pool_core::connection::Status { mode: "off", configured: false, recovery_pending: false,
            reload_required: None, error: None, operation: None, phase: None, operation_started_at: None });
    }
    a.connection.status().await
}
#[tauri::command]
async fn close_pool(app: tauri::AppHandle, exit_app: bool) -> Result<()> {
    request_close(app, exit_app);
    Ok(())
}
#[tauri::command]
async fn logs(a: tauri::State<'_, App>) -> Result<Vec<RequestLog>> {
    let store = a.accounts.store.clone();
    blocking(move || store.logs()).await
}
#[tauri::command]
async fn request_history(a: tauri::State<'_, App>, conversation_key: Option<String>, result: String, system: bool, cursor: Option<pool_core::storage::history::LogCursor>, filter: Option<pool_core::storage::history::HistoryFilter>) -> Result<pool_core::storage::history::LogPage> {
    let store = a.accounts.store.clone();
    blocking(move || store.filtered_log_page(conversation_key.as_deref(), &result, system, cursor, filter.unwrap_or_default())).await
}
#[tauri::command]
async fn conversation_usage(a: tauri::State<'_, App>) -> Result<serde_json::Value> {
    let store = a.accounts.store.clone();
    blocking(move || Ok(serde_json::json!({"conversations":store.conversations()?,"recentRequest":store.recent_request(),"catalog":store.history_catalog()?}))).await
}
#[tauri::command]
async fn usage(a: tauri::State<'_, App>) -> Result<Vec<pool_core::usage::AccountUsage>> {
    let store = a.accounts.store.clone();
    blocking(move || store.usage()).await
}
#[tauri::command]
async fn usage_dashboard(a: tauri::State<'_, App>) -> Result<pool_core::local_usage::Dashboard> {
    a.usage_dashboard.snapshot()
}
#[tauri::command]
async fn select_account(a: tauri::State<'_, App>, account_id: Option<String>) -> Result<()> {
    let _work = a.updates.enter_work()?;
    let scheduler = a.gateway.scheduler.clone();
    blocking(move || scheduler.select_account(account_id)).await
}
#[tauri::command]
async fn scheduling_status(a: tauri::State<'_, App>) -> Result<SchedulingStatus> {
    let scheduler = a.gateway.scheduler.clone();
    blocking(move || scheduler.status()).await
}
#[tauri::command]
async fn set_selection_policy(a: tauri::State<'_, App>, policy: SelectionPolicy) -> Result<()> {
    let _work = a.updates.enter_work()?;
    let scheduler = a.gateway.scheduler.clone();
    blocking(move || scheduler.set_policy(policy)).await
}
#[tauri::command]
fn start_pool(app: tauri::AppHandle, account_id: Option<String>) -> Result<()> {
    let a = app.state::<App>();
    let work = a.updates.enter_work()?;
    let close = a.close.lock().unwrap();
    if close.running { return Err("账号池正在关闭，请等待完成".into()); }
    if a.connection.starting() { return Ok(()); }
    let switch = a.lifecycle.clone().try_lock_owned().map_err(|_| "账号池正在切换，请等待完成")?;
    let (connection, scheduler, store) = (a.connection.clone(), a.gateway.scheduler.clone(), a.accounts.store.clone());
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let _work = work;
        let _switch = switch;
        let mut legacy_started = false;
        let result = async {
            handle.state::<App>().api_clients.stop(pool_core::api_sources::ClientKind::Codex).await?;
            connection.begin_start()?;
            legacy_started = true;
            if let Some(id) = account_id { blocking(move || scheduler.select_account(Some(id))).await?; }
            // Check eligibility and compatibility without opening a temporary
            // desktop. Refresh the fingerprint after the potentially long probe.
            connection.prepare_enable().await?;
            connection.set_phase("compatibility");
            let check_store = store.clone();
            let identity = blocking(move || codex_window::prepare_for_enable(&check_store)).await?;
            let fingerprint = connection.prepare_enable().await?;
            connection.set_phase("configuring");
            connection.enable(&fingerprint, identity.client.catalog_version()?).await?;
            connection.wait_for_restart().await?;
            let progress = connection.clone();
            let restart_store = store.clone();
            let target = blocking(move || codex_lifecycle::stop_client(&restart_store, |phase| progress.set_phase(phase))).await?;
            connection.resume_gateway().await?;
            let progress = connection.clone();
            blocking(move || codex_lifecycle::launch(target, &identity, &store, |phase| progress.set_phase(phase))).await?;
            connection.client_reloaded().await
        }.await;
        if legacy_started { if let Err(error) = &result { connection.start_failed(error.clone()).await; } }
        match result {
            Ok(()) => { handle.state::<App>().updates.resume_finished(); let _ = handle.emit("start-complete", ()); }
            Err(error) => {
                if let Some(w) = handle.get_webview_window("main") {
                    let _ = w.show(); let _ = w.emit("main-visible", true); let _ = w.set_focus();
                }
                let _ = handle.emit("start-failed", error);
            }
        }
    });
    Ok(())
}
#[tauri::command]
async fn restore_preview(
    a: tauri::State<'_, App>,
    strategy: Option<config::RecoveryStrategy>,
) -> Result<config::RecoveryPlan> {
    a.connection.recovery_preview(strategy).await
}
#[tauri::command]
async fn image_compatibility(
    a: tauri::State<'_, App>,
) -> Result<pool_core::compatibility::ImageCompatibility> {
    let store = a.accounts.store.clone();
    let running = a.gateway.status().await.running;
    let path = config::codex_config_path()?;
    tauri::async_runtime::spawn_blocking(move || {
        pool_core::compatibility::status(&store, path, running, codex_lifecycle::running_identity(false))
    })
    .await
    .map_err(|_| "无法检查生图兼容状态")?
}
#[tauri::command]
async fn probe(a: tauri::State<'_, App>) -> Result<String> {
    let _work = a.updates.enter_work()?;
    let status = a.gateway.status().await;
    if !status.running {
        return Err("请先启动服务".into());
    }
    let identity = blocking(move || codex_lifecycle::running_identity(true)).await?;
    a.gateway.check_models(identity.client.catalog_version()?, Arc::new(|_| {})).await?;
    Ok("本地服务和上游模型目录已连通。尚未验证真实推理、工具调用与压缩。".into())
}
#[tauri::command]
async fn export_diagnostics(a: tauri::State<'_, App>, app: tauri::AppHandle) -> Result<String> {
    let scale = app.get_webview_window("main").and_then(|w| w.scale_factor().ok());
    let environment = windows_environment::collect(scale);
    let accounts = a.accounts.clone();
    blocking(move || {
        let path = data_dir()?.join(format!("diagnostics-{}.json", pool_core::now()));
        // Deliberately omit account IDs/emails, model inputs, raw headers and credential storage.
        let records: Vec<_> = accounts
            .store
            .logs()?
            .into_iter()
            .map(|mut l| {
                l.account = None;
                l.conversation_key = None;
                l
            })
            .collect();
        let compatibility = pool_core::compatibility::status(
            &accounts.store, config::codex_config_path()?, false, codex_lifecycle::running_identity(false),
        )?;
        let v = serde_json::json!({"version":env!("CARGO_PKG_VERSION"),"logs":records,"environment":environment,"widgetWarning":widget::warning(&app),"widgetInput":widget_input::diagnostics(),
            "clientCompatibility":{"client":compatibility.client,"state":compatibility.verification_state,"detail":compatibility.detail}});
        std::fs::write(&path, serde_json::to_vec_pretty(&v).unwrap())
            .map_err(|_| "导出诊断失败")?;
        Ok(path.display().to_string())
    })
    .await
}
fn main() {
    if std::env::args().any(|a| a == "--verify-update-package") {
        let args: Vec<_> = std::env::args_os().collect();
        let valid = args.len() == 4 && args[1] == "--verify-update-package"
            && updates::verify_release(std::path::Path::new(&args[2]), std::path::Path::new(&args[3])).is_ok();
        std::process::exit(if valid { 0 } else { 1 });
    }
    if std::env::current_exe().ok().and_then(|p| p.file_name().map(|n| n == "Fusion-recovery.exe")).unwrap_or(false) {
        recovery_ui::run();
        return;
    }
    if std::env::args().any(|a| a == "--delete-app-data") {
        std::process::exit(if uninstall_data::run().is_ok() { 0 } else { 1 });
    }
    if std::env::args().any(|a| a == "--check-webview-runtime") {
        std::process::exit(if windows_environment::webview_runtime_ready() { 0 } else { 1 });
    }
    if std::env::args().any(|a| a == "--recover-official") {
        recovery_ui::run();
        return;
    }
    if std::env::args().any(|a| a == "--check-uninstall-safe") {
        let safe = config::codex_config_path().and_then(|p| {
            if !pool_core::api_config::detached(&p)? { return Ok(false); }
            config::detached(
                &p,
                &std::env::current_exe().map_err(|_| "找不到程序路径")?,
                18765,
            )
        });
        std::process::exit(if safe == Ok(true) { 0 } else { 1 });
    }
    if std::env::args().any(|a| a == "--print-client-key") {
        match vault::system_vault()
            .and_then(|v| v.get("gateway-key"))
            .and_then(|v| v.ok_or("请先在账号池中启动服务".into()))
        {
            Ok(k) => println!("{k}"),
            Err(_) => std::process::exit(1),
        }
        return;
    }
    // Database failures must be reported before entering Tauri's event loop.
    let initialized = (|| -> Result<Arc<Accounts>> {
        let store = Arc::new(Store::open(&data_dir()?.join("pool.sqlite"))?);
        Ok(Arc::new(Accounts::new(store, vault::system_vault()?)?))
    })();
    let pool_accounts = match initialized {
        Ok(accounts) => accounts,
        Err(error) => {
            if std::env::args().any(|a| a == "--prepare-uninstall") {
                recovery_ui::run();
            } else {
                startup_failure(&error);
            }
            return;
        }
    };
    let result = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.emit("main-visible", true);
                let _ = w.set_focus();
            }
            if args.iter().any(|s| s == "--prepare-uninstall") {
                request_close(app.clone(), false);
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(move |app| {
            // Tauri executes setup inside its event loop and panics on Err before Builder::run returns.
            // Display the safe recovery message here as well as on early Builder failures.
            let setup_result: std::result::Result<(), Box<dyn std::error::Error>> = (|| {
                let accounts = pool_accounts;
                let bundled = app.path().resource_dir()?.join("network-kernel").join("mihomo.exe");
                #[cfg(debug_assertions)]
                let bundled = if bundled.is_file() { bundled } else {
                    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../resources/network-kernel/mihomo.exe")
                };
                let network_root = data_dir()?.join("network");
                let kernel = network_kernel::NetworkKernel::new(bundled, network_root.join("runtime"))?;
                let network = pool_core::network::Network::new(accounts.store.clone(), accounts.vault.clone(), network_root, kernel)?;
                accounts.http.set_network(network.clone())?;
                let background_network = network.clone();
                tauri::async_runtime::spawn(async move { background_network.start_background(); });
                let gateway = Gateway::new(accounts.clone());
                let connection = Connection::new(
                    gateway.clone(),
                    config::codex_config_path()?,
                    std::env::current_exe()?,
                );
                let api_sources = Arc::new(pool_core::api_sources::Sources::new(accounts.store.clone(), vault::system_vault()?));
                let claude_pool=pool_core::claude_pool::Pool::new(accounts.store.clone(),vault::system_vault()?)?;
                let claude_login=pool_core::claude_pool::login::Login::new(claude_pool.clone());
                let api_clients = api_clients::ApiClients::new(accounts.store.clone(), api_sources.clone(), config::codex_config_path()?,Some(claude_pool.clone()));
                let updates = Arc::new(updates::Updates::new(data_dir()?.join("updates")));
                let dashboard = Arc::new(usage_dashboard::UsageDashboard::default());
                let codex_root = config::codex_config_path()?.parent().ok_or("Codex 数据目录无效")?.to_path_buf();
                dashboard.clone().start(accounts.store.clone(), codex_root, updates.clone());
                app.manage(updates.clone());
                app.manage(App {
                    network,
                    pricing: pricing::Pricing::new(data_dir()?.join("ai-pricing")),
                    api_sources,
                    api_clients,
                    claude_pool,
                    claude_login,
                    api_sources_lock: tokio::sync::RwLock::new(()),
                    handoffs: Arc::new(handoffs::Handoffs::default()),
                    usage_dashboard: dashboard,
                    reports: Arc::new(error_reports::Reports::new(data_dir()?.join("error-reports"))),
                    updates: updates.clone(),
                    oauth: OAuth::new(accounts.clone()),
                    gateway,
                    connection,
                    accounts,
                    lifecycle: Arc::new(tokio::sync::Mutex::new(())),
                    close: Arc::new(std::sync::Mutex::new(CloseIntent::default())),
                });
                app.manage(window_layout::Layout::default());
                if let Some(w) = app.get_webview_window("main") {
                    window_layout::fit(&w)?;
                    w.show()?;
                }
                widget::setup(app)?;
                quota_refresh::start(app.state::<App>().accounts.clone(), updates);
                claude_commands::start_background(app.handle().clone());
                updates::Updates::start(app.handle().clone());
                let recover =
                    MenuItem::with_id(app, "recover", "恢复官方接入…", true, None::<&str>)?;
                let show = MenuItem::with_id(app, "show", "打开 Fusion", true, None::<&str>)?;
                let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
                let menu = Menu::with_items(app, &[&show, &recover, &quit])?;
                let mut tray = TrayIconBuilder::new()
                    .tooltip("Fusion")
                    .menu(&menu)
                    .show_menu_on_left_click(false)
                    .on_menu_event(|app, e| match e.id().as_ref() {
                        "show" => {
                            if let Some(w) = app.get_webview_window("main") {
                                let _ = w.show();
                                let _ = w.emit("main-visible", true);
                                let _ = w.set_focus();
                            }
                        }
                        "recover" => {
                            if let Ok(exe) = std::env::current_exe() {
                                use std::os::windows::process::CommandExt;
                                let _ = std::process::Command::new(exe)
                                    .arg("--recover-official")
                                    .creation_flags(0x08000000)
                                    .spawn();
                            }
                        }
                        "quit" => {
                            if let Some(w) = app.get_webview_window("main") {
                                let _ = w.show();
                                let _ = w.emit("main-visible", true);
                                let _ = w.set_focus();
                            }
                            request_close(app.clone(), true);
                        }
                        _ => {}
                    })
                    .on_tray_icon_event(|tray, e| {
                        if matches!(
                            e,
                            TrayIconEvent::Click {
                                button: MouseButton::Left,
                                button_state: MouseButtonState::Up,
                                ..
                            }
                        ) {
                            if let Some(w) = tray.app_handle().get_webview_window("main") {
                                let _ = w.show();
                                let _ = w.emit("main-visible", true);
                                let _ = w.set_focus();
                            }
                        }
                    });
                if let Some(icon) = app.default_window_icon() {
                    tray = tray.icon(icon.clone());
                }
                tray.build(app)?;
                if std::env::args().any(|s| s == "--prepare-uninstall") {
                    request_close(app.handle().clone(), false);
                }
                Ok(())
            })(
            );
            if let Err(error) = &setup_result {
                startup_failure(&error.to_string());
            }
            setup_result
        })
        .on_window_event(|w, e| {
            widget::event(w, e);
            if w.label() == "main" {
                if matches!(e, tauri::WindowEvent::Moved(_) | tauri::WindowEvent::ScaleFactorChanged { .. }) {
                    if let Some(window) = w.app_handle().get_webview_window("main") {
                        let _ = window_layout::fit(&window);
                    }
                }
                if let tauri::WindowEvent::CloseRequested { api, .. } = e {
                    api.prevent_close();
                    let _ = w.emit("main-visible", false);
                    let _ = w.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            claude_commands::claude_pool_status,claude_commands::claude_pool_login,
            claude_commands::claude_pool_cancel_login,claude_commands::claude_pool_start,
            claude_commands::claude_pool_stop,claude_commands::claude_pool_account,
            network_commands::network_status,
            network_commands::network_import,
            network_commands::network_refresh,
            network_commands::network_remove,
            network_commands::network_save_settings,
            network_commands::network_select,
            network_commands::network_select_subscription,
            network_commands::network_check,network_commands::network_cancel_check,
            network_commands::network_set_enabled,
            network_commands::network_retry,
            ai_pricing,
            open_pricing_website,
            api_commands::api_sources,
            api_commands::save_api_source,
            api_commands::remove_api_source,
            api_commands::check_api_source,
            api_commands::api_client_status,
            api_commands::start_api_connection,
            api_commands::stop_api_connection,
            api_commands::api_request_logs,
            api_commands::open_claude_login,
            error_reports::prepare_error_report,
            error_reports::submit_error_report,
            error_reports::error_report_status,
            error_reports::save_error_report,
            updates::update_status,
            updates::check_update,
            updates::install_update,
            updates::dismiss_update_prompt,
            updates::open_update_repair,
            updates::open_project_repository,
            main_visible,
            widget::widget_ready,
            widget::widget_input_ack,
            handoffs::handoff_chats,
            handoffs::handoff_prepare,
            handoffs::handoff_create,
            handoffs::handoff_send,
            handoffs::handoff_status,
            handoffs::handoff_cancel,
            handoffs::handoff_open,
            widget::widget_snapshot,
            widget::widget_set_expanded,
            widget::widget_drag,
            widget::widget_open_console,
            accounts,
            settings,
            save_settings,
            login,
            login_status,
            cancel_login,
            uninstall_requested,
            pause_account,
            remove_account,
            refresh_quota,
            reset_credits,
            consume_reset,
            service_status,
            cancel_request,
            connection_status,
            close_pool,
            logs,
            request_history,
            conversation_usage,
            usage,
            usage_dashboard,
            image_compatibility,
            select_account,
            scheduling_status,
            set_selection_policy,
            start_pool,
            restore_preview,
            probe,
            export_diagnostics
        ])
        .run(tauri::generate_context!());
    if let Err(e) = result {
        startup_failure(&e.to_string());
    }
}

fn startup_failure(error: &str) {
    let reason = [
        "数据来自较新版本，请使用新版程序",
        "无法打开账号池数据库",
        "无法创建数据目录",
        "初始化数据库失败",
        "网络客户端初始化失败",
    ]
    .into_iter()
    .find(|s| error.contains(s))
    .unwrap_or("桌面初始化失败，请检查数据目录权限或重新安装程序");
    let location = data_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "%LOCALAPPDATA%/PersonalCodexPool".into());
    let message = format!("Fusion 启动失败：{reason}\n\n数据位置：{location}\n请保留原数据。关闭其他实例后重试；若为较新数据版本，请安装新版 Fusion。重新安装不会自动清空数据库。\n需要恢复 Codex 时，可运行 Fusion 的 --recover-official 独立恢复入口；该入口不依赖账号数据库或凭据初始化。");
    // Keep the same safe explanation available if Windows cannot display a dialog in this session.
    if let Ok(directory) = data_dir() {
        let _ = std::fs::write(directory.join("startup-error.txt"), &message);
    }
    #[cfg(windows)]
    {
        #[link(name = "user32")]
        extern "system" {
            fn MessageBoxW(
                window: *mut std::ffi::c_void,
                text: *const u16,
                title: *const u16,
                kind: u32,
            ) -> i32;
        }
        let text: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
        let title: Vec<u16> = "Fusion 启动失败".encode_utf16().chain(Some(0)).collect();
        unsafe {
            MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), 0x10);
        }
    }
    #[cfg(not(windows))]
    eprintln!("{message}");
}
