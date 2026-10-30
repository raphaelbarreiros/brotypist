//! Quartz input ownership and ordered, bounded-before-commit shortcut transactions.
//! Layout translation and foreground/AX observation are published by the bridge;
//! neither event-tap callback calls AppKit, Accessibility, capture, or inference.
use super::ax::ffi::{
    CFMachPortCreateRunLoopSource, CFMachPortInvalidate, CFRelease, CFRetain, CFRunLoopAddSource,
    CFRunLoopGetCurrent, CFRunLoopRemoveSource, CFRunLoopRunInMode, CGEventCreateCopy,
    CGEventCreateKeyboardEvent, CGEventGetFlags, CGEventGetIntegerValueField,
    CGEventKeyboardGetUnicodeString, CGEventKeyboardSetUnicodeString, CGEventPost, CGEventSetFlags,
    CGEventSetIntegerValueField, CGEventSourceFlagsState, CGEventTapCreate, CGEventTapEnable,
    CGPreflightPostEventAccess, Native, kCFRunLoopCommonModes, kCFRunLoopDefaultMode,
};
use crate::platform::{Action, Foreground, ShortcutHandler, policy::ObservationPolicy};
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    panic::{AssertUnwindSafe, catch_unwind},
    ptr,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const SYNTHETIC_TAG: i64 = 0x4252_4f54;
const KEY_DOWN: u32 = 10;
const KEY_UP: u32 = 11;
const FLAGS_CHANGED: u32 = 12;
const TAP_TIMEOUT: u32 = u32::MAX - 1;
const TAP_USER_DISABLED: u32 = u32::MAX;
const SHIFT: u64 = 1 << 17;
const CONTROL: u64 = 1 << 18;
const ALTERNATE: u64 = 1 << 19;
const COMMAND: u64 = 1 << 20;
const FUNCTION: u64 = 1 << 23;
const KEYCODE: u32 = 9;
const SOURCE_PID: u32 = 41;
const SOURCE_TAG: u32 = 42;
const REPLAY_LIMIT: usize = 256;
const REPLAY_TIME: Duration = Duration::from_secs(2);
const STOP_TIME: Duration = Duration::from_secs(2);

type InputCallback = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InputSourceKind {
    Layout,
    Ime,
    #[default]
    Unknown,
}
impl InputSourceKind {
    fn label(self) -> &'static str {
        match self {
            Self::Layout => "layout",
            Self::Ime => "ime",
            Self::Unknown => "unknown",
        }
    }
}

/// A unique Create/Copy reference. Only this owner mutates/tags it, and mutation
/// is serialized by the input pump before it is posted and released.
struct OwnedEvent(Native);
// SAFETY: CGEvent copies are independent CF objects. No borrowed callback event
// crosses threads; the unique copy is transferred, never accessed concurrently.
unsafe impl Send for OwnedEvent {}
impl Drop for OwnedEvent {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: this is the single release of a successful Create/Copy.
            unsafe { CFRelease(self.0) }
        }
    }
}

struct Event {
    native: OwnedEvent,
    kind: u32,
    code: u16,
    flags: u64,
    has_characters: bool,
    #[cfg(test)]
    serial: usize,
}
impl Event {
    unsafe fn copy(kind: u32, raw: Native) -> Self {
        // SAFETY: Quartz lends a nonnull event for the duration of the callback;
        // CreateCopy gives this record its own reference, not the lent reference.
        let copied = unsafe { CGEventCreateCopy(raw) };
        // If Copy cannot allocate, retain the suppressed callback event instead
        // of delivering it ahead of an issued mutation. Quartz releases its
        // callback loan after return; our reference pins it until ordered replay.
        // SAFETY: raw is nonnull and live; Retain does not allocate a new event.
        let native = if copied.is_null() {
            unsafe {
                CFRetain(raw);
            }
            raw
        } else {
            copied
        };
        let mut length = 0;
        let mut characters = [0u16; 4];
        // SAFETY: all output buffers are valid for the advertised capacities.
        if kind == KEY_DOWN {
            unsafe {
                CGEventKeyboardGetUnicodeString(raw, 4, &mut length, characters.as_mut_ptr())
            };
        }
        Self {
            native: OwnedEvent(native),
            kind,
            // SAFETY: raw is still the live callback reference.
            code: unsafe { CGEventGetIntegerValueField(raw, KEYCODE) } as u16,
            flags: unsafe { CGEventGetFlags(raw) },
            has_characters: length != 0,
            #[cfg(test)]
            serial: 0,
        }
    }
    fn post(self) {
        // SAFETY: native is our uniquely owned, valid CGEvent; Quartz copies the
        // posted event before returning. The tag keeps replay invisible to taps.
        unsafe {
            CGEventSetFlags(self.native.0, self.flags);
            CGEventSetIntegerValueField(self.native.0, SOURCE_TAG, SYNTHETIC_TAG);
            CGEventPost(1, self.native.0);
        }
    }
}

