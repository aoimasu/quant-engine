//! The out-of-band, latching kill switch.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// An out-of-band halt control: trippable from anywhere, honoured at the order-submission layer.
///
/// "Out-of-band" means it is independent of the cockpit and the Hedge Planner — a watchdog, the
/// clock-skew guard (QE-008), or a manual control can all trip it. Once tripped it **latches**
/// (stays tripped for the run), giving a deterministic halt.
pub trait KillSwitch: Send + Sync {
    /// Whether the switch has been tripped.
    fn is_tripped(&self) -> bool;
    /// The reason it was tripped, if any.
    fn reason(&self) -> Option<String>;
    /// Trip the switch with a reason. Idempotent: the first reason wins (latching).
    fn trip(&self, reason: &str);
}

/// A cloneable handle to one shared kill state. Clones observe the same trip.
#[derive(Clone, Default)]
pub struct KillHandle {
    inner: Arc<KillState>,
}

#[derive(Default)]
struct KillState {
    tripped: AtomicBool,
    reason: Mutex<Option<String>>,
}

impl KillHandle {
    /// A fresh, untripped handle.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl std::fmt::Debug for KillHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KillHandle")
            .field("tripped", &self.is_tripped())
            .finish()
    }
}

impl KillSwitch for KillHandle {
    fn is_tripped(&self) -> bool {
        self.inner.tripped.load(Ordering::SeqCst)
    }

    fn reason(&self) -> Option<String> {
        self.inner.reason.lock().expect("kill reason lock").clone()
    }

    fn trip(&self, reason: &str) {
        // Latch the reason exactly once: set the text only on the first trip.
        let mut slot = self.inner.reason.lock().expect("kill reason lock");
        if slot.is_none() {
            *slot = Some(reason.to_owned());
        }
        self.inner.tripped.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_untripped() {
        let h = KillHandle::new();
        assert!(!h.is_tripped());
        assert_eq!(h.reason(), None);
    }

    #[test]
    fn trip_latches_and_first_reason_wins() {
        let h = KillHandle::new();
        h.trip("skew");
        assert!(h.is_tripped());
        assert_eq!(h.reason().as_deref(), Some("skew"));
        h.trip("something else");
        // Latched: still tripped, original reason preserved.
        assert!(h.is_tripped());
        assert_eq!(h.reason().as_deref(), Some("skew"));
    }

    #[test]
    fn clones_share_state() {
        let a = KillHandle::new();
        let b = a.clone();
        a.trip("manual");
        assert!(b.is_tripped()); // the clone observes the same trip
        assert_eq!(b.reason().as_deref(), Some("manual"));
    }
}
