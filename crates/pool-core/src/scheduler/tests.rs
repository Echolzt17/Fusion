use super::*;
use crate::tests::{add, fixture};
use serde_json::json;

fn cooling(store: &Store, id: &str, seconds: i64) {
    let mut account = store.account(id).unwrap();
    account.cooldown_until = now() + seconds;
    store.save_account(&account).unwrap();
}

#[tokio::test]
async fn chat_preference_uses_backup_only_after_the_original_is_unavailable() {
    let (_dir, accounts) = fixture(); add(&accounts, "a"); add(&accounts, "b");
    let scheduler = Scheduler::new(accounts.store.clone());
    scheduler.select_account(Some("b".into())).unwrap();
    let original = scheduler.acquire_portable_observed(Some("a"), false, |_| {}).await.unwrap();
    assert_eq!(original.account.id, "a");
    drop(original);
    cooling(&accounts.store, "a", 120);
    let backup = scheduler.acquire_portable_observed(Some("a"), false, |_| {}).await.unwrap();
    assert_eq!(backup.account.id, "b");
    assert!(scheduler.acquire(Some("a")).await.is_err());
    drop(backup);
    cooling(&accounts.store, "a", 0);
    let mut exhausted = accounts.store.account("a").unwrap();
    exhausted.quota = Some(json!({"updatedAt":now(),"rateLimit":{"allowed":false,
        "primary_window":{"used_percent":100,"reset_at":now()+3600}}}));
    accounts.store.save_account(&exhausted).unwrap();
    assert_eq!(scheduler.acquire_portable_observed(Some("a"), false, |_| {}).await.unwrap().account.id, "b");
    assert!(scheduler.acquire(Some("a")).await.is_err());
}

#[tokio::test]
async fn chat_preference_waits_for_busy_original_and_rechecks_its_health() {
    let (_dir, accounts) = fixture(); add(&accounts, "a"); add(&accounts, "b");
    let scheduler = Scheduler::new(accounts.store.clone());
    scheduler.save_service_settings(18765, 1).unwrap();
    let lease = scheduler.acquire(Some("a")).await.unwrap();
    let s = scheduler.clone();
    let pending = tokio::spawn(async move {
        s.acquire_portable_observed(Some("a"), false, |_| {}).await
    });
    tokio::time::sleep(PREFERENCE_WAIT + Duration::from_millis(100)).await;
    assert!(!pending.is_finished());
    assert!(!scheduler.is_active("b"));
    drop(lease);
    let original = tokio::time::timeout(Duration::from_secs(2), pending).await.unwrap().unwrap().unwrap();
    assert_eq!(original.account.id, "a");
    drop(original);

    let lease = scheduler.acquire(Some("a")).await.unwrap();
    let s = scheduler.clone();
    let pending = tokio::spawn(async move {
        s.acquire_portable_observed(Some("a"), false, |_| {}).await
    });
    cooling(&accounts.store, "a", 120);
    let backup = tokio::time::timeout(Duration::from_secs(2), pending).await.unwrap().unwrap().unwrap();
    assert_eq!(backup.account.id, "b");
    drop(lease); drop(backup);
}

#[tokio::test]
async fn short_cooldown_waits_without_a_lease_and_never_moves_a_bound_request() {
    let (_d, accounts) = fixture();
    add(&accounts, "a"); add(&accounts, "b");
    let scheduler = Scheduler::new(accounts.store.clone());
    scheduler.select_account(Some("a".into())).unwrap();
    cooling(&accounts.store, "a", 10);
    let phases = Arc::new(Mutex::new(Vec::new()));
    let observed = phases.clone();
    let s = scheduler.clone();
    let pending = tokio::spawn(async move {
        s.acquire_observed(Some("a"), true, move |phase| observed.lock().unwrap().push(phase)).await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while phases.lock().unwrap().is_empty() { tokio::task::yield_now().await; }
    }).await.unwrap();
    assert_eq!(*phases.lock().unwrap(), vec![QueuePhase::Cooldown]);
    assert!(!pending.is_finished());
    assert!(!scheduler.is_active("a") && !scheduler.is_active("b"));
    // New, unbound work can use the healthy backup immediately.
    assert_eq!(scheduler.acquire(None).await.unwrap().account.id, "b");
    cooling(&accounts.store, "a", 0);
    let lease = tokio::time::timeout(Duration::from_secs(2), pending).await.unwrap().unwrap().unwrap();
    assert_eq!(lease.account.id, "a");
    assert_eq!(scheduler.status().unwrap().current_account_id.as_deref(), Some("b"));
    drop(lease);
    assert!(!scheduler.is_active("a") && !scheduler.is_active("b"));
}

