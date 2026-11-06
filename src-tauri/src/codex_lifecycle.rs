//! Verified desktop process lifetime. No credentials, command lines or client patches.
use crate::codex_window;
use pool_core::{storage::Store, Result};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use windows::{
    core::PWSTR,
    Win32::{
        Foundation::{CloseHandle, FILETIME, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
        Security::{EqualSid, GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER},
        Storage::Packaging::Appx::GetApplicationUserModelId,
        System::{
            Com::CoTaskMemFree,
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
                TH32CS_SNAPPROCESS,
            },
            RemoteDesktop::ProcessIdToSessionId,
            Threading::{
                GetCurrentProcess, GetProcessTimes, OpenProcess, OpenProcessToken,
                QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject,
                PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
                PROCESS_TERMINATE,
            },
        },
        UI::Shell::{SHGetKnownFolderPath, FOLDERID_LocalAppData, KF_FLAG_DEFAULT},
    },
};

struct Handle(HANDLE);
impl Handle {
    fn exited(&self) -> Result<bool> {
        match unsafe { WaitForSingleObject(self.0, 0) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            _ => Err("无法确认 Codex 进程是否退出".into()),
        }
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
// An exited process can remain enumerable while another component holds a
// handle, even though Windows can no longer supply its image path or token.
// Only a signaled handle proves exit; a failed identity query never does.
fn inspect_live<T>(handle: &Handle, inspect: impl FnOnce() -> Result<T>) -> Result<Option<T>> {
    if handle.exited()? {
        return Ok(None);
    }
    let result = inspect();
    if handle.exited()? {
        return Ok(None);
    }
    result.map(Some)
}
fn inspect_matching_live<T>(
    handle: &Handle,
    expected_created: Option<u64>,
    inspect: impl FnOnce(u64) -> Result<T>,
) -> Result<Option<T>> {
    inspect_live(handle, || {
        let created = times(handle.0)?.0;
        // A reused PID is unrelated. Do not query its path or token before
        // rejecting it: those queries may legitimately be inaccessible.
        if expected_created.is_some_and(|expected| expected != created) {
            return Ok(None);
        }
        inspect(created).map(Some)
    }).map(Option::flatten)
}
fn stamp(t: FILETIME) -> u64 {
    ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64
}
fn times(handle: HANDLE) -> Result<(u64, u64)> {
    let (mut created, mut exited, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) }
        .map_err(|_| "无法核验 Codex 进程创建时间")?;
    Ok((stamp(created), stamp(exited)))
}
fn path(handle: HANDLE) -> Result<PathBuf> {
    let mut text = vec![0u16; 32768];
    let mut length = text.len() as u32;
    unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(text.as_mut_ptr()),
            &mut length,
        )
    }
    .map_err(|_| "无法核验 Codex 进程路径")?;
    Ok(PathBuf::from(String::from_utf16_lossy(
        &text[..length as usize],
    )))
}
fn token_user(handle: HANDLE) -> Result<Vec<usize>> {
    let mut token = HANDLE::default();
    unsafe { OpenProcessToken(handle, TOKEN_QUERY, &mut token) }
        .map_err(|_| "无法核验 Codex 进程用户")?;
    let token = Handle(token);
    let mut length = 0;
    unsafe {
        let _ = GetTokenInformation(token.0, TokenUser, None, 0, &mut length);
    }
    if length == 0 || length > 65536 {
        return Err("客户端用户信息不可用".into());
    }
    let mut buffer = vec![0usize; (length as usize).div_ceil(std::mem::size_of::<usize>())];
    unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            length,
            &mut length,
        )
    }
    .map_err(|_| "无法核验 Codex 进程用户")?;
    Ok(buffer)
}
fn same_user(handle: HANDLE) -> Result<bool> {
    let a = token_user(handle)?;
    let b = token_user(unsafe { GetCurrentProcess() })?;
    Ok(unsafe {
        EqualSid(
            (*(a.as_ptr() as *const TOKEN_USER)).User.Sid,
            (*(b.as_ptr() as *const TOKEN_USER)).User.Sid,
        )
        .is_ok()
    })
}
fn same_session(pid: u32) -> Result<bool> {
    let (mut current, mut other) = (0, 0);
    unsafe {
        ProcessIdToSessionId(std::process::id(), &mut current).map_err(|_| "无法核验当前会话")?;
        ProcessIdToSessionId(pid, &mut other).map_err(|_| "无法核验客户端会话")?;
    }
    Ok(current == other)
}
fn app_id(handle: HANDLE) -> Result<String> {
    let mut text = vec![0u16; 1024];
    let mut length = text.len() as u32;
    if !unsafe { GetApplicationUserModelId(handle, &mut length, Some(PWSTR(text.as_mut_ptr()))) }
        .is_ok()
    {
        return Err("无法核验 Codex 安装包身份".into());
    }
    let id = String::from_utf16_lossy(&text[..length.saturating_sub(1) as usize]);
    if !id.starts_with("OpenAI.Codex_") {
        return Err("客户端安装包身份不匹配".into());
    }
    Ok(id)
}
struct Entry {
    pid: u32,
    parent: u32,
    name: String,
}
fn entries() -> Result<Vec<Entry>> {
    let snapshot = Handle(
        unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
            .map_err(|_| "无法读取客户端进程列表")?,
    );
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    unsafe { Process32FirstW(snapshot.0, &mut entry) }.map_err(|_| "无法读取客户端进程列表")?;
    let mut found = vec![];
    loop {
        let length = entry
            .szExeFile
            .iter()
            .position(|c| *c == 0)
            .unwrap_or(entry.szExeFile.len());
        found.push(Entry {
            pid: entry.th32ProcessID,
            parent: entry.th32ParentProcessID,
            name: String::from_utf16_lossy(&entry.szExeFile[..length]),
        });
        if unsafe { Process32NextW(snapshot.0, &mut entry) }.is_err() {
            break;
        }
    }
    Ok(found)
}
fn engine_path_in(p: &Path, local: &Path) -> bool {
    let base = local
        .join("OpenAI/Codex/bin")
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase()
        + "\\";
    let text = p.to_string_lossy().replace('/', "\\").to_ascii_lowercase();
    text.strip_prefix(&base).is_some_and(|relative| {
        let mut parts = relative.split('\\');
        matches!((parts.next(), parts.next(), parts.next()),
            (Some(directory), Some("codex.exe"), None) if !directory.is_empty() && directory != "." && directory != "..")
    })
}
fn local_app_data() -> Option<PathBuf> {
    // The desktop and Fusion can inherit different LOCALAPPDATA values. Resolve
    // the Windows user's real folder without reading another process's environment.
    let value = unsafe { SHGetKnownFolderPath(&FOLDERID_LocalAppData, KF_FLAG_DEFAULT, None) }.ok()?;
    let text = unsafe { value.to_string() }.ok();
    unsafe { CoTaskMemFree(Some(value.0.cast())) };
    text.map(PathBuf::from)
}
fn engine_path(p: &Path) -> bool {
    use std::sync::OnceLock;
    static LOCAL: OnceLock<Option<PathBuf>> = OnceLock::new();
    LOCAL.get_or_init(local_app_data).as_ref().is_some_and(|local| engine_path_in(p, local))
        || std::env::var_os("LOCALAPPDATA").is_some_and(|local| engine_path_in(p, Path::new(&local)))
}
fn runtime_engine_layout(p: &Path) -> bool {
    // The process API can report an alias/redirected user directory. Require
    // the same managed layout even when Fusion's own user folder differs.
    let text = p.to_string_lossy().replace('/', "\\").to_ascii_lowercase();
    let parts: Vec<_> = text.rsplit('\\').take(5).collect();
    matches!(parts.as_slice(), ["codex.exe", version, "bin", "codex", "openai"]
        if !version.is_empty() && *version != "." && *version != "..")
}
fn bundled_engine_matches(engine: &Path, desktop: &Path) -> bool {
    use std::{io::Read, sync::{Mutex, OnceLock}};
    // A directory mismatch may never authorize an arbitrary executable. Only
    // accept an exact copy of the engine in the already verified MSIX desktop.
    if !runtime_engine_layout(engine) {
        return false;
    }
    let Some(package_dir) = desktop.parent() else { return false };
    let bundled = package_dir.join("resources/codex.exe");
    type FileStamp = (u64, std::time::SystemTime);
    fn stamp(path: &Path) -> Option<FileStamp> {
        let m = std::fs::metadata(path).ok()?;
        m.is_file().then_some((m.len(), m.modified().ok()?))
    }
    let Some(engine_stamp) = stamp(engine) else { return false };
    let Some(bundled_stamp) = stamp(&bundled) else { return false };
    if engine_stamp.0 == 0 || engine_stamp.0 != bundled_stamp.0 {
        return false;
    }
    type CopyStamp = (PathBuf, PathBuf, FileStamp, FileStamp);
    static VERIFIED: OnceLock<Mutex<Option<CopyStamp>>> = OnceLock::new();
    let key = (engine.to_owned(), bundled.clone(), engine_stamp, bundled_stamp);
    let Ok(mut verified) = VERIFIED.get_or_init(Default::default).lock() else { return false };
    if verified.as_ref() == Some(&key) {
        return true;
    }
    let equal = (|| -> Option<bool> {
        let mut actual = std::fs::File::open(engine).ok()?;
        let mut expected = std::fs::File::open(&bundled).ok()?;
        let (mut a, mut b) = ([0u8; 65536], [0u8; 65536]);
        let mut remaining = engine_stamp.0;
        while remaining > 0 {
            let count = remaining.min(a.len() as u64) as usize;
            actual.read_exact(&mut a[..count]).ok()?;
            expected.read_exact(&mut b[..count]).ok()?;
            if a[..count] != b[..count] {
                return Some(false);
            }
            remaining -= count as u64;
        }
        Some(stamp(engine) == Some(engine_stamp) && stamp(&bundled) == Some(bundled_stamp))
    })().unwrap_or(false);
    if equal {
        *verified = Some(key);
    }
    equal
}
fn client_binary_path(p: &Path, desktop: Option<&Path>) -> bool {
    desktop.and_then(Path::parent).is_some_and(|dir| p.starts_with(dir))
        || engine_path(p)
        || desktop.is_some_and(|desktop| bundled_engine_matches(p, desktop))
}
fn owns_lifetime(expected: u64, created: u64, child: u64, exited: Option<u64>) -> bool {
    created == expected && child >= created && exited.is_none_or(|exit| child <= exit)
}
struct Tracked {
    handle: Handle,
    pid: u32,
    created: u64,
    path: PathBuf,
}
impl Tracked {
    fn open_live(pid: u32) -> Result<Option<Self>> {
        Self::open_matching_live(pid, None)
    }
    fn open_matching_live(pid: u32, expected_created: Option<u64>) -> Result<Option<Self>> {
        let handle = Handle(
            unsafe {
                OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                    false,
                    pid,
                )
            }
            .map_err(|_| "无法访问 Codex 进程，请使用相同 Windows 用户运行")?,
        );
        let Some((created, path)) = inspect_matching_live(&handle, expected_created, |created| Ok((created, path(handle.0)?)))? else {
            return Ok(None);
        };
        Ok(Some(Self {
            handle,
            pid,
            created,
            path,
        }))
    }
    #[cfg(test)]
    fn open(pid: u32) -> Result<Self> {
        Self::open_live(pid)?.ok_or_else(|| "测试进程已退出".into())
    }
    fn exited(&self) -> Result<bool> {
        self.handle.exited()
    }
    fn owns(&self, child: &Self) -> Result<bool> {
        // GetProcessTimes explicitly leaves exit time undefined for a live
        // process. Observe the handle first; only use exit time after it exits.
        // The child handle was captured before this observation, so seeing the
        // parent alive proves its lifetime includes the child's creation.
        let has_exited = self.exited()?;
        let (created, exited) = times(self.handle.0)?;
        Ok(owns_lifetime(self.created, created, child.created, has_exited.then_some(exited)))
    }
    fn terminate(&self) -> Result<()> {
        if self.exited()? {
            return Ok(());
        }
        // Reopening with termination rights is allowed only after proving it is
        // still the captured process. Never issue taskkill by executable name.
        let kill = match unsafe {
                OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE | PROCESS_SYNCHRONIZE,
                    false,
                    self.pid,
                )
            } {
            Ok(handle) => Handle(handle),
            Err(_) if self.exited()? => return Ok(()),
            Err(_) => return Err("Codex 后台结束权限不足，切换尚未完成，请使用相同 Windows 用户运行后重试".into()),
        };
        let verified = inspect_live(&kill, || {
            if times(kill.0)?.0 != self.created || path(kill.0)? != self.path || !same_user(kill.0)? {
                return Err("Codex 进程身份已变化，未结束该进程".into());
            }
            Ok(())
        })?;
        if verified.is_none() {
            return Ok(());
        }
        match unsafe { TerminateProcess(kill.0, 0) } {
            Ok(()) => Ok(()),
            Err(_) if self.exited()? => Ok(()),
            Err(_) => Err("无法结束已核验的 Codex 后台，切换尚未完成".into()),
        }
    }
}
struct Client {
    processes: Vec<Tracked>,
    desktop: Option<PathBuf>,
    id: Option<String>,
    engine_rejections: Vec<&'static str>,
}
impl Client {
    fn capture() -> Result<Self> {
        let mut client = Self {
            processes: vec![],
            desktop: None,
            id: None,
            engine_rejections: vec![],
        };
        let snapshot = entries()?;
        for e in snapshot.iter().filter(|e| {
            e.name.eq_ignore_ascii_case("ChatGPT.exe")
                && !snapshot.iter().any(|parent| {
                    parent.pid == e.parent && parent.name.eq_ignore_ascii_case("ChatGPT.exe")
                })
        }) {
            if !same_session(e.pid).unwrap_or(false) {
                continue;
            }
            let p = match Tracked::open_live(e.pid) {
                Ok(Some(p)) => p,
                Ok(None) => continue,
                Err(error) => {
                    if !entries()?.iter().any(|live| live.pid == e.pid) {
                        continue;
                    }
                    return Err(error);
                }
            };
            if p.exited()? {
                continue;
            }
            let Some(id) = inspect_live(&p.handle, || {
                if !codex_window::desktop_path(&p.path) || !same_user(p.handle.0)? {
                    return Ok(None);
                }
                app_id(p.handle.0).map(Some)
            })?.flatten() else { continue };
            if client.desktop.as_ref().is_some_and(|path| *path != p.path) {
                return Err("检测到多个 Codex 安装版本，无法确定重开目标".into());
            }
            client.desktop = Some(p.path.clone());
            client.id = Some(id);
            client.processes.push(p);
        }
        client.refresh()?;
        Ok(client)
    }
    fn refresh(&mut self) -> Result<()> {
        let snapshot = entries()?;
        let mut index = 0;
        while index < self.processes.len() {
            let parent_pid = self.processes[index].pid;
            for e in snapshot.iter().filter(|e| e.parent == parent_pid) {
                if self
                    .processes
                    .iter()
                    .any(|p| p.pid == e.pid && !p.exited().unwrap_or(false))
                {
                    continue;
                }
                // Only client binaries directly descended through verified client
                // binaries qualify. Do not traverse a user's shell/tool process.
                if !e.name.eq_ignore_ascii_case("ChatGPT.exe")
                    && !e.name.eq_ignore_ascii_case("codex.exe")
                    && !e.name.eq_ignore_ascii_case("codex-command-runner.exe")
                {
                    continue;
                }
                let p = match Tracked::open_live(e.pid) {
                    Ok(Some(p)) => p,
                    Ok(None) => continue,
                    Err(error) => {
                        if !entries()?.iter().any(|live| live.pid == e.pid) {
                            continue;
                        }
                        return Err(error);
                    }
                };
                if p.exited()? {
                    continue;
                }
                let Some(rejection) = inspect_live(&p.handle, || Ok(if !same_user(p.handle.0)? {
                    Some("Windows 用户不匹配")
                } else if !same_session(p.pid)? {
                    Some("Windows 会话不匹配")
                } else if !self.processes[index].owns(&p)? {
                    Some("父子进程存续时间不匹配")
                } else if !client_binary_path(&p.path, self.desktop.as_deref()) {
                    Some("运行目录不匹配，且无法确认内核与当前桌面安装包一致")
                } else {
                    None
                }))? else { continue };
                match rejection {
                    Some(reason) if is_engine(&p.path) => {
                        if !self.engine_rejections.contains(&reason) {
                            self.engine_rejections.push(reason);
                        }
                    }
                    Some(_) => {},
                    None => self.processes.push(p),
                }
            }
            index += 1;
        }
        Ok(())
    }
    fn missing_engine_error(&self) -> String {
        let detail = if self.engine_rejections.is_empty() {
            "尚未发现由目标桌面派生的存活内核".into()
        } else {
            format!("已发现内核，但核验未通过：{}", self.engine_rejections.join("、"))
        };
        format!("Codex 内核识别失败：{detail}。账号池切换未完成；请将此提示提供给维护人员")
    }
}
// One source of client identity for activation, compatibility and restart.
// Unrelated npm CLIs, other users and detached engines never qualify.
fn desktop_version(path: &Path) -> Result<String> {
    path.ancestors().filter_map(|p| p.file_name())
        .filter_map(|p| p.to_str())
        .find_map(|s| s.strip_prefix("OpenAI.Codex_").and_then(|s| s.split('_').next()))
        .filter(|s| s.split('.').count() == 4 && s.split('.').all(|v| !v.is_empty() && v.bytes().all(|c| c.is_ascii_digit())))
        .map(str::to_owned).ok_or_else(|| "无法读取实际运行的 Codex 安装版本".into())
}
fn is_engine(path: &Path) -> bool {
    path.file_name().is_some_and(|v| v.eq_ignore_ascii_case("codex.exe"))
}
fn resolve_identity() -> Result<pool_core::compatibility::ClientIdentity> {
    let client = Client::capture()?;
    let desktop = client.desktop.as_ref().ok_or("Codex 尚未打开，启动账号池时将自动打开客户端")?;
    let mut paths: Vec<_> = client.processes.iter()
        .filter(|p| is_engine(&p.path) && !p.exited().unwrap_or(true))
        .map(|p| p.path.clone()).collect();
    paths.sort();
    paths.dedup();
    match paths.as_slice() {
        [path] => pool_core::compatibility::ClientIdentity::inspect(desktop_version(desktop)?, desktop, path.clone()),
        [] => Err(client.missing_engine_error()),
        _ => Err("同一桌面检测到多个运行内核，无法安全选择；请完全退出多余客户端后重试".into()),
    }
}
pub fn running_identity(refresh: bool) -> Result<pool_core::compatibility::ClientIdentity> {
    use std::sync::{Mutex, OnceLock};
    type Cache = Option<(Instant, Result<pool_core::compatibility::ClientIdentity>)>;
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let mut cache = CACHE.get_or_init(Default::default).lock().map_err(|_| "客户端检测暂不可用")?;
    if let Some((at, result)) = cache.as_ref() {
        let ttl = if result.is_ok() { Duration::from_secs(30) } else { Duration::from_millis(500) };
        if !refresh && at.elapsed() < ttl { return result.clone(); }
    }
    let result = resolve_identity();
    *cache = Some((Instant::now(), result.clone()));
    result
}
pub fn prepare_identity(store: &Store) -> Result<pool_core::compatibility::ClientIdentity> {
    // Only an absent desktop permits install discovery. A live but unverified
    // client must not be disguised as a cold start or replaced with another CLI.
    if Client::capture()?.desktop.is_none() {
        let installed = codex_window::installed_client()?;
        return pool_core::compatibility::inspect_bundled(
            store, installed.version.clone(), &installed.desktop, installed.engine(),
        );
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match running_identity(true) {
            Ok(identity) => {
                pool_core::compatibility::ensure_verified(store, &identity)?;
                return Ok(identity);
            }
            Err(error) if Instant::now() >= deadline => return Err(error),
            Err(_) => std::thread::sleep(Duration::from_millis(250)),
        }
    }
}
#[derive(Serialize, Deserialize)]
struct RestartRecord {
    version: u32,
    desktop: Option<PathBuf>,
    id: Option<String>,
    processes: Vec<ProcessIdentity>,
}
#[derive(Serialize, Deserialize)]
struct ProcessIdentity {
    pid: u32,
    created: u64,
    path: PathBuf,
}
impl Client {
    fn persist(&self, store: &Store) -> Result<()> {
        let record = RestartRecord {
            version: 1,
            desktop: self.desktop.clone(),
            id: self.id.clone(),
            processes: self
                .processes
                .iter()
                .map(|p| ProcessIdentity {
                    pid: p.pid,
                    created: p.created,
                    path: p.path.clone(),
                })
                .collect(),
        };
        store.set(
            "codex_restart_context",
            &serde_json::to_string(&record).map_err(|_| "无法保存客户端重开状态")?,
        )
    }
    fn restore_pending(&mut self, store: &Store) -> Result<()> {
        let Some(raw) = store
            .get("codex_restart_context")?
            .filter(|s| !s.is_empty())
        else {
            return Ok(());
        };
        let record: RestartRecord =
            serde_json::from_str(&raw).map_err(|_| "客户端重开记录损坏，未结束任何进程")?;
        if record.version != 1
            || record.processes.len() > 512
            || record
                .desktop
                .as_ref()
                .is_some_and(|p| !codex_window::desktop_path(p))
            || record.id.as_ref().is_some_and(|id| {
                !id.starts_with("OpenAI.Codex_")
                    || !id
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "_.!-".contains(c))
            })
        {
            return Err("客户端重开记录版本或安装身份不可用，未结束任何进程".into());
        }
        // A persisted path is historical, not evidence of a live installation.
        // Resolve exact surviving identities before comparing versions: shutdown,
        // a client update, or PID reuse must not pin retries to yesterday's path.
        let snapshot = entries()?;
        let mut survivors = vec![];
        for saved in record.processes {
            if !snapshot.iter().any(|p| p.pid == saved.pid) {
                continue;
            }
            let p = match Tracked::open_matching_live(saved.pid, Some(saved.created)) {
                Ok(Some(p)) => p,
                Ok(None) => continue,
                Err(_) if !entries()?.iter().any(|p| p.pid == saved.pid) => continue,
                Err(error) => return Err(error),
            };
            if p.created != saved.created || p.path != saved.path || p.exited()? {
                continue;
            }
            survivors.push(p);
        }
        if survivors.is_empty() {
            // With no current desktop, launch resolves today's registered package.
            return self.refresh();
        }
        if self.desktop.is_some() && record.desktop.is_some() && self.desktop != record.desktop {
            return Err("旧版本 Codex 仍有存活进程，同时检测到新版本；请退出旧版本后重试，未结束其他实例".into());
        }
        if self.desktop.is_none() {
            self.desktop = record.desktop;
            self.id = record.id;
        }
        for p in survivors {
            let verified = inspect_live(&p.handle, || {
                if !same_user(p.handle.0)?
                || !same_session(p.pid)?
                || !client_binary_path(&p.path, self.desktop.as_deref())
                {
                    return Err("残留客户端进程身份不匹配，未结束该进程".into());
                }
                Ok(())
            })?;
            if verified.is_none() { continue; }
            if !self.processes.iter().any(|live| live.pid == p.pid && live.created == p.created) {
                self.processes.push(p);
            }
        }
        self.refresh()
    }
}
struct PendingClient<'a> {
    client: &'a mut Client,
    store: &'a Store,
}
impl ExitControl for PendingClient<'_> {
    fn refresh(&mut self) -> Result<()> {
        self.client.refresh()?;
        self.client.persist(self.store)
    }
    fn exited(&self) -> Result<bool> {
        self.client.exited()
    }
    fn terminate(&self) -> Result<()> {
        self.client.terminate()
    }
}
trait ExitControl {
    fn refresh(&mut self) -> Result<()>;
    fn exited(&self) -> Result<bool>;
    fn terminate(&self) -> Result<()>;
}
impl ExitControl for Client {
    fn refresh(&mut self) -> Result<()> {
        self.refresh()
    }
    fn exited(&self) -> Result<bool> {
        for p in &self.processes {
            if !p.exited()? {
                return Ok(false);
            }
        }
        Ok(true)
    }
    fn terminate(&self) -> Result<()> {
        for p in self.processes.iter().rev() {
            p.terminate()?;
        }
        Ok(())
    }
}
fn finish_exit(
    client: &mut impl ExitControl,
    graceful: Duration,
    forced: Duration,
    progress: impl Fn(&'static str),
) -> Result<()> {
    progress("quitting");
    let deadline = Instant::now() + graceful;
    loop {
        client.refresh()?;
        if client.exited()? {
            client.refresh()?;
            if client.exited()? {
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(50).min(graceful));
    }
    progress("terminating");
    let deadline = Instant::now() + forced;
    loop {
        client.refresh()?;
        client.terminate()?;
        if client.exited()? {
            client.refresh()?;
            if client.exited()? {
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            return Err("Codex 后台仍未退出，未启动其他实例，请重试".into());
        }
        std::thread::sleep(Duration::from_millis(50).min(forced));
    }
}
pub struct LaunchTarget {
    desktop: Option<PathBuf>,
    id: Option<String>,
    old: Vec<(u32, u64)>,
}
pub fn stop_client(store: &Store, progress: impl Fn(&'static str)) -> Result<LaunchTarget> {
    let mut client = Client::capture()?;
    client.restore_pending(store)?;
    client.persist(store)?;
    // A missing/hidden window never prevents the verified timeout fallback.
    if let Some(host) = codex_window::find_any() {
        if client.processes.iter().any(|p| p.pid == host.pid) {
            let _ = codex_window::request_normal_quit(host);
        }
    }
    finish_exit(
        &mut PendingClient {
            client: &mut client,
            store,
        },
        Duration::from_secs(30),
        Duration::from_secs(10),
        &progress,
    )?;
    // A new untracked client appearing during Quit is not silently ignored.
    if !Client::capture()?.exited()? {
        return Err("退出期间出现新的 Codex 进程，未重复启动，请重试".into());
    }
    Ok(LaunchTarget {
        desktop: client.desktop,
        id: client.id,
        old: client
            .processes
            .iter()
            .map(|p| (p.pid, p.created))
            .collect(),
    })
}
pub fn launch(mut target: LaunchTarget, prepared: &pool_core::compatibility::ClientIdentity, store: &Store, progress: impl Fn(&'static str)) -> Result<()> {
    use std::{
        os::windows::process::CommandExt,
        process::{Command, Stdio},
    };
    if target.desktop.is_none() || target.id.is_none() {
        let installed = codex_window::installed_client()?;
        target.desktop = Some(installed.desktop);
        target.id = Some(installed.id);
    }
    progress("launching");
    if let Some(id) = target.id {
        Command::new(codex_window::explorer_path()?)
            .arg(format!("shell:AppsFolder\\{id}"))
            .creation_flags(0x08000000)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| codex_window::launch_error("Codex 退出后重新启动", &error))?;
    }
    progress("verifying");
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut found = None;
    while Instant::now() < deadline {
        if let Some(host) = codex_window::find() {
            let Some(p) = Tracked::open_live(host.pid)? else {
                std::thread::sleep(Duration::from_millis(200));
                continue;
            };
            if !target.old.contains(&(p.pid, p.created))
                && target
                    .desktop
                    .as_ref()
                    .is_none_or(|expected| *expected == p.path)
                && same_user(p.handle.0)?
                && same_session(p.pid)?
                && app_id(p.handle.0).is_ok()
            {
                found = Some(p.pid);
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let pid = found.ok_or("Codex 已请求启动，但未检测到新的客户端窗口，请重试")?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let mut client = Client::capture()?;
        // The new engine must descend from the new desktop, not another CLI.
        client.processes.retain(|p| p.pid == pid);
        client.refresh()?;
        if client
            .processes
            .iter()
            .any(|p| is_engine(&p.path) && !p.exited().unwrap_or(true))
        {
            let identity = running_identity(true)?;
            pool_core::compatibility::ensure_verified_after_launch(
                store, &identity, prepared, client.desktop.as_deref().ok_or("无法复核新客户端安装路径")?,
            )?;
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(client.missing_engine_error());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    #[test]
    #[ignore = "Read-only running desktop identity; isolated temporary store and public fixture"]
    fn actual_running_client_compatibility() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("pool.sqlite")).unwrap();
        let identity = running_identity(true).unwrap();
        let client = Client::capture().unwrap();
        assert!(bundled_engine_matches(&identity.engine_path, client.desktop.as_deref().unwrap()),
            "running managed engine must match this desktop's bundled engine");
        pool_core::compatibility::ensure_verified(&store, &identity).unwrap();
        println!("{}", serde_json::to_string(&identity.client).unwrap());
    }
    #[test]
    fn package_version_is_taken_from_running_desktop_on_any_volume() {
        assert_eq!(desktop_version(Path::new(r"D:\WindowsApps\OpenAI.Codex_26.1027.3384.0_x64__fixture\app\ChatGPT.exe")).unwrap(), "26.1027.3384.0");
        assert!(desktop_version(Path::new(r"C:\standalone\ChatGPT.exe")).is_err());
        assert!(desktop_version(Path::new(r"C:\WindowsApps\OpenAI.Codex_invalid_x64__fixture\app\ChatGPT.exe")).is_err());
        assert!(is_engine(Path::new(r"D:\WindowsApps\OpenAI.Codex_26.1027.3384.0_x64__fixture\app\resources\codex.exe")));
        assert!(!is_engine(Path::new(r"D:\tools\codex-command-runner.exe")));
    }
    #[test]
    fn win11_runtime_layout_accepts_both_hashes_and_rejects_unrelated_paths() {
        let local = Path::new(r"C:\Users\fixture\AppData\Local");
        for hash in ["22cfb1d302613eae", "08d187884f2330d1"] {
            assert!(engine_path_in(&local.join(format!("OpenAI/Codex/bin/{hash}/codex.exe")), local));
        }
        for relative in [
            "OpenAI/Codex/bin/../codex.exe",
            "OpenAI/Codex/bin/codex.exe",
            "OpenAI/Codex/bin/hash/nested/codex.exe",
            "OpenAI/Codex/bin/hash/codex-command-runner.exe",
            "OpenAI/Codex/bin-other/hash/codex.exe",
            "npm/node_modules/codex.exe",
        ] {
            assert!(!engine_path_in(&local.join(relative), local), "{relative}");
        }
        assert!(!engine_path_in(Path::new(r"C:\Users\other\AppData\Local\OpenAI\Codex\bin\hash\codex.exe"), local));
    }
    #[test]
    fn mismatched_user_folder_requires_exact_engine_from_the_verified_desktop() {
        let root = tempfile::tempdir().unwrap();
        let desktop = root.path().join("package/app/ChatGPT.exe");
        let bundled = desktop.parent().unwrap().join("resources/codex.exe");
        let engine = root.path().join("redirected/OpenAI/Codex/bin/22cfb1d302613eae/codex.exe");
        for file in [&bundled, &engine] {
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        }
        let bytes: Vec<_> = (0..65539).map(|i| (i % 251) as u8).collect();
        std::fs::write(&bundled, &bytes).unwrap();
        std::fs::write(&engine, &bytes).unwrap();
        // Reproduce rejection when Fusion resolves a different local folder.
        assert!(!engine_path_in(&engine, &root.path().join("fusion-local")));
        assert!(bundled_engine_matches(&engine, &desktop));
        assert!(client_binary_path(&engine, Some(&desktop)));
        assert!(!client_binary_path(&engine, None));
        // Same size and same executable name are insufficient; every byte is
        // checked. A metadata change also invalidates the successful cache.
        let mut changed = bytes.clone();
        changed[65538] ^= 1;
        std::fs::write(&engine, changed).unwrap();
        std::fs::File::options().write(true).open(&engine).unwrap()
            .set_modified(std::time::SystemTime::now() + Duration::from_secs(2)).unwrap();
        assert!(!bundled_engine_matches(&engine, &desktop));
        assert!(!client_binary_path(&engine, Some(&desktop)));
        std::fs::remove_file(&bundled).unwrap();
        assert!(!bundled_engine_matches(&engine, &desktop));
    }
    #[test]
    fn identical_binary_in_unmanaged_layout_cannot_bypass_directory_checks() {
        let root = tempfile::tempdir().unwrap();
        let desktop = root.path().join("package/app/ChatGPT.exe");
        let bundled = desktop.parent().unwrap().join("resources/codex.exe");
        std::fs::create_dir_all(bundled.parent().unwrap()).unwrap();
        std::fs::write(&bundled, b"fixture engine").unwrap();
        for relative in [
            "npm/codex.exe", "OpenAI/Codex/bin/hash/nested/codex.exe",
            "OpenAI/Codex/bin/hash/codex-command-runner.exe", "OpenAI/Codex/bin-other/hash/codex.exe",
        ] {
            let engine = root.path().join(relative);
            std::fs::create_dir_all(engine.parent().unwrap()).unwrap();
            std::fs::write(&engine, b"fixture engine").unwrap();
            assert!(!bundled_engine_matches(&engine, &desktop), "{relative}");
            assert!(!client_binary_path(&engine, Some(&desktop)), "{relative}");
        }
        for path in [r"\\?\C:\Users\fixture\AppData\Local\OpenAI\Codex\bin\hash\codex.exe",
            r"C:\Users\FIXTUR~1\AppData\Local\OpenAI\Codex\bin\hash\CODEX.EXE",
            r"D:\redirected\OpenAI\Codex\bin\hash\codex.exe"] {
            assert!(runtime_engine_layout(Path::new(path)));
        }
    }
    #[test]
    fn parent_lifetime_uses_exit_time_only_after_handle_signals_exit() {
        assert!(owns_lifetime(10, 10, 20, None));
        assert!(owns_lifetime(10, 10, 20, Some(30)));
        assert!(!owns_lifetime(10, 10, 20, Some(15)));
        assert!(!owns_lifetime(10, 10, 9, None));
        assert!(!owns_lifetime(10, 11, 20, None));
    }
    #[test]
    fn engine_failure_distinguishes_missing_process_from_rejected_identity() {
        let mut client = Client { processes: vec![], desktop: None, id: None, engine_rejections: vec![] };
        assert!(client.missing_engine_error().contains("尚未发现"));
        client.engine_rejections.push("父子进程存续时间不匹配");
        let error = client.missing_engine_error();
        assert!(error.contains("已发现内核"));
        assert!(error.contains("父子进程存续时间不匹配"));
        assert!(!error.contains("请等待"));
    }
    struct Fake {
        alive: Cell<bool>,
        denied: bool,
        terminate_calls: Cell<usize>,
        refresh_calls: usize,
        graceful_exit: bool,
    }
    impl ExitControl for Fake {
        fn refresh(&mut self) -> Result<()> {
            self.refresh_calls += 1;
            if self.graceful_exit {
                self.alive.set(false);
            }
            Ok(())
        }
        fn exited(&self) -> Result<bool> {
            Ok(!self.alive.get())
        }
        fn terminate(&self) -> Result<()> {
            self.terminate_calls.set(self.terminate_calls.get() + 1);
            if self.denied {
                Err("fixture permission denied".into())
            } else {
                self.alive.set(false);
                Ok(())
            }
        }
    }
    #[test]
    fn graceful_exit_never_forces_and_background_timeout_forces_before_success() {
        for graceful in [true, false] {
            let mut fake = Fake {
                alive: Cell::new(true),
                denied: false,
                terminate_calls: Cell::new(0),
                refresh_calls: 0,
                graceful_exit: graceful,
            };
            let phases = std::cell::RefCell::new(vec![]);
            finish_exit(
                &mut fake,
                Duration::from_millis(5),
                Duration::from_millis(5),
                |p| phases.borrow_mut().push(p),
            )
            .unwrap();
            assert!(!fake.alive.get());
            assert_eq!(fake.terminate_calls.get(), usize::from(!graceful));
            assert_eq!(phases.borrow().contains(&"terminating"), !graceful);
            assert!(phases.borrow().iter().all(|phase| matches!(*phase, "quitting" | "terminating")));
        }
    }
    #[test]
    fn permission_failure_is_not_a_completed_exit() {
        let mut fake = Fake {
            alive: Cell::new(true),
            denied: true,
            terminate_calls: Cell::new(0),
            refresh_calls: 0,
            graceful_exit: false,
        };
        assert!(finish_exit(&mut fake, Duration::ZERO, Duration::from_millis(5), |_| {}).is_err());
        assert!(fake.alive.get());
    }
    #[test]
    fn owned_temporary_process_can_be_terminated_without_touching_other_processes() {
        use std::{
            os::windows::process::CommandExt,
            process::{Command, Stdio},
        };
        let temp = tempfile::tempdir().unwrap();
        let mut child = Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Start-Sleep -Seconds 60",
            ])
            .current_dir(temp.path())
            .env("CODEX_HOME", temp.path())
            .env("LOCALAPPDATA", temp.path())
            .creation_flags(0x08000000)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let owned = Tracked::open(child.id()).unwrap();
        assert!(same_user(owned.handle.0).unwrap());
        assert!(Tracked::open(std::process::id()).unwrap().owns(&owned).unwrap());
        owned.terminate().unwrap();
        child.wait().unwrap();
        assert!(owned.exited().unwrap());
        assert!(!engine_path(&owned.path));
    }
    #[test]
    fn reused_pid_is_rejected_before_querying_inaccessible_identity() {
        let live = Tracked::open(std::process::id()).unwrap();
        assert!(inspect_matching_live::<()>(&live.handle, Some(live.created + 1), |_| {
            panic!("reused PID must not query path or token")
        }).unwrap().is_none());
        let error = "fixture: identity inaccessible";
        assert_eq!(inspect_matching_live(&live.handle, Some(live.created), |_| {
            Err::<(), _>(error.into())
        }).unwrap_err(), error);
        assert!(Tracked::open_matching_live(live.pid, Some(live.created + 1)).unwrap().is_none());
        assert!(!live.exited().unwrap());
    }
    #[test]
    fn exited_process_with_retained_handle_does_not_require_an_image_path() {
        use std::{os::windows::process::CommandExt, process::{Command, Stdio}};
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("pool.sqlite")).unwrap();
        let mut child = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", "Start-Sleep -Seconds 60"])
            .current_dir(temp.path()).env("CODEX_HOME", temp.path()).env("LOCALAPPDATA", temp.path())
            .creation_flags(0x08000000).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .spawn().unwrap();
        let owned = Tracked::open(child.id()).unwrap();
        let record = RestartRecord { version: 1, desktop: None, id: None,
            processes: vec![ProcessIdentity { pid: owned.pid, created: owned.created, path: owned.path.clone() }] };
        let raw = serde_json::to_string(&record).unwrap();
        store.set("codex_restart_context", &raw).unwrap();
        owned.terminate().unwrap();
        child.wait().unwrap();
        assert!(owned.exited().unwrap());
        // Keep both handles alive, as desktop integrations can. Reopening this
        // exited PID must succeed as an exit observation without querying path.
        assert!(Tracked::open_live(owned.pid).unwrap().is_none());
        let inspected = Cell::new(false);
        assert!(inspect_live(&owned.handle, || {
            inspected.set(true);
            Err::<(), _>("无法核验 Codex 进程路径".into())
        }).unwrap().is_none());
        assert!(!inspected.get());
        let mut client = Client { processes: vec![], desktop: None, id: None, engine_rejections: vec![] };
        client.restore_pending(&store).unwrap();
        assert!(client.processes.is_empty());
        assert_eq!(store.get("codex_restart_context").unwrap().as_deref(), Some(raw.as_str()));
        owned.terminate().unwrap();
    }
    #[test]
    fn identity_query_failure_is_ignored_only_when_the_same_handle_has_exited() {
        use std::{os::windows::process::CommandExt, process::{Command, Stdio}};
        let temp = tempfile::tempdir().unwrap();
        let mut child = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", "Start-Sleep -Seconds 60"])
            .current_dir(temp.path()).env("CODEX_HOME", temp.path()).env("LOCALAPPDATA", temp.path())
            .creation_flags(0x08000000).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .spawn().unwrap();
        let owned = Tracked::open(child.id()).unwrap();
        let error = "无法核验 Codex 进程路径";
        assert_eq!(inspect_live(&owned.handle, || Err::<(), _>(error.into())).unwrap_err(), error);
        assert!(!owned.exited().unwrap());
        let result = inspect_live(&owned.handle, || {
            owned.terminate()?;
            child.wait().unwrap();
            Err::<(), _>(error.into())
        });
        assert!(result.unwrap().is_none());
    }
    #[test]
    fn stale_restart_path_does_not_override_current_installation_or_block_pid_reuse() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("pool.sqlite")).unwrap();
        let old = PathBuf::from(r"C:\WindowsApps\OpenAI.Codex_26.1027.3384.0_x64__fixture\app\ChatGPT.exe");
        let current = PathBuf::from(r"C:\WindowsApps\OpenAI.Codex_26.1027.6604.0_x64__fixture\app\ChatGPT.exe");
        let live = Tracked::open(std::process::id()).unwrap();
        for processes in [vec![], vec![ProcessIdentity { pid: live.pid, created: live.created + 1, path: live.path.clone() }]] {
            let record = RestartRecord { version: 1, desktop: Some(old.clone()), id: Some("OpenAI.Codex_fixture!App".into()), processes };
            let raw = serde_json::to_string(&record).unwrap();
            store.set("codex_restart_context", &raw).unwrap();
            for desktop in [None, Some(current.clone())] {
                let mut client = Client { processes: vec![], desktop: desktop.clone(), id: None, engine_rejections: vec![] };
                client.restore_pending(&store).unwrap();
                assert_eq!(client.desktop, desktop);
                assert!(client.processes.is_empty());
            }
            assert_eq!(store.get("codex_restart_context").unwrap().as_deref(), Some(raw.as_str()));
        }
        let record = RestartRecord { version: 1, desktop: Some(old), id: Some("OpenAI.Codex_fixture!App".into()), processes: vec![ProcessIdentity { pid: live.pid, created: live.created, path: live.path.clone() }] };
        store.set("codex_restart_context", &serde_json::to_string(&record).unwrap()).unwrap();
        let mut client = Client { processes: vec![], desktop: Some(current), id: None, engine_rejections: vec![] };
        assert!(client.restore_pending(&store).unwrap_err().contains("仍有存活进程"));
        assert!(!live.exited().unwrap());
        assert!(client.processes.is_empty());
    }
    #[test]
    fn invalid_or_forward_restart_record_is_preserved_without_process_actions() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("pool.sqlite")).unwrap();
        for record in [
            "invalid",
            r#"{"version":2,"desktop":null,"id":null,"processes":[]}"#,
        ] {
            store.set("codex_restart_context", record).unwrap();
            let mut client = Client {
                processes: vec![],
                desktop: None,
                id: None,
                engine_rejections: vec![],
            };
            assert!(client.restore_pending(&store).is_err());
            assert!(client.processes.is_empty());
            assert_eq!(
                store.get("codex_restart_context").unwrap().as_deref(),
                Some(record)
            );
        }
    }
    #[test]
    fn no_running_client_finishes_exit_without_forced_termination() {
        let mut fake = Fake {
            alive: Cell::new(false),
            denied: true,
            terminate_calls: Cell::new(0),
            refresh_calls: 0,
            graceful_exit: false,
        };
        finish_exit(&mut fake, Duration::ZERO, Duration::ZERO, |_| {}).unwrap();
        assert_eq!(fake.terminate_calls.get(), 0);
    }
    #[test]
    fn retry_rejects_reused_pid_and_unrelated_process_without_terminating_it() {
        use std::{os::windows::process::CommandExt, process::{Command, Stdio}};
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("pool.sqlite")).unwrap();
        let mut child = Command::new("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", "Start-Sleep -Seconds 60"])
            .current_dir(temp.path()).env("CODEX_HOME",temp.path()).env("LOCALAPPDATA",temp.path())
            .creation_flags(0x08000000).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        let owned = Tracked::open(child.id()).unwrap();
        let mut record = RestartRecord {version:1,desktop:None,id:None,processes:vec![ProcessIdentity {pid:owned.pid,created:owned.created+1,path:owned.path.clone()}]};
        store.set("codex_restart_context", &serde_json::to_string(&record).unwrap()).unwrap();
        // Model PID reuse with a different creation time. The saved PID must not
        // authorize termination of the currently live process at that PID.
        let mut client = Client {processes:vec![],desktop:None,id:None,engine_rejections:vec![]};
        client.restore_pending(&store).unwrap();
        assert!(client.processes.is_empty());
        assert!(!owned.exited().unwrap());
        record.processes[0].created=owned.created;
        store.set("codex_restart_context", &serde_json::to_string(&record).unwrap()).unwrap();
        assert!(client.restore_pending(&store).is_err());
        assert!(!owned.exited().unwrap());
        owned.terminate().unwrap(); child.wait().unwrap();
    }
}
