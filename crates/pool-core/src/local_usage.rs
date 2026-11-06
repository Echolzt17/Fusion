//! Read-only Codex usage import. Only numeric evidence and hashed identities leave
//! the parser; unknown payloads (including conversation content) are discarded.
use crate::{
    storage::Store,
    usage::{AccountUsage, TokenUsage},
    Result,
};
use chrono::{DateTime, Local, TimeZone, Timelike, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

const PREFIX: &str = "local-usage/v1/";
const POOL: &str = "personal_codex_pool";
const MAX_LINE: usize = 1024 * 1024;
const FILE_BUDGET: u64 = 32 * 1024 * 1024;

#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct Cursor {
    offset: u64,
    head: String,
    head_len: usize,
    tail: String,
    modified: u128,
    provider: Option<String>,
    active: BTreeMap<String, TurnRecord>,
    partial: bool,
    legacy_tokens: bool,
    #[serde(default)]
    records: u64,
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct UsageRecord {
    at: i64,
    pool: bool,
    tokens: TokenUsage,
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct TurnRecord {
    pub(crate) start: i64,
    pub(crate) end: i64,
    pub(crate) complete: bool,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageDay {
    date: String,
    total_tokens: Option<u64>,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageHour {
    hour: u32,
    minutes: Option<f64>,
    provisional: bool,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Dashboard {
    pub tokens: TokenUsage,
    pub measured: bool,
    pub days: Vec<UsageDay>,
    pub hours: Vec<UsageHour>,
    pub today: String,
    pub timezone: String,
    pub updated_at: Option<i64>,
    pub scanning: bool,
    pub history_partial: bool,
    pub activity_partial: bool,
    pub error: Option<String>,
}
impl Default for Dashboard {
    fn default() -> Self {
        Self {
            tokens: TokenUsage::default(),
            measured: false,
            days: vec![],
            hours: vec![],
            today: String::new(),
            timezone: String::new(),
            updated_at: None,
            scanning: true,
            history_partial: false,
            activity_partial: false,
            error: None,
        }
    }
}

#[derive(Deserialize)]
struct Envelope<'a> {
    timestamp: Option<String>,
    #[serde(rename = "type")]
    kind: String,
    #[serde(borrow)]
    payload: &'a serde_json::value::RawValue,
}
#[derive(Default, Deserialize)]
struct Fields {
    #[serde(rename = "type")]
    kind: Option<String>,
    turn_id: Option<String>,
    response_id: Option<String>,
    model_provider: Option<String>,
    thread_settings: Option<Provider>,
    usage: Option<WireUsage>,
    started_at: Option<serde_json::Value>,
    completed_at: Option<serde_json::Value>,
}
#[derive(Deserialize)]
struct Provider {
    model_provider_id: Option<String>,
}
#[derive(Deserialize)]
struct WireUsage {
    input_tokens: u64,
    output_tokens: u64,
    total_tokens: Option<u64>,
    cached_input_tokens: Option<u64>,
    reasoning_output_tokens: Option<u64>,
}
impl WireUsage {
    fn tokens(self) -> TokenUsage {
        TokenUsage {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            total_tokens: self
                .total_tokens
                .unwrap_or(self.input_tokens.saturating_add(self.output_tokens)),
            cached_tokens: self.cached_input_tokens,
            reasoning_tokens: self.reasoning_output_tokens,
        }
    }
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn identity(value: Option<&str>) -> Option<String> {
    value
        .filter(|s| !s.is_empty() && s.len() <= 512)
        .map(|s| hash(s.as_bytes()))
}
fn provider(value: Option<String>) -> Option<String> {
    // Persist only the routing class, never arbitrary provider strings.
    value.filter(|v| !v.is_empty()).map(|v| {
        if v == POOL {
            POOL.into()
        } else {
            "other".into()
        }
    })
}
fn timestamp(value: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|v| v.timestamp_millis())
}
fn event_time(value: Option<&serde_json::Value>) -> Option<i64> {
    value.and_then(|v| {
        v.as_str()
            .and_then(timestamp)
            .or_else(|| v.as_i64().filter(|n| *n > 100_000_000_000))
    })
}

fn files(root: &Path, found: &mut Vec<PathBuf>) -> bool {
    let Ok(entries) = fs::read_dir(root) else {
        return false;
    };
    let mut complete = true;
    for entry in entries {
        let Ok(entry) = entry else {
            complete = false;
            continue;
        };
        let Ok(kind) = entry.file_type() else {
            complete = false;
            continue;
        };
        // Never follow junctions/symlinks into other client data or credentials.
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            complete &= files(&entry.path(), found);
        } else if kind.is_file() && entry.path().extension().is_some_and(|e| e == "jsonl") {
            found.push(entry.path());
        }
    }
    complete
}

// A bounded line reader also skips huge image/message records without allocating
// their contents. An incomplete final line is retried at the same byte offset.
fn line(reader: &mut impl BufRead) -> std::io::Result<(Vec<u8>, u64, bool)> {
    let mut bytes = Vec::new();
    let mut length = 0;
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            return Ok((bytes, length, false));
        }
        let end = chunk.iter().position(|b| *b == b'\n').map(|n| n + 1);
        let take = end.unwrap_or(chunk.len());
        if bytes.len() < MAX_LINE {
            bytes.extend_from_slice(&chunk[..take.min(MAX_LINE - bytes.len())]);
        }
        length += take as u64;
        reader.consume(take);
        if end.is_some() {
            return Ok((bytes, length, true));
        }
    }
}
fn fingerprint(file: &mut File, at: u64, len: usize) -> Result<String> {
    file.seek(SeekFrom::Start(at))
        .map_err(|_| "读取本机用量位置失败")?;
    let mut bytes = vec![0; len];
    file.read_exact(&mut bytes)
        .map_err(|_| "本机用量文件正在变化")?;
    Ok(hash(&bytes))
}

fn consume(
    bytes: &[u8],
    cursor: &mut Cursor,
    events: &mut Vec<(String, UsageRecord)>,
    turns: &mut BTreeMap<String, TurnRecord>,
) {
    let Ok(row) = serde_json::from_slice::<Envelope<'_>>(bytes) else {
        cursor.partial = true;
        return;
    };
    let at = row.timestamp.as_deref().and_then(timestamp);
    let selected = matches!(
        row.kind.as_str(),
        "session_meta" | "token_usage_record" | "event_msg"
    );
    let parsed = if selected {
        serde_json::from_str::<Fields>(row.payload.get()).ok()
    } else {
        None
    };
    // A subsequent task is not evidence that an abandoned previous task ran
    // throughout the intervening idle period.
    if parsed
        .as_ref()
        .is_some_and(|p| p.kind.as_deref() == Some("task_started"))
    {
        cursor.active.clear();
    }
    // A new observed event proves activity only up to its own timestamp.
    if let Some(at) = at {
        for (id, turn) in &mut cursor.active {
            turn.end = turn.end.max(at);
            turns.insert(id.clone(), turn.clone());
        }
    }
    if !selected {
        return;
    }
    let Some(p) = parsed else {
        cursor.partial = true;
        return;
    };
    match row.kind.as_str() {
        "session_meta" => {
            cursor.provider = provider(p.model_provider);
        }
        "token_usage_record" => {
            let (Some(id), Some(at), Some(usage), Some(source)) = (
                identity(p.response_id.as_deref()),
                at,
                p.usage,
                cursor.provider.as_deref(),
            ) else {
                cursor.partial = true;
                return;
            };
            // Forked rollouts can contain inherited records. Response identity,
            // rather than file offset or current thread, is the deduplication key.
            events.push((
                id,
                UsageRecord {
                    at,
                    pool: source == POOL,
                    tokens: usage.tokens(),
                },
            ));
            cursor.records += 1;
        }
        "event_msg" => match p.kind.as_deref() {
            Some("thread_settings_applied") => {
                if let Some(value) = p
                    .thread_settings
                    .and_then(|s| provider(s.model_provider_id))
                {
                    cursor.provider = Some(value);
                }
            }
            Some("token_count") => {
                cursor.legacy_tokens = true;
            }
            Some("task_started") => {
                let (Some(id), Some(at)) = (identity(p.turn_id.as_deref()), at) else {
                    cursor.partial = true;
                    return;
                };
                // Do not bridge an unclosed previous task across idle time.
                cursor.active.clear();
                let start = event_time(p.started_at.as_ref())
                    .filter(|s| *s <= at)
                    .unwrap_or(at);
                let turn = TurnRecord {
                    start,
                    end: at,
                    complete: false,
                };
                turns.insert(id.clone(), turn.clone());
                cursor.active.insert(id, turn);
            }
            Some("task_complete" | "turn_aborted") => {
                let Some(at) = at else {
                    cursor.partial = true;
                    return;
                };
                let id = identity(p.turn_id.as_deref()).or_else(|| {
                    (cursor.active.len() == 1).then(|| cursor.active.keys().next().unwrap().clone())
                });
                if let Some(id) = id {
                    let old = cursor.active.remove(&id);
                    let start = event_time(p.started_at.as_ref()).or_else(|| old.map(|v| v.start));
                    if let Some(start) = start.filter(|s| *s <= at) {
                        let end = event_time(p.completed_at.as_ref())
                            .filter(|e| *e >= start && *e <= at)
                            .unwrap_or(at);
                        turns.insert(
                            id,
                            TurnRecord {
                                start,
                                end,
                                complete: true,
                            },
                        );
                    } else {
                        cursor.partial = true;
                    }
                } else {
                    cursor.partial = true;
                }
            }
            _ => {}
        },
        _ => {}
    }
}