struct Queued {
    event: Event,
    redispatch: bool,
}
struct Gate {
    id: u64,
    code: u16,
    original: Option<Event>,
    events: VecDeque<Event>,
    deadline: Instant,
    dispatching: bool,
    issued: bool,
    committing: bool,
    ready: bool,
    consumed: bool,
    aborted: bool,
    released: bool,
    overrun: bool,
    bypass: bool,
}
struct State {
    policy: Option<Arc<ObservationPolicy>>,
    foreground: Foreground,
    layout: BTreeMap<u16, String>,
    source: InputSourceKind,
    enabled: bool,
    stopping: bool,
    handler: Option<ShortcutHandler>,
    callback: Option<InputCallback>,
    gate: Option<Gate>,
    next_gate: u64,
    replay: VecDeque<Queued>,
    driving: bool,
    suppressed: SuppressedKeyUps,
    timeouts: VecDeque<Instant>,
    revision: u64,
    revision_signal: Arc<AtomicU64>,
    user_revision: u64,
    timestamp: Option<Instant>,
    dead_key: bool,
    notify: bool,
    message: &'static str,
}
impl Default for State {
    fn default() -> Self {
        Self {
            policy: None,
            foreground: Foreground::default(),
            layout: BTreeMap::new(),
            source: InputSourceKind::Unknown,
            enabled: false,
            stopping: false,
            handler: None,
            callback: None,
            gate: None,
            next_gate: 0,
            replay: VecDeque::new(),
            driving: false,
            suppressed: SuppressedKeyUps::default(),
            timeouts: VecDeque::with_capacity(4),
            revision: 0,
            user_revision: 0,
            revision_signal: Arc::new(AtomicU64::new(0)),
            timestamp: None,
            dead_key: false,
            notify: false,
            message: "not started",
        }
    }
}
impl State {
    fn note(&mut self, user: bool, now: Instant) {
        self.revision = self.revision.wrapping_add(1);
        self.revision_signal.store(self.revision, Ordering::Release);
        if user {
            self.user_revision = self.user_revision.wrapping_add(1);
        }
        self.timestamp = Some(now);
        self.notify = true;
    }
    fn intercepting(&self) -> bool {
        self.enabled && !self.stopping && !self.gate.as_ref().is_some_and(|g| g.released)
    }
    fn holding(&self) -> bool {
        self.gate.as_ref().is_some_and(|g| !g.released)
    }
    fn tap_needed(&self) -> bool {
        self.intercepting() || self.holding() || self.driving || !self.replay.is_empty()
    }
    fn action(&self, code: u16, flags: u64) -> Option<Action> {
        if !self.intercepting() {
            return None;
        }
        let key = self.layout.get(&code)?;
        // Fn modifies ordinary keys (for example Fn+Backspace); it must not
        // accidentally match an unmodified binding. Function keys retain the
        // system Fn flag as on the original Quartz path.
        if flags & FUNCTION != 0 && !is_function_key(key) {
            return None;
        }
        let mut modifiers = [""; 4];
        let mut count = 0;
        for (name, mask) in [
            ("Ctrl", CONTROL),
            ("Alt", ALTERNATE),
            ("Shift", SHIFT),
            ("Super", COMMAND),
        ] {
            if flags & mask != 0 {
                modifiers[count] = name;
                count += 1;
            }
        }
        self.policy.as_ref()?.key_action(
            key,
            &modifiers[..count],
            "macos",
            &self.foreground.app_id,
            &self.foreground.domain,
        )
    }
    fn ordinary(
        &mut self,
        kind: u32,
        code: u16,
        flags: u64,
        has_characters: bool,
        now: Instant,
    ) -> bool {
        if kind == KEY_UP {
            return !self.suppressed.remove(&code);
        }
        if kind == KEY_DOWN {
            self.suppressed.remove(&code);
            if flags & FUNCTION == 0
                || self
                    .layout
                    .get(&code)
                    .is_some_and(|key| is_function_key(key))
            {
                self.dead_key = flags & (COMMAND | CONTROL) == 0 && !has_characters;
            }
        } else if kind != FLAGS_CHANGED {
            self.dead_key = false;
        }
        self.note(true, now);
        true
    }
    fn open(&mut self, action: &Action, original: Event, now: Instant) -> u64 {
        self.next_gate = self.next_gate.wrapping_add(1);
        let id = self.next_gate;
        self.gate = Some(Gate {
            id,
            code: original.code,
            original: Some(original),
            events: VecDeque::with_capacity(REPLAY_LIMIT),
            deadline: now + REPLAY_TIME,
            dispatching: true,
            issued: false,
            committing: false,
            ready: false,
            consumed: false,
            aborted: false,
            released: false,
            overrun: false,
            bypass: *action == Action::Bypass,
        });
        id
    }
    fn abort(&mut self, now: Instant) {
        let Some(gate) = self.gate.as_mut() else {
            return;
        };
        if gate.released {
            return;
        }
        gate.aborted = true;
        if gate.issued && !gate.ready {
            // No fixed memory bound is claimed after an irreversible OS call.
            // Keep its entire tail until the outcome is known, even past 256.
            gate.overrun = true;
            return;
        }
        if !gate.dispatching {
            self.release(now);
        }
    }
    fn expire(&mut self, now: Instant) {
        if self
            .gate
            .as_ref()
            .is_some_and(|g| !g.released && now >= g.deadline)
        {
            self.abort(now);
        }
    }
    fn release(&mut self, now: Instant) {
        let Some(mut gate) = self.gate.take() else {
            return;
        };
        if gate.released || gate.dispatching || (!gate.ready && (!gate.aborted || gate.issued)) {
            self.gate = Some(gate);
            return;
        }
        let consumed = gate.consumed || gate.issued;
        if consumed && !gate.bypass {
            self.suppressed.insert(gate.code);
        } else {
            self.suppressed.remove(&gate.code);
            if let Some(event) = gate.original.take() {
                self.replay.push_back(Queued {
                    event,
                    redispatch: false,
                });
            }
        }
        if gate.aborted {
            self.note(true, now);
        }
        for event in gate.events.drain(..) {
            self.replay.push_back(Queued {
                event,
                redispatch: !gate.aborted,
            });
        }
        if gate.overrun {
            self.enabled = false;
            self.message = "Deferred input exceeded its limit during a native edit; queued input was preserved and released in order; interception is disabled.";
        }
        if !gate.ready {
            // A precommit timeout has already replayed the original. Keep its
            // identity until runtime finish so a late commit/finish cannot act
            // on a newer shortcut transaction (the public finish API is bool).
            gate.released = true;
            gate.original = None;
            self.gate = Some(gate);
        }
    }
    fn enqueue(&mut self, event: Event, now: Instant) {
        let was_holding = self.holding();
        if self.holding() {
            let over_limit = self
                .gate
                .as_ref()
                .is_some_and(|g| now >= g.deadline || g.events.len() >= REPLAY_LIMIT);
            if over_limit {
                self.abort(now);
            }
            if let Some(gate) = self.gate.as_mut().filter(|g| !g.released) {
                gate.events.push_back(event);
                return;
            }
        }
        self.replay.push_back(Queued {
            event,
            redispatch: !was_holding && (self.driving || !self.replay.is_empty()),
        });
    }
    fn complete_dispatch(&mut self, id: u64, accepted: bool, now: Instant) {
        let Some(gate) = self.gate.as_mut().filter(|g| g.id == id) else {
            return;
        };
        gate.dispatching = false;
        if gate.bypass {
            if accepted {
                if let Some(event) = gate.original.as_mut() {
                    event.flags &= !ALTERNATE;
                }
            }
            gate.ready = true;
            gate.consumed = false;
            self.dead_key = false;
        } else if !accepted {
            gate.ready = true;
            gate.consumed |= gate.issued;
        }
        self.release(now);
    }
    fn timeout(&mut self, now: Instant) {
        while self
            .timeouts
            .front()
            .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(60))
        {
            self.timeouts.pop_front();
        }
        self.timeouts.push_back(now);
        self.abort(now);
        self.note(true, now);
        if self.timeouts.len() > 3 {
            self.enabled = false;
            self.message = "macOS repeatedly timed out the keyboard tap; interception is disabled. Pause and resume global typing to retry.";
        }
    }
}
fn is_function_key(key: &str) -> bool {
    key.strip_prefix('F')
        .and_then(|n| n.parse::<u8>().ok())
        .is_some_and(|n| (1..=20).contains(&n))
}

