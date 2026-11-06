//! The read-only WebView must never receive native mouse input: returning HWND
//! focus afterwards is too late to preserve a foreign Chromium/TSF text store.
use crate::{codex_window::{self, Host}, Result};
use serde::Serialize;
use std::sync::{Arc, OnceLock};
use super::trace::{self, Trace};
use windows::{core::w, Win32::{
    Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
    Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT},
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        HiDpi::GetDpiForWindow,
        Input::KeyboardAndMouse::{GetCapture, ReleaseCapture, SetCapture},
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::*,
    },
}};

const SUBCLASS: usize = 0x46555350;
static REGISTERED: OnceLock<bool> = OnceLock::new();
unsafe extern "system" fn window_proc(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    DefWindowProcW(window, message, wparam, lparam)
}
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PointerAction {
    pub kind: &'static str,
    pub x: f64,
    pub y: f64,
    pub delta: f64,
}
struct Surface {
    host: Host,
    action: Box<dyn Fn(PointerAction) -> bool>,
    trace: Arc<Trace>,
}
impl Surface {
    fn dispatch(&self, action: PointerAction) {
        self.trace.dispatched(action.kind);
        self.trace.bridge((self.action)(action));
    }
    fn forward(&self, action: PointerAction, host_foreground: bool) {
        self.trace.native(action.kind);
        if host_foreground { self.dispatch(action); } else { self.trace.rejected(); }
    }
}

pub(super) fn install(parent: HWND, host: Host, action: impl Fn(PointerAction) -> bool + 'static) -> Result<HWND> {
    unsafe {
        let instance = HINSTANCE(GetModuleHandleW(None).map_err(|_| "无法读取挂件模块")?.0);
        let class = w!("FusionWidgetPointerSurface");
        if !*REGISTERED.get_or_init(|| RegisterClassW(&WNDCLASSW {
            hInstance: instance, lpszClassName: class, lpfnWndProc: Some(window_proc),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        }) != 0) { return Err("无法注册挂件鼠标窗口".into()); }
        // A normal transparent-painted child, NOT a layered click-through window.
        // All hit testing stays here; the browser receives only semantic UI events.
        // Use our own class: static text controls can be skipped by WindowFromPoint.
        let surface = CreateWindowExW(WS_EX_TRANSPARENT | WS_EX_NOACTIVATE, class, w!(""),
            WS_CHILD | WS_VISIBLE, 0, 0, 0, 0, Some(parent), None, Some(instance), None)
            .map_err(|_| "无法创建挂件的非激活鼠标层".to_owned())?;
        let trace = Trace::new();
        let data = Box::into_raw(Box::new(Surface { host, action: Box::new(action), trace: trace.clone() }));
        if !SetWindowSubclass(surface, Some(procedure), SUBCLASS, data as usize).as_bool() {
            drop(Box::from_raw(data));
            let _ = DestroyWindow(surface);
            return Err("无法保护挂件的输入法焦点".into());
        }
        if let Err(error) = resize(surface, parent) {
            let _ = DestroyWindow(surface);
            return Err(error);
        }
        trace::remember(trace);
        Ok(surface)
    }
}
pub(super) fn resize(surface: HWND, parent: HWND) -> Result<()> {
    unsafe {
        let mut rect = RECT::default();
        GetClientRect(parent, &mut rect).map_err(|_| "无法读取挂件鼠标区域")?;
        SetWindowPos(surface, Some(HWND_TOP), 0, 0, rect.right, rect.bottom,
            SWP_NOACTIVATE).map_err(|_| "无法更新挂件鼠标区域".into())
    }
}
fn point(lparam: LPARAM, scale: f64) -> (f64, f64) {
    ((lparam.0 as u16 as i16) as f64 / scale, ((lparam.0 >> 16) as u16 as i16) as f64 / scale)
}
unsafe extern "system" fn procedure(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM, _: usize, data: usize) -> LRESULT {
    let surface = &*(data as *const Surface);
    match message {
        WM_NCHITTEST => return LRESULT(HTCLIENT as isize),
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => return LRESULT(1),
        WM_CAPTURECHANGED => {
            surface.trace.capture_changed();
            surface.dispatch(PointerAction { kind: "cancel", x: 0., y: 0., delta: 0. });
            return LRESULT(0);
        }
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            let _ = BeginPaint(window, &mut paint);
            let _ = EndPaint(window, &paint);
            return LRESULT(0);
        }
        WM_LBUTTONDOWN | WM_LBUTTONUP | WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
            let scale = GetDpiForWindow(window).max(96) as f64 / 96.;
            let (mut x, mut y) = point(lparam, scale);
            let kind = match message {
                WM_LBUTTONDOWN => { SetCapture(window); "down" }
                WM_LBUTTONUP => "up",
                WM_MOUSEHWHEEL => "horizontalWheel",
                _ => "wheel",
            };
            let delta = if matches!(message, WM_MOUSEWHEEL | WM_MOUSEHWHEEL) {
                let mut location = windows::Win32::Foundation::POINT {
                    x: (x * scale).round() as i32, y: (y * scale).round() as i32,
                };
                let _ = windows::Win32::Graphics::Gdi::ScreenToClient(window, &mut location);
                x = location.x as f64 / scale;
                y = location.y as f64 / scale;
                ((wparam.0 >> 16) as u16 as i16) as f64
            } else { 0. };
            surface.forward(PointerAction { kind, x, y, delta }, codex_window::host_foreground(surface.host));
            // Releasing capture synchronously emits WM_CAPTURECHANGED. Deliver
            // up first, otherwise cancellation would swallow every button click.
            if message == WM_LBUTTONUP && GetCapture() == window { let _ = ReleaseCapture(); }
            return LRESULT(0);
        }
        // No text selection, focus, context menu or keyboard forwarding from a
        // read-only status panel into the browser (or into the user's editor).
        WM_RBUTTONDOWN | WM_RBUTTONUP | WM_MBUTTONDOWN | WM_MBUTTONUP => return LRESULT(0),
        WM_NCDESTROY => {
            surface.trace.destroyed();
            let _ = RemoveWindowSubclass(window, Some(procedure), SUBCLASS);
            drop(Box::from_raw(data as *mut Surface));
        }
        _ => {}
    }
    DefSubclassProc(window, message, wparam, lparam)
}

