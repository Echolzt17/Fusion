//! Native window hosting and graceful restart. Never reads client credentials or patches files.
use pool_core::Result;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use windows::{
    core::PWSTR,
    Win32::{
        Foundation::{CloseHandle, HANDLE, HWND, LPARAM, POINT, RECT},
        Graphics::Gdi::ScreenToClient,
        System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
            PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        },
        UI::{
            HiDpi::{GetDpiForWindow, GetWindowDpiAwarenessContext, GetThreadDpiAwarenessContext, AreDpiAwarenessContextsEqual},
            Input::KeyboardAndMouse::{
                GetAsyncKeyState, SetFocus, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
                KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_CONTROL, VK_ESCAPE, VK_LBUTTON, VK_Q,
            },
            WindowsAndMessaging::*,
        },
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Host {
    pub window: isize,
    pub pid: u32,
}
impl Host {
    pub fn hwnd(self) -> HWND {
        HWND(self.window as *mut _)
    }
    pub fn area(self) -> Option<(u32, u32, f64)> {
        unsafe {
            if !self.alive()
                || !IsWindowVisible(self.hwnd()).as_bool()
                || IsIconic(self.hwnd()).as_bool()
            {
                return None;
            }
            let mut r = RECT::default();
            GetClientRect(self.hwnd(), &mut r).ok()?;
            let (width, height) = (r.right - r.left, r.bottom - r.top);
            (width > 0 && height > 0).then(|| {
                (
                    width as u32,
                    height as u32,
                    GetDpiForWindow(self.hwnd()).max(96) as f64 / 96.,
                )
            })
        }
    }
    pub fn alive(self) -> bool {
        let mut pid = 0;
        unsafe {
            GetWindowThreadProcessId(self.hwnd(), Some(&mut pid));
            IsWindow(Some(self.hwnd())).as_bool() && pid == self.pid
        }
    }
    pub fn cursor(self) -> Option<(i32, i32)> {
        unsafe {
            let mut p = POINT::default();
            GetCursorPos(&mut p).ok()?;
            ScreenToClient(self.hwnd(), &mut p)
                .as_bool()
                .then_some((p.x, p.y))
        }
    }
}
struct Process(HANDLE);
impl Drop for Process {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
fn process(pid: u32, wait: bool) -> Option<Process> {
    unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION
                | if wait {
                    PROCESS_SYNCHRONIZE
                } else {
                    Default::default()
                },
            false,
            pid,
        )
        .ok()
        .map(Process)
    }
}
fn executable(p: &Process) -> Option<PathBuf> {
    let mut text = vec![0u16; 32768];
    let mut length = text.len() as u32;
    unsafe {
        QueryFullProcessImageNameW(
            p.0,
            PROCESS_NAME_WIN32,
            PWSTR(text.as_mut_ptr()),
            &mut length,
        )
        .ok()?;
    }
    Some(PathBuf::from(String::from_utf16_lossy(
        &text[..length as usize],
    )))
}
pub(crate) fn desktop_path(path: &std::path::Path) -> bool {
    let text = path
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase();
    // The desktop executable is ChatGPT.exe in this package, NOT codex.exe (the engine).
    text.contains("\\windowsapps\\openai.codex_") && text.ends_with("\\app\\chatgpt.exe")
}
pub fn find() -> Option<Host> {
    find_window(false)
}
pub(crate) fn find_any() -> Option<Host> { find().or_else(|| find_window(true)) }
// Resolve and probe the client without activating a desktop under the old provider.
pub fn prepare_for_enable(store: &pool_core::storage::Store) -> Result<pool_core::compatibility::ClientIdentity> {
    crate::codex_lifecycle::prepare_identity(store)
}

fn activation_result(code: Option<i32>) -> Result<()> {
    match code {
        Some(0) => Ok(()),
        Some(2) => Err("未找到已安装的 Codex 桌面客户端，请安装后再次启用账号池".into()),
        Some(3) => Err("无法确定 Codex 安装入口，请先手动打开 Codex 后重试".into()),
        Some(4) => Err("无法加载 Windows Appx 组件，Codex 启动检查未完成；请检查系统组件及组织策略".into()),
        Some(5) => Err("无法查询当前 Windows 用户的 Codex 商店安装信息，启动检查未完成".into()),
        Some(6) => Err("无法读取 Codex 商店应用清单，启动检查未完成；请检查商店更新是否完成".into()),
        Some(7) => Err("Windows 未能启动 Codex 商店入口，请检查系统策略及客户端安装状态".into()),
        Some(code) => Err(format!("Codex 启动检查异常结束（退出码 {code}），请将此提示提供给维护人员")),
        None => Err("Codex 启动检查异常结束，未取得退出码".into()),
    }
}

