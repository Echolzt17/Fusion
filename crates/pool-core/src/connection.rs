//! Serial mutations, short status locks, and a shared display-only inspection cache.
use crate::{config, gateway::Gateway, Result};
use serde::Serialize;
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;
#[cfg(test)]
mod tests;
#[derive(Clone, Default)]
struct Transition {
    enabling: bool,
    closing: bool,
    error: Option<String>,
    revision: u64,
    operation_name: Option<&'static str>,
    phase: Option<&'static str>,
    operation_started_at: Option<i64>,
    close_reload: bool,
    close_was_running: bool,
}
#[derive(Clone)]
struct Inspection {
    configured: bool,
    pending: bool,
    transaction: bool,
    safe: bool,
    origin_ok: bool,
    reload: Option<String>,
    error: Option<String>,
}
pub struct Connection {
    gateway: Arc<Gateway>,
    path: PathBuf,
    exe: PathBuf,
    operation: Arc<Mutex<()>>,
    transition: Arc<std::sync::Mutex<Transition>>,
    inspection: Mutex<Option<(Instant, u64, Inspection)>>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub mode: &'static str,
    pub configured: bool,
    pub recovery_pending: bool,
    pub reload_required: Option<String>,
    pub error: Option<String>,
    pub operation: Option<&'static str>,
    pub phase: Option<&'static str>,
    pub operation_started_at: Option<i64>,
}
struct Enabling<'a>(&'a Connection);
struct EnableCleanup {
    gateway: Arc<Gateway>,
    path: PathBuf,
    operation: Option<tokio::sync::OwnedMutexGuard<()>>,
    transition: Arc<std::sync::Mutex<Transition>>,
    started: bool,
}
impl Drop for EnableCleanup {
    fn drop(&mut self) {
        if !self.started { return; }
        let (gateway, path, operation, transition) = (self.gateway.clone(), self.path.clone(), self.operation.take(), self.transition.clone());
        tokio::spawn(async move {
            let _operation = operation;
            let store = gateway.accounts.store.clone();
            if !blocking(move || config::is_applied(&store, &path)).await.unwrap_or(true) {
                gateway.stop().await;
            }
            let mut t = transition.lock().unwrap();
            t.phase = Some("failed");
            t.error = Some("启动检查已取消，未自动重放；请重试启动".into());
            t.revision = t.revision.wrapping_add(1);
        });
    }
}
impl Drop for Enabling<'_> {
    fn drop(&mut self) {
        self.0.update(|t| t.enabling = false);
    }
}
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| "接入文件任务异常")?
}
impl Connection {
    pub fn new(gateway: Arc<Gateway>, path: PathBuf, exe: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            gateway,
            path,
            exe,
            operation: Arc::new(Mutex::new(())),
            transition: Arc::new(std::sync::Mutex::new(Transition::default())),
            inspection: Mutex::new(None),
        })
    }
    fn update(&self, change: impl FnOnce(&mut Transition)) {
        let mut t = self.transition.lock().unwrap();
        change(&mut t);
        t.revision = t.revision.wrapping_add(1);
    }
    async fn inspect(&self) -> Result<Inspection> {
        let revision = self.transition.lock().unwrap().revision;
        let mut cache = self.inspection.lock().await;
        if let Some((_, _, value)) = cache
            .as_ref()
            .filter(|(at, r, _)| *r == revision && at.elapsed() < Duration::from_secs(3))
        {
            return Ok(value.clone());
        }
        let (store, path, exe) = (
            self.gateway.accounts.store.clone(),
            self.path.clone(),
            self.exe.clone(),
        );
        let value = blocking(move || {
            let (safe, error) =
                match config::detached(&path, &exe, store.settings().map_or(18765, |s| s.port)) {
                    Ok(safe) => (safe, None),
                    Err(e) => (false, Some(e)),
                };
            Ok(Inspection {
                pending: config::has_backup(&store).unwrap_or(true),
                transaction: config::recovery_transaction_pending(&path),
                configured: config::is_applied(&store, &path).unwrap_or(false),
                safe,
                error,
                origin_ok: config::require_takeover_ready(&store, &path, &exe).is_ok(),
                reload: store
                    .get("codex_client_reload_required")?
                    .filter(|s| !s.is_empty()),
            })
        })
        .await?;
        *cache = Some((Instant::now(), revision, value.clone()));
        Ok(value)
    }
    pub async fn status(&self) -> Result<Status> {
        let i = self.inspect().await?;
        let service = self.gateway.status().await;
        let t = self.transition.lock().unwrap().clone();
        let mode = if t.enabling || (t.operation_name == Some("start") && t.phase != Some("failed")) {
            "enabling"
        } else if t.closing {
            "closing"
        } else if i.transaction || (t.operation_name == Some("start") && t.phase == Some("failed") && i.pending) {
            "recovery_required"
        } else if i.configured && service.running && !service.draining && i.origin_ok && !i.safe {
            "on"
        } else if !service.running && i.safe && !i.pending {
            "off"
        } else {
            "recovery_required"
        };
        Ok(Status {
            mode,
            configured: i.configured,
            recovery_pending: i.transaction || i.pending,
            reload_required: i.reload,
            error: t.error.or(i.error).or_else(|| {
                i.transaction
                    .then(|| "救援事务尚未完成，请继续恢复；原配置保留文件不会被删除".into())
            }),
            operation: t.operation_name,
            phase: t.phase,
            operation_started_at: t.operation_started_at,
        })
    }
    pub fn starting(&self) -> bool {
        let t = self.transition.lock().unwrap();
        t.operation_name == Some("start") && t.phase != Some("failed")
    }
    pub fn begin_start(&self) -> Result<bool> {
        let mut t = self.transition.lock().unwrap();
        if t.closing { return Err("账号池正在关闭，请等待完成".into()); }
        if t.operation_name == Some("start") && t.phase != Some("failed") { return Ok(false); }
        t.operation_name = Some("start");
        t.phase = Some("checking");
        t.operation_started_at = Some(crate::now());
        t.error = None;
        t.revision = t.revision.wrapping_add(1);
        Ok(true)
    }
    pub fn set_phase(&self, phase: &'static str) { self.update(|t| t.phase = Some(phase)); }
    pub async fn prepare_enable(&self) -> Result<String> {
        let preview = self.enable_preview().await?;
        let accounts = self.gateway.accounts.clone();
        let scheduler = self.gateway.scheduler.clone();
        if !blocking(move || Ok(accounts.store.accounts()?.iter().any(|a| scheduler.available(a)))).await? {
            return Err("没有可用账号，未修改 Codex 接入".into());
        }
        Ok(preview.fingerprint)
    }
    pub async fn wait_for_restart(&self) -> Result<()> {
        self.set_phase("draining");
        self.gateway.drain().await;
        self.wait_idle().await;
        Ok(())
    }
    async fn wait_idle(&self) {
        loop {
            let service = self.gateway.status().await;
            if service.active_requests == 0 && !service.running { break; }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    pub async fn resume_gateway(&self) -> Result<()> {
        if !self.gateway.status().await.running { self.gateway.start().await?; }
        Ok(())
    }
    pub async fn start_failed(&self, error: String) {
        let (store, path) = (self.gateway.accounts.store.clone(), self.path.clone());
        if blocking(move || config::is_applied(&store, &path)).await.unwrap_or(false) {
            let _ = self.resume_gateway().await;
        }
        self.update(|t| { t.enabling = false; t.operation_name = Some("start"); t.phase = Some("failed"); t.error = Some(error); });
    }
    async fn preview_unlocked(&self) -> Result<config::Preview> {
        let (store, path, exe) = (
            self.gateway.accounts.store.clone(),
            self.path.clone(),
            self.exe.clone(),
        );
        let accounts = self.gateway.accounts.clone();
        blocking(move || config::preview(&store, &path, &exe, &accounts.api_key()?)).await
    }
    pub async fn enable_preview(&self) -> Result<config::Preview> {
        let _operation = self.operation.lock().await;
        if self.transition.lock().unwrap().closing {
            return Err("账号池正在关闭，请等待完成".into());
        }
        self.preview_unlocked().await
    }
    pub async fn enable(&self, fingerprint: &str, client_version: &str) -> Result<()> {
        let operation = self.operation.clone().lock_owned().await;
        let mut cleanup = EnableCleanup { gateway: self.gateway.clone(), path: self.path.clone(), operation: Some(operation), transition: self.transition.clone(), started: false };
        if self.transition.lock().unwrap().closing {
            return Err("账号池正在关闭，请等待完成".into());
        }
        self.update(|t| {
            t.enabling = true;
            t.error = None;
        });
        let _enabling = Enabling(self);
        let mut started = false;
        let result = async {
            if self.preview_unlocked().await?.fingerprint != fingerprint {
                return Err("配置在启动期间已变化，请重试启动".into());
            }
            let accounts = self.gateway.accounts.clone();
            let scheduler = self.gateway.scheduler.clone();
            if !blocking(move || {
                Ok(accounts
                    .store
                    .accounts()?
                    .iter()
                    .any(|a| scheduler.available(a)))
            })
            .await?
            {
                return Err("没有可用账号，未修改 Codex 接入".into());
            }
            let service = self.gateway.status().await;
            if service.draining {
                return Err("服务正在等待请求完成".into());
            }
            started = !service.running;
            if started {
                self.gateway.start().await?;
                cleanup.started = true;
            }
            let transition = self.transition.clone();
            self.gateway.check_models(client_version, Arc::new(move |phase| {
                let mut t = transition.lock().unwrap();
                t.phase = Some(phase);
                t.revision = t.revision.wrapping_add(1);
            })).await?;
            self.set_phase("configuring");
            let (store, path, exe, fingerprint) = (
                self.gateway.accounts.store.clone(),
                self.path.clone(),
                self.exe.clone(),
                fingerprint.to_owned(),
            );
            // The display cache never authorizes a write. Revalidate the complete internally prepared fingerprint.
            let accounts = self.gateway.accounts.clone();
            blocking(move || {
                config::apply(&store, &path, &exe, &accounts.api_key()?, &fingerprint)?;
                store.set("codex_client_reload_required", "pool")
            })
            .await
        }
        .await;
        if result.is_err() && started {
            let (store, path) = (self.gateway.accounts.store.clone(), self.path.clone());
            if !blocking(move || config::is_applied(&store, &path))
                .await
                .unwrap_or(false)
            {
                self.gateway.stop().await;
            }
        }
        cleanup.started = false;
        self.update(|t| t.error = result.as_ref().err().cloned());
        result
    }
    pub async fn recovery_preview(
        &self,
        strategy: Option<config::RecoveryStrategy>,
    ) -> Result<config::RecoveryPlan> {
        let (store, path, exe) = (
            self.gateway.accounts.store.clone(),
            self.path.clone(),
            self.exe.clone(),
        );
        blocking(move || {
            Ok(config::recovery_preview(
                Some(&store),
                &path,
                &exe,
                strategy,
            ))
        })
        .await
    }
    pub async fn prepare_close(&self) -> Result<bool> {
        let _operation = self.operation.lock().await;
        {
            let t = self.transition.lock().unwrap();
            if t.closing {
                return Ok(t.close_reload);
            }
        }
        let was_running = self.gateway.status().await.running;
        self.update(|t| {
            t.closing = true;
            t.operation_name = Some("stop");
            t.phase = Some("restoring");
            t.operation_started_at = Some(crate::now());
            t.error = None;
            t.close_was_running = was_running;
        });
        let (store, path, exe) = (
            self.gateway.accounts.store.clone(),
            self.path.clone(),
            self.exe.clone(),
        );
        let result = blocking(move || config::restore_for_close(&store, &path, &exe)).await;
        match result {
            Ok(reload) => {
                self.update(|t| {
                    t.close_reload = reload;
                    t.phase = Some("draining");
                });
                self.gateway.drain().await;
                Ok(reload)
            }
            Err(error) => {
                self.update(|t| {
                    t.closing = false;
                    t.phase = Some("failed");
                    t.error = Some(error.clone());
                });
                Err(error)
            }
        }
    }
    pub async fn wait_drained(&self) -> Result<()> {
        self.wait_idle().await;
        self.verify_closed().await?;
        self.update(|t| t.phase = Some("quitting"));
        Ok(())
    }
    async fn verify_closed(&self) -> Result<()> {
        let (store, path, exe) = (
            self.gateway.accounts.store.clone(),
            self.path.clone(),
            self.exe.clone(),
        );
        blocking(move || config::verify_closed(&store, &path, &exe)).await
    }
    pub async fn complete_close(&self) -> Result<()> {
        let _operation = self.operation.lock().await;
        self.verify_closed().await?;
        let store = self.gateway.accounts.store.clone();
        blocking(move || store.finish_client_switch(true)).await?;
        self.gateway.stop().await;
        self.update(|t| {
            t.closing = false;
            t.operation_name = None;
            t.phase = None;
            t.operation_started_at = None;
            t.error = None;
        });
        Ok(())
    }
    pub async fn close_failed(&self, error: String) {
        let _operation = self.operation.lock().await;
        // Keep the old client's gateway usable after a blocked desktop exit. Never replay work.
        let was_running = self.transition.lock().unwrap().close_was_running;
        if was_running && !self.gateway.status().await.running {
            let _ = self.gateway.start().await;
        }
        self.update(|t| {
            t.closing = false;
            t.phase = Some("failed");
            t.error = Some(error);
        });
    }
    pub async fn client_reloaded(&self) -> Result<()> {
        let (store, path, exe) = (self.gateway.accounts.store.clone(), self.path.clone(), self.exe.clone());
        blocking(move || {
            if !config::is_applied(&store, &path)? || config::detached(&path, &exe, store.settings()?.port)? {
                return Err("客户端重开期间接入发生变化，启动尚未完成，请重试".into());
            }
            store.finish_client_switch(false)
        }).await?;
        self.update(|t| { t.enabling = false; t.operation_name = None; t.phase = None; t.operation_started_at = None; t.error = None; });
        Ok(())
    }
    pub async fn save_service_settings(&self, port: u16, concurrency: usize) -> Result<()> {
        let _operation = self.operation.lock().await;
        if self.transition.lock().unwrap().closing || self.gateway.status().await.running {
            return Err("请先关闭账号池并恢复原接入，再修改服务设置".into());
        }
        let (store, path, exe, scheduler) = (
            self.gateway.accounts.store.clone(),
            self.path.clone(),
            self.exe.clone(),
            self.gateway.scheduler.clone(),
        );
        let result = blocking(move || {
            if config::has_backup(&store)?
                || config::recovery_transaction_pending(&path)
                || !config::detached(&path, &exe, store.settings()?.port)?
            {
                return Err("请先关闭账号池并恢复原接入，再修改服务设置".into());
            }
            scheduler.save_service_settings(port, concurrency)
        })
        .await;
        self.update(|_| {});
        result
    }
}
