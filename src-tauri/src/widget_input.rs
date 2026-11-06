//! Mouse observation plus a native input surface that keeps WebView2 unfocused.
mod surface;
mod trace;
pub use surface::PointerAction;
pub use trace::{diagnostics, acknowledge, stalled};
use crate::{codex_window::Host, Result};
use std::mem::size_of;
use windows::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
    UI::{
        Input::{GetRawInputData, GetRegisteredRawInputDevices, RegisterRawInputDevices,
            HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RIDEV_INPUTSINK, RIDEV_REMOVE,
            RID_INPUT, RIM_TYPEMOUSE},
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::{GetCursorPos, GetForegroundWindow, IsChild, IsWindow,
            WindowFromPoint, DestroyWindow, MA_NOACTIVATE, WM_INPUT, WM_MOUSEACTIVATE, WM_NCDESTROY, WM_SIZE},
    },
};

const SUBCLASS: usize = 0x46555349;
struct Observer {
    host: Host,
    surface: HWND,
    previous: Option<RAWINPUTDEVICE>,
    pointer: Box<dyn Fn(bool)>,
}
fn mouse_registration() -> Option<RAWINPUTDEVICE> {
    unsafe {
        let mut count = 0;
        let size = size_of::<RAWINPUTDEVICE>() as u32;
        if GetRegisteredRawInputDevices(None, &mut count, size) == u32::MAX { return None; }
        let mut devices = vec![RAWINPUTDEVICE::default(); count as usize];
        if GetRegisteredRawInputDevices(Some(devices.as_mut_ptr()), &mut count, size) == u32::MAX { return None; }
        devices.into_iter().take(count as usize).find(|d| d.usUsagePage == 1 && d.usUsage == 2)
    }
}

// UI thread only. Save Tao's previous raw-mouse registration and restore it on
// destruction; legacy mouse/keyboard delivery is never disabled or intercepted.
pub fn install(window: HWND, host: Host, pointer: impl Fn(bool) + 'static,
    action: impl Fn(PointerAction) -> bool + 'static) -> Result<()> {
    let surface = surface::install(window, host, action)?;
    let observer = Box::new(Observer { host, surface, previous: mouse_registration(), pointer: Box::new(pointer) });
    let data = Box::into_raw(observer);
    unsafe {
        if !SetWindowSubclass(window, Some(procedure), SUBCLASS, data as usize).as_bool() {
            drop(Box::from_raw(data));
            let _ = DestroyWindow(surface);
            return Err("无法设置挂件的非激活鼠标处理".into());
        }
        let device = RAWINPUTDEVICE { usUsagePage: 1, usUsage: 2, dwFlags: RIDEV_INPUTSINK, hwndTarget: window };
        if RegisterRawInputDevices(&[device], size_of::<RAWINPUTDEVICE>() as u32).is_err() {
            let _ = RemoveWindowSubclass(window, Some(procedure), SUBCLASS);
            drop(Box::from_raw(data));
            let _ = DestroyWindow(surface);
            return Err("无法观察挂件外部点击，未显示挂件".into());
        }
    }
    Ok(())
}
unsafe extern "system" fn procedure(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM, _: usize, data: usize) -> LRESULT {
    let observer = &*(data as *const Observer);
    if message == WM_SIZE {
        // Let Wry resize first, then keep the native surface above its HWNDs.
        let result = DefSubclassProc(window, message, wparam, lparam);
        let _ = surface::resize(observer.surface, window);
        return result;
    }
    if message == WM_MOUSEACTIVATE {
        (observer.pointer)(true);
        if GetForegroundWindow() == observer.host.hwnd() {
            // WS_EX_NOACTIVATE alone does not protect a cross-process WS_CHILD.
            return LRESULT(MA_NOACTIVATE as isize);
        }
    }
    if message == WM_INPUT {
        let mut input = RAWINPUT::default();
        let mut size = size_of::<RAWINPUT>() as u32;
        let received = GetRawInputData(HRAWINPUT(lparam.0 as *mut _), RID_INPUT,
            Some(&mut input as *mut _ as *mut _), &mut size, size_of::<RAWINPUTHEADER>() as u32);
        if received == size_of::<RAWINPUT>() as u32 && input.header.dwType == RIM_TYPEMOUSE.0 {
            // Left/right/middle/X button down only. No movement history, text,
            // keyboard data, or raw-input bytes are retained.
            if input.data.mouse.Anonymous.Anonymous.usButtonFlags & 0x155 != 0 {
                let mut point = POINT::default();
                if GetCursorPos(&mut point).is_ok() {
                    let hit = WindowFromPoint(point);
                    (observer.pointer)(hit == window || IsChild(window, hit).as_bool());
                }
            }
        }
    }
    if message == WM_NCDESTROY {
        if mouse_registration().is_some_and(|d| d.hwndTarget == window) {
            let device = observer.previous.filter(|d| d.hwndTarget.is_invalid() || IsWindow(Some(d.hwndTarget)).as_bool())
                .unwrap_or(RAWINPUTDEVICE { usUsagePage: 1, usUsage: 2, dwFlags: RIDEV_REMOVE, ..Default::default() });
            let _ = RegisterRawInputDevices(&[device], size_of::<RAWINPUTDEVICE>() as u32);
        }
        let _ = RemoveWindowSubclass(window, Some(procedure), SUBCLASS);
        drop(Box::from_raw(data as *mut Observer));
    }
    DefSubclassProc(window, message, wparam, lparam)
}
#[cfg(test)]
pub fn tests_registration() -> Option<RAWINPUTDEVICE> { mouse_registration() }
#[cfg(test)]
pub fn tests_surface(window: HWND) -> HWND {
    unsafe {
        let mut data = 0;
        assert!(windows::Win32::UI::Shell::GetWindowSubclass(window, Some(procedure), SUBCLASS, Some(&mut data)).as_bool());
        (*(data as *const Observer)).surface
    }
}
/// UI thread only. Reassert the input surface above late-created WebView HWNDs.
pub fn maintain(window: HWND, enabled: bool) -> Result<()> {
    unsafe {
        let mut data = 0;
        if !windows::Win32::UI::Shell::GetWindowSubclass(window, Some(procedure), SUBCLASS, Some(&mut data)).as_bool() || data == 0 { return Err("挂件鼠标观察层已失效".into()); }
        let observer = &mut *(data as *mut Observer);
        surface::resize(observer.surface, window)?;
        if !mouse_registration().is_some_and(|device| device.hwndTarget == window) {
            observer.previous = mouse_registration();
            let device = RAWINPUTDEVICE { usUsagePage: 1, usUsage: 2, dwFlags: RIDEV_INPUTSINK, hwndTarget: window };
            RegisterRawInputDevices(&[device], size_of::<RAWINPUTDEVICE>() as u32).map_err(|_| "无法恢复挂件外部点击观察")?;
        }
        surface::ready(observer.surface, enabled)
    }
}
#[cfg(test)]
pub fn tests_diagnostics(window: HWND) -> serde_json::Value {
    serde_json::to_value(surface::tests_diagnostics(tests_surface(window))).unwrap()
}