// Ask Windows instead of relying on PATH, WINDIR, or the working directory of
// the shortcut that launched Fusion. Do not fall back to a same-named local exe.
fn windows_directory(system: bool) -> Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows::Win32::System::SystemInformation::{GetSystemDirectoryW, GetWindowsDirectoryW};
    let mut buffer = vec![0u16; 32768];
    let length = unsafe {
        if system { GetSystemDirectoryW(Some(&mut buffer)) }
        else { GetWindowsDirectoryW(Some(&mut buffer)) }
    } as usize;
    if length == 0 || length >= buffer.len() {
        return Err("无法从 Windows 获取系统目录，未启动 Codex".into());
    }
    let path = PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length]));
    if !path.is_absolute() { return Err("Windows 返回的系统目录无效，未启动 Codex".into()); }
    Ok(path)
}

pub(crate) fn explorer_path() -> Result<PathBuf> {
    Ok(windows_directory(false)?.join("explorer.exe"))
}

pub(crate) fn launch_error(stage: &str, error: &std::io::Error) -> String {
    use std::io::ErrorKind;
    let reason = match error.raw_os_error() {
        Some(1260 | 577 | 225 | 226) => "被系统策略或安全软件阻止",
        _ => match error.kind() {
            ErrorKind::NotFound => "系统启动组件缺失",
            ErrorKind::PermissionDenied => "访问被拒绝，请检查系统策略及运行权限",
            _ => "系统未能执行启动组件",
        },
    };
    // Only a fixed classification and numeric OS code; never arbitrary error text.
    let code = error.raw_os_error().map(|code| format!("（Windows 错误码 {code}）")).unwrap_or_default();
    format!("{stage}失败：{reason}{code}。请将此提示提供给维护人员")
}

fn activation_command(script: &str) -> Result<std::process::Command> {
    use std::{os::windows::process::CommandExt, process::{Command, Stdio}};
    let system = windows_directory(true)?;
    let mut command = Command::new(system.join(r"WindowsPowerShell\v1.0\powershell.exe"));
    command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", script])
        .current_dir(system)
        .creation_flags(0x08000000)
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    Ok(command)
}

