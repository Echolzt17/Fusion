use super::*;
use fusion_report_protocol::{ReliabilityHour, ReliabilitySummary, ORIGINS};
use rusqlite::OptionalExtension;
const PREFIX: &str = "reliability/v1/hour/";
const START: &str = "reliability/v1/start";

pub(super) fn record(tx: &rusqlite::Transaction<'_>, log: &RequestLog) -> Result<()> {
    if !["responses", "compact", "image_generate", "image_edit"].contains(&log.operation.as_str()) {
        return Ok(());
    }
    let Some(d) = &log.diagnostic else {
        return Ok(());
    };
    let Some(version) = fusion_report_protocol::version(&d.app_version) else {
        return Ok(());
    };
    let at = crate::now();
    if log.at < at - 7 * 86400 || log.at > at {
        return Ok(());
    }
    tx.execute(
        "INSERT OR IGNORE INTO kv VALUES(?1,?2)",
        params![START, at.to_string()],
    )
    .map_err(|_| "保存可靠性覆盖时间失败")?;
    let hour = log.at.div_euclid(3600) * 3600;
    let key = format!("{PREFIX}{hour}/{version}/{}", log.operation);
    let raw: Option<String> = tx
        .query_row("SELECT value FROM kv WHERE key=?1", [&key], |r| r.get(0))
        .optional()
        .map_err(|_| "读取可靠性统计失败")?;
    let mut b: ReliabilityHour = match raw {
        Some(raw) => serde_json::from_str(&raw).map_err(|_| "可靠性统计损坏")?,
        None => ReliabilityHour {
            hour,
            app_version: version,
            operation: log.operation.clone(),
            ..Default::default()
        },
    };
    let t = d.execution.as_ref().and_then(|x| x.transport.as_ref());
    let size = match t.and_then(|t| t.request_bytes) {
        None => 0,
        Some(n) if n < 65536 => 1,
        Some(n) if n < 1048576 => 2,
        Some(n) if n < 8388608 => 3,
        _ => 4,
    };
    match log.status.as_str() {
        "success" => {
            b.success += 1;
            b.success_by_size[size] += 1;
        }
        "cancelled" => b.cancelled += 1,
        "service_stopped" => b.stopped += 1,
        _ => {
            b.failure += 1;
            b.failure_by_size[size] += 1;
            let origin = crate::diagnostics::failure_origin(&log.status, d);
            b.failures_by_origin[ORIGINS.iter().position(|s| *s == origin).unwrap_or(4)] += 1;
            b.stream_interrupted += u64::from(matches!(
                log.status.as_str(),
                "stream_interrupted" | "upstream_read_timeout" | "read_error"
            ));
        }
    }
    if !matches!(log.status.as_str(), "cancelled" | "service_stopped") {
        let bucket = [5000, 15000, 60000, 180000, 600000]
            .iter()
            .position(|n| log.elapsed_ms < *n)
            .unwrap_or(5);
        b.elapsed_histogram[bucket] += 1;
    }
    if let Some(t) = t {
        b.reconnect_started += u64::from(t.reconnect_started);
        b.reconnect_recovered += u64::from(t.reconnect_started && log.status == "success");
    }
    tx.execute(
        "INSERT OR REPLACE INTO kv VALUES(?1,?2)",
        params![
            key,
            serde_json::to_string(&b).map_err(|_| "可靠性统计编码失败")?
        ],
    )
    .map_err(|_| "保存可靠性统计失败")?;
    tx.execute(
        "DELETE FROM kv WHERE key LIKE 'reliability/v1/hour/%' AND json_extract(value,'$.hour')<?1",
        [(at - 7 * 86400).div_euclid(3600) * 3600],
    )
    .map_err(|_| "清理可靠性统计失败")?;
    Ok(())
}
impl Store {
    pub fn reliability_summary(&self, at: i64) -> Result<Option<ReliabilitySummary>> {
        let db = self.db.lock().map_err(|_| "数据库锁异常")?;
        let start: Option<String> = db
            .query_row("SELECT value FROM kv WHERE key=?1", [START], |r| r.get(0))
            .optional()
            .map_err(|_| "读取可靠性覆盖时间失败")?;
        let Some(start) = start else {
            return Ok(None);
        };
        let start: i64 = start.parse().map_err(|_| "可靠性覆盖时间损坏")?;
        let mut st=db.prepare("SELECT value FROM kv WHERE key LIKE 'reliability/v1/hour/%' AND json_extract(value,'$.hour')>=?1 AND json_extract(value,'$.hour')<=?2 ORDER BY key DESC LIMIT 4097").map_err(|_| "读取可靠性统计失败")?;
        let rows = st
            .query_map(params![(at - 7 * 86400).div_euclid(3600) * 3600, at], |r| {
                r.get::<_, String>(0)
            })
            .map_err(|_| "读取可靠性统计失败")?;
        let mut buckets = Vec::new();
        for row in rows {
            buckets.push(
                serde_json::from_str::<ReliabilityHour>(&row.map_err(|_| "读取可靠性统计失败")?)
                    .map_err(|_| "可靠性统计损坏")?,
            );
        }
        let gap: Option<String> = db
            .query_row(
                "SELECT value FROM kv WHERE key='reliability/v1/gap'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(|_| "读取可靠性缺失标记失败")?;
        let gap = gap
            .map(|s| s.parse::<i64>())
            .transpose()
            .map_err(|_| "可靠性缺失标记损坏")?;
        let partial = start > at - 7 * 86400
            || buckets.len() > 4096
            || gap.is_some_and(|n| n >= at - 7 * 86400)
            || self.persistence_warning().is_some();
        buckets.truncate(4096);
        Ok(Some(ReliabilitySummary {
            coverage_start: start,
            snapshot_at: at,
            history_partial: partial,
            buckets,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn log(id: &str, status: &str) -> RequestLog {
        serde_json::from_value(serde_json::json!({"id":id,"at":crate::now(),"account":null,"operation":"responses","status":status,"elapsedMs":12000,"advice":"",
            "diagnostic":{"appVersion":"1.0.6","upstreamStatus":200,"responseType":"sse","failureStage":if status=="success" {serde_json::Value::Null}else{"response_read".into()},"networkErrorKind":"unexpected_eof","proxySource":"loopback_direct"}})).unwrap()
    }
    #[test]
    fn transactional_statistics_survive_log_pruning_restart_and_duplicate_ids() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pool.sqlite");
        let store = Store::open(&path).unwrap();
        assert!(store.reliability_summary(crate::now()).unwrap().is_none());
        for n in 0..2001 {
            store.log(&log(&n.to_string(), "success")).unwrap();
        }
        store.log(&log("0", "success")).unwrap();
        store.log(&log("failed", "stream_interrupted")).unwrap();
        store.log(&log("cancel", "cancelled")).unwrap();
        store.log(&log("stop", "service_stopped")).unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        let s = store.reliability_summary(crate::now()).unwrap().unwrap();
        assert!(s.valid(crate::now()) && s.history_partial);
        assert_eq!(s.buckets.iter().map(|b| b.total()).sum::<u64>(), 2004);
        assert_eq!(s.buckets.iter().map(|b| b.success).sum::<u64>(), 2001);
        assert_eq!(s.buckets.iter().map(|b| b.failure).sum::<u64>(), 1);
        store.reject_writes_for_test();
        assert!(store.log(&log("rejected", "success")).is_err());
        assert_eq!(
            store
                .reliability_summary(crate::now())
                .unwrap()
                .unwrap()
                .buckets
                .iter()
                .map(|b| b.total())
                .sum::<u64>(),
            2004
        );
        store
            .db
            .lock()
            .unwrap()
            .execute_batch("PRAGMA query_only=OFF")
            .unwrap();
        store.log(&log("rejected", "success")).unwrap();
        assert_eq!(
            store
                .reliability_summary(crate::now())
                .unwrap()
                .unwrap()
                .buckets
                .iter()
                .map(|b| b.total())
                .sum::<u64>(),
            2005
        );
    }
}
