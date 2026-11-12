use crate::{now, storage::Store, subscription, Account, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::Duration,
};

const CURRENT: &str = "scheduling_current_account";
const PREFERENCE_WAIT: Duration = Duration::from_millis(250);
const COOLDOWN_WAIT: Duration = Duration::from_secs(60);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueuePhase {
    Capacity,
    Cooldown,
}
#[derive(Clone, Copy)]
enum Affinity<'a> {
    Pinned(Option<&'a str>),
    Portable(Option<&'a str>),
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionPolicy {
    #[default]
    Auto,
    SubscriptionDesc,
    SubscriptionAsc,
    QuotaDesc,
    QuotaAsc,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SchedulingStatus {
    pub policy: SelectionPolicy,
    pub preferred_account_id: Option<String>,
    pub current_account_id: Option<String>,
}
#[derive(Default)]
struct State {
    removing: HashSet<String>,
    active: HashMap<String, usize>,
    images: HashMap<String, usize>,
}
pub struct Scheduler {
    store: Arc<Store>,
    state: Mutex<State>,
    changed: tokio::sync::Notify,
}
pub struct Lease {
    pub account: Account,
    scheduler: Arc<Scheduler>,
    image: bool,
}
pub struct Removal {
    scheduler: Arc<Scheduler>,
    id: String,
}
impl Drop for Removal {
    fn drop(&mut self) {
        self.scheduler
            .state
            .lock()
            .unwrap()
            .removing
            .remove(&self.id);
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        let mut state = self.scheduler.state.lock().unwrap();
        if let Some(n) = state.active.get_mut(&self.account.id) {
            *n = n.saturating_sub(1);
        }
        if self.image {
            if let Some(n) = state.images.get_mut(&self.account.id) {
                *n = n.saturating_sub(1);
            }
        }
        drop(state);
        self.scheduler.changed.notify_waiters();
    }
}
pub fn binding_key(kind: &str, raw: &str) -> String {
    format!("{kind}:{:x}", Sha256::digest(raw.as_bytes()))
}
fn fresh_quota(a: &Account) -> Option<&serde_json::Value> {
    let q = a.quota.as_ref()?;
    let age = now().checked_sub(q["updatedAt"].as_i64()?)?;
    (a.quota_error.is_none() && (0..=crate::account::QUOTA_REFRESH_INTERVAL_SECS).contains(&age))
        .then_some(q)
}
pub fn remaining(a: &Account) -> Option<f64> {
    let q = fresh_quota(a)?;
    ["primary_window", "secondary_window"]
        .iter()
        .filter_map(|key| {
            let w = &q["rateLimit"][key];
            if w["reset_at"].as_i64().is_some_and(|t| t <= now()) {
                return None;
            }
            w["used_percent"]
                .as_f64()
                .filter(|n| n.is_finite() && (0.0..=100.0).contains(n))
                .map(|n| 100.0 - n)
        })
        .min_by(f64::total_cmp)
}
fn quota_blocked(a: &Account) -> bool {
    let Some(q) = fresh_quota(a) else {
        return false;
    };
    let r = &q["rateLimit"];
    if r["allowed"].as_bool() != Some(false) && r["limit_reached"].as_bool() != Some(true) {
        return false;
    }
    // An elapsed reset makes the old denial inconclusive, never a permanent ban.
    let exhausted: Vec<_> = ["primary_window", "secondary_window"]
        .iter()
        .map(|k| &r[k])
        .filter(|w| w["used_percent"].as_f64().is_some_and(|n| n >= 100.0))
        .collect();
    if !exhausted.is_empty() {
        return exhausted
            .iter()
            .any(|w| w["reset_at"].as_i64().is_none_or(|t| t > now()));
    }
    let resets: Vec<_> = ["primary_window", "secondary_window"]
        .iter()
        .filter_map(|k| r[k]["reset_at"].as_i64())
        .collect();
    resets.is_empty() || resets.iter().all(|t| *t > now())
}
fn eligible(a: &Account, state: &State, _pinned: bool) -> bool {
    !state.removing.contains(&a.id)
        && a.state == "ready"
        && a.cooldown_until <= now()
        && !quota_blocked(a)
}
fn known_cmp<T: Ord>(a: Option<T>, b: Option<T>, descending: bool) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => {
            if descending {
                b.cmp(&a)
            } else {
                a.cmp(&b)
            }
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        _ => Ordering::Equal,
    }
}
fn compare(a: &Account, b: &Account, policy: SelectionPolicy, state: &State) -> Ordering {
    let quota = |descending| match (remaining(a), remaining(b)) {
        (Some(a), Some(b)) => {
            if descending {
                b.total_cmp(&a)
            } else {
                a.total_cmp(&b)
            }
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        _ => Ordering::Equal,
    };
    let tier = |descending| known_cmp(subscription::rank(a), subscription::rank(b), descending);
    let order = match policy {
        SelectionPolicy::Auto | SelectionPolicy::QuotaDesc => quota(true).then_with(|| tier(true)),
        SelectionPolicy::QuotaAsc => quota(false).then_with(|| tier(true)),
        SelectionPolicy::SubscriptionDesc => tier(true).then_with(|| quota(true)),
        SelectionPolicy::SubscriptionAsc => tier(false).then_with(|| quota(true)),
    };
    order
        .then_with(|| {
            state
                .active
                .get(&a.id)
                .copied()
                .unwrap_or(0)
                .cmp(&state.active.get(&b.id).copied().unwrap_or(0))
        })
        .then_with(|| a.id.cmp(&b.id))
}
pub(crate) const REQUEST_QUEUE_WAIT: Duration = Duration::from_secs(600);

impl Scheduler {
    pub fn reserve_removal(self: &Arc<Self>, id: &str) -> Result<Removal> {
        let mut state = self.state.lock().unwrap();
        if state.active.get(id).copied().unwrap_or(0) > 0 || state.removing.contains(id) {
            return Err("该账号仍有请求或删除操作进行中，请等待完成".into());
        }
        state.removing.insert(id.into());
        Ok(Removal {
            scheduler: self.clone(),
            id: id.into(),
        })
    }
    pub fn available(&self, account: &Account) -> bool {
        eligible(account, &self.state.lock().unwrap(), false)
    }
    pub fn new(store: Arc<Store>) -> Arc<Self> {
        Arc::new(Self {
            store,
            state: Mutex::new(State::default()),
            changed: tokio::sync::Notify::new(),
        })
    }
    #[cfg(test)]
    pub fn is_active(&self, id: &str) -> bool {
        self.state
            .lock()
            .unwrap()
            .active
            .get(id)
            .copied()
            .unwrap_or(0)
            > 0
    }
    pub fn status(&self) -> Result<SchedulingStatus> {
        let _state = self.state.lock().unwrap();
        let settings = self.store.settings()?;
        let current = self
            .store
            .get(CURRENT)?
            .or_else(|| settings.fixed_account.clone())
            .filter(|s| !s.is_empty());
        Ok(SchedulingStatus {
            policy: settings.selection_policy,
            preferred_account_id: settings.fixed_account,
            current_account_id: current.filter(|id| self.store.account(id).is_ok()),
        })
    }
    pub fn set_policy(&self, policy: SelectionPolicy) -> Result<()> {
        let state = self.state.lock().unwrap();
        let mut settings = self.store.settings()?;
        settings.selection_policy = policy;
        settings.fixed_account = None;
        let accounts = self.store.accounts()?;
        let selected = accounts
            .iter()
            .filter(|a| eligible(a, &state, false))
            .min_by(|a, b| compare(a, b, policy, &state));
        self.store
            .save_scheduling(&settings, selected.map(|a| a.id.as_str()))
    }
    pub fn select_account(&self, id: Option<String>) -> Result<()> {
        if id.is_none() {
            return self.set_policy(SelectionPolicy::Auto);
        }
        let state = self.state.lock().unwrap();
        let account = self.store.account(id.as_deref().unwrap())?;
        if !eligible(&account, &state, false) {
            return Err("请先恢复账号授权、解除暂停或等待额度和冷却恢复".into());
        }
        let mut settings = self.store.settings()?;
        settings.fixed_account = id;
        self.store.save_scheduling(&settings, Some(&account.id))
    }
    pub fn save_service_settings(&self, port: u16, concurrency: usize) -> Result<()> {
        let _state = self.state.lock().unwrap();
        let mut settings = self.store.settings()?;
        settings.port = port;
        settings.concurrency = concurrency;
        self.store.save_settings(&settings)
    }
    pub async fn acquire(self: &Arc<Self>, pinned: Option<&str>) -> Result<Lease> {
        self.acquire_wait(pinned, REQUEST_QUEUE_WAIT).await
    }
    pub async fn acquire_catalog(self: &Arc<Self>, pinned: Option<&str>) -> Result<Lease> {
        self.acquire_kind_observed(pinned, Duration::from_secs(10), false, false, |_| {})
            .await
    }
    pub async fn acquire_image(self: &Arc<Self>, pinned: Option<&str>) -> Result<Lease> {
        self.acquire_kind(pinned, REQUEST_QUEUE_WAIT, true).await
    }
    pub async fn acquire_observed(
        self: &Arc<Self>,
        pinned: Option<&str>,
        image: bool,
        progress: impl Fn(QueuePhase) + Send + Sync,
    ) -> Result<Lease> {
        self.acquire_kind_observed(pinned, REQUEST_QUEUE_WAIT, image, true, progress)
            .await
    }
    /// The caller has verified a complete portable input. Preserve its chat
    /// preference while healthy, including when busy; choose a backup only
    /// after that account becomes unavailable, never for hard references.
    pub async fn acquire_portable_observed(
        self: &Arc<Self>,
        preferred: Option<&str>,
        image: bool,
        progress: impl Fn(QueuePhase) + Send + Sync,
    ) -> Result<Lease> {
        self.acquire_portable_excluding_observed(
            preferred, &[], tokio::time::Instant::now() + REQUEST_QUEUE_WAIT, image, progress,
        ).await
    }
    /// A failed authorization has not dispatched business input. Exclusions
    /// apply only to this request, without changing other chats' health.
    pub(crate) async fn acquire_portable_excluding_observed(
        self: &Arc<Self>,
        preferred: Option<&str>,
        excluded: &[String],
        deadline: tokio::time::Instant,
        image: bool,
        progress: impl Fn(QueuePhase) + Send + Sync,
    ) -> Result<Lease> {
        let wait = deadline.saturating_duration_since(tokio::time::Instant::now());
        if wait.is_zero() {
            return Err("本机账号并发繁忙，排队超时；本次请求未发送上游，请等待当前任务完成".into());
        }
        self.acquire_affinity_observed(
            Affinity::Portable(preferred), excluded, wait, image, true, progress,
        ).await
    }
    async fn acquire_wait(self: &Arc<Self>, pinned: Option<&str>, wait: Duration) -> Result<Lease> {
        self.acquire_kind(pinned, wait, false).await
    }
    async fn acquire_kind(
        self: &Arc<Self>,
        pinned: Option<&str>,
        wait: Duration,
        image: bool,
    ) -> Result<Lease> {
        self.acquire_kind_observed(pinned, wait, image, true, |_| {})
            .await
    }
    async fn acquire_kind_observed(
        self: &Arc<Self>,
        pinned: Option<&str>,
        wait: Duration,
        image: bool,
        allow_cooldown: bool,
        progress: impl Fn(QueuePhase) + Send + Sync,
    ) -> Result<Lease> {
        self.acquire_affinity_observed(
            Affinity::Pinned(pinned), &[], wait, image, allow_cooldown, progress,
        ).await
    }
    async fn acquire_affinity_observed(
        self: &Arc<Self>,
        affinity: Affinity<'_>,
        excluded: &[String],
        wait: Duration,
        image: bool,
        allow_cooldown: bool,
        progress: impl Fn(QueuePhase) + Send + Sync,
    ) -> Result<Lease> {
        let pinned = match affinity { Affinity::Pinned(id) => id, Affinity::Portable(_) => None };
        let portable_preference = match affinity { Affinity::Portable(id) => id, Affinity::Pinned(_) => None };
        let started = tokio::time::Instant::now();
        let deadline = started + wait;
        let fallback_at = started + wait.min(PREFERENCE_WAIT);
        // One fixed budget per incoming request; changing/extended cooldowns do
        // not reset it. No lease, credentials or upstream request exist here.
        let cooldown_deadline = started + wait.min(COOLDOWN_WAIT);
        let latest_reset = now().saturating_add(COOLDOWN_WAIT.as_secs() as i64);
        let mut last_phase = None;
        loop {
            if last_phase == Some(QueuePhase::Cooldown)
                && tokio::time::Instant::now() >= cooldown_deadline
            {
                return Err("账号冷却等待超时；本次请求未发送上游，请稍后重试".into());
            }
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let phase = {
                // Persistence and reservation share a lock: concurrent first requests agree.
                let mut state = self.state.lock().unwrap();
                let settings = self.store.settings()?;
                let accounts = self.store.accounts()?;
                let mut candidates: Vec<_> = accounts
                    .iter()
                    .filter(|a| {
                        !excluded.contains(&a.id)
                            && pinned.is_none_or(|id| a.id == id) && eligible(a, &state, pinned.is_some())
                    })
                    .cloned()
                    .collect();
                if candidates.is_empty() {
                    let cooling = allow_cooldown
                        && tokio::time::Instant::now() < cooldown_deadline
                        && accounts.iter().any(|a| {
                            !excluded.contains(&a.id)
                                && pinned.is_none_or(|id| a.id == id)
                                && !state.removing.contains(&a.id)
                                && a.state == "ready"
                                && !quota_blocked(a)
                                && a.cooldown_until > now()
                                && a.cooldown_until <= latest_reset
                        });
                    if !cooling {
                        return Err(if pinned.is_some() {
                            "原会话账号不可用或冷却较长；请恢复该账号后重试，或新建会话"
                        } else {
                            "没有可用账号；请检查授权、暂停、额度或限流冷却状态"
                        }
                        .into());
                    }
                    QueuePhase::Cooldown
                } else {
                    candidates.sort_by(|a, b| compare(a, b, settings.selection_policy, &state));
                    let persisted = self.store.get(CURRENT)?;
                    let current = persisted.clone().or(settings.fixed_account);
                    let portable_index = portable_preference
                        .and_then(|id| candidates.iter().position(|a| a.id == id));
                    let mut index = pinned
                        .and_then(|id| candidates.iter().position(|a| a.id == id))
                        .or(portable_index)
                        .or_else(|| {
                            current
                                .as_ref()
                                .and_then(|id| candidates.iter().position(|a| a.id == *id))
                        })
                        .unwrap_or(0);
                    let full = |a: &Account| {
                        state.active.get(&a.id).copied().unwrap_or(0) >= settings.concurrency
                            || image
                                && (state.images.values().sum::<usize>() >= 2
                                    || state.images.get(&a.id).copied().unwrap_or(0) >= 1)
                    };
                    if full(&candidates[index])
                        && tokio::time::Instant::now() >= fallback_at
                        && pinned.is_none()
                        && portable_index.is_none()
                    {
                        if let Some(other) = candidates.iter().position(|a| !full(a)) {
                            index = other;
                        }
                    }
                    if !full(&candidates[index]) {
                        let account = candidates.swap_remove(index);
                        if pinned.is_none() && persisted.as_deref() != Some(account.id.as_str()) {
                            self.store.set(CURRENT, &account.id)?;
                        }
                        *state.active.entry(account.id.clone()).or_default() += 1;
                        if image {
                            *state.images.entry(account.id.clone()).or_default() += 1;
                        }
                        return Ok(Lease {
                            account,
                            scheduler: self.clone(),
                            image,
                        });
                    }
                    QueuePhase::Capacity
                }
            };
            if last_phase != Some(phase) {
                progress(phase);
                last_phase = Some(phase);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(
                    "本机账号并发繁忙，排队超时；本次请求未发送上游，请等待当前任务完成".into(),
                );
            }
            let next = if phase == QueuePhase::Cooldown {
                cooldown_deadline
            } else if pinned.is_none() && tokio::time::Instant::now() < fallback_at {
                fallback_at
            } else {
                deadline
            };
            tokio::select! {
                _ = changed => {},
                _ = tokio::time::sleep_until((tokio::time::Instant::now() + Duration::from_millis(100)).min(next)) => {},
            }
        }
    }
}
#[cfg(test)]
mod tests;
