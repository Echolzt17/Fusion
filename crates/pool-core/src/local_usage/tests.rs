use super::*;
use serde_json::{json, Value};
use std::io::Write;

fn at(s: &str) -> i64 {
    timestamp(s).unwrap()
}
fn row(time: &str, kind: &str, payload: Value) -> String {
    format!(
        "{}\n",
        json!({"timestamp":time,"type":kind,"payload":payload})
    )
}
fn meta(source: &str) -> String {
    row(
        "2026-11-04T00:00:00Z",
        "session_meta",
        json!({"id":"private-thread","model_provider":source,"base_instructions":"PRIVATE_BODY_CANARY"}),
    )
}
fn usage(id: &str, time: &str, input: u64) -> String {
    row(
        time,
        "token_usage_record",
        json!({"response_id":id,"usage":{"input_tokens":input,"output_tokens":5,"total_tokens":input+5,"cached_input_tokens":2,"reasoning_output_tokens":1},"thread_token_usage":{"total_tokens":999999}}),
    )
}
fn event(time: &str, kind: &str, id: &str) -> String {
    row(
        time,
        "event_msg",
        json!({"type":kind,"turn_id":id,"last_agent_message":"PRIVATE_BODY_CANARY"}),
    )
}
fn setup() -> (tempfile::TempDir, Store, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("codex");
    fs::create_dir_all(root.join("sessions")).unwrap();
    let store = Store::open(&temp.path().join("pool.sqlite")).unwrap();
    (temp, store, root)
}
const NOW: &str = "2026-11-04T08:00:00Z";

// Explicitly selected measurement, never part of normal tests or production
// startup. All records, paths and the SQLite database belong to this fixture.
#[test]
#[ignore = "synthetic large-history measurement; run alone with --ignored --nocapture"]
fn synthetic_large_history_scan_measurement() {
    use std::{io::BufWriter, time::Instant};
    const FILES: usize = 128;
    const PER_FILE: usize = 256;
    let (temp, store, root) = setup();
    let mut source_bytes = 0u64;
    for file in 0..FILES {
        let path = root.join(format!("sessions/fixture-{file:04}.jsonl"));
        let mut writer = BufWriter::new(File::create(&path).unwrap());
        writer.write_all(row("2026-11-04T01:00:00Z", "session_meta",
            json!({"id":format!("fixture-thread-{file}"),"model_provider":"openai"})).as_bytes()).unwrap();
        for index in 0..PER_FILE {
            let id = format!("fixture-{file}-{index}");
            for line in [
                row("2026-11-04T01:00:00Z", "event_msg", json!({"type":"task_started","turn_id":id})),
                usage(&id, "2026-11-04T01:00:00Z", 10),
                row("2026-11-04T01:00:01Z", "event_msg", json!({"type":"task_complete","turn_id":id})),
            ] { writer.write_all(line.as_bytes()).unwrap(); }
        }
        writer.flush().unwrap();
        source_bytes += fs::metadata(path).unwrap().len();
    }
    let memory_before = measurement_memory();
    let started = Instant::now();
    let cold = scan(&store, &root, at(NOW)).unwrap();
    let cold_ms = started.elapsed().as_secs_f64() * 1000.;
    let memory_after_cold = measurement_memory();
    let started = Instant::now();
    let unchanged = scan(&store, &root, at(NOW)).unwrap();
    let unchanged_ms = started.elapsed().as_secs_f64() * 1000.;
    let memory_after_unchanged = measurement_memory();
    assert_eq!(cold.tokens.total_tokens, (FILES * PER_FILE * 15) as u64);
    assert_eq!(unchanged.tokens, cold.tokens);
    assert!(!cold.history_partial && !unchanged.history_partial);

    // Independent warm phase samples: they localize work; their sum need not
    // exactly equal the separately measured full unchanged scan above.
    let started = Instant::now();
    let mut paths = Vec::new();
    assert!(files(&root.join("sessions"), &mut paths));
    paths.sort();
    for path in paths { assert_eq!(scan_file(&store, &path).unwrap(), (false, false)); }
    let file_validation_ms = started.elapsed().as_secs_f64() * 1000.;
    let started = Instant::now();
    let events: Vec<UsageRecord> = store.local_usage_values("local-usage/v1/event/").unwrap();
    let turns: Vec<TurnRecord> = store.local_usage_values("local-usage/v1/turn/").unwrap();
    let cursors: Vec<Cursor> = store.local_usage_values("local-usage/v1/file/").unwrap();
    let accounts: Vec<AccountUsage> = store.local_usage_values("usage/v1/").unwrap();
    let load_numeric_ms = started.elapsed().as_secs_f64() * 1000.;
    assert_eq!(events.len(), FILES * PER_FILE);
    assert_eq!(turns.len(), FILES * PER_FILE);
    assert_eq!(cursors.len(), FILES);
    let started = Instant::now();
    let summary = summarize(&events, &turns, &accounts, at(NOW), &Local);
    let summarize_ms = started.elapsed().as_secs_f64() * 1000.;
    assert_eq!(summary.tokens, cold.tokens);
    let database_bytes = fs::metadata(temp.path().join("pool.sqlite")).unwrap().len();
    eprintln!("{}", json!({
        "measurement":"synthetic_local_usage", "buildProfile":if cfg!(debug_assertions) {"debug"} else {"release"},
        "files":FILES,"usageEvents":events.len(),"turns":turns.len(),"sourceBytes":source_bytes,"databaseBytes":database_bytes,
        "coldScanMs":cold_ms,"unchangedScanMs":unchanged_ms,
        "independentWarmPhasesMs":{"fileValidation":file_validation_ms,"loadAllNumericRowsAndDecode":load_numeric_ms,"summarize":summarize_ms},
        "processMemory":{"beforeCold":memory_before,"afterCold":memory_after_cold,"afterUnchanged":memory_after_unchanged},
        "scope":"temporary synthetic history only; memory covers entire test process; samples are not phase-specific peak memory or SQLite lock contention"
    }));
}

