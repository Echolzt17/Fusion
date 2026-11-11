//! The only process this module owns is the bundled, pinned network kernel.
//! Configuration and native output must never be included in errors or diagnostics.
use pool_core::{network::{KernelBackend, KernelEndpoint}, Result};
use serde_yaml_ng::{Mapping, Value};
use sha2::{Digest, Sha256};
use std::{io::{Read, Write}, net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream}, path::{Path, PathBuf}, sync::{Arc, Mutex}, time::{Duration, Instant}};

const VERSION: &str = "v1.19.33";
// Generated from the official archive whose SHA256 is pinned in the preparation script.
const EXECUTABLE_SHA256: &str = "926bfea2ce48a247d6a7ce4cf641980e416202d5e40fe4ee5c949089ace0e197";
const OWNER_FILE: &str = ".fusion-network-runtime";
const OWNER_CONTENT: &[u8] = b"Fusion network kernel runtime v1\n";

pub struct NetworkKernel {
    executable: PathBuf,
    runtime_root: PathBuf,
    process: Mutex<Option<native::OwnedProcess>>,
}

impl NetworkKernel {
    pub fn new(executable: PathBuf, runtime_root: PathBuf) -> Result<Arc<Self>> {
        std::fs::create_dir_all(&runtime_root).map_err(|_| "无法准备代理内核运行目录")?;
        check_no_links(&runtime_root)?;
        let runtime_root = runtime_root.canonicalize().map_err(|_| "无法核验代理内核运行目录")?;
        // Parent death closes the job before a later instance reaches this cleanup.
        for entry in std::fs::read_dir(&runtime_root).map_err(|_| "无法检查代理内核运行目录")? {
            let entry = entry.map_err(|_| "无法检查代理内核遗留目录")?;
            if owned_name(&entry.file_name().to_string_lossy()) {
                remove_owned_runtime(&runtime_root, &entry.path())?;
            }
        }
        Ok(Arc::new(Self { executable, runtime_root, process: Mutex::new(None) }))
    }

    fn verify_executable(&self) -> Result<PathBuf> {
        check_no_links(&self.executable)?;
        let path = self.executable.canonicalize().map_err(|_| "内置代理内核缺失，请修复安装 Fusion")?;
        let mut file = std::fs::File::open(&path).map_err(|_| "无法读取内置代理内核")?;
        let mut hash = Sha256::new();
        let mut bytes = [0; 64 * 1024];
        loop {
            let n = file.read(&mut bytes).map_err(|_| "无法核验内置代理内核")?;
            if n == 0 { break; }
            hash.update(&bytes[..n]);
        }
        if format!("{:x}", hash.finalize()) != EXECUTABLE_SHA256 {
            return Err("内置代理内核校验失败，请修复安装 Fusion".into());
        }
        Ok(path)
    }
}

