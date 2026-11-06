use super::*;
use serde_json::json;

fn log(id: &str, key: Option<&str>, operation: &str, status: &str) -> RequestLog {
    serde_json::from_value(json!({"id":id,"at":1,"startedAt":1000,"account":"account-a",
        "conversationKey":key,"operation":operation,"status":status,"elapsedMs":1,"advice":"",
        "requestedModel":"requested","responseModel":"actual",
        "usage":{"inputTokens":10,"outputTokens":2,"totalTokens":12,"cachedTokens":null,"reasoningTokens":null}})).unwrap()
}

#[test]
fn scopes_models_outcomes_and_missing_usage_are_disjoint() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("pool.sqlite")).unwrap();
    store.log(&log("a", Some("thread:a"), "responses", "success")).unwrap();
    let mut failed = log("b", Some("thread:a"), "image_edit", "network_error");
    failed.response_model = None;
    failed.usage = None;
    store.log(&failed).unwrap();
    store.log(&log("c", None, "compact", "cancelled")).unwrap();
    store.log(&log("d", Some("thread:b"), "image_generate", "service_stopped")).unwrap();
    store.log(&log("catalog", None, "models", "success")).unwrap();
    let (all, current) = store.request_summaries(Some("thread:a")).unwrap();
    let current = current.unwrap();
    assert_eq!((all.requests, all.success, all.failed, all.other), (4, 1, 1, 2));
    assert_eq!((all.measured_requests, all.total_tokens), (3, 36));
    assert_eq!(all.models.iter().map(|m| m.requests).sum::<u64>(), 4);
    assert_eq!(all.operations.values().sum::<u64>(), 4);
    assert_eq!((current.requests, current.success, current.failed, current.other), (2, 1, 1, 0));
    assert_eq!((current.measured_requests, current.total_tokens), (1, 12));
    assert!(current.models.iter().any(|m| m.name == "actual" && !m.requested && m.requests == 1));
    assert!(current.models.iter().any(|m| m.name == "requested" && m.requested && m.requests == 1));
    assert!(store.request_summaries(None).unwrap().1.is_none());
    assert_eq!(store.request_summaries(Some("thread:absent")).unwrap().1.unwrap().requests, 0);
}

#[test]
fn totals_survive_pruning_duplicate_ids_reopen_and_account_removal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pool.sqlite");
    let store = Store::open(&path).unwrap();
    for n in 0..2003 { store.log(&log(&n.to_string(), Some("thread:a"), "responses", "success")).unwrap(); }
    store.log(&log("0", Some("thread:a"), "responses", "success")).unwrap();
    assert_eq!(store.db.lock().unwrap().query_row("SELECT COUNT(*) FROM logs", [], |r| r.get::<_, u64>(0)).unwrap(), 2000);
    store.delete_account("account-a").unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    let (all, current) = store.request_summaries(Some("thread:a")).unwrap();
    assert_eq!((all.requests, all.total_tokens, all.success), (2003, 2003 * 12, 2003));
    assert_eq!(current.unwrap().requests, 2003);
    assert!(!all.history_partial);
}

#[test]
fn old_totals_keep_proven_usage_and_mark_unrecoverable_classifications() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pool.sqlite");
    let store = Store::open(&path).unwrap();
    for n in 0..4 { store.log(&log(&n.to_string(), Some("thread:a"), "responses", "success")).unwrap(); }
    {
        let db = store.db.lock().unwrap();
        db.execute("DELETE FROM kv WHERE key LIKE 'request-summary/v1/%'", []).unwrap();
        db.execute("DELETE FROM logs WHERE id IN ('0','1')", []).unwrap();
    }
    drop(store);
    let store = Store::open(&path).unwrap();
    let (all, current) = store.request_summaries(Some("thread:a")).unwrap();
    let current = current.unwrap();
    assert_eq!((all.requests, all.total_tokens, all.success, all.unclassified_requests), (4, 48, 2, 2));
    assert_eq!((current.requests, current.total_tokens, current.unclassified_requests), (4, 48, 2));
    assert!(all.history_partial && current.history_partial);
    store.log(&log("0", Some("thread:a"), "responses", "success")).unwrap();
    assert_eq!(store.request_summaries(None).unwrap().0.requests, 4);
    store.log(&log("new", None, "image_generate", "success")).unwrap();
    let all = store.request_summaries(None).unwrap().0;
    assert_eq!((all.requests, all.success, all.unclassified_requests), (5, 3, 2));
}

#[test]
fn failed_transaction_does_not_publish_a_success() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("pool.sqlite")).unwrap();
    store.reject_writes_for_test();
    assert!(store.log(&log("failed-write", Some("thread:a"), "responses", "success")).is_err());
    assert_eq!(store.request_summaries(None).unwrap().0.requests, 0);
    assert!(store.persistence_warning().is_some());
}

#[test]
fn legacy_accountless_pruned_requests_preserve_a_proven_global_lower_bound() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pool.sqlite");
    let store = Store::open(&path).unwrap();
    for n in 0..4 {
        let mut request = log(&n.to_string(), Some("thread:a"), "responses", "success");
        request.account = None;
        store.log(&request).unwrap();
    }
    {
        let db = store.db.lock().unwrap();
        db.execute("DELETE FROM kv WHERE key LIKE 'request-summary/v1/%'", []).unwrap();
        db.execute("DELETE FROM logs WHERE id IN ('0','1')", []).unwrap();
    }
    drop(store);
    let store = Store::open(&path).unwrap();
    let (all, current) = store.request_summaries(Some("thread:a")).unwrap();
    assert_eq!((all.requests, all.total_tokens, all.unclassified_requests), (4, 48, 2));
    assert_eq!(all.requests, current.unwrap().requests);
    assert!(all.history_partial);
}
