//! Bounded, per-surface metadata only. A successful bridge handoff is not proof
//! that the WebView received the event or that a button action completed.
use serde::Serialize;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

static LATEST: OnceLock<Mutex<Option<Arc<Trace>>>> = OnceLock::new();

#[derive(Default)]
struct State {
    active: bool,
    native_down: u64,
    native_up: u64,
    native_wheel: u64,
    capture_changes: u64,
    foreground_rejected: u64,
    dispatched_down: u64,
    dispatched_up: u64,
    dispatched_cancel: u64,
    bridge_accepted: u64,
    bridge_failed: u64,
    last_native: Option<Instant>,
    last_dispatch: Option<Instant>,
    last_bridge: Option<Instant>,
    last_sent: u64,
    last_ack: u64,
    frontend_received: u64,
    frontend_completed: u64,
}
pub(super) struct Trace {
    created: Instant,
    state: Mutex<State>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    active: bool,
    surface_age_ms: u64,
    native_down: u64,
    native_up: u64,
    native_wheel: u64,
    capture_changes: u64,
    foreground_rejected: u64,
    dispatched_down: u64,
    dispatched_up: u64,
    dispatched_cancel: u64,
    bridge_accepted: u64,
    bridge_failed: u64,
    last_native_age_ms: Option<u64>,
    last_dispatch_age_ms: Option<u64>,
    last_bridge_age_ms: Option<u64>,
    frontend_received: u64,
    frontend_completed: u64,
    unacknowledged: bool,
}
fn age(at: Instant) -> u64 { at.elapsed().as_millis().min(u64::MAX as u128) as u64 }

