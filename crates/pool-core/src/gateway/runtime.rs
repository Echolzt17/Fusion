//! Bounded, in-memory request ownership. No request or response content is retained.
use super::*;
use std::{collections::HashMap, sync::Mutex as StdMutex};

#[derive(Default)]
pub(super) struct Registry {
    entries: StdMutex<HashMap<String, Entry>>,
}
struct Entry {
    operation: &'static str,
    account: Option<String>,
    conversation_key: Option<String>,
    phase: &'static str,
    started: Instant,
    activity: Instant,
    cancel: CancellationToken,
    done: CancellationToken,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveRequest {
    pub id: String,
    pub operation: &'static str,
    pub account: Option<String>,
    pub conversation_key: Option<String>,
    pub phase: &'static str,
    pub elapsed_ms: u64,
    pub idle_ms: u64,
}
#[derive(Clone)]
pub(super) struct RequestHandle {
    pub id: String,
    pub cancel: CancellationToken,
    pub audit_claimed: Arc<std::sync::atomic::AtomicBool>,
    pub started: Instant,
    pub started_at: i64,
    pub ingress_queue_ms: Option<u64>,
    registry: Arc<Registry>,
    upload: Arc<StdMutex<Option<tokio::sync::OwnedSemaphorePermit>>>,
}
pub(super) struct Registration {
    handle: RequestHandle,
    done: CancellationToken,
}
impl Drop for Registration {
    fn drop(&mut self) {
        self.handle.finish_upload();
        self.handle.registry.entries.lock().unwrap().remove(&self.handle.id);
        self.done.cancel();
    }
}
impl Registry {
    pub fn register(self: &Arc<Self>, operation: &'static str, id: String, started: Instant, started_at: i64) -> (RequestHandle, Registration) {
        let cancel = CancellationToken::new();
        let done = CancellationToken::new();
        self.entries.lock().unwrap().insert(id.clone(), Entry {
            operation, account: None, conversation_key: None, phase: "uploading",
            started, activity: Instant::now(), cancel: cancel.clone(), done: done.clone(),
        });
        let handle = RequestHandle { id, cancel, audit_claimed:Arc::new(std::sync::atomic::AtomicBool::new(false)),started,started_at,ingress_queue_ms:None,registry: self.clone(), upload: Arc::new(StdMutex::new(None)) };
        let registration = Registration { handle: handle.clone(), done };
        (handle, registration)
    }
    pub fn snapshot(&self) -> Vec<ActiveRequest> {
        let mut entries: Vec<_> = self.entries.lock().unwrap().iter().map(|(id, entry)| ActiveRequest {
            id: id.clone(), operation: entry.operation, account: entry.account.clone(),
            conversation_key: entry.conversation_key.clone(), phase: entry.phase,
            elapsed_ms: entry.started.elapsed().as_millis() as u64,
            idle_ms: entry.activity.elapsed().as_millis() as u64,
        }).collect();
        entries.sort_by(|a, b| b.elapsed_ms.cmp(&a.elapsed_ms).then_with(|| a.id.cmp(&b.id)));
        entries
    }
    pub async fn cancel(&self, id: &str) -> Result<()> {
        let pending = {
            let mut entries = self.entries.lock().unwrap();
            entries.get_mut(id).map(|entry| {
                entry.phase = "cancelling";
                entry.cancel.cancel();
                entry.done.clone()
            })
        };
        if let Some(done) = pending {
            tokio::time::timeout(Duration::from_secs(5), done.cancelled()).await
                .map_err(|_| "取消仍在处理中，尚未确认释放完成；请等待状态更新")?;
        }
        Ok(())
    }
}
impl RequestHandle {
    pub fn hold_upload(&self, permit: tokio::sync::OwnedSemaphorePermit) {
        *self.upload.lock().unwrap() = Some(permit);
    }
    pub fn finish_upload(&self) {
        self.upload.lock().unwrap().take();
    }
    pub fn phase(&self, phase: &'static str) {
        if let Some(entry) = self.registry.entries.lock().unwrap().get_mut(&self.id) {
            if entry.phase != "cancelling" { entry.phase = phase; }
            entry.activity = Instant::now();
        }
    }
    pub fn identify(&self, account: Option<&str>, conversation: Option<&str>) {
        if let Some(entry) = self.registry.entries.lock().unwrap().get_mut(&self.id) {
            entry.account = account.map(str::to_owned);
            entry.conversation_key = conversation.map(str::to_owned);
        }
    }
}
