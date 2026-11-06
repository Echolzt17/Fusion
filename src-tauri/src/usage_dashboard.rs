use pool_core::{
    local_usage::{self, Dashboard},
    storage::Store,
};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Default)]
pub struct UsageDashboard(Mutex<Dashboard>);
impl UsageDashboard {
    pub fn snapshot(&self) -> pool_core::Result<Dashboard> {
        self.0
            .lock()
            .map(|v| v.clone())
            .map_err(|_| "读取本机用量失败".into())
    }
    pub fn start(
        self: Arc<Self>,
        store: Arc<Store>,
        root: PathBuf,
        updates: Arc<crate::updates::Updates>,
    ) {
        tauri::async_runtime::spawn(async move {
            let mut timer = tokio::time::interval(Duration::from_secs(15));
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                timer.tick().await;
                let Ok(_work) = updates.enter_work() else {
                    continue;
                };
                let store = store.clone();
                let root = root.clone();
                let result = tauri::async_runtime::spawn_blocking(move || {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as i64;
                    local_usage::scan(&store, &root, now)
                })
                .await;
                if let Ok(mut snapshot) = self.0.lock() {
                    match result {
                        Ok(Ok(value)) => *snapshot = value,
                        _ => {
                            snapshot.scanning = false;
                            snapshot.error = Some("本机用量读取失败，已保留上次统计".into());
                        }
                    }
                }
            }
        });
    }
}