fn wait_for_activation(child: &mut std::process::Child) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let result = match child.try_wait() {
            Ok(Some(status)) => return activation_result(status.code()),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            Ok(None) => Err("Codex 商店启动检查超时；未确认启动完成，请检查系统响应后重试".into()),
            Err(error) => Err(launch_error("读取 Codex 启动检查状态", &error)),
        };
        // Terminate only our own helper, never a client or another PowerShell.
        let _ = child.kill();
        let _ = child.wait();
        return result;
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InstalledClient {
    pub desktop: PathBuf,
    pub id: String,
    pub version: String,
}
impl InstalledClient {
    pub fn engine(&self) -> PathBuf {
        self.desktop.with_file_name("resources").join("codex.exe")
    }
}
fn parse_installed_client(bytes: &[u8]) -> Result<InstalledClient> {
    let client: InstalledClient = serde_json::from_slice(bytes).map_err(|_| "Codex 商店安装信息无效")?;
    let package = client.desktop.parent().and_then(std::path::Path::parent)
        .and_then(std::path::Path::file_name).and_then(|s| s.to_str()).unwrap_or_default();
    if !client.desktop.is_absolute() || !desktop_path(&client.desktop)
        || client.version.split('.').count() != 4
        || !client.version.split('.').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        || !package.starts_with(&format!("OpenAI.Codex_{}_", client.version))
        || !client.id.starts_with("OpenAI.Codex_") || client.id.len() > 512
        || client.id.split('!').count() != 2 || client.id.ends_with('!')
        || !client.id.chars().all(|c| c.is_ascii_alphanumeric() || "_.!-".contains(c))
    {
        return Err("Codex 商店安装身份不匹配，未启动客户端".into());
    }
    if !client.desktop.is_file() || !client.engine().is_file() {
        return Err("Codex 商店安装文件不完整，请等待客户端更新完成后重试".into());
    }
    Ok(client)
}
pub(crate) fn installed_client() -> Result<InstalledClient> {
    use std::io::{Read, Seek, SeekFrom};
    // Resolve the current user's package and its desktop entry from the manifest;
    // neither a hardcoded version/path nor the separate ChatGPT package is used.
    // Windows PowerShell is intentional: the Appx module cannot load in pwsh 7.
    // Import the system module explicitly: a PSModulePath inherited from pwsh
    // or another launcher must not hide Appx or substitute a user module.
    let script = r#"$ErrorActionPreference='Stop';
      [Console]::OutputEncoding=[Text.UTF8Encoding]::new($false);
      try { Import-Module (Join-Path $PSHOME 'Modules\Appx\Appx.psd1') -ErrorAction Stop } catch { exit 4 };
      try {
        $packages=@(Get-AppxPackage -Name OpenAI.Codex);
        if($packages.Count -eq 0){exit 2}; if($packages.Count -ne 1){exit 3};
      } catch { exit 5 };
      try {
        $package=$packages[0]; $manifest=Get-AppxPackageManifest -Package $package.PackageFullName;
        $apps=@($manifest.Package.Applications.Application | Where-Object { ($_.Executable -replace '\\','/') -ieq 'app/ChatGPT.exe' });
        if($apps.Count -ne 1){exit 3};
      } catch { exit 6 };
      try {
        [Console]::Write((@{desktop=(Join-Path $package.InstallLocation $apps[0].Executable); id=($package.PackageFamilyName+'!'+$apps[0].Id); version=$package.Version.ToString()} | ConvertTo-Json -Compress));
        exit 0
      } catch { exit 6 }"#;
    // A temporary file avoids waiting on a full stdout pipe. It contains only
    // package metadata and is removed on every success/failure path.
    let mut output = tempfile::tempfile().map_err(|_| "无法创建 Codex 安装检查临时文件")?;
    let mut child = activation_command(script)?
        .stdout(output.try_clone().map_err(|_| "无法准备 Codex 安装检查输出")?)
        .spawn().map_err(|error| launch_error("执行 Codex 启动检查", &error))?;
    wait_for_activation(&mut child)?;
    output.seek(SeekFrom::Start(0)).map_err(|_| "无法读取 Codex 安装检查输出")?;
    let mut bytes = Vec::new();
    output.take(65537).read_to_end(&mut bytes).map_err(|_| "无法读取 Codex 安装检查输出")?;
    if bytes.len() > 65536 { return Err("Codex 安装检查输出超过允许大小".into()); }
    parse_installed_client(&bytes)
}

fn find_window(include_hidden: bool) -> Option<Host> {
    struct Search {
        include_hidden: bool,
        found: Option<Host>,
    }
    unsafe extern "system" fn visit(hwnd: HWND, data: LPARAM) -> windows::core::BOOL {
        let search = &mut *(data.0 as *mut Search);
        if GetWindowLongPtrW(hwnd, GWL_STYLE) as u32 & WS_CHILD.0 != 0
            || GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0
        {
            return true.into();
        }
        if GetWindow(hwnd, GW_OWNER)
            .ok()
            .is_some_and(|owner| !owner.is_invalid())
        {
            return true.into();
        }
        let mut class = [0u16; 128];
        let length = GetClassNameW(hwnd, &mut class);
        if !String::from_utf16_lossy(&class[..length.max(0) as usize])
            .starts_with("Chrome_WidgetWin_")
        {
            return true.into();
        }
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if (IsWindowVisible(hwnd).as_bool()
            || (search.include_hidden && GetWindowTextLengthW(hwnd) > 0))
            && process(pid, false)
                .and_then(|p| executable(&p))
                .is_some_and(|p| desktop_path(&p))
        {
            let found = &mut search.found;
            let candidate = Host {
                window: hwnd.0 as isize,
                pid,
            };
            if found.is_none() || GetForegroundWindow() == hwnd {
                *found = Some(candidate);
            }
        }
        true.into()
    }
    let mut search = Search {
        include_hidden,
        found: None,
    };
    unsafe {
        let _ = EnumWindows(Some(visit), LPARAM(&mut search as *mut _ as isize));
    }
    search.found
}
pub fn left_pressed() -> bool {
    unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) < 0 }
}
pub fn host_foreground(host: Host) -> bool {
    host.alive() && unsafe { GetForegroundWindow() == host.hwnd() }
}
pub fn escape_pressed() -> bool {
    unsafe { GetAsyncKeyState(VK_ESCAPE.0 as i32) < 0 }
}