#[cfg(windows)]
fn measurement_memory() -> Option<Value> {
    // Test-only, current process metadata. No process enumeration, handles to
    // other processes, session data or credentials are accessed.
    #[repr(C)]
    #[derive(Default)]
    struct Counters {
        cb: u32, page_fault_count: u32, peak_working_set_size: usize, working_set_size: usize,
        quota_peak_paged_pool_usage: usize, quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize, quota_non_paged_pool_usage: usize,
        pagefile_usage: usize, peak_pagefile_usage: usize,
    }
    #[link(name = "psapi")]
    unsafe extern "system" {
        fn GetProcessMemoryInfo(process: *mut std::ffi::c_void, counters: *mut Counters, size: u32) -> i32;
    }
    let mut counters = Counters { cb: std::mem::size_of::<Counters>() as u32, ..Counters::default() };
    // -1 is Windows' documented pseudo-handle for the calling process.
    let result = unsafe { GetProcessMemoryInfo(-1isize as *mut _, &mut counters, std::mem::size_of::<Counters>() as u32) };
    (result != 0).then(|| json!({"workingSetBytes":counters.working_set_size,"processPeakWorkingSetBytes":counters.peak_working_set_size}))
}
#[cfg(not(windows))]
fn measurement_memory() -> Option<Value> { None }

#[test]
fn official_pool_custom_totals_deduplicate_and_preserve_removed_accounts() {
    let (_temp, store, root) = setup();
    let mut pool = AccountUsage {
        account: "removed-fixture".into(),
        ..Default::default()
    };
    pool.record(
        at(NOW) / 1000,
        Some(&TokenUsage {
            input_tokens: 30,
            output_tokens: 5,
            total_tokens: 35,
            cached_tokens: Some(2),
            reasoning_tokens: Some(1),
        }),
    );
    store
        .set(
            "usage/v1/removed-fixture",
            &serde_json::to_string(&pool).unwrap(),
        )
        .unwrap();
    fs::write(
        root.join("sessions/a.jsonl"),
        meta("openai") + &usage("official", "2026-11-04T01:00:00Z", 10),
    )
    .unwrap();
    fs::write(
        root.join("sessions/b.jsonl"),
        meta(POOL) + &usage("pool", "2026-11-04T02:00:00Z", 30),
    )
    .unwrap();
    fs::write(
        root.join("sessions/c.jsonl"),
        meta("custom")
            + &usage("custom", "2026-11-04T03:00:00Z", 20)
            + &usage("official", "2026-11-04T01:00:00Z", 10),
    )
    .unwrap();
    let first = scan(&store, &root, at(NOW)).unwrap();
    assert_eq!(first.tokens.total_tokens, 75);
    assert_eq!(first.tokens.input_tokens, 60);
    assert_eq!(first.tokens.cached_tokens, Some(6));
    assert!(!first.history_partial);
    assert_eq!(scan(&store, &root, at(NOW)).unwrap().tokens, first.tokens);
    let rows: Vec<UsageRecord> = store.local_usage_values("local-usage/v1/event/").unwrap();
    assert_eq!(rows.len(), 3);
    let tz = chrono::FixedOffset::east_opt(8 * 3600).unwrap();
    let days = summarize(&rows, &[], &[pool], at(NOW), &tz).days;
    assert_eq!(days[6].total_tokens, Some(75));
}

