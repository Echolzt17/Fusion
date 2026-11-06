//! Selective, recoverable Codex API configuration. Never reads client credentials.
use crate::{storage::Store, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs, path::{Path, PathBuf}};
use toml_edit::{value, DocumentMut, Item, Table};

mod protected;
#[cfg(test)]
mod tests;

const BACKUP: &str = "api-connections/v1/codex-config";
const PREFIX: &str = "fusion_api_";

#[derive(Serialize, Deserialize)]
struct Backup {
    path: PathBuf,
    provider: String,
    original: String,
    applied: String,
    applied_provider: String,
    profile: Option<String>,
}

fn read(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(_) => Err("无法读取 Codex API 接入配置".into()),
    }
}
fn parse(text: &str) -> Result<DocumentMut> {
    text.parse().map_err(|_| "Codex 配置不是有效 TOML，未修改接入".into())
}
fn same(left: &str, right: &str) -> bool {
    let parse = |text: &str| toml_edit::de::from_str::<serde_json::Value>(text).ok();
    parse(left).zip(parse(right)).is_some_and(|(a,b)| a == b)
}
fn profile(doc: &DocumentMut) -> Result<Option<String>> {
    match doc.get("profile") {
        None => Ok(None),
        Some(item) => item.as_str().filter(|v| !v.is_empty())
            .map(|v| Some(v.to_owned())).ok_or_else(|| "当前 Codex profile 无效".into()),
    }
}
fn selections(doc: &DocumentMut, profile: Option<&str>) -> String {
    let mut output = DocumentMut::new();
    for key in ["model", "model_provider"] {
        if let Some(item) = doc.get(key) { output[key] = item.clone(); }
    }
    if let Some(item) = doc.get("features").and_then(|v| v.get("image_generation")) {
        output["features"] = Item::Table(Table::new());
        output["features"]["image_generation"] = item.clone();
    }
    if let Some(name) = profile {
        for key in ["model", "model_provider"] {
            if let Some(item) = doc.get("profiles").and_then(|v|v.get(name)).and_then(|v|v.get(key)) {
                output["profiles"][name][key] = item.clone();
            }
        }
    }
    output.to_string()
}
fn replace_selections(doc: &mut DocumentMut, fragment: &str, profile: Option<&str>) -> Result<()> {
    let incoming = parse(fragment)?;
    for key in ["model", "model_provider"] {
        doc.remove(key);
        if let Some(item) = incoming.get(key) { doc[key] = item.clone(); }
    }
    if let Some(features) = doc.get_mut("features").and_then(Item::as_table_like_mut) {
        features.remove("image_generation");
    }
    if let Some(item) = incoming.get("features").and_then(|v| v.get("image_generation")) {
        if doc.get("features").is_none() { doc["features"] = Item::Table(Table::new()); }
        doc["features"]["image_generation"] = item.clone();
    }
    if let Some(name) = profile {
        for key in ["model", "model_provider"] {
            if let Some(table) = doc.get_mut("profiles").and_then(|v|v.get_mut(name)).and_then(Item::as_table_like_mut) {
                table.remove(key);
            }
            if let Some(item) = incoming.get("profiles").and_then(|v|v.get(name)).and_then(|v|v.get(key)) {
                doc["profiles"][name][key] = item.clone();
            }
        }
    }
    Ok(())
}
fn provider_fragment(doc: &DocumentMut, id: &str) -> String {
    let mut fragment = DocumentMut::new();
    if let Some(item) = doc.get("model_providers").and_then(|v|v.get(id)) {
        fragment["model_providers"][id] = item.clone();
    }
    fragment.to_string()
}
fn load(store: &Store) -> Result<Option<Backup>> {
    store.get(BACKUP)?.filter(|v| !v.is_empty()).map(|raw| {
        serde_json::from_str(&protected::open(&raw)?).map_err(|_| "Codex API 恢复记录损坏".into())
    }).transpose()
}
fn persist(store: &Store, backup: &Backup) -> Result<()> {
    let json = serde_json::to_string(backup).map_err(|_| "Codex API 恢复记录编码失败")?;
    store.set(BACKUP, &protected::seal(&json)?)
}
pub fn pending(store: &Store) -> Result<bool> {
    Ok(store.get(BACKUP)?.is_some_and(|v| !v.is_empty()))
}