pub fn is_child_of(window: HWND, host: Host) -> bool {
    unsafe {
        GetWindowLongPtrW(window, GWL_STYLE) as u32 & WS_CHILD.0 != 0
            && GetParent(window).ok() == Some(host.hwnd())
    }
}

// Called on Tauri's UI thread before background WebView creation. Do not mutate the
// UI thread's DPI context or synchronously construct WebView2 inside its event callback.
pub fn thread_dpi_matches(host: Host) -> bool {
    host.alive() && unsafe {
        AreDpiAwarenessContextsEqual(GetThreadDpiAwarenessContext(), GetWindowDpiAwarenessContext(host.hwnd())).as_bool()
    }
}

pub fn child_dpi_matches(window: HWND, host: Host) -> bool {
    host.alive() && is_child_of(window, host) && unsafe {
        AreDpiAwarenessContextsEqual(GetWindowDpiAwarenessContext(window), GetWindowDpiAwarenessContext(host.hwnd())).as_bool()
    }
}

// Cross-process WS_CHILD windows share an input queue with their parent. Keep
// the last real Codex focus target; WS_EX_NOACTIVATE alone does not return focus.
#[derive(Clone, Copy)]
pub struct FocusTarget {
    window: isize,
    pid: u32,
    thread: u32,
}
fn contains_window(root: HWND, window: HWND) -> bool {
    !window.is_invalid() && (root == window || unsafe { IsChild(root, window).as_bool() })
}
fn host_input(host: Host) -> Option<GUITHREADINFO> {
    if !host.alive() { return None; }
    let thread = unsafe { GetWindowThreadProcessId(host.hwnd(), None) };
    let mut info = GUITHREADINFO { cbSize: std::mem::size_of::<GUITHREADINFO>() as u32, ..Default::default() };
    unsafe { GetGUIThreadInfo(thread, &mut info).ok()?; }
    Some(info)
}
pub fn host_focus(host: Host, widget: Option<HWND>) -> Option<FocusTarget> {
    let focus = host_input(host)?.hwndFocus;
    if !contains_window(host.hwnd(), focus) || widget.is_some_and(|w| contains_window(w, focus)) {
        return None;
    }
    let mut pid = 0;
    let thread = unsafe { GetWindowThreadProcessId(focus, Some(&mut pid)) };
    Some(FocusTarget { window: focus.0 as isize, pid, thread })
}
pub fn return_host_focus(widget: HWND, host: Host, previous: Option<FocusTarget>) {
    if !is_child_of(widget, host) || unsafe { GetForegroundWindow() } != host.hwnd() { return; }
    return_focus_within_host(widget, host, previous);
}
fn return_focus_within_host(widget: HWND, host: Host, previous: Option<FocusTarget>) {
    if !host.alive() || !is_child_of(widget, host) { return; }
    let Some(info) = host_input(host) else { return; };
    // An outside click may already have focused another Codex control. Preserve it.
    if !contains_window(widget, info.hwndFocus) { return; }
    let target = previous.filter(|target| {
        let window = HWND(target.window as *mut _);
        let mut pid = 0;
        let thread = unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) };
        thread == target.thread && pid == target.pid && contains_window(host.hwnd(), window)
            && !contains_window(widget, window)
    }).map(|target| HWND(target.window as *mut _)).unwrap_or(host.hwnd());
    // No foreground activation, synthetic keys, or AttachThreadInput. Run on
    // the widget UI thread, after its WebView/WM_KILLFOCUS callback has returned.
    unsafe { let _ = SetFocus(Some(target)); }
}
#[cfg(test)]
#[path = "codex_window/focus_tests.rs"]
mod focus_tests;
#[cfg(test)]
#[path = "codex_window/pointer_tests.rs"]
mod pointer_tests;
pub fn capsule_hovered(widget: HWND, scale: f64) -> bool {
    unsafe {
        let mut point = POINT::default();
        let mut rect = RECT::default();
        if GetCursorPos(&mut point).is_err() || GetClientRect(widget, &mut rect).is_err()
            || !contains_window(widget, WindowFromPoint(point))
            || !ScreenToClient(widget, &mut point).as_bool() { return false; }
        point.x >= (rect.right - (340. * scale).round() as i32).max(0)
            && point.x < rect.right && point.y >= (rect.bottom - (40. * scale).round() as i32).max(0)
            && point.y < rect.bottom
    }
}