#[test]
fn archive_restart_delete_and_partial_append_do_not_lose_or_repeat_usage() {
    let (temp, store, root) = setup();
    let file = root.join("sessions/a.jsonl");
    let whole = usage("fixture-response", "2026-11-04T01:00:00Z", 10);
    fs::write(&file, meta("openai") + &whole[..whole.len() / 2]).unwrap();
    assert!(!scan(&store, &root, at(NOW)).unwrap().measured);
    File::options()
        .append(true)
        .open(&file)
        .unwrap()
        .write_all(&whole.as_bytes()[whole.len() / 2..])
        .unwrap();
    assert_eq!(
        scan(&store, &root, at(NOW)).unwrap().tokens.total_tokens,
        15
    );
    fs::create_dir(root.join("archived_sessions")).unwrap();
    let archived = root.join("archived_sessions/a.jsonl");
    fs::rename(&file, &archived).unwrap();
    drop(store);
    let store = Store::open(&temp.path().join("pool.sqlite")).unwrap();
    assert_eq!(
        scan(&store, &root, at(NOW)).unwrap().tokens.total_tokens,
        15
    );
    fs::remove_file(archived).unwrap();
    assert_eq!(
        scan(&store, &root, at(NOW)).unwrap().tokens.total_tokens,
        15
    );
}