#[cfg(test)]
pub(super) fn tests_diagnostics(window: HWND) -> super::trace::Snapshot {
    unsafe {
        let mut data = 0;
        assert!(windows::Win32::UI::Shell::GetWindowSubclass(window, Some(procedure), SUBCLASS, Some(&mut data)).as_bool());
        (*(data as *const Surface)).trace.snapshot().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[test]
    fn pointer_coordinates_keep_signed_positions_and_dpi() {
        let packed = LPARAM((((-24i16 as u16) as usize) << 16 | (-12i16 as u16) as usize) as isize);
        assert_eq!(point(packed, 1.5), (-8., -16.));
    }
    #[test]
    fn native_guard_and_bridge_outcomes_are_distinct_without_changing_focus_policy() {
        let trace = Trace::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let received = calls.clone();
        let surface = Surface {
            host: Host { window: 0, pid: 0 }, trace: trace.clone(),
            action: Box::new(move |event| { received.fetch_add(1, Ordering::Relaxed); event.kind != "up" }),
        };
        surface.forward(PointerAction { kind: "down", x: 3., y: 9., delta: 0. }, false);
        assert_eq!(calls.load(Ordering::Relaxed), 0, "foreground rejection must not invoke the bridge");
        surface.forward(PointerAction { kind: "down", x: 3., y: 9., delta: 0. }, true);
        surface.forward(PointerAction { kind: "up", x: 3., y: 9., delta: 0. }, true);
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        let snapshot = serde_json::to_value(trace.snapshot().unwrap()).unwrap();
        assert_eq!(snapshot["nativeDown"], 2);
        assert_eq!(snapshot["nativeUp"], 1);
        assert_eq!(snapshot["foregroundRejected"], 1);
        assert_eq!(snapshot["dispatchedDown"], 1);
        assert_eq!(snapshot["dispatchedUp"], 1);
        assert_eq!(snapshot["bridgeAccepted"], 1);
        assert_eq!(snapshot["bridgeFailed"], 1);
    }
}