impl KernelBackend for NetworkKernel {
    fn start(&self, config: &str) -> Result<KernelEndpoint> {
        let mut state = self.process.lock().map_err(|_| "代理内核状态不可用")?;
        if state.as_ref().is_some_and(native::OwnedProcess::running) {
            return Err("代理内核正在运行，请先完成请求排空".into());
        }
        if let Some(previous) = state.as_mut() { previous.stop()?; }
        *state = None;
        let executable = self.verify_executable()?;
        let mut last_error = "代理内核启动失败".to_owned();
        for _ in 0..3 {
            // Hold both reservations until the complete configuration has been written.
            // A competing bind after release is detected by the authenticated readiness check.
            let proxy = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|_| "无法分配代理端口")?;
            let controller = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|_| "无法分配代理控制端口")?;
            let proxy_port = proxy.local_addr().map_err(|_| "无法读取代理端口")?.port();
            let controller_port = controller.local_addr().map_err(|_| "无法读取代理控制端口")?.port();
            let (yaml, secret) = prepare_config(config, proxy_port, controller_port)?;
            let runtime = self.runtime_root.join(format!("run-{}", uuid::Uuid::new_v4()));
            native::create_private_directory(&runtime)?;
            let prepared = (|| {
                std::fs::write(runtime.join(OWNER_FILE), OWNER_CONTENT).map_err(|_| "无法标记代理内核运行目录")?;
                std::fs::write(runtime.join("config.yaml"), yaml.as_bytes()).map_err(|_| "无法准备代理内核运行配置")?;
                drop(proxy); drop(controller);
                let mut child = native::OwnedProcess::spawn(&executable, &runtime, &self.runtime_root)?;
                let deadline = Instant::now() + Duration::from_secs(8);
                loop {
                    if !child.running() {
                        child.stop()?;
                        return Err("代理内核未能加载配置或监听端口".to_owned());
                    }
                    if version_ready(controller_port, &secret) && TcpStream::connect_timeout(&SocketAddr::from((Ipv4Addr::LOCALHOST, proxy_port)), Duration::from_millis(200)).is_ok() {
                        return Ok(child);
                    }
                    if Instant::now() >= deadline {
                        child.stop()?;
                        return Err("代理内核启动核验超时".to_owned());
                    }
                    std::thread::sleep(Duration::from_millis(80));
                }
            })();
            match prepared {
                Ok(child) => {
                    *state = Some(child);
                    return Ok(KernelEndpoint { proxy_url: format!("http://127.0.0.1:{proxy_port}"), controller_url: format!("http://127.0.0.1:{controller_port}"), instance_id: uuid::Uuid::new_v4().to_string() });
                }
                Err(error) => {
                    last_error = error;
                    remove_owned_runtime(&self.runtime_root, &runtime)?;
                }
            }
        }
        Err(last_error)
    }

    fn stop(&self) -> Result<()> {
        let mut state = self.process.lock().map_err(|_| "代理内核状态不可用")?;
        if let Some(child) = state.as_mut() { child.stop()?; }
        *state = None;
        Ok(())
    }

    fn running(&self) -> bool {
        // Startup holds this mutex while checking the executable and ports. A
        // synchronous status IPC must never block the WebView event loop on it.
        self.process.try_lock().ok().is_some_and(|state| state.as_ref().is_some_and(native::OwnedProcess::running))
    }
}

fn key(value: &str) -> Value { Value::String(value.to_owned()) }

fn prepare_config(raw: &str, proxy_port: u16, controller_port: u16) -> Result<(String, String)> {
    let mut value: Value = serde_yaml_ng::from_str(raw).map_err(|_| "代理内核运行配置无效")?;
    let config = value.as_mapping_mut().ok_or("代理内核运行配置无效")?;
    let secret = config.get(key("secret")).and_then(Value::as_str).filter(|s| s.len() >= 32 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')).ok_or("代理控制密钥无效")?.to_owned();
    for field in ["port", "mixed-port", "socks-port", "redir-port", "tproxy-port"] { config.insert(key(field), Value::Number(0.into())); }
    for field in ["external-controller-unix", "external-controller-pipe", "external-controller-tls", "external-ui", "external-ui-url", "external-doh-server", "authentication", "skip-auth-prefixes"] { config.remove(key(field)); }
    config.insert(key("allow-lan"), Value::Bool(false));
    config.insert(key("bind-address"), key("127.0.0.1"));
    config.insert(key("mode"), key("rule"));
    config.insert(key("log-level"), key("silent"));
    config.insert(key("external-controller"), key(&format!("127.0.0.1:{controller_port}")));
    config.insert(key("external-controller-cors"), Value::Mapping(Mapping::new()));
    for feature in ["tun", "dns"] {
        let mut disabled = Mapping::new(); disabled.insert(key("enable"), Value::Bool(false));
        config.insert(key(feature), Value::Mapping(disabled));
    }
    let listeners = config.get_mut(key("listeners")).and_then(Value::as_sequence_mut).filter(|v| v.len() == 1).ok_or("代理入站配置无效")?;
    let listener = listeners[0].as_mapping_mut().ok_or("代理入站配置无效")?;
    if listener.get(key("type")).and_then(Value::as_str) != Some("http") || listener.get(key("users")).and_then(Value::as_sequence).is_none_or(Vec::is_empty) {
        return Err("代理入口必须使用内部身份验证".into());
    }
    listener.insert(key("listen"), key("127.0.0.1"));
    listener.insert(key("port"), Value::Number(proxy_port.into()));
    let yaml = serde_yaml_ng::to_string(&value).map_err(|_| "无法生成代理内核运行配置")?;
    Ok((yaml, secret))
}

fn version_ready(port: u16, secret: &str) -> bool {
    let Ok(mut stream) = TcpStream::connect_timeout(&SocketAddr::from((Ipv4Addr::LOCALHOST, port)), Duration::from_millis(200)) else { return false; };
    if stream.set_read_timeout(Some(Duration::from_millis(300))).is_err() || stream.set_write_timeout(Some(Duration::from_millis(300))).is_err() { return false; }
    if write!(stream, "GET /version HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {secret}\r\nConnection: close\r\n\r\n").is_err() { return false; }
    let mut response = Vec::new();
    if stream.take(4097).read_to_end(&mut response).is_err() || response.len() > 4096 { return false; }
    let Some(split) = response.windows(4).position(|w| w == b"\r\n\r\n") else { return false; };
    if !response.starts_with(b"HTTP/1.1 200 ") { return false; }
    serde_json::from_slice::<serde_json::Value>(&response[split + 4..]).ok().is_some_and(|v| v["version"].as_str() == Some(VERSION))
}

fn owned_name(name: &str) -> bool {
    name.strip_prefix("run-").is_some_and(|suffix| uuid::Uuid::parse_str(suffix).is_ok())
}

fn check_no_links(path: &Path) -> Result<()> {
    let mut current = Some(path);
    while let Some(part) = current {
        if let Ok(metadata) = std::fs::symlink_metadata(part) {
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if metadata.file_attributes() & 0x400 != 0 { return Err("代理内核目录包含重解析点，操作已停止".into()); }
            }
            if metadata.file_type().is_symlink() { return Err("代理内核目录包含链接，操作已停止".into()); }
        }
        current = part.parent();
    }
    Ok(())
}

