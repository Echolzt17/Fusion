use tauri::{
    ipc::{CallbackFn, InvokeBody},
    test::{get_ipc_response, mock_builder, INVOKE_KEY},
    webview::InvokeRequest,
};
#[tauri::command]
fn login() -> &'static str {
    "fixture"
}
#[tauri::command]
fn remove_account() -> &'static str {
    "fixture"
}
#[tauri::command]
fn consume_reset() -> &'static str {
    "fixture"
}
#[tauri::command]
fn start_pool() -> &'static str {
    "fixture"
}
#[tauri::command]
fn close_pool() -> &'static str {
    "fixture"
}
#[tauri::command]
fn cancel_request() -> &'static str { "fixture" }
#[tauri::command]
fn request_history() -> &'static str { "fixture" }
#[tauri::command]
fn widget_snapshot() -> &'static str {
    "fixture"
}
#[tauri::command]
fn update_status() -> &'static str { "fixture" }
#[tauri::command]
fn check_update() -> &'static str { "fixture" }
#[tauri::command]
fn install_update() -> &'static str { "fixture" }
#[tauri::command]
fn dismiss_update_prompt() -> &'static str { "fixture" }
#[tauri::command]
fn open_update_repair() -> &'static str { "fixture" }
#[tauri::command]
fn open_project_repository() -> &'static str { "fixture" }
#[tauri::command]
fn prepare_error_report() -> &'static str { "fixture" }
#[tauri::command]
fn submit_error_report() -> &'static str { "fixture" }
#[tauri::command]
fn error_report_status() -> &'static str { "fixture" }
#[tauri::command]
fn save_error_report() -> &'static str { "fixture" }
#[tauri::command]
fn usage_dashboard() -> &'static str { "fixture" }
macro_rules! api_fixture_commands {
    ($($name:ident),+ $(,)?) => { $(#[tauri::command] fn $name() -> &'static str { "fixture" })+ };
}
api_fixture_commands!(api_sources, save_api_source, remove_api_source, check_api_source,
    api_client_status, start_api_connection, stop_api_connection, api_request_logs, open_claude_login,
    ai_pricing, open_pricing_website);
api_fixture_commands!(claude_pool_status,claude_pool_login,claude_pool_cancel_login,claude_pool_start,claude_pool_stop,claude_pool_account);
api_fixture_commands!(local_projects_status, restore_local_projects, open_local_chat);
api_fixture_commands!(network_status, network_import, network_refresh, network_remove,
    network_save_settings, network_select, network_select_subscription, network_check,network_cancel_check, network_set_enabled, network_retry);
#[tauri::command]
fn handoff_chats() -> &'static str { "fixture" }
#[tauri::command]
fn handoff_prepare() -> &'static str { "fixture" }
#[tauri::command]
fn handoff_create() -> &'static str { "fixture" }
#[tauri::command]
fn handoff_send() -> &'static str { "fixture" }
#[tauri::command]
fn handoff_status() -> &'static str { "fixture" }
#[tauri::command]
fn handoff_cancel() -> &'static str { "fixture" }
#[tauri::command]
fn handoff_open() -> &'static str { "fixture" }
#[test]
fn widget_cannot_invoke_account_configuration_or_exit_commands() {
    let app = mock_builder()
        .invoke_handler(tauri::generate_handler![
            claude_pool_status,claude_pool_login,claude_pool_cancel_login,claude_pool_start,claude_pool_stop,claude_pool_account,
            network_status, network_import, network_refresh, network_remove,
            network_save_settings, network_select, network_select_subscription, network_check,network_cancel_check, network_set_enabled, network_retry,
            ai_pricing, open_pricing_website,
            api_sources, save_api_source, remove_api_source, check_api_source,
            api_client_status, start_api_connection, stop_api_connection, api_request_logs, open_claude_login,
            prepare_error_report, submit_error_report, error_report_status, save_error_report,
            handoff_chats,handoff_prepare,handoff_create,handoff_send,handoff_status,handoff_cancel,handoff_open,
            usage_dashboard,
            local_projects_status, restore_local_projects, open_local_chat,
            update_status, check_update, install_update, dismiss_update_prompt, open_update_repair, open_project_repository,
            login,
            remove_account,
            consume_reset,
            start_pool,
            close_pool,
            cancel_request,
            widget_snapshot,
            request_history
        ])
        .build(tauri::generate_context!())
        .unwrap();
    let main = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();
    let widget = tauri::WebviewWindowBuilder::new(&app, "pool-widget", Default::default())
        .build()
        .unwrap();
    let request = |name: &str, remote: bool| InvokeRequest {
        cmd: name.into(),
        callback: CallbackFn(0),
        error: CallbackFn(1),
        url: if remote {
            "https://example.invalid"
        } else {
            "http://tauri.localhost"
        }
        .parse()
        .unwrap(),
        body: InvokeBody::default(),
        headers: Default::default(),
        invoke_key: INVOKE_KEY.into(),
    };
    for name in [
        "claude_pool_status","claude_pool_login","claude_pool_cancel_login","claude_pool_start","claude_pool_stop","claude_pool_account",
        "network_status", "network_import", "network_refresh", "network_remove",
        "network_save_settings", "network_select", "network_select_subscription", "network_check","network_cancel_check", "network_set_enabled", "network_retry",
        "ai_pricing", "open_pricing_website",
        "api_sources", "save_api_source", "remove_api_source", "check_api_source",
        "api_client_status", "start_api_connection", "stop_api_connection", "api_request_logs", "open_claude_login",
        "prepare_error_report", "submit_error_report", "error_report_status", "save_error_report",
        "handoff_chats","handoff_prepare","handoff_create","handoff_send","handoff_status","handoff_cancel","handoff_open",
        "usage_dashboard",
        "local_projects_status", "restore_local_projects", "open_local_chat",
        "update_status", "check_update", "install_update", "dismiss_update_prompt", "open_update_repair", "open_project_repository",
        "login",
        "remove_account",
        "consume_reset",
        "start_pool",
        "close_pool",
        "cancel_request",
    ] {
        assert!(
            get_ipc_response(&widget, request(name, false)).is_err(),
            "widget unexpectedly allowed {name}"
        );
        assert!(
            get_ipc_response(&main, request(name, false)).is_ok(),
            "main unexpectedly denied {name}"
        );
        assert!(
            get_ipc_response(&main, request(name, true)).is_err(),
            "remote unexpectedly allowed {name}"
        );
    }
    assert!(get_ipc_response(&widget, request("widget_snapshot", false)).is_ok());
    assert!(get_ipc_response(&main, request("widget_snapshot", false)).is_err());
    assert!(get_ipc_response(&widget, request("request_history", false)).is_err());
    assert!(get_ipc_response(&main, request("request_history", false)).is_ok());
    assert!(get_ipc_response(&widget, request("request_history", true)).is_err());
}
