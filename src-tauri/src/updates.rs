//! Backend-owned, signed Windows updates. No account data or authorization headers
//! enter the update transport. Installer launch is strictly after safe shutdown.
use crate::{blocking, close_connection, codex_lifecycle, config, App, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tauri::Manager;
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_updater::{Update, UpdaterExt};
use tokio::sync::{OwnedRwLockReadGuard, RwLock};

const REPOSITORY: &str = "https://github.com/Echolzt17/Fusion";
const MAX_PACKAGE: u64 = 512 * 1024 * 1024;
const PERIOD: Duration = Duration::from_secs(6 * 60 * 60);
const STABLE_FEED: &str = "https://raw.githubusercontent.com/Echolzt17/Fusion/main/updates/stable.json";
mod transport;
#[cfg(test)]
use transport::download_to_file;
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    current_version: &'static str,
    phase: &'static str,
    version: Option<String>,
    notes: Option<String>,
    downloaded: u64,
    total: Option<u64>,
    message: Option<String>,
    installing: bool,
    operation_active: bool,
    startup_prompt_pending: bool,
    resume_message: Option<String>,
    #[serde(skip)]
    candidate: Option<Value>,
}
pub struct Updates {
    root: PathBuf,
    pub installing: AtomicBool,
    requested: AtomicBool,
    prompt_dismissed: AtomicBool,
    work: Arc<RwLock<()>>,
    operation: Arc<tokio::sync::Mutex<()>>,
    status: Mutex<Status>,
    cancellation: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}
#[derive(Clone, Deserialize, Serialize)]
struct Resume {
    from: String,
    to: String,
    enabled: bool,
    claimed: bool,
    directory: PathBuf,
}