/// Read-only uninstall/update guard. Unselected historical definitions do not count.
pub fn detached(path: &Path) -> Result<bool> {
    let doc = parse(&read(path)?)?;
    let current_profile = profile(&doc)?;
    let selected = current_profile.as_deref().and_then(|name| doc.get("profiles")?.get(name)?.get("model_provider"))
        .or_else(||doc.get("model_provider"));
    let Some(selected) = selected else { return Ok(true); };
    let id = selected.as_str().ok_or("无法确认 Codex 当前接入来源")?;
    if id.starts_with(PREFIX) { return Ok(false); }
    // Renaming a selected provider must not bypass the local gateway guard.
    let endpoint = doc.get("model_providers").and_then(|v|v.get(id)).and_then(|v|v.get("base_url"));
    Ok(!endpoint.and_then(Item::as_str).is_some_and(|v|v.contains("/fusion-api/")))
}

pub fn provider_id(source_id: &str, revision: u64) -> String {
    let hash = format!("{:x}", Sha256::digest(source_id.as_bytes()));
    format!("{PREFIX}{}_r{revision}", &hash[..24])
}

/// Stores the first recovery target before writing a provider with only a local key.
pub fn apply(store: &Store, path: &Path, source_id: &str, revision: u64, model: &str,
             base_url: &str, local_key: &str) -> Result<()> {
    if model.is_empty() || model.len() > 200 || model.chars().any(char::is_control) {
        return Err("API 默认模型无效".into());
    }
    let url = url::Url::parse(base_url).map_err(|_| "本机 API 接入地址无效")?;
    if url.scheme() != "http" || url.host_str() != Some("127.0.0.1") || url.port().is_none()
        || !url.path().contains("/fusion-api/") || !url.username().is_empty()
        || url.password().is_some() || url.query().is_some() || url.fragment().is_some()
        || local_key.is_empty() || local_key.chars().any(char::is_control) {
        return Err("本机 API 接入参数无效".into());
    }
    let _lock = lock(path)?;
    if pending(store)? { return Err("请先完成上一次 Codex API 接入的正常关闭".into()); }
    if !detached(path)? { return Err("检测到遗留 API 接入，请先恢复原接入".into()); }
    let source = read(path)?;
    let mut doc = parse(&source)?;
    let active_profile = profile(&doc)?;
    let id = provider_id(source_id, revision);
    let retained_key = format!("api-connections/v1/provider/{id}");
    let existing = provider_fragment(&doc, &id);
    if !existing.is_empty() {
        let retained = store.get(&retained_key)?.ok_or("同名 API provider 已存在，未覆盖")?;
        if !same(&existing, &protected::open(&retained)?) {
            return Err("API provider 被外部修改，未覆盖".into());
        }
    }
    let original = selections(&doc, active_profile.as_deref());
    if doc.get("features").is_some_and(|v|v.as_table_like().is_none()) {
        return Err("features 不是有效配置表，未修改接入".into());
    }
    doc["model"] = value(model);
    doc["model_provider"] = value(&id);
    if doc.get("features").is_none() { doc["features"] = Item::Table(Table::new()); }
    doc["features"]["image_generation"] = value(false);
    if let Some(name) = active_profile.as_deref() {
        if let Some(table) = doc.get_mut("profiles").and_then(|v|v.get_mut(name)).and_then(Item::as_table_like_mut) {
            table.remove("model_provider"); table.remove("model");
        }
    }
    if doc.get("model_providers").is_none() { doc["model_providers"] = Item::Table(Table::new()); }
    if doc.get("model_providers").is_some_and(|v|v.as_table_like().is_none()) {
        return Err("model_providers 不是有效配置表，未修改接入".into());
    }
    let mut provider = Table::new();
    provider["name"] = value("Fusion API");
    provider["base_url"] = value(base_url);
    provider["wire_api"] = value("responses");
    provider["requires_openai_auth"] = value(true);
    provider["request_max_retries"] = value(0);
    provider["stream_max_retries"] = value(0);
    provider["stream_idle_timeout_ms"] = value(600000);
    provider["http_headers"]["x-fusion-client-key"] = value(local_key);
    let provider = if doc.get("model_providers").is_some_and(Item::is_inline_table) {
        Item::Value(provider.into_inline_table().into())
    } else { Item::Table(provider) };
    doc.get_mut("model_providers").and_then(Item::as_table_like_mut).ok_or("API provider 表无效")?.insert(&id, provider);
    let backup = Backup { path: path.to_owned(), provider: id,
        original, applied: selections(&doc, active_profile.as_deref()),
        applied_provider: provider_fragment(&doc, &provider_id(source_id, revision)), profile: active_profile };
    persist(store, &backup)?;
    store.set(&retained_key, &protected::seal(&backup.applied_provider)?)?;
    atomic_write(path, &source, &doc.to_string())?;
    verify_applied(store, path)
}

