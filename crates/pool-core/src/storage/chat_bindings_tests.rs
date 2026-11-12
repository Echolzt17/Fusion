use crate::{scheduler::binding_key, tests::{add, fixture}};

#[test]
fn chat_preference_switch_is_atomic_and_preserves_hard_references() {
    let (_dir, accounts) = fixture();
    add(&accounts, "a"); add(&accounts, "b");
    let chats = vec![binding_key("thread", "chat"), binding_key("session", "chat")];
    let hard: Vec<_> = ["turn", "opaque", "response", "image_file", "image_generation", "image_call"]
        .into_iter().map(|kind| binding_key(kind, "reference")).collect();
    accounts.store.bind_many(&chats, "a").unwrap();
    accounts.store.bind_many(&hard, "a").unwrap();
    accounts.store.switch_chat_bindings(&chats, "a", "b").unwrap();
    for key in &chats {
        assert_eq!(accounts.store.binding(key).unwrap().as_deref(), Some("b"));
        assert_eq!(accounts.store.get(&format!("portable-chat/v1/{key}")).unwrap().as_deref(), Some("b"));
    }
    for key in &hard {
        assert_eq!(accounts.store.binding(key).unwrap().as_deref(), Some("a"));
        assert!(accounts.store.switch_chat_bindings(std::slice::from_ref(key), "a", "b").is_err());
        assert!(accounts.store.bind(key, "b").is_err());
    }
    assert!(accounts.store.bind_many(&chats, "a").is_err());
    let version: i64 = accounts.store.db.lock().unwrap()
        .query_row("PRAGMA user_version", [], |row| row.get(0)).unwrap();
    assert_eq!(version, 1);
}

#[test]
fn chat_preference_conflicts_and_invalid_targets_do_not_partially_write() {
    let (_dir, accounts) = fixture();
    for account in ["a", "b", "c"] { add(&accounts, account); }
    let first = binding_key("thread", "first");
    let second = binding_key("session", "second");
    accounts.store.bind(&first, "a").unwrap();
    accounts.store.bind(&second, "b").unwrap();
    assert!(accounts.store.switch_chat_bindings(&[first.clone(), second.clone()], "a", "c").is_err());
    assert_eq!(accounts.store.binding(&first).unwrap().as_deref(), Some("a"));
    assert_eq!(accounts.store.binding(&second).unwrap().as_deref(), Some("b"));
    assert!(accounts.store.switch_chat_bindings(std::slice::from_ref(&first), "a", "missing").is_err());
    assert_eq!(accounts.store.binding(&first).unwrap().as_deref(), Some("a"));
    assert!(accounts.store.get(&format!("portable-chat/v1/{first}")).unwrap().is_none());
    let missing = binding_key("thread", "missing");
    assert!(accounts.store.switch_chat_bindings(&[first.clone(), missing], "a", "c").is_err());
    assert_eq!(accounts.store.binding(&first).unwrap().as_deref(), Some("a"));
    assert!(accounts.store.switch_chat_bindings(&[], "a", "b").is_err());
    assert!(accounts.store.switch_chat_bindings(&["thread:raw-id".into()], "a", "b").is_err());
}

#[test]
fn chat_preference_cannot_revive_expired_or_retired_chats() {
    for retired in [false, true] {
        let (_dir, accounts) = fixture();
        add(&accounts, "a"); add(&accounts, "b");
        let key = binding_key("thread", "chat");
        accounts.store.bind(&key, "a").unwrap();
        if retired { accounts.store.delete_account("a").unwrap(); }
        else {
            accounts.store.db.lock().unwrap().execute(
                "UPDATE bindings SET expires=0 WHERE key=?1", [&key],
            ).unwrap();
        }
        assert!(accounts.store.switch_chat_bindings(std::slice::from_ref(&key), "a", "b").is_err());
        assert!(accounts.store.binding(&key).is_err());
    }
}

#[test]
fn chat_preference_concurrent_switches_have_one_winner() {
    let (_dir, accounts) = fixture();
    for account in ["a", "b", "c"] { add(&accounts, account); }
    let key = binding_key("thread", "chat");
    accounts.store.bind(&key, "a").unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let handles: Vec<_> = ["b", "c"].into_iter().map(|target| {
        let store = accounts.store.clone(); let key = key.clone(); let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait(); store.switch_chat_bindings(&[key], "a", target).map(|_| target)
        })
    }).collect();
    barrier.wait();
    let results: Vec<_> = handles.into_iter().map(|handle| handle.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    let winner = results.into_iter().find_map(Result::ok).unwrap();
    assert_eq!(accounts.store.binding(&key).unwrap().as_deref(), Some(winner));
}