#[tokio::test]
async fn unbound_work_waits_only_when_no_eligible_account_exists() {
    let (_d, accounts) = fixture(); add(&accounts, "a");
    let scheduler = Scheduler::new(accounts.store.clone());
    cooling(&accounts.store, "a", 10);
    let s = scheduler.clone();
    let pending = tokio::spawn(async move { s.acquire(None).await });
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(!pending.is_finished() && !scheduler.is_active("a"));
    cooling(&accounts.store, "a", 0);
    assert_eq!(tokio::time::timeout(Duration::from_secs(2), pending).await.unwrap().unwrap().unwrap().account.id, "a");
}

#[tokio::test]
async fn cooldown_wait_is_bounded_by_the_original_queue_deadline_and_catalog_never_waits() {
    let (_d, accounts) = fixture(); add(&accounts, "a");
    let scheduler = Scheduler::new(accounts.store.clone());
    cooling(&accounts.store, "a", 10);
    let phases = Mutex::new(Vec::new());
    let started = tokio::time::Instant::now();
    let result = scheduler.acquire_kind_observed(Some("a"), Duration::from_millis(40), false, true,
        |phase| phases.lock().unwrap().push(phase)).await;
    assert!(result.err().unwrap().contains("冷却等待超时"));
    assert!(started.elapsed() >= Duration::from_millis(40) && started.elapsed() < Duration::from_secs(2));
    assert_eq!(*phases.lock().unwrap(), vec![QueuePhase::Cooldown]);
    assert!(!scheduler.is_active("a"));
    let result = tokio::time::timeout(Duration::from_millis(200), scheduler.acquire_catalog(Some("a"))).await;
    assert!(result.unwrap().is_err());
    cooling(&accounts.store, "a", 120);
    assert!(tokio::time::timeout(Duration::from_millis(200), scheduler.acquire(Some("a"))).await.unwrap().is_err());
}

#[tokio::test]
async fn cooldown_wait_rechecks_unavailable_states_and_is_drop_safe() {
    for state in ["paused", "reauth", "deleting", "removed", "quota", "long_cooldown", "cancelled"] {
        let (_d, accounts) = fixture(); add(&accounts, "a"); add(&accounts, "b");
        let scheduler = Scheduler::new(accounts.store.clone());
        cooling(&accounts.store, "a", 10);
        let seen = Arc::new(tokio::sync::Notify::new());
        let observed = seen.clone();
        let waiting = seen.notified();
        let s = scheduler.clone();
        let pending = tokio::spawn(async move {
            s.acquire_observed(Some("a"), false, move |_| observed.notify_one()).await
        });
        tokio::time::timeout(Duration::from_secs(2), waiting).await.unwrap();
        match state {
            "cancelled" => pending.abort(),
            "removed" => accounts.store.delete_account("a").unwrap(),
            "long_cooldown" => cooling(&accounts.store, "a", 120),
            "quota" => {
                quota(&accounts.store, "a", "pro", 0., 0.);
                let mut a = accounts.store.account("a").unwrap();
                a.quota.as_mut().unwrap()["rateLimit"]["allowed"] = json!(false);
                accounts.store.save_account(&a).unwrap();
            }
            _ => {
                let mut a = accounts.store.account("a").unwrap();
                a.state = state.into();
                accounts.store.save_account(&a).unwrap();
            }
        }
        let result = tokio::time::timeout(Duration::from_secs(2), pending).await.unwrap();
        if state == "cancelled" { assert!(matches!(result, Err(error) if error.is_cancelled())); }
        else { assert!(result.unwrap().is_err(), "{state} must not move the bound request to b"); }
        assert!(!scheduler.is_active("a") && !scheduler.is_active("b"));
    }
}

