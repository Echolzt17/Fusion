use super::*;
use crate::{gateway::Gateway, tests::add, vault::MemoryVault};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Default)]
struct FailingVault { inner: MemoryVault, fail: AtomicBool }
impl Vault for FailingVault {
    fn get(&self, key: &str) -> Result<Option<String>> { self.inner.get(key) }
    fn set(&self, key: &str, value: &str) -> Result<()> { self.inner.set(key, value) }
    fn delete(&self, key: &str) -> Result<()> {
        if self.fail.load(Ordering::SeqCst) { Err("fixture vault unavailable".into()) }
        else { self.inner.delete(key) }
    }
}
#[tokio::test]
async fn deletion_database_failures_preserve_credentials_or_durable_cleanup_state() {
    let (d, a) = crate::tests::fixture(); add(&a, "a");
    let db = rusqlite::Connection::open(d.path().join("pool.sqlite")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_update BEFORE UPDATE ON accounts BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    let g = Gateway::new(a.clone());
    assert!(g.remove_account("a").await.is_err());
    assert_eq!(a.store.account("a").unwrap().state, "ready");
    assert!(a.vault.get("account/a").unwrap().is_some());
    db.execute_batch("DROP TRIGGER fail_update; CREATE TRIGGER fail_delete BEFORE DELETE ON accounts BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(g.remove_account("a").await.is_err());
    assert_eq!(a.store.account("a").unwrap().state, "deleting");
    assert!(!g.scheduler.available(&a.store.account("a").unwrap()));
    assert!(a.token("a").await.is_err());
    assert!(a.set_state("a", false).await.is_err());
    db.execute_batch("DROP TRIGGER fail_delete;").unwrap();
    g.remove_account("a").await.unwrap();
    assert!(a.store.accounts().unwrap().is_empty());
}
#[tokio::test]
async fn deletion_resumes_after_vault_failure_and_restart_on_either_side_of_vault_cleanup() {
    for credentials_already_removed in [false, true] {
        let d = tempfile::tempdir().unwrap(); let path = d.path().join("pool.sqlite");
        let vault = Arc::new(FailingVault::default());
        {
            let a = Arc::new(Accounts::new(Arc::new(Store::open(&path).unwrap()), vault.clone()).unwrap()); add(&a, "a");
            a.store.bind("thread:fixture", "a").unwrap();
            vault.fail.store(true, Ordering::SeqCst);
            assert!(Gateway::new(a.clone()).remove_account("a").await.is_err());
            assert_eq!(a.store.account("a").unwrap().state, "deleting");
            assert!(vault.get("account/a").unwrap().is_some());
        }
        vault.fail.store(false, Ordering::SeqCst);
        if credentials_already_removed { vault.delete("account/a").unwrap(); }
        let a = Arc::new(Accounts::new(Arc::new(Store::open(&path).unwrap()), vault.clone()).unwrap());
        let g = Gateway::new(a.clone());
        assert!(!g.scheduler.available(&a.store.account("a").unwrap()));
        g.remove_account("a").await.unwrap();
        assert!(vault.get("account/a").unwrap().is_none());
        add(&a, "a");
        assert!(a.store.bind("thread:fixture", "a").is_err());
    }
}
