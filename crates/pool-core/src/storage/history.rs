use super::*;
use crate::usage::ConversationUsage;
use rusqlite::{OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};

const CONVERSATIONS_SQL: &str = "SELECT value FROM kv WHERE key >= 'conversation-usage/v1/' AND key < 'conversation-usage/v10'";
const CONVERSATION_SQL: &str = "SELECT value FROM kv WHERE key=?1";
const LATEST_CONVERSATION_SQL: &str = "SELECT value FROM kv WHERE key >= 'conversation-usage/v1/' AND key < 'conversation-usage/v10' ORDER BY json_extract(value,'$.lastStartedAt') DESC,key LIMIT 1";

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentRequest {
    pub id: String,
    pub conversation_key: Option<String>,
    pub started_at: i64,
    pub account: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct LogCursor {
    pub at: i64,
    pub row: i64,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogPage {
    pub items: Vec<RequestLog>,
    pub next_cursor: Option<LogCursor>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct HistoryFilter {
    pub unknown_conversation: bool,
    pub operations: Vec<String>,
    // Empty string selects missing metadata; None means no filter.
    pub account: Option<String>,
    pub model: Option<String>,
}
#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryFacets {
    pub branches: Vec<HistoryBranch>,
    pub accounts: std::collections::BTreeSet<Option<String>>,
    pub models: std::collections::BTreeSet<String>,
    pub unknown_model: bool,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryBranch {
    pub conversation_key: Option<String>,
    pub operation: String,
    pub requests: u64,
}
#[derive(Default, Serialize)]
pub struct HistoryCatalog {
    pub requests: HistoryFacets,
    pub system: HistoryFacets,
}

pub(super) fn is_request(log: &RequestLog) -> bool {
    matches!(
        log.operation.as_str(),
        "responses" | "compact" | "image_generate" | "image_edit"
    )
}
pub(super) fn record_conversation(
    tx: &Transaction<'_>,
    log: &RequestLog,
    partial: bool,
) -> Result<()> {
    let Some(conversation) = &log.conversation_key else {
        return Ok(());
    };
    let key = format!("conversation-usage/v1/{conversation}");
    let raw: Option<String> = tx
        .query_row("SELECT value FROM kv WHERE key=?1", [&key], |r| r.get(0))
        .optional()
        .map_err(|_| "读取对话累计失败")?;
    let mut usage: ConversationUsage = raw
        .map(|s| serde_json::from_str(&s))
        .transpose()
        .map_err(|_| "对话累计数据损坏")?
        .unwrap_or_default();
    usage.conversation_key = conversation.clone();
    usage.history_partial |= partial;
    usage.record(log);
    tx.execute(
        "INSERT INTO kv VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![
            key,
            serde_json::to_string(&usage).map_err(|_| "对话累计编码失败")?
        ],
    )
    .map_err(|_| "保存对话累计失败")?;
    Ok(())
}
impl Store {
    // Only existing numeric/hashed log metadata is imported, once, transactionally.
    pub(super) fn initialize_history(&self) -> Result<()> {
        let mut db = self.db.lock().map_err(|_| "数据库锁异常")?;
        let tx = db.transaction().map_err(|_| "初始化对话统计失败")?;
        let initialized: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM kv WHERE key='conversation-usage-initialized/v1')",
                [],
                |r| r.get(0),
            )
            .map_err(|_| "读取对话统计状态失败")?;
        if !initialized {
            let logs = {
                let mut st = tx
                    .prepare("SELECT body FROM logs ORDER BY at,rowid")
                    .map_err(|_| "读取历史日志失败")?;
                let rows = st
                    .query_map([], |r| r.get::<_, String>(0))
                    .map_err(|_| "读取历史日志失败")?;
                rows.map(|r| {
                    serde_json::from_str::<RequestLog>(&r.map_err(|_| "读取历史日志失败")?)
                        .map_err(|_| "日志损坏".to_owned())
                })
                .collect::<Result<Vec<_>>>()?
            };
            for log in logs.iter().filter(|l| is_request(l)) {
                record_conversation(&tx, log, true)?;
                tx.execute(
                    "INSERT OR IGNORE INTO kv VALUES(?1,'1')",
                    [format!("usage-seen/v1/{}", log.id)],
                )
                .map_err(|_| "初始化统计去重失败")?;
            }
            tx.execute(
                "INSERT INTO kv VALUES('conversation-usage-initialized/v1','1')",
                [],
            )
            .map_err(|_| "初始化对话统计失败")?;
        }
        tx.commit().map_err(|_| "提交对话统计失败".into())
    }
    pub fn conversations(&self) -> Result<Vec<ConversationUsage>> {
        let db = self.db.lock().map_err(|_| "数据库锁异常")?;
        let mut st = db
            .prepare(CONVERSATIONS_SQL)
            .map_err(|_| "读取对话累计失败")?;
        let rows = st
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|_| "读取对话累计失败")?;
        let raw = rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|_| "读取对话累计失败")?;
        drop(st);
        drop(db);
        let mut values = raw.into_iter()
            .map(|r| {
                serde_json::from_str::<ConversationUsage>(&r)
                    .map_err(|_| "对话累计数据损坏".into())
            })
            .collect::<Result<Vec<_>>>()?;
        values.sort_by(|a, b| {
            b.last_started_at
                .cmp(&a.last_started_at)
                .then(a.conversation_key.cmp(&b.conversation_key))
        });
        Ok(values)
    }
    pub fn conversation(&self, key: Option<&str>) -> Result<Option<ConversationUsage>> {
        let db = self.db.lock().map_err(|_| "数据库锁异常")?;
        let raw: Option<String> = match key {
            Some(key) => db.query_row(CONVERSATION_SQL, [format!("conversation-usage/v1/{key}")], |r| r.get(0)),
            None => db.query_row(LATEST_CONVERSATION_SQL, [], |r| r.get(0)),
        }.optional().map_err(|_| "读取对话累计失败")?;
        drop(db);
        raw.map(|s| serde_json::from_str(&s).map_err(|_| "对话累计数据损坏".into()))
            .transpose()
    }
    pub fn note_request_started(&self, request: RecentRequest) {
        if let Ok(mut recent) = self.recent_request.lock() {
            if recent
                .as_ref()
                .is_none_or(|r| r.started_at <= request.started_at)
            {
                *recent = Some(request);
            }
        }
    }
    pub fn note_request_account(&self, id: &str, account: &str) {
        if let Ok(mut recent) = self.recent_request.lock() {
            if let Some(r) = recent.as_mut().filter(|r| r.id == id) {
                r.account = Some(account.into());
            }
        }
    }
    pub fn recent_request(&self) -> Option<RecentRequest> {
        self.recent_request.lock().ok().and_then(|r| r.clone())
    }
    pub fn log_page(
        &self,
        conversation: Option<&str>,
        result: &str,
        system: bool,
        cursor: Option<LogCursor>,
    ) -> Result<LogPage> {
        self.filtered_log_page(
            conversation,
            result,
            system,
            cursor,
            HistoryFilter::default(),
        )
    }
    pub fn history_catalog(&self) -> Result<HistoryCatalog> {
        let mut catalog = HistoryCatalog::default();
        let db = self.db.lock().map_err(|_| "数据库锁异常")?;
        let mut statement = db
            .prepare("SELECT body FROM logs ORDER BY at DESC,rowid DESC")
            .map_err(|_| "读取请求目录失败")?;
        let rows = statement
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|_| "读取请求目录失败")?;
        for row in rows {
            let log: RequestLog = serde_json::from_str(&row.map_err(|_| "读取请求目录失败")?)
                .map_err(|_| "日志损坏")?;
            let facets = if log.operation == "models" {
                &mut catalog.system
            } else if is_request(&log) {
                &mut catalog.requests
            } else {
                continue;
            };
            if let Some(branch) = facets.branches.iter_mut().find(|b| {
                b.conversation_key == log.conversation_key && b.operation == log.operation
            }) {
                branch.requests += 1;
            } else {
                facets.branches.push(HistoryBranch {
                    conversation_key: log.conversation_key,
                    operation: log.operation,
                    requests: 1,
                });
            }
            facets.accounts.insert(log.account);
            facets.unknown_model |= log.requested_model.is_none() && log.response_model.is_none();
            facets.models.extend(log.requested_model);
            facets.models.extend(log.response_model);
        }
        Ok(catalog)
    }
    pub fn filtered_log_page(
        &self,
        conversation: Option<&str>,
        result: &str,
        system: bool,
        cursor: Option<LogCursor>,
        filter: HistoryFilter,
    ) -> Result<LogPage> {
        if !matches!(result, "all" | "success" | "other") {
            return Err("请求结果筛选无效".into());
        }
        if (filter.unknown_conversation && conversation.is_some())
            || filter.operations.iter().any(|op| {
                !matches!(
                    op.as_str(),
                    "responses" | "compact" | "image_generate" | "image_edit" | "models"
                )
            })
        {
            return Err("请求分类筛选无效".into());
        }
        let operations =
            serde_json::to_string(&filter.operations).map_err(|_| "请求分类编码失败")?;
        let db = self.db.lock().map_err(|_| "数据库锁异常")?;
        let mut st = db.prepare("SELECT body,at,rowid FROM logs WHERE
            (?1 IS NULL OR json_extract(body,'$.conversationKey')=?1)
            AND ((?2=1 AND json_extract(body,'$.operation')='models') OR (?2=0 AND json_extract(body,'$.operation') IN ('responses','compact','image_generate','image_edit')))
            AND (?3='all' OR (?3='success' AND json_extract(body,'$.status')='success') OR (?3='other' AND json_extract(body,'$.status')!='success'))
            AND (?4 IS NULL OR at<?4 OR (at=?4 AND rowid<?5))
            AND (?6=0 OR json_extract(body,'$.conversationKey') IS NULL)
            AND (?7='[]' OR json_extract(body,'$.operation') IN (SELECT value FROM json_each(?7)))
            AND (?8 IS NULL OR (?8='' AND json_extract(body,'$.account') IS NULL) OR json_extract(body,'$.account')=?8)
            AND (?9 IS NULL OR (?9='' AND json_extract(body,'$.requestedModel') IS NULL AND json_extract(body,'$.responseModel') IS NULL) OR json_extract(body,'$.requestedModel')=?9 OR json_extract(body,'$.responseModel')=?9)
            ORDER BY at DESC,rowid DESC LIMIT 51").map_err(|_| "读取请求记录失败")?;
        let rows = st
            .query_map(
                params![
                    conversation,
                    system,
                    result,
                    cursor.as_ref().map(|c| c.at),
                    cursor.as_ref().map(|c| c.row),
                    filter.unknown_conversation,
                    operations,
                    filter.account,
                    filter.model
                ],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        LogCursor {
                            at: r.get(1)?,
                            row: r.get(2)?,
                        },
                    ))
                },
            )
            .map_err(|_| "读取请求记录失败")?;
        let mut items = Vec::new();
        let mut last = None;
        let mut more = false;
        for row in rows {
            let (raw, cursor) = row.map_err(|_| "读取请求记录失败")?;
            if items.len() == 50 {
                more = true;
                break;
            }
            items.push(serde_json::from_str(&raw).map_err(|_| "日志损坏")?);
            last = Some(cursor);
        }
        Ok(LogPage {
            items,
            next_cursor: if more { last } else { None },
        })
    }
}

#[cfg(test)]
mod tests;
