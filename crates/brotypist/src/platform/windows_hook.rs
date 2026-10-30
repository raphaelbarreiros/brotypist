//! Dedicated Win32 message owner. No accessibility, inference, or text reads here.
use super::{decisions::{ConsumedKeys, Modifiers, PhysicalKey, shortcut_action}, ObservationPolicy, ShortcutHandler};
use anyhow::{Result, anyhow, bail};
use std::{cell::RefCell, sync::{Arc, Condvar, Mutex, atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering}, mpsc::{self, Receiver}}, thread::{self, JoinHandle}, time::{Duration, Instant}};
use windows::Win32::{Foundation::{LPARAM, LRESULT, WPARAM}, System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId}, UI::{Input::KeyboardAndMouse::GetAsyncKeyState, WindowsAndMessaging::*}};

const IDLE: u8 = 0;
const PENDING: u8 = 1;
const MUTATING: u8 = 2;
const ACCEPTED: u8 = 3;
const REJECTED: u8 = 4;
const CANCELLED: u8 = 5;
const UNCERTAIN: u8 = 6;
const ENQUEUED: u8 = 7;
const PHASE: u8 = 15;
const HOOK_RETURNED: u8 = 16;
const OWNER_ACK: u8 = 32;
const MAX_CLAIM: Duration = Duration::from_millis(500);
pub(super) struct Published {
    pub policy: Arc<ObservationPolicy>,
    pub app: String,
    pub foreground: usize,
    pub callback: Option<ShortcutHandler>,
    pub input_callback: Option<Arc<dyn Fn() + Send + Sync>>,
}
pub(super) struct HookShared {
    pub published: Mutex<Published>,
    pub enabled: AtomicBool,
    pub stopping: AtomicBool,
    pub revision: AtomicU64,
    pub user_revision: AtomicU64,
    pub timestamp: Mutex<Option<Instant>>,
    pub diagnostic: Mutex<String>,
    transaction: AtomicU8,
    deadline: Mutex<Option<Instant>>,
    wait_lock: Mutex<()>,
    disposition: Condvar,
}
impl HookShared {
    pub fn new(policy: Arc<ObservationPolicy>) -> Self {
        Self { published: Mutex::new(Published { policy, app: String::new(), foreground: 0, callback: None, input_callback: None }), enabled: AtomicBool::new(false), stopping: AtomicBool::new(false), revision: AtomicU64::new(0), user_revision: AtomicU64::new(0), timestamp: Mutex::new(None), diagnostic: Mutex::new(String::new()), transaction: AtomicU8::new(IDLE), deadline: Mutex::new(None), wait_lock: Mutex::new(()), disposition: Condvar::new() }
    }
    pub fn note(&self, user: bool) {
        let revision = self.revision.fetch_add(1, Ordering::AcqRel) + 1;
        if user {
            self.user_revision.store(revision, Ordering::Release);
            if let Ok(mut timestamp) = self.timestamp.try_lock() { *timestamp = Some(Instant::now()); }
        }
        // The callback is a nonblocking runtime wake. Never hold a lock across it.
        let callback = self.published.try_lock().ok().and_then(|state| state.input_callback.clone());
        if !self.stopping.load(Ordering::Acquire) {
            if let Some(callback) = callback { let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback())); }
        }
    }
    fn begin_claim(&self) -> bool {
        let _guard = self.wait_lock.lock().unwrap_or_else(|error| error.into_inner());
        if self.transaction.load(Ordering::Acquire) != IDLE || self.stopping.load(Ordering::Acquire) { return false; }
        *self.deadline.lock().unwrap_or_else(|error| error.into_inner()) = Some(Instant::now() + MAX_CLAIM);
        self.transaction.store(ENQUEUED, Ordering::Release);
        true
    }
    fn retire(&self, bit: u8) {
        let previous = self.transaction.fetch_or(bit, Ordering::AcqRel);
        let next = previous | bit;
        if next & (HOOK_RETURNED | OWNER_ACK) == (HOOK_RETURNED | OWNER_ACK) {
            let _ = self.transaction.compare_exchange(next, IDLE, Ordering::AcqRel, Ordering::Acquire);
        }
    }
    pub fn claim_valid(&self) -> bool {
        match self.transaction.load(Ordering::Acquire) & PHASE {
            IDLE => !self.stopping.load(Ordering::Acquire),
            ENQUEUED | PENDING | MUTATING => self.deadline.lock().unwrap_or_else(|error| error.into_inner()).is_some_and(|deadline| Instant::now() < deadline),
            _ => false,
        }
    }
    pub fn activate_claim(&self) -> bool {
        // Dequeue activation prevents an unrelated owner-thread UI edit from
        // borrowing the permit of a shortcut which is still only enqueued.
        let _ = self.transaction.compare_exchange(ENQUEUED, PENDING, Ordering::AcqRel, Ordering::Acquire);
        self.claim_valid()
    }
    /// The cancellation CAS is the authority immediately before each native
    /// selection/text effect, not a sampled bool captured before slow UIA work.
    pub fn begin_effect(&self) -> bool {
        loop {
            let state = self.transaction.load(Ordering::Acquire);
            match state & PHASE {
                IDLE => return !self.stopping.load(Ordering::Acquire),
                PENDING | MUTATING => {
                    if !self.claim_valid() {
                        let cancelled = if state & PHASE == MUTATING { UNCERTAIN } else { CANCELLED };
                        if self.transaction.compare_exchange(state, cancelled, Ordering::AcqRel, Ordering::Acquire).is_ok() { self.disposition.notify_all(); return false; }
                    } else if self.transaction.compare_exchange(state, MUTATING, Ordering::AcqRel, Ordering::Acquire).is_ok() { return true; }
                }
                _ => return false,
            }
        }
    }
    pub fn finish_claim(&self, accepted: bool) {
        let _guard = self.wait_lock.lock().unwrap_or_else(|error| error.into_inner());
        loop {
            let state = self.transaction.load(Ordering::Acquire);
            match state & PHASE {
                IDLE | ACCEPTED | REJECTED => break,
                CANCELLED | UNCERTAIN => { self.retire(OWNER_ACK); break; }
                ENQUEUED | PENDING | MUTATING => {
                    // Once any native effect was issued, preserve uncertainty:
                    // forwarding the original shortcut could apply a second effect.
                    let disposition = if accepted || state & PHASE == MUTATING { ACCEPTED } else { REJECTED };
                    if self.transaction.compare_exchange(state, disposition, Ordering::AcqRel, Ordering::Acquire).is_ok() { break; }
                }
                _ => break,
            }
        }
        self.disposition.notify_all();
    }
    pub fn cancel_claim(&self) {
        loop {
            let state = self.transaction.load(Ordering::Acquire);
            let replacement = match state & PHASE { ENQUEUED | PENDING => CANCELLED, MUTATING => UNCERTAIN, _ => break };
            if self.transaction.compare_exchange(state, replacement, Ordering::AcqRel, Ordering::Acquire).is_ok() { break; }
        }
        self.disposition.notify_all();
    }
    fn wait_claim(&self) -> bool {
        let deadline = self.deadline.lock().unwrap_or_else(|error| error.into_inner()).unwrap_or_else(Instant::now);
        let mut guard = self.wait_lock.lock().unwrap_or_else(|error| error.into_inner());
        loop {
            let state = self.transaction.load(Ordering::Acquire);
            match state & PHASE {
                ACCEPTED | REJECTED => {
                    if self.transaction.compare_exchange(state, IDLE, Ordering::AcqRel, Ordering::Acquire).is_ok() { return state & PHASE == ACCEPTED; }
                }
                CANCELLED | UNCERTAIN => { self.retire(HOOK_RETURNED); return state & PHASE == UNCERTAIN; }
                ENQUEUED | PENDING | MUTATING => {
                    let now = Instant::now();
                    if now >= deadline { self.cancel_claim(); continue; }
                    // Original physical event remains owned by this native callback.
                    // No synthetic replay can land on a changed foreground target.
                    let (next, _) = self.disposition.wait_timeout(guard, deadline - now).unwrap_or_else(|error| error.into_inner());
                    guard = next;
                }
                _ => return false,
            }
        }
    }
}
struct Local { shared: Arc<HookShared>, consumed: ConsumedKeys }
thread_local! { static LOCAL: RefCell<Option<Local>> = const { RefCell::new(None) }; }
const STOP: u32 = WM_APP + 42;

