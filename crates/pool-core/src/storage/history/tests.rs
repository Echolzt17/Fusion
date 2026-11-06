use super::*;
use serde_json::json;

fn log(id: &str, key: Option<&str>, start: i64) -> RequestLog {
    serde_json::from_value(json!({"id":id,"at":500,"startedAt":start,"account":null,
        "conversationKey":key,"operation":"responses","status":"success","elapsedMs":1,"advice":"",
        "requestedModel":"requested-model","responseModel":"actual-model",
        "usage":{"inputTokens":10,"outputTokens":2,"totalTokens":12,"cachedTokens":null,"reasoningTokens":null}})).unwrap()
}

#[test]
fn summary_queries_use_existing_key_index_and_ignore_unrelated_long_term_records() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("pool.sqlite")).unwrap();
    store.log(&log("old", Some("thread:old"), 100)).unwrap();
    store.log(&log("new", Some("thread:new"), 200)).unwrap();
    for key in ["conversation-usage/v0/old", "conversation-usage/v10/new", "conversation-usage/v2/future", "usage-seen/v1/example"] {
        store.set(key, "not-summary-json").unwrap();
    }
    {
        let mut db = store.db.lock().unwrap();
        let tx = db.transaction().unwrap();
        for n in 0..10000 {
            tx.execute("INSERT INTO kv VALUES(?1,'1')", [format!("usage-seen/v1/{n}")]).unwrap();
        }
        tx.commit().unwrap();
        for (query, args) in [(CONVERSATION_SQL, vec!["conversation-usage/v1/thread:old"]), (CONVERSATIONS_SQL, vec![]), (LATEST_CONVERSATION_SQL, vec![])] {
            let mut statement = db.prepare(&format!("EXPLAIN QUERY PLAN {query}")).unwrap();
            let plan = statement.query_map(rusqlite::params_from_iter(args), |r| r.get::<_, String>(3)).unwrap()
                .collect::<rusqlite::Result<Vec<_>>>().unwrap().join(";");
            assert!(plan.contains("SEARCH kv USING INDEX"), "{plan}");
            assert!(!plan.contains("SCAN kv"), "{plan}");
        }
    }
    assert_eq!(store.conversations().unwrap().len(), 2);
    assert_eq!(store.conversation(None).unwrap().unwrap().conversation_key, "thread:new");
    let fixed = store.conversation(Some("thread:old")).unwrap().unwrap();
    assert_eq!(fixed.conversation_key, "thread:old");
    assert_eq!(fixed.requests, 1);
    assert!(store.conversation(Some("thread:missing")).unwrap().is_none());
    assert!(store.conversation(Some("%")).unwrap().is_none());
}

#[test]
fn cumulative_history_survives_pruning_reopen_and_duplicate_ids() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pool.sqlite");
    let store = Store::open(&path).unwrap();
    for i in 0..2005 {
        store
            .log(&log(&format!("r{i}"), Some("thread:anonymous"), i))
            .unwrap();
    }
    store.log(&log("r0", Some("thread:anonymous"), 0)).unwrap(); // already pruned: still counted once
    let summaries = store.conversations().unwrap();
    assert_eq!(summaries[0].requests, 2005);
    assert_eq!(summaries[0].tokens.total_tokens, 2005 * 12);
    assert_eq!(summaries[0].last_started_at, 2004);
    assert!(!summaries[0].history_partial);
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.conversations().unwrap()[0].requests, 2005);
    let mut cursor = None;
    let mut ids = std::collections::HashSet::new();
    loop {
        let page = store.log_page(None, "all", false, cursor).unwrap();
        assert!(page.items.len() <= 50);
        for item in page.items {
            assert!(ids.insert(item.id));
        }
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(ids.len(), 2000);
}

