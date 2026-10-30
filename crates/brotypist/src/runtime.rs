//! Main-thread native controller. Hooks only claim cached presentations and enqueue
//! transactions; model evaluation and optional-data preparation have owned workers.
use crate::{config::{self, Config}, core::{self, Authority, Snapshot, SnapshotKey, SuggestionState},
    features, models, platform::{self, Action, ContextRead, EditOutcome, NativeBridge},
    process_client::WorkerClient, providers::GenerationOptions, spelling::{SpellingResult, SpellingService},
    storage::HistoryStore, system, worker::WorkerEvent};
use anyhow::{Result, anyhow, ensure};
use serde_json::{Value, json};
use unicode_normalization::UnicodeNormalization;
use std::{cell::RefCell, collections::{BTreeMap, BTreeSet, VecDeque}, rc::Rc,
    sync::{Arc, Condvar, Mutex, atomic::{AtomicBool, AtomicU64, Ordering}, mpsc::{self, Receiver, SyncSender}},
    thread::{self, JoinHandle}, time::{Duration, Instant}};

#[derive(Clone, Debug)]
pub enum Command {
    Action(Action), ApplyConfig { config: Config, persist: bool },
    PresentationResult { key: SnapshotKey, authority: Authority, revision: u64, visible_text: String, mode: String },
    Pause { minutes: u32 }, Resume, ToggleApp { app: String, domain: String },
    SetModel { path: String, prompt_style: String, model_id: Option<String> },
    DownloadModel { id: String, accept_license: bool }, CancelDownload,
    DeleteHistory { app: Option<String>, domain: Option<String> }, HistoryCounts { unlock: bool },
    Statistics { period: String, group_by: String }, ClearStatistics, EnableDocs { token: String },
    Models, HistoryInfo, SetHistoryEnabled { enabled: bool }, SuspendForPreview, ResumeFromPreview,
    SuspendShortcuts { suspended: bool }, RetryNative,
}

/// Deterministic time boundary. Native input timestamps use the same monotonic epoch.
pub trait Clock { fn now(&self) -> Instant; }
pub struct MonotonicClock;
impl Clock for MonotonicClock { fn now(&self) -> Instant { Instant::now() } }

/// Consumer-owned inference boundary, also used by the isolated Welcome preview.
pub trait Worker {
    fn prepare(&mut self, config: &Config) -> Result<u64>;
    fn submit(&mut self, config: &Config, prefix: &str, suffix: &str, options: &GenerationOptions) -> Result<u64>;
    fn proceed(&mut self, id: u64) -> Result<()>;
    fn poll(&mut self) -> Vec<WorkerEvent>;
    fn cancel(&mut self) -> Result<()>;
    fn release(&mut self) -> Result<()>;
    fn set_idle_timeout(&mut self, seconds: u32);
    fn status(&self) -> Value;
    fn request_stop(&mut self) -> Result<()>;
    fn is_stopped(&mut self) -> bool;
}
impl Worker for WorkerClient {
    fn prepare(&mut self, c: &Config) -> Result<u64> { WorkerClient::prepare(self, c) }
    fn submit(&mut self, c: &Config, p: &str, s: &str, o: &GenerationOptions) -> Result<u64> { WorkerClient::submit(self, c, p, s, o) }
    fn proceed(&mut self, id: u64) -> Result<()> { WorkerClient::proceed(self, id) }
    fn poll(&mut self) -> Vec<WorkerEvent> { WorkerClient::poll(self) }
    fn cancel(&mut self) -> Result<()> { WorkerClient::cancel(self) }
    fn release(&mut self) -> Result<()> { WorkerClient::release(self) }
    fn set_idle_timeout(&mut self, s: u32) { WorkerClient::set_idle_timeout(self, s); }
    fn status(&self) -> Value { WorkerClient::status(self) }
    fn request_stop(&mut self) -> Result<()> { WorkerClient::request_stop(self) }
    fn is_stopped(&mut self) -> bool { WorkerClient::is_stopped(self) }
}

/// Storage is independently injected: deterministic tests never open a profile/keychain.
pub trait Storage: Send {
    fn unlock(&mut self) -> Result<()>;
    fn lock(&mut self);
    fn record(&mut self, id: &str, app: &str, domain: &str, text: &str, spans: &[(usize, usize)]) -> Result<()>;
    fn acceptance(&mut self, app: &str, text: &str, completion: bool) -> Result<()>;
    fn personalization(&self, app: &str, domain: &str, prefix: &str, strength: u32, allowed: &dyn Fn(&str, &str) -> bool) -> Result<String>;
    fn delete(&mut self, app: Option<&str>, domain: Option<&str>, unlock: bool) -> Result<usize>;
    fn counts(&self, unlock: bool) -> Result<Value>;
    fn info(&self) -> Result<Value>;
    fn statistics(&self, period: &str, group: &str) -> Result<Value>;
    fn clear_statistics(&mut self) -> Result<()>;
}
impl Storage for HistoryStore {
    fn unlock(&mut self) -> Result<()> { HistoryStore::unlock(self) }
    fn lock(&mut self) { HistoryStore::lock(self); }
    fn record(&mut self, id: &str, app: &str, domain: &str, text: &str, spans: &[(usize, usize)]) -> Result<()> {
        self.record_identified_at(id, app, domain, text, spans, chrono::Utc::now()).map(|_| ())
    }
    fn acceptance(&mut self, app: &str, text: &str, completion: bool) -> Result<()> { self.record_statistics(app, text, completion) }
    fn personalization(&self, app: &str, domain: &str, prefix: &str, strength: u32, allowed: &dyn Fn(&str, &str) -> bool) -> Result<String> {
        HistoryStore::personalization(self, app, domain, prefix, strength, allowed)
    }
    fn delete(&mut self, app: Option<&str>, domain: Option<&str>, unlock: bool) -> Result<usize> { self.delete_with_unlock(app, domain, unlock) }
    fn counts(&self, unlock: bool) -> Result<Value> { self.history_counts(unlock) }
    fn info(&self) -> Result<Value> { self.history_info() }
    fn statistics(&self, period: &str, group: &str) -> Result<Value> { HistoryStore::statistics(self, period_days(period)?, group) }
    fn clear_statistics(&mut self) -> Result<()> { HistoryStore::clear_statistics(self) }
}
fn period_days(period: &str) -> Result<u32> {
    match period { "today" | "day" => Ok(1), "week" => Ok(7), "month" => Ok(30), "year" => Ok(365), "all" => Ok(0), _ => Err(anyhow!("Unknown statistics period.")) }
}
pub type SharedStorage = Arc<Mutex<Box<dyn Storage>>>;
fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> { m.lock().unwrap_or_else(std::sync::PoisonError::into_inner) }
fn first(text: &str, n: usize) -> String { text.chars().take(n).collect() }
fn last(text: &str, n: usize) -> &str { let start = text.char_indices().rev().nth(n.saturating_sub(1)).map_or(0, |(i, _)| i); if n == 0 { "" } else { &text[start..] } }
fn domain(text: &str) -> String { text.to_lowercase().trim_end_matches('.').into() }
fn rules<'a>(c: &'a Config, app: &str, host: &str) -> impl Iterator<Item=&'a BTreeMap<String, Value>> {
    [c.app_rules.get(&format!("app:{app}")), c.app_rules.get(&format!("domain:{}", domain(host)))].into_iter().flatten()
}
fn rule_bool(c: &Config, app: &str, host: &str, name: &str, initial: bool) -> bool {
    rules(c, app, host).fold(initial, |value, rule| rule.get(name).and_then(Value::as_bool).unwrap_or(value))
}
fn excluded(c: &Config, app: &str, name: &str) -> bool {
    let app = core::casefold(app); let name = core::casefold(name);
    c.excluded_apps.iter().filter(|s| !(cfg!(target_os = "macos") && c.terminal_completion && terminal_exclusion(s))).any(|s| {
        let s = core::casefold(s); app.contains(&s) || name.contains(&s)
    })
}
fn terminal_exclusion(s: &str) -> bool { ["terminal", "iterm", "powershell", "cmd.exe", "windowsterminal", "konsole", "gnome-terminal", "kitty", "wezterm", "alacritty"].contains(&s.to_lowercase().as_str()) }
fn history_allowed(c: &Config, app: &str, host: &str) -> bool {
    if app.is_empty() || excluded(c, app, "") { return false; }
    if host.is_empty() && platform::policy::is_browser(&[app]) && c.app_rules.iter().any(|(key, rule)| key.starts_with("domain:") && rule.get("record_history") == Some(&Value::Bool(false))) { return false; }
    rule_bool(c, app, host, "record_history", c.record_history) && rule_bool(c, app, host, "enabled", c.enabled)
}
fn recording_configured(c: &Config) -> bool { c.record_history || c.app_rules.values().any(|r| r.get("record_history") == Some(&Value::Bool(true))) }

pub trait Spelling: Send + Sync {
    fn assess(&self, snapshot: &Snapshot) -> Option<SpellingResult>;
    fn invalidate(&self);
    fn close(&self) -> bool;
    fn is_stopped(&self) -> bool;
}
impl Spelling for SpellingService {
    fn assess(&self, s: &Snapshot) -> Option<SpellingResult> { SpellingService::assess(self, s) }
    fn invalidate(&self) { SpellingService::invalidate(self); }
    fn close(&self) -> bool { SpellingService::close(self) }
    fn is_stopped(&self) -> bool { SpellingService::is_stopped(self) }
}

/// Main-owner native readers. Clipboard/reference filtering and encrypted history
/// are prepared separately; these methods must obey the passed revocable guard.
pub trait Readers {
    fn context_ticket(&mut self, snapshot: &Snapshot, bridge: &mut dyn NativeBridge) -> Result<Option<Box<dyn platform::DetachedContextRead>>> { bridge.context_ticket(snapshot) }
    fn power(&mut self) -> system::PowerStatus;
    fn persist(&mut self, config: &Config) -> Result<()>;
}
pub struct NativeReaders;
impl Readers for NativeReaders {
    fn power(&mut self) -> system::PowerStatus { system::power_status() }
    fn persist(&mut self, c: &Config) -> Result<()> { config::save_config(c) }
}

/// Worker-local optional sources. Injection replaces external I/O only; the
/// production preparer still owns epoch, ticket, consent and result fences.
pub trait BackgroundReaders: Send + Sync {
    fn clipboard(&self, authorized: Arc<dyn Fn() -> bool + Send + Sync>) -> Result<String>;
    fn screen(&self, snapshot: &Snapshot, ticket: &dyn platform::DetachedContextRead, authorized: &dyn Fn() -> bool,
        max_chars: usize, accessibility: bool, screenshot: bool) -> Result<ContextRead>;
}
pub struct NativeBackgroundReaders;
impl BackgroundReaders for NativeBackgroundReaders {
    fn clipboard(&self, guard: Arc<dyn Fn() -> bool + Send + Sync>) -> Result<String> { system::clipboard_text_shared(guard) }
    fn screen(&self, snapshot: &Snapshot, ticket: &dyn platform::DetachedContextRead, guard: &dyn Fn() -> bool,
        max_chars: usize, accessibility: bool, screenshot: bool) -> Result<ContextRead> {
        system::screen_context_detached(snapshot, ticket, guard, max_chars, accessibility, screenshot)
    }
}

pub struct Preparation {
    pub token: u64, pub epoch: u64, pub snapshot: Snapshot, pub config: Config,
    pub mode: String, pub anchor: String, pub references: Vec<(String, String)>,
    pub context_ticket: Option<Box<dyn platform::DetachedContextRead>>,
    pub authorization_ticket: Option<Box<dyn platform::DetachedContextRead>>,
    pub hard_disabled_apps: BTreeSet<String>,
}
pub struct Prepared { pub token: u64, pub epoch: u64, pub options: GenerationOptions, pub context_status: String, pub warning: String, pub reference_expires: Option<Instant> }
pub struct ContextPreparation { pub token: u64, pub epoch: u64, pub snapshot: Snapshot, pub config: Config, pub ticket: Box<dyn platform::DetachedContextRead> }
pub struct ContextUpdate { pub epoch: u64, pub key: SnapshotKey, pub authority: Authority, pub status: String }
/// Latest-only asynchronous boundary; cancellation invalidates before any sensitive
/// source is read and after every wait. Implementations retain cleanup ownership.
pub trait Preparer {
    fn submit(&mut self, request: Preparation);
    fn prefetch(&mut self, request: ContextPreparation);
    fn context_updates(&mut self) -> Vec<ContextUpdate>;
    fn cancel(&mut self);
    fn poll(&mut self) -> Vec<Prepared>;
    fn request_stop(&mut self);
    fn is_stopped(&mut self) -> bool;
}
struct ScreenJob { token: u64, epoch: u64, snapshot: Snapshot, config: Config, ticket: Box<dyn platform::DetachedContextRead> }
struct PreparationState {
    pending: Option<Preparation>, results: VecDeque<Prepared>, stopped: bool,
    screen_pending: Option<ScreenJob>, screen_result: Option<(u64, u64, ContextRead, Instant)>,
    context_updates: VecDeque<ContextUpdate>,
}
pub struct AsyncPreparer {
    state: Arc<(Mutex<PreparationState>, Condvar)>,
    thread: Option<JoinHandle<()>>, screen_thread: Option<JoinHandle<()>>,
}
impl AsyncPreparer {
    pub fn new(storage: SharedStorage, epoch: Arc<AtomicU64>, readers: Arc<dyn BackgroundReaders>) -> Result<Self> {
        let state = Arc::new((Mutex::new(PreparationState { pending: None, results: VecDeque::new(), stopped: false, screen_pending: None, screen_result: None, context_updates: VecDeque::new() }), Condvar::new()));
        let shared = state.clone(); let screen_epoch = epoch.clone(); let screen_readers = readers.clone();
        let screen_thread = thread::Builder::new().name("brotypist-window-context".into()).spawn(move || loop {
            let job = {
                let mut s = locked(&shared.0);
                while s.screen_pending.is_none() && !s.stopped { s = shared.1.wait(s).unwrap_or_else(std::sync::PoisonError::into_inner); }
                if s.stopped { break; } s.screen_pending.take().expect("screen job")
            };
            let guard = || guard_epoch(&screen_epoch, job.epoch) && !locked(&shared.0).stopped;
            if !guard() { continue; }
            let result = screen_readers.screen(&job.snapshot, &*job.ticket, &guard, 2400, job.config.screen_context, job.config.screenshot_context)
                .unwrap_or_else(|_| ContextRead { text: String::new(), status: "error".into(), source: String::new() });
            let mut s = locked(&shared.0);
            if !s.stopped && guard_epoch(&screen_epoch, job.epoch) {
                s.context_updates.clear(); s.context_updates.push_back(ContextUpdate { epoch: job.epoch, key: job.snapshot.key(), authority: job.snapshot.authority(), status: result.status.clone() });
                s.screen_result = Some((job.token, job.epoch, result, Instant::now())); shared.1.notify_all();
            }
        })?;
        let shared = state.clone();
        let thread_result = thread::Builder::new().name("brotypist-writing-preparation".into()).spawn(move || loop {
            let request = {
                let mut s = locked(&shared.0);
                while s.pending.is_none() && !s.stopped { s = shared.1.wait(s).unwrap_or_else(std::sync::PoisonError::into_inner); }
                if s.stopped { break; } s.pending.take().expect("pending request")
            };
            let expected = request.epoch; let check_epoch = epoch.clone(); let check_state = shared.clone();
            let guard: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || guard_epoch(&check_epoch, expected) && !locked(&check_state.0).stopped);
            let source_guard = guard.clone();
            let authorization = request.authorization_ticket.map(|ticket| Arc::new(Mutex::new(ticket)));
            let private_guard: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
                if !source_guard() { return false; }
                let safe = authorization.as_ref().is_some_and(|ticket| locked(ticket).matches().unwrap_or(false));
                safe && source_guard()
            });
            if !guard() { continue; }
            let c = &request.config; let snapshot = &request.snapshot;
            let mut references = request.references; let mut status = "off".to_owned(); let mut expires = None;
            if c.screen_context || c.screenshot_context {
                let s = locked(&shared.0);
                let (mut s, _) = shared.1.wait_timeout_while(s, Duration::from_millis(250), |s| !s.stopped && guard_epoch(&epoch, expected)
                    && s.pending.is_none() && s.screen_result.as_ref().is_none_or(|(token, ep, _, _)| *token != request.token || *ep != expected))
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if !s.stopped && guard_epoch(&epoch, expected) {
                    status = "unavailable".into();
                    if s.screen_result.as_ref().is_some_and(|(token, ep, _, _)| *token == request.token && *ep == expected) {
                        let (_, _, read, at) = s.screen_result.take().expect("screen result");
                        status = if context_status(&read.status) { read.status } else { "error".into() };
                        if status == "ready" && ((read.source == "accessibility" && c.screen_context) || (read.source == "screenshot" && c.screenshot_context)) {
                            references.push((read.source, first(&read.text, 2400))); expires = Some(at + Duration::from_secs(2));
                        }
                    }
                }
            }
            if !guard() { continue; }
            if c.clipboard_context && private_guard() {
                match readers.clipboard(private_guard.clone()) {
                    Ok(text) if private_guard() && !text.is_empty() => { references.push(("clipboard".into(), first(&text, 4000))); status = "ready".into(); let until = Instant::now() + Duration::from_secs(2); expires = Some(expires.map_or(until, |at: Instant| at.min(until))); }
                    Ok(_) => {}, Err(_) => status = "error".into(),
                }
            }
            if !guard() { continue; }
            let mut warning = String::new();
            let source_allowed = |app: &str, host: &str| !request.hard_disabled_apps.contains(app) && history_allowed(c, app, host);
            let personalization = if c.personalization_strength != 0 && source_allowed(&snapshot.app_id, &snapshot.domain) && private_guard() {
                match locked(&storage).personalization(&snapshot.app_id, &snapshot.domain, snapshot.prefix(), c.personalization_strength, &|app, host| guard() && source_allowed(app, host)) {
                    Ok(text) if private_guard() => text, Ok(_) => String::new(), Err(_) => { warning = "Encrypted history is unavailable for personalization. Unlock your OS credential store or clear damaged history.".into(); String::new() }
                }
            } else { String::new() };
            if !guard() { continue; }
            let mut instructions = vec![c.custom_instructions.as_str()];
            for rule in rules(c, &snapshot.app_id, &snapshot.domain) { if let Some(s) = rule.get("custom_instructions").and_then(Value::as_str) { instructions.push(s); } }
            let options = GenerationOptions {
                instructions: first(&instructions.into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n"), 26000),
                context: crate::writing::reference_context(snapshot.prefix(), snapshot.selection_suffix(), &references, 8000),
                personalization, max_words: features::length_profile(&c.completion_length).1,
                alternatives: request.mode == "alternatives", anchor: request.anchor,
                utility: if request.mode == "synonyms" { features::synonym_prompt(snapshot.prefix(), snapshot.selection_suffix(), snapshot.selected_text()) } else { String::new() },
            };
            let mut s = locked(&shared.0);
            if guard_epoch(&epoch, request.epoch) && !s.stopped && s.pending.is_none() {
                s.results.clear(); s.results.push_back(Prepared { token: request.token, epoch: request.epoch, options, context_status: status, warning, reference_expires: expires });
                shared.1.notify_all();
            }
        });
        match thread_result {
            Ok(thread) => Ok(Self { state, thread: Some(thread), screen_thread: Some(screen_thread) }),
            Err(error) => { locked(&state.0).stopped = true; state.1.notify_all(); let _ = screen_thread.join(); Err(error.into()) }
        }
    }
}
fn guard_epoch(epoch: &AtomicU64, expected: u64) -> bool { epoch.load(Ordering::Acquire) == expected }
impl Preparer for AsyncPreparer {
    fn submit(&mut self, mut request: Preparation) {
        let mut s = locked(&self.state.0); if s.stopped { return; } s.screen_result = None;
        s.screen_pending = request.context_ticket.take().map(|ticket| ScreenJob { token: request.token, epoch: request.epoch, snapshot: request.snapshot.clone(), config: request.config.clone(), ticket });
        if s.screen_pending.is_none() { s.screen_result = Some((request.token, request.epoch, ContextRead { text: String::new(), status: "unavailable".into(), source: String::new() }, Instant::now())); }
        s.pending = Some(request); s.results.clear(); self.state.1.notify_all();
    }
    fn prefetch(&mut self, request: ContextPreparation) {
        let mut s = locked(&self.state.0); if s.stopped { return; }
        if s.pending.is_none() { s.screen_pending = Some(ScreenJob { token: request.token, epoch: request.epoch, snapshot: request.snapshot, config: request.config, ticket: request.ticket }); self.state.1.notify_all(); }
    }
    fn context_updates(&mut self) -> Vec<ContextUpdate> { locked(&self.state.0).context_updates.drain(..).collect() }
    fn cancel(&mut self) { let mut s = locked(&self.state.0); s.pending = None; s.screen_pending = None; s.screen_result = None; s.results.clear(); s.context_updates.clear(); self.state.1.notify_all(); }
    fn poll(&mut self) -> Vec<Prepared> { locked(&self.state.0).results.drain(..).collect() }
    fn request_stop(&mut self) { let mut s = locked(&self.state.0); s.stopped = true; s.pending = None; s.screen_pending = None; s.screen_result = None; s.results.clear(); s.context_updates.clear(); self.state.1.notify_all(); }
    fn is_stopped(&mut self) -> bool {
        for owner in [&mut self.thread, &mut self.screen_thread] {
            if owner.as_ref().is_some_and(JoinHandle::is_finished) { if let Some(t) = owner.take() { let _ = t.join(); } }
        } self.thread.is_none() && self.screen_thread.is_none()
    }
}
impl Drop for AsyncPreparer {
    fn drop(&mut self) {
        self.request_stop();
        // Explicit shutdown polls without blocking. Abandoned construction/owner
        // teardown still retains both native-reader lifetimes until they exit.
        for owner in [&mut self.thread, &mut self.screen_thread] { if let Some(thread) = owner.take() { let _ = thread.join(); } }
    }
}

