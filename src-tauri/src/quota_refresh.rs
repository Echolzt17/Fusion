use pool_core::account::Accounts;
use std::{sync::Arc, time::Duration};

pub fn start(accounts: Arc<Accounts>, updates: Arc<crate::updates::Updates>) {
    tauri::async_runtime::spawn(async move {
        // Only scans local due times. Upstream requests remain 10 minutes apart;
        // the first tick is immediate and new accounts are discovered within 15s.
        let mut timer = tokio::time::interval(Duration::from_secs(15));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            let Ok(_work) = updates.enter_work() else { continue; };
            let _ = accounts.refresh_due_quotas().await;
        }
    });
}
