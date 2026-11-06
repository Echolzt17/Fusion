use crate::Result;
use std::sync::Arc;
pub mod cleanup;

pub trait Vault: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<String>>;
    fn set(&self, key: &str, value: &str) -> Result<()>;
    fn delete(&self, key: &str) -> Result<()>;
}

pub fn system_vault() -> Result<Arc<dyn Vault>> {
    #[cfg(windows)]
    {
        Ok(Arc::new(WindowsVault))
    }
    #[cfg(not(windows))]
    {
        Err("此版本只支持 Windows 凭据管理器".into())
    }
}

#[cfg(windows)]
struct WindowsVault;
#[cfg(windows)]
impl WindowsVault {
    fn entry(key: &str) -> Result<keyring::Entry> {
        keyring::Entry::new("PersonalCodexPool", key)
            .map_err(|_| "无法打开 Windows 凭据管理器".into())
    }
    fn raw(key: &str) -> Result<Option<String>> {
        match Self::entry(key)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err("读取 Windows 凭据失败，请检查当前 Windows 用户权限".into()),
        }
    }
    fn remove_parts(key: &str, head: &str) {
        if let Some((generation, count)) = head.split_once(':') {
            if let Ok(n) = count.parse::<usize>() {
                for i in 0..n.min(128) {
                    if let Ok(e) = Self::entry(&format!("{key}/{generation}/{i}")) {
                        let _ = e.delete_credential();
                    }
                }
            }
        }
    }
}
#[cfg(windows)]
impl Vault for WindowsVault {
    fn get(&self, key: &str) -> Result<Option<String>> {
        use base64::Engine;
        let Some(head) = Self::raw(key)? else {
            return Ok(None);
        };
        let (generation, count) = head.split_once(':').ok_or("凭据索引损坏，请重新授权")?;
        let n = count.parse::<usize>().map_err(|_| "凭据索引损坏")?;
        if n > 128 {
            return Err("凭据索引超出限制".into());
        }
        let mut encoded = String::new();
        for i in 0..n {
            encoded.push_str(
                &Self::raw(&format!("{key}/{generation}/{i}"))?.ok_or("凭据不完整，请重新授权")?,
            );
        }
        let data = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| "凭据损坏")?;
        Ok(Some(String::from_utf8(data).map_err(|_| "凭据编码错误")?))
    }
    fn set(&self, key: &str, value: &str) -> Result<()> {
        use base64::Engine;
        let old = Self::raw(key)?;
        let encoded = base64::engine::general_purpose::STANDARD.encode(value);
        // Windows generic credential blobs are limited: chunks contain ASCII, <= 1800 UTF-16 bytes.
        let parts: Vec<_> = encoded.as_bytes().chunks(900).collect();
        if parts.len() > 128 {
            return Err("凭据过大".into());
        }
        let generation = uuid::Uuid::new_v4().to_string();
        let head = format!("{generation}:{}", parts.len());
        for (i, part) in parts.iter().enumerate() {
            if Self::entry(&format!("{key}/{generation}/{i}"))?
                .set_password(std::str::from_utf8(part).unwrap())
                .is_err()
            {
                Self::remove_parts(key, &head);
                return Err("保存 Windows 凭据失败".into());
            }
        }
        if Self::entry(key)?.set_password(&head).is_err() {
            Self::remove_parts(key, &head);
            return Err("保存凭据索引失败".into());
        }
        if let Some(v) = old {
            Self::remove_parts(key, &v);
        }
        Ok(())
    }
    fn delete(&self, key: &str) -> Result<()> {
        if let Some(head) = Self::raw(key)? {
            let (generation, count) = head.split_once(':').ok_or("凭据索引损坏，清理未完成")?;
            let count = count.parse::<usize>().ok().filter(|n| *n <= 128).ok_or("凭据索引损坏，清理未完成")?;
            // Retain the manifest until every part is gone, so failures and
            // process interruption can resume without leaving orphaned secrets.
            for i in 0..count {
                match Self::entry(&format!("{key}/{generation}/{i}"))?.delete_credential() {
                    Ok(()) | Err(keyring::Error::NoEntry) => (),
                    Err(_) => return Err("删除凭据分段失败，请重试移除".into()),
                }
            }
            match Self::entry(key)?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => (),
                Err(_) => return Err("删除凭据失败".into()),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[derive(Default)]
pub struct MemoryVault(pub std::sync::Mutex<std::collections::HashMap<String, String>>);
#[cfg(test)]
impl Vault for MemoryVault {
    fn get(&self, key: &str) -> Result<Option<String>> {
        Ok(self.0.lock().unwrap().get(key).cloned())
    }
    fn set(&self, key: &str, value: &str) -> Result<()> {
        self.0.lock().unwrap().insert(key.into(), value.into());
        Ok(())
    }
    fn delete(&self, key: &str) -> Result<()> {
        self.0.lock().unwrap().remove(key);
        Ok(())
    }
}