/// A CGKeyCode is u16. A fixed bitset avoids allocator work when consuming or
/// matching keyups, including unusual keycodes outside the usual hardware map.
struct SuppressedKeyUps([u64; 1024]);
impl Default for SuppressedKeyUps {
    fn default() -> Self {
        Self([0; 1024])
    }
}
impl SuppressedKeyUps {
    fn insert(&mut self, code: u16) {
        self.0[usize::from(code) / 64] |= 1u64 << (code % 64);
    }
    fn remove(&mut self, code: &u16) -> bool {
        let word = &mut self.0[usize::from(*code) / 64];
        let mask = 1u64 << (*code % 64);
        let present = *word & mask != 0;
        *word &= !mask;
        present
    }
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    #[cfg(test)]
    posted: Mutex<Vec<(u32, u16, u64, usize)>>,
}
impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        // Callback panics never cross FFI; recover poison to preserve queued
        // physical input rather than panic again and abandon its ownership.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn note(&self, user: bool) {
        self.lock().note(user, Instant::now());
        self.notify();
    }
    fn post(&self, event: Event) {
        #[cfg(test)]
        if event.native.0.is_null() {
            self.posted
                .lock()
                .unwrap()
                .push((event.kind, event.code, event.flags, event.serial));
            return;
        }
        event.post();
    }
    fn notify(&self) {
        let callback = {
            let mut state = self.lock();
            if !std::mem::take(&mut state.notify) {
                return;
            }
            state.callback.clone()
        };
        if let Some(callback) = callback {
            let _ = catch_unwind(AssertUnwindSafe(|| callback()));
        }
    }
    fn pump(&self) {
        {
            let mut state = self.lock();
            if state.driving {
                return;
            }
            state.driving = true;
        }
        loop {
            enum Work {
                Post(Event),
                Dispatch(u64, Action, Option<ShortcutHandler>),
                Done,
            }
            let work = {
                let mut state = self.lock();
                let now = Instant::now();
                state.expire(now);
                state.release(now);
                if state.holding() {
                    state.driving = false;
                    Work::Done
                } else if let Some(queued) = state.replay.pop_front() {
                    let event = queued.event;
                    let action = if queued.redispatch && event.kind == KEY_DOWN {
                        state.action(event.code, event.flags)
                    } else {
                        None
                    };
                    if let Some(action) = action {
                        let id = state.open(&action, event, now);
                        // The older replay tail must precede events arriving
                        // while this nested handler runs outside the mutex.
                        let tail = std::mem::take(&mut state.replay);
                        if let Some(gate) = state.gate.as_mut() {
                            gate.events
                                .extend(tail.into_iter().map(|queued| queued.event));
                        }
                        Work::Dispatch(id, action, state.handler.clone())
                    } else if state.ordinary(
                        event.kind,
                        event.code,
                        event.flags,
                        event.has_characters,
                        now,
                    ) {
                        Work::Post(event)
                    } else {
                        continue;
                    }
                } else {
                    state.driving = false;
                    Work::Done
                }
            };
            match work {
                Work::Post(event) => self.post(event),
                Work::Dispatch(id, action, handler) => {
                    let accepted = invoke(handler, action);
                    self.lock().complete_dispatch(id, accepted, Instant::now());
                }
                Work::Done => {
                    self.notify();
                    return;
                }
            }
            self.notify();
        }
    }
    fn dispatch(&self, id: u64, action: Action, handler: Option<ShortcutHandler>) {
        let accepted = invoke(handler, action);
        self.lock().complete_dispatch(id, accepted, Instant::now());
        self.pump();
    }
}
fn invoke(handler: Option<ShortcutHandler>, action: Action) -> bool {
    handler
        .is_some_and(|handler| catch_unwind(AssertUnwindSafe(|| handler(action))).unwrap_or(false))
}