fn scan_file(store: &Store, path: &Path) -> Result<(bool, bool)> {
    let key = format!("{PREFIX}file/{}", hash(path.to_string_lossy().as_bytes()));
    let mut cursor: Cursor = store
        .get(&key)?
        .map(|s| serde_json::from_str(&s).map_err(|_| "本机读取进度损坏"))
        .transpose()?
        .unwrap_or_default();
    let mut file = File::open(path).map_err(|_| "部分本机会话暂不可读")?;
    let meta = file.metadata().map_err(|_| "读取本机会话状态失败")?;
    let length = meta.len();
    let modified = meta
        .modified()
        .ok()
        .and_then(|v| v.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|v| v.as_nanos())
        .unwrap_or(0);
    if cursor.offset == 0 {
        cursor = Cursor::default();
    }
    let tail_len = cursor.offset.min(512) as usize;
    let changed = length < cursor.offset
        || (cursor.offset > 0
            && (fingerprint(&mut file, 0, cursor.head_len)? != cursor.head
                || fingerprint(&mut file, cursor.offset - tail_len as u64, tail_len)?
                    != cursor.tail
                || (length == cursor.offset && modified != cursor.modified)));
    if changed {
        cursor = Cursor::default();
    }
    if cursor.offset == length {
        return Ok((cursor.partial, false));
    }
    if cursor.head_len == 0 {
        cursor.head_len = length.min(512) as usize;
        cursor.head = fingerprint(&mut file, 0, cursor.head_len)?;
    }
    file.seek(SeekFrom::Start(cursor.offset))
        .map_err(|_| "读取本机用量位置失败")?;
    let mut reader = BufReader::new(file);
    let start_offset = cursor.offset;
    let mut events = Vec::new();
    let mut turns = BTreeMap::new();
    let mut partial_line = false;
    while cursor.offset - start_offset < FILE_BUDGET {
        let (bytes, len, complete) = line(&mut reader).map_err(|_| "读取本机用量失败")?;
        if !complete {
            partial_line = len > 0;
            break;
        }
        if len <= MAX_LINE as u64 {
            consume(&bytes, &mut cursor, &mut events, &mut turns);
        } else {
            cursor.partial = true;
        }
        cursor.offset += len;
    }
    let file = reader.get_mut();
    let tail_len = cursor.offset.min(512) as usize;
    cursor.tail = fingerprint(file, cursor.offset - tail_len as u64, tail_len)?;
    cursor.modified = modified;
    store.save_local_usage(
        &key,
        &cursor,
        &events,
        &turns.into_iter().collect::<Vec<_>>(),
    )?;
    Ok((cursor.partial, !partial_line && cursor.offset < length))
}