#[test]
fn scan_whitelist_skips_bodies_snapshots_and_resets_and_handles_provider_changes() {
    let (temp, store, root) = setup();
    let content = meta("openai")
        + &usage("response-private", "2026-11-04T01:00:00Z", 10)
        + &row(
            "2026-11-04T01:00:01Z",
            "event_msg",
            json!({"type":"token_count","info":{"total_token_usage":{"total_tokens":999999}}}),
        )
        + &row(
            "2026-11-04T01:00:02Z",
            "event_msg",
            json!({"type":"token_count","info":{"total_token_usage":{"total_tokens":2}}}),
        )
        + &row(
            "2026-11-04T01:00:03Z",
            "response_item",
            json!({"content":"PRIVATE_BODY_CANARY","authorization":"PRIVATE_CREDENTIAL_CANARY"}),
        )
        + &row(
            "2026-11-04T01:00:04Z",
            "event_msg",
            json!({"type":"thread_settings_applied","thread_settings":{"model_provider_id":POOL}}),
        )
        + &usage("pool-private", "2026-11-04T01:01:00Z", 30);
    fs::write(root.join("sessions/a.jsonl"), content).unwrap();
    let result = scan(&store, &root, at(NOW)).unwrap();
    assert_eq!(result.tokens.total_tokens, 15);
    assert!(result.history_partial);
    let db = rusqlite::Connection::open_with_flags(
        temp.path().join("pool.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut query = db
        .prepare("SELECT key,value FROM kv WHERE key LIKE 'local-usage/%'")
        .unwrap();
    let values = query
        .query_map([], |r| {
            Ok(format!(
                "{}{}",
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?
            ))
        })
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap()
        .join("");
    for canary in [
        "PRIVATE_BODY_CANARY",
        "PRIVATE_CREDENTIAL_CANARY",
        "private-thread",
        "response-private",
        "pool-private",
    ] {
        assert!(!values.contains(canary));
    }
}

#[test]
fn active_intervals_union_split_midnight_hour_and_never_extend_to_scan_time() {
    let tz = chrono::FixedOffset::east_opt(8 * 3600).unwrap();
    let turns = vec![
        TurnRecord {
            start: at("2026-11-03T15:50:00Z"),
            end: at("2026-11-03T16:20:00Z"),
            complete: true,
        },
        TurnRecord {
            start: at("2026-11-03T16:10:00Z"),
            end: at("2026-11-03T17:10:00Z"),
            complete: true,
        },
        TurnRecord {
            start: at("2026-11-03T17:05:00Z"),
            end: at("2026-11-03T17:20:30Z"),
            complete: false,
        },
    ];
    let result = summarize(&[], &turns, &[], at("2026-11-03T18:00:00Z"), &tz);
    assert_eq!(result.today, "2026-11-04");
    assert_eq!(result.hours[0].minutes, Some(60.0));
    assert_eq!(result.hours[1].minutes, Some(20.5));
    assert!(result.hours[1].provisional);
    assert_eq!(result.hours[2].minutes, Some(0.0));
    assert_eq!(result.hours[3].minutes, None);
    assert!(result
        .hours
        .iter()
        .filter_map(|h| h.minutes)
        .all(|n| n <= 60.0));
    let records = vec![UsageRecord {
        at: at("2026-11-03T16:00:00Z"),
        pool: false,
        tokens: TokenUsage {
            total_tokens: 15,
            ..Default::default()
        },
    }];
    let result = summarize(&records, &[], &[], at(NOW), &tz);
    assert_eq!(result.days[6].total_tokens, Some(15));
    assert_eq!(result.days[5].total_tokens, None);
}

#[test]
fn cancelled_and_unclosed_tasks_do_not_bridge_idle_time_or_reopen_on_rescan() {
    let (_temp, store, root) = setup();
    fs::write(
        root.join("sessions/a.jsonl"),
        meta("openai")
            + &event("2026-11-04T01:00:00Z", "task_started", "first")
            + &usage("one", "2026-11-04T01:10:00Z", 10)
            + &event("2026-11-04T02:00:00Z", "task_started", "second")
            + &event("2026-11-04T02:05:00Z", "turn_aborted", "second"),
    )
    .unwrap();
    scan(&store, &root, at(NOW)).unwrap();
    let turns: Vec<TurnRecord> = store.local_usage_values("local-usage/v1/turn/").unwrap();
    let result = summarize(&[], &turns, &[], at(NOW), &Utc);
    assert_eq!(result.hours[1].minutes, Some(10.0));
    assert_eq!(result.hours[2].minutes, Some(5.0));
    let key = format!("local-usage/v1/turn/{}", hash(b"second"));
    assert!(
        serde_json::from_str::<TurnRecord>(&store.get(&key).unwrap().unwrap())
            .unwrap()
            .complete
    );
    fs::write(
        root.join("sessions/b.jsonl"),
        meta("openai") + &event("2026-11-04T02:00:00Z", "task_started", "second"),
    )
    .unwrap();
    scan(&store, &root, at(NOW)).unwrap();
    assert!(
        serde_json::from_str::<TurnRecord>(&store.get(&key).unwrap().unwrap())
            .unwrap()
            .complete
    );
}

#[test]
fn legacy_missing_and_rewritten_history_are_reported_without_guessing() {
    let (_temp, store, root) = setup();
    let file = root.join("sessions/a.jsonl");
    fs::write(
        &file,
        meta("openai")
            + &row(
                "2026-11-04T01:00:00Z",
                "event_msg",
                json!({"type":"token_count","info":{"total_token_usage":{"total_tokens":999}}}),
            ),
    )
    .unwrap();
    let result = scan(&store, &root, at(NOW)).unwrap();
    assert!(result.history_partial);
    assert!(!result.measured);
    assert!(result.hours.iter().all(|h| h.minutes.is_none()));
    fs::write(
        &file,
        meta("custom") + &usage("new", "2026-11-04T01:00:00Z", 7),
    )
    .unwrap();
    assert_eq!(
        scan(&store, &root, at(NOW)).unwrap().tokens.total_tokens,
        12
    );
    let db = rusqlite::Connection::open(root.join("state_5.sqlite")).unwrap();
    db.execute_batch("CREATE TABLE threads(rollout_path TEXT);")
        .unwrap();
    db.execute(
        "INSERT INTO threads VALUES(?1)",
        [root.join("missing.jsonl").to_string_lossy().as_ref()],
    )
    .unwrap();
    assert!(scan(&store, &root, at(NOW)).unwrap().history_partial);
    assert_eq!(
        db.query_row("SELECT count(*) FROM threads", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn events_written_after_scan_clock_are_not_skipped_and_failed_commits_retry() {
    let (temp, store, root) = setup();
    fs::write(
        root.join("sessions/a.jsonl"),
        meta("openai") + &usage("future", "2026-11-04T08:00:01Z", 10),
    )
    .unwrap();
    assert_eq!(scan(&store, &root, at(NOW)).unwrap().tokens.total_tokens, 0);
    assert_eq!(
        scan(&store, &root, at("2026-11-04T08:00:02Z"))
            .unwrap()
            .tokens
            .total_tokens,
        15
    );
    // A failed transaction cannot leave an advanced cursor without its evidence.
    fs::write(
        root.join("sessions/b.jsonl"),
        meta("openai") + &usage("retry", "2026-11-04T01:00:00Z", 20),
    )
    .unwrap();
    store.reject_writes_for_test();
    let result = scan(&store, &root, at("2026-11-04T08:00:02Z")).unwrap();
    assert!(result.error.is_some());
    assert_eq!(result.tokens.total_tokens, 15);
    drop(store);
    let store = Store::open(&temp.path().join("pool.sqlite")).unwrap();
    assert_eq!(
        scan(&store, &root, at("2026-11-04T08:00:02Z"))
            .unwrap()
            .tokens
            .total_tokens,
        40
    );
}