struct Worker {
    thread: JoinHandle<()>,
    done: mpsc::Receiver<std::result::Result<(), String>>,
    started: bool,
}
/// The bridge owns this handle; the Quartz thread owns native taps and sources.
/// stop() only relinquishes ownership after native cleanup acknowledges and joins.
pub struct Input {
    shared: Arc<Shared>,
    worker: Option<Worker>,
}
impl Default for Input {
    fn default() -> Self {
        Self::new()
    }
}
impl Input {
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Shared::default()),
            worker: None,
        }
    }
    pub fn configure(&self, policy: Arc<ObservationPolicy>) {
        self.shared.lock().policy = Some(policy);
    }
    pub fn set_foreground(&self, foreground: Foreground) {
        let mut state = self.shared.lock();
        let previous = &state.foreground;
        let changed = previous.app_id != foreground.app_id
            || previous.executable != foreground.executable
            || previous.domain != foreground.domain
            || previous.window_id != foreground.window_id
            || previous.window_pid != foreground.window_pid
            || previous.document_id != foreground.document_id;
        state.foreground = foreground;
        if changed {
            state.dead_key = false;
            state.note(true, Instant::now());
        }
        drop(state);
        self.shared.notify();
    }
    /// Called only with a map precomputed on the AppKit owner thread.
    pub fn set_layout(&self, map: BTreeMap<u16, String>, source: InputSourceKind) {
        let mut state = self.shared.lock();
        state.layout = map;
        state.source = source;
        state.dead_key = false;
        state.note(true, Instant::now());
        drop(state);
        self.shared.notify();
    }
    pub fn start(&mut self, handler: ShortcutHandler) -> Result<()> {
        {
            let mut state = self.shared.lock();
            if state.stopping {
                bail!("Quartz input is stopping; stop must acknowledge before restart");
            }
            state.handler = Some(handler);
        }
        if let Some(worker) = self.worker.as_ref() {
            if !worker.started || worker.thread.is_finished() {
                bail!("Previous Quartz owner must be stopped and joined before starting again");
            }
            return Ok(());
        }
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        let shared = self.shared.clone();
        let thread = thread::Builder::new().name("brotypist-quartz".into()).spawn(move || {
            let result = catch_unwind(AssertUnwindSafe(|| event_loop(&shared, ready_tx)))
                .unwrap_or_else(|_| Err("Quartz owner thread panicked".into()));
            if result.is_err() {
                let mut state = shared.lock();
                state.enabled = false;
                state.message = "Quartz input unavailable; check Accessibility and Input Monitoring permissions and restart.";
            }
            let _ = done_tx.send(result);
        })?;
        self.worker = Some(Worker {
            thread,
            done: done_rx,
            started: false,
        });
        match ready_rx.recv_timeout(STOP_TIME) {
            Ok(Ok(())) => {
                if let Some(worker) = self.worker.as_mut() {
                    worker.started = true;
                }
                Ok(())
            }
            Ok(Err(message)) => Err(anyhow!(message)),
            Err(_) => bail!(
                "Quartz startup did not acknowledge within two seconds; native ownership is retained"
            ),
        }
    }
    pub fn set_enabled(&self, enabled: bool) {
        let mut state = self.shared.lock();
        state.enabled = enabled && !state.stopping;
        if !enabled {
            state.abort(Instant::now());
        }
        if enabled && !state.stopping {
            state.message = "Quartz shortcuts enabled";
        }
        drop(state);
        self.shared.pump();
    }
    pub fn set_callback(&self, callback: Option<InputCallback>) {
        self.shared.lock().callback = callback;
    }
    pub fn notifier(&self) -> Arc<dyn Fn(bool) + Send + Sync> {
        let shared = self.shared.clone();
        Arc::new(move |user| shared.note(user))
    }
    /// A background read ticket captures only this metadata atomic, not Input
    /// or any native handle. Layout/foreground notifications publish revisions.
    pub fn revision_reader(&self) -> Arc<dyn Fn() -> u64 + Send + Sync> {
        let revision = self.shared.lock().revision_signal.clone();
        Arc::new(move || revision.load(Ordering::Acquire))
    }
    pub fn clear_composition(&self) {
        self.shared.lock().dead_key = false;
    }
    /// Linearization point after authoritative focus/text validation and before
    /// the first irreversible AX setter or Unicode post. No lock spans OS work.
    pub fn commit(&self) -> bool {
        let mut state = self.shared.lock();
        if state.stopping {
            return false;
        }
        let now = Instant::now();
        let Some(gate) = state.gate.as_mut() else {
            return true;
        };
        if gate.aborted || gate.released || gate.ready || gate.committing || now >= gate.deadline {
            return false;
        }
        gate.issued = true;
        gate.committing = true;
        true
    }
    /// Records that the setter returned, but deliberately keeps input pinned
    /// through readback and runtime outcome publication.
    pub fn mutation_finished(&self) {
        if let Some(gate) = self.shared.lock().gate.as_mut() {
            gate.committing = false;
        }
    }
    /// `consumed` is true for Applied, Dispatched AND Indeterminate. Rejected
    /// before commit is the only outcome allowed to replay the initiating key.
    /// Once commit issued a mutation, even an erroneous false cannot double-edit.
    pub fn finish(&self, consumed: bool) -> Result<()> {
        let mut state = self.shared.lock();
        let now = Instant::now();
        if let Some(gate) = state.gate.as_mut() {
            gate.ready = true;
            gate.committing = false;
            gate.consumed = consumed || gate.issued;
            if now >= gate.deadline {
                gate.aborted = true;
                gate.overrun |= gate.issued;
            }
            if gate.released {
                state.gate = None;
            } else {
                state.release(now);
            }
        }
        drop(state);
        self.shared.pump();
        Ok(())
    }
    pub fn revision(&self) -> u64 {
        self.shared.lock().revision
    }
    pub fn user_revision(&self) -> u64 {
        self.shared.lock().user_revision
    }
    pub fn timestamp(&self) -> Option<Instant> {
        self.shared.lock().timestamp
    }
    pub fn dead_key(&self) -> bool {
        self.shared.lock().dead_key
    }
    pub fn source_kind(&self) -> InputSourceKind {
        self.shared.lock().source
    }
    pub fn note_input(&self, user: bool) {
        self.shared.note(user);
    }
    /// This helper must also be checked BEFORE commit by the bridge. It never
    /// requests permissions and never releases physical modifiers artificially.
    pub fn preflight_unicode(&self) -> Result<()> {
        if self.shared.lock().stopping {
            bail!("Quartz input is stopping");
        }
        // SAFETY: read-only Quartz state/preflight calls have no borrowed inputs.
        unsafe {
            if !CGPreflightPostEventAccess() {
                bail!("Posting keyboard input is not authorized");
            }
            if CGEventSourceFlagsState(0) & (COMMAND | CONTROL | ALTERNATE | FUNCTION) != 0 {
                bail!("Unicode insertion refused while a command modifier is held");
            }
        }
        Ok(())
    }
    /// Post preallocated down/up pairs, <=20 UTF-16 units each, with neither
    /// surrogate splitting nor allocation failures after the first post.
    /// The bridge must call commit() once immediately before this operation.
    pub fn post_unicode(&self, text: &str) -> Result<()> {
        if text.is_empty() || text.contains('\0') {
            bail!("Unicode insertion requires nonempty, NUL-free text");
        }
        self.preflight_unicode()?;
        let mut events = Vec::new();
        for chunk in unicode_chunks(text) {
            for down in [true, false] {
                // SAFETY: null source selects the default source; this returns
                // a new owned CF reference, checked before any mutation.
                let raw = unsafe { CGEventCreateKeyboardEvent(ptr::null_mut(), 0, down) };
                if raw.is_null() {
                    bail!("Quartz could not allocate a Unicode event");
                }
                let event = OwnedEvent(raw);
                // SAFETY: chunk is a live contiguous UTF-16 buffer and these
                // setters synchronously copy its contents into our owned event.
                unsafe {
                    CGEventSetFlags(raw, 0);
                    CGEventKeyboardSetUnicodeString(raw, chunk.len(), chunk.as_ptr());
                    CGEventSetIntegerValueField(raw, SOURCE_TAG, SYNTHETIC_TAG);
                }
                events.push(event);
            }
        }
        // Recheck after allocation and before the first irreversible post.
        self.preflight_unicode()?;
        for event in events {
            // SAFETY: all events are owned, tagged and fully initialized above.
            unsafe { CGEventPost(1, event.0) }
        }
        Ok(())
    }
    pub fn stop(&mut self) -> Result<()> {
        self.stop_with_timeout(STOP_TIME)
    }
    fn stop_with_timeout(&mut self, timeout: Duration) -> Result<()> {
        {
            let mut state = self.shared.lock();
            state.stopping = true;
            state.enabled = false;
            state.abort(Instant::now());
        }
        self.shared.pump();
        let Some(worker) = self.worker.as_ref() else {
            if self.shared.lock().holding() {
                bail!("Native mutation still owns deferred input");
            }
            return Ok(());
        };
        let result = match worker.done.recv_timeout(timeout) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => bail!(
                "Quartz stop did not acknowledge within two seconds; thread and event ownership are retained"
            ),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err("Quartz owner exited without cleanup acknowledgment".into())
            }
        };
        // Receipt is sent only after native sources, callbacks and taps are gone.
        // Joining cannot wait for any live native callback after that receipt.
        let worker = self
            .worker
            .take()
            .expect("worker retained until acknowledgment");
        worker
            .thread
            .join()
            .map_err(|_| anyhow!("Quartz owner thread panicked"))?;
        result.map_err(|message| anyhow!(message))
    }
    pub fn status(&self) -> Value {
        let state = self.shared.lock();
        json!({ "keyboard": state.message, "shortcuts_enabled": state.intercepting(),
            "input_source": state.source.label(), "dead_key": state.dead_key,
            "input_revision": state.revision, "user_input_revision": state.user_revision,
            "pending_input": state.holding(), "stopping": state.stopping,
            "thread_owned": self.worker.is_some() })
    }
}
impl Drop for Input {
    fn drop(&mut self) {
        if self.stop().is_err() {
            if let Some(worker) = self.worker.take() {
                // Never detach a still-running native owner in Drop. A reaper
                // retains its JoinHandle until cleanup acknowledgment/exit.
                let retained = Arc::new(Mutex::new(Some(worker)));
                let owner = retained.clone();
                if thread::Builder::new()
                    .name("brotypist-quartz-reaper".into())
                    .spawn(move || {
                        let worker = owner.lock().unwrap_or_else(|e| e.into_inner()).take();
                        if let Some(worker) = worker {
                            let _ = worker.thread.join();
                        }
                    })
                    .is_err()
                {
                    // If even spawning a reaper fails, retain the handle rather
                    // than implicitly detaching its still-live native owner.
                    std::mem::forget(retained);
                }
            }
        }
    }
}