#[derive(Clone)]
struct Claim { key: SnapshotKey, authority: Authority, revision: u64, epoch: u64, visible: String, mode: String, count: usize }
#[derive(Default)]
struct Armed { claims: Vec<Arc<Claim>>, origin: Option<Arc<(Snapshot, u64, u64)>>, active: bool, alternatives: bool, synonyms: bool, escape_passthrough: bool, inflight: bool }
struct Input { action: Action, claim: Option<Arc<Claim>>, origin: Option<Arc<(Snapshot, u64, u64)>>, transaction: bool }
#[derive(Clone)]
struct Lease { snapshot: Snapshot, epoch: u64, revision: u64 }
impl Lease { fn matches(&self, s: &Snapshot, epoch: u64) -> bool { self.epoch == epoch && self.snapshot.same_key(s) && self.snapshot.same_authority(s) } }
struct Request { id: u64, lease: Lease, generation: u64, mode: String, length: String, pending_text: Option<String>, final_: bool, proceeded: bool,
    reference_expires: Option<Instant>, provider_identity: Vec<String>, prompt_style: String }
struct PendingPreparation { token: u64, lease: Lease, mode: String, anchor: String, config: Config, native_pending: bool }
struct Choices { snapshot: Snapshot, values: Vec<String>, mode: String, start: usize, length: usize, anchored: bool }
struct PendingEdit { before: Snapshot, expected: Snapshot, text: String, start: usize, length: usize, remainder: Option<SuggestionState>, at: Instant, revision: u64, mode: String }
struct Session { key: Authority, id: String, text: String, authored: Vec<bool>, accepted: bool }
enum Record { History { id: String, app: String, domain: String, text: String, spans: Vec<(usize, usize)> }, Stats { app: String, text: String, completion: bool } }
struct Download { cancel: models::DownloadCancellation, thread: JoinHandle<()>, receiver: Receiver<Value> }

/// UI handle is intentionally !Send. Native bridge construction, poll and edits
/// remain on their toolkit owner thread. send never captures or evaluates text.
pub struct Controller { engine: Rc<RefCell<Engine>>, sender: SyncSender<Command>, receiver: Receiver<Command> }
impl Controller {
    pub fn start(mut config: Config) -> Result<Self> {
        config.validate()?;
        let storage: SharedStorage = Arc::new(Mutex::new(Box::new(HistoryStore::open()?)));
        let epoch = Arc::new(AtomicU64::new(1));
        let preparer = AsyncPreparer::new(storage.clone(), epoch.clone(), Arc::new(NativeBackgroundReaders))?;
        let spelling = (config.typo_suppression || config.typo_fixes).then(native_spelling);
        let mut engine = Engine::with_native_factory(config, Box::new(WorkerClient::new()?), storage,
            Box::new(preparer), spelling, Box::new(NativeReaders), Box::new(MonotonicClock), epoch, Box::new(platform::create_bridge))?;
        engine.set_spelling_factory(Box::new(native_spelling));
        Self::with_engine(engine)
    }
    pub fn with_engine(engine: Engine) -> Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(128);
        Ok(Self { engine: Rc::new(RefCell::new(engine)), sender, receiver })
    }
    pub fn send(&self, command: Command) -> Result<()> { self.sender.try_send(command).map_err(|_| anyhow!("Controller command queue is unavailable or full.")) }
    pub fn poll(&self) -> Vec<Value> {
        let mut engine = self.engine.borrow_mut();
        while let Ok(command) = self.receiver.try_recv() { engine.command(command); }
        engine.tick(); engine.drain_events()
    }
    pub fn status(&self) -> Value { self.engine.borrow().status() }
    pub fn config(&self) -> Config { self.engine.borrow().config.clone() }
    pub fn shutdown(&mut self) -> Result<bool> { let mut engine = self.engine.borrow_mut(); engine.begin_shutdown(); engine.shutdown_step() }
    /// Native menu tracking runs a nested toolkit loop, so this must disable
    /// hooks immediately rather than wait for the queued-command poll.
    pub fn suspend_shortcuts(&self, suspended: bool) -> Result<()> {
        let mut engine = self.engine.try_borrow_mut().map_err(|_| anyhow!("Controller is busy."))?;
        engine.suspend_shortcuts(suspended); Ok(())
    }
    pub fn native_event_source(&self) -> Option<(i32, bool)> { self.engine.borrow().bridge.as_ref().and_then(|bridge| bridge.native_event_source()) }
    pub fn bind_native_ui(&self, callback: Option<Arc<dyn Fn(String) + Send + Sync>>) { self.engine.borrow_mut().bind_native_ui(callback); }
    pub fn show_native_menu(&self, rows: Vec<(String, String, bool)>) -> Result<bool> { self.engine.borrow_mut().bridge.as_mut().map_or(Ok(false), |bridge| bridge.show_native_menu(rows)) }
    pub fn present_native(&self, anchor: &str, text: &str, paused: bool) -> Result<bool> { self.engine.borrow_mut().bridge.as_mut().map_or(Ok(false), |bridge| bridge.present_native(anchor, text, paused)) }
    pub fn hide_native(&self) -> Result<()> { self.engine.borrow_mut().bridge.as_mut().map_or(Ok(()), |bridge| bridge.hide_native()) }
}

pub type NativeFactory = Box<dyn FnMut(&Config) -> Result<Box<dyn NativeBridge>>>;