impl Trace {
    pub(super) fn reset_pending(&self) { self.update(|s| s.last_ack = s.last_sent); }
    pub(super) fn sent(&self, sequence: u64) { self.update(|s| s.last_sent = sequence); }
    fn ack(&self, sequence: u64, completed: bool) {
        self.update(|s| {
            if sequence > s.last_ack && sequence <= s.last_sent {
                s.last_ack = sequence;
                s.frontend_received = s.frontend_received.saturating_add(1);
                s.frontend_completed = s.frontend_completed.saturating_add(u64::from(completed));
            }
        });
    }
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self { created: Instant::now(), state: Mutex::new(State { active: true, ..State::default() }) })
    }
    fn update(&self, f: impl FnOnce(&mut State)) {
        // Diagnostics must never prevent normal mouse dispatch, including after
        // an unrelated telemetry failure.
        if let Ok(mut state) = self.state.lock() { f(&mut state); }
    }
    pub(super) fn native(&self, kind: &str) {
        self.update(|s| {
            let count = match kind { "down" => &mut s.native_down, "up" => &mut s.native_up, _ => &mut s.native_wheel };
            *count = count.saturating_add(1);
            s.last_native = Some(Instant::now());
        });
    }
    pub(super) fn capture_changed(&self) {
        self.update(|s| s.capture_changes = s.capture_changes.saturating_add(1));
    }
    pub(super) fn rejected(&self) {
        self.update(|s| s.foreground_rejected = s.foreground_rejected.saturating_add(1));
    }
    pub(super) fn dispatched(&self, kind: &str) {
        self.update(|s| {
            let count = match kind { "down" => &mut s.dispatched_down, "up" => &mut s.dispatched_up, "cancel" => &mut s.dispatched_cancel, _ => { s.last_dispatch = Some(Instant::now()); return; } };
            *count = count.saturating_add(1);
            s.last_dispatch = Some(Instant::now());
        });
    }
    pub(super) fn bridge(&self, accepted: bool) {
        self.update(|s| {
            let count = if accepted { &mut s.bridge_accepted } else { &mut s.bridge_failed };
            *count = count.saturating_add(1);
            s.last_bridge = Some(Instant::now());
        });
    }
    pub(super) fn destroyed(&self) { self.update(|s| s.active = false); }
    pub(super) fn snapshot(&self) -> Option<Snapshot> {
        let s = self.state.lock().ok()?;
        Some(Snapshot {
            active: s.active, surface_age_ms: age(self.created),
            native_down: s.native_down, native_up: s.native_up, native_wheel: s.native_wheel,
            capture_changes: s.capture_changes, foreground_rejected: s.foreground_rejected,
            dispatched_down: s.dispatched_down, dispatched_up: s.dispatched_up, dispatched_cancel: s.dispatched_cancel,
            bridge_accepted: s.bridge_accepted, bridge_failed: s.bridge_failed,
            last_native_age_ms: s.last_native.map(age), last_dispatch_age_ms: s.last_dispatch.map(age),
            last_bridge_age_ms: s.last_bridge.map(age),
            frontend_received: s.frontend_received,
            frontend_completed: s.frontend_completed,
            unacknowledged: s.last_ack < s.last_sent,
        })
    }
}
pub(super) fn remember(trace: Arc<Trace>) {
    if let Ok(mut latest) = LATEST.get_or_init(Default::default).lock() { *latest = Some(trace); }
}
pub fn diagnostics() -> Option<Snapshot> {
    LATEST.get()?.lock().ok()?.as_ref()?.snapshot()
}
pub fn acknowledge(sequence: u64, completed: bool) {
    if let Some(latest) = LATEST.get() {
        if let Ok(trace) = latest.lock() { if let Some(trace) = trace.as_ref() { trace.ack(sequence, completed); } }
    }
}
pub fn stalled() -> bool {
    LATEST.get().and_then(|latest| latest.lock().ok()).and_then(|trace| trace.as_ref().cloned())
        .and_then(|trace| trace.state.lock().ok().map(|s| s.active && s.last_ack < s.last_sent && s.last_bridge.is_some_and(|at| at.elapsed().as_secs() >= 3)))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_distinguishes_native_gate_dispatch_and_bridge_without_payloads() {
        let trace = Trace::new();
        trace.native("down"); trace.rejected();
        trace.native("up"); trace.dispatched("up"); trace.bridge(true);
        trace.capture_changed(); trace.dispatched("cancel"); trace.bridge(false);
        let value = serde_json::to_value(trace.snapshot().unwrap()).unwrap();
        assert_eq!(value["nativeDown"], 1);
        assert_eq!(value["nativeUp"], 1);
        assert_eq!(value["foregroundRejected"], 1);
        assert_eq!(value["dispatchedDown"], 0);
        assert_eq!(value["dispatchedUp"], 1);
        assert_eq!(value["captureChanges"], 1);
        assert_eq!(value["dispatchedCancel"], 1);
        assert_eq!(value["bridgeAccepted"], 1);
        assert_eq!(value["bridgeFailed"], 1);
        assert!(value.get("x").is_none() && value.get("window").is_none());
        trace.destroyed();
        assert!(!trace.snapshot().unwrap().active);
    }
    #[test]
    fn poisoned_metadata_is_optional_and_does_not_propagate_to_dispatch() {
        let trace = Trace::new();
        let poisoned = trace.clone();
        let _ = std::thread::spawn(move || { let _state = poisoned.state.lock().unwrap(); panic!("fixture metadata failure"); }).join();
        trace.native("down"); trace.dispatched("down"); trace.bridge(true);
        assert!(trace.snapshot().is_none());
    }
    #[test]
    fn frontend_acknowledgements_are_bounded_distinct_and_idempotent() {
        let trace = Trace::new();
        trace.sent(1); trace.ack(2, true);
        let snapshot = serde_json::to_value(trace.snapshot().unwrap()).unwrap();
        assert_eq!(snapshot["frontendReceived"],0);
        assert_eq!(snapshot["unacknowledged"],true);
        trace.ack(1,true); trace.ack(1,true); trace.sent(2); trace.ack(2,false);
        let snapshot = serde_json::to_value(trace.snapshot().unwrap()).unwrap();
        assert_eq!(snapshot["frontendReceived"],2);
        assert_eq!(snapshot["frontendCompleted"],1);
        assert_eq!(snapshot["unacknowledged"],false);
    }
}