struct UnicodeChunk {
    units: [u16; 20],
    length: usize,
}
impl std::ops::Deref for UnicodeChunk {
    type Target = [u16];
    fn deref(&self) -> &[u16] {
        &self.units[..self.length]
    }
}
fn unicode_chunks(text: &str) -> impl Iterator<Item = UnicodeChunk> + '_ {
    let mut characters = text.chars().peekable();
    std::iter::from_fn(move || {
        let mut chunk = UnicodeChunk {
            units: [0; 20],
            length: 0,
        };
        while let Some(character) = characters.peek().copied() {
            let width = character.len_utf16();
            if chunk.length + width > 20 {
                break;
            }
            characters.next();
            character.encode_utf16(&mut chunk.units[chunk.length..chunk.length + width]);
            chunk.length += width;
        }
        (chunk.length != 0).then_some(chunk)
    })
}

unsafe extern "C" fn keyboard_callback(
    _: Native,
    kind: u32,
    raw: Native,
    context: Native,
) -> Native {
    // SAFETY: event_loop retains the Arc allocation until both taps are
    // invalidated and run-loop sources removed on this same callback thread.
    let shared = unsafe { &*(context as *const Shared) };
    catch_unwind(AssertUnwindSafe(|| unsafe { receive(shared, kind, raw) })).unwrap_or_else(|_| {
        // Never unwind through C. Invalidate cached suggestions, and stop new
        // interception; an issued mutation still owns its queued physical input.
        let mut state = shared.lock();
        state.enabled = false;
        state.abort(Instant::now());
        state.note(true, Instant::now());
        raw
    })
}
unsafe fn receive(shared: &Shared, kind: u32, raw: Native) -> Native {
    if kind == TAP_TIMEOUT {
        shared.lock().timeout(Instant::now());
        shared.pump();
        return raw;
    }
    if kind == TAP_USER_DISABLED {
        return raw;
    } // owner loop restores requested state
    if raw.is_null() {
        return raw;
    }
    // SAFETY: raw is the callback's live borrowed CGEvent reference.
    if unsafe { CGEventGetIntegerValueField(raw, SOURCE_TAG) } == SYNTHETIC_TAG
        || unsafe { CGEventGetIntegerValueField(raw, SOURCE_PID) } == i64::from(std::process::id())
    {
        return raw;
    }
    // SAFETY: same live event; no AppKit translation or AX work is performed.
    let code = unsafe { CGEventGetIntegerValueField(raw, KEYCODE) } as u16;
    let flags = unsafe { CGEventGetFlags(raw) };
    let mut state = shared.lock();
    let now = Instant::now();
    let queue = state.holding() || state.driving || !state.replay.is_empty();
    let action = if !queue && kind == KEY_DOWN {
        state.action(code, flags)
    } else {
        None
    };
    if queue || action.is_some() {
        // SAFETY: copy is taken while the borrowed callback event is live.
        let event = unsafe { Event::copy(kind, raw) };
        if queue {
            state.enqueue(event, now);
            drop(state);
            shared.pump();
        } else if let Some(action) = action {
            let id = state.open(&action, event, now);
            let handler = state.handler.clone();
            drop(state);
            shared.dispatch(id, action, handler);
        }
        return ptr::null_mut();
    }
    let mut length = 0;
    let mut characters = [0u16; 4];
    if kind == KEY_DOWN {
        // SAFETY: stack output buffers match the requested four UTF-16 units.
        unsafe { CGEventKeyboardGetUnicodeString(raw, 4, &mut length, characters.as_mut_ptr()) };
    }
    let pass = state.ordinary(kind, code, flags, length != 0, now);
    drop(state);
    shared.notify();
    if pass { raw } else { ptr::null_mut() }
}
unsafe extern "C" fn pointer_callback(
    _: Native,
    kind: u32,
    raw: Native,
    context: Native,
) -> Native {
    // SAFETY: context lifetime is the same as the keyboard callback above.
    let shared = unsafe { &*(context as *const Shared) };
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if kind != TAP_TIMEOUT && kind != TAP_USER_DISABLED && !raw.is_null() {
            // SAFETY: raw is the live borrowed pointer event; tagged replay and
            // this process's own posts must not invalidate cached observations.
            if unsafe { CGEventGetIntegerValueField(raw, SOURCE_TAG) } == SYNTHETIC_TAG
                || unsafe { CGEventGetIntegerValueField(raw, SOURCE_PID) }
                    == i64::from(std::process::id())
            {
                return;
            }
        }
        let mut state = shared.lock();
        if kind == TAP_TIMEOUT || kind == TAP_USER_DISABLED || !state.tap_needed() {
            state.dead_key = false;
            state.note(true, Instant::now());
        }
        drop(state);
        shared.notify();
    }));
    raw // listen-only, never suppress or alter pointer input
}