#[tokio::test]
async fn image_limits_share_account_slots_release_on_drop_and_never_move_bound_work() {
    let (_d, a) = fixture();
    for id in ["a", "b", "c"] {
        add(&a, id);
    }
    let s = Scheduler::new(a.store.clone());
    s.save_service_settings(18765, 2).unwrap();
    let wait = Duration::from_millis(1);
    let first = s.acquire_kind(Some("a"), wait, true).await.unwrap();
    assert!(s.acquire_kind(Some("a"), wait, true).await.is_err());
    let text = s.acquire(Some("a")).await.unwrap();
    assert!(s.acquire_kind(Some("a"), wait, false).await.is_err());
    let second = s.acquire_kind(Some("b"), wait, true).await.unwrap();
    assert!(s.acquire_kind(Some("c"), wait, true).await.is_err());
    drop(first);
    drop(text);
    let third = s.acquire_kind(Some("c"), wait, true).await.unwrap();
    assert!(s.is_active("b") && s.is_active("c"));
    drop(second);
    drop(third);
    assert!(!s.is_active("b") && !s.is_active("c"));
}

fn quota(store: &Store, id: &str, plan: &str, short: f64, long: f64) {
    let mut a = store.account(id).unwrap();
    a.quota = Some(json!({"planType":plan,"updatedAt":now(),"rateLimit":{
        "primary_window":{"used_percent":100.0-short,"reset_at":now()+3600},
        "secondary_window":{"used_percent":100.0-long,"reset_at":now()+86400}}}));
    store.save_account(&a).unwrap();
}
#[tokio::test]
async fn concurrent_first_requests_agree_and_policy_changes_preserve_existing_leases() {
    let (_d, a) = fixture();
    add(&a, "a");
    add(&a, "b");
    quota(&a.store, "a", "prolite", 90.0, 90.0);
    quota(&a.store, "b", "promax", 50.0, 50.0);
    let s = Scheduler::new(a.store.clone());
    let (first, second) = tokio::join!(s.acquire(None), s.acquire(None));
    let first = first.unwrap();
    assert_eq!(second.unwrap().account.id, "a");
    s.set_policy(SelectionPolicy::SubscriptionDesc).unwrap();
    assert_eq!(first.account.id, "a");
    assert_eq!(s.acquire(None).await.unwrap().account.id, "b");
    assert_eq!(s.acquire(Some("a")).await.unwrap().account.id, "a");
}
#[test]
fn elapsed_exhausted_window_does_not_wait_for_non_exhausted_weekly_reset() {
    let (_d, a) = fixture();
    add(&a, "a");
    quota(&a.store, "a", "pro", 0.0, 80.0);
    let mut account = a.store.account("a").unwrap();
    let q = account.quota.as_mut().unwrap();
    q["rateLimit"]["allowed"] = json!(false);
    q["rateLimit"]["primary_window"]["reset_at"] = json!(now() - 1);
    assert!(!quota_blocked(&account));
    assert_eq!(remaining(&account), Some(80.0));
}
#[tokio::test]
async fn five_policies_use_official_tiers_and_bottleneck_quota() {
    let (_d, a) = fixture();
    for id in ["a", "b", "c", "d"] {
        add(&a, id);
    }
    quota(&a.store, "a", "prolite", 80.0, 20.0);
    quota(&a.store, "b", "pro", 40.0, 50.0);
    quota(&a.store, "c", "promax", 10.0, 90.0);
    quota(&a.store, "d", "enterprise", 95.0, 95.0);
    let s = Scheduler::new(a.store.clone());
    for (policy, id) in [
        (SelectionPolicy::Auto, "d"),
        (SelectionPolicy::QuotaDesc, "d"),
        (SelectionPolicy::QuotaAsc, "c"),
        (SelectionPolicy::SubscriptionDesc, "c"),
        (SelectionPolicy::SubscriptionAsc, "a"),
    ] {
        s.set_policy(policy).unwrap();
        assert_eq!(s.acquire(None).await.unwrap().account.id, id);
    }
    assert_eq!(
        subscription::label(&a.store.account("a").unwrap()),
        "Pro 100"
    );
    assert_eq!(
        subscription::label(&a.store.account("b").unwrap()),
        "Pro 200"
    );
    assert_eq!(
        subscription::label(&a.store.account("c").unwrap()),
        "Pro 500"
    );
    assert_eq!(
        subscription::label(&a.store.account("d").unwrap()),
        "Enterprise"
    );
    assert_eq!(subscription::rank(&a.store.account("d").unwrap()), None);
}
#[tokio::test]
async fn selection_survives_refresh_restart_and_service_settings_save() {
    let (d, a) = fixture();
    add(&a, "a");
    add(&a, "b");
    quota(&a.store, "a", "pro", 60.0, 60.0);
    quota(&a.store, "b", "promax", 20.0, 20.0);
    let s = Scheduler::new(a.store.clone());
    assert_eq!(s.acquire(None).await.unwrap().account.id, "a");
    quota(&a.store, "b", "promax", 99.0, 99.0);
    assert_eq!(s.acquire(None).await.unwrap().account.id, "a");
    let reopened = Scheduler::new(Arc::new(
        Store::open(&d.path().join("pool.sqlite")).unwrap(),
    ));
    assert_eq!(reopened.acquire(None).await.unwrap().account.id, "a");
    reopened
        .set_policy(SelectionPolicy::SubscriptionDesc)
        .unwrap();
    reopened.save_service_settings(19000, 3).unwrap();
    assert_eq!(
        a.store.settings().unwrap().selection_policy,
        SelectionPolicy::SubscriptionDesc
    );
    assert_eq!(reopened.acquire(None).await.unwrap().account.id, "b");
}
#[tokio::test]
async fn manual_preference_falls_back_and_recovery_does_not_reclaim() {
    let (_d, a) = fixture();
    add(&a, "a");
    add(&a, "b");
    let s = Scheduler::new(a.store.clone());
    s.select_account(Some("b".into())).unwrap();
    a.store.bind("session:old", "b").unwrap();
    a.set_state("b", true).await.unwrap();
    assert!(s.acquire(Some("b")).await.is_err());
    assert_eq!(s.acquire(None).await.unwrap().account.id, "a");
    a.set_state("b", false).await.unwrap();
    assert_eq!(s.acquire(None).await.unwrap().account.id, "a");
    assert_eq!(s.acquire(Some("b")).await.unwrap().account.id, "b");
    assert_eq!(
        a.store.binding("session:old").unwrap().as_deref(),
        Some("b")
    );
    assert_eq!(
        s.status().unwrap().preferred_account_id.as_deref(),
        Some("b")
    );
    a.remove("a").await.unwrap();
    assert_eq!(s.acquire(None).await.unwrap().account.id, "b");
}
#[tokio::test]
async fn concurrency_waits_then_uses_available_backup_but_never_moves_a_pin() {
    let (_d, a) = fixture();
    add(&a, "a");
    add(&a, "b");
    let s = Scheduler::new(a.store.clone());
    s.save_service_settings(18765, 1).unwrap();
    let first = s.acquire(None).await.unwrap();
    let start = tokio::time::Instant::now();
    let next = s
        .acquire_wait(None, Duration::from_millis(150))
        .await
        .unwrap();
    assert!(start.elapsed() >= Duration::from_millis(150));
    assert_eq!(next.account.id, "b");
    assert!(s.acquire_wait(Some("a"), Duration::ZERO).await.is_err());
    assert!(s.acquire_wait(None, Duration::ZERO).await.is_err());
    drop(first);
    drop(next);
    assert_eq!(s.acquire(None).await.unwrap().account.id, "b");
}
#[tokio::test]
async fn new_requests_use_idle_backup_promptly_but_existing_sessions_keep_their_account() {
    let (_d, a) = fixture();
    add(&a, "a");
    add(&a, "b");
    let s = Scheduler::new(a.store.clone());
    s.save_service_settings(18765, 1).unwrap();
    s.select_account(Some("a".into())).unwrap();
    a.store.bind("session:existing", "a").unwrap();
    let first = s.acquire(Some("a")).await.unwrap();
    let start = tokio::time::Instant::now();
    let next = tokio::time::timeout(Duration::from_secs(2), s.acquire(None)).await.unwrap().unwrap();
    assert!(start.elapsed() >= PREFERENCE_WAIT);
    assert_eq!(next.account.id, "b");
    drop(next);
    assert!(s.acquire_wait(Some("a"), Duration::from_millis(30)).await.is_err());
    assert_eq!(a.store.binding("session:existing").unwrap().as_deref(), Some("a"));
    drop(first);
    assert_eq!(s.acquire(None).await.unwrap().account.id, "b");
    assert_eq!(s.acquire(Some("a")).await.unwrap().account.id, "a");
}

