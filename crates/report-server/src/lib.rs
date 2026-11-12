//! Private error store. This process never depends on pool-core, Vault, Tauri or AI credentials.
use axum::{body::Bytes, extract::{DefaultBodyLimit, Path, Query, State}, http::{HeaderMap, StatusCode}, middleware::{self, Next}, response::{IntoResponse, Response}, routing::{get, post}, Json, Router};
use fusion_report_protocol::{Receipt, Report, WIRE_LIMIT};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{path::{Path as FsPath, PathBuf}, sync::{Arc, Mutex}, time::{SystemTime, UNIX_EPOCH}};
use subtle::ConstantTimeEq;
use axum::Extension;
mod report_index;
mod reliability;
mod known;
mod new_events;
pub fn now() -> i64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64 }
type Result<T> = std::result::Result<T, &'static str>;
pub struct Store { db: Connection, root: PathBuf, max_bytes: u64 }
impl Store {
    pub fn open(root: &FsPath, max_bytes: u64) -> Result<Self> {
        std::fs::create_dir_all(root).map_err(|_| "data_directory_failed")?;
        let path = root.join("reports.sqlite");
        let mut db = Connection::open(&path).map_err(|_| "database_open_failed")?;
        let v: u32 = db.query_row("PRAGMA user_version", [], |r| r.get(0)).map_err(|_| "database_version_failed")?;
        if v > 1 { return Err("database_newer_than_server"); }
        db.busy_timeout(std::time::Duration::from_secs(5)).map_err(|_| "database_config_failed")?;
        if v == 0 {
            // Back up any preexisting SQLite database before schema creation.
            let tables: usize = db.query_row("SELECT count(*) FROM sqlite_master WHERE type='table'", [], |r| r.get(0)).map_err(|_| "database_version_failed")?;
            if tables > 0 {
                let mut dst = Connection::open(root.join(format!("before-schema-{}.sqlite", now()))).map_err(|_| "backup_failed")?;
                rusqlite::backup::Backup::new(&db, &mut dst).and_then(|b| b.run_to_completion(64, std::time::Duration::from_millis(10), None)).map_err(|_| "backup_failed")?;
                return Err("unrecognized_database_schema");
            }
            db.execute_batch("PRAGMA auto_vacuum=INCREMENTAL;
                BEGIN;
                CREATE TABLE reports(id TEXT PRIMARY KEY, digest BLOB NOT NULL, received INTEGER NOT NULL, receipt TEXT NOT NULL, body TEXT NOT NULL);
                CREATE TABLE errors(installation TEXT NOT NULL, request TEXT NOT NULL, signature TEXT NOT NULL, occurred INTEGER NOT NULL, received INTEGER NOT NULL, version TEXT NOT NULL, record TEXT NOT NULL, report TEXT NOT NULL, PRIMARY KEY(installation,request));
                CREATE INDEX errors_signature ON errors(signature,occurred);
                CREATE INDEX errors_received ON errors(received);
                CREATE TABLE events(id INTEGER PRIMARY KEY AUTOINCREMENT, created INTEGER NOT NULL, kind TEXT NOT NULL, body TEXT NOT NULL);
                CREATE TABLE state(key TEXT PRIMARY KEY,value INTEGER NOT NULL);
                PRAGMA user_version=1; COMMIT;").map_err(|_| "database_init_failed")?;
        }
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA journal_size_limit=16777216;").map_err(|_| "database_config_failed")?;
        new_events::initialize(&mut db, now())?;
        let size: u64 = db.query_row("PRAGMA page_size", [], |r| r.get(0)).map_err(|_| "database_config_failed")?;
        db.pragma_update(None, "max_page_count", (max_bytes / size).max(32)).map_err(|_| "database_config_failed")?;
        Ok(Self { db, root: root.into(), max_bytes })
    }
    pub fn ingest(&mut self, report: &Report, at: i64) -> Result<Receipt> {
        report.validate()?;
        let canonical = serde_json::to_vec(report).map_err(|_| "invalid_report")?;
        let digest = Sha256::digest(&canonical);
        let existing: Option<(Vec<u8>, String)> = self.db.query_row("SELECT digest,receipt FROM reports WHERE id=?1", [&report.report_id], |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(|_| "database_read_failed")?;
        if let Some((old, receipt)) = existing {
            if old != digest.as_slice() { return Err("report_id_conflict"); }
            return serde_json::from_str(&receipt).map_err(|_| "database_read_failed");
        }
        let used: u64 = self.db.query_row("SELECT page_count*page_size FROM pragma_page_count(),pragma_page_size()", [], |r| r.get(0)).map_err(|_| "database_read_failed")?;
        if used.saturating_add(canonical.len() as u64 * 3) > self.max_bytes { return Err("storage_limit"); }
        let tx = self.db.transaction().map_err(|_| "database_write_failed")?;
        let mut accepted = 0;
        let mut new_errors = Vec::new();
        for error in &report.errors {
            let record = serde_json::to_string(error).map_err(|_| "invalid_report")?;
            let inserted = tx.execute("INSERT OR IGNORE INTO errors VALUES(?1,?2,?3,?4,?5,?6,?7,?8)", params![report.installation_id,error.request_id,error.signature(),if report.prepared_at>at+300 || error.at>at+300 {0} else {error.at},at,error.app_version.as_deref().unwrap_or("unknown"),record,report.report_id]).map_err(|_| "database_write_failed")?;
            accepted += inserted;
            if inserted > 0 { new_errors.push(error); }
        }
        let receipt = Receipt { report_id: report.report_id.clone(), received_at: at, accepted_errors: accepted, duplicate_errors: report.errors.len() - accepted };
        tx.execute("INSERT INTO reports VALUES(?1,?2,?3,?4,?5)", params![report.report_id,digest.as_slice(),at,serde_json::to_string(&receipt).map_err(|_| "invalid_report")?,String::from_utf8(canonical).map_err(|_| "invalid_report")?]).map_err(|_| "database_write_failed")?;
        if !new_errors.is_empty() {
            let summary = new_events::summary(report, &new_errors, at)?;
            tx.execute("INSERT INTO events(created,kind,body) VALUES(?1,'new_errors',?2)", params![at,summary])
                .map_err(|_| "database_write_failed")?;
        }
        tx.commit().map_err(|_| "database_write_failed")?;
        Ok(receipt)
    }
    fn groups(&self, from: i64, until: i64, occurred: bool) -> Result<Vec<Value>> {
        let column = if occurred { "occurred" } else { "received" };
        // Read the original diagnostic version, including for previously misattributed rows.
        // No rewrite of stored reports, receipts or historical notification snapshots.
        let sql = format!("SELECT signature,count(*),min(nullif(occurred,0)),max(nullif(occurred,0)),min(request),group_concat(DISTINCT coalesce(json_extract(record,'$.appVersion'),'unknown')),sum(occurred=0),sum(occurred>0 AND occurred<received-86400) FROM errors WHERE {column}>?1 AND {column}<=?2 GROUP BY signature ORDER BY count(*) DESC,signature");
        let mut st = self.db.prepare(&sql).map_err(|_| "database_read_failed")?;
        let rows = st.query_map(params![from,until], |r| Ok(json!({"signature":r.get::<_,String>(0)?,"count":r.get::<_,u64>(1)?,"firstAt":r.get::<_,Option<i64>>(2)?,"lastAt":r.get::<_,Option<i64>>(3)?,"sampleRequestId":r.get::<_,String>(4)?,"versions":r.get::<_,String>(5)?,"timeUncertainErrors":r.get::<_,u64>(6)?,"historicalErrors":r.get::<_,u64>(7)?}))).map_err(|_| "database_read_failed")?;
        let mut groups = rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|_| "database_read_failed")?;
        for group in &mut groups {
            let report: String = self.db.query_row("SELECT report FROM errors WHERE signature=?1 AND request=?2 LIMIT 1", params![group["signature"].as_str(), group["sampleRequestId"].as_str()], |r| r.get(0)).map_err(|_| "database_read_failed")?;
            group["sampleReportId"] = report.into();
        }
        Ok(groups)
    }
    pub fn sweep(&mut self, at: i64) -> Result<()> {
        // Snapshot the connection before starting the event transaction. Store is externally serialized.
        let groups = self.groups(at - 86400, at, true)?;
        // Most recent 10:00 Asia/Shanghai, represented as UTC epoch seconds.
        let day_cutoff = (at + 8 * 3600 - 10 * 3600).div_euclid(86400) * 86400 + 2 * 3600;
        let previous: Option<i64> = self.db.query_row("SELECT value FROM state WHERE key='daily_cutoff'", [], |r| r.get(0)).optional().map_err(|_| "database_read_failed")?;
        let daily = if previous != Some(day_cutoff) { self.groups(previous.unwrap_or(day_cutoff - 86400), day_cutoff, false)? } else { vec![] };
        let tx = self.db.transaction().map_err(|_| "database_write_failed")?;
        for group in groups {
            if group["count"].as_u64().unwrap_or(0) < 6 { continue; }
            let key = format!("early:{}", group["signature"].as_str().unwrap());
            let last: Option<i64> = tx.query_row("SELECT value FROM state WHERE key=?1", [&key], |r| r.get(0)).optional().map_err(|_| "database_read_failed")?;
            // Only newly received samples can rearm an unchanged 24h window.
            let fresh: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM errors WHERE signature=?1 AND received>?2)", params![group["signature"].as_str(), last.unwrap_or(0)], |r| r.get(0)).map_err(|_| "database_read_failed")?;
            if fresh && last.is_none_or(|last| at - last >= 86400) {
                let body = json!({"source":"unverified_client_reports","windowHours":24,"groups":[group]});
                tx.execute("INSERT INTO events(created,kind,body) VALUES(?1,'early',?2)", params![at,body.to_string()]).map_err(|_| "database_write_failed")?;
                tx.execute("INSERT OR REPLACE INTO state VALUES(?1,?2)", params![key,at]).map_err(|_| "database_write_failed")?;
            }
        }
        if previous != Some(day_cutoff) {
            if !daily.is_empty() {
                let body = json!({"source":"unverified_client_reports","from":previous.unwrap_or(day_cutoff-86400),"until":day_cutoff,"groups":daily});
                tx.execute("INSERT INTO events(created,kind,body) VALUES(?1,'daily',?2)", params![at,body.to_string()]).map_err(|_| "database_write_failed")?;
            }
            tx.execute("INSERT OR REPLACE INTO state VALUES('daily_cutoff',?1)", [day_cutoff]).map_err(|_| "database_write_failed")?;
        }
        tx.execute("INSERT OR REPLACE INTO state VALUES('last_sweep',?1)",[at]).map_err(|_| "database_write_failed")?;
        tx.execute("DELETE FROM reports WHERE received<?1", [at - 30 * 86400]).map_err(|_| "database_write_failed")?;
        tx.execute("DELETE FROM errors WHERE received<?1", [at - 30 * 86400]).map_err(|_| "database_write_failed")?;
        tx.execute("DELETE FROM events WHERE created<?1", [at - 90 * 86400]).map_err(|_| "database_write_failed")?;
        tx.commit().map_err(|_| "database_write_failed")?;
        self.db.execute_batch("PRAGMA incremental_vacuum(256); PRAGMA wal_checkpoint(PASSIVE);").map_err(|_| "database_maintenance_failed")?;
        Ok(())
    }
    pub fn events(&self, after: i64) -> Result<Vec<Value>> {
        let mut st = self.db.prepare("SELECT id,created,kind,body FROM events WHERE id>?1 ORDER BY id LIMIT 50").map_err(|_| "database_read_failed")?;
        let rows = st.query_map([after.max(0)], |r| { let body: String = r.get(3)?; Ok(json!({"id":r.get::<_,i64>(0)?,"createdAt":r.get::<_,i64>(1)?,"kind":r.get::<_,String>(2)?,"summary":serde_json::from_str::<Value>(&body).unwrap_or(Value::Null)})) }).map_err(|_| "database_read_failed")?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|_| "database_read_failed")
    }
    fn report(&self, id: &str) -> Result<Value> {
        if !fusion_report_protocol::uuid_ok(id) { return Err("not_found"); }
        let body: Option<String> = self.db.query_row("SELECT body FROM reports WHERE id=?1", [id], |r| r.get(0)).optional().map_err(|_| "database_read_failed")?;
        serde_json::from_str(&body.ok_or("not_found")?).map_err(|_| "database_read_failed")
    }
    pub fn backup(&self, at: i64) -> Result<()> {
        let dir = self.root.join("backups"); std::fs::create_dir_all(&dir).map_err(|_| "backup_failed")?;
        let path = dir.join(format!("backup-{at}.sqlite"));
        if path.exists() { return Err("backup_already_exists"); }
        let mut dst = Connection::open(&path).map_err(|_| "backup_failed")?;
        rusqlite::backup::Backup::new(&self.db, &mut dst).and_then(|b| b.run_to_completion(128, std::time::Duration::from_millis(10), None)).map_err(|_| "backup_failed")?;
        let check: String = dst.query_row("PRAGMA quick_check", [], |r| r.get(0)).map_err(|_| "backup_failed")?;
        if check != "ok" { return Err("backup_failed"); }
        let mut paths: Vec<_> = std::fs::read_dir(&dir).map_err(|_| "backup_failed")?.filter_map(|e| e.ok()).map(|e| e.path())
            .filter(|p| p.file_name().and_then(|s| s.to_str()).is_some_and(|s| s.strip_prefix("backup-").and_then(|s| s.strip_suffix(".sqlite")).is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit())))).collect();
        paths.sort(); paths.reverse();
        for old in paths.into_iter().skip(7) { std::fs::remove_file(old).map_err(|_| "backup_cleanup_failed")?; }
        Ok(())
    }
    fn health(&self) -> Result<Value> {
        let free=fs2::available_space(&self.root).map_err(|_| "disk_check_failed")?;
        let latest=std::fs::read_dir(self.root.join("backups")).ok().into_iter().flatten().filter_map(|e|e.ok())
            .filter_map(|e|e.file_name().to_str().and_then(|s|s.strip_prefix("backup-").and_then(|s|s.strip_suffix(".sqlite"))).and_then(|s|s.parse::<i64>().ok())).max();
        let last_sweep: Option<i64>=self.db.query_row("SELECT value FROM state WHERE key='last_sweep'",[],|r|r.get(0)).optional().map_err(|_| "database_read_failed")?;
        let mut issues=Vec::new();
        if free<10*1024*1024*1024 {issues.push("disk_space_low");}
        if latest.is_none_or(|t|now()-t>36*3600) {issues.push("backup_overdue");}
        if last_sweep.is_none_or(|t|now()-t>1800) {issues.push("sweep_overdue");}
        let immediate_errors_after_cursor: i64 = self.db.query_row("SELECT value FROM state WHERE key='immediate_errors_after_cursor'", [], |r| r.get(0)).map_err(|_| "database_read_failed")?;
        Ok(json!({"diskFreeBytes":free,"lastBackupAt":latest,"lastSweepAt":last_sweep,"issues":issues,"immediateErrorsAfterCursor":immediate_errors_after_cursor}))
    }
}

