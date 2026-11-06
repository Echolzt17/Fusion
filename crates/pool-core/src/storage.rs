use crate::{Account, RequestLog, Result, Settings};
use rusqlite::{params, Connection};
use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
};
pub mod history;
pub mod summary;
mod reliability;
mod local_usage;
use history::RecentRequest;

pub struct Store {
    db: Mutex<Connection>,
    log_failed: AtomicBool,
    recent_request: Mutex<Option<RecentRequest>>,
}
impl Store {
    pub fn finish_client_switch(&self, retire_backup: bool) -> Result<()> {
        let mut db = self.db.lock().map_err(|_| "数据库锁异常")?;
        let tx = db.transaction().map_err(|_| "接入状态保存失败")?;
        tx.execute(
            "UPDATE kv SET value='' WHERE key='codex_client_reload_required'",
            [],
        )
        .map_err(|_| "客户端切换状态保存失败")?;
        tx.execute("UPDATE kv SET value='' WHERE key='codex_restart_context'", [])
            .map_err(|_| "客户端重开状态清理失败")?;
        if retire_backup {
            tx.execute("UPDATE kv SET value='' WHERE key='codex_config_backup'", [])
                .map_err(|_| "恢复记录清理失败")?;
        }
        tx.commit().map_err(|_| "接入状态保存失败".into())
    }
    pub fn retire_recovery_if_readable(path: &Path) -> Result<()> {
        let mut db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(|_| "恢复记录暂未清理")?;
        let tx = db.transaction().map_err(|_| "恢复记录暂未清理")?;
        let version: i64 = tx
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(|_| "读取数据版本失败")?;
        if version != 1 {
            return Err("数据版本未经验证，未修改恢复记录".into());
        }
        tx.execute("UPDATE kv SET value='' WHERE key='codex_config_backup'", [])
            .map_err(|_| "恢复记录暂未清理")?;
        tx.execute("INSERT INTO kv VALUES('codex_client_reload_required','original') ON CONFLICT(key) DO UPDATE SET value=excluded.value",[]).map_err(|_| "恢复记录暂未清理")?;
        tx.commit().map_err(|_| "恢复记录暂未清理".into())
    }
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| "无法创建数据目录")?;
        }
        let db = Connection::open(path).map_err(|_| "无法打开账号池数据库")?;
        db.busy_timeout(std::time::Duration::from_secs(3))
            .map_err(|_| "数据库初始化失败")?;
        let version: i64 = db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(|_| "读取数据版本失败")?;
        if version > 1 {
            return Err("数据来自较新版本，请使用新版程序".into());
        }
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS accounts(id TEXT PRIMARY KEY, body TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS kv(key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS bindings(key TEXT PRIMARY KEY, account TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE, expires INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS logs(id TEXT PRIMARY KEY, at INTEGER NOT NULL, body TEXT NOT NULL);
            PRAGMA user_version=1;").map_err(|_| "初始化数据库失败")?;
        let store = Self {
            db: Mutex::new(db),
            log_failed: AtomicBool::new(false),
            recent_request: Mutex::new(None),
        };
        store.initialize_history()?;
        store.initialize_summaries()?;
        Ok(store)
    }
    pub fn accounts(&self) -> Result<Vec<Account>> {
        let db = self.db.lock().map_err(|_| "数据库锁异常")?;
        let mut st = db
            .prepare("SELECT body FROM accounts ORDER BY id")
            .map_err(|_| "读取账号失败")?;
        let rows = st
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|_| "读取账号失败")?;
        rows.map(|r| {
            serde_json::from_str(&r.map_err(|_| "读取账号失败")?).map_err(|_| "账号数据损坏".into())
        })
        .collect()
    }
    pub fn account(&self, id: &str) -> Result<Account> {
        use rusqlite::OptionalExtension;
        let raw: Option<String> = self
            .db
            .lock()
            .map_err(|_| "数据库锁异常")?
            .query_row("SELECT body FROM accounts WHERE id=?1", [id], |r| r.get(0))
            .optional()
            .map_err(|_| "读取账号失败")?;
        serde_json::from_str(&raw.ok_or("账号不存在")?).map_err(|_| "账号数据损坏".into())
    }

    pub fn save_account(&self, a: &Account) -> Result<()> {
        self.db.lock().map_err(|_| "数据库锁异常")?.execute("INSERT INTO accounts VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET body=excluded.body", params![a.id, serde_json::to_string(a).map_err(|_| "账号序列化失败")?]).map_err(|_| "保存账号失败")?;
        Ok(())
    }
    pub fn delete_account(&self, id: &str) -> Result<()> {
        let mut db = self.db.lock().map_err(|_| "数据库锁异常")?;
        let tx = db.transaction().map_err(|_| "删除账号事务失败")?;
        // Keep only hashed retired identities. Re-adding an account must not revive old sessions.
        tx.execute("INSERT OR IGNORE INTO kv SELECT 'retired-binding/' || key, '1' FROM bindings WHERE account=?1", [id])
            .map_err(|_| "保留会话失效标记失败")?;
        tx.execute("DELETE FROM accounts WHERE id=?1", [id])
            .map_err(|_| "删除账号失败")?;
        tx.commit().map_err(|_| "删除账号事务失败".into())
    }
    pub fn get(&self, key: &str) -> Result<Option<String>> {
        use rusqlite::OptionalExtension;
        self.db
            .lock()
            .map_err(|_| "数据库锁异常")?
            .query_row("SELECT value FROM kv WHERE key=?1", [key], |r| r.get(0))
            .optional()
            .map_err(|_| "读取配置失败".into())
    }
    pub fn set(&self, key: &str, value: &str) -> Result<()> {
        self.db
            .lock()
            .map_err(|_| "数据库锁异常")?
            .execute(
                "INSERT INTO kv VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![key, value],
            )
            .map_err(|_| "保存配置失败")?;
        Ok(())
    }
    pub fn settings(&self) -> Result<Settings> {
        self.get("settings")?
            .map(|s| {
                let mut saved: serde_json::Value = serde_json::from_str(&s).map_err(|_| "设置损坏")?;
                // Upgrade only the old shipped default on read, without a DB
                // migration/write. Explicit choices saved by this version stay.
                if saved["concurrencyConfigured"] != true && saved["concurrency"] == 2 {
                    saved["concurrency"] = serde_json::json!(4);
                }
                serde_json::from_value(saved).map_err(|_| "设置损坏".into())
            })
            .unwrap_or(Ok(Settings::default()))
    }
    fn settings_json(s: &Settings) -> Result<String> {
        let mut saved = serde_json::to_value(s).map_err(|_| "设置编码失败")?;
        saved["concurrencyConfigured"] = serde_json::json!(true);
        serde_json::to_string(&saved).map_err(|_| "设置编码失败".into())
    }
    pub fn save_settings(&self, s: &Settings) -> Result<()> {
        if s.port == 0 || s.concurrency == 0 || s.concurrency > 16 {
            return Err("端口应为 1–65535，并发应为 1–16".into());
        }
        self.set("settings", &Self::settings_json(s)?)
    }
    pub fn save_scheduling(&self, settings: &Settings, current: Option<&str>) -> Result<()> {
        let mut db = self.db.lock().map_err(|_| "数据库锁异常")?;
        let tx = db.transaction().map_err(|_| "保存调度设置失败")?;
        let body = Self::settings_json(settings)?;
        for (key, value) in [
            ("settings", body.as_str()),
            ("scheduling_current_account", current.unwrap_or("")),
        ] {
            tx.execute(
                "INSERT INTO kv VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![key, value],
            )
            .map_err(|_| "保存调度设置失败")?;
        }
        tx.commit().map_err(|_| "保存调度设置失败".into())
    }
    pub fn bind(&self, key: &str, account: &str) -> Result<()> {
        self.bind_many(&[key.to_owned()], account)
    }
    pub fn bind_many(&self, keys: &[String], account: &str) -> Result<()> {
        if keys.is_empty() {
            return Ok(());
        }
        let mut db = self.db.lock().map_err(|_| "数据库锁异常")?;
        let tx = db.transaction().map_err(|_| "会话事务失败")?;
        let db = &tx;
        use rusqlite::OptionalExtension;
        for key in keys {
            let retired: bool = db
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM kv WHERE key=?1)",
                    [format!("retired-binding/{key}")],
                    |r| r.get(0),
                )
                .map_err(|_| "读取会话状态失败")?;
            if retired {
                return Err("原会话账号已删除，请新建会话".into());
            }
            let expired: bool = db
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM bindings WHERE key=?1 AND expires<?2)",
                    params![key, crate::now()],
                    |r| r.get(0),
                )
                .map_err(|_| "读取会话状态失败")?;
            if expired {
                return Err("会话绑定已过期，请新建会话".into());
            }
            let existing: Option<String> = db
                .query_row("SELECT account FROM bindings WHERE key=?1", [key], |r| {
                    r.get(0)
                })
                .optional()
                .map_err(|_| "读取会话绑定失败")?;
            if existing.as_deref().is_some_and(|id| id != account) {
                return Err("该会话已经绑定其他账号，已阻止跨账号发送".into());
            }
            db.execute("INSERT INTO bindings VALUES(?1,?2,?3) ON CONFLICT(key) DO UPDATE SET expires=excluded.expires", params![key,account,crate::now()+30*86400]).map_err(|_| "保存会话绑定失败")?;
        }
        tx.commit().map_err(|_| "提交会话绑定失败".into())
    }
    pub fn binding(&self, key: &str) -> Result<Option<String>> {
        use rusqlite::OptionalExtension;
        let db = self.db.lock().map_err(|_| "数据库锁异常")?;
        let retired: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM kv WHERE key=?1)",
                [format!("retired-binding/{key}")],
                |r| r.get(0),
            )
            .map_err(|_| "读取会话状态失败")?;
        if retired {
            return Err("原会话账号已删除，请新建会话".into());
        }
        let binding: Option<(String, i64)> = db
            .query_row(
                "SELECT account,expires FROM bindings WHERE key=?1",
                [key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|_| "读取会话绑定失败")?;
        match binding {
            Some((_, expires)) if expires < crate::now() => {
                Err("会话绑定已过期，请新建会话".into())
            }
            other => Ok(other.map(|(account, _)| account)),
        }
    }
    pub fn persistence_warning(&self) -> Option<&'static str> {
        self.log_failed.load(Ordering::Relaxed).then_some(
            "本次运行有请求统计未完整保存，请检查磁盘空间或数据目录权限；请求不会自动重放",
        )
    }
    pub fn log(&self, log: &RequestLog) -> Result<()> {
        let result = self.write_log(log);
        if result.is_err() {
            self.log_failed.store(true, Ordering::Relaxed);
        }
        result
    }
    #[cfg(test)]
    pub(crate) fn reject_writes_for_test(&self) {
        self.db
            .lock()
            .unwrap()
            .execute_batch("PRAGMA query_only=ON")
            .unwrap();
    }
    fn write_log(&self, log: &RequestLog) -> Result<()> {
        let mut db = self.db.lock().map_err(|_| "数据库锁异常")?;
        let tx = db.transaction().map_err(|_| "用量事务失败")?;
        // Persist a known coverage gap once writes recover, without changing schema.
        if self.log_failed.load(Ordering::Relaxed) {
            tx.execute("INSERT OR REPLACE INTO kv VALUES('reliability/v1/gap',?1)", [crate::now().to_string()])
                .map_err(|_| "保存可靠性缺失标记失败")?;
        }
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM logs WHERE id=?1)",
                [&log.id],
                |r| r.get(0),
            )
            .map_err(|_| "读取日志失败")?;
        // Aggregate once, in the same transaction as the log. Pruning logs must not erase totals.
        let counted = if history::is_request(log) {
            tx.execute("INSERT OR IGNORE INTO kv VALUES(?1,'1')", [format!("usage-seen/v1/{}", log.id)])
                .map_err(|_| "保存统计去重标记失败")? > 0
        } else { false };
        if counted {
            reliability::record(&tx, log)?;
            history::record_conversation(&tx, log, false)?;
            summary::record(&tx, log)?;
        }
        if !exists && counted
            && matches!(
                log.operation.as_str(),
                "responses" | "compact" | "image_generate" | "image_edit"
            )
        {
            if let Some(account) = log.account.as_deref() {
                use rusqlite::OptionalExtension;
                let key = format!("usage/v1/{account}");
                let raw: Option<String> = tx
                    .query_row("SELECT value FROM kv WHERE key=?1", [&key], |r| r.get(0))
                    .optional()
                    .map_err(|_| "读取用量失败")?;
                let mut usage: crate::usage::AccountUsage = raw
                    .map(|s| serde_json::from_str(&s))
                    .transpose()
                    .map_err(|_| "用量数据损坏")?
                    .unwrap_or_default();
                usage.account = account.into();
                usage.record(log.at, log.usage.as_ref());
                tx.execute("INSERT INTO kv VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key, serde_json::to_string(&usage).map_err(|_| "用量编码失败")?]).map_err(|_| "保存用量失败")?;
            }
        }
        tx.execute(
            "INSERT OR REPLACE INTO logs VALUES(?1,?2,?3)",
            params![log.id, log.at, serde_json::to_string(log).unwrap()],
        )
        .map_err(|_| "记录日志失败")?;
        tx.execute("DELETE FROM logs WHERE id NOT IN (SELECT id FROM logs ORDER BY at DESC, rowid DESC LIMIT 2000)", []).map_err(|_| "清理日志失败")?;
        tx.commit().map_err(|_| "提交用量失败")?;
        Ok(())
    }
    pub fn usage(&self) -> Result<Vec<crate::usage::AccountUsage>> {
        // Return totals only for accounts that still exist. No credentials or request bodies.
        self.accounts()?
            .into_iter()
            .map(|a| {
                self.get(&format!("usage/v1/{}", a.id))?
                    .map(|s| serde_json::from_str(&s).map_err(|_| "用量数据损坏".into()))
                    .unwrap_or_else(|| {
                        Ok(crate::usage::AccountUsage {
                            account: a.id,
                            ..Default::default()
                        })
                    })
            })
            .collect()
    }
    pub fn logs(&self) -> Result<Vec<RequestLog>> {
        let db = self.db.lock().map_err(|_| "数据库锁异常")?;
        let mut st = db
            .prepare("SELECT body FROM logs ORDER BY at DESC,rowid DESC LIMIT 200")
            .map_err(|_| "读取日志失败")?;
        let rows = st
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|_| "读取日志失败")?;
        rows.map(|r| {
            serde_json::from_str(&r.map_err(|_| "读取日志失败")?).map_err(|_| "日志损坏".into())
        })
        .collect()
    }
}