fn check_tree(path: &Path) -> Result<()> {
    check_no_links(path)?;
    if path.is_dir() {
        for entry in std::fs::read_dir(path).map_err(|_| "无法核验代理内核临时文件")? {
            check_tree(&entry.map_err(|_| "无法核验代理内核临时文件")?.path())?;
        }
    }
    Ok(())
}

fn remove_owned_runtime(root: &Path, path: &Path) -> Result<()> {
    if !path.exists() { return Ok(()); }
    if path.parent() != Some(root) || !path.file_name().is_some_and(|name| owned_name(&name.to_string_lossy())) {
        return Err("代理内核临时目录归属不匹配，未清理".into());
    }
    check_tree(path)?;
    if std::fs::read(path.join(OWNER_FILE)).map_err(|_| "代理内核临时目录缺少归属记录，未清理")? != OWNER_CONTENT {
        return Err("代理内核临时目录归属记录无效，未清理".into());
    }
    let resolved = path.canonicalize().map_err(|_| "无法核验代理内核临时目录")?;
    if resolved.parent() != Some(root) { return Err("代理内核临时目录超出应用范围，未清理".into()); }
    std::fs::remove_dir_all(path).map_err(|_| "代理内核临时配置清理未完成".into())
}

#[cfg(windows)]
mod native;

