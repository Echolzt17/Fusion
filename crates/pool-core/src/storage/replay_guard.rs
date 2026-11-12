use super::Store;
use crate::Result;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const VERSION_KEY: &str = "native-replay/version";
const ENTRY_PREFIX: &str = "native-replay/v1/";
const MAX_ENTRIES: usize = 20_000;
const RETENTION_SECS: i64 = 7 * 86400;

pub struct NativeReplayClaim {
    key: String,
    owner: String,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    version: u32,
    claimed_at: i64,
    owner_hash: String,
}

impl Store {
    /// Persist the dispatch intent before any upstream send. A completed or
    /// uncertain attempt stays reserved; only a request not yet dispatched or
    /// a confirmed non-executed quota rejection may release its exact claim.
    pub fn claim_native_replay(&self, digest: &str) -> Result<NativeReplayClaim> {
        self.claim_native_replay_at(digest, crate::now(), MAX_ENTRIES)
    }

    fn claim_native_replay_at(&self, digest: &str, at: i64, capacity: usize) -> Result<NativeReplayClaim> {
        if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err("请求防重放标识无效，未发送请求".into());
        }
        let mut db = self.db.lock().map_err(|_| "数据库锁异常")?;
        let tx = db.transaction().map_err(|_| "保存请求防重放状态失败")?;
        let version: Option<String> = tx.query_row(
            "SELECT value FROM kv WHERE key=?1", [VERSION_KEY], |row| row.get(0),
        ).optional().map_err(|_| "读取请求防重放版本失败")?;
        match version.as_deref() {
            Some("1") => {}
            Some(_) => return Err("请求防重放状态来自未支持的版本，未发送请求".into()),
            None => {
                let exists: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM kv WHERE key LIKE 'native-replay/%')", [], |row| row.get(0),
                ).map_err(|_| "读取请求防重放状态失败")?;
                if exists { return Err("请求防重放版本记录缺失，未发送请求".into()); }
                tx.execute("INSERT INTO kv VALUES(?1,'1')", [VERSION_KEY])
                    .map_err(|_| "保存请求防重放版本失败")?;
            }
        }
        // Never evict a fresh unknown attempt to make room. Malformed or future
        // entries are retained and count against the bound, failing closed.
        tx.execute(
            "DELETE FROM kv WHERE key LIKE 'native-replay/v1/%' AND CASE WHEN json_valid(value) THEN json_extract(value,'$.version')=1 AND json_extract(value,'$.claimedAt')<=?1 ELSE 0 END",
            [at.saturating_sub(RETENTION_SECS)],
        ).map_err(|_| "清理到期请求防重放状态失败")?;
        let key = format!("{ENTRY_PREFIX}{digest}");
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM kv WHERE key=?1)", [&key], |row| row.get(0),
        ).map_err(|_| "读取请求防重放状态失败")?;
        if exists {
            return Err("此请求已经发送或执行状态未确认，已阻止客户端自动重放；请先核对原请求结果".into());
        }
        let count: usize = tx.query_row(
            "SELECT COUNT(*) FROM kv WHERE key LIKE 'native-replay/v1/%'", [], |row| row.get(0),
        ).map_err(|_| "读取请求防重放容量失败")?;
        if count >= capacity {
            return Err("请求防重放记录已满，未发送请求；为保留未确认执行记录，不能自动清除近期记录".into());
        }
        let owner = format!("{:x}", Sha256::digest(uuid::Uuid::new_v4().as_bytes()));
        let entry = Entry { version: 1, claimed_at: at, owner_hash: owner.clone() };
        tx.execute("INSERT INTO kv VALUES(?1,?2)", params![key, serde_json::to_string(&entry).map_err(|_| "编码请求防重放状态失败")?])
            .map_err(|_| "保存请求防重放状态失败")?;
        tx.commit().map_err(|_| "保存请求防重放状态失败")?;
        Ok(NativeReplayClaim { key, owner })
    }

    pub fn release_native_replay(&self, claim: &NativeReplayClaim) -> Result<()> {
        let mut db = self.db.lock().map_err(|_| "数据库锁异常")?;
        let tx = db.transaction().map_err(|_| "释放请求防重放状态失败")?;
        let version: String = tx.query_row(
            "SELECT value FROM kv WHERE key=?1", [VERSION_KEY], |row| row.get(0),
        ).map_err(|_| "读取请求防重放版本失败")?;
        if version != "1" { return Err("请求防重放状态版本未经验证，未释放记录".into()); }
        let value: Option<String> = tx.query_row(
            "SELECT value FROM kv WHERE key=?1", [&claim.key], |row| row.get(0),
        ).optional().map_err(|_| "读取请求防重放状态失败")?;
        if let Some(value) = value {
            let entry: Entry = serde_json::from_str(&value).map_err(|_| "请求防重放状态损坏，未释放记录")?;
            if entry.version != 1 || entry.owner_hash != claim.owner {
                return Err("请求防重放记录已变化，未释放其他请求的记录".into());
            }
            tx.execute("DELETE FROM kv WHERE key=?1", [&claim.key]).map_err(|_| "释放请求防重放状态失败")?;
        }
        tx.commit().map_err(|_| "释放请求防重放状态失败".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::fixture;

    fn digest(value: &str) -> String { format!("{:x}", Sha256::digest(value.as_bytes())) }

    #[test]
    fn native_replay_survives_restart_and_only_owner_can_release() {
        let (dir, accounts) = fixture();
        let fingerprint = digest("public business fingerprint");
        let first = accounts.store.claim_native_replay(&fingerprint).unwrap();
        let reopened = Store::open(&dir.path().join("pool.sqlite")).unwrap();
        assert!(reopened.claim_native_replay(&fingerprint).is_err());
        reopened.release_native_replay(&first).unwrap();
        let second = reopened.claim_native_replay(&fingerprint).unwrap();
        assert!(reopened.release_native_replay(&first).is_err());
        assert!(reopened.claim_native_replay(&fingerprint).is_err());
        reopened.release_native_replay(&second).unwrap();
        assert!(reopened.claim_native_replay(&fingerprint).is_ok());
    }

    #[test]
    fn native_replay_concurrent_claims_send_at_most_once() {
        let (_dir, accounts) = fixture();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let handles: Vec<_> = (0..2).map(|_| {
            let store = accounts.store.clone(); let barrier = barrier.clone();
            std::thread::spawn(move || { barrier.wait(); store.claim_native_replay(&digest("same")).is_ok() })
        }).collect();
        barrier.wait();
        assert_eq!(handles.into_iter().map(|handle| handle.join().unwrap()).filter(|claimed| *claimed).count(), 1);
    }

    #[test]
    fn native_replay_capacity_never_evicts_fresh_uncertain_attempts() {
        let (_dir, accounts) = fixture();
        let at = crate::now();
        let original = digest("original");
        accounts.store.claim_native_replay_at(&original, at, 1).unwrap();
        assert!(accounts.store.claim_native_replay_at(&digest("next"), at + RETENTION_SECS - 1, 1).is_err());
        assert!(accounts.store.claim_native_replay_at(&original, at + RETENTION_SECS - 1, 1).is_err());
        assert!(accounts.store.claim_native_replay_at(&digest("next"), at + RETENTION_SECS, 1).is_ok());
    }

    #[test]
    fn native_replay_future_version_blocks_without_modifying_state() {
        let (_dir, accounts) = fixture();
        accounts.store.set(VERSION_KEY, "2").unwrap();
        assert!(accounts.store.claim_native_replay(&digest("request")).is_err());
        assert_eq!(accounts.store.get(VERSION_KEY).unwrap().as_deref(), Some("2"));
        assert!(accounts.store.get(&format!("{ENTRY_PREFIX}{}", digest("request"))).unwrap().is_none());
    }
}
