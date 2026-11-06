use crate::codex_window::{self, Host};
use crate::{App, Result};
use serde::{Deserialize, Serialize};
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};
use tauri::{Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindowBuilder};

const LABEL: &str = "pool-widget";
const CAPSULE_WIDTH: f64 = 340.;
const CAPSULE_HEIGHT: f64 = 40.;
const PANEL_WIDTH: f64 = 416.;
const PANEL_HEIGHT: f64 = 520.;
#[derive(Clone, Copy, Serialize, Deserialize)]
struct Anchor {
    x: i32,
    y: i32,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Control {
    instance: String,
    revision: u64,
    expanded: bool,
    ready: bool,
}
struct State {
    instance: String,
    generation: u64,
    ready: bool,
    expanded: bool,
    transition: u64,
    warning: Option<String>,
    anchor: Option<Anchor>,
    shown: bool,
    host: Option<Host>,
    area: Option<(u32, u32, f64)>,
    dragging: Option<((i32, i32), Anchor)>,
    focus: Option<codex_window::FocusTarget>,
    hovered: Option<bool>,
}
pub struct Widget(Mutex<State>);
pub fn setup(app: &tauri::App) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let anchor = app
        .state::<App>()
        .accounts
        .store
        .get("widget_position_codex")?
        .and_then(|s| serde_json::from_str(&s).ok());
    app.manage(Widget(Mutex::new(State {
        instance: uuid::Uuid::new_v4().to_string(),
        generation: 0,
        ready: false,
        expanded: false,
        transition: 0,
        warning: None,
        anchor,
        shown: false,
        host: None,
        area: None,
        dragging: None,
        focus: None,
        hovered: None,
    })));
    let handle = app.handle().clone();
    tauri::async_runtime::spawn(async move {
        let mut checked = Instant::now() - Duration::from_secs(1);
        let mut on = false;
        loop {
            let interactive = handle
                .state::<Widget>()
                .0
                .lock()
                .is_ok_and(|s| s.dragging.is_some() || s.expanded);
            tokio::time::sleep(Duration::from_millis(if interactive { 16 } else { 150 })).await;
            let a = handle.state::<App>();
            let mut refresh_layer = false;
            if checked.elapsed() >= Duration::from_millis(750) {
                on = a.connection.status().await.is_ok_and(|s| s.mode == "on");
                checked = Instant::now();
                refresh_layer = true;
            }
            let state = handle.state::<Widget>();
            let host = {
                let Ok(s) = state.0.lock() else {
                    continue;
                };
                s.host.filter(|h| h.alive())
            }
            .or_else(|| on.then(codex_window::find).flatten());
            if let Some(w) = handle.get_webview_window(LABEL) {
                if !host.is_some_and(|h| {
                    w.hwnd()
                        .is_ok_and(|hwnd| codex_window::is_child_of(hwnd, h))
                }) {
                    if w.destroy().is_err() {
                        set_warning(&handle, Some("无法清理失效的 Codex 挂件，请处理窗口状态后重试".into()));
                    }
                    continue;
                }
            }
            if !on { set_warning(&handle, None); }
            let focus = {
                let child = handle.get_webview_window(LABEL).and_then(|w| w.hwnd().ok());
                host.and_then(|host| codex_window::host_focus(host, child))
            };
            if let Some(focus) = focus {
                if let Ok(mut s) = state.0.lock() { s.focus = Some(focus); }
            }
            if interactive && host.is_some_and(|h| !codex_window::host_foreground(h) || codex_window::escape_pressed()) {
                schedule_collapse(&handle);
            }
            let mut created = false;
            if let Some(host) = host.filter(|_| on && handle.get_webview_window(LABEL).is_none()) {
                // A real WS_CHILD window: Windows clips it to Codex and destroys it
                // with its parent. No globally topmost desktop fallback exists.
                if let Err(error) = create_child(&handle, host).await {
                    set_warning(&handle, Some(error));
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
                created = true;
            }
            let area = host.and_then(|h| h.area());
            let (changed, open, save) = {
                let Ok(mut s) = state.0.lock() else {
                    continue;
                };
                let shown = on && area.is_some() && s.ready;
                let mut changed = created || s.shown != shown || s.host != host || s.area != area;
                s.shown = shown;
                s.host = host;
                if let (Some(previous), Some(current), Some(anchor)) = (s.area, area, s.anchor) {
                    if previous.2 != current.2 {
                        s.anchor = Some(rescale_anchor(anchor, previous.2, current.2));
                        s.dragging = None;
                    }
                }
                s.area = area;
                let mut save = None;
                if let Some((start, anchor)) = s.dragging {
                    if codex_window::left_pressed() && shown {
                        if let Some(cursor) = host.and_then(|h| h.cursor()) {
                            s.anchor = Some(Anchor {
                                x: anchor.x + cursor.0 - start.0,
                                y: anchor.y + cursor.1 - start.1,
                            });
                            changed = true;
                        }
                    } else {
                        s.dragging = None;
                        save = s.anchor;
                    }
                }
                if !shown && s.expanded {
                    s.transition = s.transition.saturating_add(1);
                    s.expanded = false;
                }
                (changed, s.expanded, save)
            };
            if let Some(p) = save {
                let _ = a.accounts.store.set(
                    "widget_position_codex",
                    &serde_json::to_string(&p).unwrap_or_default(),
                );
            }
            if changed {
                if let Some(w) = handle.get_webview_window(LABEL) {
                    let result = if on && area.is_some() { layout(&handle, open).and_then(|_| {
                            w.show().map_err(|_| "无法显示挂件".to_owned())?;
                            if let Some(host) = host { codex_window::raise_child(w.hwnd().map_err(|_| "挂件窗口不可用")?, host)?; }
                        Ok(())
                    }) } else { let _=w.emit("widget-expanded", control(&handle).ok()); w.hide().map_err(|_| "无法隐藏挂件".to_owned()) };
                    match result {
                        Ok(()) => set_warning(&handle, None),
                        Err(error) => { if let Ok(mut s) = state.0.lock() { s.shown = false; } set_warning(&handle, Some(error)); }
                    }
                }
            }
            if refresh_layer && on && area.is_some() {
                if state.0.lock().is_ok_and(|s| s.ready) && crate::widget_input::stalled() {
                    set_warning(&handle, Some("挂件交互回执中断，正在重建交互窗口；未补发点击".into()));
                    if let Some(w) = handle.get_webview_window(LABEL) { let _ = w.destroy(); }
                    continue;
                }
                if let Some(w) = handle.get_webview_window(LABEL) {
                    let window = w.clone();
                    let ready = state.0.lock().is_ok_and(|s| s.ready);
                    if let Err(error) = on_ui_thread(handle.clone(), move |_| crate::widget_input::maintain(window.hwnd().map_err(|_| "挂件窗口不可用")?, ready)).await {
                        set_warning(&handle, Some(error));
                        let _ = w.destroy();
                        continue;
                    }
                }
                if let (Some(w),Some(host))=(handle.get_webview_window(LABEL),host) {
                    if let Ok(hwnd) = w.hwnd() {
                        if let Err(error) = codex_window::raise_child(hwnd, host) { set_warning(&handle, Some(error)); }
                    }
                }
            }
            if let Some(w) = handle.get_webview_window(LABEL) {
                let hovered = on && area.is_some_and(|(_, _, scale)| {
                    w.hwnd().is_ok_and(|hwnd| codex_window::capsule_hovered(hwnd, scale))
                });
                let changed = state.0.lock().is_ok_and(|mut s| {
                    let changed = s.hovered != Some(hovered);
                    s.hovered = Some(hovered);
                    changed
                });
                if changed { let _ = w.emit("widget-hovered", hovered); }
            }
        }
    });
    Ok(())
}
pub fn warning(app: &tauri::AppHandle) -> Option<String> {
    app.try_state::<Widget>().and_then(|state| state.0.lock().ok().and_then(|s| s.warning.clone()))
}
fn set_warning(app: &tauri::AppHandle, error: Option<String>) {
    let state = app.state::<Widget>();
    let Ok(mut s) = state.0.lock() else { return; };
    if s.warning == error { return; }
    s.warning = error.clone();
    drop(s);
    let _ = app.emit_to("main", "desktop-warning", error);
}
async fn create_child(app: &tauri::AppHandle, host: Host) -> Result<()> {
    let generation = {
        let state = app.state::<Widget>();
        let mut s = state.0.lock().map_err(|_| "挂件状态异常")?;
        s.generation = s.generation.saturating_add(1);
        s.instance = uuid::Uuid::new_v4().to_string();
        s.ready = false;
        s.generation
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let _ = tx.send(codex_window::thread_dpi_matches(host));
    }).map_err(|_| "无法核对挂件创建线程的 DPI 模式".to_owned())?;
    if !rx.await.unwrap_or(false) {
        return Err("Codex 与 Fusion 的 DPI 模式不一致或宿主已变化，未跨模式挂载挂件".into());
    }
    // Tauri 2.12.5 explicitly forbids synchronous WebView creation in Windows event handlers.
    // This call stays on the background task; only the native DPI preflight runs on the UI thread.
    let w = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("index.html?view=widget".into()))
        .title("Fusion 账号池挂件").parent_raw(host.hwnd()).inner_size(CAPSULE_WIDTH, CAPSULE_HEIGHT)
        .decorations(false).transparent(true).resizable(false).skip_taskbar(true)
        .focused(false).focusable(false).visible(false)
        .on_page_load(move |w, e| {
            if matches!(e.event(), tauri::webview::PageLoadEvent::Started) {
                if let Ok(mut s) = w.state::<Widget>().0.lock() {
                    if s.generation == generation {
                        s.ready = false;
                        s.instance = uuid::Uuid::new_v4().to_string();
                        s.transition = s.transition.saturating_add(1);
                        s.expanded = false;
                        s.dragging = None;
                    }
                }
                if let Ok(hwnd) = w.hwnd() { let _ = crate::widget_input::maintain(hwnd, false); }
            }
        }).build()
        .map_err(|_| "无法创建 Codex 内挂件，请检查 WebView2 与窗口权限".to_owned())?;
    if !w.hwnd().is_ok_and(|hwnd| codex_window::child_dpi_matches(hwnd, host)) {
        let _ = w.destroy();
        return Err("挂件与 Codex 的父窗口或 DPI 模式不一致，已隐藏挂件".into());
    }
    let handle = app.clone();
    let input_window = w.clone();
    let action_window = w.clone();
    if let Err(error) = on_ui_thread(app.clone(), move |_| {
        crate::widget_input::install(input_window.hwnd().map_err(|_| "挂件窗口不可用")?, host,
            move |inside| {
                if !handle.state::<Widget>().0.lock().is_ok_and(|s| s.generation == generation) { return; }
                if inside {
                    let child = handle.get_webview_window(LABEL).and_then(|w| w.hwnd().ok());
                    if let Some(focus) = codex_window::host_focus(host, child) {
                        if let Ok(mut s) = handle.state::<Widget>().0.lock() { s.focus = Some(focus); }
                    }
                } else { schedule_collapse(&handle); }
            }, move |action| {
                let app = action_window.app_handle();
                let current = app.state::<Widget>().0.lock().ok().and_then(|s| (s.generation == generation && s.ready).then(|| s.instance.clone()));
                current.is_some_and(|instance| action_window.emit("widget-pointer", serde_json::json!({"instance":instance,"sequence":action.sequence,"gesture":action.gesture,"kind":action.kind,"x":action.x,"y":action.y,"delta":action.delta})).is_ok())
            })
    }).await {
        let _ = w.destroy();
        return Err(error);
    }
    let input = w.clone();
    on_ui_thread(app.clone(), move |_| crate::widget_input::maintain(input.hwnd().map_err(|_| "挂件窗口不可用")?, false)).await?;
    Ok(())
}
fn rescale_anchor(anchor: Anchor, previous: f64, current: f64) -> Anchor {
    Anchor { x: (anchor.x as f64 * current / previous).round() as i32,
             y: (anchor.y as f64 * current / previous).round() as i32 }
}
fn geometry(
    anchor: Anchor,
    area: (i32, i32, u32, u32),
    scale: f64,
    open: bool,
) -> (Anchor, u32, u32, i32, i32) {
    let (left, top, aw, ah) = area;
    let cw = (CAPSULE_WIDTH * scale).round() as u32;
    let ch = (CAPSULE_HEIGHT * scale).round() as u32;
    let anchor = Anchor {
        x: anchor.x.clamp(left, left + (aw.saturating_sub(cw)) as i32),
        y: anchor.y.clamp(top, top + (ah.saturating_sub(ch)) as i32),
    };
    let width = if open {
        (PANEL_WIDTH * scale).round() as u32
    } else {
        cw
    };
    let height = if open {
        (PANEL_HEIGHT * scale).round() as u32
    } else {
        ch
    };
    let (width, height) = (width.min(aw), height.min(ah));
    let x = (anchor.x + cw as i32 - width as i32).clamp(left, left + (aw - width) as i32);
    let y = (anchor.y + ch as i32 - height as i32).clamp(top, top + (ah - height) as i32);
    (anchor, width, height, x, y)
}
fn layout(app: &tauri::AppHandle, open: bool) -> Result<()> {
    let w = app.get_webview_window(LABEL).ok_or("挂件窗口尚未创建")?;
    let state = app.state::<Widget>();
    let (anchor, shown, area) = {
        let mut s = state.0.lock().map_err(|_| "挂件状态异常")?;
        let area = s.host.and_then(|h| h.area());
        if let (Some(previous), Some(current), Some(anchor)) = (s.area, area, s.anchor) {
            if previous.2 != current.2 {
                s.anchor = Some(rescale_anchor(anchor, previous.2, current.2));
                s.dragging = None;
            }
        }
        s.area = area;
        (s.anchor, s.shown, area)
    };
    let (aw, ah, scale) = area.ok_or("Codex 窗口当前不可见")?;
    let anchor = anchor.unwrap_or(Anchor {
        x: aw as i32 - ((CAPSULE_WIDTH + 16.) * scale) as i32,
        y: ah as i32 - ((CAPSULE_HEIGHT + 16.) * scale) as i32,
    });
    let (anchor, width, height, x, y) = geometry(anchor, (0, 0, aw, ah), scale, open && shown);
    {
        let mut s = state.0.lock().map_err(|_| "挂件状态异常")?;
        s.expanded = open && shown;
        s.anchor = Some(anchor);
    }
    w.set_size(PhysicalSize::new(width, height))
        .map_err(|_| "无法调整挂件大小")?;
    w.set_position(PhysicalPosition::new(x, y))
        .map_err(|_| "无法调整挂件位置")?;
    // Layout must never activate a cross-process child. Tauri's set_focus
    // fallback simulates Alt when SetForegroundWindow rejects the child HWND.
    let _ = w.emit("widget-expanded", control(app)?);
    Ok(())
}
// Keep the native child large enough for the CSS exit animation. A later
// toggle invalidates the delayed resize; dragging/host geometry remains immediate.
pub fn expanded(app: &tauri::AppHandle, open: bool) -> Result<()> {
    let w = app.get_webview_window(LABEL).ok_or("挂件窗口尚未创建")?;
    let (generation, revision) = {
        let state = app.state::<Widget>();
        let mut s = state.0.lock().map_err(|_| "挂件状态异常")?;
        s.transition = s.transition.saturating_add(1);
        s.expanded = open && s.shown && s.ready;
        (s.generation, s.transition)
    };
    if open {
        if let Err(error) = layout(app, true) {
            if let Ok(mut s) = app.state::<Widget>().0.lock() { s.expanded = false; s.transition = s.transition.saturating_add(1); }
            let _ = w.emit("widget-expanded", control(app)?);
            return Err(error);
        }
    }
    else { let _ = w.emit("widget-expanded", control(app)?); }
    return_input(app);
    if open { return Ok(()); }
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(210)).await;
        let ui_handle = handle.clone();
        let _ = handle.run_on_main_thread(move || {
            let current = ui_handle.state::<Widget>().0.lock()
                .is_ok_and(|s| s.generation == generation && s.transition == revision && !s.expanded);
            if current {
                if let Err(error) = layout(&ui_handle, false) { set_warning(&ui_handle, Some(error)); }
            }
        });
    });
    Ok(())
}
fn return_input(app: &tauri::AppHandle) {
    let Some(w) = app.get_webview_window(LABEL) else { return; };
    let (host, focus) = {
        let state = app.state::<Widget>();
        let Ok(s) = state.0.lock() else { return; };
        (s.host, s.focus)
    };
    if let (Some(host), Ok(hwnd)) = (host, w.hwnd()) {
        codex_window::return_host_focus(hwnd, host, focus);
    }
}
fn schedule_collapse(app: &tauri::AppHandle) {
    let revision = app.state::<Widget>().0.lock().ok()
        .and_then(|s| s.expanded.then_some((s.generation, s.transition)));
    let Some((generation, revision)) = revision else { return; };
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = on_ui_thread(handle, move |app| {
            let current = app.state::<Widget>().0.lock()
                .is_ok_and(|s| s.generation == generation && s.expanded && s.transition == revision);
            if current {
                expanded(app, false)?;
            }
            Ok(())
        }).await;
    });
}
pub fn event(w: &tauri::Window, e: &tauri::WindowEvent) {
    if w.label() != LABEL {
        return;
    }
    match e {
        tauri::WindowEvent::Focused(true) => {
            // WebView2 can explicitly focus a descendant despite NOACTIVATE.
            // Return it after the COM callback, including while details stay open.
            let handle = w.app_handle().clone();
            tauri::async_runtime::spawn(async move {
                let _ = on_ui_thread(handle, |app| { return_input(app); Ok(()) }).await;
            });
        }
        tauri::WindowEvent::CloseRequested { api, .. } => {
            api.prevent_close();
            let handle = w.app_handle().clone();
            tauri::async_runtime::spawn(async move { let _ = on_ui_thread(handle, |app| expanded(app, false)).await; });
        }
        tauri::WindowEvent::Destroyed => {
            if w.app_handle().get_webview_window(LABEL).is_some_and(|current| current.hwnd().ok() != w.hwnd().ok()) { return; }
            if let Some(state) = w.app_handle().try_state::<Widget>() {
                if let Ok(mut s) = state.0.lock() {
                    s.ready = false;
                    s.host = None;
                    s.area = None;
                    s.shown = false;
                    s.expanded = false;
                    s.dragging = None;
                    s.focus = None;
                    s.hovered = None;
                }
            }
        }
        tauri::WindowEvent::ScaleFactorChanged { .. } => {
            let open = w
                .app_handle()
                .state::<Widget>()
                .0
                .lock()
                .is_ok_and(|s| s.expanded);
            let handle = w.app_handle().clone();
            tauri::async_runtime::spawn(async move { let _ = on_ui_thread(handle, move |app| expanded(app, open)).await; });
        }
        _ => {}
    }
}
#[tauri::command]
pub async fn widget_set_expanded(app: tauri::AppHandle, expanded: bool, instance: String) -> Result<Control> {
    on_ui_thread(app.clone(), move |app| { require_instance(app, &instance)?; self::expanded(app, expanded) }).await?;
    control(&app)
}
fn control(app: &tauri::AppHandle) -> Result<Control> {
    let state = app.state::<Widget>();
    let s = state.0.lock().map_err(|_| "挂件状态异常")?;
    Ok(Control { instance: s.instance.clone(), revision: s.transition, expanded: s.expanded, ready: s.ready })
}
fn require_instance(app: &tauri::AppHandle, instance: &str) -> Result<()> {
    let state = control(app)?;
    if !state.ready || state.instance != instance { return Err("挂件已重建，请等待重新连接".into()); }
    Ok(())
}
#[tauri::command]
pub async fn widget_ready(app: tauri::AppHandle) -> Result<Control> {
    on_ui_thread(app.clone(), |app| {
        let w = app.get_webview_window(LABEL).ok_or("挂件窗口尚未创建")?;
        crate::widget_input::maintain(w.hwnd().map_err(|_| "挂件窗口不可用")?, true)?;
        let state = app.state::<Widget>();
        let mut s = state.0.lock().map_err(|_| "挂件状态异常")?;
        s.ready = true;
        Ok(())
    }).await?;
    control(&app)
}
#[tauri::command]
pub fn widget_input_ack(app: tauri::AppHandle, instance: String, sequence: u64, completed: bool) -> Result<()> {
    require_instance(&app, &instance)?;
    crate::widget_input::acknowledge(sequence, completed);
    Ok(())
}
async fn on_ui_thread(app: tauri::AppHandle, action: impl FnOnce(&tauri::AppHandle) -> Result<()> + Send + 'static) -> Result<()> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let handle = app.clone();
    app.run_on_main_thread(move || { let _ = tx.send(action(&handle)); })
        .map_err(|_| "无法更新挂件窗口")?;
    rx.await.map_err(|_| "挂件窗口任务中断")?
}
#[tauri::command]
pub async fn widget_drag(app: tauri::AppHandle, instance: String) -> Result<()> {
    on_ui_thread(app, move |app| { require_instance(app, &instance)?; start_drag(app) }).await
}
fn start_drag(app: &tauri::AppHandle) -> Result<()> {
    let _ = expanded(app, false);
    layout(app, false)?;
    let state = app.state::<Widget>();
    let mut s = state.0.lock().map_err(|_| "挂件状态异常")?;
    let cursor = s.host.and_then(|h| h.cursor()).ok_or("Codex 窗口不可用")?;
    s.dragging = Some((cursor, s.anchor.ok_or("挂件位置不可用")?));
    Ok(())
}
#[tauri::command]
pub async fn widget_open_console(app: tauri::AppHandle, instance: String) -> Result<()> {
    on_ui_thread(app, move |app| { require_instance(app, &instance)?; open_console(app) }).await
}
fn open_console(app: &tauri::AppHandle) -> Result<()> {
    let _ = expanded(app, false);
    let w = app.get_webview_window("main").ok_or("账号控制台尚未创建")?;
    w.show().map_err(|_| "无法打开账号控制台")?;
    let _ = w.emit("main-visible", true);
    w.set_focus().map_err(|_| "无法聚焦账号控制台")?;
    app.emit_to("main", "widget-open-console", ())
        .map_err(|_| "无法打开账号控制台".into())
}
fn quota_remaining(account: &pool_core::Account, now: i64) -> Option<f64> {
    if account.quota_error.is_some() { return None; }
    let quota = account.quota.as_ref()?;
    let age = now.checked_sub(quota["updatedAt"].as_i64()?)?;
    if !(0..=pool_core::account::QUOTA_REFRESH_INTERVAL_SECS).contains(&age) { return None; }
    if ["primary_window", "secondary_window"].iter().any(|key| {
        quota["rateLimit"][key]["reset_at"].as_i64().is_some_and(|reset| reset <= now)
    }) { return None; }
    ["primary_window", "secondary_window"].iter()
        .filter_map(|key| quota["rateLimit"][key]["used_percent"].as_f64())
        .filter(|used| used.is_finite())
        .map(|used| (100. - used).clamp(0., 100.)).reduce(f64::min)
}
#[tauri::command]
pub async fn widget_snapshot(a: tauri::State<'_, App>, app: tauri::AppHandle, details: bool) -> Result<serde_json::Value> {
    if a.connection.status().await?.mode != "on" {
        return Ok(serde_json::json!({"on":false}));
    }
    let active = if details { a.gateway.status().await.requests } else { Vec::new() };
    let control = control(&app)?;
    let accounts_service = a.accounts.clone();
    let scheduler = a.gateway.scheduler.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let store = &accounts_service.store;
        let accounts = store.accounts()?;
        let scheduling = scheduler.status()?;
        let recent = store.recent_request();
        // An explicitly unknown recent conversation must never fall back to
        // a different identified conversation.
        let conversation = match &recent {
            Some(request) => request.conversation_key.clone(),
            None => store.conversation(None)?.map(|c| c.conversation_key),
        };
        let current = match &recent {
            Some(request) => request.account.as_ref(),
            None => scheduling.current_account_id.as_ref().or(scheduling.preferred_account_id.as_ref()),
        };
        let chosen = accounts.iter().find(|account| Some(&account.id) == current);
        let now = pool_core::now();
        let remaining = chosen.and_then(|account| quota_remaining(account, now));
        let quotas: Vec<f64> = accounts.iter().filter_map(|account| quota_remaining(account, now)).collect();
        let (all, current_summary) = store.request_summaries(conversation.as_deref())?;
        let mut out = serde_json::json!({"control":control,"on":true,"remaining":remaining,"conversationKey":conversation,
            "totalRemaining":if quotas.is_empty() { None } else { Some(quotas.iter().sum::<f64>()) },
            "quotaAccounts":quotas.len(),"totalAccounts":accounts.len(),
            "tokens":if all.measured_requests > 0 { Some(all.total_tokens) } else { None },
            "usagePartial":all.measured_requests < all.requests || all.history_partial,
            "currentAccountLabel":chosen.map(|c|format!("账号 {}",accounts.iter().position(|a|a.id==c.id).unwrap_or(0)+1)),
            "storageWarning":store.persistence_warning()});
        if details {
            let business = |operation: &str| matches!(operation, "responses" | "compact" | "image_generate" | "image_edit");
            let active_all = active.iter().filter(|r| business(r.operation)).count();
            let active_current = active.iter().filter(|r| business(r.operation) && conversation.is_some() && r.conversation_key == conversation).count();
            out["details"] = serde_json::json!({"conversationKey":conversation,"current":current_summary,"all":all,
                "activeCurrent":active_current,"activeAll":active_all});
        }
        Ok(out)
    }).await.map_err(|_| "挂件数据任务异常")?
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quota_uses_the_limiting_window_and_rejects_unknown_or_expired_snapshots() {
        let mut account = pool_core::Account {
            id: "fixture".into(), email: "fixture@example.test".into(), account_id: "fixture".into(),
            state: "ready".into(), cooldown_until: 0, quota_error: None,
            quota: Some(serde_json::json!({"updatedAt":1000,"rateLimit":{
                "primary_window":{"used_percent":17,"reset_at":2000},
                "secondary_window":{"used_percent":40,"reset_at":3000}}})),
        };
        assert_eq!(quota_remaining(&account, 1100), Some(60.));
        assert_eq!(quota_remaining(&account, 1600), Some(60.));
        assert_eq!(quota_remaining(&account, 1601), None);
        assert_eq!(quota_remaining(&account, 999), None);
        account.quota_error = Some("fixture failure".into());
        assert_eq!(quota_remaining(&account, 1100), None);
        account.quota_error = None;
        account.quota.as_mut().unwrap()["rateLimit"]["primary_window"]["reset_at"] = serde_json::json!(1100);
        assert_eq!(quota_remaining(&account, 1100), None);
        account.quota = None;
        assert_eq!(quota_remaining(&account, 1100), None);
    }
    #[test]
    fn widget_dimensions_keep_the_capsule_anchor_when_expanding() {
        let anchor = Anchor { x: 600, y: 700 };
        let (a, w, h, x, y) = geometry(anchor, (0, 0, 1200, 900), 1., false);
        assert_eq!((w, h, x, y), (340, 40, 600, 700));
        let (_, w, h, x, y) = geometry(a, (0, 0, 1200, 900), 1., true);
        assert_eq!((w, h, x, y), (416, 520, 524, 220));
        assert_eq!((x + w as i32, y + h as i32), (940, 740));
    }
    #[test]
    fn anchor_preserves_logical_position_when_moving_between_dpi_scales() {
        let anchor = Anchor { x: 200, y: 100 };
        for scale in [1., 1.25, 1.5, 1.75, 2.] {
            let next = rescale_anchor(anchor, 1., scale);
            let back = rescale_anchor(next, scale, 1.);
            assert_eq!((back.x, back.y), (200, 100));
            let (_, w, h, x, y) = geometry(next, (0, 0, 640, 360), scale, true);
            assert!(x >= 0 && y >= 0 && x + w as i32 <= 640 && y + h as i32 <= 360);
        }
    }
    #[test]
    fn positions_clamp_across_negative_monitors_dpi_and_small_work_areas() {
        for scale in [1., 1.25, 1.5, 2.] {
            for open in [false, true] {
                let (a, w, h, x, y) = geometry(
                    Anchor { x: 5000, y: -5000 },
                    (-1920, 0, 1920, 1040),
                    scale,
                    open,
                );
                assert!(
                    a.x < 0
                        && a.y >= 0
                        && x >= -1920
                        && y >= 0
                        && x + w as i32 <= 0
                        && y + h as i32 <= 1040
                );
            }
        }
        let (_, w, h, x, y) = geometry(Anchor { x: 999, y: 999 }, (0, 0, 300, 240), 2., true);
        assert_eq!((w, h, x, y), (300, 240, 0, 0));
    }
}
