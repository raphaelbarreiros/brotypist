//! One pending owner notification, independent of the bounded result queues.
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

const IDLE: u8 = 0;
const PENDING: u8 = 1;
#[cfg(any(target_os = "macos", test))]
const CLOSED: u8 = 2;

struct State {
    pending: AtomicU8,
    signal: Box<dyn Fn() + Send + Sync>,
}

/// Default is disconnected: CLI and other toolkit owners retain their own polling.
#[derive(Clone, Default)]
pub(crate) struct OwnerWake(Option<Arc<State>>);

impl OwnerWake {
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn new(signal: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Some(Arc::new(State {
            pending: AtomicU8::new(IDLE),
            signal: Box::new(signal),
        })))
    }

    /// Call after publishing and releasing every result/state lock. The signal
    /// only schedules the owner; it must never call controller code inline.
    pub(crate) fn notify(&self) {
        if let Some(state) = &self.0 {
            if state
                .pending
                .compare_exchange(IDLE, PENDING, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                #[cfg(feature = "native-smoke")]
                super::stage_diagnostics::mark(
                    super::stage_diagnostics::Stage::OwnerWakeScheduled,
                    None,
                );
                (state.signal)();
            }
        }
    }

    /// Clear BEFORE draining publications, never after. A publication preceding
    /// this exchange is in that drain; a later one owns a fresh notification.
    pub(crate) fn take(&self) -> bool {
        self.0.as_ref().is_some_and(|state| {
            let pending = state
                .pending
                .compare_exchange(PENDING, IDLE, Ordering::AcqRel, Ordering::Acquire)
                .is_ok();
            #[cfg(feature = "native-smoke")]
            if pending {
                super::stage_diagnostics::mark(
                    super::stage_diagnostics::Stage::OwnerWakeConsumed,
                    None,
                );
            }
            pending
        })
    }

    /// Already-entered signals may finish. The platform signal handle must stay
    /// alive through those calls; closing must not free a borrowed native handle.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn close(&self) {
        if let Some(state) = &self.0 {
            state.pending.store(CLOSED, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Barrier, Mutex, atomic::AtomicUsize};
    use std::thread;

    #[test]
    fn coalesces_until_owner_takes_and_disconnects_without_work() {
        let calls = Arc::new(AtomicUsize::new(0));
        let signals = calls.clone();
        let wake = OwnerWake::new(move || {
            signals.fetch_add(1, Ordering::Relaxed);
        });
        for _ in 0..256 {
            wake.notify();
        }
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(wake.take());
        assert!(!wake.take());
        wake.notify();
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert!(wake.take());
        let disconnected = OwnerWake::default();
        disconnected.notify();
        assert!(!disconnected.take());
    }

    #[test]
    fn publication_on_either_side_of_drain_has_a_wake() {
        let wake = OwnerWake::new(|| {});
        let slot = Mutex::new(None);
        *slot.lock().unwrap() = Some(1);
        wake.notify();
        assert!(wake.take());
        // A second publication after rearming but before the queue drain is
        // either consumed now or on the next turn; it cannot disappear.
        *slot.lock().unwrap() = Some(2);
        wake.notify();
        assert_eq!(slot.lock().unwrap().take(), Some(2));
        assert!(wake.take());
        assert_eq!(slot.lock().unwrap().take(), None);
        assert!(!wake.take());
        // Publication after the drain must likewise schedule another turn.
        *slot.lock().unwrap() = Some(3);
        wake.notify();
        assert!(wake.take());
        assert_eq!(slot.lock().unwrap().take(), Some(3));
    }

    #[test]
    fn concurrent_publishers_share_one_pending_notification() {
        let calls = Arc::new(AtomicUsize::new(0));
        let signals = calls.clone();
        let wake = OwnerWake::new(move || {
            signals.fetch_add(1, Ordering::Relaxed);
        });
        let start = Arc::new(Barrier::new(5));
        thread::scope(|scope| {
            for _ in 0..4 {
                let wake = wake.clone();
                let start = start.clone();
                scope.spawn(move || {
                    start.wait();
                    wake.notify();
                });
            }
            start.wait();
        });
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(wake.take());
        assert!(!wake.take());
    }

    #[test]
    fn rearming_during_an_inflight_signal_preserves_the_next_publication() {
        let entered = Arc::new(Barrier::new(2));
        let resume = Arc::new(Barrier::new(2));
        let signal_entered = entered.clone();
        let signal_resume = resume.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let signals = calls.clone();
        let wake = OwnerWake::new(move || {
            if signals.fetch_add(1, Ordering::Relaxed) == 0 {
                signal_entered.wait();
                signal_resume.wait();
            }
        });
        let publisher = wake.clone();
        let thread = thread::spawn(move || publisher.notify());
        entered.wait();
        // An ordinary native event lets the owner drain before the first
        // platform signal finishes. The next producer must still signal.
        assert!(wake.take());
        wake.notify();
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        resume.wait();
        thread.join().unwrap();
        assert!(wake.take());
        assert!(!wake.take());
    }

    #[test]
    fn closing_during_signal_never_rearms_or_invalidates_owned_callback() {
        let entered = Arc::new(Barrier::new(2));
        let resume = Arc::new(Barrier::new(2));
        let signal_entered = entered.clone();
        let signal_resume = resume.clone();
        let wake = OwnerWake::new(move || {
            signal_entered.wait();
            signal_resume.wait();
        });
        let publisher = wake.clone();
        let thread = thread::spawn(move || publisher.notify());
        entered.wait();
        wake.close();
        assert!(!wake.take());
        // These calls must not enter the blocked callback a second time.
        wake.notify();
        resume.wait();
        thread.join().unwrap();
        wake.notify();
        assert!(!wake.take());
    }
}