pub fn raise_child(window: HWND, host: Host) -> Result<()> {
    if !host.alive() || !is_child_of(window, host) { return Err("挂件宿主已变化".into()); }
    // Highest sibling within Codex only. No desktop topmost flag or focus activation.
    unsafe { SetWindowPos(window, Some(HWND_TOP), 0, 0, 0, 0,
        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS).map_err(|_| "无法显示 Codex 内挂件".into()) }
}

pub(crate) fn request_normal_quit(host: Host) -> Result<()> {
    // Verified in desktop 26.1023.6123.0: the app's Quit action is Ctrl+Q.
    // WM_CLOSE only closes a BrowserWindow; this client retains background windows.
    // Never patch Electron or inject JavaScript; lifecycle owns the bounded fallback.
    if !host.alive() {
        return Err("Codex 窗口已变化，未向其他窗口发送退出操作".into());
    }
    unsafe {
        if IsIconic(host.hwnd()).as_bool() || !IsWindowVisible(host.hwnd()).as_bool() {
            let _ = ShowWindowAsync(host.hwnd(), SW_RESTORE);
        }
        let _ = SetForegroundWindow(host.hwnd());
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if !host.alive() {
            return Err("Codex 窗口已变化，未向其他窗口发送退出操作".into());
        }
        // SendInput uses the foreground input queue. Refuse another app, a popup,
        // or held keys; do not release keys pressed by the user or steal their input.
        let ready = unsafe {
            GetForegroundWindow() == host.hwnd() && (8..=254).all(|key| GetAsyncKeyState(key) >= 0)
        };
        if ready {
            break;
        }
        if Instant::now() >= deadline {
            return Err(
                "无法确认 Codex 可接收正常退出操作（窗口未激活、存在弹窗或正在按键）；请处理后重试"
                    .into(),
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let key = |vk: VIRTUAL_KEY, flags: KEYBD_EVENT_FLAGS| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                dwFlags: flags,
                ..Default::default()
            },
        },
    };
    let input = [
        key(VK_CONTROL, KEYBD_EVENT_FLAGS::default()),
        key(VK_Q, KEYBD_EVENT_FLAGS::default()),
        key(VK_Q, KEYEVENTF_KEYUP),
        key(VK_CONTROL, KEYEVENTF_KEYUP),
    ];
    // One batch is not interleaved with other keyboard events (Win32 SendInput).
    let sent = unsafe { SendInput(&input, std::mem::size_of::<INPUT>() as i32) };
    if sent != input.len() as u32 {
        if sent > 0 {
            // Undo only the two synthetic keys if Windows accepted a partial batch.
            let release = [key(VK_Q, KEYEVENTF_KEYUP), key(VK_CONTROL, KEYEVENTF_KEYUP)];
            unsafe {
                SendInput(&release, std::mem::size_of::<INPUT>() as i32);
            }
        }
        return Err("Codex 正常退出操作未完整送达；请确认 Fusion 与 Codex 均以普通用户身份运行、处理弹窗后重试。未强制结束进程".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn installed_metadata_requires_matching_package_and_complete_bundled_engine() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join("WindowsApps/OpenAI.Codex_26.1027.6604.0_x64__fixture/app");
        std::fs::create_dir_all(app.join("resources")).unwrap();
        let desktop = app.join("ChatGPT.exe");
        let engine = app.join("resources/codex.exe");
        std::fs::write(&desktop, b"public fixture desktop").unwrap();
        std::fs::write(&engine, b"public fixture engine; never executed").unwrap();
        let metadata = serde_json::json!({"desktop":desktop,"id":"OpenAI.Codex_fixture!App","version":"26.1027.6604.0"});
        let parsed = parse_installed_client(&serde_json::to_vec(&metadata).unwrap()).unwrap();
        assert_eq!(parsed.engine(), engine);
        for (key, invalid) in [
            ("desktop", "relative/WindowsApps/OpenAI.Codex_26.1027.6604.0_x64__fixture/app/ChatGPT.exe"),
            ("version", "26.1027.3384.0"), ("version", "26.1027.6604"),
            ("id", "OpenAI.ChatGPT_fixture!App"), ("id", "OpenAI.Codex_fixture!"),
            ("id", "OpenAI.Codex_fixture!App;command"),
        ] {
            let mut changed = metadata.clone();
            changed[key] = invalid.into();
            assert!(parse_installed_client(&serde_json::to_vec(&changed).unwrap()).is_err());
        }
        std::fs::remove_file(engine).unwrap();
        assert!(parse_installed_client(&serde_json::to_vec(&metadata).unwrap()).err().unwrap().contains("文件不完整"));
    }
    #[test]
    #[ignore = "Read-only installed package discovery and isolated protocol probe; never activates Codex"]
    fn actual_installed_client_compatibility_without_activation() {
        let root = tempfile::tempdir().unwrap();
        let store = pool_core::storage::Store::open(&root.path().join("pool.sqlite")).unwrap();
        let installed = installed_client().unwrap();
        let identity = pool_core::compatibility::inspect_bundled(
            &store, installed.version.clone(), &installed.desktop, installed.engine(),
        ).unwrap();
        assert_eq!(identity.engine_path, installed.engine());
        println!("{}", serde_json::to_string(&identity.client).unwrap());
    }
    #[test]
    fn system_activation_helper_works_with_broken_path_and_module_path() {
        use std::{os::windows::process::CommandExt, process::{Command, Stdio}};
        let root = tempfile::tempdir().unwrap();
        // A shortcut's cwd/PATH can contain a different executable. Neither it
        // nor a foreign PowerShell module directory may select our helper.
        std::fs::write(root.path().join("powershell.exe"), b"invalid fixture executable").unwrap();
        let broken = Command::new("powershell.exe")
            .env("PATH", root.path()).current_dir(root.path())
            .creation_flags(0x08000000)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .spawn();
        assert!(broken.is_err(), "old PATH-based launch must fail in this fixture");
        let script = r#"$ErrorActionPreference='Stop'; try {
            Import-Module (Join-Path $PSHOME 'Modules\Appx\Appx.psd1') -ErrorAction Stop;
            if ((Get-Command Get-AppxPackage -ErrorAction Stop).ModuleName -ne 'Appx') { exit 4 };
            exit 0
        } catch { exit 4 }"#;
        let mut child = activation_command(script).unwrap()
            .env("PATH", root.path()).env("PSModulePath", root.path())
            .env("USERPROFILE", root.path()).env("LOCALAPPDATA", root.path())
            .env("CODEX_HOME", root.path()).current_dir(root.path())
            .spawn().unwrap();
        wait_for_activation(&mut child).unwrap();
        assert!(explorer_path().unwrap().is_absolute());
        // Only Appx command availability is checked; no real client is activated.
    }
    #[test]
    fn launch_failures_preserve_safe_stage_and_os_code_only() {
        for (code, expected) in [(2, "组件缺失"), (5, "访问被拒绝"), (1260, "系统策略"), (577, "安全软件"), (193, "系统未能执行")] {
            let error = launch_error("执行 Codex 启动检查", &std::io::Error::from_raw_os_error(code));
            assert!(error.contains(expected) && error.contains(&format!("错误码 {code}")));
        }
        let error = std::io::Error::other("sensitive-fixture-must-never-be-shown");
        assert!(!launch_error("启动", &error).contains("sensitive-fixture"));
        assert!(activation_result(Some(0)).is_ok());
        for code in [2, 3, 4, 5, 6, 7, 99] { assert!(activation_result(Some(code)).is_err()); }
        assert!(activation_result(None).is_err());
        assert!(activation_result(Some(4)).unwrap_err().contains("Appx"));
        assert!(activation_result(Some(6)).unwrap_err().contains("清单"));
    }
    #[test]
    fn recognizes_desktop_package_without_confusing_engine_or_chatgpt() {
        assert!(desktop_path(std::path::Path::new(
            r"C:\Program Files\WindowsApps\OpenAI.Codex_26.1023.6123.0_x64__example\app\ChatGPT.exe"
        )));
        for path in [
            r"C:\OpenAI\Codex\bin\hash\codex.exe",
            r"C:\WindowsApps\OpenAI.ChatGPT_1\app\ChatGPT.exe",
            r"C:\other\ChatGPT.exe",
        ] {
            assert!(!desktop_path(std::path::Path::new(path)));
        }
    }
    #[test]
    fn native_child_belongs_to_fixture_parent_and_is_destroyed_with_it() {
        use windows::core::w;
        unsafe {
            let parent = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                w!("Fusion isolated host"),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                640,
                480,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            let child = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                w!("Fusion isolated widget"),
                WS_CHILD,
                10,
                10,
                112,
                30,
                Some(parent),
                None,
                None,
                None,
            )
            .unwrap();
            let mut pid = 0;
            GetWindowThreadProcessId(parent, Some(&mut pid));
            let host = Host {
                window: parent.0 as isize,
                pid,
            };
            assert!(host.alive());
            assert!(is_child_of(child, host));
            assert!(child_dpi_matches(child, host));
            assert!(thread_dpi_matches(host));
            assert!(!is_child_of(parent, host));
            let foreground=GetForegroundWindow();
            raise_child(child,host).unwrap();
            assert_eq!(GetForegroundWindow(),foreground);
            assert_eq!(GetWindowLongPtrW(child,GWL_EXSTYLE) as u32 & WS_EX_TOPMOST.0,0);
            // Neither fixture window is displayed, no real client is touched.
            DestroyWindow(parent).unwrap();
            assert!(!host.alive());
            assert!(!thread_dpi_matches(host));
            assert!(!IsWindow(Some(child)).as_bool());
            assert!(raise_child(child,host).is_err());
        }
    }
    #[test]
    fn collapsed_widget_returns_native_focus_and_preserves_an_outside_click() {
        use windows::core::w;
        use windows::Win32::UI::Input::KeyboardAndMouse::GetFocus;
        unsafe {
            // Hidden native controls on this test's own UI thread. SetFocus may
            // implicitly activate the fixture. Never send keys or read real text.
            let parent = CreateWindowExW(Default::default(), w!("STATIC"), w!("isolated host"),
                WS_OVERLAPPEDWINDOW, 0, 0, 640, 480, None, None, None, None).unwrap();
            let control = |parent, class| CreateWindowExW(Default::default(), class, w!(""),
                WS_CHILD, 0, 0, 100, 30, Some(parent), None, None, None).unwrap();
            let editor = control(parent, w!("EDIT"));
            let widget = control(parent, w!("STATIC"));
            let button = control(widget, w!("BUTTON"));
            let other = control(parent, w!("EDIT"));
            let mut pid = 0;
            GetWindowThreadProcessId(parent, Some(&mut pid));
            let host = Host { window: parent.0 as isize, pid };
            let foreground_guard = super::focus_tests::retain_foreground(host);
            let _ = SetFocus(Some(editor));
            assert_eq!(GetFocus(), editor);
            let previous = host_focus(host, Some(widget));
            assert!(previous.is_some());
            for _ in 0..12 {
                let _ = SetFocus(Some(button));
                assert!(host_focus(host, Some(widget)).is_none());
                // Hidden SetFocus can activate the fixture on Windows. Check
                // the actual foreground state instead of assuming it cannot.
                let allowed = host_foreground(host);
                return_host_focus(widget, host, previous);
                assert_eq!(GetFocus(), if allowed { editor } else { button });
                return_focus_within_host(widget, host, previous);
                assert_eq!(GetFocus(), editor);
            }
            let _ = SetFocus(Some(other));
            return_focus_within_host(widget, host, previous);
            assert_eq!(GetFocus(), other);
            DestroyWindow(editor).unwrap();
            let _ = SetFocus(Some(button));
            return_focus_within_host(widget, host, previous);
            assert_eq!(GetFocus(), parent);
            drop(foreground_guard);
            DestroyWindow(parent).unwrap();
        }
    }
}