impl Updates {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            installing: AtomicBool::new(false),
            requested: AtomicBool::new(false),
            prompt_dismissed: AtomicBool::new(false),
            work: Arc::new(RwLock::new(())),
            operation: Arc::new(tokio::sync::Mutex::new(())),
            cancellation: Mutex::new(None),
            status: Mutex::new(Status {
                current_version: env!("CARGO_PKG_VERSION"),
                phase: "idle",
                version: None,
                notes: None,
                downloaded: 0,
                total: None,
                message: None,
                installing: false,
                operation_active: false,
                startup_prompt_pending: false,
                resume_message: None,
                candidate: None,
            }),
        }
    }
    pub fn enter_work(&self) -> Result<OwnedRwLockReadGuard<()>> {
        if self.installing.load(Ordering::SeqCst) {
            return Err("正在安全更新，请等待完成后再操作".into());
        }
        self.work
            .clone()
            .try_read_owned()
            .map_err(|_| "正在安全更新，请稍后再操作".into())
    }
    pub async fn freeze_work(&self) -> tokio::sync::OwnedRwLockWriteGuard<()> {
        self.work.clone().write_owned().await
    }
    fn phase(&self, phase: &'static str) {
        let mut status = self.status.lock().unwrap();
        status.phase = phase;
        status.downloaded = 0;
        status.total = None;
    }
    fn fail(&self, error: String) {
        let mut status = self.status.lock().unwrap();
        status.phase = "failed";
        status.message = Some(error);
    }
    fn updater(&self, app: &tauri::AppHandle, manifest: Value) -> Result<Update> {
        validate_manifest(&manifest)?;
        app.updater_builder()
            .version_comparator(|_, _| true)
            .build()
            .map_err(|_| "更新校验器初始化失败")?
            .restore_update(manifest)
            .map_err(|_| "更新清单或签名格式无效")?
            .ok_or("更新清单不可用".into())
    }
    async fn manifest(&self, url: &str) -> Result<Value> {
        transport::manifest(&client()?, &transport_url(url)?).await
    }
    fn package_dir(&self, manifest: &Value) -> PathBuf {
        self.root.join(manifest["version"].as_str().unwrap())
    }
    async fn package(
        &self,
        app: &tauri::AppHandle,
        manifest: Value,
        progress: bool,
    ) -> Result<PathBuf> {
        let update = self.updater(app, manifest.clone())?;
        let directory = self.package_dir(&manifest);
        safe_directory(&self.root, &directory)?;
        let path = directory.join("setup.exe");
        if path.is_file() {
            let check_path = path.clone();
            let check = update.clone();
            if blocking(move || {
                let bytes = read_package(&check_path)?;
                check.verify(&bytes).map_err(|_| "缓存签名无效".into())
            })
            .await
            .is_ok()
            {
                atomic_json(&directory.join("manifest.json"), &manifest)?;
                return Ok(path);
            }
        }
        let temporary = transport::package(&client()?, &transport_url(update.download_url.as_str())?, &directory, |downloaded, total| {
            if progress {
                let mut state = self.status.lock().unwrap();
                state.downloaded = downloaded;
                state.total = total;
            }
        })
        .await?;
        let verify_path = temporary.path().to_owned();
        blocking(move || {
            let bytes = read_package(&verify_path)?;
            update
                .verify(&bytes)
                .map_err(|_| "更新包签名或版本校验失败，未安装".into())
        })
        .await?;
        temporary
            .persist(&path)
            .map_err(|_| "无法保存已校验的更新包")?;
        atomic_json(&directory.join("manifest.json"), &manifest)?;
        Ok(path)
    }
    // Called under operation: checks never fetch an installer or acquire lifecycle/work locks.
    async fn check(&self, startup: bool) -> Result<Option<Value>> {
        self.phase("checking");
        {
            let mut s = self.status.lock().unwrap();
            s.message = None;
            s.downloaded = 0;
            s.total = None;
        }
        let manifest = self.manifest(STABLE_FEED).await?;
        if !self.apply_manifest(&manifest, startup) { return Ok(None); }
        self.prune_cache(manifest["version"].as_str().unwrap());
        Ok(Some(manifest))
    }
    fn apply_manifest(&self, manifest: &Value, startup: bool) -> bool {
        let version = manifest["version"].as_str().unwrap();
        let available = semver::Version::parse(version).unwrap()
            > semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
        let mut state = self.status.lock().unwrap();
        state.message = None;
        if available {
            state.candidate = Some(manifest.clone());
            state.version = Some(version.to_owned());
            state.notes = manifest["notes"].as_str().map(str::to_owned);
            state.phase = "available";
            if startup && !self.prompt_dismissed.load(Ordering::SeqCst) {
                state.startup_prompt_pending = true;
            }
        } else {
            state.candidate = None;
            state.phase = "current";
            state.version = None;
            state.notes = None;
            state.startup_prompt_pending = false;
        }
        available
    }
    fn dismiss_prompt(&self) {
        self.prompt_dismissed.store(true, Ordering::SeqCst);
        self.status.lock().unwrap().startup_prompt_pending = false;
    }
    fn confirmed_candidate(&self, version: &str) -> Result<Value> {
        let state = self.status.lock().unwrap();
        if !matches!(state.phase, "available" | "failed") || state.version.as_deref() != Some(version) {
            return Err("更新信息已变化，请先检查并确认要安装的版本".into());
        }
        state.candidate.clone().ok_or("请先检查更新并确认版本".into())
    }
    fn defer(&self) -> Result<()> {
        // Serialize cancellation with the point of no return. Once lifecycle
        // changes begin, hiding the dialog cannot claim the update was deferred.
        let mut cancellation = self.cancellation.lock().unwrap();
        if self.installing.load(Ordering::SeqCst) {
            return Err("正在完成安装前的安全收尾，暂时无法取消".into());
        }
        if let Some(sender) = cancellation.take() { let _ = sender.send(()); }
        self.dismiss_prompt();
        Ok(())
    }
    fn cancelled(&self) {
        let mut state = self.status.lock().unwrap();
        state.phase = if state.version.is_some() { "available" } else { "idle" };
        state.downloaded = 0;
        state.total = None;
        state.message = Some("已取消本次更新，可稍后重新检查".into());
    }
    async fn prepare_or_cancel<T>(&self, cancel: &mut tokio::sync::oneshot::Receiver<()>, task: impl std::future::Future<Output = Result<T>>) -> Result<Option<T>> {
        tokio::select! {
            biased;
            _ = cancel => { self.cancelled(); Ok(None) }
            result = task => result.map(Some),
        }
    }
    fn begin_install(&self, cancel: &mut tokio::sync::oneshot::Receiver<()>) -> bool {
        let mut cancellation = self.cancellation.lock().unwrap();
        if !matches!(cancel.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)) {
            self.cancelled(); return false;
        }
        self.installing.store(true, Ordering::SeqCst);
        cancellation.take();
        true
    }
    pub fn start(app: tauri::AppHandle) {
        tauri::async_runtime::spawn(async move {
            let updates = app.state::<Arc<Updates>>();
            // Fusion updates never enable the pool or activate Codex. Even
            // legacy records requesting automatic resume are only retired.
            if let Err(error) = updates.finish_update_record() {
                updates.status.lock().unwrap().resume_message = Some(error);
            }
            let mut startup = true;
            loop {
                {
                    let _operation = updates.operation.lock().await;
                    if !updates.requested.load(Ordering::SeqCst) {
                        if let Err(error) = updates.check(startup).await { updates.fail(error); }
                    }
                    startup = false;
                }
                tokio::time::sleep(PERIOD).await;
            }
        });
    }
    fn finish_update_record(&self) -> Result<()> {
        let path = self.root.join("resume.json");
        if !path.exists() {
            return Ok(());
        }
        let mut resume: Resume =
            serde_json::from_slice(&std::fs::read(&path).map_err(|_| "无法读取更新恢复记录")?)
                .map_err(|_| "更新恢复记录损坏，未自动启用账号池")?;
        let exe = std::env::current_exe().map_err(|_| "找不到新版程序位置")?;
        if resume.to != env!("CARGO_PKG_VERSION")
            || exe.parent() != Some(resume.directory.as_path())
        {
            return Err("上次更新尚未完成；请检查版本或打开更新修复目录".into());
        }
        // Ignore old enable intent, including an already-claimed record.
        // Only the user's start_pool command may establish the connection.
        resume.enabled = false;
        resume.claimed = true;
        atomic_json(&path, &resume)?;
        std::fs::remove_file(path).map_err(|_| "更新恢复记录清理失败")?;
        Ok(())
    }
    pub fn resume_finished(&self) {
        self.status.lock().unwrap().resume_message = None;
        // Only retire a claimed record belonging to this exact new version.
        let path = self.root.join("resume.json");
        if let Ok(bytes) = std::fs::read(&path) {
            if serde_json::from_slice::<Resume>(&bytes)
                .is_ok_and(|r| r.claimed && r.to == env!("CARGO_PKG_VERSION"))
            {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    fn install_with_resume(&self, resume: &Resume, install: impl FnOnce() -> Result<()>) -> Result<()> {
        // Preserve update completion metadata on failure, without ever saving
        // an instruction to start the pool after installation.
        let mut resume = resume.clone();
        resume.enabled = false;
        atomic_json(&self.root.join("resume.json"), &resume)?;
        self.phase("installing");
        install()
    }
    async fn prepare_repair(&self, app: &tauri::AppHandle) -> Result<()> {
        self.phase("preparing");
        let version = env!("CARGO_PKG_VERSION");
        let manifest = self
            .manifest(&format!(
                "{REPOSITORY}/releases/download/v{version}/manifest.json"
            ))
            .await?;
        if manifest["version"] != version {
            return Err("上一版本修复清单不匹配，未开始更新".into());
        }
        let package = self.package(app, manifest.clone(), true).await?;
        let repair = self.root.join("repair");
        safe_directory(&self.root, &repair)?;
        atomic_copy(&package, &repair.join("上一版安装修复.exe"))?;
        atomic_json(&repair.join("manifest.json"), &manifest)?;
        atomic_copy(
            &std::env::current_exe().map_err(|_| "找不到当前程序")?,
            &repair.join("Fusion-recovery.exe"),
        )?;
        atomic_bytes(&repair.join("修复说明.txt"), "安装未完成或新版打不开时，先运行“上一版安装修复.exe”。账号数据会保留。\r\n若提示 Codex 仍依赖 Fusion，可运行 Fusion-recovery.exe，预览并确认恢复官方接入后再修复。此操作不会恢复此前自定义接入，请先查看预览。\r\n不要删除账号数据目录；不要复制或分享凭据。\r\n".as_bytes())?;
        Ok(())
    }
    fn prune_cache(&self, candidate: &str) {
        // Only remove this updater's two known files in obsolete version dirs.
        // Never recursively remove a user-data directory, unknown file or link.
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name == candidate
                || name == env!("CARGO_PKG_VERSION")
                || semver::Version::parse(&name).is_err()
            {
                continue;
            }
            let path = entry.path();
            if safe_directory(&self.root, &path).is_err() {
                continue;
            }
            let Ok(files) = std::fs::read_dir(&path)
                .and_then(|items| items.collect::<std::io::Result<Vec<_>>>())
            else {
                continue;
            };
            if files.iter().any(|file| {
                !["setup.exe", "manifest.json"]
                    .iter()
                    .any(|name| file.file_name() == *name)
            }) {
                continue;
            }
            for file in files {
                let _ = std::fs::remove_file(file.path());
            }
            let _ = std::fs::remove_dir(path);
        }
    }
}

fn client() -> Result<reqwest::Client> {
    let builder = reqwest::Client::builder();
    #[cfg(feature = "update-acceptance")]
    let builder = builder.no_proxy().redirect(reqwest::redirect::Policy::none());
    builder
        .user_agent("Fusion-Updater")
        .https_only(!cfg!(feature = "update-acceptance"))
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| "更新网络客户端初始化失败".into())
}
fn validate_manifest(manifest: &Value) -> Result<()> {
    let version = manifest["version"].as_str().ok_or("更新清单缺少版本")?;
    let parsed = semver::Version::parse(version).map_err(|_| "更新版本格式错误")?;
    if parsed.to_string() != version || version.len() > 100 || !parsed.pre.is_empty() || !parsed.build.is_empty() {
        return Err("更新版本格式错误".into());
    }
    let entry = &manifest["platforms"]["windows-x86_64"];
    let expected =
        format!("{REPOSITORY}/releases/download/v{version}/Fusion_{version}_x64-setup.exe");
    if entry["url"].as_str() != Some(&expected) {
        return Err("更新包不属于指定的 Fusion 发布仓库或版本".into());
    }
    if !entry["signature"]
        .as_str()
        .is_some_and(|s| !s.is_empty() && s.len() < 8192)
    {
        return Err("更新包缺少有效签名".into());
    }
    if manifest["notes"].as_str().is_some_and(|s| s.len() > 16000) {
        return Err("更新说明过长".into());
    }
    Ok(())
}
fn safe_directory(root: &Path, directory: &Path) -> Result<()> {
    if !directory.starts_with(root) {
        return Err("更新缓存路径不安全".into());
    }
    let mut paths = vec![root.to_path_buf()];
    let mut path = root.to_path_buf();
    for component in directory.strip_prefix(root).unwrap().components() {
        if !matches!(component, std::path::Component::Normal(_)) {
            return Err("更新缓存路径不安全".into());
        }
        path.push(component);
        paths.push(path.clone());
    }
    for path in paths {
        if !path.exists() {
            std::fs::create_dir(&path).map_err(|_| "无法创建更新缓存目录")?;
        }
        let meta = std::fs::symlink_metadata(&path).map_err(|_| "无法检查更新缓存目录")?;
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if meta.file_attributes() & 0x400 != 0 {
                return Err("更新缓存不能使用链接或重解析点".into());
            }
        }
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err("更新缓存路径不安全".into());
        }
    }
    Ok(())
}
fn atomic_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(path.parent().ok_or("缺少更新缓存目录")?)
        .map_err(|_| "无法创建更新记录")?;
    std::io::Write::write_all(&mut file, bytes).map_err(|_| "无法写入更新记录")?;
    file.as_file()
        .sync_all()
        .map_err(|_| "更新记录未安全写入磁盘")?;
    file.persist(path).map_err(|_| "无法提交更新记录")?;
    Ok(())
}
fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    atomic_bytes(
        path,
        &serde_json::to_vec_pretty(value).map_err(|_| "无法编码更新记录")?,
    )
}
fn atomic_copy(source: &Path, target: &Path) -> Result<()> {
    let temporary = tempfile::NamedTempFile::new_in(target.parent().ok_or("缺少修复目录")?)
        .map_err(|_| "无法创建修复缓存")?;
    std::fs::copy(source, temporary.path())
        .map_err(|_| "无法保留上一版本修复文件，请检查磁盘空间")?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|_| "修复文件写入失败")?;
    temporary.persist(target).map_err(|_| "修复文件保存失败")?;
    Ok(())
}
fn read_package(path: &Path) -> Result<Vec<u8>> {
    let meta = std::fs::symlink_metadata(path).map_err(|_| "无法读取更新包")?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return Err("更新包不能使用重解析点".into());
        }
    }
    if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_PACKAGE {
        return Err("更新包大小无效".into());
    }
    std::fs::read(path).map_err(|_| "更新包读取失败".into())
}
/// Read-only release validation, before Store/Vault/Tauri initialization.
pub fn verify_release(manifest_path: &Path, package: &Path) -> Result<()> {
    let bytes = std::fs::read(manifest_path).map_err(|_| "无法读取发布清单")?;
    if bytes.len() > 64 * 1024 {
        return Err("发布清单过大".into());
    }
    let manifest: Value = serde_json::from_slice(&bytes).map_err(|_| "发布清单格式错误")?;
    validate_manifest(&manifest)?;
    let config: Value =
        serde_json::from_str(include_str!("../tauri.conf.json")).map_err(|_| "缺少发布公钥")?;
    verify_signed_bytes(
        config["plugins"]["updater"]["pubkey"]
            .as_str()
            .ok_or("缺少发布公钥")?,
        &manifest,
        &read_package(package)?,
    )
}
fn verify_signed_bytes(key: &str, manifest: &Value, bytes: &[u8]) -> Result<()> {
    use base64::Engine;
    let decode = |s: &str| -> Result<String> {
        String::from_utf8(
            base64::engine::general_purpose::STANDARD
                .decode(s.trim())
                .map_err(|_| "签名编码无效")?,
        )
        .map_err(|_| "签名格式无效".into())
    };
    let public = minisign_verify::PublicKey::decode(&decode(key)?).map_err(|_| "公钥无效")?;
    let signature = minisign_verify::Signature::decode(&decode(
        manifest["platforms"]["windows-x86_64"]["signature"]
            .as_str()
            .ok_or("缺少签名")?,
    )?)
    .map_err(|_| "签名无效")?;
    public
        .verify(bytes, &signature, false)
        .map_err(|_| "发布文件签名校验失败")?;
    if signature
        .trusted_comment()
        .split('\t')
        .find_map(|part| part.strip_prefix("version:"))
        != manifest["version"].as_str()
    {
        return Err("签名版本与发布清单不符".into());
    }
    Ok(())
}
#[cfg(windows)]
fn installed_directory() -> Result<PathBuf> {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let key = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Fusion");
    let value = wide("InstallLocation");
    let mut buffer = [0u16; 32768];
    let mut size = std::mem::size_of_val(&buffer) as u32;
    let code = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if code != 0 {
        return Err("请先安装 Fusion；构建目录或便携副本不能执行应用内覆盖更新".into());
    }
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    let directory = PathBuf::from(String::from_utf16_lossy(&buffer[..end]).trim_matches('"'));
    let exe = std::env::current_exe().map_err(|_| "无法核验当前安装位置")?;
    let installed = std::fs::canonicalize(directory.join("personal-codex-pool.exe"))
        .map_err(|_| "无法核验安装记录中的 Fusion 程序")?;
    let running = std::fs::canonicalize(&exe).map_err(|_| "无法核验当前 Fusion 程序位置")?;
    if installed != running {
        return Err("当前程序位置与安装记录不同，请从已安装的 Fusion 更新".into());
    }
    Ok(directory)
}
#[cfg(windows)]
fn ensure_install_space(directory: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    for path in [directory.to_path_buf(), std::env::temp_dir()] {
        let wide: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut free = 0;
        let ok = unsafe {
            windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut free,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err("无法确认安装磁盘剩余空间，更新已停止".into());
        }
        if free < 512 * 1024 * 1024 {
            return Err("安装位置或临时目录剩余空间不足 512 MiB，请清理后重试".into());
        }
    }
    Ok(())
}