impl Store {
    /// Filter the full retained history, not the UI's most recent 200 entries.
    pub fn error_report_records(&self, at: i64) -> Result<(Vec<RequestLog>, bool)> {
        let db = self.db.lock().map_err(|_| "数据库锁异常")?;
        let total: usize = db.query_row("SELECT count(*) FROM logs", [], |r| r.get(0)).map_err(|_| "读取错误记录失败")?;
        let mut st = db.prepare("SELECT body FROM logs WHERE at>=?1 AND at<=?2 AND json_extract(body,'$.status') NOT IN ('success','cancelled','service_stopped') ORDER BY at DESC,rowid DESC LIMIT 2000").map_err(|_| "读取错误记录失败")?;
        let rows = st.query_map(rusqlite::params![at - 7 * 86400, at], |r| r.get::<_, String>(0)).map_err(|_| "读取错误记录失败")?;
        let mut out = Vec::new();
        for row in rows {
            let log: RequestLog = serde_json::from_str(&row.map_err(|_| "读取错误记录失败")?).map_err(|_| "错误记录损坏")?;
            if crate::error_reports::is_failure(&log) { out.push(log); }
        }
        Ok((out, total >= 2000))
    }
}

#[cfg(test)]
mod preflight_tests {
    use super::*;
    #[test]
    fn recovery_retirement_preserves_forward_schema_and_does_not_create_a_missing_database() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("pool.sqlite");

        assert!(Store::retire_recovery_if_readable(&path).is_err());
        assert!(!path.exists());
        let store = Store::open(&path).unwrap();

        store
            .set("codex_config_backup", "public-fixture-backup")
            .unwrap();

        assert_eq!(
            store.get("codex_config_backup").unwrap().as_deref(),
            Some("public-fixture-backup")
        );
        Store::retire_recovery_if_readable(&path).unwrap();
        assert_eq!(
            store.get("codex_config_backup").unwrap().as_deref(),
            Some("")
        );

        store
            .db
            .lock()
            .unwrap()
            .execute_batch("PRAGMA user_version=2")
            .unwrap();

        assert!(Store::retire_recovery_if_readable(&path).is_err());
        assert_eq!(
            store
                .db
                .lock()
                .unwrap()
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
    }
}
