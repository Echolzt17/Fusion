//! Automatic local protocol acceptance, independent of hardcoded version numbers.
use crate::{config, storage::Store, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
pub(crate) mod probe;
const PROBE_REVISION: u32 = 3;
const CERTIFICATE: &str = "native_protocol_certificate";
#[derive(Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ClientVersion {
    pub desktop: Option<String>,
    pub engine: Option<String>,
}
impl ClientVersion {
    // Catalog requests identify the verified desktop engine, not Fusion or the MSIX package.
    pub fn catalog_version(&self) -> Result<&str> {
        self.engine.as_deref().and_then(|v| v.strip_prefix("codex-cli "))
            .filter(|v| !v.is_empty() && v.len() <= 64
                && v.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'+')))
            // Match Codex's client_version_to_whole: prerelease/build labels
            // belong to identity checks, while /models receives major.minor.patch.
            .and_then(|v| v.split(['-', '+']).next())
            .filter(|v| v.split('.').count() == 3
                && v.split('.').all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())))
            .ok_or_else(|| "无法读取有效的 Codex 内核版本，未发送模型目录检查请求".into())
    }
}
#[derive(Clone)]
pub struct ClientIdentity {
    pub client: ClientVersion,
    pub engine_path: PathBuf,
    pub key: String,
}
impl ClientIdentity {
    // Desktop layer proves the registered package before cold activation, or
    // user/session/parent ownership for an already running desktop engine.
    pub fn inspect(desktop: String, desktop_path: &Path, engine_path: PathBuf) -> Result<Self> {
        let executable = engine_path.clone();
        Self::inspect_with_executable(desktop, desktop_path, engine_path, &executable)
    }
    fn inspect_with_executable(desktop: String, desktop_path: &Path, engine_path: PathBuf, executable: &Path) -> Result<Self> {
        // Large desktop engines must not be reread on every UI poll/restart.
        // A fresh Fusion process still hashes the file; changes invalidate this
        // in-memory optimization, and the persisted certificate binds full bytes.
        type Stamp = (String, PathBuf, PathBuf, u64, std::time::SystemTime);
        static CACHE: OnceLock<Mutex<Option<(Stamp, ClientIdentity)>>> = OnceLock::new();
        let metadata = std::fs::metadata(&engine_path).map_err(|_| "无法读取 Codex 内核文件")?;
        let stamp = (
            desktop.clone(),
            desktop_path.to_owned(),
            engine_path.clone(),
            metadata.len(),
            metadata
                .modified()
                .map_err(|_| "无法核验 Codex 内核修改时间")?,
        );
        let mut cache = CACHE
            .get_or_init(Default::default)
            .lock()
            .map_err(|_| "客户端文件检查暂不可用")?;
        if let Some((previous, identity)) = cache.as_ref() {
            if previous == &stamp {
                return Ok(identity.clone());
            }
        }
        let root = tempfile::tempdir().map_err(|_| "无法创建客户端检查临时目录")?;
        let mut command = probe::isolated_command(executable, root.path());
        command.arg("--version");
        let engine = bounded_output(&mut command, Duration::from_secs(5))
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .map(|s| s.trim().to_owned())
            .filter(|s| s.starts_with("codex-cli ") && s.len() <= 120)
            .ok_or("无法读取 Codex 内核版本；请检查安全软件是否阻止启动检查进程")?;
        let client = ClientVersion {
            desktop: Some(desktop),
            engine: Some(engine),
        };
        let key = identity_key(&client, desktop_path, &engine_path, &engine_path)?;
        let after = std::fs::metadata(&engine_path).map_err(|_| "检查期间 Codex 内核文件已变化")?;
        if after.len() != stamp.3 || after.modified().ok() != Some(stamp.4) {
            return Err("检查期间 Codex 正在更新，请等待更新完成后重试".into());
        }
        let identity = Self {
            client,
            engine_path,
            key,
        };
        *cache = Some((stamp, identity.clone()));
        Ok(identity)
    }
}
// Store package ACLs can deny direct CreateProcess even though the signed
// bundled engine is readable. Probe only a temporary byte copy, while keeping
// identity/certificates bound to the original package path and file contents.
pub fn inspect_bundled(store: &Store, desktop: String, desktop_path: &Path, engine_path: PathBuf) -> Result<ClientIdentity> {
    fn stamp(path: &Path) -> Result<(u64, std::time::SystemTime)> {
        let metadata = std::fs::metadata(path).map_err(|_| "无法读取 Codex 包内内核")?;
        Ok((metadata.len(), metadata.modified().map_err(|_| "无法核验 Codex 包内内核时间")?))
    }
    let before = stamp(&engine_path)?;
    let root = tempfile::tempdir().map_err(|_| "无法创建包内内核检查临时目录")?;
    let executable = root.path().join(if cfg!(windows) { "codex.exe" } else { "codex" });
    // Copy bytes rather than package ACLs. Never write back to the client.
    let copied = std::io::copy(
        &mut std::fs::File::open(&engine_path).map_err(|_| "无法读取 Codex 包内内核")?,
        &mut std::fs::File::create(&executable).map_err(|_| "无法准备包内内核临时副本")?,
    ).map_err(|_| "无法复制包内内核用于隔离检查")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| "无法设置包内内核检查权限")?;
    }
    if copied != before.0 || stamp(&engine_path)? != before {
        return Err("检查期间 Codex 包内内核已变化，请等待更新完成后重试".into());
    }
    let identity = ClientIdentity::inspect_with_executable(desktop, desktop_path, engine_path.clone(), &executable)?;
    serialized_verify(store, &identity, || probe::run(&executable))?;
    if stamp(&engine_path)? != before {
        return Err("检查期间 Codex 包内内核已变化，请等待更新完成后重试".into());
    }
    Ok(identity)
}
#[derive(Serialize, Deserialize)]
struct Certificate {
    revision: u32,
    key: String,
    client: ClientVersion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    equivalent_key: Option<String>,
}
#[derive(Default)]
struct Verification {
    key: String,
    checking: bool,
    error: Option<String>,
}
fn verification() -> &'static Mutex<Verification> {
    static VALUE: OnceLock<Mutex<Verification>> = OnceLock::new();
    VALUE.get_or_init(Default::default)
}
fn certificate_matches(store: &Store, identity: &ClientIdentity) -> Result<bool> {
    Ok(store
        .get(CERTIFICATE)?
        .and_then(|s| serde_json::from_str::<Certificate>(&s).ok())
        .is_some_and(|c| {
            c.revision == PROBE_REVISION && (c.key == identity.key || c.equivalent_key.as_ref() == Some(&identity.key))
                && c.client == identity.client
        }))
}
pub fn ensure_verified(store: &Store, identity: &ClientIdentity) -> Result<()> {
    serialized_verify(store, identity, || probe::run(&identity.engine_path))
}
// A desktop may extract its bundled engine into a managed runtime directory.
// Carry protocol evidence across that path change only when the actual engine
// recreates the prepared identity's complete version/path/content fingerprint.
pub fn ensure_verified_after_launch(store: &Store, identity: &ClientIdentity, prepared: &ClientIdentity, desktop_path: &Path) -> Result<()> {
    serialized_verify_evidence(store, identity, || {
        if certificate_matches(store, prepared)? && same_prepared_engine(identity, prepared, desktop_path)? {
            Ok(Some(prepared.key.clone()))
        } else {
            probe::run(&identity.engine_path).map(|()| None)
        }
    })
}
fn same_prepared_engine(identity: &ClientIdentity, prepared: &ClientIdentity, desktop_path: &Path) -> Result<bool> {
    if identity.client != prepared.client { return Ok(false); }
    let metadata = std::fs::metadata(&identity.engine_path).map_err(|_| "无法复核启动后的 Codex 内核")?;
    let modified = metadata.modified().map_err(|_| "无法复核启动后的 Codex 内核时间")?;
    let key = identity_key(&identity.client, desktop_path, &prepared.engine_path, &identity.engine_path)?;
    let after = std::fs::metadata(&identity.engine_path).map_err(|_| "启动后的 Codex 内核已变化")?;
    Ok(after.len() == metadata.len() && after.modified().ok() == Some(modified) && key == prepared.key)
}
fn identity_key(client: &ClientVersion, desktop_path: &Path, identity_path: &Path, content_path: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(PROBE_REVISION.to_le_bytes());
    hasher.update(serde_json::to_vec(client).map_err(|_| "无法准备客户端检查")?);
    hasher.update(desktop_path.to_string_lossy().as_bytes());
    hasher.update(identity_path.to_string_lossy().as_bytes());
    let mut file = std::fs::File::open(content_path).map_err(|_| "无法读取 Codex 内核文件")?;
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer).map_err(|_| "无法核验 Codex 内核文件")?;
        if count == 0 { break; }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}