#[cfg(not(windows))]
mod native {
    use super::*;
    pub(super) struct OwnedProcess;
    impl OwnedProcess {
        pub fn spawn(_: &Path, _: &Path, _: &Path) -> Result<Self> { Err("内置代理内核仅支持 Windows".into()) }
        pub fn running(&self) -> bool { false }
        pub fn stop(&mut self) -> Result<()> { Ok(()) }
    }
    pub(super) fn create_private_directory(_: &Path) -> Result<()> { Err("内置代理内核仅支持 Windows".into()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_kernel_configuration_has_only_private_authenticated_ingress() {
        let config = "secret: fixture-control-secret-01234567890123456789\nlisteners:\n- name: fusion-http\n  type: http\n  port: 0\n  users:\n  - username: opaque\n    password: fixture\nallow-lan: true\nexternal-controller-pipe: fake\ntun: {enable: true}\ndns: {enable: true}\n";
        let (yaml, _) = prepare_config(config, 18201, 18202).unwrap();
        let value: Value = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(value["listeners"][0]["listen"].as_str(), Some("127.0.0.1"));
        assert_eq!(value["listeners"][0]["port"].as_u64(), Some(18201));
        assert_eq!(value["external-controller"].as_str(), Some("127.0.0.1:18202"));
        assert_eq!(value["allow-lan"].as_bool(), Some(false));
        assert_eq!(value["tun"]["enable"].as_bool(), Some(false));
        assert_eq!(value["dns"]["enable"].as_bool(), Some(false));
        assert!(value["external-controller-pipe"].is_null());
        assert!(prepare_config("secret: too-short\nlisteners: []", 1, 2).is_err());
    }
    #[test]
    fn status_never_waits_for_the_startup_process_mutex() {
        let temporary=tempfile::tempdir().unwrap();let kernel=NetworkKernel::new(temporary.path().join("not-started.exe"),temporary.path().join("runtime")).unwrap();
        let _starting=kernel.process.lock().unwrap();let start=Instant::now();
        assert!(!kernel.running());assert!(start.elapsed()<Duration::from_millis(100));
    }

    #[test]
    fn runtime_cleanup_requires_exact_parent_and_marker() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let runtime = root.join(format!("run-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&runtime).unwrap();
        std::fs::write(runtime.join("config.yaml"), "public fixture").unwrap();
        assert!(remove_owned_runtime(&root, &runtime).is_err());
        assert!(runtime.join("config.yaml").exists());
        std::fs::write(runtime.join(OWNER_FILE), OWNER_CONTENT).unwrap();
        assert!(remove_owned_runtime(&root.join("other"), &runtime).is_err());
        remove_owned_runtime(&root, &runtime).unwrap();
        assert!(!runtime.exists());
    }

    #[cfg(windows)]
    fn fixture_config() -> &'static str {
        "secret: fixture-controller-secret-01234567890123456789\nlisteners:\n- name: fusion-http\n  type: http\n  port: 0\n  listen: 127.0.0.1\n  users:\n  - username: fixture-route\n    password: fixture-local-password\nproxies:\n- name: fixture-node\n  type: http\n  server: 127.0.0.1\n  port: 9\nrules:\n- IN-USER,fixture-route,fixture-node\n- MATCH,REJECT\n"
    }

    #[cfg(windows)]
    fn bundled_fixture(root: &Path) -> Arc<NetworkKernel> {
        NetworkKernel::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../resources/network-kernel/mihomo.exe"), root.to_owned()).unwrap()
    }

    #[test]
    #[cfg(windows)]
    fn bundled_kernel_start_stop_and_cleanup_use_only_an_owned_temporary_directory() {
        let temporary = tempfile::tempdir().unwrap();
        let runtime = temporary.path().join("runtime");
        let kernel = bundled_fixture(&runtime);
        assert!(!kernel.running());
        let endpoint = kernel.start(fixture_config()).unwrap();
        assert!(kernel.running());
        assert!(endpoint.proxy_url.starts_with("http://127.0.0.1:"));
        assert!(endpoint.controller_url.starts_with("http://127.0.0.1:"));
        assert!(kernel.start(fixture_config()).is_err());
        kernel.stop().unwrap();
        kernel.stop().unwrap();
        assert!(!kernel.running());
        assert_eq!(std::fs::read_dir(&runtime).unwrap().count(), 0);
    }

    #[test]
    #[ignore = "helper for the isolated parent-death test"]
    #[cfg(windows)]
    fn job_owner_exit_helper() {
        let Some(path) = std::env::var_os("FUSION_KERNEL_JOB_FIXTURE") else { return; };
        let root = PathBuf::from(path);
        let kernel = bundled_fixture(&root.join("runtime"));
        kernel.start(fixture_config()).unwrap();
        let pid = kernel.process.lock().unwrap().as_ref().unwrap().id();
        std::fs::write(root.join("child.pid"), pid.to_string()).unwrap();
        // Bypass destructors to verify the operating system's job protection.
        std::process::exit(0);
    }

    #[test]
    #[cfg(windows)]
    fn abrupt_owner_exit_does_not_leave_a_kernel_process() {
        use std::os::windows::process::CommandExt;
        use windows::Win32::{Foundation::WAIT_OBJECT_0, System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject}};
        let temporary = tempfile::tempdir().unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "network_kernel::tests::job_owner_exit_helper", "--ignored", "--nocapture"])
            .env("FUSION_KERNEL_JOB_FIXTURE", temporary.path()).creation_flags(0x08000000)
            .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
            .status().unwrap();
        assert!(status.success());
        let pid: u32 = std::fs::read_to_string(temporary.path().join("child.pid")).unwrap().parse().unwrap();
        if let Ok(process) = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) } {
            assert_eq!(unsafe { WaitForSingleObject(process, 5000) }, WAIT_OBJECT_0);
            unsafe { let _ = windows::Win32::Foundation::CloseHandle(process); }
        }
        let runtime = temporary.path().join("runtime");
        assert!(std::fs::read_dir(&runtime).unwrap().count() > 0);
        let _kernel = bundled_fixture(&runtime);
        assert_eq!(std::fs::read_dir(&runtime).unwrap().count(), 0);
    }
}
