//! A local directory catalogue, independent of the desktop's signed-in account.
//! Native state is read through a strict projection. Never copy renderer atoms,
//! chat records, account identities, credentials, or the native database.
use crate::Result;
use pool_core::storage::Store;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    fs::File,
    io::{BufReader, Read},
    path::{Component, Path},
    time::{Duration, Instant},
};

const MEMORY_KEY: &str = "local-projects/v1/directories";
const MAX_STATE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_PROJECTS: usize = 256;
const MAX_ROOTS: usize = 16;
const MAX_RESTORE_BATCH: usize = 16;
pub const RESTORE_NOTICE: &str = "恢复本机目录入口会将 Codex 切到空白聊天输入页；不会发送消息或迁移原账号聊天。请先保存草稿。多目录分组和自定义标题需在 Codex 中整理。";

pub fn local_chat_route(id: &str, cursor: Option<&str>) -> Result<String> {
    let parsed = uuid::Uuid::parse_str(id).map_err(|_| "本机聊天标识无效")?;
    if parsed.to_string() != id
        || cursor.is_some_and(|c| c.len() > 4096 || c.chars().any(char::is_control))
    {
        return Err("本机聊天目录参数无效".into());
    }
    Ok(format!("codex://threads/{id}?hostId=local"))
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalProject {
    pub id: String,
    pub name: String,
    pub roots: Vec<String>,
    pub order: usize,
}

#[derive(Default)]
pub struct NativeSnapshot {
    pub owner_hash: Option<String>,
    pub account_scoped: bool,
    pub active: Vec<LocalProject>,
    retained: Vec<LocalProject>,
}

// Do not use flatten or Value here: unknown fields must be streamed past rather
// than buffered. In particular, inactive account "atoms" are never materialized.
#[derive(Default, Deserialize)]
struct NativeFile {
    #[serde(default, rename = "local-projects")]
    projects: BTreeMap<String, NativeProject>,
    #[serde(default, rename = "project-order")]
    order: Vec<String>,
    #[serde(default, rename = "electron-saved-workspace-roots")]
    roots: Vec<String>,
    #[serde(default, rename = "active-workspace-roots")]
    active_roots: Vec<String>,
    #[serde(default, rename = "electron-workspace-root-labels")]
    labels: BTreeMap<String, String>,
    #[serde(default, rename = "electron-account-renderer-state-v1")]
    accounts: Option<NativeAccounts>,
}

#[derive(Default, Deserialize)]
struct NativeGlobals {
    #[serde(default, rename = "local-projects")]
    projects: BTreeMap<String, NativeProject>,
    #[serde(default, rename = "project-order")]
    order: Vec<String>,
    #[serde(default, rename = "electron-saved-workspace-roots")]
    roots: Vec<String>,
    #[serde(default, rename = "active-workspace-roots")]
    active_roots: Vec<String>,
    #[serde(default, rename = "electron-workspace-root-labels")]
    labels: BTreeMap<String, String>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeProject {
    #[serde(default)]
    name: String,
    #[serde(default)]
    root_paths: Vec<String>,
}

#[derive(Deserialize)]
struct NativeAccounts {
    owner: Option<String>,
    #[serde(default)]
    inactive: BTreeMap<String, NativeInactive>,
}

#[derive(Default, Deserialize)]
struct NativeInactive {
    #[serde(default)]
    globals: NativeGlobals,
}

#[derive(Serialize, Deserialize)]
struct Memory {
    version: u32,
    projects: Vec<LocalProject>,
}

impl Default for Memory {
    fn default() -> Self {
        Self {
            version: 1,
            projects: Vec::new(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectEntry {
    #[serde(flatten)]
    pub project: LocalProject,
    pub present: bool,
    pub available: bool,
    pub can_restore: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectStatus {
    pub entries: Vec<ProjectEntry>,
    pub account_ready: bool,
    pub notice: &'static str,
}

fn valid_owner(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn local_path(value: &str) -> Option<String> {
    if value.len() > 32_768 || value.chars().any(char::is_control) {
        return None;
    }
    let value = value.trim();
    #[cfg(windows)]
    let value = value.strip_prefix(r"\\?\").unwrap_or(value);
    // UNC, WSL and device paths are outside this local-machine catalogue.
    if value.starts_with(r"\\") || value.starts_with("//") {
        return None;
    }
    let path = Path::new(value);
    if !path.is_absolute() || path.components().any(|c| c == Component::ParentDir) {
        return None;
    }
    #[cfg(windows)]
    {
        let bytes = value.as_bytes();
        if bytes.len() < 3 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
            return None;
        }
        let normalized = value.replace('/', "\\");
        Some(if normalized.len() > 3 {
            normalized.trim_end_matches('\\').to_owned()
        } else {
            normalized
        })
    }
    #[cfg(not(windows))]
    Some(if value == "/" {
        value.to_owned()
    } else {
        value.trim_end_matches('/').to_owned()
    })
}

fn path_key(path: &str) -> String {
    #[cfg(windows)]
    {
        path.to_lowercase()
    }
    #[cfg(not(windows))]
    {
        path.to_owned()
    }
}

fn project(name: &str, roots: &[String], order: usize) -> Option<LocalProject> {
    if roots.is_empty() || roots.len() > MAX_ROOTS {
        return None;
    }
    let mut paths = Vec::new();
    let mut unique = HashSet::new();
    for root in roots {
        let root = local_path(root)?;
        if unique.insert(path_key(&root)) {
            paths.push(root);
        }
    }
    let mut keys = paths.iter().map(|p| path_key(p)).collect::<Vec<_>>();
    keys.sort();
    let id = format!("{:x}", Sha256::digest(keys.join("\0").as_bytes()));
    let name = name
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(160)
        .collect::<String>();
    let name = if name.is_empty() {
        Path::new(&paths[0])
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("本机项目")
            .to_owned()
    } else {
        name
    };
    Some(LocalProject {
        id,
        name,
        roots: paths,
        order,
    })
}

fn globals_projects(globals: NativeGlobals) -> Result<Vec<LocalProject>> {
    if globals.projects.len() > MAX_PROJECTS
        || globals.roots.len() > MAX_PROJECTS
        || globals.active_roots.len() > MAX_PROJECTS
        || globals.order.len() > MAX_PROJECTS
    {
        return Err("本机项目目录数量超过安全读取范围，未修改 Codex 数据".into());
    }
    let order = globals
        .order
        .iter()
        .enumerate()
        .map(|(i, id)| (id.as_str(), i))
        .collect::<BTreeMap<_, _>>();
    let mut result = globals
        .projects
        .iter()
        .filter_map(|(id, p)| {
            project(
                &p.name,
                &p.root_paths,
                order.get(id.as_str()).copied().unwrap_or(MAX_PROJECTS),
            )
        })
        .collect::<Vec<_>>();
    result.sort_by(|a, b| a.order.cmp(&b.order).then(a.id.cmp(&b.id)));
    let mut roots = result
        .iter()
        .flat_map(|p| p.roots.iter().map(|r| path_key(r)))
        .collect::<HashSet<_>>();
    for path in globals.roots.into_iter().chain(globals.active_roots) {
        if let Some(p) = project(
            globals.labels.get(&path).map_or("", String::as_str),
            std::slice::from_ref(&path),
            result.len(),
        ) {
            if roots.insert(path_key(&p.roots[0])) {
                result.push(p);
            }
        }
    }
    Ok(result)
}

fn parse_snapshot(reader: impl Read) -> Result<NativeSnapshot> {
    let source: NativeFile =
        serde_json::from_reader(reader).map_err(|_| "本机项目目录暂不可读，未修改 Codex 数据")?;
    let current = NativeGlobals {
        projects: source.projects,
        order: source.order,
        roots: source.roots,
        active_roots: source.active_roots,
        labels: source.labels,
    };
    let mut snapshot = NativeSnapshot {
        active: globals_projects(current)?,
        ..NativeSnapshot::default()
    };
    if let Some(accounts) = source.accounts {
        snapshot.account_scoped = true;
        if accounts
            .owner
            .as_deref()
            .is_some_and(|owner| !valid_owner(owner))
            || accounts.inactive.len() > MAX_PROJECTS
        {
            return Err("客户端登录工作区格式无法确认，未修改 Codex 数据".into());
        }
        snapshot.owner_hash = accounts.owner;
        for (owner, state) in accounts.inactive {
            if valid_owner(&owner) {
                snapshot.retained.extend(globals_projects(state.globals)?);
            }
        }
    }
    Ok(snapshot)
}

pub fn read_snapshot(codex_home: &Path) -> Result<NativeSnapshot> {
    let path = codex_home.join(".codex-global-state.json");
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(NativeSnapshot::default()),
        Err(_) => return Err("无法读取本机项目目录，未修改 Codex 数据".into()),
    };
    let before = file.metadata().map_err(|_| "无法核验本机项目目录")?;
    if before.len() > MAX_STATE_BYTES {
        return Err("本机项目目录文件过大，未修改 Codex 数据".into());
    }
    let snapshot = parse_snapshot(BufReader::new(file).take(MAX_STATE_BYTES + 1))?;
    let after = std::fs::metadata(path).map_err(|_| "本机项目目录正在变化，请重试")?;
    if before.len() != after.len() || before.modified().ok() != after.modified().ok() {
        return Err("本机项目目录正在变化，请重试".into());
    }
    Ok(snapshot)
}

fn load(store: &Store) -> Result<Memory> {
    let Some(text) = store.get(MEMORY_KEY)? else {
        return Ok(Memory::default());
    };
    let memory: Memory = serde_json::from_str(&text).map_err(|_| "本机项目记忆无法读取")?;
    if memory.version != 1 {
        return Err("本机项目记忆版本较新，请升级 Fusion；未覆盖已有记录".into());
    }
    if memory.projects.len() > MAX_PROJECTS {
        return Err("本机项目记忆数量超过安全范围".into());
    }
    Ok(memory)
}

fn remember(store: &Store, snapshot: &NativeSnapshot) -> Result<Memory> {
    let previous = load(store)?;
    let mut memory = Memory::default();
    let mut ids = HashSet::new();
    for p in snapshot
        .active
        .iter()
        .chain(previous.projects.iter())
        .chain(snapshot.retained.iter())
    {
        if let Some(mut p) = project(&p.name, &p.roots, memory.projects.len()) {
            if ids.insert(p.id.clone()) {
                p.order = memory.projects.len();
                memory.projects.push(p);
            }
        }
    }
    if memory.projects.len() > MAX_PROJECTS {
        return Err("本机项目记忆数量超过安全范围，未覆盖已有记录".into());
    }
    let encoded = serde_json::to_string(&memory).map_err(|_| "无法保存本机项目记忆")?;
    if store.get(MEMORY_KEY)?.as_deref() != Some(encoded.as_str()) {
        store.set(MEMORY_KEY, &encoded)?;
    }
    Ok(memory)
}

fn present(snapshot: &NativeSnapshot, p: &LocalProject) -> bool {
    snapshot.active.iter().any(|current| {
        current.id == p.id
            || (p.roots.len() == 1
                && current
                    .roots
                    .iter()
                    .any(|r| path_key(r) == path_key(&p.roots[0])))
    })
}

fn available(p: &LocalProject) -> bool {
    p.roots.iter().all(|r| {
        // Resolve only directory metadata, never read project files. A symlink to
        // an unavailable or network destination must not cause automatic access.
        std::fs::canonicalize(r)
            .ok()
            .and_then(|p| p.to_str().and_then(local_path))
            .is_some_and(|p| Path::new(&p).is_dir())
    })
}

pub fn status(store: &Store, codex_home: &Path) -> Result<ProjectStatus> {
    let snapshot = read_snapshot(codex_home)?;
    let memory = remember(store, &snapshot)?;
    Ok(ProjectStatus {
        account_ready: snapshot.account_scoped && snapshot.owner_hash.is_some(),
        notice: RESTORE_NOTICE,
        entries: memory
            .projects
            .into_iter()
            .map(|p| {
                let available = available(&p);
                ProjectEntry {
                    present: present(&snapshot, &p),
                    available,
                    can_restore: available && p.roots.len() == 1,
                    project: p,
                }
            })
            .collect(),
    })
}

pub struct RestoreConditions {
    pub client_running: bool,
    pub connection_stable: bool,
    pub closing: bool,
    pub active_requests: usize,
}

impl RestoreConditions {
    pub fn check(&self) -> Result<()> {
        if !self.client_running {
            return Err("请先打开 Codex；恢复目录不会启动客户端".into());
        }
        if self.closing || !self.connection_stable {
            return Err("接入正在切换或关闭，未打开项目入口".into());
        }
        if self.active_requests > 0 {
            return Err("请等待当前任务结束后恢复本机项目入口".into());
        }
        Ok(())
    }
}

// The desktop adapter supplies freshly verified lifecycle facts and performs the
// official URI action. Its open_directory must recheck the lifecycle immediately
// before dispatch too. This module cannot launch a client or write native state.
pub trait RestoreAdapter: Sync {
    fn conditions(&self) -> impl std::future::Future<Output = Result<RestoreConditions>> + Send;
    fn snapshot(&self) -> Result<NativeSnapshot>;
    fn open_directory(&self, url: &str) -> Result<()>;
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreOutcome {
    pub restored: usize,
    pub already_present: usize,
    pub skipped_unavailable: usize,
    pub skipped_multi_root: usize,
    pub remaining: usize,
    pub pending_confirmation: bool,
    pub stopped: bool,
    pub message: String,
}

pub struct Restorer {
    operation: tokio::sync::Mutex<()>,
    confirmation_timeout: Duration,
    poll_interval: Duration,
}

impl Default for Restorer {
    fn default() -> Self {
        Self {
            operation: tokio::sync::Mutex::new(()),
            confirmation_timeout: Duration::from_secs(5),
            poll_interval: Duration::from_millis(250),
        }
    }
}

impl Restorer {
    // Invoke only from the explicit restore action after presenting RESTORE_NOTICE.
    // Never call during startup, account polling, pool start/stop, or an update.
    pub async fn restore(
        &self,
        store: &Store,
        adapter: &impl RestoreAdapter,
    ) -> Result<RestoreOutcome> {
        let _operation = self
            .operation
            .try_lock()
            .map_err(|_| "本机项目入口正在恢复，请等待当前操作")?;
        adapter.conditions().await?.check()?;
        let initial = adapter.snapshot()?;
        let owner = initial
            .owner_hash
            .clone()
            .filter(|_| initial.account_scoped)
            .ok_or("请先在 Codex 完成登录，再恢复本机项目入口")?;
        let memory = remember(store, &initial)?;
        let mut outcome = RestoreOutcome::default();
        let mut candidates = Vec::new();
        for p in memory.projects {
            if present(&initial, &p) {
                outcome.already_present += 1;
            } else if !available(&p) {
                outcome.skipped_unavailable += 1;
            } else if p.roots.len() != 1 {
                outcome.skipped_multi_root += 1;
            } else {
                candidates.push(p);
            }
        }
        outcome.remaining = candidates.len();
        let deadline = Instant::now() + Duration::from_secs(30);
        // The native directory action inserts at the top. Restore in reverse
        // remembered order so a completed batch retains the original ordering.
        for p in candidates.into_iter().rev().take(MAX_RESTORE_BATCH) {
            if Instant::now() >= deadline {
                break;
            }
            if let Err(message) = adapter.conditions().await.and_then(|c| c.check()) {
                outcome.stopped = true;
                outcome.message = message;
                return Ok(outcome);
            }
            let current = adapter.snapshot()?;
            if current.owner_hash.as_deref() != Some(owner.as_str()) {
                outcome.stopped = true;
                outcome.message = "登录工作区已变化，已停止恢复；未打开其他项目".into();
                return Ok(outcome);
            }
            if present(&current, &p) {
                outcome.already_present += 1;
                outcome.remaining -= 1;
                continue;
            }
            let mut url = reqwest::Url::parse("codex://new").map_err(|_| "项目入口地址无效")?;
            url.query_pairs_mut().append_pair("path", &p.roots[0]);
            // Exactly one URI dispatch for this directory in this explicit action.
            adapter.open_directory(url.as_str())?;
            let started = Instant::now();
            loop {
                if let Err(message) = adapter.conditions().await.and_then(|c| c.check()) {
                    outcome.stopped = true;
                    outcome.message = message;
                    return Ok(outcome);
                }
                let current = adapter.snapshot()?;
                if current.owner_hash.as_deref() != Some(owner.as_str()) {
                    outcome.stopped = true;
                    outcome.message = "登录工作区已变化，已停止恢复；未打开其他项目".into();
                    return Ok(outcome);
                }
                if present(&current, &p) {
                    outcome.restored += 1;
                    outcome.remaining -= 1;
                    break;
                }
                if started.elapsed() >= self.confirmation_timeout || Instant::now() >= deadline {
                    outcome.pending_confirmation = true;
                    outcome.message = "已请求打开目录，尚未确认项目入口；请在 Codex 查看目录信任提示。未重复打开或继续其他目录。".into();
                    return Ok(outcome);
                }
                tokio::time::sleep(self.poll_interval).await;
            }
        }
        outcome.message = if outcome.remaining > 0 {
            "已完成本批目录入口，其余可再次点击恢复"
        } else {
            "本机目录入口已核对；原账号聊天未迁移"
        }
        .into();
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    };

    fn fixture(root: &Path, name: &str) -> LocalProject {
        let path = root.join(name);
        std::fs::create_dir_all(&path).unwrap();
        project(name, &[path.to_string_lossy().into_owned()], 0).unwrap()
    }

    #[test]
    fn projection_keeps_only_local_directory_metadata_across_account_scopes() {
        let root = tempfile::tempdir().unwrap();
        let a = fixture(root.path(), "project a");
        let b = fixture(root.path(), "项目 b");
        let source = json!({"local-projects":{"raw-native-id":{"name":b.name,"rootPaths":b.roots,"chatGptBacking":{"accountId":"SECRET_ACCOUNT"}}},
            "project-order":["raw-native-id"],"electron-persisted-atom-state":{"draft":"SECRET_DRAFT"},
            "electron-account-renderer-state-v1":{"owner":"b".repeat(64),"inactive":{"a".repeat(64):{
                "atoms":{"messages":["SECRET_BODY"],"token":"SECRET_TOKEN"},"globals":{"local-projects":{"old-native-id":{"name":a.name,"rootPaths":a.roots}},"project-order":["old-native-id"]}}}}});
        let original = source.to_string();
        let native_path = root.path().join(".codex-global-state.json");
        std::fs::write(&native_path, &original).unwrap();
        let snapshot = read_snapshot(root.path()).unwrap();
        assert_eq!(snapshot.active.len(), 1);
        assert_eq!(snapshot.retained.len(), 1);
        let store = Store::open(&root.path().join("pool.sqlite")).unwrap();
        let memory = remember(&store, &snapshot).unwrap();
        assert_eq!(memory.projects.len(), 2);
        let saved = store.get(MEMORY_KEY).unwrap().unwrap();
        for forbidden in ["SECRET_", "raw-native-id", "old-native-id", &"b".repeat(64)] {
            assert!(!saved.contains(forbidden));
        }
        let mut switched = NativeSnapshot {
            owner_hash: Some("a".repeat(64)),
            account_scoped: true,
            active: vec![a],
            retained: Vec::new(),
        };
        assert_eq!(remember(&store, &switched).unwrap().projects.len(), 2);
        switched.active.clear();
        assert_eq!(remember(&store, &switched).unwrap().projects.len(), 2);
        assert_eq!(std::fs::read_to_string(native_path).unwrap(), original);
    }

    #[test]
    fn invalid_and_network_paths_are_excluded_and_future_memory_is_preserved() {
        assert!(local_path("relative/path").is_none());
        assert!(local_path(r"\\server\share").is_none());
        assert!(local_path(r"\\wsl$\Ubuntu\home").is_none());
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("pool.sqlite")).unwrap();
        store
            .set(MEMORY_KEY, r#"{"version":2,"projects":[]}"#)
            .unwrap();
        assert!(remember(&store, &NativeSnapshot::default()).is_err());
        assert!(store
            .get(MEMORY_KEY)
            .unwrap()
            .unwrap()
            .contains("\"version\":2"));
    }

    #[test]
    fn restore_requires_an_existing_idle_client_and_stable_connection() {
        for (running, stable, closing, requests) in [
            (false, true, false, 0),
            (true, false, false, 0),
            (true, true, true, 0),
            (true, true, false, 1),
        ] {
            assert!(RestoreConditions {
                client_running: running,
                connection_stable: stable,
                closing,
                active_requests: requests
            }
            .check()
            .is_err());
        }
    }

    #[test]
    fn history_route_accepts_only_a_canonical_uuid_and_fixed_local_host() {
        let id = "0199b551-3c14-7431-91aa-195be1881d11";
        assert_eq!(
            local_chat_route(id, None).unwrap(),
            format!("codex://threads/{id}?hostId=local")
        );
        for invalid in [
            "codex://threads/anything",
            "../../auth.json",
            "a?hostId=cloud",
            "not-a-uuid",
        ] {
            assert!(local_chat_route(invalid, None).is_err());
        }
        assert!(local_chat_route(id, Some("bad\ncursor")).is_err());
        assert!(local_chat_route(id, Some(&"x".repeat(4097))).is_err());
    }

    struct Adapter {
        snapshot: Mutex<NativeSnapshot>,
        opened: Mutex<Vec<String>>,
        closing: AtomicBool,
        acknowledge: bool,
        switch_owner: bool,
    }
    impl RestoreAdapter for Adapter {
        async fn conditions(&self) -> Result<RestoreConditions> {
            Ok(RestoreConditions {
                client_running: true,
                connection_stable: true,
                closing: self.closing.load(Ordering::SeqCst),
                active_requests: 0,
            })
        }
        fn snapshot(&self) -> Result<NativeSnapshot> {
            let s = self.snapshot.lock().unwrap();
            Ok(NativeSnapshot {
                owner_hash: s.owner_hash.clone(),
                account_scoped: s.account_scoped,
                active: s.active.clone(),
                retained: s.retained.clone(),
            })
        }
        fn open_directory(&self, url: &str) -> Result<()> {
            self.opened.lock().unwrap().push(url.to_owned());
            let mut snapshot = self.snapshot.lock().unwrap();
            if self.switch_owner {
                snapshot.owner_hash = Some("b".repeat(64));
            }
            if self.acknowledge {
                let url = reqwest::Url::parse(url).unwrap();
                assert_eq!(url.host_str(), Some("new"));
                assert_eq!(url.query_pairs().count(), 1);
                let path = url.query_pairs().next().unwrap().1.into_owned();
                snapshot
                    .active
                    .push(project("native default title", &[path], 0).unwrap());
            }
            Ok(())
        }
    }

    async fn run(acknowledge: bool, switch_owner: bool, closing: bool) -> (RestoreOutcome, usize) {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("pool.sqlite")).unwrap();
        let entries = vec![fixture(root.path(), "one & #"), fixture(root.path(), "二")];
        remember(
            &store,
            &NativeSnapshot {
                active: entries,
                ..NativeSnapshot::default()
            },
        )
        .unwrap();
        let adapter = Adapter {
            snapshot: Mutex::new(NativeSnapshot {
                owner_hash: Some("a".repeat(64)),
                account_scoped: true,
                ..NativeSnapshot::default()
            }),
            opened: Mutex::new(Vec::new()),
            closing: AtomicBool::new(closing),
            acknowledge,
            switch_owner,
        };
        let restorer = Restorer {
            confirmation_timeout: Duration::from_millis(15),
            poll_interval: Duration::from_millis(1),
            ..Restorer::default()
        };
        let result = restorer.restore(&store, &adapter).await;
        let calls = adapter.opened.lock().unwrap().len();
        if closing {
            assert!(result.is_err());
            return (RestoreOutcome::default(), calls);
        }
        (result.unwrap(), calls)
    }

    #[tokio::test]
    async fn explicit_restore_waits_for_native_directory_ack_and_never_sends_text() {
        let (outcome, calls) = run(true, false, false).await;
        assert_eq!(outcome.restored, 2);
        assert_eq!(outcome.remaining, 0);
        assert_eq!(calls, 2);
    }

    #[tokio::test]
    async fn unresolved_open_is_not_replayed_and_scope_or_close_stops_remaining_entries() {
        let (outcome, calls) = run(false, false, false).await;
        assert!(outcome.pending_confirmation);
        assert_eq!(calls, 1);
        assert_eq!(outcome.restored, 0);
        let (outcome, calls) = run(true, true, false).await;
        assert!(outcome.stopped);
        assert_eq!(calls, 1);
        let (_, calls) = run(true, false, true).await;
        assert_eq!(calls, 0);
    }
}
