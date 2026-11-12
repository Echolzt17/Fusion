use crate::{storage::Store, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
};
use toml_edit::{value, DocumentMut, Item, Table};

const PROVIDER: &str = "personal_codex_pool";
mod recovery;
mod protected;
pub use recovery::{apply_recovery, detached, recovery_preview, RecoveryPlan, RecoveryStrategy};
pub(crate) use recovery::{restore_for_close, verify_closed};
pub fn recovery_transaction_pending(path: &Path) -> bool {
    path.with_file_name("personal-codex-pool-recovery.json")
        .exists()
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub path: String,
    pub before: String,
    pub after: String,
    pub fingerprint: String,
    pub already_applied: bool,
}
#[derive(Clone, Serialize, Deserialize)]
struct Backup {
    original_selection: String,
    applied_selection: String,
    #[serde(with = "protected")]
    original_provider: String,
    #[serde(with = "protected")]
    applied_provider: String,
    #[serde(default)]
    original_profile: Option<String>,
    #[serde(default)]
    applied_profile: Option<String>,
    #[serde(default)]
    original_image_feature: Option<String>,
    #[serde(default)]
    applied_image_feature: Option<String>,
    #[serde(default)]
    original_compression: Option<String>,
    #[serde(default)]
    applied_compression: Option<String>,
    #[serde(default)]
    previous: Option<Owned>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Owned {
    selection: String,
    #[serde(with = "protected")]
    provider: String,
    image_feature: String,
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    compression: Option<String>,
}
fn image_feature(doc: &DocumentMut) -> String {
    let mut d = DocumentMut::new();
    if let Some(v) = doc.get("features").and_then(|v| v.get("image_generation")) {
        d["features"] = Item::Table(Table::new());
        d["features"]["image_generation"] = v.clone();
    }
    d.to_string()
}
fn compression_fields_doc(doc: &DocumentMut) -> DocumentMut {
    let mut d = DocumentMut::new();
    if let Some(v) = doc.get("features").and_then(|f| f.get("enable_request_compression")) {
        d["features"]["enable_request_compression"] = v.clone();
    }
    if let Ok(Some((name, p))) = recovery::active(doc) {
        d["profile"] = value(name);
        if let Some(v) = p.get("features").and_then(|f| f.get("enable_request_compression")) {
            d["profiles"][name]["features"]["enable_request_compression"] = v.clone();
        }
    }
    d
}
fn compression_fields(doc: &DocumentMut) -> String { compression_fields_doc(doc).to_string() }
fn restore_compression(doc: &mut DocumentMut, saved: &str) -> Result<()> {
    let source = parse(saved)?;
    if let Some(v) = source.get("features").and_then(|f| f.get("enable_request_compression")) {
        doc["features"]["enable_request_compression"] = v.clone();
    } else if let Some(features) = doc.get_mut("features").and_then(Item::as_table_like_mut) {
        features.remove("enable_request_compression");
        if features.is_empty() { doc.remove("features"); }
    }
    if let Some(name) = source.get("profile").and_then(Item::as_str) {
        let original = source.get("profiles").and_then(|p| p.get(name)).and_then(|p| p.get("features"))
            .and_then(|f| f.get("enable_request_compression"));
        if let Some(profile) = doc.get_mut("profiles").and_then(|p| p.get_mut(name)).and_then(Item::as_table_like_mut) {
            if let Some(v) = original {
                if !profile.contains_key("features") { profile.insert("features", Item::Table(Table::new())); }
                profile.get_mut("features").and_then(Item::as_table_like_mut).ok_or("原 profile 功能配置无效")?
                    .insert("enable_request_compression", v.clone());
            } else if let Some(features) = profile.get_mut("features").and_then(Item::as_table_like_mut) {
                features.remove("enable_request_compression");
                if features.is_empty() { profile.remove("features"); }
            }
        }
    }
    Ok(())
}
fn managed_matches(doc: &DocumentMut, b: &Backup) -> bool {
    equivalent(&selected(doc), &b.applied_selection)
        && equivalent(&provider(doc), &b.applied_provider)
        && b.applied_profile
            .as_ref()
            .is_none_or(|p| equivalent(&profile_selection(doc), p))
        && b.applied_image_feature
            .as_ref()
            .is_none_or(|f| equivalent(&image_feature(doc), f))
        && b.applied_compression.as_ref().is_none_or(|c| equivalent(&compression_fields(doc), c))
}
fn previous_matches(doc: &DocumentMut, b: &Backup) -> bool {
    b.previous.as_ref().is_some_and(|p| {
        equivalent(&selected(doc), &p.selection)
            && equivalent(&provider(doc), &p.provider)
            && equivalent(&image_feature(doc), &p.image_feature)
            && p.profile
                .as_ref()
                .is_none_or(|p| equivalent(&profile_selection(doc), p))
            && p.compression.as_ref().is_none_or(|c| equivalent(&compression_fields(doc), c))
    })
}
fn backup(store: &Store) -> Result<Option<Backup>> {
    store
        .get("codex_config_backup")?
        .filter(|s| !s.is_empty())
        .map(|s| serde_json::from_str(&s).map_err(|_| "接入备份损坏".into()))
        .transpose()
}
fn retained_matches(store: &Store, doc: &DocumentMut) -> Result<bool> {
    Ok(store
        .get("codex_retained_provider")?
        .filter(|s| !s.is_empty())
        .map(|s| protected::open(&s).map(|s| equivalent(&provider(doc), &s)))
        .transpose()?.unwrap_or(false))
}
fn fragment(doc: &DocumentMut) -> String {
    let mut redacted = doc.clone();
    if let Some(headers) = redacted.get_mut("model_providers")
        .and_then(|p| p.get_mut(PROVIDER)).and_then(|p| p.get_mut("http_headers"))
        .and_then(Item::as_table_like_mut) {
        if headers.contains_key("x-fusion-client-key") {
            headers.insert("x-fusion-client-key", value("<本机接入密钥已隐藏>"));
        }
    }
    let mut output = compression_fields_doc(&redacted);
    if let Some(v) = redacted.get("model_provider") { output["model_provider"] = v.clone(); }
    if let Ok(Some((name, p))) = recovery::active(&redacted) {
        if let Some(v) = p.get("model_provider") { output["profiles"][name]["model_provider"] = v.clone(); }
    }
    if let Some(v) = redacted.get("features").and_then(|f| f.get("image_generation")) {
        output["features"]["image_generation"] = v.clone();
    }
    if let Some(v) = redacted.get("model_providers").and_then(|p| p.get(PROVIDER)) { output["provider"] = v.clone(); }
    output.to_string()
}
pub fn image_ready(store: &Store, path: &Path) -> Result<bool> {
    let d = parse(&read(path)?)?;
    // Use get chains: an unconfigured document has no tables to index.
    let v: serde_json::Value =
        toml_edit::de::from_str(&d.to_string()).map_err(|_| "配置格式错误")?;
    Ok(is_applied(store, path)?
        && v["features"]["image_generation"] == true
        && v["features"]["enable_request_compression"] == false
        && v["model_providers"][PROVIDER]["http_headers"]["x-openai-actor-authorization"]
            == "fusion-local"
        && v["model_providers"][PROVIDER]["stream_idle_timeout_ms"] == 600000
        && v["model_providers"][PROVIDER]["requires_openai_auth"] == true
        && v["model_providers"][PROVIDER]["capabilities"]["remote_compaction"] == "unsupported"
        && v["model_providers"][PROVIDER]["http_headers"]["x-fusion-client-key"].as_str().is_some_and(|s| !s.is_empty())
        && v["model_providers"][PROVIDER]["auth"].is_null())
}
pub fn codex_config_path() -> Result<PathBuf> {
    let root = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join(".codex")))
        .ok_or("找不到 Codex 配置目录")?;
    Ok(root.join("config.toml"))
}
pub fn has_backup(store: &Store) -> Result<bool> {
    Ok(store
        .get("codex_config_backup")?
        .is_some_and(|s| !s.is_empty()))
}
// Take over provider selection, including an active profile, without touching external credentials.
pub fn require_takeover_ready(store: &Store, path: &Path, exe: &Path) -> Result<()> {
    let doc = parse(&read(path)?)?;
    recovery::provider_name(&doc)?;
    if backup(store)?.is_none() && !recovery::detached_doc(&doc, exe, store.settings()?.port)? {
        return Err("检测到遗留账号池接入且恢复记录缺失，请先通过恢复入口处理".into());
    }
    Ok(())
}
fn read(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(_) => Err("无法读取 Codex 配置".into()),
    }
}
fn parse(s: &str) -> Result<DocumentMut> {
    s.parse()
        .map_err(|_| "现有 Codex 配置不是有效 TOML，未进行修改".into())
}
fn hash(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}
fn preview_fingerprint(
    source: &str,
    path: &Path,
    exe: &Path,
    port: u16,
    generated: &str,
) -> String {
    hash(
        &serde_json::json!([
            source,
            path.to_string_lossy(),
            exe.to_string_lossy(),
            port,
            generated
        ])
        .to_string(),
    )
}
fn selected(doc: &DocumentMut) -> String {
    let mut d = DocumentMut::new();
    if let Some(v) = doc.get("model_provider") {
        d["model_provider"] = v.clone();
    }
    d.to_string()
}
// Whitelist only routing fields. Never copy a profile's headers, keys, or other settings.
fn profile_selection(doc: &DocumentMut) -> String {
    let mut d = DocumentMut::new();
    if let Some(v) = doc.get("profile") {
        d["profile"] = v.clone();
    }
    if let Ok(Some((name, p))) = recovery::active(doc) {
        if let Some(v) = p.get("model_provider") {
            d["profiles"][name]["model_provider"] = v.clone();
        }
    }
    d.to_string()
}
fn provider(doc: &DocumentMut) -> String {
    let mut d = DocumentMut::new();
    if let Some(v) = doc.get("model_providers").and_then(|v| v.get(PROVIDER)) {
        d["provider"] = v.clone();
    }
    d.to_string()
}
fn equivalent(a: &str, b: &str) -> bool {
    match (
        toml_edit::de::from_str::<serde_json::Value>(a),
        toml_edit::de::from_str::<serde_json::Value>(b),
    ) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}