#[tauri::command]
pub fn update_status(updates: tauri::State<Arc<Updates>>) -> Status {
    let mut s = updates.status.lock().unwrap().clone();
    s.installing = updates.installing.load(Ordering::SeqCst);
    s.operation_active = updates.requested.load(Ordering::SeqCst);
    s
}
#[tauri::command]
pub fn check_update(app: tauri::AppHandle) -> Result<()> {
    let updates = app.state::<Arc<Updates>>().inner().clone();
    if updates.requested.load(Ordering::SeqCst) { return Ok(()); }
    let Ok(operation) = updates.operation.clone().try_lock_owned() else { return Ok(()); };
    updates.phase("checking");
    tauri::async_runtime::spawn(async move {
        let _operation = operation;
        if let Err(error) = updates.check(false).await { updates.fail(error); }
    });
    Ok(())
}
#[tauri::command]
pub fn dismiss_update_prompt(updates: tauri::State<Arc<Updates>>) -> Result<()> {
    updates.defer()
}
#[tauri::command]
pub fn open_project_repository(app: tauri::AppHandle) -> Result<()> {
    app.opener().open_url(REPOSITORY, None::<&str>).map_err(|_| "无法打开系统浏览器".into())
}
#[tauri::command]
pub fn open_update_repair(
    app: tauri::AppHandle,
    updates: tauri::State<Arc<Updates>>,
) -> Result<()> {
    let repair = updates.root.join("repair");
    safe_directory(&updates.root, &repair)?;
    app.opener()
        .open_path(repair.to_string_lossy().to_string(), None::<&str>)
        .map_err(|_| "无法打开更新修复目录".into())
}
#[tauri::command]
pub fn install_update(app: tauri::AppHandle, version: String) -> Result<()> {
    let updates = app.state::<Arc<Updates>>().inner().clone();
    let mut cancellation = updates.cancellation.lock().unwrap();
    if updates.requested.load(Ordering::SeqCst) { return Ok(()); }
    let manifest = updates.confirmed_candidate(&version)?;
    // Register synchronously before spawning, including while an automatic check is in flight.
    if updates.requested.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
        return Ok(());
    }
    let (sender, receiver) = tokio::sync::oneshot::channel();
    *cancellation = Some(sender);
    drop(cancellation);
    updates.dismiss_prompt();
    tauri::async_runtime::spawn(async move {
        let result = install(&app, &updates, manifest, receiver).await;
        if let Err(error) = result { updates.fail(error); }
        let mut cancellation = updates.cancellation.lock().unwrap();
        cancellation.take();
        updates.installing.store(false, Ordering::SeqCst);
        updates.requested.store(false, Ordering::SeqCst);
    });
    Ok(())
}
async fn install(app: &tauri::AppHandle, updates: &Updates, manifest: Value, mut cancel: tokio::sync::oneshot::Receiver<()>) -> Result<()> {
    let _operation = tokio::select! {
        biased;
        _ = &mut cancel => { updates.cancelled(); return Ok(()); }
        guard = updates.operation.lock() => guard,
    };
    let prepared = async {
        let fresh = updates.check(false).await?;
        if fresh.as_ref() != Some(&manifest) {
            return Err("发布版本已变化或撤回，请查看更新说明后重新确认".into());
        }
        let directory = installed_directory()?;
        ensure_install_space(&directory)?;
        updates.phase("downloading");
        let package = updates.package(app, manifest.clone(), true).await?;
        updates.prepare_repair(app).await?;
        Ok::<_, String>((directory, package))
    };
    let Some((directory, package)) = updates.prepare_or_cancel(&mut cancel, prepared).await? else { return Ok(()); };
    let a = app.state::<App>();
    let _lifecycle = a.lifecycle.clone().try_lock_owned()
        .map_err(|_| "账号池正在切换，请等待完成后重试更新")?;
    if a.close.lock().unwrap().running {
        return Err("账号池正在关闭，请等待完成后重试更新".into());
    }
    if !updates.begin_install(&mut cancel) { return Ok(()); }
    updates.phase("finalizing");
    if a.oauth.active() { return Err("请先完成或取消浏览器登录，再重试更新".into()); }
    if a.claude_login.active() {return Err("请先完成或取消 Claude 登录，再重试更新".into());}
    let _network_checks = a.network.pause_background_checks();
    let _work = updates.work.clone().write_owned().await;
    if a.oauth.active() { return Err("请先完成或取消浏览器登录，再重试更新".into()); }
    if a.claude_login.active() {return Err("请先完成或取消 Claude 登录，再重试更新".into());}
    let fresh = updates.manifest(STABLE_FEED).await?;
    if fresh != manifest {
        updates.apply_manifest(&fresh, false);
        return Err("发布版本已变化或撤回，已刷新更新信息；请重试更新".into());
    }
    let version = manifest["version"].as_str().unwrap();
    let update = updates.updater(app, manifest.clone())?;
    let check = update.clone();
    let bytes = blocking(move || {
        let bytes = read_package(&package)?;
        check.verify(&bytes).map_err(|_| "更新包签名校验失败")?;
        Ok(bytes)
    }).await?;
    ensure_install_space(&directory)?;
    updates.phase("closing");
    a.api_clients.stop_all().await?;
    let (connection, progress, store) = (a.connection.clone(), a.connection.clone(), a.accounts.store.clone());
    close_for_update(&connection, move || {
        codex_lifecycle::stop_client(&store, |phase| progress.set_phase(phase)).map(|_| ())
    }).await?;
    updates.phase("finishing");
    a.accounts.wait_background_idle().await?;
    a.claude_pool.wait_idle().await?;
    a.network.shutdown().await?;
    if !pool_core::api_config::detached(&config::codex_config_path()?)? {
        return Err("Codex API 接入仍依赖 Fusion，更新已停止".into());
    }
    if !config::detached(&config::codex_config_path()?, &std::env::current_exe().map_err(|_| "找不到程序路径")?, a.accounts.store.settings()?.port)? {
        return Err("Codex 仍依赖 Fusion，更新已停止".into());
    }
    updates.install_with_resume(&Resume {
        from: env!("CARGO_PKG_VERSION").into(), to: version.into(), enabled: false, claimed: false, directory,
    }, || update.install(bytes).map_err(|_| "安装器无法启动；账号池保持关闭，可重试更新或打开修复目录".to_owned()))
}
// Called with the lifecycle and exclusive work locks held, after package validation.
// An interrupted switch must go through the same verified close as an ordinary
// shutdown; the display status is not evidence that recovery is impossible.
async fn close_for_update(
    connection: &pool_core::connection::Connection,
    stop_client: impl FnOnce() -> Result<()> + Send + 'static,
) -> Result<()> {
    let before = connection.status().await?;
    match before.mode {
        // A detached pool has no client lifecycle work to do. The installer
        // still rechecks config::detached before touching Fusion's files.
        "off" => return Ok(()),
        "on" | "recovery_required" => {},
        _ => return Err("账号池正在切换，请等待完成后重试更新".into()),
    }
    if let Err(error) = close_connection(connection, stop_client).await {
        connection.close_failed(error.clone()).await;
        return Err(format!("更新前恢复原接入未完成：{error}"));
    }
    Ok(())
}
// Compiled out of normal builds. An acceptance build can only redirect the fixed
// public update URLs to an explicit IPv4 loopback server; package signatures remain required.
fn transport_url(url: &str) -> Result<String> {
    #[cfg(feature = "update-acceptance")]
    {
        let origin = std::env::var("FUSION_ACCEPTANCE_UPDATE_ORIGIN")
            .map_err(|_| "线下验收构建需要本机更新源，不能作为正式版使用")?;
        acceptance_url(&origin, url)
    }
    #[cfg(not(feature = "update-acceptance"))]
    Ok(url.to_owned())
}
#[cfg(any(test, feature = "update-acceptance"))]
fn acceptance_url(origin: &str, url: &str) -> Result<String> {
    let parsed = reqwest::Url::parse(origin).map_err(|_| "验收更新源格式错误")?;
    if parsed.scheme() != "http" || parsed.host_str() != Some("127.0.0.1")
        || parsed.port().is_none() || !parsed.username().is_empty() || parsed.password().is_some()
        || parsed.path() != "/" || parsed.query().is_some() || parsed.fragment().is_some() {
        return Err("验收更新源只允许带端口的 IPv4 本机 HTTP 地址".into());
    }
    let path = if url == STABLE_FEED { "stable.json" } else {
        url.strip_prefix(&format!("{REPOSITORY}/releases/download/"))
            .ok_or("不是指定的 Fusion 更新资源")?
    };
    Ok(format!("{}/{path}", origin.trim_end_matches('/')))
}

#[cfg(test)]
mod tests;