unsafe extern "system" fn keyboard_hook(code: i32, message: WPARAM, pointer: LPARAM) -> LRESULT {
    // No panic may cross the system ABI, including a poisoned application callback.
    let consumed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if code < 0 || pointer.0 == 0 { return false; }
        let event = unsafe { &*(pointer.0 as *const KBDLLHOOKSTRUCT) };
        if event.flags.0 & 0x12 != 0 { return false; } // injected or lower-integrity injected
        let down = matches!(message.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
        let up = matches!(message.0 as u32, WM_KEYUP | WM_SYSKEYUP);
        if !down && !up { return false; }
        LOCAL.with(|local| {
            let mut local = local.borrow_mut();
            let Some(local) = local.as_mut() else { return false };
            let key = PhysicalKey { vk: event.vkCode, scan: event.scanCode, extended: event.flags.0 & 1 != 0 };
            if up {
                let consumed = local.consumed.release(key);
                if local.shared.stopping.load(Ordering::Acquire) && local.consumed.is_empty() {
                    unsafe { let _ = PostThreadMessageW(GetCurrentThreadId(), STOP, WPARAM(0), LPARAM(0)); }
                }
                return consumed;
            }
            local.shared.note(true);
            if local.consumed.contains(key) { return true; } // same consumption for auto-repeat
            if !local.shared.enabled.load(Ordering::Acquire) || local.shared.stopping.load(Ordering::Acquire)
                || !local.consumed.has_capacity() { return false; }
            let pressed = |key| unsafe { GetAsyncKeyState(key) } < 0;
            let modifiers = Modifiers { ctrl: pressed(0x11), alt: pressed(0x12), shift: pressed(0x10), super_key: pressed(0x5b) || pressed(0x5c), right_alt: pressed(0xa5) };
            let dispatch = local.shared.published.try_lock().ok().and_then(|state| {
                if state.foreground == 0 || unsafe { GetForegroundWindow() }.0 as usize != state.foreground { return None; }
                let action = shortcut_action(&state.policy, key, event.flags.0, modifiers, &state.app)?;
                Some((state.callback.clone()?, action))
            });
            let Some((callback, action)) = dispatch else { return false };
            if !local.shared.begin_claim() { return false; }
            let queued = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback(action))).unwrap_or_else(|_| {
                local.shared.enabled.store(false, Ordering::Release); false
            });
            if !queued { local.shared.finish_claim(false); }
            local.shared.wait_claim() && local.consumed.press(key)
        })
    })).unwrap_or(false);
    if consumed { LRESULT(1) } else { unsafe { CallNextHookEx(None, code, message, pointer) } }
}