#[test]
fn legacy_import_and_filters_keep_unknown_separate_and_start_order_stable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pool.sqlite");
    let store = Store::open(&path).unwrap();
    let mut old = log("old", Some("thread:a"), 1000);
    old.started_at = None;
    old.requested_model = None;
    old.response_model = None;
    store
        .db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO logs VALUES(?1,?2,?3)",
            params![old.id, old.at, serde_json::to_string(&old).unwrap()],
        )
        .unwrap();
    store
        .db
        .lock()
        .unwrap()
        .execute(
            "DELETE FROM kv WHERE key='conversation-usage-initialized/v1'",
            [],
        )
        .unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    assert!(store.conversations().unwrap()[0].history_partial);
    let mut unknown = log("unknown", None, 600000);
    unknown.usage = None;
    unknown.status = "cancelled".into();
    store.log(&unknown).unwrap();
    let mut late = log("older-finishes-later", Some("thread:a"), 2000);
    late.usage = None;
    store.log(&late).unwrap();
    store.log(&log("newer", Some("thread:b"), 800000)).unwrap();
    store.log(&log("late", Some("thread:a"), 3000)).unwrap();
    assert_eq!(
        store.conversations().unwrap()[0].conversation_key,
        "thread:b"
    );
    assert_eq!(
        store
            .log_page(Some("thread:a"), "success", false, None)
            .unwrap()
            .items
            .len(),
        3
    );
    assert_eq!(
        store.log_page(None, "other", false, None).unwrap().items[0].id,
        "unknown"
    );
    assert_eq!(store.conversations().unwrap().len(), 2);
    assert_eq!(store.conversations().unwrap()[1].measured_requests, 2);
    store.note_request_started(RecentRequest {
        id: "new".into(),
        conversation_key: Some("thread:b".into()),
        started_at: 900000,
        account: None,
    });
    store.note_request_started(RecentRequest {
        id: "older".into(),
        conversation_key: Some("thread:a".into()),
        started_at: 800000,
        account: None,
    });
    store.note_request_account("older", "wrong");
    store.note_request_account("new", "right");
    assert_eq!(
        store.recent_request().unwrap().account.as_deref(),
        Some("right")
    );
    assert!(store.log_page(None, "invalid", false, None).is_err());
}

#[test]
fn combined_filters_precede_pagination_and_catalog_covers_retained_history() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("pool.sqlite")).unwrap();
    for i in 0..315 {
        let mut item = log(&format!("filter-{i}"), Some("thread:a"), i);
        item.operation = if i < 55 { "image_edit" } else { "responses" }.into();
        item.account = Some("account-a".into());
        store.log(&item).unwrap();
    }
    let mut missing = log("unknown", None, 116);
    missing.operation = "image_generate".into();
    missing.requested_model = None;
    missing.response_model = None;
    store.log(&missing).unwrap();
    let mut system = log("system", None, 117);
    system.operation = "models".into();
    store.log(&system).unwrap();
    let filter = || HistoryFilter {
        operations: vec!["image_edit".into()],
        account: Some("account-a".into()),
        model: Some("actual-model".into()),
        ..Default::default()
    };
    let first = store
        .filtered_log_page(Some("thread:a"), "success", false, None, filter())
        .unwrap();
    assert_eq!(first.items.len(), 50);
    assert!(first.items.iter().all(|l| l.operation == "image_edit"));
    let second = store
        .filtered_log_page(
            Some("thread:a"),
            "success",
            false,
            first.next_cursor,
            filter(),
        )
        .unwrap();
    assert_eq!(second.items.len(), 5);
    assert!(second.next_cursor.is_none());
    let unknown = store
        .filtered_log_page(
            None,
            "all",
            false,
            None,
            HistoryFilter {
                unknown_conversation: true,
                model: Some(String::new()),
                account: Some(String::new()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(unknown.items.len(), 1);
    assert_eq!(unknown.items[0].id, "unknown");
    let catalog = store.history_catalog().unwrap();
    assert_eq!(
        catalog
            .requests
            .branches
            .iter()
            .map(|b| b.requests)
            .sum::<u64>(),
        316
    );
    assert_eq!(catalog.system.branches[0].requests, 1);
    assert!(catalog.requests.models.contains("requested-model"));
    assert!(catalog.requests.models.contains("actual-model"));
    assert!(catalog.requests.unknown_model);
    assert!(store
        .filtered_log_page(
            None,
            "all",
            false,
            None,
            HistoryFilter {
                operations: vec!["invalid".into()],
                ..Default::default()
            }
        )
        .is_err());
}
