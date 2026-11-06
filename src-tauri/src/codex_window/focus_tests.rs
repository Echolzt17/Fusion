use super::*;
use std::sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}};
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};
use windows::{core::w, Win32::UI::Input::KeyboardAndMouse::GetFocus};

pub(super) fn retain_foreground(parent: Host) -> impl Drop {
    struct RestoreForeground { parent: Host, previous: isize }
    impl Drop for RestoreForeground {
        fn drop(&mut self) {
            let previous = HWND(self.previous as *mut _);
            if host_foreground(self.parent) && unsafe { IsWindow(Some(previous)).as_bool() } {
                unsafe { let _ = SetForegroundWindow(previous); }
            }
        }
    }
    RestoreForeground { parent, previous: unsafe { GetForegroundWindow() }.0 as isize }
}

// Actual Wry/WebView2, with a temporary browser profile. MoveFocus temporarily
// activates the fixture even when hidden. Restore the previous foreground app;
// never send keys or access an installed client's data.
#[test]
#[ignore = "requires installed WebView2; run alone with --ignored"]
#[allow(deprecated)]
fn actual_webview2_returns_focus_while_panel_remains_open() {
    let profile = tempfile::tempdir().unwrap();
    let target = Arc::new(Mutex::new(None::<(isize, Host, Option<FocusTarget>)>));
    let events = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = events.clone();
    let restore = target.clone();
    let mut app = tauri::Builder::<tauri::Wry>::default().any_thread()
        .on_window_event(move |window, event| {
            if window.label() != "focus-probe" || !matches!(event, tauri::WindowEvent::Focused(true)) { return; }
            seen.fetch_add(1, Ordering::SeqCst);
            let restore = restore.clone();
            let app = window.app_handle().clone();
            tauri::async_runtime::spawn(async move {
                app.run_on_main_thread(move || {
                    if let Some((child, host, previous)) = *restore.lock().unwrap() {
                        return_host_focus(HWND(child as *mut _), host, previous);
                    }
                }).unwrap();
            });
        })
        .build(tauri::test::mock_context(tauri::test::noop_assets())).unwrap();
    unsafe {
        let parent = CreateWindowExW(Default::default(), w!("STATIC"), w!("isolated WebView2 host"),
            WS_OVERLAPPEDWINDOW, 0, 0, 900, 700, None, None, None, None).unwrap();
        let editor = CreateWindowExW(Default::default(), w!("EDIT"), w!(""), WS_CHILD,
            0, 0, 200, 40, Some(parent), None, None, None).unwrap();
        let mut pid = 0;
        GetWindowThreadProcessId(parent, Some(&mut pid));
        let host = Host { window: parent.0 as isize, pid };
        let foreground_guard = retain_foreground(host);
        let loaded = Arc::new(AtomicBool::new(false));
        let ready = loaded.clone();
        let view = WebviewWindowBuilder::new(&app, "focus-probe", WebviewUrl::External("about:blank".parse().unwrap()))
            .parent_raw(parent).inner_size(416., 520.).visible(false).focused(false).focusable(false)
            .data_directory(profile.path().to_owned())
            .on_page_load(move |_, event| {
                if matches!(event.event(), tauri::webview::PageLoadEvent::Finished) { ready.store(true, Ordering::SeqCst); }
            }).build().unwrap();
        let child = view.hwnd().unwrap();
        crate::widget_input::install(child, host, |_| {}, |_| true).unwrap();
        view.with_webview(|webview| {
            webview.controller().CoreWebView2().unwrap()
                .NavigateToString(w!("<!doctype html><button>Isolated focus fixture</button>")).unwrap();
        }).unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while !loaded.load(Ordering::SeqCst) && Instant::now() < deadline {
            app.run_iteration(|_, _| {});
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(loaded.load(Ordering::SeqCst), "isolated WebView2 did not load");
        let _ = SetFocus(Some(editor));
        assert_eq!(GetFocus(), editor);
        *target.lock().unwrap() = Some((child.0 as isize, host, host_focus(host, Some(child))));
        assert_eq!(crate::widget_input::tests_registration().unwrap().hwndTarget, child);
        for round in 0..12 {
            let before = events.load(Ordering::SeqCst);
            view.with_webview(|webview| { webview.controller().MoveFocus(Default::default()).unwrap(); }).unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                app.run_iteration(|_, _| {});
                if events.load(Ordering::SeqCst) > before && GetFocus() == editor { break; }
                assert!(Instant::now() < deadline, "WebView2 focus was not returned in round {round}");
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(IsWindow(Some(child)).as_bool());
        }
        view.destroy().unwrap();
        for _ in 0..10 { app.run_iteration(|_, _| {}); }
        assert!(!crate::widget_input::tests_registration().is_some_and(|d| d.hwndTarget == child));
        drop(foreground_guard);
        DestroyWindow(parent).unwrap();
        app.cleanup_before_exit();
    }
}