// The client index is optional and read-only; it is used solely to detect missing
// historical rollouts. It never supplies totals, titles, messages or identities.
fn missing_history(root: &Path) -> bool {
    let db = root.join("state_5.sqlite");
    if !db.exists() {
        return false;
    }
    (|| -> std::result::Result<bool, rusqlite::Error> {
        let db =
            rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        db.busy_timeout(std::time::Duration::from_millis(100))?;
        let mut query = db.prepare("SELECT rollout_path FROM threads")?;
        let paths = query.query_map([], |r| r.get::<_, String>(0))?;
        for path in paths {
            if !Path::new(&path?).is_file() {
                return Ok(true);
            }
        }
        Ok(false)
    })()
    .unwrap_or(true)
}

pub fn scan(store: &Store, root: &Path, now: i64) -> Result<Dashboard> {
    let mut paths = Vec::new();
    let mut partial = !files(&root.join("sessions"), &mut paths);
    if root.join("archived_sessions").exists() {
        partial |= !files(&root.join("archived_sessions"), &mut paths);
    }
    let mut scanning = false;
    let mut error = None;
    paths.sort();
    for path in paths {
        match scan_file(store, &path) {
            Ok((incomplete, pending)) => {
                partial |= incomplete;
                scanning |= pending;
            }
            Err(_) => {
                partial = true;
                error = Some("部分本机用量暂不可读，已保留上次记录".into());
            }
        }
    }
    partial |= missing_history(root);
    let events: Vec<UsageRecord> = store.local_usage_values(&format!("{PREFIX}event/"))?;
    let turns: Vec<TurnRecord> = store.local_usage_values(&format!("{PREFIX}turn/"))?;
    let cursors: Vec<Cursor> = store.local_usage_values(&format!("{PREFIX}file/"))?;
    if cursors
        .iter()
        .any(|c| c.partial || (c.legacy_tokens && c.records == 0))
    {
        partial = true;
    }
    let accounts: Vec<AccountUsage> = store.local_usage_values("usage/v1/")?;
    let mut result = summarize(&events, &turns, &accounts, now, &Local);
    if turns.is_empty() {
        for hour in &mut result.hours {
            hour.minutes = None;
        }
    }
    result.history_partial |= partial;
    result.scanning = scanning;
    result.error = error;
    Ok(result)
}