/// All native resources are created, run and destroyed by this one thread.
fn event_loop(
    shared: &Arc<Shared>,
    ready: mpsc::SyncSender<std::result::Result<(), String>>,
) -> std::result::Result<(), String> {
    struct Resources {
        runloop: Native,
        taps: Vec<Native>,
        sources: Vec<Native>,
    }
    impl Drop for Resources {
        fn drop(&mut self) {
            // SAFETY: this destructor runs on the creating/run-loop owner thread
            // after callback execution has unwound. Every Create ref is released
            // exactly once, including partially initialized startup failures.
            unsafe {
                for source in self.sources.drain(..) {
                    CFRunLoopRemoveSource(self.runloop, source, kCFRunLoopCommonModes);
                    CFRelease(source);
                }
                for tap in self.taps.drain(..) {
                    CFMachPortInvalidate(tap);
                    CFRelease(tap);
                }
            }
        }
    }
    // SAFETY: context is the Arc's stable allocation and is retained by the
    // event_loop caller through Resources destruction; no native callback outlives it.
    unsafe {
        let context = Arc::as_ptr(shared) as Native;
        let runloop = CFRunLoopGetCurrent();
        let mut resources = Resources {
            runloop,
            taps: Vec::with_capacity(2),
            sources: Vec::with_capacity(2),
        };
        let mask = [KEY_DOWN, KEY_UP, FLAGS_CHANGED, 1, 2, 3, 4, 25, 26, 22]
            .into_iter()
            .fold(0, |mask, kind| mask | (1u64 << kind));
        let tap = CGEventTapCreate(1, 0, 0, mask, keyboard_callback, context);
        if tap.is_null() {
            let message = "Keyboard capture denied. Enable Accessibility and Input Monitoring for BroTypist, then restart.".to_owned();
            let _ = ready.send(Err(message.clone()));
            return Err(message);
        }
        resources.taps.push(tap);
        let pointer_mask = [1, 3, 25, 22]
            .into_iter()
            .fold(0, |mask, kind| mask | (1u64 << kind));
        let pointer = CGEventTapCreate(1, 1, 1, pointer_mask, pointer_callback, context);
        if pointer.is_null() {
            let message =
                "Pointer observation tap unavailable; keyboard capture was not started.".to_owned();
            let _ = ready.send(Err(message.clone()));
            return Err(message);
        }
        resources.taps.push(pointer);
        for &port in &resources.taps {
            let source = CFMachPortCreateRunLoopSource(ptr::null_mut(), port, 0);
            if source.is_null() {
                let message = "Quartz run-loop source unavailable".to_owned();
                let _ = ready.send(Err(message.clone()));
                return Err(message);
            }
            resources.sources.push(source);
            CFRunLoopAddSource(runloop, source, kCFRunLoopCommonModes);
        }
        let mut enabled = shared.lock().tap_needed();
        CGEventTapEnable(tap, enabled);
        CGEventTapEnable(pointer, true);
        shared.lock().message = "Quartz input ready";
        let _ = ready.send(Ok(()));
        loop {
            {
                let mut state = shared.lock();
                state.expire(Instant::now());
            }
            shared.pump();
            let state = shared.lock();
            let desired = state.tap_needed();
            let exit =
                state.stopping && !state.holding() && !state.driving && state.replay.is_empty();
            drop(state);
            if exit {
                break;
            }
            if desired != enabled {
                CGEventTapEnable(tap, desired);
                enabled = desired;
            }
            // Re-enable timeout/user-disabled notifications only according to
            // current intent, including notifications queued by our own disable.
            if enabled {
                CGEventTapEnable(tap, true);
            }
            CGEventTapEnable(pointer, true);
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.01, 1);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(kind: u32, code: u16, serial: usize) -> Event {
        Event {
            native: OwnedEvent(ptr::null_mut()),
            kind,
            code,
            flags: 0,
            has_characters: true,
            serial,
        }
    }
    fn open(state: &mut State, now: Instant) -> u64 {
        let id = state.open(&Action::Word, event(KEY_DOWN, 48, 0), now);
        state.complete_dispatch(id, true, now);
        id
    }
    #[test]
    fn range_selection_and_text_write_share_one_irreversible_gate() {
        let input = Input::new();
        open(&mut input.shared.lock(), Instant::now());
        assert!(input.commit());
        assert!(!input.commit()); // A simultaneous native write cannot enter.
        input.mutation_finished();
        assert!(input.commit()); // Fresh readback authorized the second setter.
        input.mutation_finished();
        input.finish(false).unwrap(); // Issued operations cannot replay Tab.
        assert!(input.shared.posted.lock().unwrap().is_empty());
    }
    fn finish(state: &mut State, consumed: bool, now: Instant) {
        let gate = state.gate.as_mut().unwrap();
        gate.ready = true;
        gate.consumed = consumed;
        gate.committing = false;
        state.release(now);
    }
    fn delivered(state: &mut State, now: Instant) -> Vec<usize> {
        let mut serials = Vec::new();
        while let Some(queued) = state.replay.pop_front() {
            let e = queued.event;
            if state.ordinary(e.kind, e.code, e.flags, e.has_characters, now) {
                serials.push(e.serial);
            }
        }
        serials
    }
    #[test]
    fn rejected_shortcut_replays_down_up_and_tail_exactly_once() {
        let now = Instant::now();
        let mut state = State::default();
        open(&mut state, now);
        assert_eq!(state.user_revision, 0);
        state.enqueue(event(KEY_UP, 48, 1), now);
        state.enqueue(event(KEY_DOWN, 0, 2), now);
        finish(&mut state, false, now);
        assert_eq!(delivered(&mut state, now), [0, 1, 2]);
        state.release(now);
        assert!(delivered(&mut state, now).is_empty());
        assert!(state.user_revision > 0);
    }
    #[test]
    fn accepted_shortcut_suppresses_only_its_matching_up() {
        let now = Instant::now();
        let mut state = State::default();
        open(&mut state, now);
        state.enqueue(event(KEY_UP, 48, 1), now);
        state.enqueue(event(FLAGS_CHANGED, 56, 2), now);
        state.enqueue(event(KEY_DOWN, 0, 3), now);
        state.enqueue(event(1, 0, 4), now);
        finish(&mut state, true, now);
        assert_eq!(delivered(&mut state, now), [2, 3, 4]);
        open(&mut state, now);
        finish(&mut state, true, now);
        assert!(!state.ordinary(KEY_UP, 48, 0, true, now));
        assert!(state.ordinary(KEY_DOWN, 48, 0, true, now));
        assert!(state.ordinary(KEY_UP, 48, 0, true, now));
    }
    #[test]
    fn precommit_limit_and_deadline_replay_without_allowing_late_commit() {
        for deadline in [false, true] {
            let now = Instant::now();
            let mut state = State::default();
            open(&mut state, now);
            for index in 1..=REPLAY_LIMIT {
                state.enqueue(event(KEY_DOWN, 0, index), now);
            }
            if deadline {
                state.expire(now + REPLAY_TIME);
            } else {
                state.enqueue(event(KEY_DOWN, 0, 257), now);
            }
            let expected = if deadline { 256 } else { 257 };
            assert_eq!(
                delivered(&mut state, now),
                (0..=expected).collect::<Vec<_>>()
            );
            assert!(state.gate.as_ref().unwrap().released);
            assert!(state.gate.as_ref().unwrap().aborted);
            assert!(!state.intercepting());
        }
    }
    #[test]
    fn issued_mutation_pins_all_following_input_and_never_replays_initiator() {
        let now = Instant::now();
        let mut state = State::default();
        open(&mut state, now);
        let gate = state.gate.as_mut().unwrap();
        gate.issued = true;
        gate.committing = true;
        for index in 1..=400 {
            state.enqueue(event(KEY_DOWN, 0, index), now);
        }
        state.expire(now + REPLAY_TIME);
        assert!(state.replay.is_empty());
        assert_eq!(state.gate.as_ref().unwrap().events.len(), 400);
        // False after issuance is uncertain, not proof of non-mutation.
        finish(&mut state, false, now + REPLAY_TIME);
        assert_eq!(delivered(&mut state, now), (1..=400).collect::<Vec<_>>());
        assert!(!state.enabled);
        assert!(state.gate.is_none());
    }
    #[test]
    fn nested_shortcut_is_published_before_its_tail_is_delivered() {
        let now = Instant::now();
        let mut state = State::default();
        open(&mut state, now);
        state.enqueue(event(KEY_UP, 48, 1), now);
        state.enqueue(event(KEY_DOWN, 48, 2), now);
        state.enqueue(event(KEY_DOWN, 0, 3), now);
        finish(&mut state, true, now);
        let up = state.replay.pop_front().unwrap().event;
        assert!(!state.ordinary(up.kind, up.code, up.flags, true, now));
        let next = state.replay.pop_front().unwrap();
        assert!(next.redispatch);
        let next_id = state.open(&Action::Word, next.event, now);
        let tail = state.replay.pop_front().unwrap().event;
        state.enqueue(tail, now);
        assert_eq!(
            state
                .gate
                .as_ref()
                .unwrap()
                .original
                .as_ref()
                .unwrap()
                .serial,
            2
        );
        assert_eq!(
            state.gate.as_ref().unwrap().events.front().unwrap().serial,
            3
        );
        state.complete_dispatch(next_id, false, now);
        assert_eq!(delivered(&mut state, now), [2, 3]);
    }
    #[test]
    fn pump_preserves_nested_fifo_when_new_input_arrives_during_dispatch() {
        let input = Input::new();
        let now = Instant::now();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let observations = seen.clone();
        let weak = Arc::downgrade(&input.shared);
        let handler: ShortcutHandler = Arc::new(move |_| {
            let shared = weak.upgrade().unwrap();
            let mut state = shared.lock();
            let serial = state
                .gate
                .as_ref()
                .unwrap()
                .original
                .as_ref()
                .unwrap()
                .serial;
            observations.lock().unwrap().push(serial);
            if serial == 2 {
                // This new physical event must follow the older tail (3).
                state.enqueue(event(KEY_DOWN, 1, 4), Instant::now());
            }
            true
        });
        let id = {
            let mut state = input.shared.lock();
            state.enabled = true;
            state.policy = Some(Arc::new(
                ObservationPolicy::new(&crate::config::Config::default()).unwrap(),
            ));
            state.layout.insert(48, "Tab".into());
            state.handler = Some(handler.clone());
            state.open(&Action::Word, event(KEY_DOWN, 48, 0), now)
        };
        input.shared.dispatch(id, Action::Word, Some(handler));
        {
            let mut state = input.shared.lock();
            state.enqueue(event(KEY_UP, 48, 1), now);
            state.enqueue(event(KEY_DOWN, 48, 2), now);
            state.enqueue(event(KEY_DOWN, 0, 3), now);
        }
        input.finish(true).unwrap();
        assert_eq!(*seen.lock().unwrap(), [0, 2]);
        assert!(input.shared.posted.lock().unwrap().is_empty());
        input.finish(false).unwrap();
        let serials: Vec<_> = input
            .shared
            .posted
            .lock()
            .unwrap()
            .iter()
            .map(|e| e.3)
            .collect();
        assert_eq!(serials, [2, 3, 4]);
        input.finish(false).unwrap();
        assert_eq!(input.shared.posted.lock().unwrap().len(), 3);
    }
    #[test]
    fn rolling_timeout_window_recovers_three_then_disables() {
        let now = Instant::now();
        let mut state = State {
            enabled: true,
            ..State::default()
        };
        for _ in 0..3 {
            state.timeout(now);
            assert!(state.enabled);
        }
        state.timeout(now);
        assert!(!state.enabled);
        state.enabled = true;
        state.timeout(now + Duration::from_secs(60));
        assert!(state.enabled);
        assert_eq!(state.timeouts.len(), 1);
    }
    #[test]
    fn modifiers_layout_and_option_tab_are_exact_and_cached() {
        let mut state = State {
            enabled: true,
            ..State::default()
        };
        state.policy = Some(Arc::new(
            ObservationPolicy::new(&crate::config::Config::default()).unwrap(),
        ));
        state.layout.insert(48, "Tab".into());
        state.layout.insert(10, "AboveTab".into());
        assert_eq!(state.action(48, ALTERNATE), Some(Action::Bypass));
        assert_eq!(state.action(48, FUNCTION), None);
        assert_eq!(state.action(10, CONTROL), Some(Action::Force));
        assert_eq!(state.action(10, CONTROL | ALTERNATE), None);
        state.layout.insert(10, "Q".into());
        assert_eq!(state.action(10, CONTROL), None);
    }
    #[test]
    fn composition_and_nonuser_notifications_preserve_revision_distinction() {
        let now = Instant::now();
        let mut state = State::default();
        state.ordinary(KEY_DOWN, 14, ALTERNATE, false, now);
        assert!(state.dead_key);
        state.ordinary(KEY_DOWN, 14, 0, true, now);
        assert!(!state.dead_key);
        let user = state.user_revision;
        state.note(false, now);
        assert_eq!(state.user_revision, user);
        assert_eq!(state.revision, user + 1);
    }
    #[test]
    fn unicode_chunks_never_split_surrogates_at_twenty_units() {
        for text in [
            "a".repeat(19) + "😀b",
            "😀".repeat(21),
            "a\r\n".into(),
            "".into(),
        ] {
            let chunks: Vec<_> = unicode_chunks(&text).collect();
            assert!(
                chunks
                    .iter()
                    .all(|chunk| chunk.len() <= 20 && !chunk.is_empty())
            );
            let reconstructed: String = chunks
                .iter()
                .map(|chunk| String::from_utf16(chunk).unwrap())
                .collect();
            assert_eq!(reconstructed, text);
        }
    }
    #[test]
    fn same_window_document_navigation_revokes_cached_input_authority() {
        let input = Input::new();
        let first = Foreground {
            app_id: "browser".into(),
            domain: "example.test".into(),
            window_id: "window".into(),
            document_id: "first-native-digest".into(),
            ..Foreground::default()
        };
        input.set_foreground(first.clone());
        let revision = input.revision();
        input.set_foreground(first.clone());
        assert_eq!(input.revision(), revision);
        input.set_foreground(Foreground {
            document_id: "second-native-digest".into(),
            ..first
        });
        assert_eq!(input.revision(), revision + 1);
        assert_eq!(input.user_revision(), revision + 1);
    }
    #[test]
    fn revision_ticket_observes_source_foreground_and_notification_changes() {
        let input = Input::new();
        let revision = input.revision_reader();
        assert_eq!(revision(), 0);
        input.set_layout(BTreeMap::new(), InputSourceKind::Ime);
        assert_eq!(revision(), 1);
        assert_eq!(input.source_kind(), InputSourceKind::Ime);
        input.set_foreground(Foreground {
            app_id: "editor".into(),
            ..Foreground::default()
        });
        assert_eq!(revision(), 2);
        input.notifier()(false);
        assert_eq!(revision(), 3);
        assert_eq!(input.user_revision(), 2);
    }
    #[test]
    fn stop_timeout_retains_owner_until_cleanup_acknowledges() {
        let mut input = Input::new();
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        let thread = thread::spawn(move || {
            release_rx.recv().unwrap();
            done_tx.send(Ok(())).unwrap();
            ack_tx.send(()).unwrap();
        });
        input.worker = Some(Worker {
            thread,
            done: done_rx,
            started: true,
        });
        assert!(input.stop_with_timeout(Duration::ZERO).is_err());
        assert!(input.worker.is_some());
        release_tx.send(()).unwrap();
        ack_rx.recv().unwrap();
        input.stop_with_timeout(Duration::ZERO).unwrap();
        assert!(input.worker.is_none());
    }
    #[test]
    fn stop_cannot_release_input_past_issued_mutation_even_after_setter_returns() {
        let mut input = Input::new();
        let now = Instant::now();
        {
            let mut state = input.shared.lock();
            open(&mut state, now);
            state.enqueue(event(KEY_DOWN, 0, 1), now);
        }
        assert!(input.commit());
        input.mutation_finished();
        assert!(input.stop_with_timeout(Duration::ZERO).is_err());
        assert!(input.shared.posted.lock().unwrap().is_empty());
        assert!(!input.commit());
        input.finish(false).unwrap();
        let serials: Vec<_> = input
            .shared
            .posted
            .lock()
            .unwrap()
            .iter()
            .map(|e| e.3)
            .collect();
        assert_eq!(serials, [1]); // initiating down can never repeat an issued edit
        input.stop_with_timeout(Duration::ZERO).unwrap();
    }
}