fn serialized_verify(
    store: &Store,
    identity: &ClientIdentity,
    check: impl FnOnce() -> Result<()>,
) -> Result<()> {
    serialized_verify_evidence(store, identity, || check().map(|()| None))
}
fn serialized_verify_evidence(
    store: &Store,
    identity: &ClientIdentity,
    check: impl FnOnce() -> Result<Option<String>>,
) -> Result<()> {
    static OPERATION: Mutex<()> = Mutex::new(());
    let _operation = OPERATION
        .lock()
        .map_err(|_| "客户端兼容检查暂不可用，请重试启动")?;
    verify_with_evidence(store, identity, check)
}
#[cfg(test)]
fn verify_with(
    store: &Store,
    identity: &ClientIdentity,
    check: impl FnOnce() -> Result<()>,
) -> Result<()> {
    verify_with_evidence(store, identity, || check().map(|()| None))
}
fn verify_with_evidence(
    store: &Store,
    identity: &ClientIdentity,
    check: impl FnOnce() -> Result<Option<String>>,
) -> Result<()> {
    if certificate_matches(store, identity)? {
        return Ok(());
    }
    *verification().lock().unwrap() = Verification {
        key: identity.key.clone(),
        checking: true,
        error: None,
    };
    let result = check().and_then(|equivalent_key| {
        let c = Certificate {
            revision: PROBE_REVISION,
            key: identity.key.clone(),
            client: identity.client.clone(),
            equivalent_key,
        };
        store.set(
            CERTIFICATE,
            &serde_json::to_string(&c).map_err(|_| "无法保存客户端验证结果")?,
        )
    });
    let error = result.as_ref().err().map(|reason| format!(
        "客户端自动兼容检查未通过：{reason}。桌面 {} · {}。账号池切换未完成；请将此提示提供给维护人员。",
        identity.client.desktop.as_deref().unwrap_or("未知"), identity.client.engine.as_deref().unwrap_or("未知"),
    ));
    *verification().lock().unwrap() = Verification {
        key: identity.key.clone(),
        checking: false,
        error: error.clone(),
    };
    error.map_or(Ok(()), Err)
}
pub fn record_image_success(store: &Store) {
    // Retain evidence for the accepted client, never image bytes or prompt text.
    if let Ok(Some(s)) = store.get(CERTIFICATE) {
        if let Ok(c) = serde_json::from_str::<Certificate>(&s) {
            let _ = store.set(
                "native_image_success",
                &serde_json::json!({"clientKey": c.key, "at": crate::now()}).to_string(),
            );
        }
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageCompatibility {
    pub client: ClientVersion,
    pub verification_state: &'static str,
    pub detail: String,
    pub isolated_verified: bool,
    pub configuration_ready: bool,
    pub routes_ready: bool,
    pub upstream_image_received: bool,
    pub desktop_acceptance: &'static str,
}
pub fn status(
    store: &Store,
    path: PathBuf,
    running: bool,
    identity: Result<ClientIdentity>,
) -> Result<ImageCompatibility> {
    let (client, key, verified, mut state, mut detail) = match identity {
        Ok(identity) => {
            let verified = certificate_matches(store, &identity)?;
            let state = if verified { "verified" } else { "pending" };
            let detail = if verified {
                "当前客户端已通过本机隔离协议检查，实际桌面生图仍需验收"
            } else {
                "启动时自动检查当前客户端，无需手动验收版本或修改配置"
            };
            (
                identity.client,
                identity.key,
                verified,
                state,
                detail.to_owned(),
            )
        }
        Err(error) => (
            ClientVersion::default(),
            String::new(),
            false,
            "unavailable",
            error,
        ),
    };
    {
        let check = verification().lock().unwrap();
        if !verified && !key.is_empty() && check.key == key {
            if check.checking {
                state = "checking";
                detail =
                    "正在本机临时环境检查对话、生图、改图与失败不重放，不使用账号或真实配置".into();
            } else if let Some(error) = &check.error {
                state = "failed";
                detail.clone_from(error);
            }
        }
    }
    let received = store
        .get("native_image_success")?
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .is_some_and(|v| verified && v["clientKey"].as_str() == Some(&key));
    Ok(ImageCompatibility {
        client,
        verification_state: state,
        detail,
        isolated_verified: verified,
        configuration_ready: verified && config::image_ready(store, &path)?,
        routes_ready: running,
        upstream_image_received: received,
        desktop_acceptance: "pending",
    })
}
fn bounded_output(command: &mut std::process::Command, timeout: Duration) -> Option<Vec<u8>> {
    use std::{
        fs::OpenOptions,
        io::{Read, Seek},
        process::Stdio,
    };
    let path = std::env::temp_dir().join(format!("fusion-version-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
            .ok()?;
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::from(file.try_clone().ok()?))
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let deadline = Instant::now() + timeout;
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => break,
                Ok(Some(_)) => return None,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
        }
        if file.metadata().ok()?.len() > 8192 {
            return None;
        }
        file.rewind().ok()?;
        let mut bytes = Vec::new();
        file.take(8192).read_to_end(&mut bytes).ok()?;
        Some(bytes)
    })();
    let _ = std::fs::remove_file(path);
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    static TEST_STATE: Mutex<()> = Mutex::new(());
    #[test]
    fn copied_engine_reuses_protocol_evidence_only_for_exact_prepared_contents() {
        let _state = TEST_STATE.lock().unwrap();
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("pool.sqlite")).unwrap();
        let desktop = root.path().join("ChatGPT.exe");
        let bundled = root.path().join("bundled.exe");
        let runtime = root.path().join("runtime.exe");
        std::fs::write(&bundled, b"public fake engine; must never execute").unwrap();
        std::fs::copy(&bundled, &runtime).unwrap();
        let client = ClientVersion { desktop: Some("26.1027.6604.0".into()), engine: Some("codex-cli 0.166.0".into()) };
        let prepared = ClientIdentity {
            key: identity_key(&client, &desktop, &bundled, &bundled).unwrap(),
            client: client.clone(), engine_path: bundled,
        };
        let actual = ClientIdentity {
            key: identity_key(&client, &desktop, &runtime, &runtime).unwrap(),
            client, engine_path: runtime.clone(),
        };
        verify_with(&store, &prepared, || Ok(())).unwrap();
        assert!(!certificate_matches(&store, &actual).unwrap());
        ensure_verified_after_launch(&store, &actual, &prepared, &desktop).unwrap();
        assert!(certificate_matches(&store, &actual).unwrap());
        assert!(certificate_matches(&store, &prepared).unwrap());
        let reopened = Store::open(&root.path().join("pool.sqlite")).unwrap();
        serialized_verify(&reopened, &prepared, || panic!("next cold start must reuse prepared evidence")).unwrap();
        assert!(!same_prepared_engine(&actual, &prepared, &root.path().join("other-desktop.exe")).unwrap());
        std::fs::write(&runtime, b"different contents with the same claimed version").unwrap();
        let changed = ClientIdentity { key: identity_key(&actual.client, &desktop, &runtime, &runtime).unwrap(), ..actual };
        assert!(!same_prepared_engine(&changed, &prepared, &desktop).unwrap());
        verify_with(&store, &prepared, || Ok(())).unwrap();
        assert!(ensure_verified_after_launch(&store, &changed, &prepared, &desktop).is_err());
        assert!(!certificate_matches(&store, &changed).unwrap());
        store.set(CERTIFICATE, "").unwrap();
        assert!(ensure_verified_after_launch(&store, &prepared, &prepared, &desktop).is_err());
        assert!(!certificate_matches(&store, &prepared).unwrap());
    }
    #[test]
    fn catalog_version_uses_engine_not_desktop_or_fusion_version() {
        let mut client = ClientVersion {
            desktop: Some("26.1027.3384.0".into()),
            engine: Some("codex-cli 0.166.0-alpha.5.1".into()),
        };
        assert_eq!(client.catalog_version().unwrap(), "0.166.0");
        client.engine = Some("codex-cli 0.165.0+build".into());
        assert_eq!(client.catalog_version().unwrap(), "0.165.0");
        for invalid in [None, Some("0.166.0"), Some("codex-cli "), Some("codex-cli bad/version"), Some("codex-cli 0.166.0\nextra"), Some("codex-cli bad-version"), Some("codex-cli 0.166")] {
            client.engine = invalid.map(str::to_owned);
            assert!(client.catalog_version().is_err());
        }
    }
    #[cfg(windows)]
    #[test]
    fn version_probe_timeout_only_ends_its_owned_temporary_child() {
        let root = tempfile::tempdir().unwrap();
        let helper = PathBuf::from(std::env::var_os("SYSTEMROOT").unwrap())
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let mut command = probe::isolated_command(&helper, root.path());
        command.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Start-Sleep -Seconds 20",
        ]);
        let started = Instant::now();
        assert!(bounded_output(&mut command, Duration::from_millis(150)).is_none());
        assert!(started.elapsed() < Duration::from_secs(3));
    }
    fn identity(key: &str) -> ClientIdentity {
        ClientIdentity {
            client: ClientVersion {
                desktop: Some("26.1027.3384.0".into()),
                engine: Some("codex-cli 0.166.0-alpha.5.1".into()),
            },
            key: key.into(),
            engine_path: PathBuf::from("not-executed"),
        }
    }
    #[test]
    fn changed_versions_require_local_protocol_evidence_not_a_whitelist() {
        let _state = TEST_STATE.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("pool.sqlite")).unwrap();
        let a = identity("binary-a");
        verify_with(&store, &a, || Ok(())).unwrap();
        verify_with(&store, &a, || panic!("reuse certificate")).unwrap();
        record_image_success(&store);
        assert!(
            status(&store, dir.path().join("config.toml"), false, Ok(a.clone()))
                .unwrap()
                .upstream_image_received
        );
        let b = identity("replaced-binary");
        assert!(!certificate_matches(&store, &b).unwrap());
        let error = verify_with(&store, &b, || Err("图片编辑协议不兼容".into())).unwrap_err();
        assert!(error.contains("0.166.0-alpha.5.1") && error.contains("图片编辑"));
        assert!(certificate_matches(&store, &a).unwrap());
        let failed = status(&store, dir.path().join("config.toml"), false, Ok(b.clone())).unwrap();
        assert_eq!(failed.verification_state, "failed");
        assert!(!failed.upstream_image_received);
        verify_with(&store, &b, || Ok(())).unwrap();
        assert!(
            !status(&store, dir.path().join("config.toml"), false, Ok(b))
                .unwrap()
                .upstream_image_received
        );
        assert!(!dir.path().join("config.toml").exists());
        store
            .set(
                CERTIFICATE,
                &serde_json::to_string(&Certificate {
                    revision: 999,
                    key: a.key.clone(),
                    client: a.client.clone(),
                    equivalent_key: None,
                })
                .unwrap(),
            )
            .unwrap();
        assert!(!certificate_matches(&store, &a).unwrap());
    }
    #[test]
    fn concurrent_callers_share_verification_and_restart_reuses_only_matching_certificate() {
        let _state = TEST_STATE.lock().unwrap();
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc, Barrier,
        };
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&dir.path().join("pool.sqlite")).unwrap());
        let barrier = Arc::new(Barrier::new(8));
        let calls = Arc::new(AtomicUsize::new(0));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let (store, barrier, calls) = (store.clone(), barrier.clone(), calls.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    serialized_verify(&store, &identity("single-flight"), || {
                        calls.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    })
                    .unwrap();
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        drop(store);
        let reopened = Store::open(&dir.path().join("pool.sqlite")).unwrap();
        serialized_verify(&reopened, &identity("single-flight"), || {
            panic!("persisted acceptance")
        })
        .unwrap();
        let mut changed = identity("single-flight");
        changed.client.desktop = Some("26.1101.1.0".into());
        assert!(!certificate_matches(&reopened, &changed).unwrap());
        reopened.set(CERTIFICATE, "damaged").unwrap();
        assert!(!certificate_matches(&reopened, &identity("single-flight")).unwrap());
    }
    #[test]
    #[ignore = "Set FUSION_NATIVE_ENGINE to an actual desktop engine; isolated local fixture only"]
    fn actual_desktop_protocol_probe() {
        let engine = std::env::var_os("FUSION_NATIVE_ENGINE").expect("FUSION_NATIVE_ENGINE");
        probe::run(Path::new(&engine)).unwrap();
    }
}