fn summarize<T: TimeZone>(
    events: &[UsageRecord],
    turns: &[TurnRecord],
    accounts: &[AccountUsage],
    now: i64,
    tz: &T,
) -> Dashboard
where
    T::Offset: std::fmt::Display,
{
    let now_time = Utc
        .timestamp_millis_opt(now)
        .single()
        .expect("valid clock")
        .with_timezone(tz);
    let today = now_time.date_naive();
    let mut result = Dashboard {
        today: today.to_string(),
        timezone: format!("UTC{}", now_time.offset()),
        updated_at: Some(now),
        scanning: false,
        ..Dashboard::default()
    };
    let mut pool_total = 0u64;
    for account in accounts {
        result.tokens.add(&account.tokens);
        result.measured |= account.measured_requests > 0;
        pool_total = pool_total.saturating_add(account.tokens.total_tokens);
    }
    let mut local_pool = 0u64;
    let mut days = BTreeMap::<String, u64>::new();
    for event in events {
        // A live file can append after the scan's clock snapshot. Persist it now,
        // expose it on the next snapshot instead of advancing past and losing it.
        if event.at > now {
            continue;
        }
        if event.pool {
            local_pool = local_pool.saturating_add(event.tokens.total_tokens);
        } else {
            result.tokens.add(&event.tokens);
            result.measured = true;
        }
        if let Some(time) = Utc
            .timestamp_millis_opt(event.at)
            .single()
            .filter(|v| v.timestamp_millis() <= now)
        {
            let date = time.with_timezone(tz).date_naive().to_string();
            let total = days.entry(date).or_default();
            *total = total.saturating_add(event.tokens.total_tokens);
        }
    }
    result.history_partial = local_pool != pool_total;
    result.days = (0..7)
        .rev()
        .map(|ago| {
            let date = (today - chrono::Days::new(ago)).to_string();
            UsageDay {
                total_tokens: days.get(&date).copied(),
                date,
            }
        })
        .collect();
    // Union before bucketing. Clamp to a window containing today's entire local
    // day, then split in UTC minutes so fractional minutes and DST stay correct.
    let mut intervals: Vec<_> = turns
        .iter()
        .filter_map(|t| {
            let start = t.start.max(now.saturating_sub(27 * 3600 * 1000));
            let end = t.end.min(now);
            (end > start).then_some((start, end))
        })
        .collect();
    intervals.sort_unstable();
    let mut merged: Vec<(i64, i64)> = Vec::new();
    for (start, end) in intervals {
        if let Some(last) = merged.last_mut().filter(|last| start <= last.1) {
            last.1 = last.1.max(end);
        } else {
            merged.push((start, end));
        }
    }
    let mut minutes = [0f64; 24];
    let mut provisional = [false; 24];
    for (mut start, end) in merged {
        while start < end {
            let next = ((start.div_euclid(60_000) + 1) * 60_000).min(end);
            if let Some(time) = Utc.timestamp_millis_opt(start).single() {
                let local = time.with_timezone(tz);
                if local.date_naive() == today {
                    minutes[local.hour() as usize] += (next - start) as f64 / 60_000.0;
                }
            }
            start = next;
        }
    }
    for turn in turns.iter().filter(|t| !t.complete) {
        let mut at = turn.start.max(now.saturating_sub(27 * 3600 * 1000));
        while at < turn.end.min(now) {
            let local = Utc.timestamp_millis_opt(at).unwrap().with_timezone(tz);
            if local.date_naive() == today {
                provisional[local.hour() as usize] = true;
            }
            at = ((at.div_euclid(60_000) + 1) * 60_000).min(turn.end);
        }
    }
    result.activity_partial = provisional.iter().any(|p| *p);
    result.hours = (0..24)
        .map(|hour| UsageHour {
            hour,
            minutes: (hour <= now_time.hour()).then_some(minutes[hour as usize].min(60.0)),
            provisional: provisional[hour as usize],
        })
        .collect();
    result
}

#[cfg(test)]
mod tests;