fn install(doc: &mut DocumentMut, port: u16, client_key: &str) -> Result<()> {
    recovery::provider_name(doc)?;
    let active_profile = recovery::active(doc)?.map(|(name, _)| name.to_owned());
    if doc.get("model_providers").is_some_and(|v| v.as_table_like().is_none()) {
        return Err("model_providers 使用了不支持的格式；请手动合并 provider".into());
    }
    if doc
        .get("features")
        .is_some_and(|v| v.as_table_like().is_none())
    {
        return Err("features 使用了不支持的格式，未修改配置".into());
    }
    if doc.get("features").is_none() {
        doc["features"] = Item::Table(Table::new());
    }
    doc["features"]["image_generation"] = value(true);
    doc["features"]["enable_request_compression"] = value(false);
    doc["model_provider"] = value(PROVIDER);
    // The top-level selection now applies to the active profile as well.
    if let Some(name) = active_profile {
        if doc["profiles"][&name].get("features").is_some_and(|f| f.as_table_like().is_none()) {
            return Err("当前 profile 的 features 格式无效，未修改接入".into());
        }
        doc["profiles"][&name]
            .as_table_like_mut()
            .ok_or("当前 profile 格式无效")?
            .remove("model_provider");
        if let Some(features) = doc["profiles"][&name].get_mut("features").and_then(Item::as_table_like_mut) {
            features.remove("enable_request_compression");
        }
    }
    if doc.get("model_providers").is_none() {
        doc["model_providers"] = Item::Table(Table::new());
    }
    let p = provider_config(
        &format!("http://127.0.0.1:{port}/v1"),
        client_key,
    )?;
    // Inline tables must contain Values; converting only our generated subtree
    // preserves every unrelated provider and the outer table's representation.
    set_provider(doc, Item::Table(p))?;
    Ok(())
}
fn set_provider(doc: &mut DocumentMut, item: Item) -> Result<()> {
    if doc.get("model_providers").is_none() {
        doc["model_providers"] = Item::Table(Table::new());
    }
    let item = if doc["model_providers"].is_inline_table() {
        Item::Value(item.into_value().map_err(|_| "provider 格式无效")?)
    } else { item };
    doc["model_providers"].as_table_like_mut().ok_or("model_providers 格式无效")?
        .insert(PROVIDER, item);
    Ok(())
}
// The automatic isolated check exercises the same provider settings we install.
pub(crate) fn provider_config(base_url: &str, client_key: &str) -> Result<Table> {
    let endpoint = url::Url::parse(base_url).map_err(|_| "本机接入地址无效")?;
    if endpoint.scheme() != "http" || endpoint.host_str() != Some("127.0.0.1")
        || client_key.is_empty() || client_key.contains(['\r', '\n']) {
        return Err("本机接入地址或密钥无效，未修改配置".into());
    }
    let mut p = Table::new();
    // This is the model vendor's display name. The independent routing id
    // remains personal_codex_pool so a closed pool cannot route its encrypted
    // conversations through a different personal OpenAI account.
    p["name"] = value("OpenAI");
    p["base_url"] = value(base_url);
    p["wire_api"] = value("responses");
    // Keep the desktop's existing ChatGPT identity and its account-targeted UI.
    // Its Authorization is discarded at ingress; only this separate local key
    // authenticates requests to Fusion. Never combine command auth with this flag.
    p["requires_openai_auth"] = value(true);
    p["supports_websockets"] = value(false);
    // Disable client-side replay as well: an interrupted turn may already have executed.
    p["request_max_retries"] = value(0);
    p["stream_max_retries"] = value(0);
    p["stream_idle_timeout_ms"] = value(600000);
    let mut capabilities = Table::new();
    capabilities["remote_compaction"] = value("unsupported");
    p["capabilities"] = Item::Table(capabilities);
    let mut headers = Table::new();
    headers["x-openai-actor-authorization"] = value("fusion-local");
    headers["x-fusion-client-key"] = value(client_key);
    p["http_headers"] = Item::Table(headers);
    Ok(p)
}
pub fn preview(store: &Store, path: &Path, exe: &Path, client_key: &str) -> Result<Preview> {
    require_takeover_ready(store, path, exe)?;
    let before = read(path)?;
    let mut doc = parse(&before)?;
    if let Some(b) = backup(store)? {
        if !managed_matches(&doc, &b) && !previous_matches(&doc, &b) {
            return Err("接入字段已被其他程序修改，未覆盖；请核对配置".into());
        }
    } else if !provider(&doc).is_empty() && !retained_matches(store, &doc)? {
        return Err("同名 provider 已存在，未覆盖".into());
    }
    let before_fragment = fragment(&doc);
    let port = store.settings()?.port;
    install(&mut doc, port, client_key)?;
    Ok(Preview {
        path: path.display().to_string(),
        before: before_fragment,
        after: fragment(&doc),
        fingerprint: preview_fingerprint(&before, path, exe, port, &doc.to_string()),
        already_applied: store
            .get("codex_config_backup")?
            .is_some_and(|v| !v.is_empty()),
    })
}
pub fn is_applied(store: &Store, path: &Path) -> Result<bool> {
    let Some(raw) = store.get("codex_config_backup")?.filter(|s| !s.is_empty()) else {
        return Ok(false);
    };
    let backup: Backup = serde_json::from_str(&raw).map_err(|_| "接入备份损坏")?;
    let doc = parse(&read(path)?)?;
    Ok(managed_matches(&doc, &backup) || previous_matches(&doc, &backup))
}
fn locked(path: &Path) -> Result<std::fs::File> {
    let parent = path.parent().ok_or("配置路径无效")?;
    fs::create_dir_all(parent).map_err(|_| "无法创建配置目录")?;
    let f = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(parent.join("personal-codex-pool-config.lock"))
        .map_err(|_| "无法创建配置锁")?;
    f.try_lock_exclusive()
        .map_err(|_| "另一个账号池操作正在修改配置，请稍后重试")?;
    Ok(f)
}
fn write(path: &Path, expected: &str, updated: &str) -> Result<()> {
    if read(path)? != expected {
        return Err("配置在操作期间被其他程序修改，请重试操作".into());
    }
    // Same-directory temp file + replacement: no truncate-in-place loss on process interruption.
    let tmp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        use std::io::Write;
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|_| "无法写入临时配置")?;
        f.write_all(updated.as_bytes())
            .and_then(|_| f.sync_all())
            .map_err(|_| "保存配置失败")?;
        drop(f);
        if read(path)? != expected {
            return Err("配置已变化，请重试操作".into());
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            #[link(name = "kernel32")]
            extern "system" {
                fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
            }
            let from: Vec<u16> = tmp.as_os_str().encode_wide().chain(Some(0)).collect();
            let to: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 1 | 8) } == 0 {
                return Err("无法替换 Codex 配置，请关闭占用文件的程序后重试".into());
            }
        }
        #[cfg(not(windows))]
        fs::rename(&tmp, path).map_err(|_| "无法替换配置")?;
        Ok(())
    })();
    let _ = fs::remove_file(tmp);
    result
}
pub fn apply(store: &Store, path: &Path, exe: &Path, client_key: &str, fingerprint: &str) -> Result<()> {
    let _lock = locked(path)?;
    require_takeover_ready(store, path, exe)?;
    let source = read(path)?;
    let port = store.settings()?.port;
    let mut generated = parse(&source)?;
    install(&mut generated, port, client_key)?;
    if preview_fingerprint(&source, path, exe, port, &generated.to_string()) != fingerprint {
        return Err("配置在预览后已发生变化，请重试操作".into());
    }
    let mut doc = parse(&source)?;
    let old_raw = store.get("codex_config_backup")?.unwrap_or_default();
    let mut b = if let Some(b) = backup(store)? {
        if !managed_matches(&doc, &b) && !previous_matches(&doc, &b) {
            return Err("接入字段在应用后被修改，未覆盖".into());
        }
        b
    } else {
        if !provider(&doc).is_empty() && !retained_matches(store, &doc)? {
            return Err("同名 provider 已存在，未覆盖".into());
        }
        Backup {
            original_selection: selected(&doc),
            original_provider: provider(&doc),
            applied_selection: String::new(),
            applied_provider: String::new(),
            original_profile: None,
            applied_profile: None,
            original_image_feature: None,
            applied_image_feature: None,
            original_compression: None,
            applied_compression: None,
            previous: None,
        }
    };
    if b.original_image_feature.is_none() {
        b.original_image_feature = Some(image_feature(&doc));
    }
    if b.original_profile.is_none() {
        b.original_profile = Some(profile_selection(&doc));
    }
    if b.original_compression.is_none() { b.original_compression = Some(compression_fields(&doc)); }
    b.previous = Some(Owned {
        selection: selected(&doc),
        provider: provider(&doc),
        image_feature: image_feature(&doc),
        profile: Some(profile_selection(&doc)),
        compression: Some(compression_fields(&doc)),
    });
    install(&mut doc, port, client_key)?;
    b.applied_selection = selected(&doc);
    b.applied_provider = provider(&doc);
    b.applied_image_feature = Some(image_feature(&doc));
    b.applied_profile = Some(profile_selection(&doc));
    b.applied_compression = Some(compression_fields(&doc));
    store.set("codex_config_backup", &serde_json::to_string(&b).map_err(|_| "无法保护接入恢复记录")?)?;
    if let Err(e) = write(path, &source, &doc.to_string()) {
        store.set("codex_config_backup", &old_raw)?;
        return Err(e);
    }
    // A crash before cleanup can restore either the previous or the new owned fields.
    b.previous = None;
    store.set("codex_config_backup", &serde_json::to_string(&b).map_err(|_| "无法保护接入恢复记录")?)?;
    Ok(())
}
fn restored(source: &str, b: &Backup) -> Result<DocumentMut> {
    let mut doc = parse(source)?;
    // A crash after backing up but before writing is also recoverable without touching the config.
    if equivalent(&selected(&doc), &b.original_selection)
        && (equivalent(&provider(&doc), &b.original_provider)
            || equivalent(&provider(&doc), &b.applied_provider))
        && b.original_profile
            .as_ref()
            .is_none_or(|p| equivalent(&profile_selection(&doc), p))
        && b.original_image_feature
            .as_ref()
            .is_none_or(|f| equivalent(&image_feature(&doc), f))
        && b.original_compression.as_ref().is_none_or(|c| equivalent(&compression_fields(&doc), c))
    {
        // Older Fusion builds removed this table before a failed desktop restart.
        // Rebuild only the definition recorded by our own takeover; no credentials
        // or external provider tables are copied into the saved thread's route.
        if let Some(p) = parse(&b.applied_provider)?.get("provider") {
            set_provider(&mut doc, p.clone())?;
        }
        return Ok(doc);
    }
    if !managed_matches(&doc, b) && !previous_matches(&doc, b) {
        return Err("接入字段在应用后被修改，自动恢复已停止；其他设置未被覆盖".into());
    }
    let original = parse(&b.original_selection)?;
    match original.get("model_provider") {
        Some(v) => doc["model_provider"] = v.clone(),
        None => {
            doc.remove("model_provider");
        }
    }
    if let Some(original) = &b.original_profile {
        let original = parse(original)?;
        if let Some(name) = original.get("profile").and_then(Item::as_str) {
            let provider = original
                .get("profiles")
                .and_then(|p| p.get(name))
                .and_then(|p| p.get("model_provider"));
            if let Some(profile) = doc
                .get_mut("profiles")
                .and_then(|p| p.get_mut(name))
                .and_then(Item::as_table_like_mut)
            {
                match provider {
                    Some(p) => {
                        profile.insert("model_provider", p.clone());
                    }
                    None => {
                        profile.remove("model_provider");
                    }
                }
            }
        }
    }
    // Saved threads resolve their original provider even when another provider is
    // selected by default. Keep this definition: never route those threads through
    // the restored origin or make config loading fail with "provider not found".
    if let Some(feature) = &b.original_image_feature {
        let original = parse(feature)?;
        if let Some(v) = original
            .get("features")
            .and_then(|f| f.get("image_generation"))
        {
            doc["features"]["image_generation"] = v.clone();
        } else if let Some(features) = doc.get_mut("features").and_then(Item::as_table_like_mut) {
            features.remove("image_generation");
            if features.is_empty() {
                doc.remove("features");
            }
        }
    }
    if let Some(saved) = &b.original_compression { restore_compression(&mut doc, saved)?; }
    Ok(doc)
}
#[cfg(test)]
mod image_tests {
    use super::*;
    use crate::tests::fixture;
    #[test]
    fn desktop_identity_config_protects_local_key_and_keeps_original_login() {
        let (d, a) = fixture();
        let path = d.path().join("config.toml");
        let exe = d.path().join("pool.exe");
        let key = a.api_key().unwrap();
        fs::write(&path, "model_provider='openai'\n[projects.fixture]\ntrust_level='trusted'\n").unwrap();
        fs::write(d.path().join("auth.json"), "public-fixture-login").unwrap();
        let p = preview(&a.store, &path, &exe, &key).unwrap();
        assert!(!p.before.contains(&key) && !p.after.contains(&key));
        assert!(apply(&a.store, &path, &exe, "different-local-key", &p.fingerprint).is_err());
        apply(&a.store, &path, &exe, &key, &p.fingerprint).unwrap();
        let doc = parse(&read(&path).unwrap()).unwrap();
        let provider = &doc["model_providers"][PROVIDER];
        assert_eq!(doc["model_provider"].as_str(), Some(PROVIDER));
        assert_eq!(provider["name"].as_str(), Some("OpenAI"));
        assert_eq!(provider["capabilities"]["remote_compaction"].as_str(), Some("unsupported"));
        assert_eq!(provider["requires_openai_auth"].as_bool(), Some(true));
        assert!(provider.get("auth").is_none());
        assert_eq!(provider["http_headers"]["x-fusion-client-key"].as_str(), Some(key.as_str()));
        let saved = a.store.get("codex_config_backup").unwrap().unwrap();
        assert!(!saved.contains(&key));
        assert!(saved.contains("fusion-dpapi-v1:"));
        assert!(managed_matches(&doc, &backup(&a.store).unwrap().unwrap()));
        restore_for_close(&a.store, &path, &exe).unwrap();
        assert!(!a.store.get("codex_retained_provider").unwrap().unwrap().contains(&key));
        assert_eq!(parse(&read(&path).unwrap()).unwrap()["projects"]["fixture"]["trust_level"].as_str(), Some("trusted"));
        assert_eq!(fs::read_to_string(d.path().join("auth.json")).unwrap(), "public-fixture-login");
        assert!(protected::open("fusion-dpapi-v1:invalid").is_err());
    }
    #[test]
    fn compression_override_restores_original_profile_and_refuses_owned_edits() {
        let (d, a) = fixture();
        let path = d.path().join("config.toml");
        let exe = d.path().join("pool.exe");
        let source = "model_provider='openai'\nprofile='work'\n[features]\nenable_request_compression=true\nkeep=true\n[profiles.work.features]\nenable_request_compression=true\nother=true\n";
        fs::write(&path, source).unwrap();
        let key = a.api_key().unwrap();
        let p = preview(&a.store, &path, &exe, &key).unwrap();
        assert!(parse(&p.after).is_ok(), "preview must remain a valid merged fragment");
        apply(&a.store, &path, &exe, &key, &p.fingerprint).unwrap();
        let applied = parse(&read(&path).unwrap()).unwrap();
        assert_eq!(applied["model_provider"].as_str(), Some(PROVIDER));
        assert_eq!(applied["features"]["enable_request_compression"].as_bool(), Some(false));
        assert!(applied["profiles"]["work"]["features"].get("enable_request_compression").is_none());
        for profile in [false, true] {
            let mut changed = applied.clone();
            if profile { changed["profiles"]["work"]["features"]["enable_request_compression"] = value(true); }
            else { changed["features"]["enable_request_compression"] = value(true); }
            fs::write(&path, changed.to_string()).unwrap();
            assert!(!is_applied(&a.store, &path).unwrap());
            assert!(restore_for_close(&a.store, &path, &exe).is_err());
            assert_eq!(read(&path).unwrap(), changed.to_string());
        }
        let mut unrelated = applied;
        unrelated["features"]["keep"] = value(false);
        fs::write(&path, unrelated.to_string()).unwrap();
        restore_for_close(&a.store, &path, &exe).unwrap();
        verify_closed(&a.store, &path, &exe).unwrap();
        let restored = parse(&read(&path).unwrap()).unwrap();
        assert_eq!(restored["model_provider"].as_str(), Some("openai"));
        assert_eq!(restored["features"]["enable_request_compression"].as_bool(), Some(true));
        assert_eq!(restored["profiles"]["work"]["features"]["enable_request_compression"].as_bool(), Some(true));
        assert_eq!(restored["features"]["keep"].as_bool(), Some(false));
        assert_eq!(restored["profiles"]["work"]["features"]["other"].as_bool(), Some(true));
    }
    #[test]
    fn inline_providers_preserve_custom_profile_and_detect_external_edits() {
        let (d, a) = fixture(); let path = d.path().join("config.toml"); let exe = d.path().join("pool.exe");
        let original = "model='keep'\nmodel_provider='custom'\nprofile='work'\nmodel_providers={custom={name='Fixture',base_url='http://127.0.0.1:9/v1',wire_api='responses'}}\nprofiles={work={model_provider='custom',other=true}}\nfeatures={image_generation=false,other=true}\n";
        fs::write(&path, original).unwrap();
        let custom = parse(original).unwrap()["model_providers"]["custom"].to_string();
        for _ in 0..2 {
            let p = preview(&a.store, &path, &exe, &a.api_key().unwrap()).unwrap();
            fs::write(&path, format!("{}\n# concurrent edit\n", read(&path).unwrap())).unwrap();
            assert!(apply(&a.store, &path, &exe, &a.api_key().unwrap(), &p.fingerprint).is_err());
            let p = preview(&a.store, &path, &exe, &a.api_key().unwrap()).unwrap(); apply(&a.store, &path, &exe, &a.api_key().unwrap(), &p.fingerprint).unwrap();
            let mut doc = parse(&read(&path).unwrap()).unwrap();
            assert_eq!(doc["model_providers"]["custom"].to_string(), custom);
            assert!(image_ready(&a.store, &path).unwrap());
            doc["model"] = value("external-model"); fs::write(&path, doc.to_string()).unwrap();
            assert!(restore_for_close(&a.store, &path, &exe).unwrap());
            verify_closed(&a.store, &path, &exe).unwrap(); a.store.finish_client_switch(true).unwrap();
            let doc = parse(&read(&path).unwrap()).unwrap();
            assert_eq!(recovery::provider_name(&doc).unwrap(), "custom");
            assert_eq!(doc["profiles"]["work"]["model_provider"].as_str(), Some("custom"));
            assert_eq!(doc["features"]["image_generation"].as_bool(), Some(false));
            assert_eq!(doc["model"].as_str(), Some("external-model"));
        }
        let p = preview(&a.store, &path, &exe, &a.api_key().unwrap()).unwrap(); apply(&a.store, &path, &exe, &a.api_key().unwrap(), &p.fingerprint).unwrap();
        let mut doc = parse(&read(&path).unwrap()).unwrap(); doc["model_providers"][PROVIDER]["base_url"] = value("http://127.0.0.1:99/v1");
        fs::write(&path, doc.to_string()).unwrap();
        assert!(restore_for_close(&a.store, &path, &exe).is_err());
    }
    #[test]
    fn retained_provider_survives_close_reenable_and_rejects_external_edits() {
        let (d, a) = fixture();
        let path = d.path().join("config.toml");
        let exe = d.path().join("pool.exe");
        fs::write(&path, "model_provider='custom'\n[model_providers.custom]\nbase_url='https://fixture.invalid/v1'\n").unwrap();
        let p = preview(&a.store, &path, &exe, &a.api_key().unwrap()).unwrap();
        apply(&a.store, &path, &exe, &a.api_key().unwrap(), &p.fingerprint).unwrap();
        let definition = provider(&parse(&read(&path).unwrap()).unwrap());
        assert!(restore_for_close(&a.store, &path, &exe).unwrap());
        // Reproduce 0.1.4's interrupted close: origin restored, pool definition deleted.
        let mut old_close = parse(&read(&path).unwrap()).unwrap();
        old_close["model_providers"]
            .as_table_like_mut()
            .unwrap()
            .remove(PROVIDER);
        fs::write(&path, old_close.to_string()).unwrap();
        // Retrying after a failed desktop restart is idempotent.
        assert!(restore_for_close(&a.store, &path, &exe).unwrap());
        verify_closed(&a.store, &path, &exe).unwrap();
        a.store.set("codex_config_backup", "").unwrap();
        let doc = parse(&read(&path).unwrap()).unwrap();
        assert_eq!(recovery::provider_name(&doc).unwrap(), "custom");
        assert!(equivalent(&provider(&doc), &definition));
        assert!(!is_applied(&a.store, &path).unwrap());
        let mut settings = a.store.settings().unwrap();
        settings.port += 1;
        a.store.save_settings(&settings).unwrap();
        let moved = d.path().join("installed-pool.exe");
        let p = preview(&a.store, &path, &moved, &a.api_key().unwrap()).unwrap();
        apply(&a.store, &path, &moved, &a.api_key().unwrap(), &p.fingerprint).unwrap();
        crate::tests::recover(&a.store, &path).unwrap();
        let restored = read(&path).unwrap();
        assert!(restored.contains("requires_openai_auth = true"));
        assert!(detached(&path, &moved, settings.port).unwrap());
        fs::write(
            &path,
            restored.replace("supports_websockets = false", "supports_websockets = true"),
        )
        .unwrap();
        assert!(preview(&a.store, &path, &moved, &a.api_key().unwrap()).is_err());
        assert!(!has_backup(&a.store).unwrap());
    }
    #[test]
    fn preview_binds_target_executable_port_and_generated_content() {
        let (d, a) = fixture();
        let path = d.path().join("config.toml");
        let exe = d.path().join("pool.exe");
        let p = preview(&a.store, &path, &exe, &a.api_key().unwrap()).unwrap();
        let mut settings = a.store.settings().unwrap();
        settings.port += 1;
        a.store.save_settings(&settings).unwrap();
        assert!(apply(&a.store, &path, &exe, &a.api_key().unwrap(), &p.fingerprint).is_err());
        assert!(!path.exists());
        let p = preview(&a.store, &path, &exe, &a.api_key().unwrap()).unwrap();
        assert!(apply(&a.store, &path, &d.path().join("moved.exe"), &a.api_key().unwrap(), &p.fingerprint).is_err());
        assert!(apply(&a.store, &d.path().join("other.toml"), &exe, &a.api_key().unwrap(), &p.fingerprint).is_err());
        assert!(a.store.get("codex_config_backup").unwrap().is_none());
        apply(&a.store, &path, &exe, &a.api_key().unwrap(), &p.fingerprint).unwrap();
        assert!(read(&path).unwrap().contains(&settings.port.to_string()));
    }
    #[test]
    fn legacy_upgrade_preserves_original_restore_and_unrelated_features() {
        let (d, a) = fixture();
        let path = d.path().join("config.toml");
        let exe = d.path().join("pool.exe");
        let original="model_provider='old'\nmodel='keep-model'\n[features]\nimage_generation=false\nother=true\n";
        std::fs::write(&path, original).unwrap();
        let mut doc = parse(original).unwrap();
        let original_selection = selected(&doc);
        install(&mut doc, 18765, &a.api_key().unwrap()).unwrap();
        doc["features"]["image_generation"] = value(false);
        let p = doc["model_providers"][PROVIDER].as_table_mut().unwrap();
        p.remove("http_headers");
        p.remove("stream_idle_timeout_ms");
        let legacy = serde_json::json!({"original_selection":original_selection,"applied_selection":selected(&doc),"original_provider":"","applied_provider":provider(&doc)});
        a.store
            .set("codex_config_backup", &legacy.to_string())
            .unwrap();
        std::fs::write(&path, doc.to_string()).unwrap();
        let preview = preview(&a.store, &path, &exe, &a.api_key().unwrap()).unwrap();
        assert!(preview.already_applied);
        apply(&a.store, &path, &exe, &a.api_key().unwrap(), &preview.fingerprint).unwrap();
        assert!(image_ready(&a.store, &path).unwrap());
        // Repeated upgrades must never replace the original restore target.
        let preview = super::preview(&a.store, &path, &exe, &a.api_key().unwrap()).unwrap();
        apply(&a.store, &path, &exe, &a.api_key().unwrap(), &preview.fingerprint).unwrap();
        crate::tests::recover(&a.store, &path).unwrap();
        let restored = parse(&read(&path).unwrap()).unwrap();
        assert_eq!(restored["model_provider"].as_str(), Some("old"));
        assert_eq!(
            restored["features"]["image_generation"].as_bool(),
            Some(false)
        );
        assert_eq!(restored["features"]["other"].as_bool(), Some(true));
        assert_eq!(restored["model"].as_str(), Some("keep-model"));
    }
    #[test]
    fn feature_conflict_and_interrupted_upgrade_are_recoverable() {
        let (d, a) = fixture();
        let path = d.path().join("config.toml");
        let exe = d.path().join("pool.exe");
        let p = preview(&a.store, &path, &exe, &a.api_key().unwrap()).unwrap();
        apply(&a.store, &path, &exe, &a.api_key().unwrap(), &p.fingerprint).unwrap();
        let applied = read(&path).unwrap();
        std::fs::write(
            &path,
            applied.replace("image_generation = true", "image_generation = false"),
        )
        .unwrap();
        assert!(preview(&a.store, &path, &exe, &a.api_key().unwrap()).is_err());
        assert!(crate::tests::recover(&a.store, &path).is_err());
        std::fs::write(&path, &applied).unwrap();
        let mut b = backup(&a.store).unwrap().unwrap();
        let doc = parse(&applied).unwrap();
        b.previous = Some(Owned {
            selection: selected(&doc),
            provider: provider(&doc),
            image_feature: image_feature(&doc),
            profile: Some(profile_selection(&doc)),
            compression: Some(compression_fields(&doc)),
        });
        b.applied_provider = "[provider]\nname='pending-version'".into();
        a.store
            .set("codex_config_backup", &serde_json::to_string(&b).unwrap())
            .unwrap();
        crate::tests::recover(&a.store, &path).unwrap();
        assert!(
            read(&path).unwrap().trim().is_empty()
                || !read(&path).unwrap().contains("image_generation")
        );
    }
}