pub fn verify_applied(store: &Store, path: &Path) -> Result<()> {
    let backup = load(store)?.ok_or("Codex API 恢复记录缺失")?;
    let doc = parse(&read(path)?)?;
    if backup.path != path || profile(&doc)? != backup.profile
        || !same(&selections(&doc, backup.profile.as_deref()), &backup.applied)
        || !same(&provider_fragment(&doc, &backup.provider), &backup.applied_provider) {
        return Err("Codex API 接入配置已变化，请先恢复或解决冲突".into());
    }
    Ok(())
}

/// Restore only owned selections. The local provider remains for historical chats.
/// Keep the recovery record until drain and verified client exit complete.
pub fn restore(store: &Store, path: &Path) -> Result<bool> {
    let _lock = lock(path)?;
    let Some(backup) = load(store)? else {
        if detached(path)? { return Ok(false); }
        return Err("API 接入恢复记录缺失，未猜测原接入".into());
    };
    if backup.path != path { return Err("API 接入恢复路径不匹配".into()); }
    let source = read(path)?;
    let mut doc = parse(&source)?;
    if profile(&doc)? != backup.profile { return Err("Codex profile 已变化，未覆盖外部接入".into()); }
    let selected = selections(&doc, backup.profile.as_deref());
    if same(&selected, &backup.original) {
        if !detached(path)? { return Err("恢复后的接入仍依赖 API 网关".into()); }
        return Ok(true);
    }
    if !same(&selected, &backup.applied)
        || !same(&provider_fragment(&doc, &backup.provider), &backup.applied_provider) {
        return Err("API 接入字段被外部修改，原恢复记录已保留".into());
    }
    replace_selections(&mut doc, &backup.original, backup.profile.as_deref())?;
    atomic_write(path, &source, &doc.to_string())?;
    if !detached(path)? { return Err("Codex API 接入尚未脱离".into()); }
    Ok(true)
}

pub fn finish_close(store: &Store, path: &Path) -> Result<()> {
    let _lock = lock(path)?;
    if let Some(backup) = load(store)? {
        let doc = parse(&read(path)?)?;
        if backup.path != path || profile(&doc)? != backup.profile
            || !same(&selections(&doc, backup.profile.as_deref()), &backup.original) || !detached(path)? {
            return Err("关闭期间 Codex API 配置发生变化，恢复记录已保留".into());
        }
        store.set(BACKUP, "")?;
    } else if !detached(path)? { return Err("Codex API 接入尚未脱离".into()); }
    Ok(())
}

fn lock(path: &Path) -> Result<fs::File> {
    let parent = path.parent().ok_or("API 配置路径无效")?;
    fs::create_dir_all(parent).map_err(|_| "无法创建 API 配置目录")?;
    let file = fs::OpenOptions::new().create(true).truncate(false).read(true).write(true)
        .open(parent.join("personal-codex-pool-config.lock")).map_err(|_| "无法打开配置锁")?;
    file.try_lock_exclusive().map_err(|_| "另一个接入操作正在修改配置")?;
    Ok(file)
}
fn atomic_write(path: &Path, expected: &str, next: &str) -> Result<()> {
    use std::io::Write;
    if read(path)? != expected { return Err("配置在切换期间被修改".into()); }
    let mut temp = tempfile::NamedTempFile::new_in(path.parent().ok_or("配置路径无效")?)
        .map_err(|_| "无法创建 API 配置临时文件")?;
    temp.write_all(next.as_bytes()).and_then(|_|temp.as_file().sync_all()).map_err(|_| "保存 API 配置失败")?;
    if read(path)? != expected { return Err("配置在切换期间被修改".into()); }
    temp.persist(path).map_err(|_| "无法替换 Codex API 配置")?;
    Ok(())
}