pub(super) struct HookOwner { pub thread_id: u32, thread: Option<JoinHandle<()>>, stopped: Receiver<std::result::Result<(), String>>, acknowledged: bool }
impl HookOwner {
    pub fn start(shared: Arc<HookShared>) -> Result<Self> {
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (stop_tx, stop_rx) = mpsc::sync_channel(1);
        let thread = thread::Builder::new().name("inklow-win-hook".into()).spawn(move || unsafe {
            let mut message = MSG::default();
            PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE); // force queue creation before ready ACK
            LOCAL.with(|local| *local.borrow_mut() = Some(Local { shared: shared.clone(), consumed: ConsumedKeys::default() }));
            let hook = GetModuleHandleW(None).and_then(|module| SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), Some(module.into()), 0));
            let hook = match hook {
                Ok(hook) => hook,
                Err(error) => { let _ = ready_tx.send(Err(error.to_string())); LOCAL.with(|local| *local.borrow_mut() = None); return; }
            };
            let _ = ready_tx.send(Ok(GetCurrentThreadId()));
            loop {
                // Keep the hook while a consumed key is physically down, even
                // after disable/stop, so the target never sees an orphan release.
                if shared.stopping.load(Ordering::Acquire) && LOCAL.with(|local| local.borrow().as_ref().is_none_or(|local| local.consumed.is_empty())) {
                    match UnhookWindowsHookEx(hook) {
                        Ok(()) => break,
                        Err(error) => { if let Ok(mut diagnostic) = shared.diagnostic.lock() { *diagnostic = format!("Keyboard hook cleanup failed: {error}"); } }
                    }
                }
                let result = GetMessageW(&mut message, None, 0, 0).0;
                if result == -1 {
                    shared.enabled.store(false, Ordering::Release);
                    shared.stopping.store(true, Ordering::Release);
                    // Do not drop callback storage while Windows may still own it.
                    if UnhookWindowsHookEx(hook).is_ok() { break; }
                    thread::sleep(Duration::from_millis(10));
                } else if result == 0 || message.message == STOP {
                    shared.enabled.store(false, Ordering::Release);
                    shared.stopping.store(true, Ordering::Release);
                }
            }
            LOCAL.with(|local| *local.borrow_mut() = None);
            let _ = stop_tx.send(Ok(())); // ACK only after the hook and callback have been released
        })?;
        let thread_id = match ready_rx.recv() {
            Ok(Ok(id)) => id,
            Ok(Err(message)) => { let _ = thread.join(); bail!("Windows keyboard hook denied: {message}"); }
            Err(error) => { let _ = thread.join(); return Err(error.into()); }
        };
        Ok(Self { thread_id, thread: Some(thread), stopped: stop_rx, acknowledged: false })
    }
    pub fn stop(&mut self, shared: &HookShared) -> Result<()> {
        if self.acknowledged { return Ok(()); }
        shared.enabled.store(false, Ordering::Release);
        shared.stopping.store(true, Ordering::Release);
        shared.cancel_claim();
        unsafe { PostThreadMessageW(self.thread_id, STOP, WPARAM(0), LPARAM(0))?; }
        self.stopped.recv_timeout(Duration::from_secs(2)).map_err(|error| anyhow!("Hook stop has not acknowledged release (held shortcut keys must be released): {error}"))?.map_err(anyhow::Error::msg)?;
        self.acknowledged = true;
        if let Some(thread) = self.thread.take() { thread.join().map_err(|_| anyhow!("Windows hook owner panicked"))?; }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn shared() -> HookShared { HookShared::new(Arc::new(ObservationPolicy::new(&crate::config::Config::default()).unwrap())) }
    fn expire(shared: &HookShared) { *shared.deadline.lock().unwrap() = Some(Instant::now() - Duration::from_secs(1)); }
    #[test]
    fn timeout_passes_original_and_disarms_until_cancelled_owner_ack() {
        let shared = shared();
        assert!(shared.begin_claim());
        expire(&shared);
        assert!(!shared.wait_claim());
        assert!(!shared.activate_claim());
        assert!(!shared.begin_effect());
        assert!(!shared.begin_claim()); // stale queued command still owns retirement
        shared.finish_claim(false);
        assert!(shared.begin_claim());
        shared.finish_claim(false);
        assert!(!shared.wait_claim());
        assert_eq!(shared.transaction.load(Ordering::Acquire), IDLE);
    }
    #[test]
    fn preflight_does_not_mutate_before_dequeue_and_cancel_cas_beats_effect() {
        let shared = shared();
        assert!(shared.begin_claim());
        assert!(!shared.begin_effect()); // no borrowing an enqueued action's permit
        assert!(shared.activate_claim());
        shared.cancel_claim();
        assert!(!shared.begin_effect());
        shared.finish_claim(false);
        assert!(!shared.wait_claim());
        assert!(shared.claim_valid()); // direct UI commands work without a hook claim
    }
    #[test]
    fn effect_cas_beats_timeout_and_uncertain_input_never_replays_original() {
        let shared = shared();
        assert!(shared.begin_claim());
        assert!(shared.activate_claim());
        assert!(shared.begin_effect());
        expire(&shared);
        assert!(shared.wait_claim()); // native operation may already have changed the host
        assert!(!shared.begin_effect()); // no later text event after the permit expired
        assert!(!shared.begin_claim());
        shared.finish_claim(false); // even a rejecting/uncertain owner cannot duplicate original input
        assert_eq!(shared.transaction.load(Ordering::Acquire), IDLE);
    }
    #[test]
    fn owner_disposition_and_close_release_original_without_synthetic_input() {
        let shared = shared();
        assert!(shared.begin_claim());
        shared.finish_claim(false);
        assert!(!shared.wait_claim());
        assert!(shared.begin_claim());
        shared.stopping.store(true, Ordering::Release);
        shared.cancel_claim();
        shared.finish_claim(false);
        assert!(!shared.wait_claim());
        assert!(!shared.claim_valid());
    }
}