pub struct Server { pub store: Mutex<Store>, admin_key: String, slots: Arc<tokio::sync::Semaphore>, minimum_free: u64 }
impl Server {
    pub fn new(store: Store, key: String, minimum_free: u64) -> Result<Arc<Self>> {
        if key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) { return Err("invalid_admin_key"); }
        Ok(Arc::new(Self { store: Mutex::new(store), admin_key: key, slots: Arc::new(tokio::sync::Semaphore::new(4)), minimum_free }))
    }
    fn authorized(&self, h: &HeaderMap) -> bool {
        h.get("authorization").and_then(|v| v.to_str().ok()).and_then(|s| s.strip_prefix("Bearer ")).is_some_and(|key| bool::from(key.as_bytes().ct_eq(self.admin_key.as_bytes())))
    }
}
fn failure(code: &'static str) -> Response {
    let status = match code { "unauthorized" => StatusCode::UNAUTHORIZED, "not_found" => StatusCode::NOT_FOUND,
        "report_id_conflict" => StatusCode::CONFLICT, "storage_limit" | "disk_space_low" => StatusCode::INSUFFICIENT_STORAGE,
        "report_too_large" => StatusCode::PAYLOAD_TOO_LARGE, "busy" => StatusCode::TOO_MANY_REQUESTS,
        "invalid_report" | "invalid_error" | "invalid_gzip" | "invalid_query" => StatusCode::BAD_REQUEST, _ => StatusCode::SERVICE_UNAVAILABLE };
    (status, [("cache-control", "no-store")], Json(json!({"error":code}))).into_response()
}
async fn ingest(State(s): State<Arc<Server>>, Extension(permit): Extension<Arc<tokio::sync::OwnedSemaphorePermit>>, headers: HeaderMap, bytes: Bytes) -> Response {
    if headers.get("content-type").and_then(|v| v.to_str().ok()) != Some("application/gzip") { return failure("invalid_report"); }
    // A bounded blocking worker performs gzip/schema/SQLite work while holding a request slot.
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let report = Report::decode(&bytes)?;
        let mut store = s.store.lock().map_err(|_| "database_lock_failed")?;
        if fs2::available_space(&store.root).map_err(|_| "disk_check_failed")? < s.minimum_free { return Err("disk_space_low"); }
        store.ingest(&report, now())
    }).await;
    match result { Ok(Ok(receipt)) => (StatusCode::OK, [("cache-control", "no-store")], Json(receipt)).into_response(), Ok(Err(e)) => failure(e), Err(_) => failure("worker_failed") }
}
#[derive(Deserialize)]
struct Cursor { #[serde(default)] after: i64 }
async fn events(State(s): State<Arc<Server>>, headers: HeaderMap, Query(cursor): Query<Cursor>) -> Response {
    if !s.authorized(&headers) { return failure("unauthorized"); }
    let result = tokio::task::spawn_blocking(move || s.store.lock().map_err(|_| "database_lock_failed")?.events(cursor.after)).await;
    match result { Ok(Ok(events)) => ([("cache-control", "no-store")], Json(json!({"events":events}))).into_response(), _ => failure("database_read_failed") }
}
async fn report(State(s): State<Arc<Server>>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    if !s.authorized(&headers) { return failure("unauthorized"); }
    let result = tokio::task::spawn_blocking(move || s.store.lock().map_err(|_| "database_lock_failed")?.report(&id)).await;
    match result { Ok(Ok(report)) => ([("cache-control", "no-store")], Json(report)).into_response(), Ok(Err(e)) => failure(e), _ => failure("database_read_failed") }
}
async fn health(State(s): State<Arc<Server>>, headers: HeaderMap) -> Response {
    if !s.authorized(&headers) {return failure("unauthorized");}
    match tokio::task::spawn_blocking(move ||s.store.lock().map_err(|_| "database_lock_failed")?.health()).await {
        Ok(Ok(value))=>([("cache-control","no-store")],Json(value)).into_response(),
        _=>failure("health_check_failed"),
    }
}
async fn bound(State(s): State<Arc<Server>>, mut request: axum::extract::Request, next: Next) -> Response {
    let Ok(permit) = s.slots.clone().try_acquire_owned() else { return failure("busy"); };
    request.extensions_mut().insert(Arc::new(permit));
    match tokio::time::timeout(std::time::Duration::from_secs(60), next.run(request)).await { Ok(r) => r, Err(_) => failure("request_timeout") }
}
pub fn router(s: Arc<Server>) -> Router {
    Router::new().route("/health", get(|| async { "ok" }))
        .route("/v1/error-reports", post(ingest))
        .route("/v1/error-reports/known", post(known::check).layer(DefaultBodyLimit::max(fusion_report_protocol::KNOWN_JSON_LIMIT)))
        .route("/v1/admin/events", get(events)).route("/v1/admin/reports/{id}", get(report))
        .route("/v1/admin/reports", get(report_index::list))
        .route("/v1/admin/reliability", get(reliability::list))
        .route("/v1/admin/health", get(health))
        .layer(DefaultBodyLimit::max(WIRE_LIMIT)).layer(middleware::from_fn_with_state(s.clone(), bound)).with_state(s)
}

#[cfg(test)] mod tests;
