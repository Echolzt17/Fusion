use super::*;
use crate::{codex_window, widget_input::{install, tests_surface}};
use std::{sync::{Arc, atomic::{AtomicBool, AtomicUsize, Ordering}}, time::{Duration, Instant}};
use tauri::{WebviewUrl, WebviewWindowBuilder};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::{core::w, Win32::{
    Foundation::{WPARAM, LRESULT},
    UI::Input::KeyboardAndMouse::SetFocus,
}};

// Different UI threads, actual WebView2, and an optional visible IME fixture.
// No installed client, account, credentials, profile or network is used.
#[test]
#[ignore = "requires WebView2; run alone; FUSION_IME_INTERACTIVE=1 enables a 120 second real-IME fixture"]
#[allow(deprecated)]
fn actual_pointer_surface_keeps_foreign_thread_editor_focus() {
    let interactive = std::env::var_os("FUSION_IME_INTERACTIVE").is_some();
    let profile = tempfile::tempdir().unwrap();
    let running = Arc::new(AtomicBool::new(true));
    let stop = running.clone();
    let changes = Arc::new(AtomicUsize::new(0));
    let lost = changes.clone();
    let compositions = Arc::new(AtomicUsize::new(0));
    let ime = compositions.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let host_thread = std::thread::spawn(move || unsafe {
        struct Counters { lost: Arc<AtomicUsize>, ime: Arc<AtomicUsize> }
        unsafe extern "system" fn editor_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM, _: usize, data: usize) -> LRESULT {
            let counters = &*(data as *const Counters);
            if msg == WM_KILLFOCUS { counters.lost.fetch_add(1, Ordering::SeqCst); }
            if msg == WM_IME_ENDCOMPOSITION { counters.ime.fetch_add(1, Ordering::SeqCst); }
            if msg == WM_NCDESTROY { drop(Box::from_raw(data as *mut Counters)); }
            DefSubclassProc(hwnd, msg, wp, lp)
        }
        let parent = CreateWindowExW(Default::default(), w!("STATIC"), w!("Fusion IME verification - temporary editor"),
            WS_OVERLAPPEDWINDOW, 160, 120, 850, 600, None, None, None, None).unwrap();
        let _ = CreateWindowExW(Default::default(), w!("STATIC"), w!("Type Pinyin in this editor, then toggle the panel without refocusing the editor:"),
            WS_CHILD | WS_VISIBLE, 20, 15, 780, 25, Some(parent), None, None, None).unwrap();
        let editor = CreateWindowExW(WS_EX_CLIENTEDGE, w!("EDIT"), w!(""),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(0x0004),
            20, 50, 780, 100, Some(parent), None, None, None).unwrap();
        let data = Box::into_raw(Box::new(Counters { lost, ime }));
        assert!(SetWindowSubclass(editor, Some(editor_proc), 1, data as usize).as_bool());
        let mut pid = 0;
        GetWindowThreadProcessId(parent, Some(&mut pid));
        let host = Host { window: parent.0 as isize, pid };
        let foreground_guard = codex_window::focus_tests::retain_foreground(host);
        if interactive { let _ = ShowWindow(parent, SW_SHOW); let _ = SetForegroundWindow(parent); }
        let _ = SetFocus(Some(editor));
        tx.send((host, editor.0 as isize)).unwrap();
        while stop.load(Ordering::SeqCst) && IsWindow(Some(parent)).as_bool() {
            let mut message = MSG::default();
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        drop(foreground_guard);
        let _ = DestroyWindow(parent);
    });
    struct Stop(Arc<AtomicBool>);
    impl Drop for Stop { fn drop(&mut self) { self.0.store(false, Ordering::SeqCst); } }
    let _stop = Stop(running.clone());
    let (host, editor) = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let mut app = tauri::Builder::<tauri::Wry>::default().any_thread()
        .build(tauri::test::mock_context(tauri::test::noop_assets())).unwrap();
    let focused = Arc::new(AtomicUsize::new(0));
    let seen = focused.clone();
    let loaded = Arc::new(AtomicBool::new(false));
    let ready = loaded.clone();
    let view = WebviewWindowBuilder::new(&app, "pointer-probe", WebviewUrl::External("about:blank".parse().unwrap()))
        .parent_raw(host.hwnd()).inner_size(416., 320.).position(350., 200.)
        .decorations(false).transparent(true).resizable(false).skip_taskbar(true)
        .visible(interactive).focused(false).focusable(false).data_directory(profile.path().to_owned())
        .on_page_load(move |_, event| {
            if matches!(event.event(), tauri::webview::PageLoadEvent::Finished) { ready.store(true, Ordering::SeqCst); }
        })
        .build().unwrap();
    view.on_window_event(move |e| {
        if matches!(e, tauri::WindowEvent::Focused(true)) { seen.fetch_add(1, Ordering::SeqCst); }
    });
    let child = view.hwnd().unwrap();
    let actions = Arc::new(AtomicUsize::new(0));
    let clicked = actions.clone();
    let target = view.clone();
    install(child, host, |_| {}, move |event| {
        clicked.fetch_add(1, Ordering::SeqCst);
        if event.kind == "up" {
            let script = format!("document.elementFromPoint({}, {})?.closest('button')?.click()", event.x, event.y);
            target.eval(script).is_ok()
        } else {
            true
        }
    }).unwrap();
    view.with_webview(move |webview| unsafe {
        webview.controller().CoreWebView2().unwrap().NavigateToString(w!("<!doctype html><html><meta charset='utf-8'><style>body{margin:0;background:#eceef3;font:18px system-ui;padding:20px}button{padding:15px;font:inherit}#panel{height:140px}</style><div id='panel'>真实输入法测试：开合后不要再点击输入框，继续拼音输入。<p>Native mouse surface / WebView2 receives no focus</p></div><button onclick=\"let p=document.querySelector('#panel');p.hidden=!p.hidden\">打开 / 关闭胶囊详情</button></html>")).unwrap();
    }).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while !loaded.load(Ordering::SeqCst) && Instant::now() < deadline {
        app.run_iteration(|_, _| {});
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(loaded.load(Ordering::SeqCst), "actual WebView2 must finish loading before pointer checks");
    // Real browser on another UI thread must not steal even once. Do not call
    // return_host_focus here: the original bug was hidden by after-the-fact repair.
    let baseline = changes.load(Ordering::SeqCst);
    assert_eq!(baseline, 0, "creating/loading the read-only WebView must also preserve host focus");
    let events_before = focused.load(Ordering::SeqCst);
    unsafe {
        assert_eq!(codex_window::host_focus(host, Some(child)).unwrap().window, editor);
        let surface = tests_surface(child);
        assert_eq!(GetWindow(child, GW_CHILD).unwrap(), surface, "mouse surface must be above the browser");
        assert_eq!(SendMessageW(surface, WM_NCHITTEST, None, Some(LPARAM(0))).0, HTCLIENT as isize);
        for (width, height) in [(340, 40), (416, 520), (340, 40)] {
            SetWindowPos(child, None, 0, 0, width, height, SWP_NOMOVE | SWP_NOACTIVATE | SWP_NOZORDER).unwrap();
            app.run_iteration(|_, _| {});
            let mut rect = RECT::default();
            let mut client = RECT::default();
            GetClientRect(child, &mut client).unwrap();
            GetClientRect(surface, &mut rect).unwrap();
            assert_eq!(rect, client, "every resized client pixel must stay behind the input surface");
            assert_eq!(GetWindow(child, GW_CHILD).unwrap(), surface);
        }
        let activation = SendMessageW(surface, WM_MOUSEACTIVATE, Some(WPARAM(host.window as usize)), Some(LPARAM(0)));
        assert_eq!(activation.0, MA_NOACTIVATE as isize);
        for _ in 0..12 {
            SendMessageW(surface, WM_LBUTTONDOWN, Some(WPARAM(1)), Some(LPARAM(40 | (40 << 16))));
            SendMessageW(surface, WM_LBUTTONUP, Some(WPARAM(0)), Some(LPARAM(40 | (40 << 16))));
            app.run_iteration(|_, _| {});
            assert_eq!(GetWindow(child, GW_CHILD).unwrap(), surface);
        }
    }
    assert_eq!(changes.load(Ordering::SeqCst), baseline);
    assert_eq!(focused.load(Ordering::SeqCst), events_before);
    let pointer_trace = crate::widget_input::tests_diagnostics(child);
    assert_eq!(pointer_trace["nativeDown"], 12);
    assert_eq!(pointer_trace["nativeUp"], 12);
    // A hidden fixture may be rejected by the production foreground gate. Count
    // it explicitly: preserving focus alone must not be reported as successful
    // action delivery. The standalone gate test exercises both dispatch outcomes.
    let dispatched = pointer_trace["dispatchedDown"].as_u64().unwrap()
        + pointer_trace["dispatchedUp"].as_u64().unwrap();
    assert_eq!(dispatched + pointer_trace["foregroundRejected"].as_u64().unwrap(), 24);
    eprintln!("isolated pointer metadata: {pointer_trace}");
    if interactive {
        view.set_size(tauri::LogicalSize::new(416., 320.)).unwrap();
        app.run_iteration(|_, _| {});
        eprintln!("IME fixture ready for 120 seconds; type real Pinyin, toggle only, continue Pinyin. Nothing is sent.");
        let until = Instant::now() + Duration::from_secs(120);
        while host.alive() && Instant::now() < until { app.run_iteration(|_, _| {}); std::thread::sleep(Duration::from_millis(5)); }
        eprintln!("fixture results: native pointer actions={}, IME composition ends={}, editor focus losses={}",
            actions.load(Ordering::SeqCst), compositions.load(Ordering::SeqCst), changes.load(Ordering::SeqCst) - baseline);
    }
    view.destroy().unwrap();
    for _ in 0..10 { app.run_iteration(|_, _| {}); }
    running.store(false, Ordering::SeqCst);
    host_thread.join().unwrap();
    app.cleanup_before_exit();
}
