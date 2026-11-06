use super::*;
use crate::local_usage::{Cursor, TurnRecord, UsageRecord};
use rusqlite::OptionalExtension;

impl Store {
    // File contents are parsed outside the database lock. The cursor and numeric
    // evidence commit together, so interruption cannot skip or count a record twice.
    pub(crate) fn save_local_usage(
        &self,
        key: &str,
        cursor: &Cursor,
        events: &[(String, UsageRecord)],
        turns: &[(String, TurnRecord)],
    ) -> Result<()> {
        let mut db = self.db.lock().map_err(|_| "数据库锁异常")?;
        let tx = db.transaction().map_err(|_| "本机用量事务失败")?;
        for (id, event) in events {
            let value = serde_json::to_string(event).map_err(|_| "本机用量编码失败")?;
            tx.execute(
                "INSERT OR IGNORE INTO kv VALUES(?1,?2)",
                params![format!("local-usage/v1/event/{id}"), value],
            )
            .map_err(|_| "本机用量保存失败")?;
        }
        for (id, turn) in turns {
            let key = format!("local-usage/v1/turn/{id}");
            let old: Option<String> = tx
                .query_row("SELECT value FROM kv WHERE key=?1", [&key], |r| r.get(0))
                .optional()
                .map_err(|_| "读取任务时长失败")?;
            let mut merged = turn.clone();
            if let Some(old) = old {
                let old: TurnRecord = serde_json::from_str(&old).map_err(|_| "任务时长数据损坏")?;
                // Re-reading an archived copy must not reopen a completed turn.
                if old.complete && !merged.complete {
                    merged = old;
                } else if old.complete == merged.complete {
                    merged.start = merged.start.min(old.start);
                    merged.end = merged.end.max(old.end);
                }
            }
            tx.execute(
                "INSERT INTO kv VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![
                    key,
                    serde_json::to_string(&merged).map_err(|_| "任务时长编码失败")?
                ],
            )
            .map_err(|_| "任务时长保存失败")?;
        }
        tx.execute(
            "INSERT INTO kv VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![
                key,
                serde_json::to_string(cursor).map_err(|_| "读取进度编码失败")?
            ],
        )
        .map_err(|_| "读取进度保存失败")?;
        tx.commit().map_err(|_| "本机用量提交失败".into())
    }

    pub(crate) fn local_usage_values<T: serde::de::DeserializeOwned>(
        &self,
        prefix: &str,
    ) -> Result<Vec<T>> {
        let raw = {
            let db = self.db.lock().map_err(|_| "数据库锁异常")?;
            let mut query = db
                .prepare("SELECT value FROM kv WHERE key>=?1 AND key<?2")
                .map_err(|_| "读取本机统计失败")?;
            let rows = query
                .query_map(params![prefix, format!("{prefix}\u{10ffff}")], |r| {
                    r.get::<_, String>(0)
                })
                .map_err(|_| "读取本机统计失败")?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|_| "读取本机统计失败")?
        };
        raw.into_iter()
            .map(|s| serde_json::from_str(&s).map_err(|_| "本机统计数据损坏".into()))
            .collect()
    }
}