#[tokio::test]
async fn all_busy_accounts_keep_waiting_after_preference_grace_and_use_released_capacity() {
    let (_d, a) = fixture();
    add(&a, "a");
    add(&a, "b");
    let s = Scheduler::new(a.store.clone());
    s.save_service_settings(18765, 1).unwrap();
    let first = s.acquire(Some("a")).await.unwrap();
    let second = s.acquire(Some("b")).await.unwrap();
    let scheduler = s.clone();
    let pending = tokio::spawn(async move { scheduler.acquire(None).await });
    tokio::time::sleep(PREFERENCE_WAIT + Duration::from_millis(150)).await;
    assert!(!pending.is_finished());
    drop(second);
    let next = tokio::time::timeout(Duration::from_secs(2), pending).await.unwrap().unwrap().unwrap();
    assert_eq!(next.account.id, "b");
    drop(first);
}

#[tokio::test]
async fn legacy_fixed_account_is_soft_and_defaults_to_auto() {
    let (_d, a) = fixture();
    add(&a, "a");
    add(&a, "b");
    a.store
        .set(
            "settings",
            r#"{"port":18765,"concurrency":2,"fixedAccount":"b"}"#,
        )
        .unwrap();
    assert_eq!(
        a.store.settings().unwrap().selection_policy,
        SelectionPolicy::Auto
    );
    let s = Scheduler::new(a.store.clone());
    assert_eq!(s.acquire(None).await.unwrap().account.id, "b");
    a.mark_failure("b", 429, 60).await.unwrap();
    assert_eq!(s.acquire(None).await.unwrap().account.id, "a");
}
#[tokio::test]
async fn stale_unknown_failed_and_reset_quota_never_become_zero() {
    let (_d, a) = fixture();
    for id in ["a", "b", "c"] {
        add(&a, id);
    }
    quota(&a.store, "a", "pro", 80.0, 20.0);
    quota(&a.store, "b", "pro", 99.0, 99.0);
    let mut b = a.store.account("b").unwrap();
    b.quota_error = Some("查询失败".into());
    a.store.save_account(&b).unwrap();
    let s = Scheduler::new(a.store.clone());
    s.set_policy(SelectionPolicy::QuotaAsc).unwrap();
    assert_eq!(s.acquire(None).await.unwrap().account.id, "a");
    b.quota_error = None;
    b.quota.as_mut().unwrap()["updatedAt"] = json!(now() - 601);
    assert_eq!(remaining(&b), None);
    let q = b.quota.as_mut().unwrap();
    q["updatedAt"] = json!(now());
    q["rateLimit"]["primary_window"]["used_percent"] = json!(-1);
    q["rateLimit"]["secondary_window"]["reset_at"] = json!(now() - 1);
    assert_eq!(remaining(&b), None);
    let mut current = a.store.account("a").unwrap();
    current.quota_error = Some("查询失败".into());
    a.store.save_account(&current).unwrap();
    assert_eq!(s.acquire(None).await.unwrap().account.id, "a");
}
#[tokio::test]
async fn official_denial_blocks_until_reset_without_permanent_quota_ban() {
    let (_d, a) = fixture();
    add(&a, "a");
    add(&a, "b");
    quota(&a.store, "a", "promax", 0.0, 0.0);
    let mut blocked = a.store.account("a").unwrap();
    blocked.quota.as_mut().unwrap()["rateLimit"]["allowed"] = json!(false);
    a.store.save_account(&blocked).unwrap();
    let s = Scheduler::new(a.store.clone());
    assert_eq!(s.acquire(None).await.unwrap().account.id, "b");
    assert!(s.acquire(Some("a")).await.is_err());
    for k in ["primary_window", "secondary_window"] {
        blocked.quota.as_mut().unwrap()["rateLimit"][k]["reset_at"] = json!(now() - 1);
    }
    a.store.save_account(&blocked).unwrap();
    assert_eq!(s.acquire(Some("a")).await.unwrap().account.id, "a");
    assert_eq!(s.acquire(None).await.unwrap().account.id, "b");
}