/// Injectable single-owner state machine. Tests drive tick with a logical clock
/// and real consumer contracts, without product OS mocks or a model sidecar.
pub struct Engine {
    pub config: Config, bridge: Option<Box<dyn NativeBridge>>, worker: Box<dyn Worker>, storage: SharedStorage,
    native_factory: NativeFactory, native_unavailable_reason: String,
    input_callback: Arc<dyn Fn() + Send + Sync>, native_ui_callback: Option<Arc<dyn Fn(String) + Send + Sync>>,
    preparer: Box<dyn Preparer>, spelling: Option<Arc<dyn Spelling>>, readers: Box<dyn Readers>, clock: Box<dyn Clock>,
    epoch: Arc<AtomicU64>, armed: Arc<Mutex<Armed>>, inputs: Receiver<Input>, input_pending: Arc<AtomicBool>,
    shortcut_callback: platform::ShortcutHandler, hooks_started: bool, native_ready: bool,
    events: VecDeque<Value>, candidate: Option<Snapshot>, candidate_revision: u64, candidate_since: Instant,
    capture_after: Instant, input_revision: u64, suggestion: SuggestionState, request: Option<Request>, preparing: Option<PendingPreparation>, serial: u64,
    choices: Option<Choices>, correction: Option<(Snapshot, SpellingResult)>, claims: Vec<Arc<Claim>>,
    attempted: Option<(SnapshotKey, Authority, String)>, failed_correction: Option<(SnapshotKey, Authority)>,
    pending_edit: Option<PendingEdit>, completion_counted: bool, temporary: BTreeMap<String, (bool, Instant)>, escape_app: String,
    advance: Option<(SnapshotKey, Authority, Snapshot, String)>,
    redraw_pending: bool,
    uncertain_provenance: Option<(Authority, u64)>,
    paused_until: Instant, timed_pause: bool, field_pause: Option<(Authority, Instant)>, app: String, host: String, browser_apps: BTreeSet<String>,
    context_status: String, error: String, records: VecDeque<Record>, session: Option<Session>, session_serial: u64,
    context_prefetched: Option<(SnapshotKey, Authority, u64, Instant)>,
    history_suppressed: Option<(Authority, String)>, on_battery: bool, power_after: Instant,
    resident: Option<Vec<String>>, model_activity: Option<(u64, Authority, u64, usize)>, docs: Option<(Value, platform::Foreground, u64)>, docs_after: Instant,
    download: Option<Download>, download_status: Option<Value>, spelling_cleanup: Vec<JoinHandle<()>>,
    spelling_factory: Option<Box<dyn Fn() -> Arc<dyn Spelling>>>,
    preview: bool, shortcuts_suspended: bool, stopping: bool, done: BTreeSet<&'static str>,
}
impl Engine {
    #[allow(clippy::too_many_arguments)]
    pub fn new(config: Config, bridge: Box<dyn NativeBridge>, worker: Box<dyn Worker>, storage: SharedStorage,
        preparer: Box<dyn Preparer>, spelling: Option<Arc<dyn Spelling>>, readers: Box<dyn Readers>, clock: Box<dyn Clock>, epoch: Arc<AtomicU64>) -> Result<Self> {
        Self::build(config, Some(bridge), worker, storage, preparer, spelling, readers, clock, epoch, Box::new(platform::create_bridge), String::new())
    }
    /// Construction failure is an explicit absence, never a writable fallback.
    /// The factory stays on the native owner thread and runs again only on an
    /// explicit RetryNative/ApplyConfig operation.
    #[allow(clippy::too_many_arguments)]
    pub fn with_native_factory(mut config: Config, worker: Box<dyn Worker>, storage: SharedStorage,
        preparer: Box<dyn Preparer>, spelling: Option<Arc<dyn Spelling>>, readers: Box<dyn Readers>, clock: Box<dyn Clock>,
        epoch: Arc<AtomicU64>, mut factory: NativeFactory) -> Result<Self> {
        config.validate()?;
        let (bridge, reason) = match factory(&config) { Ok(bridge) => (Some(bridge), String::new()), Err(error) => (None, error.to_string()) };
        Self::build(config, bridge, worker, storage, preparer, spelling, readers, clock, epoch, factory, reason)
    }
    #[allow(clippy::too_many_arguments)]
    fn build(mut config: Config, mut bridge: Option<Box<dyn NativeBridge>>, mut worker: Box<dyn Worker>, storage: SharedStorage,
        preparer: Box<dyn Preparer>, spelling: Option<Arc<dyn Spelling>>, readers: Box<dyn Readers>, clock: Box<dyn Clock>, epoch: Arc<AtomicU64>,
        native_factory: NativeFactory, mut native_unavailable_reason: String) -> Result<Self> {
        config.validate()?; worker.set_idle_timeout(config.idle_release_seconds);
        let configured = if let Some(bridge) = &mut bridge {
            match bridge.configure(&config) { Ok(()) => true, Err(_) => { native_unavailable_reason = "Native configuration could not be applied.".into(); false } }
        } else { false };
        let spelling = spelling.or_else(|| (config.typo_suppression || config.typo_fixes).then(|| Arc::new(SpellingService::new(None, None)) as Arc<dyn Spelling>));
        let now = clock.now(); let armed = Arc::new(Mutex::new(Armed::default())); let input_pending = Arc::new(AtomicBool::new(false));
        let (sender, inputs) = mpsc::sync_channel(32);
        let cache = armed.clone(); let hook_epoch = epoch.clone();
        let shortcut_callback: platform::ShortcutHandler = Arc::new(move |action| {
            let Ok(mut cache) = cache.try_lock() else { return false; };
            let claim_index = match &action {
                Action::Word | Action::All => primary_claim_index(&cache.claims, false),
                Action::Choice(index) => cache.claims.iter().position(|c| matches!(c.mode.as_str(), "emoji" | "synonyms" | "alternatives") && *index < c.count),
                Action::Dismiss | Action::Bypass => primary_claim_index(&cache.claims, true), _ => None,
            };
            let claim = claim_index.map(|i| cache.claims[i].clone());
            let mut origin = None;
            let consume = match action {
                Action::Word | Action::All | Action::Choice(_) => claim.is_some(),
                Action::Bypass => true,
                Action::Dismiss => claim.is_some() && !cache.escape_passthrough,
                Action::Force | Action::Alternatives | Action::Synonyms | Action::AppToggle => {
                    if !cache.active || (action == Action::Alternatives && !cache.alternatives) || (action == Action::Synonyms && !cache.synonyms) { return false; }
                    origin = cache.origin.clone(); origin.is_some()
                }
                Action::Toggle | Action::Settings | Action::Quit => true,
            };
            if !consume && !(action == Action::Dismiss && (claim.is_some() || cache.inflight)) { return false; }
            let transaction = consume && action != Action::Bypass;
            if sender.try_send(Input { action: action.clone(), claim, origin, transaction }).is_err() { return false; }
            cache.claims.clear();
            if matches!(action, Action::Dismiss | Action::Bypass) { hook_epoch.fetch_add(1, Ordering::AcqRel); }
            consume
        });
        let hooks_started = configured && bridge.as_mut().is_some_and(|bridge| bridge.start_shortcuts(shortcut_callback.clone()).is_ok());
        if configured && !hooks_started { native_unavailable_reason = "Native shortcuts could not start. Check permissions, then retry.".into(); }
        let cache = armed.clone(); let wake = input_pending.clone(); let input_epoch = epoch.clone();
        let input_callback: Arc<dyn Fn() + Send + Sync> = Arc::new(move || { wake.store(true, Ordering::Release); input_epoch.fetch_add(1, Ordering::AcqRel); if let Ok(mut c) = cache.try_lock() { c.claims.clear(); } });
        if let Some(bridge) = &mut bridge { bridge.set_input_callback(Some(input_callback.clone())); bridge.set_shortcuts_enabled(hooks_started); }
        let mut engine = Self { config, bridge, worker, storage, preparer, spelling, readers, clock, epoch, armed, inputs, input_pending,
            shortcut_callback, hooks_started, native_ready: hooks_started,
            native_factory, native_unavailable_reason, input_callback, native_ui_callback: None,
            events: VecDeque::new(), candidate: None, candidate_revision: 0, candidate_since: now, capture_after: now, input_revision: 0,
            suggestion: SuggestionState::new(), request: None, preparing: None, serial: 0, choices: None, correction: None, claims: Vec::new(),
            advance: None,
            redraw_pending: false,
            attempted: None, failed_correction: None, pending_edit: None, completion_counted: false, temporary: BTreeMap::new(), escape_app: String::new(),
            uncertain_provenance: None,
            paused_until: now, timed_pause: false, field_pause: None, app: String::new(), host: String::new(), browser_apps: BTreeSet::new(), context_status: "off".into(), error: String::new(),
            records: VecDeque::new(), session: None, session_serial: 0, history_suppressed: None, on_battery: false, power_after: now,
            resident: None, model_activity: None, docs: None, docs_after: now, download: None, download_status: None, spelling_cleanup: Vec::new(),
            spelling_factory: None,
            context_prefetched: None,
            preview: false, shortcuts_suspended: false, stopping: false, done: BTreeSet::new() };
        if recording_configured(&engine.config) { let result = locked(&engine.storage).unlock(); if result.is_err() { engine.error = "Encrypted history could not be unlocked.".into(); } }
        if !engine.hooks_started { engine.fail("Native shortcuts startup; check permissions and retry"); }
        engine.refresh_arming(); Ok(engine)
    }
    fn bind_native_ui(&mut self, callback: Option<Arc<dyn Fn(String) + Send + Sync>>) {
        self.native_ui_callback = callback;
        if let Some(bridge) = &mut self.bridge { bridge.bind_native_ui(self.native_ui_callback.clone()); }
    }
    fn secure_input(&self) -> Option<bool> { self.bridge.as_ref().and_then(|bridge| bridge.secure_input_enabled()) }
    fn user_revision(&self) -> Option<u64> { self.bridge.as_ref().map(|bridge| bridge.user_input_revision()) }
    fn native_input_revision(&self) -> Option<u64> { self.bridge.as_ref().map(|bridge| bridge.input_revision()) }
    fn input_claim_valid(&self) -> bool { self.bridge.as_ref().is_none_or(|bridge| bridge.input_claim_valid()) }
    fn set_native_shortcuts(&mut self, enabled: bool) { if let Some(bridge) = &mut self.bridge { bridge.set_shortcuts_enabled(enabled); } }
    fn finish_input(&mut self, accepted: bool) -> Result<()> { self.bridge.as_mut().ok_or_else(|| anyhow!("Native input is unavailable."))?.finish_input(accepted) }
    pub fn set_spelling_factory(&mut self, factory: Box<dyn Fn() -> Arc<dyn Spelling>>) { self.spelling_factory = Some(factory); }
    fn now(&self) -> Instant { self.clock.now() }
    fn active(&self) -> bool { !self.stopping && !self.preview && self.config.global_enabled && self.now() >= self.paused_until }
    fn current_epoch(&self) -> u64 { self.epoch.load(Ordering::Acquire) }
    fn bump_epoch(&self) { self.epoch.fetch_add(1, Ordering::AcqRel); }
    fn emit(&mut self, event: Value) { if self.events.len() == 128 { self.events.pop_front(); } self.events.push_back(event); }
    pub fn drain_events(&mut self) -> Vec<Value> { self.events.drain(..).collect() }
    fn fail(&mut self, operation: &str) { self.error = format!("{operation} could not be completed."); self.emit(json!({"type":"error","operation":operation,"message":self.error})); }
    fn temporary_value(&self, scope: &str) -> Option<bool> { self.temporary.get(scope).filter(|(_, at)| self.now() < *at).map(|(value, _)| *value) }
    fn allowed(&self, app: &str, host: &str, name: &str) -> bool {
        if app.is_empty() || app == self.escape_app || excluded(&self.config, app, name) { return false; }
        let app_override = self.temporary_value(&format!("app:{app}")); if app_override == Some(false) { return false; }
        let enabled = self.config.app_rules.get(&format!("app:{app}")).and_then(|r| r.get("enabled")).and_then(Value::as_bool).unwrap_or(self.config.enabled);
        let enabled = app_override.unwrap_or(enabled);
        let enabled = self.config.app_rules.get(&format!("domain:{}", domain(host))).and_then(|r| r.get("enabled")).and_then(Value::as_bool).unwrap_or(enabled);
        self.temporary_value(&format!("domain:{}", domain(host))).unwrap_or(enabled)
    }
    fn record_allowed(&self, app: &str, host: &str) -> bool {
        if host.is_empty() && self.browser_apps.contains(app) && self.config.app_rules.iter().any(|(k, r)| k.starts_with("domain:") && r.get("record_history") == Some(&Value::Bool(false))) { return false; }
        self.allowed(app, host, "") && rule_bool(&self.config, app, host, "record_history", self.config.record_history)
    }
    fn native_config(&self) -> Config {
        let mut config = self.config.clone();
        for (scope, (enabled, at)) in &self.temporary { if self.now() < *at { config.app_rules.entry(scope.clone()).or_default().insert("enabled".into(), json!(enabled)); } }
        if !self.escape_app.is_empty() { config.app_rules.entry(format!("app:{}", self.escape_app)).or_default().insert("enabled".into(), json!(false)); }
        if !self.active() { config.global_enabled = false; }
        config
    }
    fn configure_native(&mut self) {
        self.native_ready = false; self.disarm(); self.set_native_shortcuts(false);
        let config = self.native_config();
        let Some(bridge) = &mut self.bridge else { self.refresh_arming(); return; };
        if bridge.configure(&config).is_err() { self.clear(true, true); self.fail("Native configuration"); }
        else { self.native_ready = self.hooks_started; }
        self.set_native_shortcuts(self.native_ready && !self.shortcuts_suspended && !self.stopping);
        self.refresh_arming();
    }
    fn foreground(&mut self) -> Option<platform::Foreground> {
        let mut info = self.bridge.as_mut()?.foreground().ok().flatten()?; info.domain = domain(&info.domain);
        if !self.escape_app.is_empty() && info.app_id != self.escape_app { self.escape_app.clear(); self.configure_native(); }
        self.app = info.app_id.clone(); self.host = info.domain.clone();
        let browser = platform::policy::is_browser(&[&info.app_id, &info.executable]);
        if browser { self.browser_apps.insert(info.app_id.clone()); }
        if browser && info.domain.is_empty() && self.config.app_rules.iter().any(|(k, r)| k.starts_with("domain:") && r.get("enabled") == Some(&Value::Bool(false))) { return None; }
        self.allowed(&info.app_id, &info.domain, &info.executable).then_some(info)
    }
    fn capture(&mut self) -> Option<Snapshot> {
        if !self.native_ready || !self.active() || self.secure_input() != Some(false) { return None; }
        let before = self.foreground()?;
        if !self.native_ready || self.secure_input() != Some(false) { return None; }
        let revision = self.user_revision()?;
        let mut snapshot = self.bridge.as_mut()?.capture().ok().flatten()?;
        let after = self.foreground()?;
        if snapshot.protected || !snapshot.valid_range() || !self.native_ready || !self.active() || self.secure_input() != Some(false)
            || before.app_id != after.app_id || before.domain != after.domain || before.window_id != after.window_id || before.window_pid != after.window_pid || before.document_id != after.document_id
            || snapshot.app_id != after.app_id || domain(&snapshot.domain) != after.domain
            || (!after.window_id.is_empty() && snapshot.window_id != after.window_id)
            || !self.allowed(&snapshot.app_id, &snapshot.domain, "") || Some(revision) != self.user_revision() { return None; }
        if snapshot.inline_render.as_ref().is_some_and(|r| r.source.contains("+vision")) && !self.config.screenshot_appearance { snapshot.inline_render = None; }
        self.candidate_revision = revision; Some(snapshot)
    }
    fn disarm(&mut self) { self.claims.clear(); if let Ok(mut a) = self.armed.try_lock() { a.claims.clear(); } }
    fn clear_docs(&mut self) { if self.docs.take().is_some() { self.emit(json!({"type":"docs_help","hint":null})); } }
    fn clear(&mut self, hide: bool, cancel: bool) {
        self.bump_epoch(); if cancel { let _ = self.worker.cancel(); }
        self.preparer.cancel(); self.preparing = None; self.request = None; self.suggestion.invalidate(); self.choices = None; self.correction = None;
        self.pending_edit = None; self.context_prefetched = None;
        self.advance = None; self.redraw_pending = false;
        if let Some(s) = &self.spelling { s.invalidate(); }
        self.disarm(); self.clear_docs(); self.context_status = if self.config.screen_context || self.config.screenshot_context || self.config.clipboard_context { "blocked" } else { "off" }.into();
        if let Some(bridge) = &mut self.bridge { let _ = bridge.hide_native(); } self.emit(json!({"type":"clear","hide_accessory":hide}));
    }
    fn attempted(&self, s: &Snapshot, mode: &str) -> bool { self.attempted.as_ref().is_some_and(|(key, authority, m)| *key == s.key() && *authority == s.authority() && m == mode) }
    fn mark_attempt(&mut self, s: &Snapshot, mode: &str) { self.attempted = Some((s.key(), s.authority(), mode.into())); }
    fn field_paused(&self, s: &Snapshot) -> bool { self.field_pause.as_ref().is_some_and(|(a, at)| *a == s.authority() && self.now() < *at) }
    fn observe(&mut self, snapshot: Option<Snapshot>) {
        let Some(snapshot) = snapshot else {
            if self.candidate.is_some() || self.preparing.is_some() || self.request.is_some() { self.finish_session(); self.clear(true, true); }
            self.candidate = None; self.attempted = None; self.pending_edit = None; return;
        };
        self.observe_pending_edit(&snapshot);
        if self.uncertain_provenance.as_ref().is_some_and(|(authority,_)| *authority != snapshot.authority()) { self.uncertain_provenance = None; }
        if snapshot.selection_length != 0 { self.finish_session(); } else if snapshot.eligible(1) { self.observe_session(&snapshot); }
        let previous = self.candidate.clone();
        if previous.as_ref().is_some_and(|p| p.same_key(&snapshot) && p.same_authority(&snapshot)) {
            let changed = previous.as_ref().is_some_and(|p| p != &snapshot);
            self.candidate = Some(snapshot.clone());
            if changed || self.redraw_pending || self.claims.iter().any(|c| c.revision != self.candidate_revision) {
                self.disarm();
                self.redraw_pending = false;
                if self.correction.is_some() { self.publish_correction_geometry(&snapshot); }
                else if !self.suggestion.text.is_empty() { self.suggestion.snapshot = Some(snapshot.clone()); self.publish_completion(); }
                else if self.choices.is_some() { if let Some(c) = &mut self.choices { c.snapshot = snapshot.clone(); } self.publish_choices(); }
                self.publish_caret(&snapshot);
            }
            return;
        }
        self.bump_epoch(); self.attempted = None; self.failed_correction = None;
        let now = self.now(); self.candidate_since = now;
        if previous.as_ref().is_some_and(|p| p.app_id == snapshot.app_id && p.field_id == snapshot.field_id) {
            if let Some(at) = self.bridge.as_ref().and_then(|bridge| bridge.input_timestamp()) { if at <= now && now.duration_since(at) <= Duration::from_millis(200) { self.candidate_since = at; } }
        }
        let advance = self.correction.is_none() && self.suggestion.advance(&snapshot);
        self.candidate = Some(snapshot.clone());
        if advance { let _ = self.worker.cancel(); self.request = None; self.preparing = None; self.preparer.cancel(); self.disarm(); self.publish_completion(); }
        else { self.clear(false, true); }
        self.publish_caret(&snapshot);
    }
    fn publish_caret(&mut self, s: &Snapshot) {
        if s.has_anchor() { self.emit(json!({"type":"caret","rect":s.caret_rect,"field_rect":s.field_rect,"native_anchor":s.native_anchor})); }
        else { self.emit(json!({"type":"clear","hide_accessory":true})); }
    }
    fn presentation_event(&self, s: &Snapshot, text: &str, mode: &str, choices: &[String]) -> Value {
        json!({"type":"suggestion","key":s.key(),"authority":s.authority(),"revision":self.candidate_revision,"text":text,"mode":mode,
            "rect":s.anchor_rect(),"field_rect":s.field_rect,"field_anchor":s.caret_rect.is_none(),"native_anchor":s.native_anchor,
            "inline":mode == "completion" || mode == "correction","render":s.inline_render,"choices":choices})
    }
    fn publish_completion(&mut self) {
        self.claims.retain(|c| c.mode != "completion"); self.refresh_arming();
        let Some(s) = self.suggestion.snapshot.clone() else { return; };
        let fallback = !s.inline_safe() && self.advance.as_ref().is_some_and(|(key,authority,geometry,_)| *key == s.key() && *authority == s.authority() && geometry.inline_safe());
        if !fallback { self.advance = None; }
        let geometry = if fallback { &self.advance.as_ref().expect("verified advance").2 } else { &s };
        if self.suggestion.text.is_empty() || !geometry.inline_safe() { self.emit(json!({"type":"clear"})); return; }
        let choices = self.choices.as_ref().filter(|c| c.anchored && c.snapshot.same_key(&s) && c.snapshot.same_authority(&s)).map_or(&[][..], |c| c.values.as_slice());
        let mut event = self.presentation_event(&s, &self.suggestion.text, "completion", choices);
        event["rect"] = json!(geometry.anchor_rect()); event["field_rect"] = json!(geometry.field_rect);
        event["native_anchor"] = json!(geometry.native_anchor); event["render"] = json!(geometry.inline_render);
        if let Some((_,_,_,accepted)) = &self.advance { event["advance"] = json!(accepted); }
        if !choices.is_empty() { event["choices_mode"] = json!("alternatives"); }
        self.emit(event);
    }
    fn publish_choices(&mut self) {
        self.claims.retain(|c| !matches!(c.mode.as_str(), "alternatives" | "synonyms" | "emoji")); self.refresh_arming();
        let Some(c) = &self.choices else { return; };
        if c.anchored { self.publish_completion(); return; }
        if !c.snapshot.has_anchor() || c.values.is_empty() { return; }
        let event = self.presentation_event(&c.snapshot, &c.values[0], &c.mode, &c.values); self.emit(event);
    }
    fn set_choices(&mut self, s: Snapshot, mut values: Vec<String>, mode: &str, start: usize, length: usize, anchored: bool) {
        if (mode == "emoji" || mode == "synonyms" || length != 0 || start != s.caret) && !self.can_replace(&s) { return; }
        values.truncate(5); if values.is_empty() { return; }
        if !anchored { self.suggestion.invalidate(); self.completion_counted = false; self.disarm(); }
        self.choices = Some(Choices { snapshot: s, values, mode: mode.into(), start, length, anchored }); self.publish_choices();
    }
    fn can_replace(&self, s: &Snapshot) -> bool { s.can_replace && self.bridge.as_ref().is_some_and(|bridge| bridge.atomic_replacement_supported()) }
    fn assess(&self, s: &Snapshot) -> Option<SpellingResult> { self.spelling.as_ref().and_then(|service| service.assess(s)).filter(|r| r.matches(s)) }
    fn typo_active(&self, s: &Snapshot) -> bool { self.config.typo_suppression || (self.config.typo_fixes && self.can_replace(s)) }
    fn completion_blocked(&self, s: &Snapshot) -> Option<bool> {
        if !self.typo_active(s) { return Some(false); }
        if self.correction.as_ref().is_some_and(|(before, _)| before.same_key(s) && before.same_authority(s)) { return Some(true); }
        self.assess(s).map(|r| r.suspected && self.config.typo_suppression)
    }
    fn show_correction(&mut self, s: &Snapshot, result: SpellingResult) -> bool {
        if !self.config.typo_fixes || result.replacement.is_empty() || !self.can_replace(s) || !result.matches(s) { return false; }
        if self.attempted(s, "correction") && self.correction.is_none() { return false; }
        if self.correction.as_ref().is_some_and(|(before, _)| before.same_key(s) && before.same_authority(s)) { return true; }
        if self.failed_correction.as_ref().is_some_and(|(k,a)| *k == s.key() && *a == s.authority()) { return false; }
        let rect = self.bridge.as_mut().and_then(|bridge| bridge.range_rect(s, result.start, result.length).ok().flatten()).filter(valid_rect);
        if rect.is_none() || !s.inline_safe() || s.inline_render.is_none() { self.failed_correction = Some((s.key(), s.authority())); return false; }
        self.clear(false, true); self.mark_attempt(s, "correction"); self.correction = Some((s.clone(), result)); self.publish_correction(s, rect); true
    }
    fn publish_correction_geometry(&mut self, s: &Snapshot) {
        let Some((_, result)) = &self.correction else { return; };
        let rect = self.bridge.as_mut().and_then(|bridge| bridge.range_rect(s, result.start, result.length).ok().flatten()).filter(valid_rect);
        self.publish_correction(s, rect);
    }
    fn publish_correction(&mut self, s: &Snapshot, rect: Option<core::Rect>) {
        self.claims.retain(|c| c.mode != "correction");
        let Some((_, result)) = self.correction.clone() else { return; };
        let Some(rect) = rect else { self.correction = None; self.emit(json!({"type":"clear"})); return; };
        if !s.inline_safe() || s.inline_render.is_none() || !result.matches(s) { self.correction = None; self.emit(json!({"type":"clear"})); return; }
        self.correction = Some((s.clone(), result.clone()));
        let mut event = self.presentation_event(s, &result.replacement, "correction", &[]);
        event["correction"] = json!({"start":result.start,"length":result.length,"word":result.word,"replacement":result.replacement,"language":result.language,"rect":rect}); self.emit(event);
    }
    fn length(&self) -> String { if self.on_battery && self.config.battery_shorter { features::shorter_length(&self.config.completion_length) } else { self.config.completion_length.clone() } }
    fn effective_config(&mut self, mode: &str) -> Config {
        let mut c = self.native_config(); c.completion_length = self.length();
        c.max_tokens = if mode == "completion" { c.max_tokens.min(features::length_profile(&c.completion_length).0) } else { 128 };
        if self.on_battery && c.backend == "gguf" && c.battery_model != "same" {
            match models::get_preset(&c.battery_model).ok().filter(|p| models::is_installed(p)) {
                Some(p) => if let Ok(path) = models::model_path(p) { c.model_path = path.to_string_lossy().into(); c.prompt_style = p.prompt_style.clone(); c.model_id = p.id.clone(); },
                None => self.error = "The battery model is not installed; the selected model is still used.".into(),
            }
        } c
    }
    fn submit(&mut self, s: &Snapshot, mode: &str, anchor: String) -> bool {
        if mode == "completion" && !s.inline_safe() { return false; }
        if matches!(mode, "emoji" | "synonyms") && !self.can_replace(s) { return false; }
        let config = self.effective_config(mode);
        if mode == "synonyms" && config.backend == "gguf" && config.prompt_style != "writing" { self.mark_attempt(s, mode); return false; }
        let anchored = mode == "alternatives" && !anchor.is_empty() && self.suggestion.snapshot.as_ref().is_some_and(|p| p.same_key(s) && p.same_authority(s));
        if anchored { self.bump_epoch(); let _ = self.worker.cancel(); self.request = None; self.preparer.cancel(); self.choices = None; }
        else { self.clear(false, true); self.completion_counted = false; }
        self.error.clear(); self.serial = self.serial.checked_add(1).expect("request identifier exhausted"); self.mark_attempt(s, mode);
        self.preparing = Some(PendingPreparation { token: self.serial, lease: Lease { snapshot: s.clone(), epoch: self.current_epoch(), revision: self.candidate_revision },
            mode: mode.into(), anchor: if anchored { anchor } else { String::new() }, config, native_pending: true }); true
    }
    fn advance_preparation(&mut self) {
        // Main thread acquires metadata only. The context owner independently
        // reacquires AX handles and fences every phase; no AppKit/TIS is moved.
        if self.preparing.as_ref().is_some_and(|p| p.native_pending) {
            let mut p = self.preparing.take().expect("preparation");
            let current = self.capture();
            if !current.as_ref().is_some_and(|s| p.lease.matches(s, self.current_epoch()) && s.has_anchor()) { self.clear(true, true); return; }
            let context_ticket = if (p.config.screen_context || p.config.screenshot_context) && !p.lease.snapshot.prefix().trim().is_empty()
                && p.lease.snapshot.selection_length == 0 && !p.lease.snapshot.window_id.is_empty() && !p.lease.snapshot.window_revision.is_empty() {
                self.context_status = "pending".into();
                self.bridge.as_mut().and_then(|bridge| self.readers.context_ticket(&p.lease.snapshot, &mut **bridge).ok().flatten())
            } else { None };
            let authorization_ticket = if p.config.clipboard_context || (p.config.personalization_strength != 0 && self.record_allowed(&p.lease.snapshot.app_id, &p.lease.snapshot.domain)) {
                self.bridge.as_mut().and_then(|bridge| self.readers.context_ticket(&p.lease.snapshot, &mut **bridge).ok().flatten())
            } else { None };
            if !self.capture().as_ref().is_some_and(|s| p.lease.matches(s, self.current_epoch())) { self.clear(true, true); return; }
            p.native_pending = false;
            let mut hard_disabled_apps = BTreeSet::new();
            for (scope, (enabled, expires)) in &self.temporary { if !enabled && self.now() < *expires { if let Some(app) = scope.strip_prefix("app:") { hard_disabled_apps.insert(app.to_owned()); } } }
            if !self.escape_app.is_empty() { hard_disabled_apps.insert(self.escape_app.clone()); }
            self.preparer.submit(Preparation { token: p.token, epoch: p.lease.epoch, snapshot: p.lease.snapshot.clone(), config: p.config.clone(), mode: p.mode.clone(), anchor: p.anchor.clone(), references: Vec::new(), context_ticket, authorization_ticket, hard_disabled_apps });
            self.preparing = Some(p);
        }
        for mut prepared in self.preparer.poll() {
            let Some(p) = self.preparing.take() else { continue; };
            if prepared.token != p.token || prepared.epoch != p.lease.epoch { self.preparing = Some(p); continue; }
            let current = self.capture();
            if !current.as_ref().is_some_and(|s| p.lease.matches(s, self.current_epoch()) && s.has_anchor() && (p.mode != "completion" || s.inline_safe())) { self.clear(true, true); continue; }
            let c = self.effective_config(&p.mode);
            if prepared.reference_expires.is_some_and(|at| self.now() > at) { prepared.options.context.clear(); prepared.reference_expires = None; }
            if c.provider_identity() != p.config.provider_identity() || c.prompt_style != p.config.prompt_style { self.attempted = None; continue; }
            self.error = prepared.warning;
            if self.context_status != "error" { self.context_status = prepared.context_status; }
            let s = &p.lease.snapshot;
            match self.worker.submit(&c, last(s.prefix(), c.context_chars as usize), &first(s.selection_suffix(), c.context_chars as usize), &prepared.options) {
                Ok(id) => self.request = Some(Request { id, lease: p.lease, generation: self.suggestion.generation, mode: p.mode,
                    length: c.completion_length.clone(), pending_text: None, final_: false, proceeded: false, reference_expires: prepared.reference_expires,
                    provider_identity: c.provider_identity(), prompt_style: c.prompt_style.clone() }),
                Err(_) => self.fail("Local inference submission"),
            }
        }
    }
    fn consume_results(&mut self) {
        let events = self.worker.poll();
        if !events.is_empty() { let fresh = self.capture(); self.observe(fresh); }
        for event in events {
            let Some(mut request) = self.request.take() else { continue; };
            if event.id != request.id { self.request = Some(request); continue; }
            let current = self.candidate.clone();
            if !self.active() || !current.as_ref().is_some_and(|s| request.lease.matches(s, self.current_epoch())) { self.clear(true, true); continue; }
            let current = current.expect("current lease");
            match event.kind.as_str() {
                "ready" => {
                    let effective = self.effective_config(&request.mode);
                    if request.reference_expires.is_some_and(|at| self.now() > at) || effective.provider_identity() != request.provider_identity
                        || effective.prompt_style != request.prompt_style || effective.completion_length != request.length {
                        self.submit(&current, &request.mode, self.suggestion.text.clone()); continue;
                    }
                    let fresh = self.capture();
                    if fresh.as_ref().is_some_and(|s| request.lease.matches(s, self.current_epoch()) && Some(request.lease.revision) == self.user_revision() && s.has_anchor() && (request.mode != "completion" || s.inline_safe())) {
                        if self.worker.proceed(request.id).is_ok() { request.proceeded = true; self.request = Some(request); }
                        else { self.fail("Local inference authorization"); self.clear(false, true); }
                    } else { self.clear(true, true); }
                }
                "result" if request.proceeded => {
                    if request.mode == "completion" {
                        request.pending_text = Some(if request.length == "word" { features::current_word_only(current.prefix(), &event.text) } else { event.text });
                        request.final_ |= event.final_;
                        self.request = Some(request); self.publish_pending_completion();
                    } else if event.final_ {
                        let mode = request.mode.clone(); let mut choices = features::parse_choices(&event.text, 5);
                        if mode == "synonyms" { let selected = core::casefold(current.selected_text()); choices.retain(|c| core::casefold(c) != selected); }
                        let anchored = mode == "alternatives" && !self.suggestion.text.is_empty() && self.suggestion.snapshot.as_ref().is_some_and(|s| s.same_key(&current) && s.same_authority(&current));
                        let length = if mode == "synonyms" { current.selection_length } else { 0 };
                        let start = current.caret; self.set_choices(current, choices, &mode, start, length, anchored);
                    } else { self.request = Some(request); }
                }
                "error" => { self.fail("Local inference"); self.clear(false, false); }
                "done" => { request.final_ = true; if request.pending_text.is_some() { self.request = Some(request); self.publish_pending_completion(); } }
                _ => self.request = Some(request),
            }
        }
        self.publish_pending_completion();
    }
    fn publish_pending_completion(&mut self) {
        let Some(mut request) = self.request.take() else { return; };
        let Some(s) = self.candidate.clone() else { return; };
        if !request.lease.matches(&s, self.current_epoch()) { return; }
        if request.mode != "completion" || request.pending_text.is_none() { self.request = Some(request); return; }
        match self.completion_blocked(&s) {
            None => { self.request = Some(request); } // R7: final text remains owned until verdict.
            Some(true) => { if !request.final_ { self.request = Some(request); } }
            Some(false) => {
                let fresh = self.capture();
                if !fresh.as_ref().is_some_and(|current| request.lease.matches(current, self.current_epoch()) && current.inline_safe()) { self.clear(true, true); return; }
                let text = request.pending_text.take().expect("pending text");
                if self.suggestion.offer(request.generation, s, text) { self.publish_completion(); }
                if !request.final_ { self.request = Some(request); }
            }
        }
    }
    fn schedule(&mut self, s: &Snapshot) {
        if !self.active() || s.on_demand_only || self.field_paused(s) || self.pending_edit.is_some() || !s.has_anchor() { return; }
        if self.uncertain_provenance.as_ref().is_some_and(|(authority, revision)| *authority == s.authority() && Some(*revision) == self.user_revision()) { return; }
        let selected = selected_word(s); let mut mode = if selected.is_empty() { "completion" } else { "synonyms" };
        let mut eligible = if !selected.is_empty() { self.can_replace(s) && self.config.synonyms_enabled && self.config.synonyms_mode == "selection" }
            else { s.inline_safe() && s.eligible(self.config.min_chars as usize) && (self.config.complete_inside_text || s.line_suffix().trim().is_empty())
                && (self.config.completion_length != "word" || features::mid_word(s.prefix())) };
        if self.can_replace(s) && selected.is_empty() && s.eligible(1) {
            if let Some((suffix, choices)) = features::emoji_choices(s.prefix(), &self.config) {
                if !suffix.is_empty() && s.prefix().ends_with(&suffix) && !self.attempted(s, "emoji") { self.clear(false, true); self.mark_attempt(s, "emoji"); self.set_choices(s.clone(), choices, "emoji", s.caret - suffix.chars().count(), suffix.chars().count(), false); }
                return;
            }
        }
        if selected.is_empty() && self.typo_active(s) && s.eligible(1) {
            match self.assess(s) {
                None => eligible = false,
                Some(result) if result.suspected => {
                    if !self.attempted(s, "correction") && self.show_correction(s, result) { eligible = false; }
                    if self.config.typo_suppression { eligible = false; }
                }
                _ => {},
            }
        }
        if self.config.alternatives_enabled && self.config.alternatives_mode == "pause" && selected.is_empty()
            && self.request.is_none() && self.preparing.is_none() && !self.suggestion.text.is_empty() && self.correction.is_none()
            && self.claims.iter().any(|c| c.mode == "completion") && !self.attempted(s, "alternatives")
            && self.now().duration_since(self.candidate_since) >= Duration::from_millis(self.config.alternatives_delay_ms.into()) {
            self.submit(s, "alternatives", self.suggestion.text.clone()); return;
        }
        if self.on_battery && self.config.battery_on_demand { eligible = false; }
        let delay = if mode == "synonyms" { self.config.synonyms_delay_ms } else { self.config.debounce_ms };
        if eligible && self.now().duration_since(self.candidate_since) >= Duration::from_millis(delay.into()) && !self.attempted(s, mode)
            && (mode != "completion" || (self.suggestion.text.is_empty() && self.correction.is_none())) {
            if !selected.is_empty() { mode = "synonyms"; } self.submit(s, mode, String::new());
        }
    }
    fn prepare_models(&mut self) {
        if !self.active() || (self.on_battery && self.config.battery_on_demand) { self.resident = None; return; }
        if self.config.idle_release_seconds == 0 {
            let c = self.effective_config("completion");
            let identity = c.provider_identity();
            if self.resident.as_ref() != Some(&identity) { self.resident = Some(identity); if self.worker.prepare(&c).is_err() { self.fail("Model preparation"); } }
        }
        let Some(s) = self.candidate.as_ref() else { return; };
        let empty_native = s.text.is_empty() && s.caret == 0 && s.window_pid > 0 && !s.window_id.is_empty() && s.field_rect.as_ref().is_some_and(valid_rect);
        if s.protected || !s.multiline || (!s.inline_safe() && !empty_native) || !s.has_anchor() || s.selection_length != 0 || s.on_demand_only || self.field_paused(s) { return; }
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(s.text.as_ref(), &mut hash);
        let fingerprint = std::hash::Hasher::finish(&hash);
        if self.model_activity.as_ref().is_some_and(|(text, authority, revision, caret)| *text == fingerprint && authority_matches(authority, s) && *revision == self.candidate_revision && *caret == s.caret) { return; }
        self.model_activity = Some((fingerprint, s.authority(), self.candidate_revision, s.caret));
        let c = self.effective_config("completion"); if self.worker.prepare(&c).is_err() { self.fail("Model preparation"); }
    }
    fn release_model(&mut self) { self.resident = None; self.model_activity = None; if self.worker.release().is_err() { self.fail("Model release"); } }
    fn refresh_arming(&self) {
        let Ok(mut a) = self.armed.try_lock() else { return; };
        if self.shortcuts_suspended || self.stopping { a.claims.clear(); } else { a.claims.clone_from(&self.claims); }
        if let Some(candidate) = &self.candidate {
            if a.origin.as_ref().is_none_or(|origin| origin.1 != self.candidate_revision || origin.2 != self.current_epoch() || !origin.0.same_key(candidate) || !origin.0.same_authority(candidate)) {
                a.origin = Some(Arc::new((candidate.clone(), self.candidate_revision, self.current_epoch())));
            }
        } else { a.origin = None; }
        a.active = self.active() && self.native_ready && !self.shortcuts_suspended; a.alternatives = self.config.alternatives_enabled; a.synonyms = self.config.synonyms_enabled && self.candidate.as_ref().is_some_and(|s| self.can_replace(s));
        a.escape_passthrough = self.config.escape_behavior == "passthrough"; a.inflight = self.request.is_some() || self.preparing.is_some();
    }
    fn presentation(&mut self, key: SnapshotKey, authority: Authority, revision: u64, visible: String, mode: String) {
        let candidate = self.candidate.as_ref();
        let current = candidate.is_some_and(|s| s.key() == key && s.authority() == authority) && revision == self.candidate_revision && Some(revision) == self.user_revision();
        let mut count = 0;
        let valid = current && !visible.is_empty() && match mode.as_str() {
            "completion" => self.suggestion.snapshot.as_ref().is_some_and(|s| s.key() == key && s.authority() == authority && (s.inline_safe() || self.advance.as_ref().is_some_and(|(k,a,g,_)| *k == key && *a == authority && g.inline_safe()))) && self.suggestion.text.starts_with(&visible),
            "correction" => self.correction.as_ref().is_some_and(|(s,r)| s.key() == key && s.authority() == authority && r.replacement == visible),
            "alternatives" | "synonyms" | "emoji" => self.choices.as_ref().is_some_and(|c| {
                if c.mode != mode || c.snapshot.key() != key || c.snapshot.authority() != authority { return false; }
                count = visible.split('\n').zip(&c.values).take_while(|(drawn, value)| *drawn == value.as_str()).count(); count != 0
            }), _ => false,
        };
        self.claims.retain(|c| c.mode != mode);
        if valid && self.native_ready && !self.shortcuts_suspended { self.claims.push(Arc::new(Claim { key, authority, revision, epoch: self.current_epoch(), visible, mode, count })); }
        self.refresh_arming();
    }
    fn accept(&mut self, action: &Action, supplied: Option<Arc<Claim>>) -> bool {
        let Some(current) = self.capture().filter(Snapshot::has_anchor) else { self.clear(true, true); return false; };
        let claim = supplied.or_else(|| match action {
            Action::Choice(_) => self.claims.iter().find(|c| matches!(c.mode.as_str(), "alternatives" | "emoji" | "synonyms")).cloned(),
            _ => primary_claim_index(&self.claims, false).map(|index| self.claims[index].clone()),
        });
        self.disarm();
        let Some(claim) = claim.filter(|c| c.key == current.key() && c.authority == current.authority() && Some(c.revision) == self.user_revision() && c.epoch == self.current_epoch()) else { return false; };
        let (text, start, length, remainder) = if claim.mode == "correction" {
            let Some((s,r)) = &self.correction else { return false; };
            if !s.same_key(&current) || !s.same_authority(&current) || !self.can_replace(&current) || r.replacement != claim.visible { return false; }
            (r.replacement.clone(), r.start, r.length, None)
        } else if matches!(claim.mode.as_str(), "alternatives" | "synonyms" | "emoji") {
            let Some(c) = &self.choices else { return false; };
            let index = match action { Action::Choice(i) => *i, _ if c.mode != "alternatives" => 0, _ => return false };
            if index >= c.values.len() || index >= claim.count || c.mode != claim.mode || !c.snapshot.same_key(&current) || !c.snapshot.same_authority(&current) { return false; }
            if (c.length != 0 || c.start != current.caret || c.mode == "emoji" || c.mode == "synonyms") && !self.can_replace(&current) { return false; }
            (c.values[index].clone(), c.start, c.length, None)
        } else {
            if !current.inline_safe() || !current.eligible(1) { self.clear(true, true); return false; }
            let mut proposed = self.suggestion.clone();
            let Some(text) = proposed.accept(&current, *action == Action::All, self.config.trailing_space,
                features::include_punctuation(&self.config.punctuation_mode, *action == Action::All), Some(&claim.visible)) else { return false; };
            (text, current.caret, 0, Some(proposed))
        };
        let Some(expected) = edit_snapshot(&current, start, length, &text) else { return false; };
        if !self.input_claim_valid() { return false; }
        let outcome = self.bridge.as_mut().ok_or_else(|| anyhow!("Native input is unavailable.")).and_then(|bridge| {
            if start == current.caret && length == 0 { bridge.insert(&current, &text) } else { bridge.replace_range(&current, start, length, &text) }
        });
        let edit = PendingEdit { before: current.clone(), expected, text, start, length, remainder, at: self.now(), revision: claim.revision, mode: claim.mode.clone() };
        match outcome {
            Ok(EditOutcome::Applied(readback)) => {
                if confirmed_edit(&edit, &readback) { self.commit_edit(edit, readback); true }
                else { self.clear(true, true); self.uncertain_provenance = Some((current.authority(), claim.revision)); true } // uncertain native call: never replay.
            }
            Ok(EditOutcome::Dispatched) => { self.clear(false, true); self.uncertain_provenance = Some((current.authority(), claim.revision)); self.pending_edit = Some(edit); true }
            Ok(EditOutcome::Indeterminate) | Err(_) => { self.clear(true, true); self.uncertain_provenance = Some((current.authority(), claim.revision)); self.mark_attempt(&current, &claim.mode); true }
            Ok(EditOutcome::Rejected) => { self.clear(false, true); false }
        }
    }
    fn observe_pending_edit(&mut self, snapshot: &Snapshot) {
        let Some(edit) = self.pending_edit.take() else { return; };
        if confirmed_edit(&edit, snapshot) && self.user_revision() == Some(edit.revision) { self.commit_edit(edit, snapshot.clone()); }
        else if snapshot.same_key(&edit.before) && snapshot.same_authority(&edit.before) && self.now().duration_since(edit.at) < Duration::from_secs(2)
            && self.user_revision() == Some(edit.revision) { self.pending_edit = Some(edit); }
        // Any different observation expires the uncertain dispatch. Never retry it.
    }
    fn commit_edit(&mut self, edit: PendingEdit, readback: Snapshot) {
        if self.config.statistics_enabled { self.push_record(Record::Stats { app: edit.before.app_id.clone(), text: edit.text.clone(), completion: !self.completion_counted }); }
        self.completion_counted = true;
        if self.uncertain_provenance.as_ref().is_some_and(|(authority,_)| *authority == readback.authority()) { self.uncertain_provenance = None; }
        if self.record_allowed(&edit.before.app_id, &edit.before.domain) {
            self.history_suppressed = None; self.observe_session(&edit.before);
            if let Some(session) = &mut self.session {
                if readback.text.scalar_len() <= 32000 {
                    session.authored.splice(edit.start..edit.start + edit.length, std::iter::repeat_n(false, edit.text.chars().count()));
                    session.text = readback.text.to_string(); session.accepted = true; self.queue_session();
                } else { self.finish_session(); }
            }
        }
        self.clear(false, true); self.candidate = Some(readback.clone()); if let Some(revision) = self.user_revision() { self.candidate_revision = revision; } self.capture_after = self.now();
        if let Some(mut remainder) = edit.remainder {
            if !remainder.text.is_empty() {
                if !readback.inline_safe() && edit.before.inline_safe() { self.advance = Some((readback.key(), readback.authority(), edit.before.clone(), edit.text.clone())); }
                remainder.snapshot = Some(readback.clone()); self.suggestion = remainder; self.publish_completion();
            }
        }
        if edit.mode != "completion" { self.candidate_since = self.now(); }
        self.mark_attempt(&readback, &edit.mode);
    }
    fn dismiss(&mut self, bypass: bool, claim: Option<Arc<Claim>>) -> bool {
        let claim = claim.or_else(|| primary_claim_index(&self.claims, true).map(|index| self.claims[index].clone()));
        if let Some(claim) = claim {
            if !self.owns_claim(&claim) { return false; }
            let Some(current) = self.capture() else { self.clear(true, true); return false; };
            if current.key() != claim.key || current.authority() != claim.authority || !self.input_claim_valid() { self.clear(true, true); return false; }
            self.clear(false, true); self.mark_attempt(&current, &claim.mode); // R6: actual tool and authority.
            if !bypass { match self.config.escape_behavior.as_str() {
                "pause" => self.field_pause = Some((current.authority(), self.now() + Duration::from_secs(self.config.escape_pause_seconds.into()))),
                "app" => { self.escape_app = current.app_id.clone(); self.configure_native(); }, _ => {},
            }}
            return bypass || self.config.escape_behavior != "passthrough";
        }
        if !bypass && (self.request.is_some() || self.preparing.is_some()) {
            let mode = self.request.as_ref().map(|r| r.mode.clone()).or_else(|| self.preparing.as_ref().map(|p| p.mode.clone())).unwrap_or_else(|| "completion".into());
            let candidate = self.candidate.clone(); self.clear(false, true); if let Some(s) = candidate { self.mark_attempt(&s, &mode); }
        } false
    }
    fn action(&mut self, action: Action, claim: Option<Arc<Claim>>, origin: Option<Arc<(Snapshot, u64, u64)>>) -> bool {
        if self.stopping || self.shortcuts_suspended || !self.input_claim_valid() { return false; }
        if let Some(origin) = origin {
            if origin.2 != self.current_epoch() { return false; }
            let current = self.capture();
            if origin.2 != self.current_epoch() || Some(origin.1) != self.user_revision() || !current.as_ref().is_some_and(|s| s.same_key(&origin.0) && s.same_authority(&origin.0)) { return false; }
            if !self.input_claim_valid() { return false; }
        }
        match action {
            Action::Word | Action::All | Action::Choice(_) => self.accept(&action, claim),
            Action::Dismiss | Action::Bypass => self.dismiss(action == Action::Bypass, claim),
            Action::Settings => { self.emit(json!({"type":"settings","pane":"general"})); true }
            Action::Quit => { self.emit(json!({"type":"quit"})); true }
            Action::Toggle => {
                if self.config.global_enabled && self.now() < self.paused_until { self.paused_until = self.now(); self.attempted = None; self.configure_native(); }
                else { let mut c = self.config.clone(); c.global_enabled = !c.global_enabled; if self.apply_config(c, true).is_err() { self.fail("Preferences"); } }
                true
            }
            Action::AppToggle => { let Some(info) = self.foreground() else { return false; }; if !self.input_claim_valid() { return false; } self.toggle_app(&info.app_id, &info.domain) }
            Action::Force | Action::Alternatives | Action::Synonyms => {
                if !self.active() || (action == Action::Alternatives && !self.config.alternatives_enabled) || (action == Action::Synonyms && !self.config.synonyms_enabled) { return false; }
                let Some(s) = self.capture().filter(Snapshot::has_anchor) else { return false; };
                let mode = if action == Action::Force { "completion" } else if action == Action::Alternatives { "alternatives" } else { "synonyms" };
                if (mode == "synonyms" && selected_word(&s).is_empty()) || (mode != "synonyms" && !s.eligible(1))
                    || (mode == "completion" && (!s.inline_safe() || (self.config.completion_length == "word" && !features::mid_word(s.prefix())))) { return false; }
                let anchor = if mode == "alternatives" && self.suggestion.snapshot.as_ref().is_some_and(|p| p.same_key(&s) && p.same_authority(&s)) { self.suggestion.text.clone() } else { String::new() };
                if !self.input_claim_valid() { return false; }
                self.observe(Some(s.clone())); self.submit(&s, mode, anchor)
            }
        }
    }
    fn run_inputs(&mut self) {
        while let Ok(input) = self.inputs.try_recv() {
            let bypass = input.action == Action::Bypass;
            if (input.transaction || bypass) && !self.input_claim_valid() { let _ = self.finish_input(false); continue; }
            let accepted = self.action(input.action, input.claim, input.origin);
            if (input.transaction || bypass) && self.finish_input(accepted && !bypass).is_err() { self.fail("Shortcut transaction"); }
        }
    }
    fn observe_session(&mut self, s: &Snapshot) {
        if self.history_suppressed.as_ref().is_some_and(|(key, text)| *key == s.authority() && text.as_str() == s.text.as_ref()) { return; }
        self.history_suppressed = None;
        if !self.record_allowed(&s.app_id, &s.domain) || s.text.scalar_len() > 32000 { self.finish_session(); return; }
        if self.session.as_ref().is_none_or(|session| session.key != s.authority()) {
            self.finish_session(); self.session_serial = self.session_serial.wrapping_add(1);
            self.session = Some(Session { key: s.authority(), id: format!("{:016x}{:016x}", chrono::Utc::now().timestamp_micros(), self.session_serial), text: s.text.to_string(), authored: vec![false; s.text.scalar_len()], accepted: false });
        } else {
            let uncertain = self.uncertain_provenance.as_ref().is_some_and(|(authority,_)| *authority == s.authority());
            let changed = self.session.as_ref().is_some_and(|session| session.text.as_str() != s.text.as_ref());
            if let Some(session) = &mut self.session { update_authored(session, &s.text, !uncertain); }
            if changed && self.uncertain_provenance.as_ref().is_some_and(|(authority,revision)| *authority == s.authority() && self.user_revision().is_some_and(|current| *revision != current)) { self.uncertain_provenance = None; }
        }
    }
    fn queue_session(&mut self) {
        if let Some(s) = &self.session {
            let record = Record::History { id: s.id.clone(), app: s.key.app_id.clone(), domain: s.key.domain.clone(), text: s.text.clone(), spans: authored_spans(&s.authored) };
            self.push_record(record);
        }
    }
    fn push_record(&mut self, record: Record) { if self.records.len() == 128 { self.records.pop_front(); } self.records.push_back(record); }
    fn finish_session(&mut self) {
        if let Some(s) = &self.session { if (s.accepted || self.config.record_unaccepted) && self.record_allowed(&s.key.app_id, &s.key.domain) { self.queue_session(); } }
        self.session = None;
    }
    fn flush_records(&mut self) -> Result<()> {
        let mut failed = false;
        while let Some(record) = self.records.pop_front() {
            let result = match record {
                Record::History { id, app, domain, text, spans } if self.record_allowed(&app, &domain) => locked(&self.storage).record(&id, &app, &domain, &text, &spans),
                Record::Stats { app, text, completion } if self.config.statistics_enabled => locked(&self.storage).acceptance(&app, &text, completion),
                _ => Ok(()),
            }; failed |= result.is_err();
        }
        if failed { self.error = "Local history or statistics could not be saved.".into(); Err(anyhow!(self.error.clone())) } else { Ok(()) }
    }
    fn apply_config(&mut self, mut c: Config, persist: bool) -> Result<()> {
        c.validate()?;
        if recording_configured(&c) { locked(&self.storage).unlock()?; }
        if persist { self.readers.persist(&c)?; }
        self.finish_session();
        let release = !c.global_enabled || self.config.provider_identity() != c.provider_identity() || privacy_revoked(&self.config, &c);
        self.config = c; self.clear(true, true); self.candidate = None; self.attempted = None; self.failed_correction = None; self.capture_after = self.now(); self.resident = None;
        self.worker.set_idle_timeout(self.config.idle_release_seconds); self.configure_native();
        if !self.native_ready { let _ = self.retry_native(); }
        self.set_native_shortcuts(self.native_ready && !self.shortcuts_suspended && !self.stopping);
        if release { self.release_model(); }
        if !recording_configured(&self.config) { self.session = None; locked(&self.storage).lock(); }
        if !(self.config.typo_suppression || self.config.typo_fixes) {
            if let Some(service) = self.spelling.take() { self.spawn_spelling_cleanup(service); }
        } else if self.spelling.is_none() {
            self.spelling = Some(self.spelling_factory.as_ref().map_or_else(|| Arc::new(SpellingService::new(None, None)) as Arc<dyn Spelling>, |factory| factory()));
        }
        self.emit(json!({"type":"config","config":self.config})); Ok(())
    }
    fn toggle_app(&mut self, app: &str, host: &str) -> bool {
        if app.is_empty() || excluded(&self.config, app, "") { return false; }
        let host = domain(host); let scope = if host.is_empty() { format!("app:{app}") } else { format!("domain:{host}") };
        let desired = !self.allowed(app, &host, ""); if self.escape_app == app { self.escape_app.clear(); }
        self.temporary.remove(&scope);
        if self.allowed(app, &host, "") != desired { self.temporary.insert(scope, (desired, self.now() + Duration::from_secs(300))); }
        self.finish_session(); self.clear(true, true); self.candidate = None; self.attempted = None; self.configure_native(); true
    }
    pub fn command(&mut self, command: Command) {
        if self.stopping { return; }
        let result: Result<()> = match command {
            Command::Action(action) => { let handled = self.action(action.clone(), None, None); self.emit(json!({"type":"action_result","action":action,"handled":handled})); Ok(()) }
            Command::ApplyConfig { config, persist } => self.apply_config(config, persist),
            Command::PresentationResult { key, authority, revision, visible_text, mode } => { self.presentation(key, authority, revision, visible_text, mode); Ok(()) }
            Command::Pause { minutes } => {
                self.finish_session(); let _ = self.flush_records(); self.paused_until = self.now() + Duration::from_secs(u64::from(minutes) * 60); self.timed_pause = true;
                self.clear(true, true); self.candidate = None; self.release_model(); self.configure_native(); self.emit(json!({"type":"pause","minutes":minutes})); Ok(())
            }
            Command::Resume => {
                self.paused_until = self.now(); self.timed_pause = false; self.attempted = None;
                let result = if !self.config.global_enabled { let mut c = self.config.clone(); c.global_enabled = true; self.apply_config(c, true) } else { self.configure_native(); Ok(()) };
                if result.is_ok() { self.emit(json!({"type":"resume"})); } result
            }
            Command::ToggleApp { app, domain } => { let changed = self.toggle_app(&app, &domain); self.emit(json!({"type":"app_toggle","changed":changed,"app":app,"domain":domain})); Ok(()) }
            Command::SetModel { path, prompt_style, model_id } => {
                let mut c = self.config.clone(); c.backend = "gguf".into(); c.model_path = path; c.prompt_style = prompt_style;
                let selected = crate::paths::expand_user(std::ffi::OsStr::new(&c.model_path)).ok();
                c.model_id = model_id.unwrap_or_else(|| models::presets().iter().find(|p| selected.as_ref().is_some_and(|selected| models::model_path(p).ok().as_ref() == Some(selected))).map_or(String::new(), |p| p.id.clone())); self.apply_config(c, true)
            }
            Command::SetHistoryEnabled { enabled } => { let mut c = self.config.clone(); c.record_history = enabled; self.apply_config(c, true) }
            Command::DownloadModel { id, accept_license } => self.download_model(&id, accept_license),
            Command::CancelDownload => { if let Some(d) = &self.download { d.cancel.set(); } Ok(()) }
            Command::Models => { match model_inventory() { Ok(v) => { self.emit(json!({"type":"models","models":v})); Ok(()) }, Err(e) => Err(e) } }
            Command::HistoryInfo => { let value = locked(&self.storage).info(); value.map(|value| self.emit(json!({"type":"history_info","result":value}))) }
            Command::HistoryCounts { unlock } => { let _ = self.flush_records(); let value = locked(&self.storage).counts(unlock); value.map(|value| self.emit(json!({"type":"history_counts","result":value}))) }
            Command::Statistics { period, group_by } => { let _ = self.flush_records(); let value = locked(&self.storage).statistics(&period, &group_by); value.map(|value| self.emit(json!({"type":"statistics","period":period,"group_by":group_by,"result":value}))) }
            Command::ClearStatistics => { self.records.retain(|r| !matches!(r, Record::Stats { .. })); let result = locked(&self.storage).clear_statistics(); result.map(|_| self.emit(json!({"type":"statistics_cleared"}))) }
            Command::DeleteHistory { app, domain } => self.delete_history(app.as_deref(), domain.as_deref()),
            Command::EnableDocs { token } => { self.enable_docs(&token); Ok(()) }
            Command::SuspendForPreview => { self.finish_session(); let _ = self.flush_records(); self.preview = true; self.clear(true, true); self.candidate = None; self.release_model(); self.configure_native(); Ok(()) }
            Command::ResumeFromPreview => { self.preview = false; self.capture_after = self.now(); self.configure_native(); Ok(()) }
            Command::SuspendShortcuts { suspended } => { self.suspend_shortcuts(suspended); Ok(()) }
            Command::RetryNative => self.retry_native(),
        };
        if result.is_err() { self.fail("Requested operation"); }
        self.refresh_arming();
    }
    fn delete_history(&mut self, app: Option<&str>, host: Option<&str>) -> Result<()> {
        ensure!(app.is_none() || host.is_none(), "Delete one app or one website at a time."); let host = host.map(domain);
        let matches = |a: &str, d: &str| app.is_none() && host.is_none() || app == Some(a) || host.as_deref() == Some(d);
        self.records.retain(|r| !matches!(r, Record::History { app, domain, .. } if matches(app, domain)));
        if self.session.as_ref().is_some_and(|s| matches(&s.key.app_id, &s.key.domain)) { self.session = None; }
        if let Some(s) = &self.candidate { if matches(&s.app_id, &s.domain) { self.history_suppressed = Some((s.authority(), s.text.to_string())); } }
        self.clear(true, true); self.candidate = None; self.attempted = None; self.release_model();
        let result = locked(&self.storage).delete(app, host.as_deref(), app.is_some() || host.is_some());
        let count = result?; self.emit(json!({"type":"history_deleted","count":count,"app":app,"domain":host})); Ok(())
    }
    fn download_model(&mut self, id: &str, accept_license: bool) -> Result<()> {
        models::get_preset(id)?;
        ensure!(self.download.as_ref().is_none_or(|d| d.thread.is_finished()), "Cancel the current model download before starting another.");
        if let Some(d) = self.download.take() { let _ = d.thread.join(); }
        let preset = models::get_preset(id)?.clone(); let id = id.to_owned(); let cancel = models::DownloadCancellation::new(); let token = cancel.clone();
        let (sender, receiver) = mpsc::sync_channel(128);
        let initial = json!({"type":"download","id":id,"received":0,"total":preset.size_bytes,"state":"downloading","error":""});
        self.download_status = Some(initial.clone()); self.emit(initial);
        let thread = thread::Builder::new().name("brotypist-model-download".into()).spawn(move || {
            let progress_sender = sender.clone();
            let result = models::download_model(&id, accept_license, &token, &mut |received,total| { let _ = progress_sender.try_send(json!({"type":"download","id":id,"received":received,"total":total,"state":"downloading","error":""})); });
            let event = match result {
                Ok(path) if !token.is_set() => json!({"type":"download","id":id,"received":preset.size_bytes,"total":preset.size_bytes,"state":"complete","path":path,"prompt_style":preset.prompt_style,"error":""}),
                _ => json!({"type":"download","id":id,"state":if token.is_set() {"cancelled"} else {"error"},"error":if token.is_set() {"Download cancelled."} else {"Model download failed; existing weights were preserved."}}),
            }; let _ = sender.send(event);
        })?;
        self.download = Some(Download { cancel, thread, receiver }); Ok(())
    }
    fn consume_download(&mut self) {
        let events = self.download.as_ref().map(|d| d.receiver.try_iter().collect::<Vec<_>>()).unwrap_or_default();
        for event in events { self.download_status = Some(event.clone()); if !self.stopping { self.emit(event); } }
        if self.download.as_ref().is_some_and(|d| d.thread.is_finished()) { if let Some(d) = self.download.take() { let _ = d.thread.join(); } }
    }
    fn refresh_docs(&mut self, info: Option<platform::Foreground>) {
        let revision = self.bridge.input_revision();
        let eligible = info.as_ref().is_some_and(|_| self.active() && self.bridge.secure_input_enabled() == Some(false));
        if self.docs.as_ref().is_some_and(|(_, old, rev)| !eligible || info.as_ref().is_none_or(|i| i.app_id != old.app_id || i.domain != old.domain || i.window_id != old.window_id || i.window_pid != old.window_pid || i.document_id != old.document_id) || *rev != revision) {
            self.clear_docs(); self.docs_after = self.docs_after.max(self.now() + Duration::from_secs(2));
        }
        if !eligible || self.candidate.is_some() { self.clear_docs(); return; }
        if self.now() < self.docs_after { return; }
        self.docs_after = self.now() + Duration::from_secs(2);
        let Some(before) = info else { return; };
        let hint = self.bridge.docs_hint().ok().flatten();
        let after = self.foreground();
        if self.bridge.input_revision() != revision || self.bridge.secure_input_enabled() != Some(false)
            || after.as_ref().is_none_or(|i| i.app_id != before.app_id || i.domain != before.domain || i.window_id != before.window_id || i.window_pid != before.window_pid || i.document_id != before.document_id) { self.clear_docs(); return; }
        if let Some(hint) = hint.filter(|h| h.get("token").and_then(Value::as_str).is_some_and(|t| !t.is_empty()) && h.get("support").and_then(Value::as_str) == Some("unknown") && h.get("app_id").and_then(Value::as_str) == Some(before.app_id.as_str())) {
            if self.docs.as_ref().is_none_or(|(old,_,_)| *old != hint) { self.emit(json!({"type":"docs_help","hint":hint})); }
            self.docs = Some((hint, before, revision));
        } else { self.clear_docs(); }
    }
    fn enable_docs(&mut self, token: &str) {
        let info = self.foreground(); let mut result = json!({"status":"unknown","diagnostic":"stale-capability","help_url":""});
        if let Some((hint, old, revision)) = self.docs.take() {
            self.emit(json!({"type":"docs_help","hint":null}));
            if self.active() && self.bridge.secure_input_enabled() == Some(false) && self.bridge.input_revision() == revision
                && hint.get("token").and_then(Value::as_str) == Some(token)
                && info.as_ref().is_some_and(|i| i.app_id == old.app_id && i.domain == old.domain && i.window_id == old.window_id && i.window_pid == old.window_pid && i.document_id == old.document_id) {
                if let Ok(value) = self.bridge.enable_docs_accessibility(token) { if value.get("status").and_then(Value::as_str).is_some_and(|s| ["enabled","already_enabled","unknown"].contains(&s)) { result = value; } }
            }
        }
        self.capture_after = self.now(); self.emit(json!({"type":"docs_result","result":result}));
    }
    pub fn tick(&mut self) {
        if self.bridge.pump_events().is_err() { self.fail("Native event transport"); }
        self.consume_download(); self.reap_spelling();
        if self.stopping { let _ = self.shutdown_step(); return; }
        self.run_inputs(); let now = self.now();
        if self.timed_pause && now >= self.paused_until { self.timed_pause = false; self.attempted = None; self.configure_native(); }
        if now >= self.power_after { self.power_after = now + Duration::from_secs(5); let power = self.readers.power(); self.on_battery = power.available && power.has_battery && power.on_battery; }
        let previous = self.temporary.len(); self.temporary.retain(|_, (_, at)| now < *at);
        if previous != self.temporary.len() { self.clear(true, true); self.candidate = None; self.attempted = None; self.configure_native(); }
        let revision = self.bridge.input_revision();
        if revision != self.input_revision || self.input_pending.swap(false, Ordering::AcqRel) { self.input_revision = revision; self.capture_after = now; self.disarm(); }
        let foreground = self.foreground();
        if !self.active() || foreground.is_none() || self.bridge.secure_input_enabled() != Some(false) { self.observe(None); self.clear_docs(); }
        else if now >= self.capture_after {
            let snapshot = self.capture(); self.observe(snapshot); self.capture_after = now + if self.request.is_some() || self.preparing.is_some() || now.duration_since(self.candidate_since) < Duration::from_secs(2) { Duration::from_millis(50) } else { Duration::from_millis(100) };
        }
        self.refresh_docs(foreground); self.prepare_models(); self.advance_preparation(); self.consume_results();
        self.refresh_context();
        if let Some(s) = self.candidate.clone() { self.schedule(&s); }
        let _ = self.flush_records(); self.refresh_arming();
    }
    pub fn status(&self) -> Value {
        let mut status = self.worker.status(); if !status.is_object() { status = json!({}); }
        if !self.native_ready { status["state"] = json!("error"); status["message"] = json!("Native input is unavailable. Check permissions and keyboard settings, then retry."); }
        else if !self.active() { status["state"] = json!("paused"); status["message"] = json!(if self.config.global_enabled { "Typing suggestions are paused." } else { "Typing suggestions are disabled." }); }
        else if !self.error.is_empty() { status["state"] = json!("error"); status["message"] = json!(self.error); }
        let temporary = [format!("domain:{}", self.host), format!("app:{}", self.app)].into_iter().find_map(|scope| self.temporary.get(&scope).filter(|(_, at)| self.now() < *at).map(|(_, at)| (scope, at.saturating_duration_since(self.now()).as_secs_f64())));
        let fields = json!({"enabled":self.active(),"inline_available":self.candidate.as_ref().is_some_and(Snapshot::inline_safe),
            "inline_message":if self.candidate.as_ref().is_some_and(Snapshot::inline_safe) {"Verified same-line completion is available."} else {"This field does not expose verified same-line rendering; ordinary completion is silent."},
            "context_status":self.context_status,"context_message":context_message(&self.context_status),"current_app_enabled":self.allowed(&self.app,&self.host,""),
            "current_recording":self.record_allowed(&self.app,&self.host),"current_tab_enabled":rule_bool(&self.config,&self.app,&self.host,"tab_enabled",true),
            "secure_input":self.bridge.secure_input_enabled() != Some(false),"temporary_scope":temporary.as_ref().map(|(scope,_)|scope.as_str()).unwrap_or(""),
            "temporary_remaining":temporary.map(|(_,seconds)|seconds),"escape_suppressed":!self.escape_app.is_empty() && self.escape_app == self.app,
            "native_status":self.bridge.status(),"current_app":self.app,"current_domain":self.host,"paused_remaining":self.paused_until.saturating_duration_since(self.now()).as_secs_f64(),
            "on_battery":self.on_battery,"download":self.download_status,"preview":self.preview,
            "native_started":self.native_ready,"native_hooks_started":self.hooks_started,"preview_suspended":self.preview && status.get("release_complete").and_then(Value::as_bool) == Some(true)
                && status.get("worker_pid").is_none_or(Value::is_null) && status.get("terminating").and_then(Value::as_bool) != Some(true),
            "shutdown_complete":self.done.contains("complete")});
        status.as_object_mut().expect("status object").extend(fields.as_object().expect("fields").clone()); status
    }
    fn suspend_shortcuts(&mut self, suspended: bool) {
        self.shortcuts_suspended = suspended; self.disarm();
        if suspended { self.bridge.set_shortcuts_enabled(false); }
        else { self.configure_native(); self.bridge.set_shortcuts_enabled(self.native_ready && !self.stopping); self.redraw_pending = self.native_ready; self.capture_after = self.now(); }
        self.refresh_arming();
    }
    pub fn begin_shutdown(&mut self) {
        if self.stopping { return; }
        self.finish_session(); self.stopping = true; self.bump_epoch(); self.preparer.request_stop();
        self.bridge.set_input_callback(None); self.bridge.set_shortcuts_enabled(false); self.clear(true, true); self.candidate = None; self.pending_edit = None;
        while let Ok(input) = self.inputs.try_recv() { if input.transaction || input.action == Action::Bypass { let _ = self.bridge.finish_input(false); } }
        if let Some(d) = &self.download { d.cancel.set(); }
        if let Some(s) = self.spelling.take() { self.spawn_spelling_cleanup(s); }
    }
    fn spawn_spelling_cleanup(&mut self, service: Arc<dyn Spelling>) {
        // Retain the service on spawn failure; no drop-and-forget pending cleanup.
        let holder = Arc::new(Mutex::new(Some(service))); let owned = holder.clone();
        match thread::Builder::new().name("brotypist-spelling-cleanup".into()).spawn(move || {
            let service = locked(&owned).take().expect("spelling cleanup owner");
            while !service.close() || !service.is_stopped() { thread::sleep(Duration::from_millis(10)); }
        }) { Ok(thread) => self.spelling_cleanup.push(thread), Err(_) => { self.spelling = locked(&holder).take(); self.error = "Native spelling cleanup is pending.".into(); } }
    }
    fn reap_spelling(&mut self) {
        let mut i = 0; while i < self.spelling_cleanup.len() { if self.spelling_cleanup[i].is_finished() { let t = self.spelling_cleanup.swap_remove(i); let _ = t.join(); } else { i += 1; } }
    }
    pub fn shutdown_step(&mut self) -> Result<bool> {
        self.begin_shutdown(); let mut failed = false;
        if !self.done.contains("bridge") { match self.bridge.stop() { Ok(()) => { self.done.insert("bridge"); }, Err(_) => failed = true } }
        if !self.done.contains("worker_signal") { match self.worker.request_stop() { Ok(()) => { self.done.insert("worker_signal"); }, Err(_) => failed = true } }
        if self.worker.is_stopped() { self.done.insert("worker"); }
        self.preparer.request_stop(); if self.preparer.is_stopped() { self.done.insert("preparer"); }
        if let Some(s) = self.spelling.take() { self.spawn_spelling_cleanup(s); }
        self.reap_spelling(); if self.spelling.is_none() && self.spelling_cleanup.is_empty() { self.done.insert("spelling"); }
        self.consume_download(); if self.download.is_none() { self.done.insert("download"); }
        if self.done.contains("preparer") && !self.done.contains("records") { if self.flush_records().is_ok() { self.done.insert("records"); } else { failed = true; } }
        if self.done.contains("preparer") && !self.done.contains("history") { locked(&self.storage).lock(); self.done.insert("history"); }
        let complete = ["bridge","worker","preparer","spelling","download","records","history"].iter().all(|name| self.done.contains(name));
        if complete { self.done.insert("complete"); }
        if failed { self.error = "Shutdown is incomplete. Try Quit again to finish releasing local resources.".into(); Err(anyhow!(self.error.clone())) } else { Ok(complete) }
    }
}
impl Engine {
    fn retry_native(&mut self) -> Result<()> {
        self.native_ready = false; self.bridge.set_shortcuts_enabled(false);
        self.clear(true, true); self.candidate = None; self.attempted = None;
        let config = self.native_config();
        if self.bridge.configure(&config).is_err() { self.fail("Native configuration"); return Err(anyhow!("Native configuration could not be applied.")); }
        if !self.hooks_started { self.hooks_started = self.bridge.start_shortcuts(self.shortcut_callback.clone()).is_ok(); }
        self.native_ready = self.hooks_started;
        self.bridge.set_shortcuts_enabled(self.native_ready && !self.shortcuts_suspended && !self.stopping);
        self.capture_after = self.now();
        self.emit(json!({"type":"native_started","started":self.hooks_started}));
        if self.hooks_started { self.error.clear(); Ok(()) }
        else { self.fail("Native shortcuts startup; check permissions and retry"); Err(anyhow!("Native shortcuts could not start.")) }
    }
}
impl Engine {
    fn owns_claim(&self, claim: &Claim) -> bool {
        match claim.mode.as_str() {
            "completion" => self.suggestion.snapshot.as_ref().is_some_and(|s| s.key() == claim.key && s.authority() == claim.authority) && !self.suggestion.text.is_empty() && self.suggestion.text.starts_with(&claim.visible),
            "correction" => self.correction.as_ref().is_some_and(|(s,result)| s.key() == claim.key && s.authority() == claim.authority && result.replacement == claim.visible),
            "alternatives" | "emoji" | "synonyms" => self.choices.as_ref().is_some_and(|c| c.snapshot.key() == claim.key && c.snapshot.authority() == claim.authority && c.mode == claim.mode),
            _ => false,
        }
    }
}
impl Engine {
    fn refresh_context(&mut self) {
        for update in self.preparer.context_updates() {
            if self.active() && update.epoch == self.current_epoch() && self.candidate.as_ref().is_some_and(|s| s.key() == update.key && s.authority() == update.authority) {
                self.context_status = if context_status(&update.status) { update.status } else { "error".into() };
                self.context_prefetched = Some((update.key, update.authority, update.epoch, self.now()));
            }
        }
        if !self.active() || !(self.config.screen_context || self.config.screenshot_context) || self.preparing.is_some() || self.request.is_some() { return; }
        let Some(s) = self.candidate.clone() else { return; };
        if s.protected || s.selection_length != 0 || s.prefix().trim().is_empty() || s.window_id.is_empty() || s.window_revision.is_empty() { return; }
        if self.context_prefetched.as_ref().is_some_and(|(key, authority, epoch, at)| *key == s.key() && *authority == s.authority() && *epoch == self.current_epoch() && self.now().saturating_duration_since(*at) <= Duration::from_secs(2)) { return; }
        self.context_prefetched = Some((s.key(), s.authority(), self.current_epoch(), self.now()));
        let ticket = self.bridge.as_mut().and_then(|bridge| self.readers.context_ticket(&s, &mut **bridge).ok().flatten());
        if let Some(ticket) = ticket {
            self.serial = self.serial.checked_add(1).expect("context identifier exhausted");
            self.context_status = "pending".into();
            self.preparer.prefetch(ContextPreparation { token: self.serial, epoch: self.current_epoch(), snapshot: s, config: self.native_config(), ticket });
        } else { self.context_status = "unavailable".into(); }
    }
}
fn native_spelling() -> Arc<dyn Spelling> {
    #[cfg(target_os = "macos")]
    let backend = crate::platform::macos::spelling::spelling_backend();
    #[cfg(not(target_os = "macos"))]
    let backend = None;
    Arc::new(SpellingService::new(backend, None))
}
fn valid_rect(r: &core::Rect) -> bool { r.iter().all(|v| v.is_finite()) && r[2] > 0.0 && r[3] > 0.0 }
fn authority_matches(a: &Authority, s: &Snapshot) -> bool {
    a.app_id == s.app_id && a.field_id == s.field_id && a.domain == s.domain && a.window_id == s.window_id
        && a.window_revision == s.window_revision && a.window_pid == s.window_pid
}
fn selected_word(s: &Snapshot) -> String {
    // Validation derives NFC only; the original selected spelling is inserted.
    if s.protected || !s.can_replace || !s.valid_range() || s.selection_length == 0 || s.selection_length > 80 { return String::new(); }
    let value = s.selected_text(); let normalized: String = value.nfc().collect();
    for (index, word) in normalized.split(' ').enumerate() {
        if index >= 5 || word.is_empty() { return String::new(); }
        let mut needs_letter = true;
        for c in word.chars() {
            if core::is_alphanumeric(c) && unicode_general_category::get_general_category(c) != unicode_general_category::GeneralCategory::DecimalNumber { needs_letter = false; }
            else if !needs_letter && "’'-".contains(c) { needs_letter = true; }
            else { return String::new(); }
        }
        if needs_letter { return String::new(); }
    }
    value.into()
}
fn edit_snapshot(before: &Snapshot, start: usize, length: usize, insertion: &str) -> Option<Snapshot> {
    let end = start.checked_add(length)?; let tail = before.text.scalar_len().checked_sub(end)?;
    let prefix = before.text.slice(0,start)?; let suffix = before.text.slice(end,tail)?;
    let mut text = String::with_capacity(prefix.len()+insertion.len()+suffix.len()); text.push_str(prefix); text.push_str(insertion); text.push_str(suffix);
    let mut expected = before.clone(); expected.text = text.into(); expected.caret = start + insertion.chars().count(); expected.selection_length = 0; Some(expected)
}
fn confirmed_edit(edit: &PendingEdit, readback: &Snapshot) -> bool {
    !readback.protected && readback.valid_range() && readback.same_authority(&edit.before) && readback.text == edit.expected.text
        && readback.caret == edit.expected.caret && readback.selection_length == 0 && readback.can_replace == edit.before.can_replace && readback.on_demand_only == edit.before.on_demand_only
}
fn update_authored(session: &mut Session, text: &str, user_authored: bool) {
    if session.text == text { return; }
    let old_len = session.authored.len(); let new_len = text.chars().count();
    let start = session.text.chars().zip(text.chars()).take_while(|(a,b)| a == b).count();
    let tail = session.text.chars().rev().take(old_len-start).zip(text.chars().rev().take(new_len-start)).take_while(|(a,b)| a == b).count();
    let old_end = old_len-tail; let new_end = new_len-tail; let authored = user_authored && (old_end-start).max(new_end-start) <= 64;
    session.authored.splice(start..old_end, std::iter::repeat_n(authored,new_end-start)); session.text = text.into();
}
fn authored_spans(flags: &[bool]) -> Vec<(usize,usize)> {
    let mut spans = Vec::new(); let mut start = None;
    for (index,flag) in flags.iter().copied().chain(std::iter::once(false)).enumerate() {
        if flag && start.is_none() { start = Some(index); } else if !flag { if let Some(first) = start.take() { spans.push((first,index)); if spans.len() > 1024 { return Vec::new(); } } }
    } spans
}
fn privacy_revoked(old: &Config, new: &Config) -> bool {
    [(old.global_enabled,new.global_enabled),(old.enabled,new.enabled),(old.clipboard_context,new.clipboard_context),(old.screen_context,new.screen_context),
        (old.screenshot_context,new.screenshot_context),(old.screenshot_appearance,new.screenshot_appearance),(old.record_history,new.record_history),(old.terminal_completion,new.terminal_completion)]
        .into_iter().any(|(a,b)| a && !b)
        || (old.personalization_strength != 0 && new.personalization_strength == 0 && recording_configured(old))
        || new.excluded_apps.iter().any(|app| !old.excluded_apps.contains(app))
        || old.app_rules.keys().chain(new.app_rules.keys()).any(|scope| ["enabled","record_history"].iter().any(|name| {
            let default_old = if *name == "enabled" { old.enabled } else { old.record_history }; let default_new = if *name == "enabled" { new.enabled } else { new.record_history };
            old.app_rules.get(scope).and_then(|r|r.get(*name)).and_then(Value::as_bool).unwrap_or(default_old)
                && !new.app_rules.get(scope).and_then(|r|r.get(*name)).and_then(Value::as_bool).unwrap_or(default_new)
        }))
}
pub fn model_inventory() -> Result<Vec<Value>> {
    models::presets().iter().map(|p| Ok(json!({"id":p.id,"label":p.label,"description":p.description,"path":models::model_path(p)?,"size_bytes":p.size_bytes,
        "installed":models::is_installed(p),"prompt_style":p.prompt_style,"license_name":p.license_name,"license_url":p.license_url,
        "requires_license_acceptance":p.requires_license_acceptance,"availability":models::model_availability(p,models::physical_memory_bytes())}))).collect()
}
fn context_status(s: &str) -> bool { ["off","pending","ready","empty","unavailable","blocked","stale","error","screen_permission"].contains(&s) }
fn context_message(s: &str) -> &'static str { match s {
    "pending" => "Reading eligible text from the focused window.", "ready" => "Optional reference text is available.", "empty" => "No relevant optional reference text is available.",
    "unavailable" => "This field does not expose safe focused-window Accessibility context.", "blocked" => "Optional context was blocked by current focus or privacy policy.",
    "stale" => "Focused-window context changed and was discarded.", "error" => "Optional context could not be read. Check Accessibility permissions or turn it off.",
    "screen_permission" => "Screen Recording permission is needed for screenshot context.", _ => "Optional context is off.",
} }
fn primary_claim_index(claims: &[Arc<Claim>], alternatives: bool) -> Option<usize> {
    for mode in ["correction", "completion"] { if let Some(index) = claims.iter().position(|claim| claim.mode == mode) { return Some(index); } }
    claims.iter().position(|claim| alternatives || claim.mode != "alternatives")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::InlineRender;
    use crate::platform::{Foreground, Permission, ShortcutHandler};

    #[derive(Clone, Copy, Default)]
    enum Outcome { #[default] Applied, AppliedNoGeometry, Dispatched, Rejected, Indeterminate, WrongReadback }
    struct NativeState {
        snapshot: Option<Snapshot>, revision: u64, secure: Option<bool>, hook: Option<ShortcutHandler>,
        wake: Option<Arc<dyn Fn() + Send + Sync>>, outcome: Outcome, edits: Vec<String>, finished: Vec<bool>,
        capture_count: usize, stop_calls: usize, stop_failures: usize, shortcuts: bool,
        start_failures: usize, start_calls: usize, configure_failures: usize,
        docs: Option<Value>, docs_reads: usize, metadata: Option<Foreground>, enables: usize, configured: Config,
    }
    struct Native(Rc<RefCell<NativeState>>);
    impl NativeBridge for Native {
        fn configure(&mut self, c: &Config) -> Result<()> { let mut s=self.0.borrow_mut();if s.configure_failures>0{s.configure_failures-=1;return Err(anyhow!("private native configuration"));}s.configured=c.clone();Ok(()) }
        fn capture(&mut self) -> Result<Option<Snapshot>> { let mut s = self.0.borrow_mut(); s.capture_count += 1; Ok(s.snapshot.clone()) }
        fn foreground(&mut self) -> Result<Option<Foreground>> {
            let s = self.0.borrow(); Ok(s.metadata.clone().or_else(|| s.snapshot.as_ref().map(|s| Foreground { app_id:s.app_id.clone(), domain:s.domain.clone(), window_id:s.window_id.clone(), window_pid:s.window_pid, ..Default::default() })))
        }
        fn insert(&mut self, s:&Snapshot, text:&str) -> Result<EditOutcome> { self.replace_range(s,s.caret,0,text) }
        fn replace_range(&mut self, before:&Snapshot,start:usize,length:usize,text:&str) -> Result<EditOutcome> {
            let mut s = self.0.borrow_mut(); s.edits.push(text.into());
            let expected = edit_snapshot(before,start,length,text).unwrap();
            Ok(match s.outcome {
                Outcome::Applied => { s.snapshot=Some(expected.clone()); EditOutcome::Applied(expected) }
                Outcome::AppliedNoGeometry => { let mut readback=expected;readback.caret_rect=None;readback.inline_render=None;readback.native_inline=false;readback.native_anchor.clear();s.snapshot=Some(readback.clone());EditOutcome::Applied(readback) }
                Outcome::Dispatched => EditOutcome::Dispatched,
                Outcome::Rejected => EditOutcome::Rejected,
                Outcome::Indeterminate => EditOutcome::Indeterminate,
                Outcome::WrongReadback => EditOutcome::Applied(before.clone()),
            })
        }
        fn range_rect(&mut self,_:&Snapshot,_:usize,_:usize)->Result<Option<core::Rect>>{Ok(Some([10.,20.,40.,18.]))}
        fn focused_context(&mut self,_:&Snapshot,_:&dyn Fn()->bool,_:usize)->Result<ContextRead>{Ok(ContextRead::blocked("accessibility"))}
        fn start_shortcuts(&mut self,callback:ShortcutHandler)->Result<()>{let mut s=self.0.borrow_mut();s.hook=Some(callback);s.start_calls+=1;if s.start_failures>0{s.start_failures-=1;Err(anyhow!("private native startup"))}else{Ok(())}}
        fn set_shortcuts_enabled(&mut self,enabled:bool){self.0.borrow_mut().shortcuts=enabled;}
        fn set_input_callback(&mut self,callback:Option<Arc<dyn Fn()+Send+Sync>>){self.0.borrow_mut().wake=callback;}
        fn finish_input(&mut self,accepted:bool)->Result<()>{self.0.borrow_mut().finished.push(accepted);Ok(())}
        fn input_revision(&self)->u64{self.0.borrow().revision}
        fn user_input_revision(&self)->u64{self.0.borrow().revision}
        fn input_timestamp(&self)->Option<Instant>{None}
        fn secure_input_enabled(&self)->Option<bool>{self.0.borrow().secure}
        fn request_permission(&mut self,_:&str)->Result<Permission>{Ok(Permission::Unknown)}
        fn status(&self)->Value{json!({"native":"synthetic consumer"})}
        fn stop(&mut self)->Result<()>{let mut s=self.0.borrow_mut();s.stop_calls+=1;if s.stop_failures>0{s.stop_failures-=1;Err(anyhow!("private detail"))}else{Ok(())}}
        fn atomic_replacement_supported(&self)->bool{true}
        fn docs_hint(&mut self)->Result<Option<Value>>{let mut s=self.0.borrow_mut();s.docs_reads+=1;Ok(s.docs.clone())}
        fn enable_docs_accessibility(&mut self,_:&str)->Result<Value>{self.0.borrow_mut().enables+=1;Ok(json!({"status":"unknown"}))}
    }
    #[derive(Default)]
    struct WorkerState { events:Vec<WorkerEvent>, submitted:Vec<(Config,String,String,GenerationOptions)>, prepared:usize, proceeds:Vec<u64>, cancelled:usize, released:usize, stopped:bool, stop_calls:usize }
    struct Inference(Rc<RefCell<WorkerState>>);
    impl Worker for Inference {
        fn prepare(&mut self,_:&Config)->Result<u64>{self.0.borrow_mut().prepared+=1;Ok(0)}
        fn submit(&mut self,c:&Config,p:&str,s:&str,o:&GenerationOptions)->Result<u64>{let mut w=self.0.borrow_mut();w.submitted.push((c.clone(),p.into(),s.into(),o.clone()));Ok(w.submitted.len() as u64)}
        fn proceed(&mut self,id:u64)->Result<()>{self.0.borrow_mut().proceeds.push(id);Ok(())}
        fn poll(&mut self)->Vec<WorkerEvent>{std::mem::take(&mut self.0.borrow_mut().events)}
        fn cancel(&mut self)->Result<()>{self.0.borrow_mut().cancelled+=1;Ok(())}
        fn release(&mut self)->Result<()>{self.0.borrow_mut().released+=1;Ok(())}
        fn set_idle_timeout(&mut self,_:u32){}
        fn status(&self)->Value{json!({"state":"idle","worker_pid":null,"terminating":false,"release_complete":true})}
        fn request_stop(&mut self)->Result<()>{self.0.borrow_mut().stop_calls+=1;Ok(())}
        fn is_stopped(&mut self)->bool{self.0.borrow().stopped}
    }
    #[derive(Default)]
    struct Stored { unlocked:bool, unlock_fail:bool, records:Vec<(String,String,String,Vec<(usize,usize)>)>, stats:Vec<(String,bool)>, deletes:usize, locked:usize }
    struct Memory(Arc<Mutex<Stored>>);
    impl Storage for Memory {
        fn unlock(&mut self)->Result<()>{let mut s=locked(&self.0);if s.unlock_fail{return Err(anyhow!("secret key failure"));}s.unlocked=true;Ok(())}
        fn lock(&mut self){let mut s=locked(&self.0);s.unlocked=false;s.locked+=1;}
        fn record(&mut self,_:&str,app:&str,host:&str,text:&str,spans:&[(usize,usize)])->Result<()>{locked(&self.0).records.push((app.into(),host.into(),text.into(),spans.into()));Ok(())}
        fn acceptance(&mut self,_:&str,text:&str,new:bool)->Result<()>{locked(&self.0).stats.push((text.into(),new));Ok(())}
        fn personalization(&self,_:&str,_:&str,_:&str,_:u32,_:&dyn Fn(&str,&str)->bool)->Result<String>{Ok(String::new())}
        fn delete(&mut self,_:Option<&str>,_:Option<&str>,_:bool)->Result<usize>{let mut s=locked(&self.0);let n=s.records.len();s.records.clear();s.deletes+=1;Ok(n)}
        fn counts(&self,_:bool)->Result<Value>{Ok(json!({"available":true,"apps":{},"domains":{}}))}
        fn info(&self)->Result<Value>{Ok(json!({"count":locked(&self.0).records.len()}))}
        fn statistics(&self,_:&str,_:&str)->Result<Value>{Ok(json!({"acceptances":locked(&self.0).stats.len()}))}
        fn clear_statistics(&mut self)->Result<()>{locked(&self.0).stats.clear();Ok(())}
    }
    #[derive(Default)]
    struct PreparationQueue { pending:Option<Preparation>, results:Vec<Prepared>, cancelled:usize, stopped:bool }
    struct PreparationConsumer(Rc<RefCell<PreparationQueue>>);
    impl Preparer for PreparationConsumer {
        fn submit(&mut self,p:Preparation){self.0.borrow_mut().pending=Some(p);}
        fn prefetch(&mut self,_:ContextPreparation){}
        fn context_updates(&mut self)->Vec<ContextUpdate>{Vec::new()}
        fn cancel(&mut self){let mut p=self.0.borrow_mut();p.cancelled+=1;p.pending=None;}
        fn poll(&mut self)->Vec<Prepared>{std::mem::take(&mut self.0.borrow_mut().results)}
        fn request_stop(&mut self){}
        fn is_stopped(&mut self)->bool{self.0.borrow().stopped}
    }
    struct Time(Rc<RefCell<Instant>>);
    impl Clock for Time {fn now(&self)->Instant{*self.0.borrow()}}
    struct Services(Rc<RefCell<system::PowerStatus>>);
    impl Readers for Services {
        fn power(&mut self)->system::PowerStatus{self.0.borrow().clone()}
        fn persist(&mut self,_:&Config)->Result<()>{Ok(())}
    }
    #[derive(Default)]
    struct SpellState { ready:bool, suspected:bool, replacement:String, close_calls:usize, release:bool }
    struct Dictionary(Arc<Mutex<SpellState>>);
    impl Spelling for Dictionary {
        fn assess(&self,s:&Snapshot)->Option<SpellingResult>{
            let state=locked(&self.0);state.ready.then(||SpellingResult{key:s.key(),authority:s.authority(),start:0,length:s.caret,word:s.prefix().into(),replacement:state.replacement.clone(),suspected:state.suspected,language:"en".into(),reason:"native fixture".into(),finished:true})
        }
        fn invalidate(&self){}
        fn close(&self)->bool{let mut s=locked(&self.0);s.close_calls+=1;s.release}
        fn is_stopped(&self)->bool{locked(&self.0).release}
    }
    struct Harness {
        engine:Engine, native:Rc<RefCell<NativeState>>, worker:Rc<RefCell<WorkerState>>,
        preparation:Rc<RefCell<PreparationQueue>>, storage:Arc<Mutex<Stored>>, time:Rc<RefCell<Instant>>, power:Rc<RefCell<system::PowerStatus>>,
    }
    fn field(text:&str)->Snapshot {
        Snapshot{app_id:"writer".into(),field_id:"body".into(),text:text.into(),caret:text.chars().count(),caret_rect:Some([10.,20.,1.,18.]),
            field_rect:Some([0.,0.,500.,80.]),native_anchor:"verified".into(),native_inline:true,window_id:"window-1".into(),
            window_revision:"document-1".into(),window_pid:42,..Default::default()}
    }
    fn harness(config:Config,spelling:Option<Arc<dyn Spelling>>)->Harness{harness_start(config,spelling,0)}
    fn harness_start(config:Config,spelling:Option<Arc<dyn Spelling>>,start_failures:usize)->Harness{
        let native=Rc::new(RefCell::new(NativeState{snapshot:Some(field("The update")),revision:1,secure:Some(false),hook:None,wake:None,
            outcome:Outcome::Applied,edits:Vec::new(),finished:Vec::new(),capture_count:0,stop_calls:0,stop_failures:0,shortcuts:false,start_failures:0,start_calls:0,configure_failures:0,docs:None,docs_reads:0,metadata:None,enables:0,configured:config.clone()}));
        native.borrow_mut().start_failures=start_failures;
        let worker=Rc::new(RefCell::new(WorkerState::default()));let preparation=Rc::new(RefCell::new(PreparationQueue::default()));
        let storage=Arc::new(Mutex::new(Stored::default()));let time=Rc::new(RefCell::new(Instant::now()));let power=Rc::new(RefCell::new(system::PowerStatus::default()));
        let engine=Engine::new(config,Box::new(Native(native.clone())),Box::new(Inference(worker.clone())),Arc::new(Mutex::new(Box::new(Memory(storage.clone())))),
            Box::new(PreparationConsumer(preparation.clone())),spelling,Box::new(Services(power.clone())),Box::new(Time(time.clone())),Arc::new(AtomicU64::new(1))).unwrap();
        Harness{engine,native,worker,preparation,storage,time,power}
    }
    impl Harness {
        fn elapse(&mut self,millis:u64){*self.time.borrow_mut()+=Duration::from_millis(millis);}
        fn observe(&mut self){self.elapse(101);self.engine.tick();}
        fn force(&mut self){self.engine.command(Command::Action(Action::Force));self.engine.tick();}
        fn prepared(&mut self){
            let p=self.preparation.borrow_mut().pending.take().expect("queued preparation");
            self.preparation.borrow_mut().results.push(Prepared{token:p.token,epoch:p.epoch,options:GenerationOptions::default(),context_status:"off".into(),warning:String::new(),reference_expires:None});
            self.engine.tick();
        }
        fn response(&mut self,kind:&str,text:&str,final_:bool){let id=self.worker.borrow().submitted.len() as u64;self.worker.borrow_mut().events.push(WorkerEvent{id,kind:kind.into(),text:text.into(),final_,model_loaded:true,message:String::new()});self.engine.tick();}
        fn offer(&mut self,text:&str){self.force();self.prepared();self.response("ready","",false);self.response("result",text,true);}
        fn drawn(&mut self,visible:&str,mode:&str){let s=self.native.borrow().snapshot.clone().unwrap();let revision=self.native.borrow().revision;self.engine.command(Command::PresentationResult{key:s.key(),authority:s.authority(),revision,visible_text:visible.into(),mode:mode.into()});}
        fn invoke_hook(&self,action:Action)->bool{let callback=self.native.borrow().hook.clone().unwrap();callback(action)}
    }
    #[test]
    fn ready_requires_fresh_native_authority_and_geometry_does_not_regenerate(){
        let mut h=harness(Config::default(),None);h.force();h.prepared();assert!(h.worker.borrow().proceeds.is_empty());
        h.native.borrow_mut().snapshot.as_mut().unwrap().window_revision="other-document".into();
        h.response("ready","",false);assert!(h.worker.borrow().proceeds.is_empty());assert!(h.native.borrow().edits.is_empty());
        h.offer(" arrives tomorrow");let submitted=h.worker.borrow().submitted.len();
        h.native.borrow_mut().snapshot.as_mut().unwrap().caret_rect=Some([20.,20.,1.,18.]);h.observe();
        assert_eq!(h.worker.borrow().submitted.len(),submitted);
        assert!(h.engine.drain_events().iter().any(|v|v["type"]=="suggestion"&&v["authority"]["window_revision"]=="other-document"));
    }
    #[test]
    fn window_only_change_rejects_output_and_old_presentation_ack(){
        let mut h=harness(Config::default(),None);h.offer(" arrives");let old=h.native.borrow().snapshot.clone().unwrap();
        h.native.borrow_mut().snapshot.as_mut().unwrap().window_id="window-2".into();h.observe();
        h.engine.command(Command::PresentationResult{key:old.key(),authority:old.authority(),revision:1,visible_text:" arrives".into(),mode:"completion".into()});
        assert!(!h.invoke_hook(Action::Word));h.engine.command(Command::Action(Action::Word));assert!(h.native.borrow().edits.is_empty());
    }
    #[test]
    fn no_hidden_or_truncated_word_can_be_accepted_and_claim_is_single_use(){
        let mut h=harness(Config::default(),None);h.offer(" arrives tomorrow");assert!(!h.invoke_hook(Action::Word));
        h.drawn(" arr","completion");assert!(h.invoke_hook(Action::Word));assert!(!h.invoke_hook(Action::Word));h.engine.tick();
        assert!(h.native.borrow().edits.is_empty());assert_eq!(h.native.borrow().finished,vec![false]);
        h.drawn(" arrives","completion");assert!(h.invoke_hook(Action::All));h.engine.tick();assert_eq!(h.native.borrow().edits,vec![" arrives "]);
        assert_eq!(locked(&h.storage).stats.len(),1);
    }
    #[test]
    fn final_completion_waits_for_pending_spelling_instead_of_losing_explicit_force(){
        let spelling=Arc::new(Mutex::new(SpellState::default()));let mut c=Config::default();c.typo_suppression=true;
        let mut h=harness(c,Some(Arc::new(Dictionary(spelling.clone()))));h.offer(" arrives tomorrow");
        assert!(!h.engine.drain_events().iter().any(|e|e["type"]=="suggestion"));
        locked(&spelling).ready=true;h.engine.tick();assert!(h.engine.drain_events().iter().any(|e|e["type"]=="suggestion"&&e["text"]==" arrives tomorrow"));
        locked(&spelling).release=true;
    }
    #[test]
    fn dispatched_edit_counts_only_after_exact_observation_and_uncertain_is_never_replayed(){
        for outcome in [Outcome::Dispatched,Outcome::Indeterminate,Outcome::WrongReadback] {
            let mut h=harness(Config::default(),None);h.offer(" arrives");h.drawn(" arrives","completion");h.native.borrow_mut().outcome=outcome;
            assert!(h.invoke_hook(Action::All));h.engine.tick();assert!(locked(&h.storage).stats.is_empty());assert_eq!(h.native.borrow().finished,vec![true]);
            assert!(!h.invoke_hook(Action::All));h.engine.tick();assert_eq!(h.native.borrow().edits.len(),1);
            if matches!(outcome,Outcome::Dispatched) {
                let before=h.native.borrow().snapshot.clone().unwrap();h.native.borrow_mut().snapshot=edit_snapshot(&before,before.caret,0," arrives");h.observe();
                assert_eq!(locked(&h.storage).stats,vec![(" arrives".into(),true)]);
            }
        }
    }
    #[test]
    fn rejected_native_edit_returns_original_shortcut_without_accounting(){
        let mut h=harness(Config::default(),None);h.offer(" arrives");h.drawn(" arrives","completion");h.native.borrow_mut().outcome=Outcome::Rejected;
        assert!(h.invoke_hook(Action::Word));h.engine.tick();assert_eq!(h.native.borrow().finished,vec![false]);assert!(locked(&h.storage).stats.is_empty());
        assert_eq!(h.native.borrow().snapshot.as_ref().unwrap().text.as_ref(),"The update");
    }
    #[test]
    fn revoked_clipboard_consent_and_superseded_preparation_cannot_submit(){
        let mut c=Config::default();c.clipboard_context=true;let mut h=harness(c,None);h.force();
        let p=h.preparation.borrow_mut().pending.take().unwrap();let mut updated=h.engine.config.clone();updated.clipboard_context=false;
        h.engine.command(Command::ApplyConfig{config:updated,persist:false});
        h.preparation.borrow_mut().results.push(Prepared{token:p.token,epoch:p.epoch,options:GenerationOptions{context:"must never submit".into(),..Default::default()},context_status:"ready".into(),warning:String::new(),reference_expires:None});
        h.engine.tick();assert!(h.worker.borrow().submitted.is_empty());assert_eq!(h.worker.borrow().released,1);
    }
    #[test]
    fn same_text_after_real_input_revokes_acceptance_but_passthrough_dismissal_keeps_actual_emoji_mode(){
        let mut c=Config::default();c.escape_behavior="passthrough".into();let mut h=harness(c,None);
        h.native.borrow_mut().snapshot=Some(field("Hi :smile"));h.observe();
        let event=h.engine.drain_events().into_iter().find(|e|e["type"]=="suggestion").unwrap();assert_eq!(event["mode"],"emoji");
        let rows=event["choices"].as_array().unwrap().iter().map(|s|s.as_str().unwrap()).collect::<Vec<_>>().join("\n");h.drawn(&rows,"emoji");
        assert!(!h.invoke_hook(Action::Dismiss));h.native.borrow_mut().revision+=1;
        let wake=h.native.borrow().wake.clone().unwrap();wake();h.engine.tick();h.observe();
        assert!(!h.engine.drain_events().iter().any(|e|e["type"]=="suggestion"&&e["mode"]=="emoji"));
    }
    #[test]
    fn selection_synonyms_replace_only_selected_range_and_alternatives_do_not_steal_tab(){
        let mut c=Config::default();c.prompt_style="writing".into();c.alternatives_enabled=true;
        let mut h=harness(c,None);let mut selected=field("A useful update");selected.caret=2;selected.selection_length=6;h.native.borrow_mut().snapshot=Some(selected);
        h.engine.command(Command::Action(Action::Synonyms));h.engine.tick();h.prepared();h.response("ready","",false);h.response("result","[\"helpful\",\"practical\",\"valuable\"]",true);
        h.drawn("helpful\npractical","synonyms");h.engine.command(Command::Action(Action::Choice(0)));h.engine.tick();assert_eq!(h.native.borrow().snapshot.as_ref().unwrap().text.as_ref(),"A helpful update");
        h.native.borrow_mut().snapshot=Some(field("The update"));h.observe();h.engine.command(Command::Action(Action::Alternatives));h.engine.tick();h.prepared();h.response("ready","",false);h.response("result","[\" arrives\",\" is ready\",\" will ship\"]",true);
        h.drawn(" arrives\n is ready","alternatives");assert!(!h.invoke_hook(Action::Word));h.engine.command(Command::Action(Action::Choice(1)));h.engine.tick();assert_eq!(h.native.borrow().snapshot.as_ref().unwrap().text.as_ref(),"The update is ready");
    }
    #[test]
    fn on_demand_append_only_fields_allow_force_but_no_automatic_tools(){
        let mut c=Config::default();c.alternatives_enabled=true;c.alternatives_mode="pause".into();let mut h=harness(c,None);
        {let mut n=h.native.borrow_mut();let s=n.snapshot.as_mut().unwrap();s.on_demand_only=true;s.can_replace=false;}
        h.observe();h.elapse(5000);h.engine.tick();assert!(h.preparation.borrow().pending.is_none());assert_eq!(h.worker.borrow().prepared,0);
        h.offer(" arrives");h.drawn(" arrives","completion");h.engine.command(Command::Action(Action::All));h.engine.tick();assert_eq!(h.native.borrow().edits,vec![" arrives"]);
    }
    #[test]
    fn app_session_overrides_expire_without_persisting_and_pause_releases_then_resumes(){
        let mut c=Config::default();c.enabled=false;c.app_rules.insert("app:writer".into(),BTreeMap::from([("enabled".into(),json!(true))]));
        let mut h=harness(c,None);h.observe();assert_eq!(h.engine.status()["current_app_enabled"],true);
        h.engine.command(Command::ToggleApp{app:"writer".into(),domain:String::new()});assert_eq!(h.engine.status()["current_app_enabled"],false);
        assert_eq!(h.engine.config.app_rules["app:writer"]["enabled"],true);h.elapse(300001);h.engine.tick();assert_eq!(h.engine.status()["current_app_enabled"],true);
        h.engine.command(Command::Pause{minutes:1});assert_eq!(h.worker.borrow().released,1);assert!(!h.native.borrow().configured.global_enabled);
        h.elapse(60001);h.engine.tick();assert_eq!(h.engine.status()["enabled"],true);assert!(h.native.borrow().configured.global_enabled);
    }
    #[test]
    fn battery_force_only_and_explicit_zero_residency_preserve_load_without_evaluation(){
        let mut c=Config::default();c.idle_release_seconds=0;c.battery_on_demand=true;let mut h=harness(c,None);
        *h.power.borrow_mut()=system::PowerStatus{available:true,has_battery:true,on_battery:true,charging:Some(false),percent:Some(18)};
        h.observe();h.elapse(1000);h.engine.tick();assert_eq!(h.worker.borrow().prepared,0);assert!(h.worker.borrow().submitted.is_empty());
        h.force();h.prepared();assert_eq!(h.worker.borrow().submitted.len(),1);assert!(h.worker.borrow().proceeds.is_empty());
        h.engine.command(Command::Pause{minutes:0});h.power.borrow_mut().on_battery=false;h.elapse(5001);h.engine.tick();assert!(h.worker.borrow().prepared>0);assert_eq!(h.worker.borrow().submitted.len(),1);
    }
    #[test]
    fn authored_provenance_never_marks_generated_or_initial_text_as_typing(){
        let mut c=Config::default();c.record_history=true;c.record_unaccepted=true;let mut h=harness(c,None);
        h.observe();h.native.borrow_mut().snapshot=Some(field("The update is"));h.observe();h.offer(" ready");h.drawn(" ready","completion");h.engine.command(Command::Action(Action::All));h.engine.tick();
        let records=&locked(&h.storage).records;let record=records.last().unwrap();assert_eq!(record.2,"The update is ready");assert_eq!(record.3,vec![(10,13)]);
    }
    #[test]
    fn scoped_deletion_invalidates_derived_context_and_suppresses_unchanged_draft(){
        let mut c=Config::default();c.record_history=true;c.record_unaccepted=true;let mut h=harness(c,None);h.observe();h.offer(" ready");h.drawn(" ready","completion");h.engine.command(Command::Action(Action::All));h.engine.tick();
        h.engine.command(Command::DeleteHistory{app:Some("writer".into()),domain:None});assert_eq!(locked(&h.storage).deletes,1);assert!(locked(&h.storage).records.is_empty());h.observe();
        h.engine.command(Command::Pause{minutes:1});assert!(locked(&h.storage).records.is_empty());assert!(h.worker.borrow().released>=1);
    }
    #[test]
    fn unknown_security_never_reads_text_and_history_unlock_failure_does_not_save_consent(){
        let mut h=harness(Config::default(),None);h.native.borrow_mut().secure=None;h.engine.tick();assert_eq!(h.native.borrow().capture_count,0);
        locked(&h.storage).unlock_fail=true;let mut c=h.engine.config.clone();c.record_history=true;
        h.engine.command(Command::ApplyConfig{config:c,persist:true});assert!(!h.engine.config.record_history);assert!(h.engine.drain_events().iter().any(|e|e["type"]=="error"));
    }
    #[test]
    fn shutdown_retries_only_incomplete_owners_and_never_forgets_spelling_cleanup(){
        let mut h=harness(Config::default(),None);h.native.borrow_mut().stop_failures=1;h.engine.begin_shutdown();assert!(h.engine.shutdown_step().is_err());
        assert_eq!(h.worker.borrow().stop_calls,1);assert!(!h.engine.shutdown_step().unwrap());assert_eq!(h.native.borrow().stop_calls,2);
        h.worker.borrow_mut().stopped=true;h.preparation.borrow_mut().stopped=true;assert!(h.engine.shutdown_step().unwrap());assert_eq!(h.native.borrow().stop_calls,2);assert_eq!(h.worker.borrow().stop_calls,1);
    }
    #[test]
    fn nonmutating_shortcut_suspension_and_preview_are_distinct(){
        let mut h=harness(Config::default(),None);h.offer(" ready");let releases=h.worker.borrow().released;
        h.engine.command(Command::SuspendShortcuts{suspended:true});assert!(!h.native.borrow().shortcuts);assert_eq!(h.worker.borrow().released,releases);
        assert!(!h.invoke_hook(Action::Word));h.engine.command(Command::SuspendShortcuts{suspended:false});assert!(h.native.borrow().shortcuts);
        h.engine.command(Command::SuspendForPreview);assert_eq!(h.engine.status()["preview_suspended"],true);assert_eq!(h.worker.borrow().released,releases+1);
        h.engine.command(Command::ResumeFromPreview);assert_eq!(h.engine.status()["preview"],false);
    }
    #[test]
    fn authored_scalar_ranges_handle_unicode_replacement_and_bulk_paste_abstention(){
        let s=field("a🖋e\u{301}");let mut session=Session{key:s.authority(),id:"synthetic".into(),text:s.text.to_string(),authored:vec![false;4],accepted:false};
        update_authored(&mut session,"a🖋e\u{301}界",true);assert_eq!(authored_spans(&session.authored),vec![(4,5)]);
        update_authored(&mut session,&"x".repeat(100),true);assert!(authored_spans(&session.authored).is_empty());
    }
    #[test]
    fn correction_dismissal_and_replacement_require_current_authority(){
        let spell=Arc::new(Mutex::new(SpellState{ready:true,suspected:true,replacement:"update".into(),release:true,..Default::default()}));
        let mut c=Config::default();c.typo_fixes=true;c.escape_behavior="passthrough".into();let mut h=harness(c,Some(Arc::new(Dictionary(spell))));
        {let mut n=h.native.borrow_mut();let s=n.snapshot.as_mut().unwrap();s.inline_render=Some(InlineRender{font_name:"Fixture".into(),font_size:14.,baseline:34.,clip_rect:[0.,0.,500.,80.],ascent:12.,descent:3.,source:"native".into(),..Default::default()});}
        h.observe();let events=h.engine.drain_events();assert!(events.iter().any(|e|e["mode"]=="correction"));
        h.drawn("update","correction");h.engine.command(Command::Action(Action::Dismiss));h.observe();assert!(!h.engine.drain_events().iter().any(|e|e["mode"]=="correction"));
    }
    #[test]
    fn docs_help_is_metadata_only_single_use_and_document_identity_revokes_it(){
        let mut h=harness(Config::default(),None);
        {
            let mut n=h.native.borrow_mut();n.snapshot=None;
            n.metadata=Some(Foreground{app_id:"writer".into(),domain:"docs.google.com".into(),window_id:"docs-window".into(),window_pid:42,document_id:"doc-a".into(),..Default::default()});
            n.docs=Some(json!({"token":"capability-a","app_id":"writer","support":"unknown","help_url":"https://support.google.com/docs/"}));
        }
        h.engine.tick();let events=h.engine.drain_events();assert!(events.iter().any(|e|e["type"]=="docs_help"&&e["hint"]["token"]=="capability-a"));
        assert!(!h.invoke_hook(Action::Word));h.engine.tick();assert_eq!(h.native.borrow().docs_reads,1);assert!(h.worker.borrow().submitted.is_empty());
        h.engine.command(Command::EnableDocs{token:"capability-a".into()});h.engine.command(Command::EnableDocs{token:"capability-a".into()});
        assert_eq!(h.native.borrow().enables,1);assert!(h.native.borrow().edits.is_empty());
        h.elapse(2001);h.engine.tick();h.engine.drain_events();
        h.native.borrow_mut().metadata.as_mut().unwrap().document_id="doc-b".into();h.engine.tick();
        h.engine.command(Command::EnableDocs{token:"capability-a".into()});assert_eq!(h.native.borrow().enables,1);
        assert!(h.engine.drain_events().iter().any(|e|e["type"]=="docs_help"&&e["hint"].is_null()));
    }
    #[test]
    fn pending_spelling_cleanup_remains_owned_until_retry_can_finish(){
        let state=Arc::new(Mutex::new(SpellState::default()));
        let mut h=harness(Config::default(),Some(Arc::new(Dictionary(state.clone()))));
        h.worker.borrow_mut().stopped=true;h.preparation.borrow_mut().stopped=true;
        h.engine.begin_shutdown();assert!(!h.engine.shutdown_step().unwrap());assert!(Arc::strong_count(&state)>1);
        locked(&state).release=true;
        // Join only the injected cleanup boundary to provide deterministic worker
        // completion; shutdown itself never waits for this service.
        for thread in std::mem::take(&mut h.engine.spelling_cleanup) { thread.join().unwrap(); }
        assert!(locked(&state).close_calls>0);assert!(h.engine.shutdown_step().unwrap());
    }
    struct ContextTicket {
        snapshot:Snapshot, gate:Option<Arc<(Mutex<bool>,Condvar)>>, entered:Option<SyncSender<()>>, text:String,
    }
    impl platform::DetachedContextRead for ContextTicket {
        fn matches_snapshot(&self,s:&Snapshot)->bool{self.snapshot.same_key(s)&&self.snapshot.same_authority(s)}
        fn matches(&self)->Result<bool>{Ok(true)}
        fn read(&self,authorized:&dyn Fn()->bool,_:usize)->Result<ContextRead>{
            if let Some(entered)=&self.entered{let _=entered.try_send(());}
            if let Some(gate)=&self.gate{
                let state=locked(&gate.0);drop(gate.1.wait_timeout_while(state,Duration::from_secs(2),|ready|!*ready).unwrap());
            }
            Ok(if authorized(){ContextRead{text:self.text.clone(),status:"ready".into(),source:"accessibility".into()}}else{ContextRead::blocked("accessibility")})
        }
    }
    fn preparation(snapshot:Snapshot,token:u64,epoch:u64,ticket:Option<Box<dyn platform::DetachedContextRead>>)->Preparation{
        let mut config=Config::default();config.screen_context=true;config.personalization_strength=0;
        Preparation{token,epoch,snapshot,config,mode:"completion".into(),anchor:String::new(),references:Vec::new(),context_ticket:ticket,authorization_ticket:None,hard_disabled_apps:BTreeSet::new()}
    }
    struct SyntheticSources;
    impl BackgroundReaders for SyntheticSources {
        fn clipboard(&self,_:Arc<dyn Fn()->bool+Send+Sync>)->Result<String>{panic!("clipboard must be independently opted in and authorized")}
        fn screen(&self,snapshot:&Snapshot,ticket:&dyn platform::DetachedContextRead,guard:&dyn Fn()->bool,max:usize,ax:bool,_:bool)->Result<ContextRead>{
            if !ax || !guard() || !ticket.matches_snapshot(snapshot) || !ticket.matches()? { return Ok(ContextRead::blocked("accessibility")); }
            let result=ticket.read(guard,max)?;if guard()&&ticket.matches()? {Ok(result)}else{Ok(ContextRead::blocked("accessibility"))}
        }
    }
    #[test]
    fn obsolete_blocked_window_reader_does_not_hold_latest_preparation_or_reuse_reference(){
        let stored=Arc::new(Mutex::new(Stored::default()));let storage:SharedStorage=Arc::new(Mutex::new(Box::new(Memory(stored))));
        let epoch=Arc::new(AtomicU64::new(1));let mut service=AsyncPreparer::new(storage,epoch.clone(),Arc::new(SyntheticSources)).unwrap();
        let gate=Arc::new((Mutex::new(false),Condvar::new()));let (entered,receive)=mpsc::sync_channel(1);let snapshot=field("Thank you for the launch review");
        service.submit(preparation(snapshot.clone(),1,1,Some(Box::new(ContextTicket{snapshot:snapshot.clone(),gate:Some(gate.clone()),entered:Some(entered),text:"OLD_PRIVATE_REFERENCE".into()}))));
        receive.recv_timeout(Duration::from_secs(1)).unwrap();
        epoch.store(2,Ordering::Release);service.cancel();service.submit(preparation(snapshot,2,2,None));
        let state=locked(&service.state.0);
        let (mut state,wait)=service.state.1.wait_timeout_while(state,Duration::from_secs(1),|s|s.results.is_empty()).unwrap();
        assert!(!wait.timed_out());let latest=state.results.pop_front().unwrap();drop(state);
        assert_eq!(latest.token,2);assert!(latest.options.context.is_empty());
        *locked(&gate.0)=true;gate.1.notify_all();service.request_stop();drop(service);
    }
    #[test]
    fn history_and_statistics_commands_return_results_without_enabling_recording(){
        let mut h=harness(Config::default(),None);
        h.engine.command(Command::HistoryInfo);h.engine.command(Command::HistoryCounts{unlock:true});
        h.engine.command(Command::Statistics{period:"week".into(),group_by:"day".into()});h.engine.command(Command::ClearStatistics);
        let events=h.engine.drain_events();for kind in ["history_info","history_counts","statistics","statistics_cleared"] {assert!(events.iter().any(|e|e["type"]==kind));}
        assert!(!h.engine.config.record_history);assert!(!locked(&h.storage).unlocked);
    }
    #[test]
    fn withdrawn_history_and_statistics_consent_filter_already_queued_acceptances(){
        let mut c=Config::default();c.record_history=true;let mut h=harness(c,None);h.offer(" ready");h.drawn(" ready","completion");
        h.engine.command(Command::Action(Action::All));
        let mut c=h.engine.config.clone();c.record_history=false;c.statistics_enabled=false;h.engine.command(Command::ApplyConfig{config:c,persist:false});h.engine.tick();
        assert!(locked(&h.storage).records.is_empty());assert!(locked(&h.storage).stats.is_empty());
    }
    #[test]
    fn uncertain_generated_observation_never_becomes_authored_learning(){
        let mut c=Config::default();c.record_history=true;c.record_unaccepted=true;let mut h=harness(c,None);h.observe();
        h.native.borrow_mut().snapshot=Some(field("The update X"));h.observe();h.offer(" ready");h.drawn(" ready","completion");
        h.native.borrow_mut().outcome=Outcome::Indeterminate;h.engine.command(Command::Action(Action::All));h.engine.tick();
        h.native.borrow_mut().snapshot=Some(field("The update X ready"));h.observe();h.engine.command(Command::Pause{minutes:1});
        let records=locked(&h.storage);assert_eq!(records.records.last().unwrap().3,vec![(10,12)]);assert!(records.stats.is_empty());
    }
    #[test]
    fn dispatched_text_matching_later_user_typing_is_not_native_acceptance(){
        let mut h=harness(Config::default(),None);h.offer(" ready");h.drawn(" ready","completion");h.native.borrow_mut().outcome=Outcome::Dispatched;
        h.engine.command(Command::Action(Action::All));h.engine.tick();
        {let mut n=h.native.borrow_mut();n.revision+=1;n.snapshot=Some(field("The update ready"));}
        h.observe();assert!(locked(&h.storage).stats.is_empty());assert_eq!(h.native.borrow().edits.len(),1);
    }
    #[test]
    fn partial_remainder_uses_verified_advance_but_waits_for_current_geometry_before_another_edit(){
        let mut h=harness(Config::default(),None);h.offer(" arrives tomorrow");h.drawn(" arrives tomorrow","completion");
        h.native.borrow_mut().outcome=Outcome::AppliedNoGeometry;h.engine.drain_events();h.engine.command(Command::Action(Action::Word));
        assert!(h.engine.drain_events().iter().any(|e|e["type"]=="suggestion"&&e["text"]=="tomorrow"&&e["advance"]==" arrives "));
        h.drawn("tomorrow","completion");assert!(h.invoke_hook(Action::Word));h.engine.tick();
        assert_eq!(h.native.borrow().edits.len(),1);assert_eq!(h.native.borrow().finished,vec![false]);assert_eq!(locked(&h.storage).stats.len(),1);
    }
    #[test]
    fn native_startup_failure_retains_settings_and_shutdown_then_explicit_retry_recovers(){
        let mut h=harness_start(Config::default(),None,1);assert_eq!(h.engine.status()["state"],"error");h.engine.tick();assert_eq!(h.native.borrow().capture_count,0);
        h.engine.command(Command::Action(Action::Settings));assert!(h.engine.drain_events().iter().any(|e|e["type"]=="settings"));assert!(!h.native.borrow().shortcuts);
        h.engine.command(Command::RetryNative);h.observe();assert_eq!(h.engine.status()["native_started"],true);assert!(h.native.borrow().capture_count>0);assert_eq!(h.native.borrow().start_calls,2);
        h.worker.borrow_mut().stopped=true;h.preparation.borrow_mut().stopped=true;assert!(h.engine.shutdown_step().unwrap());
    }
    #[test]
    fn failed_policy_reconfiguration_never_reads_under_the_previous_policy(){
        let mut h=harness(Config::default(),None);h.native.borrow_mut().configure_failures=2;
        h.engine.command(Command::ApplyConfig{config:h.engine.config.clone(),persist:false});h.engine.tick();assert_eq!(h.engine.status()["state"],"error");assert_eq!(h.native.borrow().capture_count,0);assert!(!h.native.borrow().shortcuts);
        h.engine.command(Command::RetryNative);h.observe();assert!(h.native.borrow().capture_count>0);
    }
}
