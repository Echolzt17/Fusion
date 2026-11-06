use super::types::*;
use crate::{storage::Store, vault::Vault, Result};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

const INDEX_KEY: &str = "network/v1/state";
const MAX_SNAPSHOT: usize = 16 * 1024 * 1024;

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Index {
    #[serde(default = "version")]
    pub version: u32,
    pub settings: NetworkSettings,
    pub snapshot_file: Option<String>,
    pub url_key: Option<String>,
    pub current_node_id: Option<String>,
    #[serde(default)]
    pub health: BTreeMap<String, NodeHealth>,
    #[serde(default)]
    pub selected_subscription_id: Option<String>,
    #[serde(default)]
    pub subscriptions: Vec<SubscriptionRecord>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SubscriptionRecord {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub node_count: usize,
    pub updated_at: i64,
    pub settings: NetworkSettings,
    pub snapshot_file: Option<String>,
    pub url_key: Option<String>,
    pub current_node_id: Option<String>,
    #[serde(default)]
    pub health: BTreeMap<String, NodeHealth>,
}
impl SubscriptionRecord {
    pub fn new(id: String, snapshot: &Snapshot) -> Self {
        let allowed_regions = snapshot
            .nodes
            .iter()
            .map(|node| node.region.clone())
            .filter(|region| region != "UNKNOWN")
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        Self {
            id,
            name: snapshot.label.clone(),
            kind: snapshot.kind.clone(),
            node_count: snapshot.nodes.len(),
            updated_at: snapshot.updated_at,
            settings: NetworkSettings {
                allowed_regions,
                ..NetworkSettings::default()
            },
            snapshot_file: None,
            url_key: None,
            current_node_id: None,
            health: BTreeMap::new(),
        }
    }
}
impl Index {
    // The flattened fields are the selected subscription's working state. Keep
    // its saved entry current before persisting or selecting a different entry.
    pub fn sync_selected(&mut self) {
        if let Some(entry) = self
            .subscriptions
            .iter_mut()
            .find(|entry| Some(&entry.id) == self.selected_subscription_id.as_ref())
        {
            entry.settings = self.settings.clone();
            entry.snapshot_file = self.snapshot_file.clone();
            entry.url_key = self.url_key.clone();
            entry.current_node_id = self.current_node_id.clone();
            entry.health = self.health.clone();
        }
    }
    pub fn select(&mut self, id: &str) -> Result<()> {
        self.sync_selected();
        let entry = self
            .subscriptions
            .iter()
            .find(|entry| entry.id == id)
            .ok_or("代理订阅不存在")?;
        self.settings = entry.settings.clone();
        self.snapshot_file = entry.snapshot_file.clone();
        self.url_key = entry.url_key.clone();
        self.current_node_id = entry.current_node_id.clone();
        self.health = entry.health.clone();
        self.selected_subscription_id = Some(id.to_owned());
        Ok(())
    }
    pub fn clear_selected(&mut self) {
        self.settings = NetworkSettings::default();
        self.snapshot_file = None;
        self.url_key = None;
        self.current_node_id = None;
        self.health.clear();
        self.selected_subscription_id = None;
    }
}
fn version() -> u32 {
    1
}

pub(crate) struct Persistence {
    pub store: Arc<Store>,
    vault: Arc<dyn Vault>,
    directory: PathBuf,
}
impl Persistence {
    pub fn new(store: Arc<Store>, vault: Arc<dyn Vault>, directory: PathBuf) -> Self {
        Self {
            store,
            vault,
            directory,
        }
    }
    pub fn load(&self) -> Result<(Index, Option<Snapshot>)> {
        let mut index = match self.store.get(INDEX_KEY)? {
            Some(raw) => serde_json::from_str::<Index>(&raw).map_err(|_| "专用网络设置损坏")?,
            None => Index {
                version: 1,
                ..Index::default()
            },
        };
        if index.version > 2 {
            return Err("专用网络设置来自较新版本，请更新 Fusion".into());
        }
        if index
            .url_key
            .as_deref()
            .is_some_and(|key| !valid_url_key(key))
        {
            return Err("订阅凭据索引无效".into());
        }
        let snapshot = self.load_snapshot(index.snapshot_file.as_deref())?;
        if index.version == 1 {
            if let Some(snapshot) = &snapshot {
                let id = index
                    .snapshot_file
                    .as_deref()
                    .and_then(|file| file.strip_suffix(".bin"))
                    .ok_or("订阅文件索引无效")?
                    .to_owned();
                index.subscriptions = vec![SubscriptionRecord::new(id.clone(), snapshot)];
                index.selected_subscription_id = Some(id);
                index.sync_selected();
            }
            index.version = 2;
        } else {
            let mut ids = std::collections::BTreeSet::new();
            for entry in &index.subscriptions {
                if uuid::Uuid::parse_str(&entry.id).is_err()
                    || !ids.insert(&entry.id)
                    || entry
                        .url_key
                        .as_deref()
                        .is_some_and(|key| !valid_url_key(key))
                {
                    return Err("订阅目录索引无效".into());
                }
                // Check every generation and its forward version, including
                // inactive subscriptions, before accepting any state.
                if self
                    .load_snapshot(entry.snapshot_file.as_deref())?
                    .is_none()
                {
                    return Err("订阅目录缺少已保存的节点".into());
                }
            }
            if let Some(id) = index.selected_subscription_id.clone() {
                let entry = index
                    .subscriptions
                    .iter()
                    .find(|entry| entry.id == id)
                    .ok_or("当前订阅索引无效")?;
                if index.snapshot_file != entry.snapshot_file || index.url_key != entry.url_key {
                    return Err("当前订阅文件索引不一致".into());
                }
            } else if snapshot.is_some() || index.settings.enabled {
                return Err("当前订阅索引无效".into());
            }
        }
        if snapshot.is_none() {
            index.settings.enabled = false;
        }
        Ok((index, snapshot))
    }
    pub fn load_snapshot(&self, file: Option<&str>) -> Result<Option<Snapshot>> {
        Ok(match file {
            Some(file) => {
                let path = self.snapshot_path(file)?;
                let metadata =
                    std::fs::symlink_metadata(&path).map_err(|_| "无法读取已保存的订阅")?;
                if !metadata.is_file()
                    || metadata.file_type().is_symlink()
                    || metadata.len() > (MAX_SNAPSHOT + 4096) as u64
                {
                    return Err("已保存订阅文件无效".into());
                }
                let data = std::fs::read(path).map_err(|_| "读取已保存订阅失败")?;
                let decrypted = crypt(&data, false)?;
                let snapshot: Snapshot =
                    serde_json::from_slice(&decrypted).map_err(|_| "已保存订阅损坏")?;
                if snapshot.version > 1 {
                    return Err("订阅来自较新版本，请更新 Fusion".into());
                }
                if snapshot.nodes.is_empty() || snapshot.nodes.len() > MAX_NODES {
                    return Err("已保存订阅节点数无效".into());
                }
                Some(snapshot)
            }
            None => None,
        })
    }
    fn snapshot_path(&self, name: &str) -> Result<PathBuf> {
        let id = name.strip_suffix(".bin").ok_or("订阅文件索引无效")?;
        if uuid::Uuid::parse_str(id).is_err() {
            return Err("订阅文件索引无效".into());
        }
        Ok(self.directory.join(name))
    }
    pub fn save_index(&self, index: &Index) -> Result<()> {
        let mut index = index.clone();
        index.version = 2;
        index.sync_selected();
        self.store.set(
            INDEX_KEY,
            &serde_json::to_string(&index).map_err(|_| "网络设置编码失败")?,
        )
    }
    pub fn url(&self, index: &Index) -> Result<Option<String>> {
        if index
            .url_key
            .as_deref()
            .is_some_and(|key| !valid_url_key(key))
        {
            return Err("订阅凭据索引无效".into());
        }
        index
            .url_key
            .as_deref()
            .map(|key| self.vault.get(key))
            .transpose()
            .map(Option::flatten)
    }
    // Commit a fresh encrypted generation before retiring the old one. The kv
    // pointer is the only commit point; a failed write leaves the old import usable.
    pub fn replace(
        &self,
        old: &Index,
        new: &mut Index,
        snapshot: &Snapshot,
        url: Option<&str>,
    ) -> Result<()> {
        let selected = new.selected_subscription_id.clone();
        self.write_inactive(old, new, snapshot, url, selected.as_deref())
    }
    pub fn remove(&self, old: &Index, new: &Index) -> Result<()> {
        // Delete the secret first, retaining its index when Windows refuses.
        let old_keys = Self::url_keys(old);
        let new_keys = Self::url_keys(new);
        let removed = old_keys.difference(&new_keys).cloned().collect::<Vec<_>>();
        let mut previous: Vec<(String, String)> = Vec::new();
        for key in &removed {
            if !valid_url_key(key) {
                return Err("订阅凭据索引无效".into());
            }
            let url = self.vault.get(key)?;
            if let Err(error) = self.vault.delete(key) {
                for (old_key, old_url) in &previous {
                    let _ = self.vault.set(old_key, old_url);
                }
                return Err(error);
            }
            if let Some(url) = url {
                previous.push((key.clone(), url));
            }
        }
        if let Err(error) = self.save_index(new) {
            for (key, url) in &previous {
                if self.vault.set(key, url).is_err() {
                    return Err("移除未完成：设置保存和订阅地址恢复均失败，请重新导入订阅".into());
                }
            }
            return Err(error);
        }
        self.retire(old, new);
        Ok(())
    }
    fn retire(&self, old: &Index, new: &Index) {
        for key in Self::url_keys(old).difference(&Self::url_keys(new)) {
            if valid_url_key(key) {
                let _ = self.vault.delete(key);
            }
        }
        for name in Self::files(old).difference(&Self::files(new)) {
            if let Ok(path) = self.snapshot_path(name) {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    fn files(index: &Index) -> std::collections::BTreeSet<String> {
        index
            .snapshot_file
            .iter()
            .cloned()
            .chain(
                index
                    .subscriptions
                    .iter()
                    .filter_map(|entry| entry.snapshot_file.clone()),
            )
            .collect()
    }
    fn url_keys(index: &Index) -> std::collections::BTreeSet<String> {
        index
            .url_key
            .iter()
            .cloned()
            .chain(
                index
                    .subscriptions
                    .iter()
                    .filter_map(|entry| entry.url_key.clone()),
            )
            .collect()
    }

    pub fn append(&self, index: &Index, snapshot: &Snapshot, url: Option<&str>) -> Result<Index> {
        let mut next = index.clone();
        next.sync_selected();
        let selected = next.selected_subscription_id.clone();
        let id = uuid::Uuid::new_v4().to_string();
        next.subscriptions
            .push(SubscriptionRecord::new(id.clone(), snapshot));
        next.select(&id)?;
        // replace stages a complete generation atomically. Save it with the
        // original selection so adding a subscription cannot change the exit.
        let mut projected = next.clone();
        self.write_inactive(index, &mut projected, snapshot, url, selected.as_deref())?;
        Ok(projected)
    }

    pub fn update_inactive(
        &self,
        index: &Index,
        id: &str,
        snapshot: &Snapshot,
        url: Option<&str>,
    ) -> Result<Index> {
        let mut next = index.clone();
        let selected = next.selected_subscription_id.clone();
        next.select(id)?;
        next.settings
            .node_order
            .retain(|id| snapshot.nodes.iter().any(|node| node.id == *id));
        next.settings
            .excluded_node_ids
            .retain(|id| snapshot.nodes.iter().any(|node| node.id == *id));
        next.settings
            .region_overrides
            .retain(|id, _| snapshot.nodes.iter().any(|node| node.id == *id));
        next.health
            .retain(|id, _| snapshot.nodes.iter().any(|node| node.id == *id));
        if let Some(entry) = next.subscriptions.iter_mut().find(|entry| entry.id == id) {
            entry.name = snapshot.label.clone();
            entry.kind = snapshot.kind.clone();
            entry.node_count = snapshot.nodes.len();
            entry.updated_at = snapshot.updated_at;
        }
        self.write_inactive(index, &mut next, snapshot, url, selected.as_deref())?;
        Ok(next)
    }

    fn write_inactive(
        &self,
        old: &Index,
        new: &mut Index,
        snapshot: &Snapshot,
        url: Option<&str>,
        selected: Option<&str>,
    ) -> Result<()> {
        // Stage in a separate index, then restore the active projection before
        // committing. No intermediate selection is written to the database.
        std::fs::create_dir_all(&self.directory).map_err(|_| "无法创建专用网络目录")?;
        let bytes = serde_json::to_vec(snapshot).map_err(|_| "订阅编码失败")?;
        if bytes.len() > MAX_SNAPSHOT {
            return Err("标准化订阅超出大小限制".into());
        }
        let encrypted = crypt(&bytes, true)?;
        let generation = uuid::Uuid::new_v4().to_string();
        let name = format!("{generation}.bin");
        let path = self.snapshot_path(&name)?;
        let mut file =
            tempfile::NamedTempFile::new_in(&self.directory).map_err(|_| "无法准备加密订阅文件")?;
        use std::io::Write;
        file.write_all(&encrypted).map_err(|_| "保存加密订阅失败")?;
        file.as_file().sync_all().map_err(|_| "保存加密订阅失败")?;
        file.persist_noclobber(&path)
            .map_err(|_| "提交加密订阅失败")?;
        let key = url.map(|_| format!("network/subscription/{generation}"));
        if let (Some(key), Some(url)) = (key.as_deref(), url) {
            if let Err(error) = self.vault.set(key, url) {
                let _ = std::fs::remove_file(&path);
                return Err(error);
            }
        }
        new.snapshot_file = Some(name);
        new.url_key = key.clone();
        if let Some(entry) = new
            .subscriptions
            .iter_mut()
            .find(|entry| Some(&entry.id) == new.selected_subscription_id.as_ref())
        {
            entry.name = snapshot.label.clone();
            entry.kind = snapshot.kind.clone();
            entry.node_count = snapshot.nodes.len();
            entry.updated_at = snapshot.updated_at;
        }
        new.sync_selected();
        if let Some(id) = selected {
            new.select(id)?;
        } else {
            new.clear_selected();
        }
        if let Err(error) = self.save_index(new) {
            if let Some(key) = key {
                let _ = self.vault.delete(&key);
            }
            let _ = std::fs::remove_file(&path);
            return Err(error);
        }
        self.retire(old, new);
        Ok(())
    }
}
fn valid_url_key(key: &str) -> bool {
    key.strip_prefix("network/subscription/")
        .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
}

#[cfg(windows)]
fn crypt(bytes: &[u8], protect: bool) -> Result<Vec<u8>> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
        },
    };
    if bytes.len() > MAX_SNAPSHOT + 4096 {
        return Err("加密订阅文件过大".into());
    }
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let ok = unsafe {
        if protect {
            CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
    };
    if ok == 0 {
        return Err("无法加密或读取订阅，请使用原 Windows 用户重试".into());
    }
    let result =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe {
        std::ptr::write_bytes(output.pbData, 0, output.cbData as usize);
        LocalFree(output.pbData.cast());
    }
    Ok(result)
}
#[cfg(not(windows))]
fn crypt(_: &[u8], _: bool) -> Result<Vec<u8>> {
    Err("专用网络加密存储仅支持 Windows".into())
}

pub(crate) fn validate_url(raw: &str) -> Result<String> {
    let url = url::Url::parse(raw.trim()).map_err(|_| "订阅地址格式无效")?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || raw.len() > 16 * 1024
    {
        return Err("订阅地址必须使用 HTTPS，不支持用户名、密码或片段".into());
    }
    Ok(url.into())
}
