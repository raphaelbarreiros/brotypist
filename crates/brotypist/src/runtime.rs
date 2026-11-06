//! Main-thread native controller. Hooks only claim cached presentations and enqueue
//! transactions; model evaluation and optional-data preparation have owned workers.
use crate::{
    config::{self, Config},
    core::{self, Authority, Snapshot, SnapshotKey, SuggestionState},
    features, models,
    platform::{self, Action, ContextRead, EditOutcome, NativeBridge},
    process_client::WorkerClient,
    providers::GenerationOptions,
    spelling::{SpellingResult, SpellingService},
    storage::HistoryStore,
    system,
    worker::WorkerEvent,
};
use anyhow::{Result, anyhow, ensure};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, VecDeque},
    rc::Rc,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use unicode_normalization::UnicodeNormalization;

mod native_client;
pub(crate) mod owner_wake;
use owner_wake::OwnerWake;
#[cfg(feature = "native-smoke")]
#[path = "stage_diagnostics.rs"]
pub(crate) mod stage_diagnostics;
#[cfg(feature = "native-smoke")]
use stage_diagnostics::{Span as DiagnosticSpan, Stage as TraceStage};

#[derive(Clone, Debug)]
pub enum Command {
    Action(Action),
    ApplyConfig {
        config: Config,
        persist: bool,
    },
    PresentationResult {
        key: SnapshotKey,
        authority: Authority,
        /// Echo the presentation's version unchanged, not the current native revision.
        revision: u64,
        visible_text: String,
        mode: String,
    },
    Pause {
        minutes: u32,
    },
    Resume,
    ToggleApp {
        app: String,
        domain: String,
    },
    SetModel {
        path: String,
        prompt_style: String,
        model_id: Option<String>,
    },
    DownloadModel {
        id: String,
        accept_license: bool,
    },
    CancelDownload,
    DeleteHistory {
        app: Option<String>,
        domain: Option<String>,
    },
    HistoryCounts {
        unlock: bool,
    },
    Statistics {
        period: String,
        group_by: String,
    },
    ClearStatistics,
    EnableDocs {
        token: String,
    },
    Models,
    HistoryInfo,
    SetHistoryEnabled {
        enabled: bool,
    },
    SuspendForPreview,
    ResumeFromPreview,
    SuspendShortcuts {
        suspended: bool,
    },
    RetryNative,
}

/// Deterministic time boundary. Native input timestamps use the same monotonic epoch.
pub trait Clock {
    fn now(&self) -> Instant;
}
pub struct MonotonicClock;
impl Clock for MonotonicClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Consumer-owned inference boundary, also used by the isolated Welcome preview.
pub trait Worker {
    fn prepare(&mut self, config: &Config) -> Result<u64>;
    fn submit(
        &mut self,
        config: &Config,
        prefix: &str,
        suffix: &str,
        options: &GenerationOptions,
    ) -> Result<u64>;
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
    fn prepare(&mut self, c: &Config) -> Result<u64> {
        WorkerClient::prepare(self, c)
    }
    fn submit(&mut self, c: &Config, p: &str, s: &str, o: &GenerationOptions) -> Result<u64> {
        WorkerClient::submit(self, c, p, s, o)
    }
    fn proceed(&mut self, id: u64) -> Result<()> {
        WorkerClient::proceed(self, id)
    }
    fn poll(&mut self) -> Vec<WorkerEvent> {
        WorkerClient::poll(self)
    }
    fn cancel(&mut self) -> Result<()> {
        WorkerClient::cancel(self)
    }
    fn release(&mut self) -> Result<()> {
        WorkerClient::release(self)
    }
    fn set_idle_timeout(&mut self, s: u32) {
        WorkerClient::set_idle_timeout(self, s);
    }
    fn status(&self) -> Value {
        WorkerClient::status(self)
    }
    fn request_stop(&mut self) -> Result<()> {
        WorkerClient::request_stop(self)
    }
    fn is_stopped(&mut self) -> bool {
        WorkerClient::is_stopped(self)
    }
}

/// Storage is independently injected: deterministic tests never open a profile/keychain.
pub trait Storage: Send {
    fn unlock(&mut self) -> Result<()>;
    fn lock(&mut self);
    fn record(
        &mut self,
        id: &str,
        app: &str,
        domain: &str,
        text: &str,
        spans: &[(usize, usize)],
    ) -> Result<()>;
    fn acceptance(&mut self, app: &str, text: &str, completion: bool) -> Result<()>;
    fn personalization(
        &self,
        app: &str,
        domain: &str,
        prefix: &str,
        strength: u32,
        allowed: &dyn Fn(&str, &str) -> bool,
    ) -> Result<String>;
    fn delete(&mut self, app: Option<&str>, domain: Option<&str>, unlock: bool) -> Result<usize>;
    fn counts(&self, unlock: bool) -> Result<Value>;
    fn info(&self) -> Result<Value>;
    fn statistics(&self, period: &str, group: &str) -> Result<Value>;
    fn clear_statistics(&mut self) -> Result<()>;
}
impl Storage for HistoryStore {
    fn unlock(&mut self) -> Result<()> {
        HistoryStore::unlock(self)
    }
    fn lock(&mut self) {
        HistoryStore::lock(self);
    }
    fn record(
        &mut self,
        id: &str,
        app: &str,
        domain: &str,
        text: &str,
        spans: &[(usize, usize)],
    ) -> Result<()> {
        self.record_identified_at(id, app, domain, text, spans, chrono::Utc::now())
            .map(|_| ())
    }
    fn acceptance(&mut self, app: &str, text: &str, completion: bool) -> Result<()> {
        self.record_statistics(app, text, completion)
    }
    fn personalization(
        &self,
        app: &str,
        domain: &str,
        prefix: &str,
        strength: u32,
        allowed: &dyn Fn(&str, &str) -> bool,
    ) -> Result<String> {
        HistoryStore::personalization(self, app, domain, prefix, strength, allowed)
    }
    fn delete(&mut self, app: Option<&str>, domain: Option<&str>, unlock: bool) -> Result<usize> {
        self.delete_with_unlock(app, domain, unlock)
    }
    fn counts(&self, unlock: bool) -> Result<Value> {
        self.history_counts(unlock)
    }
    fn info(&self) -> Result<Value> {
        self.history_info()
    }
    fn statistics(&self, period: &str, group: &str) -> Result<Value> {
        HistoryStore::statistics(self, period_days(period)?, group)
    }
    fn clear_statistics(&mut self) -> Result<()> {
        HistoryStore::clear_statistics(self)
    }
}
fn period_days(period: &str) -> Result<u32> {
    match period {
        "today" | "day" => Ok(1),
        "week" => Ok(7),
        "month" => Ok(30),
        "year" => Ok(365),
        "all" => Ok(0),
        _ => Err(anyhow!("Unknown statistics period.")),
    }
}
pub type SharedStorage = Arc<Mutex<Box<dyn Storage>>>;
fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
fn first(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}
fn last(text: &str, n: usize) -> &str {
    let start = text
        .char_indices()
        .rev()
        .nth(n.saturating_sub(1))
        .map_or(0, |(i, _)| i);
    if n == 0 { "" } else { &text[start..] }
}
fn domain(text: &str) -> String {
    text.to_lowercase().trim_end_matches('.').into()
}
fn rules<'a>(
    c: &'a Config,
    app: &str,
    host: &str,
) -> impl Iterator<Item = &'a BTreeMap<String, Value>> {
    [
        c.app_rules.get(&format!("app:{app}")),
        c.app_rules.get(&format!("domain:{}", domain(host))),
    ]
    .into_iter()
    .flatten()
}
fn rule_bool(c: &Config, app: &str, host: &str, name: &str, initial: bool) -> bool {
    rules(c, app, host).fold(initial, |value, rule| {
        rule.get(name).and_then(Value::as_bool).unwrap_or(value)
    })
}
fn excluded(c: &Config, app: &str, name: &str) -> bool {
    let app = core::casefold(app);
    let name = core::casefold(name);
    c.excluded_apps
        .iter()
        .filter(|s| !(cfg!(target_os = "macos") && c.terminal_completion && terminal_exclusion(s)))
        .any(|s| {
            let s = core::casefold(s);
            app.contains(&s) || name.contains(&s)
        })
}
fn terminal_exclusion(s: &str) -> bool {
    [
        "terminal",
        "iterm",
        "powershell",
        "cmd.exe",
        "windowsterminal",
        "konsole",
        "gnome-terminal",
        "kitty",
        "wezterm",
        "alacritty",
    ]
    .contains(&s.to_lowercase().as_str())
}
fn history_allowed(c: &Config, app: &str, host: &str) -> bool {
    if app.is_empty() || excluded(c, app, "") {
        return false;
    }
    if host.is_empty()
        && platform::policy::is_browser(&[app])
        && c.app_rules.iter().any(|(key, rule)| {
            key.starts_with("domain:") && rule.get("record_history") == Some(&Value::Bool(false))
        })
    {
        return false;
    }
    rule_bool(c, app, host, "record_history", c.record_history)
        && rule_bool(c, app, host, "enabled", c.enabled)
}
fn recording_configured(c: &Config) -> bool {
    c.record_history
        || c.app_rules
            .values()
            .any(|r| r.get("record_history") == Some(&Value::Bool(true)))
}

pub trait Spelling: Send + Sync {
    fn assess(&self, snapshot: &Snapshot) -> Option<SpellingResult>;
    fn invalidate(&self);
    fn close(&self) -> bool;
    fn is_stopped(&self) -> bool;
}
impl Spelling for SpellingService {
    fn assess(&self, s: &Snapshot) -> Option<SpellingResult> {
        SpellingService::assess(self, s)
    }
    fn invalidate(&self) {
        SpellingService::invalidate(self);
    }
    fn close(&self) -> bool {
        SpellingService::close(self)
    }
    fn is_stopped(&self) -> bool {
        SpellingService::is_stopped(self)
    }
}

/// Main-owner native readers. Clipboard/reference filtering and encrypted history
/// are prepared separately; these methods must obey the passed revocable guard.
pub trait Readers {
    fn context_ticket(
        &mut self,
        snapshot: &Snapshot,
        bridge: &mut dyn NativeBridge,
    ) -> Result<Option<Box<dyn platform::DetachedContextRead>>> {
        bridge.context_ticket(snapshot)
    }
    fn power(&mut self) -> system::PowerStatus;
    fn persist(&mut self, config: &Config) -> Result<()>;
}
pub struct NativeReaders;
impl Readers for NativeReaders {
    fn power(&mut self) -> system::PowerStatus {
        system::power_status()
    }
    fn persist(&mut self, c: &Config) -> Result<()> {
        config::save_config(c)
    }
}

/// Worker-local optional sources. Injection replaces external I/O only; the
/// production preparer still owns epoch, ticket, consent and result fences.
pub trait BackgroundReaders: Send + Sync {
    /// Poll only liveness while queued; resolve authorization at private-data fences.
    fn clipboard(
        &self,
        liveness: Arc<dyn Fn() -> bool + Send + Sync>,
        authorized: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<String>;
    fn screen(
        &self,
        snapshot: &Snapshot,
        ticket: &dyn platform::DetachedContextRead,
        authorized: &dyn Fn() -> bool,
        max_chars: usize,
        accessibility: bool,
        screenshot: bool,
    ) -> Result<ContextRead>;
}
pub struct NativeBackgroundReaders;
impl BackgroundReaders for NativeBackgroundReaders {
    fn clipboard(
        &self,
        liveness: Arc<dyn Fn() -> bool + Send + Sync>,
        authorized: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<String> {
        system::clipboard_text_shared(liveness, authorized)
    }
    fn screen(
        &self,
        snapshot: &Snapshot,
        ticket: &dyn platform::DetachedContextRead,
        guard: &dyn Fn() -> bool,
        max_chars: usize,
        accessibility: bool,
        screenshot: bool,
    ) -> Result<ContextRead> {
        system::screen_context_detached(
            snapshot,
            ticket,
            guard,
            max_chars,
            accessibility,
            screenshot,
        )
    }
}

pub struct Preparation {
    pub token: u64,
    pub epoch: u64,
    pub snapshot: Snapshot,
    pub config: Config,
    pub mode: String,
    pub anchor: String,
    pub references: Vec<(String, String)>,
    pub context_ticket: Option<Box<dyn platform::DetachedContextRead>>,
    pub authorization_ticket: Option<Box<dyn platform::DetachedContextRead>>,
    pub hard_disabled_apps: BTreeSet<String>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextCollectorOutcome {
    #[default]
    Off,
    TimedOut,
    TokenStale,
    TicketUnavailable,
    AuthorizationStale,
    Ready,
    Empty,
    Blocked,
    Stale,
    Unavailable,
    Error,
    ScreenPermission,
}
impl ContextCollectorOutcome {
    fn from_status(status: &str) -> Self {
        match status {
            "ready" => Self::Ready,
            "empty" => Self::Empty,
            "blocked" => Self::Blocked,
            "stale" => Self::Stale,
            "unavailable" => Self::Unavailable,
            "screen_permission" => Self::ScreenPermission,
            _ => Self::Error,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextAttachmentOutcome {
    #[default]
    NoReferences,
    RankerEmpty,
    Attached,
    Expired,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct ContextPreparationDiagnostic {
    screen: ContextCollectorOutcome,
    clipboard: ContextCollectorOutcome,
    attachment: ContextAttachmentOutcome,
}
fn context_attachment_outcome(references: bool, attached: bool) -> ContextAttachmentOutcome {
    if attached {
        ContextAttachmentOutcome::Attached
    } else if references {
        ContextAttachmentOutcome::RankerEmpty
    } else {
        ContextAttachmentOutcome::NoReferences
    }
}
pub struct Prepared {
    pub token: u64,
    pub epoch: u64,
    pub options: GenerationOptions,
    pub context_status: String,
    pub context_diagnostic: ContextPreparationDiagnostic,
    pub warning: String,
    pub reference_expires: Option<Instant>,
}
pub struct ContextPreparation {
    pub token: u64,
    pub epoch: u64,
    pub snapshot: Snapshot,
    pub config: Config,
    pub ticket: Box<dyn platform::DetachedContextRead>,
}
pub struct ContextUpdate {
    pub epoch: u64,
    pub key: SnapshotKey,
    pub authority: Authority,
    pub status: String,
}
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
struct ScreenJob {
    token: u64,
    epoch: u64,
    snapshot: Snapshot,
    config: Config,
    ticket: Box<dyn platform::DetachedContextRead>,
}
struct ScreenOwner {
    job_token: u64,
    consumer_token: u64,
    epoch: u64,
    key: SnapshotKey,
    authority: Authority,
    accessibility: bool,
    screenshot: bool,
}
impl ScreenOwner {
    fn new(token: u64, epoch: u64, snapshot: &Snapshot, config: &Config) -> Self {
        Self {
            job_token: token,
            consumer_token: token,
            epoch,
            key: snapshot.key(),
            authority: snapshot.authority(),
            accessibility: config.screen_context,
            screenshot: config.screenshot_context,
        }
    }
    fn matches(&self, request: &Preparation) -> bool {
        self.epoch == request.epoch
            && self.key == request.snapshot.key()
            && self.authority == request.snapshot.authority()
            && self.accessibility == request.config.screen_context
            && self.screenshot == request.config.screenshot_context
    }
}
struct PreparationState {
    pending: Option<Preparation>,
    results: VecDeque<Prepared>,
    stopped: bool,
    screen_pending: Option<ScreenJob>,
    screen_owner: Option<ScreenOwner>,
    screen_result: Option<(u64, u64, ContextRead, Instant)>,
    context_updates: VecDeque<ContextUpdate>,
}
pub struct AsyncPreparer {
    state: Arc<(Mutex<PreparationState>, Condvar)>,
    thread: Option<JoinHandle<()>>,
    screen_thread: Option<JoinHandle<()>>,
}
impl AsyncPreparer {
    pub fn new(
        storage: SharedStorage,
        epoch: Arc<AtomicU64>,
        readers: Arc<dyn BackgroundReaders>,
    ) -> Result<Self> {
        Self::new_with_wake(storage, epoch, readers, OwnerWake::default())
    }
    fn new_with_wake(
        storage: SharedStorage,
        epoch: Arc<AtomicU64>,
        readers: Arc<dyn BackgroundReaders>,
        owner_wake: OwnerWake,
    ) -> Result<Self> {
        let state = Arc::new((
            Mutex::new(PreparationState {
                pending: None,
                results: VecDeque::new(),
                stopped: false,
                screen_pending: None,
                screen_owner: None,
                screen_result: None,
                context_updates: VecDeque::new(),
            }),
            Condvar::new(),
        ));
        let shared = state.clone();
        let screen_epoch = epoch.clone();
        let screen_readers = readers.clone();
        let screen_wake = owner_wake.clone();
        let screen_thread = thread::Builder::new()
            .name("brotypist-window-context".into())
            .spawn(move || {
                loop {
                    let job = {
                        let mut s = locked(&shared.0);
                        while s.screen_pending.is_none() && !s.stopped {
                            s = shared
                                .1
                                .wait(s)
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                        }
                        if s.stopped {
                            break;
                        }
                        s.screen_pending.take().expect("screen job")
                    };
                    let guard =
                        || guard_epoch(&screen_epoch, job.epoch) && !locked(&shared.0).stopped;
                    if !guard() {
                        continue;
                    }
                    let result = screen_readers
                        .screen(
                            &job.snapshot,
                            &*job.ticket,
                            &guard,
                            2400,
                            job.config.screen_context,
                            job.config.screenshot_context,
                        )
                        .unwrap_or_else(|_| ContextRead {
                            text: String::new(),
                            status: "error".into(),
                            source: String::new(),
                        });
                    let mut s = locked(&shared.0);
                    if !s.stopped
                        && guard_epoch(&screen_epoch, job.epoch)
                        && s.screen_owner.as_ref().is_some_and(|owner| {
                            owner.job_token == job.token && owner.epoch == job.epoch
                        })
                    {
                        s.context_updates.clear();
                        s.context_updates.push_back(ContextUpdate {
                            epoch: job.epoch,
                            key: job.snapshot.key(),
                            authority: job.snapshot.authority(),
                            status: result.status.clone(),
                        });
                        let token = s
                            .screen_owner
                            .as_ref()
                            .expect("screen owner")
                            .consumer_token;
                        s.screen_result = Some((token, job.epoch, result, Instant::now()));
                        shared.1.notify_all();
                        drop(s);
                        screen_wake.notify();
                    }
                }
            })?;
        let shared = state.clone();
        let thread_result = thread::Builder::new().name("brotypist-writing-preparation".into()).spawn(move || loop {
            let request = {
                let mut s = locked(&shared.0);
                while s.pending.is_none() && !s.stopped { s = shared.1.wait(s).unwrap_or_else(std::sync::PoisonError::into_inner); }
                if s.stopped { break; } s.pending.take().expect("pending request")
            };
            #[cfg(feature = "native-smoke")]
            let _stage = DiagnosticSpan::start(TraceStage::ContextPreparation, Some(request.token));
            let expected = request.epoch; let check_epoch = epoch.clone(); let check_state = shared.clone();
            let guard: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || guard_epoch(&check_epoch, expected) && !locked(&check_state.0).stopped);
            let source_guard = guard.clone();
            let authorization_available = request.authorization_ticket.is_some();
            let authorization = request.authorization_ticket.map(|ticket| Arc::new(Mutex::new(ticket)));
            let private_guard: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
                if !source_guard() { return false; }
                let safe = authorization.as_ref().is_some_and(|ticket| locked(ticket).matches().unwrap_or(false));
                safe && source_guard()
            });
            if !guard() { continue; }
            let c = &request.config; let snapshot = &request.snapshot;
            let mut references = request.references; let mut status = "off".to_owned(); let mut expires = None;
            let mut context_diagnostic = ContextPreparationDiagnostic::default();
            if c.screen_context || c.screenshot_context {
                let s = locked(&shared.0);
                let (mut s, _) = shared.1.wait_timeout_while(s, Duration::from_millis(250), |s| !s.stopped && guard_epoch(&epoch, expected)
                    && s.pending.is_none() && s.screen_result.as_ref().is_none_or(|(token, ep, _, _)| *token != request.token || *ep != expected))
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let collected = if !s.stopped && guard_epoch(&epoch, expected) {
                    context_diagnostic.screen = if s.screen_result.is_some() { ContextCollectorOutcome::TokenStale } else { ContextCollectorOutcome::TimedOut };
                    status = "unavailable".into();
                    if s.screen_result.as_ref().is_some_and(|(token, ep, _, _)| *token == request.token && *ep == expected) {
                        s.screen_owner = None;
                        s.screen_result.take()
                    } else { None }
                } else { None };
                drop(s);
                if let Some((_, _, read, at)) = collected {
                    // A handed-off result keeps its original collection time.
                    // Revalidate the fresh consumer ticket outside the queue lock.
                    let ticket_current = request.context_ticket.as_ref().is_none_or(|ticket| {
                        guard() && ticket.matches_snapshot(snapshot) && ticket.matches().unwrap_or(false) && guard()
                    });
                    if ticket_current {
                        status = if context_status(&read.status) { read.status } else { "error".into() };
                        context_diagnostic.screen = ContextCollectorOutcome::from_status(&status);
                        if status == "ready" && ((read.source == "accessibility" && c.screen_context) || (read.source == "screenshot" && c.screenshot_context)) {
                            references.push((read.source, first(&read.text, 2400))); expires = Some(at + Duration::from_secs(2));
                        }
                    } else {
                        status = "stale".into();
                        context_diagnostic.screen = ContextCollectorOutcome::Stale;
                    }
                }
            }
            if !guard() { continue; }
            if c.clipboard_context {
                context_diagnostic.clipboard = if authorization_available { ContextCollectorOutcome::AuthorizationStale } else { ContextCollectorOutcome::TicketUnavailable };
                if private_guard() {
                    match readers.clipboard(guard.clone(), private_guard.clone()) {
                        Ok(text) => {
                            if private_guard() {
                                if text.is_empty() { context_diagnostic.clipboard = ContextCollectorOutcome::Empty; }
                                else { references.push(("clipboard".into(), first(&text, 4000))); status = "ready".into(); context_diagnostic.clipboard = ContextCollectorOutcome::Ready; let until = Instant::now() + Duration::from_secs(2); expires = Some(expires.map_or(until, |at: Instant| at.min(until))); }
                            }
                        },
                        Err(_) => { status = "error".into(); context_diagnostic.clipboard = ContextCollectorOutcome::Error; },
                    }
                }
            }
            if !guard() { continue; }
            let mut warning = String::new();
            let source_allowed = |app: &str, host: &str| !request.hard_disabled_apps.contains(app) && history_allowed(c, app, host);
            let personalization = if c.personalization_strength != 0 && source_allowed(&snapshot.app_id, &snapshot.domain) && private_guard() {
                let result = {
                    let store = locked(&storage);
                    store.personalization(&snapshot.app_id, &snapshot.domain, snapshot.prefix(), c.personalization_strength, &|app, host| guard() && source_allowed(app, host))
                };
                // Native post-authorization may block; history flushes must
                // not inherit that wait through the shared storage mutex.
                match result {
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
            context_diagnostic.attachment = context_attachment_outcome(!references.is_empty(), !options.context.is_empty());
            let mut s = locked(&shared.0);
            if guard_epoch(&epoch, request.epoch) && !s.stopped && s.pending.is_none() {
                s.results.clear(); s.results.push_back(Prepared { token: request.token, epoch: request.epoch, options, context_status: status, context_diagnostic, warning, reference_expires: expires });
                shared.1.notify_all();
                drop(s);
                owner_wake.notify();
            }
        });
        match thread_result {
            Ok(thread) => Ok(Self {
                state,
                thread: Some(thread),
                screen_thread: Some(screen_thread),
            }),
            Err(error) => {
                locked(&state.0).stopped = true;
                state.1.notify_all();
                let _ = screen_thread.join();
                Err(error.into())
            }
        }
    }
}
fn guard_epoch(epoch: &AtomicU64, expected: u64) -> bool {
    epoch.load(Ordering::Acquire) == expected
}
impl Preparer for AsyncPreparer {
    fn submit(&mut self, mut request: Preparation) {
        let mut s = locked(&self.state.0);
        if s.stopped {
            return;
        }
        let handoff = request.context_ticket.is_some()
            && s.screen_owner
                .as_ref()
                .is_some_and(|owner| owner.matches(&request))
            && s.screen_result
                .as_ref()
                .is_none_or(|(token, epoch, _, at)| {
                    s.screen_owner.as_ref().is_some_and(|owner| {
                        *token == owner.consumer_token && *epoch == owner.epoch
                    }) && Instant::now().saturating_duration_since(*at) < Duration::from_secs(2)
                });
        if handoff {
            s.screen_owner
                .as_mut()
                .expect("screen owner")
                .consumer_token = request.token;
            if let Some((token, _, _, _)) = s.screen_result.as_mut() {
                *token = request.token;
            }
        } else {
            s.screen_result = None;
            s.screen_owner = request.context_ticket.as_ref().map(|_| {
                ScreenOwner::new(
                    request.token,
                    request.epoch,
                    &request.snapshot,
                    &request.config,
                )
            });
            s.screen_pending = request.context_ticket.take().map(|ticket| ScreenJob {
                token: request.token,
                epoch: request.epoch,
                snapshot: request.snapshot.clone(),
                config: request.config.clone(),
                ticket,
            });
            if s.screen_pending.is_none() {
                s.screen_result = Some((
                    request.token,
                    request.epoch,
                    ContextRead {
                        text: String::new(),
                        status: "unavailable".into(),
                        source: String::new(),
                    },
                    Instant::now(),
                ));
            }
        }
        s.pending = Some(request);
        s.results.clear();
        self.state.1.notify_all();
    }
    fn prefetch(&mut self, request: ContextPreparation) {
        let mut s = locked(&self.state.0);
        if s.stopped {
            return;
        }
        if s.pending.is_none() {
            s.screen_result = None;
            s.screen_owner = Some(ScreenOwner::new(
                request.token,
                request.epoch,
                &request.snapshot,
                &request.config,
            ));
            s.screen_pending = Some(ScreenJob {
                token: request.token,
                epoch: request.epoch,
                snapshot: request.snapshot,
                config: request.config,
                ticket: request.ticket,
            });
            self.state.1.notify_all();
        }
    }
    fn context_updates(&mut self) -> Vec<ContextUpdate> {
        locked(&self.state.0).context_updates.drain(..).collect()
    }
    fn cancel(&mut self) {
        let mut s = locked(&self.state.0);
        s.pending = None;
        s.screen_pending = None;
        s.screen_owner = None;
        s.screen_result = None;
        s.results.clear();
        s.context_updates.clear();
        self.state.1.notify_all();
    }
    fn poll(&mut self) -> Vec<Prepared> {
        locked(&self.state.0).results.drain(..).collect()
    }
    fn request_stop(&mut self) {
        let mut s = locked(&self.state.0);
        s.stopped = true;
        s.pending = None;
        s.screen_pending = None;
        s.screen_owner = None;
        s.screen_result = None;
        s.results.clear();
        s.context_updates.clear();
        self.state.1.notify_all();
    }
    fn is_stopped(&mut self) -> bool {
        for owner in [&mut self.thread, &mut self.screen_thread] {
            if owner.as_ref().is_some_and(JoinHandle::is_finished) {
                if let Some(t) = owner.take() {
                    let _ = t.join();
                }
            }
        }
        self.thread.is_none() && self.screen_thread.is_none()
    }
}
impl Drop for AsyncPreparer {
    fn drop(&mut self) {
        self.request_stop();
        // Explicit shutdown polls without blocking. Abandoned construction/owner
        // teardown still retains both native-reader lifetimes until they exit.
        for owner in [&mut self.thread, &mut self.screen_thread] {
            if let Some(thread) = owner.take() {
                let _ = thread.join();
            }
        }
    }
}

#[derive(Clone)]
enum PresentationEvidence {
    Rendered,
    NativePreedit(platform::NativePreedit),
    NativeChoices(platform::NativeChoices),
}

#[derive(Clone)]
struct Claim {
    key: SnapshotKey,
    authority: Authority,
    revision: u64,
    epoch: u64,
    visible: String,
    mode: String,
    count: usize,
    evidence: PresentationEvidence,
}
#[derive(Clone, Copy)]
struct CompletionOffer {
    epoch: u64,
    revision: u64,
    serial: u64,
}
#[derive(Default)]
struct Armed {
    claims: Vec<Arc<Claim>>,
    origin: Option<Arc<(Snapshot, u64, u64)>>,
    active: bool,
    alternatives: bool,
    synonyms: bool,
    escape_passthrough: bool,
    inflight: bool,
}
struct Input {
    action: Action,
    claim: Option<Arc<Claim>>,
    origin: Option<Arc<(Snapshot, u64, u64)>>,
    transaction: bool,
}
#[derive(Clone)]
struct Lease {
    snapshot: Snapshot,
    epoch: u64,
    revision: u64,
}
impl Lease {
    fn matches(&self, s: &Snapshot, epoch: u64) -> bool {
        self.epoch == epoch && self.snapshot.same_key(s) && self.snapshot.same_authority(s)
    }
}
struct Request {
    id: u64,
    lease: Lease,
    generation: u64,
    mode: String,
    length: String,
    pending_text: Option<String>,
    final_: bool,
    proceeded: bool,
    reference_expires: Option<Instant>,
    provider_identity: Vec<String>,
    prompt_style: String,
    #[cfg(feature = "native-smoke")]
    first_usable_observed: bool,
}
struct PendingPreparation {
    token: u64,
    lease: Lease,
    mode: String,
    anchor: String,
    config: Config,
    native_pending: bool,
}
struct Choices {
    snapshot: Snapshot,
    values: Vec<String>,
    mode: String,
    start: usize,
    length: usize,
    anchored: bool,
}
struct PendingEdit {
    before: Snapshot,
    expected: Snapshot,
    text: String,
    start: usize,
    length: usize,
    remainder: Option<SuggestionState>,
    at: Instant,
    revision: u64,
    mode: String,
}
struct Session {
    key: Authority,
    id: String,
    text: String,
    authored: Vec<bool>,
    accepted: bool,
}
enum Record {
    History {
        id: String,
        app: String,
        domain: String,
        text: String,
        spans: Vec<(usize, usize)>,
    },
    Stats {
        app: String,
        text: String,
        completion: bool,
    },
}
struct Download {
    cancel: models::DownloadCancellation,
    thread: JoinHandle<()>,
    receiver: Receiver<Value>,
}
struct ModelMigration {
    cancel: models::DownloadCancellation,
    thread: Option<JoinHandle<Result<Option<String>>>>,
    provider: Vec<String>,
    model_id: String,
}
impl ModelMigration {
    fn start(config: &Config) -> Result<Self> {
        let cancel = models::DownloadCancellation::new();
        let worker_cancel = cancel.clone();
        let mut selected = config.clone();
        let thread = thread::Builder::new()
            .name("brotypist-model-storage".into())
            .spawn(move || {
                config::migrate_model_storage(&mut selected, &worker_cancel)
                    .map(|changed| changed.then_some(selected.model_path))
            })?;
        Ok(Self {
            cancel,
            thread: Some(thread),
            provider: config.provider_identity(),
            model_id: config.model_id.clone(),
        })
    }
}
impl Drop for ModelMigration {
    fn drop(&mut self) {
        self.cancel.set();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
const MODEL_STORAGE_WARNING: &str = "Couldn’t update model storage. Existing model kept.";

const WHATSAPP_APP_ID: &str = "net.whatsapp.WhatsApp";

#[derive(Clone, Copy, Default, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum DiagnosticMode {
    #[default]
    None,
    Completion,
    Alternatives,
    Synonyms,
    Emoji,
    Correction,
    Unknown,
}
impl DiagnosticMode {
    fn from_mode(mode: &str) -> Self {
        match mode {
            "completion" => Self::Completion,
            "alternatives" => Self::Alternatives,
            "synonyms" => Self::Synonyms,
            "emoji" => Self::Emoji,
            "correction" => Self::Correction,
            _ => Self::Unknown,
        }
    }
}
#[derive(Clone, Copy, Default, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum DiagnosticStage {
    #[default]
    Unobserved,
    Captured,
    Scheduled,
    PreparationQueued,
    Prepared,
    Submitted,
    Ready,
    Proceeded,
    Result,
    Offered,
    Presented,
    PresentationRejected,
    Cleared,
    Revoked,
    SubmissionError,
    WorkerError,
}
#[derive(Clone, Copy, Default, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum DiagnosticRevocation {
    #[default]
    None,
    Epoch,
    UserRevision,
    CaptureUnavailable,
    SnapshotIdentity,
    Geometry,
}
#[derive(Clone, Copy, Default, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum DiagnosticWorkerState {
    #[default]
    Unknown,
    Idle,
    Loading,
    AwaitingProceed,
    Evaluating,
    Ready,
    Error,
    Terminating,
    Stopped,
}
#[derive(Default, serde::Serialize)]
struct DiagnosticWorker {
    state: DiagnosticWorkerState,
    worker_pid: Option<u64>,
    model_loaded: Option<bool>,
    initializing: Option<bool>,
    awaiting_proceed: Option<bool>,
    evaluating: Option<bool>,
    terminating: Option<bool>,
    release_complete: Option<bool>,
}
impl DiagnosticWorker {
    fn from_status(status: &Value) -> Self {
        Self {
            state: match status
                .get("lifecycle")
                .or_else(|| status.get("state"))
                .and_then(Value::as_str)
            {
                Some("idle") => DiagnosticWorkerState::Idle,
                Some("loading") => DiagnosticWorkerState::Loading,
                Some("awaiting_proceed") => DiagnosticWorkerState::AwaitingProceed,
                Some("evaluating") => DiagnosticWorkerState::Evaluating,
                Some("ready") => DiagnosticWorkerState::Ready,
                Some("error") => DiagnosticWorkerState::Error,
                Some("terminating") => DiagnosticWorkerState::Terminating,
                Some("stopped") => DiagnosticWorkerState::Stopped,
                _ => DiagnosticWorkerState::Unknown,
            },
            worker_pid: status.get("worker_pid").and_then(Value::as_u64),
            model_loaded: status.get("model_loaded").and_then(Value::as_bool),
            initializing: status.get("initializing").and_then(Value::as_bool),
            awaiting_proceed: status.get("awaiting_proceed").and_then(Value::as_bool),
            evaluating: status.get("evaluating").and_then(Value::as_bool),
            terminating: status.get("terminating").and_then(Value::as_bool),
            release_complete: status.get("release_complete").and_then(Value::as_bool),
        }
    }
}
#[derive(Default, serde::Serialize)]
struct WhatsAppCounters {
    captures: u64,
    inline_safe_captures: u64,
    scheduled: u64,
    preparation_queued: u64,
    prepared: u64,
    submitted: u64,
    ready: u64,
    proceeded: u64,
    results: u64,
    offered: u64,
    presentation_acks: u64,
    rendered_acks: u64,
    rejected_acks: u64,
    clears: u64,
    cancellation_calls: u64,
    inflight_cancellations: u64,
    revoked_epochs: u64,
    revoked_native_evidence: u64,
    stale_worker_events: u64,
    submission_errors: u64,
    worker_errors: u64,
}
#[derive(Default, serde::Serialize)]
struct DiagnosticLease {
    present: bool,
    mode: DiagnosticMode,
    id: u64,
    epoch_current: bool,
    user_revision_current: bool,
    snapshot_current: bool,
    authority_current: bool,
}
#[derive(Default, serde::Serialize)]
struct WhatsAppRuntimeState {
    foreground_whatsapp: bool,
    active: bool,
    native_ready: bool,
    secure_input_clear: bool,
    app_allowed: bool,
    on_battery: bool,
    battery_on_demand: bool,
    candidate_present: bool,
    inline_safe: bool,
    prefix_meets_minimum: bool,
    on_demand_only: bool,
    selection_present: bool,
    has_anchor: bool,
    screen_context_enabled: bool,
    screenshot_context_enabled: bool,
    clipboard_context_enabled: bool,
    context_read_ready: bool,
    field_paused: bool,
    pending_edit: bool,
    attempted_completion: bool,
    debounce_elapsed: bool,
    inside_text_allowed: bool,
    typo_filtering: bool,
    preparing: DiagnosticLease,
    preparation_native_pending: bool,
    request: DiagnosticLease,
    request_proceeded: bool,
    request_pending_text: bool,
    request_final: bool,
    request_reference_expired: bool,
    suggestion_present: bool,
    choice_mode: DiagnosticMode,
    choice_count: usize,
    claim_count: usize,
}
#[derive(Default, serde::Serialize)]
struct WhatsAppRuntimeDiagnostic {
    observed: bool,
    last_stage: DiagnosticStage,
    last_revocation: DiagnosticRevocation,
    last_schedule_eligible: Option<bool>,
    last_spelling_pending: bool,
    last_spelling_suspected: Option<bool>,
    last_context_attached: Option<bool>,
    last_context_read_ready: Option<bool>,
    last_context_preparation: Option<ContextPreparationDiagnostic>,
    last_request_id: Option<u64>,
    last_screen_ticket_available: Option<bool>,
    last_authorization_ticket_available: Option<bool>,
    last_submission_mode: DiagnosticMode,
    last_ack_mode: DiagnosticMode,
    last_ack_visible: bool,
    last_ack_current: bool,
    last_ack_accepted: bool,
    counters: WhatsAppCounters,
    state: WhatsAppRuntimeState,
    worker: DiagnosticWorker,
}

/// UI handle is intentionally !Send. Native bridge construction, poll and edits
/// remain on their toolkit owner thread. send never captures or evaluates text.
pub struct Controller {
    engine: Rc<RefCell<Engine>>,
    sender: SyncSender<Command>,
    receiver: Receiver<Command>,
    owner_wake: OwnerWake,
}
impl Controller {
    pub fn start(config: Config) -> Result<Self> {
        Self::start_with_wake(config, OwnerWake::default())
    }
    pub(crate) fn start_with_wake(mut config: Config, owner_wake: OwnerWake) -> Result<Self> {
        config.validate()?;
        let storage: SharedStorage = Arc::new(Mutex::new(Box::new(HistoryStore::open()?)));
        let epoch = Arc::new(AtomicU64::new(1));
        let preparer = AsyncPreparer::new_with_wake(
            storage.clone(),
            epoch.clone(),
            Arc::new(NativeBackgroundReaders),
            owner_wake.clone(),
        )?;
        let spelling = (config.typo_suppression || config.typo_fixes).then(native_spelling);
        let mut engine = Engine::with_native_factory(
            config,
            Box::new(WorkerClient::new_with_wake(owner_wake.clone())?),
            storage,
            Box::new(preparer),
            spelling,
            Box::new(NativeReaders),
            Box::new(MonotonicClock),
            epoch,
            Box::new(platform::create_bridge),
        )?;
        engine.set_spelling_factory(Box::new(native_spelling));
        engine.start_model_migration();
        Self::with_engine_and_wake(engine, owner_wake)
    }
    pub fn with_engine(engine: Engine) -> Result<Self> {
        Self::with_engine_and_wake(engine, OwnerWake::default())
    }
    fn with_engine_and_wake(engine: Engine, owner_wake: OwnerWake) -> Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(128);
        Ok(Self {
            engine: Rc::new(RefCell::new(engine)),
            sender,
            receiver,
            owner_wake,
        })
    }
    pub fn send(&self, command: Command) -> Result<()> {
        self.sender
            .try_send(command)
            .map_err(|_| anyhow!("Controller command queue is unavailable or full."))?;
        self.owner_wake.notify();
        Ok(())
    }
    pub fn poll(&self) -> Vec<Value> {
        self.owner_wake.take();
        let mut engine = self.engine.borrow_mut();
        engine.pump_client_confirmations();
        self.process_commands(&mut engine);
        engine.tick();
        engine.drain_events()
    }
    fn process_commands(&self, engine: &mut Engine) {
        // Queue capacity is 128; one owner turn must not become a busy drain.
        for command in self.receiver.try_iter().take(128) {
            if matches!(&command, Command::PresentationResult { .. }) && !engine.stopping {
                let fresh = engine.capture();
                engine.observe(fresh);
            }
            engine.command(command);
        }
    }
    /// Command-only renderer acknowledgement pass: no scheduling, housekeeping
    /// or recursive tick. Events created here are rendered once by the shell.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn flush_commands(&self) -> Vec<Value> {
        let mut engine = self.engine.borrow_mut();
        self.process_commands(&mut engine);
        engine.drain_events()
    }
    /// A wake consumes only already-actionable work. Routine capture, debounce,
    /// model preparation and housekeeping keep their periodic deadlines.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn poll_ready(&self) -> Vec<Value> {
        if !self.owner_wake.take() {
            return Vec::new();
        }
        let mut engine = self.engine.borrow_mut();
        engine.pump_client_confirmations();
        self.process_commands(&mut engine);
        engine.consume_ready();
        engine.drain_events()
    }
    pub fn status(&self) -> Value {
        self.engine.borrow().status()
    }
    /// Explicit, content-free export from the actual GUI controller owner.
    pub fn export_whatsapp_diagnostic(&self) -> Result<()> {
        self.engine
            .try_borrow_mut()
            .map_err(|_| anyhow!("Controller is busy."))?
            .export_whatsapp_diagnostic()
    }
    pub fn config(&self) -> Config {
        self.engine.borrow().config.clone()
    }
    pub fn shutdown(&mut self) -> Result<bool> {
        let mut engine = self.engine.borrow_mut();
        engine.begin_shutdown();
        engine.shutdown_step()
    }
    /// Native menu tracking runs a nested toolkit loop, so this must disable
    /// hooks immediately rather than wait for the queued-command poll.
    pub fn suspend_shortcuts(&self, suspended: bool) -> Result<()> {
        let mut engine = self
            .engine
            .try_borrow_mut()
            .map_err(|_| anyhow!("Controller is busy."))?;
        engine.suspend_shortcuts(suspended);
        Ok(())
    }
    pub fn native_event_source(&self) -> Option<(i32, bool)> {
        self.engine
            .borrow()
            .bridge
            .as_ref()
            .and_then(|bridge| bridge.native_event_source())
    }
    pub fn bind_native_ui(&self, callback: Option<Arc<dyn Fn(String) + Send + Sync>>) {
        self.engine.borrow_mut().bind_native_ui(callback);
    }
    pub fn show_native_menu(&self, rows: Vec<(String, String, bool)>) -> Result<bool> {
        let mut engine = self.engine.borrow_mut();
        if !engine.native_ready {
            return Ok(false);
        }
        engine
            .bridge
            .as_mut()
            .map_or(Ok(false), |bridge| bridge.show_native_menu(rows))
    }
    pub fn present_native(&self, anchor: &str, text: &str, paused: bool) -> Result<bool> {
        let mut engine = self.engine.borrow_mut();
        if !engine.native_ready {
            return Ok(false);
        }
        engine.bridge.as_mut().map_or(Ok(false), |bridge| {
            bridge.present_native(anchor, text, paused)
        })
    }
    pub fn set_native_panel_environment(
        &self,
        width: i32,
        height: i32,
        scale: i32,
        dark: bool,
    ) -> Result<()> {
        self.engine
            .borrow_mut()
            .bridge
            .as_mut()
            .map_or(Ok(()), |bridge| {
                bridge.set_native_panel_environment(width, height, scale, dark)
            })
    }
    pub fn present_preedit(
        &self,
        key: &SnapshotKey,
        authority: &Authority,
        revision: u64,
        anchor: &str,
        text: &str,
    ) -> Result<bool> {
        self.engine
            .borrow_mut()
            .present_preedit(key, authority, revision, anchor, text)
    }
    pub fn present_choices(
        &self,
        key: &SnapshotKey,
        authority: &Authority,
        revision: u64,
        anchor: &str,
        mode: &str,
        values: &[String],
    ) -> Result<bool> {
        self.engine
            .borrow_mut()
            .present_choices(key, authority, revision, anchor, mode, values)
    }

    pub fn hide_native(&self) -> Result<()> {
        self.engine
            .borrow_mut()
            .bridge
            .as_mut()
            .map_or(Ok(()), |bridge| bridge.hide_native())
    }
}

pub type NativeFactory = Box<dyn FnMut(&Config) -> Result<Box<dyn NativeBridge>>>;

/// Injectable single-owner state machine. Tests drive tick with a logical clock
/// and real consumer contracts, without product OS mocks or a model sidecar.
pub struct Engine {
    pub config: Config,
    bridge: Option<Box<dyn NativeBridge>>,
    worker: Box<dyn Worker>,
    storage: SharedStorage,
    native_factory: NativeFactory,
    native_unavailable_reason: String,
    input_callback: Arc<dyn Fn() + Send + Sync>,
    native_ui_callback: Option<Arc<dyn Fn(String) + Send + Sync>>,
    preparer: Box<dyn Preparer>,
    spelling: Option<Arc<dyn Spelling>>,
    readers: Box<dyn Readers>,
    clock: Box<dyn Clock>,
    epoch: Arc<AtomicU64>,
    armed: Arc<Mutex<Armed>>,
    inputs: Receiver<Input>,
    input_pending: Arc<AtomicBool>,
    shortcut_callback: platform::ShortcutHandler,
    hooks_started: bool,
    native_ready: bool,
    events: VecDeque<Value>,
    candidate: Option<Snapshot>,
    candidate_revision: u64,
    candidate_since: Instant,
    capture_after: Instant,
    input_revision: u64,
    suggestion: SuggestionState,
    completion_offer: Option<CompletionOffer>,
    completion_serial: u64,
    request: Option<Request>,
    preparing: Option<PendingPreparation>,
    serial: u64,
    choices: Option<Choices>,
    correction: Option<(Snapshot, SpellingResult)>,
    claims: Vec<Arc<Claim>>,
    native_client: native_client::State,
    attempted: Option<(SnapshotKey, Authority, String)>,
    failed_correction: Option<(SnapshotKey, Authority)>,
    pending_edit: Option<PendingEdit>,
    completion_counted: bool,
    temporary: BTreeMap<String, (bool, Instant)>,
    escape_app: String,
    advance: Option<(SnapshotKey, Authority, Snapshot, String)>,
    redraw_pending: bool,
    uncertain_provenance: Option<(Authority, u64)>,
    paused_until: Instant,
    timed_pause: bool,
    field_pause: Option<(Authority, Instant)>,
    app: String,
    host: String,
    browser_apps: BTreeSet<String>,
    context_status: String,
    error: String,
    records: VecDeque<Record>,
    session: Option<Session>,
    session_serial: u64,
    context_prefetched: Option<(SnapshotKey, Authority, u64, Instant)>,
    history_suppressed: Option<(Authority, String)>,
    on_battery: bool,
    power_after: Instant,
    resident: Option<Vec<String>>,
    model_activity: Option<(u64, Authority, u64, usize)>,
    docs: Option<(Value, platform::Foreground, u64)>,
    docs_after: Instant,
    download: Option<Download>,
    model_migration: Option<ModelMigration>,
    download_status: Option<Value>,
    spelling_cleanup: Vec<JoinHandle<()>>,
    spelling_factory: Option<Box<dyn Fn() -> Arc<dyn Spelling>>>,
    preview: bool,
    shortcuts_suspended: bool,
    stopping: bool,
    done: BTreeSet<&'static str>,
    whatsapp_diagnostic: WhatsAppRuntimeDiagnostic,
}
impl Engine {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: Config,
        bridge: Box<dyn NativeBridge>,
        worker: Box<dyn Worker>,
        storage: SharedStorage,
        preparer: Box<dyn Preparer>,
        spelling: Option<Arc<dyn Spelling>>,
        readers: Box<dyn Readers>,
        clock: Box<dyn Clock>,
        epoch: Arc<AtomicU64>,
    ) -> Result<Self> {
        Self::build(
            config,
            Some(bridge),
            worker,
            storage,
            preparer,
            spelling,
            readers,
            clock,
            epoch,
            Box::new(platform::create_bridge),
            String::new(),
        )
    }
    /// Construction failure is an explicit absence, never a writable fallback.
    /// The factory stays on the native owner thread and runs again only on an
    /// explicit RetryNative/ApplyConfig operation.
    #[allow(clippy::too_many_arguments)]
    pub fn with_native_factory(
        mut config: Config,
        worker: Box<dyn Worker>,
        storage: SharedStorage,
        preparer: Box<dyn Preparer>,
        spelling: Option<Arc<dyn Spelling>>,
        readers: Box<dyn Readers>,
        clock: Box<dyn Clock>,
        epoch: Arc<AtomicU64>,
        mut factory: NativeFactory,
    ) -> Result<Self> {
        config.validate()?;
        let (bridge, reason) = match factory(&config) {
            Ok(bridge) => (Some(bridge), String::new()),
            Err(error) => (None, error.to_string()),
        };
        Self::build(
            config, bridge, worker, storage, preparer, spelling, readers, clock, epoch, factory,
            reason,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn build(
        mut config: Config,
        mut bridge: Option<Box<dyn NativeBridge>>,
        mut worker: Box<dyn Worker>,
        storage: SharedStorage,
        preparer: Box<dyn Preparer>,
        spelling: Option<Arc<dyn Spelling>>,
        readers: Box<dyn Readers>,
        clock: Box<dyn Clock>,
        epoch: Arc<AtomicU64>,
        native_factory: NativeFactory,
        mut native_unavailable_reason: String,
    ) -> Result<Self> {
        config.validate()?;
        worker.set_idle_timeout(config.idle_release_seconds);
        let configured = if let Some(bridge) = &mut bridge {
            match bridge.configure(&config) {
                Ok(()) => true,
                Err(_) => {
                    native_unavailable_reason = "Native configuration could not be applied.".into();
                    false
                }
            }
        } else {
            false
        };
        let spelling = spelling.or_else(|| {
            (config.typo_suppression || config.typo_fixes)
                .then(|| Arc::new(SpellingService::new(None, None)) as Arc<dyn Spelling>)
        });
        let now = clock.now();
        let armed = Arc::new(Mutex::new(Armed::default()));
        let input_pending = Arc::new(AtomicBool::new(false));
        let (sender, inputs) = mpsc::sync_channel(32);
        let cache = armed.clone();
        let hook_epoch = epoch.clone();
        let shortcut_callback: platform::ShortcutHandler = Arc::new(move |action| {
            let Ok(mut cache) = cache.try_lock() else {
                return false;
            };
            let claim_index = match &action {
                Action::Word | Action::All => primary_claim_index(&cache.claims, false),
                Action::Choice(index) => cache.claims.iter().position(|c| {
                    matches!(c.mode.as_str(), "emoji" | "synonyms" | "alternatives")
                        && *index < c.count
                }),
                Action::Dismiss | Action::Bypass => primary_claim_index(&cache.claims, true),
                _ => None,
            };
            let claim = claim_index.map(|i| cache.claims[i].clone());
            let mut origin = None;
            let consume = match action {
                Action::Word | Action::All | Action::Choice(_) => claim.is_some(),
                Action::Bypass => true,
                Action::Dismiss => claim.is_some() && !cache.escape_passthrough,
                Action::Force | Action::Alternatives | Action::Synonyms | Action::AppToggle => {
                    if !cache.active
                        || (action == Action::Alternatives && !cache.alternatives)
                        || (action == Action::Synonyms && !cache.synonyms)
                    {
                        return false;
                    }
                    origin = cache.origin.clone();
                    origin.is_some()
                }
                Action::Toggle | Action::Settings | Action::Quit => true,
            };
            if !consume && !(action == Action::Dismiss && (claim.is_some() || cache.inflight)) {
                return false;
            }
            let transaction = consume && action != Action::Bypass;
            if sender
                .try_send(Input {
                    action: action.clone(),
                    claim,
                    origin,
                    transaction,
                })
                .is_err()
            {
                return false;
            }
            cache.claims.clear();
            if matches!(action, Action::Dismiss | Action::Bypass) {
                hook_epoch.fetch_add(1, Ordering::AcqRel);
            }
            consume
        });
        let hooks_started = configured
            && bridge
                .as_mut()
                .is_some_and(|bridge| bridge.start_shortcuts(shortcut_callback.clone()).is_ok());
        if configured && !hooks_started {
            native_unavailable_reason =
                "Native shortcuts could not start. Check permissions, then retry.".into();
        }
        let cache = armed.clone();
        let wake = input_pending.clone();
        let input_epoch = epoch.clone();
        let input_callback: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            wake.store(true, Ordering::Release);
            input_epoch.fetch_add(1, Ordering::AcqRel);
            if let Ok(mut c) = cache.try_lock() {
                c.claims.clear();
            }
        });
        if let Some(bridge) = &mut bridge {
            bridge.set_input_callback(Some(input_callback.clone()));
            bridge.set_shortcuts_enabled(hooks_started);
        }
        let mut engine = Self {
            config,
            bridge,
            worker,
            storage,
            preparer,
            spelling,
            readers,
            clock,
            epoch,
            armed,
            inputs,
            input_pending,
            shortcut_callback,
            hooks_started,
            native_ready: hooks_started,
            native_factory,
            native_unavailable_reason,
            input_callback,
            native_ui_callback: None,
            events: VecDeque::new(),
            candidate: None,
            candidate_revision: 0,
            candidate_since: now,
            capture_after: now,
            input_revision: 0,
            suggestion: SuggestionState::new(),
            completion_offer: None,
            completion_serial: 0,
            request: None,
            preparing: None,
            serial: 0,
            choices: None,
            correction: None,
            claims: Vec::new(),
            native_client: native_client::State::default(),
            advance: None,
            redraw_pending: false,
            attempted: None,
            failed_correction: None,
            pending_edit: None,
            completion_counted: false,
            temporary: BTreeMap::new(),
            escape_app: String::new(),
            uncertain_provenance: None,
            paused_until: now,
            timed_pause: false,
            field_pause: None,
            app: String::new(),
            host: String::new(),
            browser_apps: BTreeSet::new(),
            context_status: "off".into(),
            error: String::new(),
            records: VecDeque::new(),
            session: None,
            session_serial: 0,
            history_suppressed: None,
            on_battery: false,
            power_after: now,
            resident: None,
            model_activity: None,
            docs: None,
            docs_after: now,
            download: None,
            model_migration: None,
            download_status: None,
            spelling_cleanup: Vec::new(),
            spelling_factory: None,
            context_prefetched: None,
            preview: false,
            shortcuts_suspended: false,
            stopping: false,
            done: BTreeSet::new(),
            whatsapp_diagnostic: WhatsAppRuntimeDiagnostic::default(),
        };
        if recording_configured(&engine.config) {
            let result = locked(&engine.storage).unlock();
            if result.is_err() {
                engine.error = "Encrypted history could not be unlocked.".into();
            }
        }
        if !engine.hooks_started {
            engine.error = engine.native_unavailable_reason.clone();
        }
        engine.refresh_arming();
        Ok(engine)
    }
    fn bind_native_ui(&mut self, callback: Option<Arc<dyn Fn(String) + Send + Sync>>) {
        self.native_ui_callback = callback;
        if let Some(bridge) = &mut self.bridge {
            bridge.bind_native_ui(self.native_ui_callback.clone());
        }
    }
    fn secure_input(&self) -> Option<bool> {
        self.bridge
            .as_ref()
            .and_then(|bridge| bridge.secure_input_enabled())
    }
    fn user_revision(&self) -> Option<u64> {
        self.bridge
            .as_ref()
            .map(|bridge| bridge.user_input_revision())
    }
    fn native_input_revision(&self) -> Option<u64> {
        self.bridge.as_ref().map(|bridge| bridge.input_revision())
    }
    fn input_claim_valid(&self) -> bool {
        self.bridge
            .as_ref()
            .is_none_or(|bridge| bridge.input_claim_valid())
    }
    fn set_native_shortcuts(&mut self, enabled: bool) {
        if let Some(bridge) = &mut self.bridge {
            bridge.set_shortcuts_enabled(enabled);
        }
    }
    fn finish_input(&mut self, accepted: bool) -> Result<()> {
        self.bridge
            .as_mut()
            .ok_or_else(|| anyhow!("Native input is unavailable."))?
            .finish_input(accepted)
    }
    pub fn set_spelling_factory(&mut self, factory: Box<dyn Fn() -> Arc<dyn Spelling>>) {
        self.spelling_factory = Some(factory);
    }
    fn now(&self) -> Instant {
        self.clock.now()
    }
    fn whatsapp_relevant(&self) -> bool {
        self.app == WHATSAPP_APP_ID
            || self
                .candidate
                .as_ref()
                .is_some_and(|s| s.app_id == WHATSAPP_APP_ID)
            || self
                .preparing
                .as_ref()
                .is_some_and(|p| p.lease.snapshot.app_id == WHATSAPP_APP_ID)
            || self
                .request
                .as_ref()
                .is_some_and(|r| r.lease.snapshot.app_id == WHATSAPP_APP_ID)
            || self
                .suggestion
                .snapshot
                .as_ref()
                .is_some_and(|s| s.app_id == WHATSAPP_APP_ID)
            || self
                .choices
                .as_ref()
                .is_some_and(|c| c.snapshot.app_id == WHATSAPP_APP_ID)
            || self
                .correction
                .as_ref()
                .is_some_and(|(s, _)| s.app_id == WHATSAPP_APP_ID)
            || self
                .pending_edit
                .as_ref()
                .is_some_and(|p| p.before.app_id == WHATSAPP_APP_ID)
    }
    fn whatsapp_stage(&mut self, stage: DiagnosticStage) {
        if !self.whatsapp_relevant() {
            return;
        }
        let diagnostic = &mut self.whatsapp_diagnostic;
        diagnostic.observed = true;
        let counters = &mut diagnostic.counters;
        let count = match stage {
            DiagnosticStage::Captured => Some(&mut counters.captures),
            DiagnosticStage::Scheduled => Some(&mut counters.scheduled),
            DiagnosticStage::PreparationQueued => Some(&mut counters.preparation_queued),
            DiagnosticStage::Prepared => Some(&mut counters.prepared),
            DiagnosticStage::Submitted => Some(&mut counters.submitted),
            DiagnosticStage::Ready => Some(&mut counters.ready),
            DiagnosticStage::Proceeded => Some(&mut counters.proceeded),
            DiagnosticStage::Result => Some(&mut counters.results),
            DiagnosticStage::Offered => Some(&mut counters.offered),
            DiagnosticStage::Presented => Some(&mut counters.rendered_acks),
            DiagnosticStage::PresentationRejected => Some(&mut counters.rejected_acks),
            DiagnosticStage::Cleared => Some(&mut counters.clears),
            DiagnosticStage::SubmissionError => Some(&mut counters.submission_errors),
            DiagnosticStage::WorkerError => Some(&mut counters.worker_errors),
            _ => None,
        };
        if let Some(count) = count {
            *count = count.saturating_add(1);
        }
        // Routine captures must not obscure the last request/render boundary.
        if !matches!(stage, DiagnosticStage::Captured) || counters.scheduled == 0 {
            diagnostic.last_stage = stage;
        }
    }
    fn whatsapp_worker(&mut self) {
        if self.whatsapp_relevant() {
            self.whatsapp_diagnostic.worker = DiagnosticWorker::from_status(&self.worker.status());
        }
    }
    fn whatsapp_lease(&self, lease: &Lease, id: u64, mode: &str) -> DiagnosticLease {
        DiagnosticLease {
            present: true,
            mode: DiagnosticMode::from_mode(mode),
            id,
            epoch_current: lease.epoch == self.current_epoch(),
            user_revision_current: Some(lease.revision) == self.user_revision(),
            snapshot_current: self
                .candidate
                .as_ref()
                .is_some_and(|s| lease.snapshot.same_key(s)),
            authority_current: self
                .candidate
                .as_ref()
                .is_some_and(|s| lease.snapshot.same_authority(s)),
        }
    }
    fn whatsapp_state(&mut self) {
        if !self.whatsapp_relevant() && !self.whatsapp_diagnostic.state.foreground_whatsapp {
            return;
        }
        let foreground = self.app == WHATSAPP_APP_ID;
        if !foreground {
            let state = &mut self.whatsapp_diagnostic.state;
            state.foreground_whatsapp = false;
            state.candidate_present = false;
            state.preparing = DiagnosticLease::default();
            state.request = DiagnosticLease::default();
            state.preparation_native_pending = false;
            state.request_proceeded = false;
            state.request_pending_text = false;
            state.request_final = false;
            state.request_reference_expired = false;
            state.suggestion_present = false;
            state.choice_mode = DiagnosticMode::None;
            state.choice_count = 0;
            state.claim_count = 0;
            return;
        }
        let candidate = self
            .candidate
            .as_ref()
            .filter(|s| foreground && s.app_id == WHATSAPP_APP_ID);
        let preparing = self
            .preparing
            .as_ref()
            .filter(|p| p.lease.snapshot.app_id == WHATSAPP_APP_ID);
        let request = self
            .request
            .as_ref()
            .filter(|r| r.lease.snapshot.app_id == WHATSAPP_APP_ID);
        let choices = self
            .choices
            .as_ref()
            .filter(|c| c.snapshot.app_id == WHATSAPP_APP_ID);
        let state = WhatsAppRuntimeState {
            foreground_whatsapp: foreground,
            active: self.active(),
            native_ready: self.native_ready,
            secure_input_clear: self.secure_input() == Some(false),
            app_allowed: self.whatsapp_diagnostic.state.app_allowed,
            on_battery: self.on_battery,
            battery_on_demand: self.config.battery_on_demand,
            candidate_present: candidate.is_some(),
            inline_safe: candidate.is_some_and(Snapshot::inline_safe),
            prefix_meets_minimum: candidate
                .is_some_and(|s| s.eligible(self.config.min_chars as usize)),
            on_demand_only: candidate.is_some_and(|s| s.on_demand_only),
            selection_present: candidate.is_some_and(|s| s.selection_length != 0),
            has_anchor: candidate.is_some_and(Snapshot::has_anchor),
            field_paused: candidate.is_some_and(|s| self.field_paused(s)),
            pending_edit: self
                .pending_edit
                .as_ref()
                .is_some_and(|p| p.before.app_id == WHATSAPP_APP_ID),
            attempted_completion: candidate.is_some_and(|s| {
                self.attempted
                    .as_ref()
                    .is_some_and(|(key, authority, mode)| {
                        mode == "completion" && key.matches(s) && authority_matches(authority, s)
                    })
            }),
            debounce_elapsed: self.now().saturating_duration_since(self.candidate_since)
                >= Duration::from_millis(self.config.debounce_ms.into()),
            inside_text_allowed: candidate.is_some_and(|s| {
                self.config.complete_inside_text || s.line_suffix().trim().is_empty()
            }),
            typo_filtering: self.config.typo_suppression || self.config.typo_fixes,
            screen_context_enabled: self.config.screen_context,
            screenshot_context_enabled: self.config.screenshot_context,
            clipboard_context_enabled: self.config.clipboard_context,
            context_read_ready: self.context_status == "ready",
            preparing: preparing.map_or_else(DiagnosticLease::default, |p| {
                self.whatsapp_lease(&p.lease, p.token, &p.mode)
            }),
            preparation_native_pending: preparing.is_some_and(|p| p.native_pending),
            request: request.map_or_else(DiagnosticLease::default, |r| {
                self.whatsapp_lease(&r.lease, r.id, &r.mode)
            }),
            request_proceeded: request.is_some_and(|r| r.proceeded),
            request_pending_text: request.is_some_and(|r| r.pending_text.is_some()),
            request_final: request.is_some_and(|r| r.final_),
            request_reference_expired: request
                .is_some_and(|r| r.reference_expires.is_some_and(|at| self.now() > at)),
            suggestion_present: self
                .suggestion
                .snapshot
                .as_ref()
                .is_some_and(|s| s.app_id == WHATSAPP_APP_ID)
                && !self.suggestion.text.is_empty(),
            choice_mode: choices
                .map_or(DiagnosticMode::None, |c| DiagnosticMode::from_mode(&c.mode)),
            choice_count: choices.map_or(0, |c| c.values.len()),
            claim_count: self
                .claims
                .iter()
                .filter(|c| c.key.app_id == WHATSAPP_APP_ID)
                .count(),
        };
        self.whatsapp_diagnostic.observed = true;
        self.whatsapp_diagnostic.state = state;
    }
    fn whatsapp_revocation(&mut self, lease: &Lease, current: Option<&Snapshot>) {
        if lease.snapshot.app_id != WHATSAPP_APP_ID {
            return;
        }
        let reason = if lease.epoch != self.current_epoch() {
            DiagnosticRevocation::Epoch
        } else if Some(lease.revision) != self.user_revision() {
            DiagnosticRevocation::UserRevision
        } else if let Some(current) = current {
            if !lease.snapshot.same_key(current) || !lease.snapshot.same_authority(current) {
                DiagnosticRevocation::SnapshotIdentity
            } else {
                DiagnosticRevocation::Geometry
            }
        } else {
            DiagnosticRevocation::CaptureUnavailable
        };
        self.whatsapp_diagnostic.last_revocation = reason;
        self.whatsapp_diagnostic.last_stage = DiagnosticStage::Revoked;
        let count = if matches!(reason, DiagnosticRevocation::Epoch) {
            &mut self.whatsapp_diagnostic.counters.revoked_epochs
        } else {
            &mut self.whatsapp_diagnostic.counters.revoked_native_evidence
        };
        *count = count.saturating_add(1);
    }
    fn export_whatsapp_diagnostic(&mut self) -> Result<()> {
        if self.app == WHATSAPP_APP_ID {
            self.whatsapp_state();
            self.whatsapp_worker();
        }
        #[derive(serde::Serialize)]
        struct Export<'a> {
            schema: &'static str,
            version: &'static str,
            app_id: &'static str,
            runtime: &'a WhatsAppRuntimeDiagnostic,
        }
        system::export_whatsapp_runtime_diagnostic(&Export {
            schema: "brotypist-whatsapp-runtime-diagnostic/1",
            version: env!("CARGO_PKG_VERSION"),
            app_id: WHATSAPP_APP_ID,
            runtime: &self.whatsapp_diagnostic,
        })
    }
    fn active(&self) -> bool {
        !self.stopping
            && !self.preview
            && self.config.global_enabled
            && self.now() >= self.paused_until
    }
    fn current_epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }
    fn bump_epoch(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
    }
    fn emit(&mut self, event: Value) {
        if self.events.len() == 128 {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }
    pub fn drain_events(&mut self) -> Vec<Value> {
        self.events.drain(..).collect()
    }
    fn fail(&mut self, operation: &str) {
        self.error = format!("{operation} could not be completed.");
        self.emit(json!({"type":"error","operation":operation,"message":self.error}));
    }
    fn temporary_value(&self, scope: &str) -> Option<bool> {
        self.temporary
            .get(scope)
            .filter(|(_, at)| self.now() < *at)
            .map(|(value, _)| *value)
    }
    fn allowed(&self, app: &str, host: &str, name: &str) -> bool {
        if app.is_empty() || app == self.escape_app || excluded(&self.config, app, name) {
            return false;
        }
        let app_override = self.temporary_value(&format!("app:{app}"));
        if app_override == Some(false) {
            return false;
        }
        let enabled = self
            .config
            .app_rules
            .get(&format!("app:{app}"))
            .and_then(|r| r.get("enabled"))
            .and_then(Value::as_bool)
            .unwrap_or(self.config.enabled);
        let enabled = app_override.unwrap_or(enabled);
        let enabled = self
            .config
            .app_rules
            .get(&format!("domain:{}", domain(host)))
            .and_then(|r| r.get("enabled"))
            .and_then(Value::as_bool)
            .unwrap_or(enabled);
        self.temporary_value(&format!("domain:{}", domain(host)))
            .unwrap_or(enabled)
    }
    fn record_allowed(&self, app: &str, host: &str) -> bool {
        if host.is_empty()
            && self.browser_apps.contains(app)
            && self.config.app_rules.iter().any(|(k, r)| {
                k.starts_with("domain:") && r.get("record_history") == Some(&Value::Bool(false))
            })
        {
            return false;
        }
        self.allowed(app, host, "")
            && rule_bool(
                &self.config,
                app,
                host,
                "record_history",
                self.config.record_history,
            )
    }
    fn native_config(&self) -> Config {
        let mut config = self.config.clone();
        for (scope, (enabled, at)) in &self.temporary {
            if self.now() < *at {
                config
                    .app_rules
                    .entry(scope.clone())
                    .or_default()
                    .insert("enabled".into(), json!(enabled));
            }
        }
        if !self.escape_app.is_empty() {
            config
                .app_rules
                .entry(format!("app:{}", self.escape_app))
                .or_default()
                .insert("enabled".into(), json!(false));
        }
        if !self.active() {
            config.global_enabled = false;
        }
        config
    }
    fn configure_native(&mut self) {
        self.native_ready = false;
        self.disarm();
        self.set_native_shortcuts(false);
        let config = self.native_config();
        let Some(bridge) = &mut self.bridge else {
            self.refresh_arming();
            return;
        };
        if bridge.configure(&config).is_err() {
            self.clear(true, true);
            self.fail("Native configuration");
        } else {
            self.native_ready = self.hooks_started;
        }
        self.set_native_shortcuts(self.native_ready && !self.shortcuts_suspended && !self.stopping);
        self.refresh_arming();
    }
    fn foreground(&mut self) -> Option<platform::Foreground> {
        let mut info = self.bridge.as_mut()?.foreground().ok().flatten()?;
        info.domain = domain(&info.domain);
        if !self.escape_app.is_empty() && info.app_id != self.escape_app {
            self.escape_app.clear();
            self.configure_native();
        }
        self.app = info.app_id.clone();
        self.host = info.domain.clone();
        let browser = platform::policy::is_browser(&[&info.app_id, &info.executable]);
        if browser {
            self.browser_apps.insert(info.app_id.clone());
        }
        if browser
            && info.domain.is_empty()
            && self.config.app_rules.iter().any(|(k, r)| {
                k.starts_with("domain:") && r.get("enabled") == Some(&Value::Bool(false))
            })
        {
            return None;
        }
        let allowed = self.allowed(&info.app_id, &info.domain, &info.executable);
        if info.app_id == WHATSAPP_APP_ID {
            self.whatsapp_diagnostic.state.app_allowed = allowed;
        }
        allowed.then_some(info)
    }
    fn capture(&mut self) -> Option<Snapshot> {
        #[cfg(feature = "native-smoke")]
        let _stage = DiagnosticSpan::start(TraceStage::NativeCapture, None);
        self.pump_client_confirmations();
        if !self.native_ready || !self.active() || self.secure_input() != Some(false) {
            return None;
        }
        let before = self.foreground()?;
        if !self.native_ready || self.secure_input() != Some(false) {
            return None;
        }
        let revision = self.user_revision()?;
        let confirmed_before = self.native_client.confirmed;
        let mut captured = self.bridge.as_mut()?.capture();
        self.pump_client_confirmations();
        if matches!(&captured, Ok(None)) && self.native_client.confirmed != confirmed_before {
            // A real edit completed while a pending client capture was refused.
            // Re-read once; never replay the edit or synthesize a snapshot.
            captured = self.bridge.as_mut()?.capture();
            self.pump_client_confirmations();
        }
        let mut snapshot = captured.ok().flatten()?;
        let after = self.foreground()?;
        if snapshot.protected
            || !snapshot.valid_range()
            || !self.native_ready
            || !self.active()
            || self.secure_input() != Some(false)
            || before.app_id != after.app_id
            || before.domain != after.domain
            || before.window_id != after.window_id
            || before.window_pid != after.window_pid
            || before.document_id != after.document_id
            || snapshot.app_id != after.app_id
            || domain(&snapshot.domain) != after.domain
            || (!after.window_id.is_empty() && snapshot.window_id != after.window_id)
            || !self.allowed(&snapshot.app_id, &snapshot.domain, "")
            || Some(revision) != self.user_revision()
        {
            return None;
        }
        if snapshot
            .inline_render
            .as_ref()
            .is_some_and(|r| r.source.contains("+vision"))
            && !self.config.screenshot_appearance
        {
            snapshot.inline_render = None;
        }
        self.candidate_revision = revision;
        if snapshot.app_id == WHATSAPP_APP_ID {
            self.whatsapp_stage(DiagnosticStage::Captured);
            if snapshot.inline_safe() {
                let count = &mut self.whatsapp_diagnostic.counters.inline_safe_captures;
                *count = count.saturating_add(1);
            }
        }
        Some(snapshot)
    }
    fn disarm(&mut self) {
        self.claims.clear();
        if let Ok(mut a) = self.armed.try_lock() {
            a.claims.clear();
        }
    }
    fn clear_docs(&mut self) {
        if self.docs.take().is_some() {
            self.emit(json!({"type":"docs_help","hint":null}));
        }
    }
    fn clear(&mut self, hide: bool, cancel: bool) {
        self.clear_state(hide, cancel, false);
        if let Some(s) = &self.spelling {
            s.invalidate();
        }
    }
    fn clear_state(&mut self, hide: bool, cancel: bool, preserve_context: bool) {
        #[cfg(feature = "native-smoke")]
        if self.preparing.is_some() || self.request.is_some() || !self.suggestion.text.is_empty() {
            stage_diagnostics::mark(TraceStage::RuntimeCleared, None);
        }
        let whatsapp = self.whatsapp_relevant();
        if whatsapp {
            self.whatsapp_stage(DiagnosticStage::Cleared);
        }
        if whatsapp && cancel {
            let counters = &mut self.whatsapp_diagnostic.counters;
            counters.cancellation_calls = counters.cancellation_calls.saturating_add(1);
            if self.preparing.is_some() || self.request.is_some() {
                counters.inflight_cancellations = counters.inflight_cancellations.saturating_add(1);
            }
        }
        if !preserve_context {
            self.bump_epoch();
        }
        if cancel {
            let _ = self.worker.cancel();
            if whatsapp {
                self.whatsapp_worker();
            }
        }
        if !preserve_context {
            self.preparer.cancel();
        }
        self.preparing = None;
        self.request = None;
        self.suggestion.invalidate();
        self.completion_offer = None;
        self.choices = None;
        self.correction = None;
        self.pending_edit = None;
        if !preserve_context {
            self.context_prefetched = None;
        }
        self.advance = None;
        self.redraw_pending = false;
        self.disarm();
        self.clear_docs();
        self.context_status = if self.config.screen_context
            || self.config.screenshot_context
            || self.config.clipboard_context
        {
            "blocked"
        } else {
            "off"
        }
        .into();
        if let Some(bridge) = &mut self.bridge {
            let _ = bridge.hide_native();
        }
        self.emit(json!({"type":"clear","hide_accessory":hide}));
        if whatsapp {
            self.whatsapp_state();
        }
    }
    fn revoke_inflight(&mut self, hide: bool) {
        let whatsapp = self.whatsapp_relevant();
        self.clear(hide, true);
        // Revoked native evidence is not a completed or dismissed attempt.
        // A fresh eligible capture may start new work after the usual debounce.
        self.attempted = None;
        self.candidate_since = self.now();
        if whatsapp {
            self.whatsapp_diagnostic.last_stage = DiagnosticStage::Revoked;
            self.whatsapp_state();
        }
    }
    fn attempted(&self, s: &Snapshot, mode: &str) -> bool {
        self.attempted.as_ref().is_some_and(|(key, authority, m)| {
            *key == s.key() && *authority == s.authority() && m == mode
        })
    }
    fn mark_attempt(&mut self, s: &Snapshot, mode: &str) {
        self.attempted = Some((s.key(), s.authority(), mode.into()));
    }
    fn field_paused(&self, s: &Snapshot) -> bool {
        self.field_pause
            .as_ref()
            .is_some_and(|(a, at)| *a == s.authority() && self.now() < *at)
    }
    fn observe(&mut self, snapshot: Option<Snapshot>) {
        let Some(snapshot) = snapshot else {
            if self.candidate.is_some() || self.preparing.is_some() || self.request.is_some() {
                self.finish_session();
                self.clear(true, true);
            }
            self.candidate = None;
            self.attempted = None;
            self.pending_edit = None;
            return;
        };
        self.observe_pending_edit(&snapshot);
        if self
            .uncertain_provenance
            .as_ref()
            .is_some_and(|(authority, _)| *authority != snapshot.authority())
        {
            self.uncertain_provenance = None;
        }
        if snapshot.selection_length != 0 {
            self.finish_session();
        } else if snapshot.eligible(1) {
            self.observe_session(&snapshot);
        }
        let previous = self.candidate.clone();
        if previous
            .as_ref()
            .is_some_and(|p| p.same_key(&snapshot) && p.same_authority(&snapshot))
        {
            // A delayed native value/selection notification can revoke the
            // epoch without changing this draft or its user-input revision.
            // Retire that work before the same-key fast path; otherwise its
            // later rejection leaves the attempted latch blocking this draft.
            let epoch = self.current_epoch();
            if self
                .preparing
                .as_ref()
                .is_some_and(|p| p.lease.epoch != epoch)
                || self
                    .request
                    .as_ref()
                    .is_some_and(|request| request.lease.epoch != epoch)
            {
                if snapshot.app_id == WHATSAPP_APP_ID {
                    self.whatsapp_diagnostic.last_revocation = DiagnosticRevocation::Epoch;
                    let count = &mut self.whatsapp_diagnostic.counters.revoked_epochs;
                    *count = count.saturating_add(1);
                }
                self.revoke_inflight(false);
            }
            let changed = previous.as_ref().is_some_and(|p| p != &snapshot);
            self.candidate = Some(snapshot.clone());
            if changed
                || self.redraw_pending
                || self
                    .claims
                    .iter()
                    .any(|c| c.revision != self.candidate_revision)
            {
                self.disarm();
                self.redraw_pending = false;
                if self.correction.is_some() {
                    self.publish_correction_geometry(&snapshot);
                } else if !self.suggestion.text.is_empty() {
                    self.suggestion.snapshot = Some(snapshot.clone());
                    self.publish_completion();
                } else if self.choices.is_some() {
                    if let Some(c) = &mut self.choices {
                        c.snapshot = snapshot.clone();
                    }
                    self.publish_choices();
                }
                self.publish_caret(&snapshot);
            }
            return;
        }
        self.bump_epoch();
        self.attempted = None;
        self.failed_correction = None;
        let now = self.now();
        self.candidate_since = now;
        if previous
            .as_ref()
            .is_some_and(|p| p.app_id == snapshot.app_id && p.field_id == snapshot.field_id)
        {
            if let Some(at) = self
                .bridge
                .as_ref()
                .and_then(|bridge| bridge.input_timestamp())
            {
                if at <= now && now.duration_since(at) <= Duration::from_millis(200) {
                    self.candidate_since = at;
                }
            }
        }
        let advance = self.correction.is_none() && self.suggestion.advance(&snapshot);
        self.candidate = Some(snapshot.clone());
        if advance {
            let _ = self.worker.cancel();
            self.request = None;
            self.preparing = None;
            self.preparer.cancel();
            self.disarm();
            self.publish_completion();
        } else {
            self.clear(false, true);
        }
        self.publish_caret(&snapshot);
    }
    fn publish_caret(&mut self, s: &Snapshot) {
        if s.has_anchor() {
            self.emit(json!({"type":"caret","rect":s.caret_rect,"field_rect":s.field_rect,"native_anchor":s.native_anchor}));
        } else {
            self.emit(json!({"type":"clear","hide_accessory":true}));
        }
    }
    fn presentation_event(
        &self,
        s: &Snapshot,
        text: &str,
        mode: &str,
        choices: &[String],
    ) -> Value {
        json!({"type":"suggestion","key":s.key(),"authority":s.authority(),"revision":self.candidate_revision,"text":text,"mode":mode,
            "rect":s.anchor_rect(),"field_rect":s.field_rect,"field_anchor":s.caret_rect.is_none(),"native_anchor":s.native_anchor,
            "inline":mode == "completion" || mode == "correction","render":s.inline_render,"choices":choices})
    }
    fn publish_completion(&mut self) {
        self.claims.retain(|c| c.mode != "completion");
        self.completion_offer = None;
        self.refresh_arming();
        let Some(s) = self.suggestion.snapshot.clone() else {
            return;
        };
        let fallback = !s.inline_safe()
            && self
                .advance
                .as_ref()
                .is_some_and(|(key, authority, geometry, _)| {
                    *key == s.key() && *authority == s.authority() && geometry.inline_safe()
                });
        if !fallback {
            self.advance = None;
        }
        let geometry = if fallback {
            &self.advance.as_ref().expect("verified advance").2
        } else {
            &s
        };
        if self.suggestion.text.is_empty() || !geometry.inline_safe() {
            self.emit(json!({"type":"clear"}));
            return;
        }
        let choices = self
            .choices
            .as_ref()
            .filter(|c| c.anchored && c.snapshot.same_key(&s) && c.snapshot.same_authority(&s))
            .map_or(&[][..], |c| c.values.as_slice());
        let mut event = self.presentation_event(&s, &self.suggestion.text, "completion", choices);
        event["rect"] = json!(geometry.anchor_rect());
        event["field_rect"] = json!(geometry.field_rect);
        event["native_anchor"] = json!(geometry.native_anchor);
        event["render"] = json!(geometry.inline_render);
        if let Some((_, _, _, accepted)) = &self.advance {
            event["advance"] = json!(accepted);
        }
        if !choices.is_empty() {
            event["choices_mode"] = json!("alternatives");
        }
        self.completion_serial = self
            .completion_serial
            .checked_add(1)
            .expect("completion presentation sequence exhausted");
        // Overlays echo this existing field. Bind their acknowledgement to
        // this emitted view, independently of the native user-input revision.
        event["revision"] = json!(self.completion_serial);
        self.completion_offer = Some(CompletionOffer {
            epoch: self.current_epoch(),
            revision: self.candidate_revision,
            serial: self.completion_serial,
        });
        self.emit(event);
        self.whatsapp_stage(DiagnosticStage::Offered);
    }
    fn publish_choices(&mut self) {
        self.claims
            .retain(|c| !matches!(c.mode.as_str(), "alternatives" | "synonyms" | "emoji"));
        self.refresh_arming();
        let Some(c) = &self.choices else {
            return;
        };
        if c.anchored {
            self.publish_completion();
            return;
        }
        if !c.snapshot.has_anchor() || c.values.is_empty() {
            return;
        }
        let event = self.presentation_event(&c.snapshot, &c.values[0], &c.mode, &c.values);
        self.emit(event);
        self.whatsapp_stage(DiagnosticStage::Offered);
    }
    fn set_choices(
        &mut self,
        s: Snapshot,
        mut values: Vec<String>,
        mode: &str,
        start: usize,
        length: usize,
        anchored: bool,
    ) {
        if (mode == "emoji" || mode == "synonyms" || length != 0 || start != s.caret)
            && !self.can_replace(&s)
        {
            return;
        }
        values.truncate(5);
        if values.is_empty() {
            return;
        }
        if !anchored {
            self.suggestion.invalidate();
            self.completion_counted = false;
            self.disarm();
        }
        self.choices = Some(Choices {
            snapshot: s,
            values,
            mode: mode.into(),
            start,
            length,
            anchored,
        });
        self.publish_choices();
    }
    fn can_replace(&self, s: &Snapshot) -> bool {
        s.can_replace
            && self
                .bridge
                .as_ref()
                .is_some_and(|bridge| bridge.atomic_replacement_supported())
    }
    fn assess(&self, s: &Snapshot) -> Option<SpellingResult> {
        self.spelling
            .as_ref()
            .and_then(|service| service.assess(s))
            .filter(|r| r.matches(s))
    }
    fn typo_active(&self, s: &Snapshot) -> bool {
        self.config.typo_suppression || (self.config.typo_fixes && self.can_replace(s))
    }
    fn completion_blocked(&self, s: &Snapshot) -> Option<bool> {
        if !self.typo_active(s) {
            return Some(false);
        }
        if self
            .correction
            .as_ref()
            .is_some_and(|(before, _)| before.same_key(s) && before.same_authority(s))
        {
            return Some(true);
        }
        self.assess(s)
            .map(|r| r.suspected && self.config.typo_suppression)
    }
    fn show_correction(&mut self, s: &Snapshot, result: SpellingResult) -> bool {
        if !self.config.typo_fixes
            || result.replacement.is_empty()
            || !self.can_replace(s)
            || !result.matches(s)
        {
            return false;
        }
        if self.attempted(s, "correction") && self.correction.is_none() {
            return false;
        }
        if self
            .correction
            .as_ref()
            .is_some_and(|(before, _)| before.same_key(s) && before.same_authority(s))
        {
            return true;
        }
        if self
            .failed_correction
            .as_ref()
            .is_some_and(|(k, a)| *k == s.key() && *a == s.authority())
        {
            return false;
        }
        let rect = self
            .bridge
            .as_mut()
            .and_then(|bridge| {
                bridge
                    .range_rect(s, result.start, result.length)
                    .ok()
                    .flatten()
            })
            .filter(valid_rect);
        if rect.is_none() || !s.inline_safe() || s.inline_render.is_none() {
            self.failed_correction = Some((s.key(), s.authority()));
            return false;
        }
        self.clear(false, true);
        self.mark_attempt(s, "correction");
        self.correction = Some((s.clone(), result));
        self.publish_correction(s, rect);
        true
    }
    fn publish_correction_geometry(&mut self, s: &Snapshot) {
        let Some((_, result)) = &self.correction else {
            return;
        };
        let rect = self
            .bridge
            .as_mut()
            .and_then(|bridge| {
                bridge
                    .range_rect(s, result.start, result.length)
                    .ok()
                    .flatten()
            })
            .filter(valid_rect);
        self.publish_correction(s, rect);
    }
    fn publish_correction(&mut self, s: &Snapshot, rect: Option<core::Rect>) {
        self.claims.retain(|c| c.mode != "correction");
        let Some((_, result)) = self.correction.clone() else {
            return;
        };
        let Some(rect) = rect else {
            self.correction = None;
            self.emit(json!({"type":"clear"}));
            return;
        };
        if !s.inline_safe() || s.inline_render.is_none() || !result.matches(s) {
            self.correction = None;
            self.emit(json!({"type":"clear"}));
            return;
        }
        self.correction = Some((s.clone(), result.clone()));
        let mut event = self.presentation_event(s, &result.replacement, "correction", &[]);
        event["correction"] = json!({"start":result.start,"length":result.length,"word":result.word,"replacement":result.replacement,"language":result.language,"rect":rect});
        self.emit(event);
    }
    fn length(&self) -> String {
        if self.on_battery && self.config.battery_shorter {
            features::shorter_length(&self.config.completion_length)
        } else {
            self.config.completion_length.clone()
        }
    }
    fn effective_config(&mut self, mode: &str) -> Config {
        let mut c = self.native_config();
        c.completion_length = self.length();
        c.max_tokens = if mode == "completion" {
            c.max_tokens
                .min(features::length_profile(&c.completion_length).0)
        } else {
            128
        };
        if self.on_battery && c.backend == "gguf" && c.battery_model != "same" {
            match models::get_preset(&c.battery_model)
                .ok()
                .filter(|p| models::is_installed(p))
            {
                Some(p) => {
                    if let Ok(path) = models::model_path(p) {
                        c.model_path = path.to_string_lossy().into();
                        c.prompt_style = p.prompt_style.clone();
                        c.model_id = p.id.clone();
                    }
                }
                None => {
                    self.error =
                        "The battery model is not installed; the selected model is still used."
                            .into()
                }
            }
        }
        c
    }
    fn submit(&mut self, s: &Snapshot, mode: &str, anchor: String) -> bool {
        if mode == "completion" && !s.inline_safe() {
            return false;
        }
        if matches!(mode, "emoji" | "synonyms") && !self.can_replace(s) {
            return false;
        }
        let config = self.effective_config(mode);
        if mode == "synonyms" && config.backend == "gguf" && config.prompt_style != "writing" {
            self.mark_attempt(s, mode);
            return false;
        }
        let anchored = mode == "alternatives"
            && !anchor.is_empty()
            && self
                .suggestion
                .snapshot
                .as_ref()
                .is_some_and(|p| p.same_key(s) && p.same_authority(s));
        if anchored {
            self.bump_epoch();
            self.disarm();
            let _ = self.worker.cancel();
            self.request = None;
            self.preparer.cancel();
            self.choices = None;
            self.publish_completion();
        } else {
            // Scheduling this exact unchanged lease is a consumer handoff, not
            // a focus/policy revocation. Do not discard its bounded prefetch.
            // Spelling is also keyed to this draft and authority. Revoking
            // clear() invalidates it; this handoff must not make the first
            // result wait for the same spelling assessment a second time.
            let preserve_context = self.preparing.is_none()
                && self.request.is_none()
                && self
                    .context_prefetched
                    .as_ref()
                    .is_some_and(|(key, authority, epoch, at)| {
                        *key == s.key()
                            && *authority == s.authority()
                            && *epoch == self.current_epoch()
                            && self.now().saturating_duration_since(*at) < Duration::from_secs(2)
                    });
            self.clear_state(false, true, preserve_context);
            self.completion_counted = false;
        }
        self.error.clear();
        self.serial = self
            .serial
            .checked_add(1)
            .expect("request identifier exhausted");
        self.mark_attempt(s, mode);
        self.preparing = Some(PendingPreparation {
            token: self.serial,
            lease: Lease {
                snapshot: s.clone(),
                epoch: self.current_epoch(),
                revision: self.candidate_revision,
            },
            mode: mode.into(),
            anchor: if anchored { anchor } else { String::new() },
            config,
            native_pending: true,
        });
        #[cfg(feature = "native-smoke")]
        stage_diagnostics::scheduled(self.candidate_since, self.serial);
        self.whatsapp_stage(DiagnosticStage::Scheduled);
        self.whatsapp_state();
        true
    }
    fn advance_preparation(&mut self) {
        // Main thread acquires metadata only. The context owner independently
        // reacquires AX handles and fences every phase; no AppKit/TIS is moved.
        if self.preparing.as_ref().is_some_and(|p| p.native_pending) {
            let mut p = self.preparing.take().expect("preparation");
            #[cfg(feature = "native-smoke")]
            let _stage = DiagnosticSpan::start(TraceStage::NativeContextPreparation, Some(p.token));
            let current = self.capture();
            if !current
                .as_ref()
                .is_some_and(|s| p.lease.matches(s, self.current_epoch()) && s.has_anchor())
            {
                self.whatsapp_revocation(&p.lease, current.as_ref());
                self.revoke_inflight(true);
                return;
            }
            let context_ticket = if (p.config.screen_context || p.config.screenshot_context)
                && !p.lease.snapshot.prefix().trim().is_empty()
                && p.lease.snapshot.selection_length == 0
                && !p.lease.snapshot.window_id.is_empty()
                && !p.lease.snapshot.window_revision.is_empty()
            {
                self.context_status = "pending".into();
                self.bridge.as_mut().and_then(|bridge| {
                    self.readers
                        .context_ticket(&p.lease.snapshot, &mut **bridge)
                        .ok()
                        .flatten()
                })
            } else {
                None
            };
            let authorization_ticket = if p.config.clipboard_context
                || (p.config.personalization_strength != 0
                    && self.record_allowed(&p.lease.snapshot.app_id, &p.lease.snapshot.domain))
            {
                self.bridge.as_mut().and_then(|bridge| {
                    self.readers
                        .context_ticket(&p.lease.snapshot, &mut **bridge)
                        .ok()
                        .flatten()
                })
            } else {
                None
            };
            let current = self.capture();
            if !current
                .as_ref()
                .is_some_and(|s| p.lease.matches(s, self.current_epoch()))
            {
                self.whatsapp_revocation(&p.lease, current.as_ref());
                self.revoke_inflight(true);
                return;
            }
            if p.lease.snapshot.app_id == WHATSAPP_APP_ID {
                self.whatsapp_diagnostic.last_screen_ticket_available =
                    Some(context_ticket.is_some());
                self.whatsapp_diagnostic.last_authorization_ticket_available =
                    Some(authorization_ticket.is_some());
            }
            p.native_pending = false;
            let mut hard_disabled_apps = BTreeSet::new();
            for (scope, (enabled, expires)) in &self.temporary {
                if !enabled && self.now() < *expires {
                    if let Some(app) = scope.strip_prefix("app:") {
                        hard_disabled_apps.insert(app.to_owned());
                    }
                }
            }
            if !self.escape_app.is_empty() {
                hard_disabled_apps.insert(self.escape_app.clone());
            }
            #[cfg(feature = "native-smoke")]
            stage_diagnostics::mark(TraceStage::ContextQueued, Some(p.token));
            self.preparer.submit(Preparation {
                token: p.token,
                epoch: p.lease.epoch,
                snapshot: p.lease.snapshot.clone(),
                config: p.config.clone(),
                mode: p.mode.clone(),
                anchor: p.anchor.clone(),
                references: Vec::new(),
                context_ticket,
                authorization_ticket,
                hard_disabled_apps,
            });
            self.preparing = Some(p);
            self.whatsapp_stage(DiagnosticStage::PreparationQueued);
            self.whatsapp_state();
        }
        for mut prepared in self.preparer.poll() {
            let Some(p) = self.preparing.take() else {
                continue;
            };
            if prepared.token != p.token || prepared.epoch != p.lease.epoch {
                self.preparing = Some(p);
                continue;
            }
            #[cfg(feature = "native-smoke")]
            stage_diagnostics::mark(TraceStage::ContextConsumed, Some(p.token));
            self.whatsapp_stage(DiagnosticStage::Prepared);
            let current = self.capture();
            if !current.as_ref().is_some_and(|s| {
                p.lease.matches(s, self.current_epoch())
                    && s.has_anchor()
                    && (p.mode != "completion" || s.inline_safe())
            }) {
                self.whatsapp_revocation(&p.lease, current.as_ref());
                self.revoke_inflight(true);
                continue;
            }
            let c = self.effective_config(&p.mode);
            if prepared.reference_expires.is_some_and(|at| self.now() > at) {
                prepared.options.context.clear();
                prepared.reference_expires = None;
                prepared.context_diagnostic.attachment = ContextAttachmentOutcome::Expired;
            }
            if c.provider_identity() != p.config.provider_identity()
                || c.prompt_style != p.config.prompt_style
            {
                self.attempted = None;
                continue;
            }
            self.error = prepared.warning;
            if self.context_status != "error" {
                self.context_status = prepared.context_status;
            }
            let s = &p.lease.snapshot;
            match self.worker.submit(
                &c,
                last(s.prefix(), c.context_chars as usize),
                &first(s.selection_suffix(), c.context_chars as usize),
                &prepared.options,
            ) {
                Ok(id) => {
                    if s.app_id == WHATSAPP_APP_ID {
                        self.whatsapp_diagnostic.last_context_attached =
                            Some(!prepared.options.context.is_empty());
                        self.whatsapp_diagnostic.last_context_read_ready = Some(
                            prepared.context_diagnostic.screen == ContextCollectorOutcome::Ready
                                || prepared.context_diagnostic.clipboard
                                    == ContextCollectorOutcome::Ready,
                        );
                        self.whatsapp_diagnostic.last_context_preparation =
                            Some(prepared.context_diagnostic);
                        self.whatsapp_diagnostic.last_request_id = Some(id);
                        self.whatsapp_diagnostic.last_submission_mode =
                            DiagnosticMode::from_mode(&p.mode);
                    }
                    self.request = Some(Request {
                        id,
                        lease: p.lease,
                        generation: self.suggestion.generation,
                        mode: p.mode,
                        length: c.completion_length.clone(),
                        pending_text: None,
                        final_: false,
                        proceeded: false,
                        reference_expires: prepared.reference_expires,
                        provider_identity: c.provider_identity(),
                        prompt_style: c.prompt_style.clone(),
                        #[cfg(feature = "native-smoke")]
                        first_usable_observed: false,
                    });
                    self.whatsapp_stage(DiagnosticStage::Submitted);
                    self.whatsapp_state();
                    self.whatsapp_worker();
                }
                Err(_) => {
                    self.whatsapp_stage(DiagnosticStage::SubmissionError);
                    self.whatsapp_worker();
                    self.fail("Local inference submission");
                }
            }
        }
    }
    fn consume_results(&mut self) {
        let events = self.worker.poll();
        if !events.is_empty() {
            let fresh = self.capture();
            self.observe(fresh);
        }
        for event in events {
            let Some(mut request) = self.request.take() else {
                if self.whatsapp_diagnostic.last_request_id == Some(event.id) {
                    let count = &mut self.whatsapp_diagnostic.counters.stale_worker_events;
                    *count = count.saturating_add(1);
                }
                continue;
            };
            if event.id != request.id {
                if request.lease.snapshot.app_id == WHATSAPP_APP_ID {
                    let count = &mut self.whatsapp_diagnostic.counters.stale_worker_events;
                    *count = count.saturating_add(1);
                }
                self.request = Some(request);
                continue;
            }
            let current = self.candidate.clone();
            if !self.active()
                || !current
                    .as_ref()
                    .is_some_and(|s| request.lease.matches(s, self.current_epoch()))
            {
                self.whatsapp_revocation(&request.lease, current.as_ref());
                self.revoke_inflight(true);
                continue;
            }
            let current = current.expect("current lease");
            match event.kind.as_str() {
                "ready" => {
                    #[cfg(feature = "native-smoke")]
                    stage_diagnostics::mark(TraceStage::RuntimeReady, Some(request.id));
                    self.whatsapp_stage(DiagnosticStage::Ready);
                    self.whatsapp_worker();
                    let effective = self.effective_config(&request.mode);
                    if request.reference_expires.is_some_and(|at| self.now() > at)
                        || effective.provider_identity() != request.provider_identity
                        || effective.prompt_style != request.prompt_style
                        || effective.completion_length != request.length
                    {
                        self.submit(&current, &request.mode, self.suggestion.text.clone());
                        continue;
                    }
                    let fresh = self.capture();
                    if fresh.as_ref().is_some_and(|s| {
                        request.lease.matches(s, self.current_epoch())
                            && Some(request.lease.revision) == self.user_revision()
                            && s.has_anchor()
                            && (request.mode != "completion" || s.inline_safe())
                    }) {
                        if self.worker.proceed(request.id).is_ok() {
                            #[cfg(feature = "native-smoke")]
                            stage_diagnostics::mark(TraceStage::RuntimeProceed, Some(request.id));
                            request.proceeded = true;
                            self.request = Some(request);
                            self.whatsapp_stage(DiagnosticStage::Proceeded);
                            self.whatsapp_state();
                            self.whatsapp_worker();
                        } else {
                            self.fail("Local inference authorization");
                            self.clear(false, true);
                        }
                    } else {
                        self.whatsapp_revocation(&request.lease, fresh.as_ref());
                        self.revoke_inflight(true);
                    }
                }
                "result" if request.proceeded => {
                    self.whatsapp_stage(DiagnosticStage::Result);
                    self.whatsapp_worker();
                    if request.mode == "completion" {
                        request.pending_text = Some(if request.length == "word" {
                            features::current_word_only(current.prefix(), &event.text)
                        } else {
                            event.text
                        });
                        request.final_ |= event.final_;
                        self.request = Some(request);
                        self.publish_pending_completion();
                    } else if event.final_ {
                        let mode = request.mode.clone();
                        let mut choices = features::parse_choices(&event.text, 5);
                        if mode == "synonyms" {
                            let selected = core::casefold(current.selected_text());
                            choices.retain(|c| core::casefold(c) != selected);
                        }
                        let anchored = mode == "alternatives"
                            && !self.suggestion.text.is_empty()
                            && self.suggestion.snapshot.as_ref().is_some_and(|s| {
                                s.same_key(&current) && s.same_authority(&current)
                            });
                        let length = if mode == "synonyms" {
                            current.selection_length
                        } else {
                            0
                        };
                        let start = current.caret;
                        self.set_choices(current, choices, &mode, start, length, anchored);
                    } else {
                        self.request = Some(request);
                    }
                }
                "error" => {
                    self.whatsapp_stage(DiagnosticStage::WorkerError);
                    self.whatsapp_worker();
                    self.fail("Local inference");
                    self.clear(false, false);
                }
                "done" => {
                    request.final_ = true;
                    if request.pending_text.is_some() {
                        self.request = Some(request);
                        self.publish_pending_completion();
                    }
                }
                _ => self.request = Some(request),
            }
        }
        self.publish_pending_completion();
    }
    fn publish_pending_completion(&mut self) {
        let Some(mut request) = self.request.take() else {
            return;
        };
        let Some(s) = self.candidate.clone() else {
            return;
        };
        if !request.lease.matches(&s, self.current_epoch()) {
            self.whatsapp_revocation(&request.lease, Some(&s));
            self.revoke_inflight(true);
            return;
        }
        if request.mode != "completion" || request.pending_text.is_none() {
            self.request = Some(request);
            return;
        }
        match self.completion_blocked(&s) {
            None => {
                self.request = Some(request);
            } // R7: final text remains owned until verdict.
            Some(true) => {
                if !request.final_ {
                    self.request = Some(request);
                }
            }
            Some(false) => {
                let fresh = self.capture();
                if !fresh.as_ref().is_some_and(|current| {
                    request.lease.matches(current, self.current_epoch()) && current.inline_safe()
                }) {
                    self.whatsapp_revocation(&request.lease, fresh.as_ref());
                    self.revoke_inflight(true);
                    return;
                }
                let text = request.pending_text.take().expect("pending text");
                if self.suggestion.offer(request.generation, s, text) {
                    #[cfg(feature = "native-smoke")]
                    if !request.first_usable_observed && !self.suggestion.text.trim().is_empty() {
                        stage_diagnostics::mark(
                            TraceStage::FirstUsableCompletion,
                            Some(request.id),
                        );
                        request.first_usable_observed = true;
                    }
                    self.publish_completion();
                }
                if !request.final_ {
                    self.request = Some(request);
                }
            }
        }
    }
    fn schedule(&mut self, s: &Snapshot) {
        if s.app_id == WHATSAPP_APP_ID {
            self.whatsapp_diagnostic.last_schedule_eligible = Some(false);
            self.whatsapp_diagnostic.last_spelling_pending = false;
            self.whatsapp_diagnostic.last_spelling_suspected = None;
        }
        if !self.active()
            || s.on_demand_only
            || self.field_paused(s)
            || self.pending_edit.is_some()
            || !s.has_anchor()
        {
            return;
        }
        if self
            .uncertain_provenance
            .as_ref()
            .is_some_and(|(authority, revision)| {
                *authority == s.authority() && Some(*revision) == self.user_revision()
            })
        {
            return;
        }
        let selected = selected_word(s);
        let mut mode = if selected.is_empty() {
            "completion"
        } else {
            "synonyms"
        };
        let mut eligible = if !selected.is_empty() {
            self.can_replace(s)
                && self.config.synonyms_enabled
                && self.config.synonyms_mode == "selection"
        } else {
            s.inline_safe()
                && s.eligible(self.config.min_chars as usize)
                && (self.config.complete_inside_text || s.line_suffix().trim().is_empty())
                && (self.config.completion_length != "word" || features::mid_word(s.prefix()))
        };
        if self.can_replace(s) && selected.is_empty() && s.eligible(1) {
            if let Some((suffix, choices)) = features::emoji_choices(s.prefix(), &self.config) {
                if !suffix.is_empty()
                    && s.prefix().ends_with(&suffix)
                    && !self.attempted(s, "emoji")
                {
                    self.clear(false, true);
                    self.mark_attempt(s, "emoji");
                    self.set_choices(
                        s.clone(),
                        choices,
                        "emoji",
                        s.caret - suffix.chars().count(),
                        suffix.chars().count(),
                        false,
                    );
                }
                return;
            }
        }
        if selected.is_empty() && self.typo_active(s) && s.eligible(1) {
            let assessment = self.assess(s);
            if s.app_id == WHATSAPP_APP_ID {
                self.whatsapp_diagnostic.last_spelling_pending = assessment.is_none();
                self.whatsapp_diagnostic.last_spelling_suspected =
                    assessment.as_ref().map(|r| r.suspected);
            }
            match assessment {
                None => eligible = false,
                Some(result) if result.suspected => {
                    if !self.attempted(s, "correction") && self.show_correction(s, result) {
                        eligible = false;
                    }
                    if self.config.typo_suppression {
                        eligible = false;
                    }
                }
                _ => {}
            }
        }
        if self.config.alternatives_enabled
            && self.config.alternatives_mode == "pause"
            && selected.is_empty()
            && self.request.is_none()
            && self.preparing.is_none()
            && !self.suggestion.text.is_empty()
            && self.correction.is_none()
            && self.claims.iter().any(|c| c.mode == "completion")
            && !self.attempted(s, "alternatives")
            && self.now().duration_since(self.candidate_since)
                >= Duration::from_millis(self.config.alternatives_delay_ms.into())
        {
            self.submit(s, "alternatives", self.suggestion.text.clone());
            return;
        }
        if self.on_battery && self.config.battery_on_demand {
            eligible = false;
        }
        if s.app_id == WHATSAPP_APP_ID {
            self.whatsapp_diagnostic.last_schedule_eligible = Some(eligible);
        }
        let delay = if mode == "synonyms" {
            self.config.synonyms_delay_ms
        } else {
            self.config.debounce_ms
        };
        if eligible
            && self.now().duration_since(self.candidate_since)
                >= Duration::from_millis(delay.into())
            && !self.attempted(s, mode)
            && (mode != "completion"
                || (self.suggestion.text.is_empty() && self.correction.is_none()))
        {
            if !selected.is_empty() {
                mode = "synonyms";
            }
            self.submit(s, mode, String::new());
        }
    }
    fn prepare_models(&mut self) {
        if !self.active() || (self.on_battery && self.config.battery_on_demand) {
            self.resident = None;
            return;
        }
        if self.config.idle_release_seconds == 0 {
            let c = self.effective_config("completion");
            let identity = c.provider_identity();
            if self.resident.as_ref() != Some(&identity) {
                self.resident = Some(identity);
                if self.worker.prepare(&c).is_err() {
                    self.fail("Model preparation");
                }
            }
        }
        let Some(s) = self.candidate.as_ref() else {
            return;
        };
        let empty_native = s.text.is_empty()
            && s.caret == 0
            && s.window_pid > 0
            && !s.window_id.is_empty()
            && s.field_rect.as_ref().is_some_and(valid_rect);
        if s.protected
            || !s.multiline
            || (!s.inline_safe() && !empty_native)
            || !s.has_anchor()
            || s.selection_length != 0
            || s.on_demand_only
            || self.field_paused(s)
        {
            return;
        }
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(s.text.as_ref(), &mut hash);
        let fingerprint = std::hash::Hasher::finish(&hash);
        if self
            .model_activity
            .as_ref()
            .is_some_and(|(text, authority, revision, caret)| {
                *text == fingerprint
                    && authority_matches(authority, s)
                    && *revision == self.candidate_revision
                    && *caret == s.caret
            })
        {
            return;
        }
        self.model_activity = Some((fingerprint, s.authority(), self.candidate_revision, s.caret));
        let c = self.effective_config("completion");
        if self.worker.prepare(&c).is_err() {
            self.fail("Model preparation");
        }
    }
    fn release_model(&mut self) {
        self.resident = None;
        self.model_activity = None;
        if self.worker.release().is_err() {
            self.fail("Model release");
        }
    }
    fn presentation_is_current(
        &self,
        key: &SnapshotKey,
        authority: &Authority,
        revision: u64,
        mode: &str,
    ) -> bool {
        let completion_view = mode == "completion"
            || (mode == "alternatives" && self.choices.as_ref().is_some_and(|c| c.anchored));
        self.candidate
            .as_ref()
            .is_some_and(|s| s.key() == *key && s.authority() == *authority)
            && Some(self.candidate_revision) == self.user_revision()
            && if completion_view {
                self.completion_offer.is_some_and(|offer| {
                    offer.serial == revision
                        && offer.epoch == self.current_epoch()
                        && offer.revision == self.candidate_revision
                }) && !self.input_pending.load(Ordering::Acquire)
                    && self.native_input_revision() == Some(self.input_revision)
            } else {
                revision == self.candidate_revision
            }
    }
    fn presentation_count(
        &self,
        key: &SnapshotKey,
        authority: &Authority,
        visible: &str,
        mode: &str,
    ) -> Option<usize> {
        if visible.is_empty() {
            return None;
        }
        match mode {
            "completion" => (self.suggestion.snapshot.as_ref().is_some_and(|s| {
                s.key() == *key
                    && s.authority() == *authority
                    && (s.inline_safe()
                        || self.advance.as_ref().is_some_and(|(k, a, g, _)| {
                            *k == *key && *a == *authority && g.inline_safe()
                        }))
            }) && self.suggestion.text.starts_with(visible))
            .then_some(0),
            "correction" => self
                .correction
                .as_ref()
                .is_some_and(|(s, r)| {
                    s.key() == *key && s.authority() == *authority && r.replacement == visible
                })
                .then_some(0),
            "alternatives" | "synonyms" | "emoji" => {
                let choices = self.choices.as_ref().filter(|choices| {
                    choices.mode == mode
                        && choices.snapshot.key() == *key
                        && choices.snapshot.authority() == *authority
                })?;
                let count = visible
                    .split('\n')
                    .zip(&choices.values)
                    .take_while(|(shown, value)| *shown == value.as_str())
                    .count();
                (count != 0).then_some(count)
            }
            _ => None,
        }
    }
    fn native_claim_valid(&self, claim: &Claim) -> bool {
        match &claim.evidence {
            PresentationEvidence::Rendered => true,
            PresentationEvidence::NativePreedit(receipt) => self
                .bridge
                .as_ref()
                .is_some_and(|bridge| bridge.native_preedit_valid(receipt)),
            PresentationEvidence::NativeChoices(receipt) => self
                .bridge
                .as_ref()
                .is_some_and(|bridge| bridge.native_choices_valid(receipt)),
        }
    }
    fn present_preedit(
        &mut self,
        key: &SnapshotKey,
        authority: &Authority,
        revision: u64,
        anchor: &str,
        text: &str,
    ) -> Result<bool> {
        if !self.native_ready
            || self.stopping
            || self.shortcuts_suspended
            || !self.presentation_is_current(key, authority, revision, "completion")
            || self
                .presentation_count(key, authority, text, "completion")
                .is_none()
            || !self
                .candidate
                .as_ref()
                .is_some_and(|s| s.native_anchor == anchor)
        {
            return Ok(false);
        }
        let receipt = self
            .bridge
            .as_mut()
            .map_or(Ok(None), |bridge| bridge.present_preedit(anchor, text))?;
        let Some(receipt) = receipt else {
            return Ok(false);
        };
        let valid = receipt.anchor == anchor
            && receipt.text == text
            && Some(receipt.revision) == self.user_revision()
            && self.presentation_is_current(key, authority, revision, "completion")
            && self
                .bridge
                .as_ref()
                .is_some_and(|bridge| bridge.native_preedit_valid(&receipt));
        if !valid {
            if let Some(bridge) = &mut self.bridge {
                let _ = bridge.hide_native();
            }
            return Ok(false);
        }
        self.claims.retain(|claim| claim.mode != "completion");
        self.claims.push(Arc::new(Claim {
            key: key.clone(),
            authority: authority.clone(),
            revision: self.candidate_revision,
            epoch: self.current_epoch(),
            visible: receipt.text.clone(),
            mode: "completion".into(),
            count: 0,
            evidence: PresentationEvidence::NativePreedit(receipt),
        }));
        self.refresh_arming();
        Ok(true)
    }
    fn present_choices(
        &mut self,
        key: &SnapshotKey,
        authority: &Authority,
        revision: u64,
        anchor: &str,
        mode: &str,
        values: &[String],
    ) -> Result<bool> {
        if !matches!(mode, "alternatives" | "synonyms" | "emoji")
            || !self.native_ready
            || self.stopping
            || self.shortcuts_suspended
            || !self.presentation_is_current(key, authority, revision, mode)
            || !self
                .candidate
                .as_ref()
                .is_some_and(|s| s.native_anchor == anchor)
            || !self
                .choices
                .as_ref()
                .is_some_and(|choices| choices.mode == mode && choices.values == values)
        {
            return Ok(false);
        }
        let receipt = self
            .bridge
            .as_mut()
            .map_or(Ok(None), |bridge| bridge.present_choices(anchor, values))?;
        let Some(receipt) = receipt else {
            return Ok(false);
        };
        let count = receipt.values.len();
        let valid = count != 0
            && count <= values.len()
            && receipt.values == values[..count]
            && receipt.anchor == anchor
            && Some(receipt.revision) == self.user_revision()
            && self.presentation_is_current(key, authority, revision, mode)
            && self
                .bridge
                .as_ref()
                .is_some_and(|bridge| bridge.native_choices_valid(&receipt));
        if !valid {
            if let Some(bridge) = &mut self.bridge {
                let _ = bridge.hide_native();
            }
            return Ok(false);
        }
        self.claims.retain(|claim| claim.mode != mode);
        self.claims.push(Arc::new(Claim {
            key: key.clone(),
            authority: authority.clone(),
            revision: self.candidate_revision,
            epoch: self.current_epoch(),
            visible: receipt.values.join("\n"),
            mode: mode.into(),
            count,
            evidence: PresentationEvidence::NativeChoices(receipt),
        }));
        self.refresh_arming();
        Ok(true)
    }
    fn renew_native_presentations(&mut self) {
        let renew_by = self.now() + Duration::from_millis(100);
        for _ in 0..self.claims.len() {
            let Some(claim) = self
                .claims
                .iter()
                .find(|claim| match &claim.evidence {
                    PresentationEvidence::Rendered => false,
                    PresentationEvidence::NativePreedit(receipt) => receipt.expires_at <= renew_by,
                    PresentationEvidence::NativeChoices(receipt) => receipt.expires_at <= renew_by,
                })
                .cloned()
            else {
                break;
            };
            let view_revision = if claim.mode == "completion"
                || (claim.mode == "alternatives"
                    && self
                        .choices
                        .as_ref()
                        .is_some_and(|choices| choices.anchored))
            {
                self.completion_offer.map(|offer| offer.serial)
            } else {
                Some(claim.revision)
            };
            let renewed = if claim.epoch == self.current_epoch()
                && Some(claim.revision) == self.user_revision()
            {
                if self
                    .bridge
                    .as_ref()
                    .is_some_and(|bridge| bridge.client_owns_actions())
                {
                    Ok(view_revision
                        .and_then(|revision| {
                            self.present_client(&claim.key, &claim.authority, revision, &claim.mode)
                        })
                        .unwrap_or(false))
                } else {
                    match (&claim.evidence, view_revision) {
                        (PresentationEvidence::NativePreedit(receipt), Some(revision)) => self
                            .present_preedit(
                                &claim.key,
                                &claim.authority,
                                revision,
                                &receipt.anchor,
                                &receipt.text,
                            ),
                        (PresentationEvidence::NativeChoices(receipt), Some(revision)) => {
                            let values = self
                                .choices
                                .as_ref()
                                .map(|choices| choices.values.clone())
                                .unwrap_or_default();
                            self.present_choices(
                                &claim.key,
                                &claim.authority,
                                revision,
                                &receipt.anchor,
                                &claim.mode,
                                &values,
                            )
                        }
                        _ => Ok(false),
                    }
                }
            } else {
                Ok(false)
            };
            if !matches!(renewed, Ok(true)) {
                self.claims
                    .retain(|current| matches!(&current.evidence, PresentationEvidence::Rendered));
                if let Some(bridge) = &mut self.bridge {
                    let _ = bridge.hide_native();
                }
                self.redraw_pending = true;
                self.refresh_arming();
                break;
            }
        }
    }

    fn refresh_arming(&self) {
        let client_owned = self
            .bridge
            .as_ref()
            .is_some_and(|bridge| bridge.client_owns_actions());
        let Ok(mut a) = self.armed.try_lock() else {
            return;
        };
        if self.shortcuts_suspended || self.stopping || client_owned {
            a.claims.clear();
        } else {
            a.claims.clear();
            a.claims.extend(
                self.claims
                    .iter()
                    .filter(|claim| self.native_claim_valid(claim))
                    .cloned(),
            );
        }
        if let Some(candidate) = &self.candidate {
            if a.origin.as_ref().is_none_or(|origin| {
                origin.1 != self.candidate_revision
                    || origin.2 != self.current_epoch()
                    || !origin.0.same_key(candidate)
                    || !origin.0.same_authority(candidate)
            }) {
                a.origin = Some(Arc::new((
                    candidate.clone(),
                    self.candidate_revision,
                    self.current_epoch(),
                )));
            }
        } else {
            a.origin = None;
        }
        a.active = self.active() && self.native_ready && !self.shortcuts_suspended && !client_owned;
        a.alternatives = self.config.alternatives_enabled;
        a.synonyms = self.config.synonyms_enabled
            && self.candidate.as_ref().is_some_and(|s| self.can_replace(s));
        a.escape_passthrough = self.config.escape_behavior == "passthrough";
        a.inflight = self.request.is_some() || self.preparing.is_some();
    }
    fn presentation(
        &mut self,
        key: SnapshotKey,
        authority: Authority,
        revision: u64,
        visible: String,
        mode: String,
    ) {
        let current = self.presentation_is_current(&key, &authority, revision, &mode);
        let count = self.presentation_count(&key, &authority, &visible, &mode);
        let valid = current && count.is_some();
        let count = count.unwrap_or(0);
        #[cfg(feature = "native-smoke")]
        if mode == "completion" {
            stage_diagnostics::mark(
                if valid && self.native_ready && !self.shortcuts_suspended {
                    TraceStage::PresentationAccepted
                } else {
                    TraceStage::PresentationRejected
                },
                None,
            );
        }
        if key.app_id == WHATSAPP_APP_ID {
            let diagnostic = &mut self.whatsapp_diagnostic;
            diagnostic.observed = true;
            diagnostic.counters.presentation_acks =
                diagnostic.counters.presentation_acks.saturating_add(1);
            diagnostic.last_ack_mode = DiagnosticMode::from_mode(&mode);
            diagnostic.last_ack_visible = !visible.is_empty();
            diagnostic.last_ack_current = current;
            diagnostic.last_ack_accepted = valid && self.native_ready && !self.shortcuts_suspended;
            if diagnostic.last_ack_accepted {
                diagnostic.last_stage = DiagnosticStage::Presented;
                diagnostic.counters.rendered_acks =
                    diagnostic.counters.rendered_acks.saturating_add(1);
            } else {
                diagnostic.last_stage = DiagnosticStage::PresentationRejected;
                diagnostic.counters.rejected_acks =
                    diagnostic.counters.rejected_acks.saturating_add(1);
            }
        }
        self.claims.retain(|c| c.mode != mode);
        if valid && self.native_ready && !self.shortcuts_suspended {
            self.claims.push(Arc::new(Claim {
                key,
                authority,
                revision: self.candidate_revision,
                epoch: self.current_epoch(),
                visible,
                mode,
                count,
                evidence: PresentationEvidence::Rendered,
            }));
        }
        self.refresh_arming();
        self.whatsapp_state();
    }
    fn accept(&mut self, action: &Action, supplied: Option<Arc<Claim>>) -> bool {
        let Some(current) = self.capture().filter(Snapshot::has_anchor) else {
            self.clear(true, true);
            return false;
        };
        let claim = supplied.or_else(|| match action {
            Action::Choice(_) => self
                .claims
                .iter()
                .find(|c| matches!(c.mode.as_str(), "alternatives" | "emoji" | "synonyms"))
                .cloned(),
            _ => primary_claim_index(&self.claims, false).map(|index| self.claims[index].clone()),
        });
        self.disarm();
        let Some(claim) = claim.filter(|c| {
            c.key == current.key()
                && c.authority == current.authority()
                && Some(c.revision) == self.user_revision()
                && c.epoch == self.current_epoch()
                && self.native_claim_valid(c)
        }) else {
            return false;
        };
        let (text, start, length, remainder) = if claim.mode == "correction" {
            let Some((s, r)) = &self.correction else {
                return false;
            };
            if !s.same_key(&current)
                || !s.same_authority(&current)
                || !self.can_replace(&current)
                || r.replacement != claim.visible
            {
                return false;
            }
            (r.replacement.clone(), r.start, r.length, None)
        } else if matches!(claim.mode.as_str(), "alternatives" | "synonyms" | "emoji") {
            let Some(c) = &self.choices else {
                return false;
            };
            let index = match action {
                Action::Choice(i) => *i,
                _ if c.mode != "alternatives" => 0,
                _ => return false,
            };
            if index >= c.values.len()
                || index >= claim.count
                || c.mode != claim.mode
                || !c.snapshot.same_key(&current)
                || !c.snapshot.same_authority(&current)
            {
                return false;
            }
            if (c.length != 0
                || c.start != current.caret
                || c.mode == "emoji"
                || c.mode == "synonyms")
                && !self.can_replace(&current)
            {
                return false;
            }
            (c.values[index].clone(), c.start, c.length, None)
        } else {
            if !current.inline_safe() || !current.eligible(1) {
                self.clear(true, true);
                return false;
            }
            let mut proposed = self.suggestion.clone();
            let Some(text) = proposed.accept(
                &current,
                *action == Action::All,
                self.config.trailing_space,
                features::include_punctuation(
                    &self.config.punctuation_mode,
                    *action == Action::All,
                ),
                Some(&claim.visible),
            ) else {
                return false;
            };
            (text, current.caret, 0, Some(proposed))
        };
        let Some(expected) = edit_snapshot(&current, start, length, &text) else {
            return false;
        };
        if !self.input_claim_valid() {
            return false;
        }
        let outcome = self
            .bridge
            .as_mut()
            .ok_or_else(|| anyhow!("Native input is unavailable."))
            .and_then(|bridge| {
                if start == current.caret && length == 0 {
                    bridge.insert(&current, &text)
                } else {
                    bridge.replace_range(&current, start, length, &text)
                }
            });
        let edit = PendingEdit {
            before: current.clone(),
            expected,
            text,
            start,
            length,
            remainder,
            at: self.now(),
            revision: claim.revision,
            mode: claim.mode.clone(),
        };
        match outcome {
            Ok(EditOutcome::Applied(readback)) => {
                if confirmed_edit(&edit, &readback) {
                    self.commit_edit(edit, readback);
                    true
                } else {
                    self.clear(true, true);
                    self.uncertain_provenance = Some((current.authority(), claim.revision));
                    true
                } // uncertain native call: never replay.
            }
            Ok(EditOutcome::Dispatched) => {
                self.clear(false, true);
                self.uncertain_provenance = Some((current.authority(), claim.revision));
                self.pending_edit = Some(edit);
                true
            }
            Ok(EditOutcome::Indeterminate) | Err(_) => {
                self.clear(true, true);
                self.uncertain_provenance = Some((current.authority(), claim.revision));
                self.mark_attempt(&current, &claim.mode);
                true
            }
            Ok(EditOutcome::Rejected) => {
                self.clear(false, true);
                false
            }
        }
    }
    fn observe_pending_edit(&mut self, snapshot: &Snapshot) {
        let Some(edit) = self.pending_edit.take() else {
            return;
        };
        if confirmed_edit(&edit, snapshot) && self.user_revision() == Some(edit.revision) {
            self.commit_edit(edit, snapshot.clone());
        } else if snapshot.same_key(&edit.before)
            && snapshot.same_authority(&edit.before)
            && self.now().duration_since(edit.at) < Duration::from_secs(2)
            && self.user_revision() == Some(edit.revision)
        {
            self.pending_edit = Some(edit);
        }
        // Any different observation expires the uncertain dispatch. Never retry it.
    }
    fn commit_edit(&mut self, edit: PendingEdit, readback: Snapshot) {
        let count_completion = !self.completion_counted;
        self.commit_confirmed_edit(edit, readback, true, count_completion);
    }
    fn commit_confirmed_edit(
        &mut self,
        edit: PendingEdit,
        readback: Snapshot,
        restore_view: bool,
        count_completion: bool,
    ) {
        if self.config.statistics_enabled {
            self.push_record(Record::Stats {
                app: edit.before.app_id.clone(),
                text: edit.text.clone(),
                completion: count_completion,
            });
        }
        if restore_view {
            self.completion_counted = true;
        }
        if self
            .uncertain_provenance
            .as_ref()
            .is_some_and(|(authority, _)| *authority == readback.authority())
        {
            self.uncertain_provenance = None;
        }
        if self.record_allowed(&edit.before.app_id, &edit.before.domain)
            && (restore_view
                || self.session.as_ref().is_some_and(|session| {
                    session.key == edit.before.authority()
                        && session.text.as_str() == edit.before.text.as_ref()
                }))
        {
            self.history_suppressed = None;
            self.observe_session(&edit.before);
            if let Some(session) = &mut self.session {
                if readback.text.scalar_len() <= 32000 {
                    session.authored.splice(
                        edit.start..edit.start + edit.length,
                        std::iter::repeat_n(false, edit.text.chars().count()),
                    );
                    session.text = readback.text.to_string();
                    session.accepted = true;
                    self.queue_session();
                } else {
                    self.finish_session();
                }
            }
        }
        if !restore_view {
            return;
        }
        self.clear(false, true);
        self.candidate = Some(readback.clone());
        if let Some(revision) = self.user_revision() {
            self.candidate_revision = revision;
        }
        self.capture_after = self.now();
        if let Some(mut remainder) = edit.remainder {
            if !remainder.text.is_empty() {
                if !readback.inline_safe() && edit.before.inline_safe() {
                    self.advance = Some((
                        readback.key(),
                        readback.authority(),
                        edit.before.clone(),
                        edit.text.clone(),
                    ));
                }
                remainder.snapshot = Some(readback.clone());
                self.suggestion = remainder;
                self.publish_completion();
            }
        }
        if edit.mode != "completion" {
            self.candidate_since = self.now();
        }
        self.mark_attempt(&readback, &edit.mode);
    }
    fn dismiss(&mut self, bypass: bool, claim: Option<Arc<Claim>>) -> bool {
        let claim = claim.or_else(|| {
            primary_claim_index(&self.claims, true).map(|index| self.claims[index].clone())
        });
        if let Some(claim) = claim {
            if !self.owns_claim(&claim) || !self.native_claim_valid(&claim) {
                return false;
            }
            let Some(current) = self.capture() else {
                self.clear(true, true);
                return false;
            };
            if current.key() != claim.key
                || current.authority() != claim.authority
                || !self.input_claim_valid()
                || !self.native_claim_valid(&claim)
            {
                self.clear(true, true);
                return false;
            }
            return self.finish_dismissal(&current, &claim.mode, bypass, self.now(), true);
        }
        if !bypass && (self.request.is_some() || self.preparing.is_some()) {
            let mode = self
                .request
                .as_ref()
                .map(|r| r.mode.clone())
                .or_else(|| self.preparing.as_ref().map(|p| p.mode.clone()))
                .unwrap_or_else(|| "completion".into());
            let candidate = self.candidate.clone();
            self.clear(false, true);
            if let Some(s) = candidate {
                self.mark_attempt(&s, &mode);
            }
        }
        false
    }
    fn action(
        &mut self,
        action: Action,
        claim: Option<Arc<Claim>>,
        origin: Option<Arc<(Snapshot, u64, u64)>>,
    ) -> bool {
        if self.stopping || self.shortcuts_suspended || !self.input_claim_valid() {
            return false;
        }
        if let Some(origin) = origin {
            if origin.2 != self.current_epoch() {
                return false;
            }
            let current = self.capture();
            if origin.2 != self.current_epoch()
                || Some(origin.1) != self.user_revision()
                || !current
                    .as_ref()
                    .is_some_and(|s| s.same_key(&origin.0) && s.same_authority(&origin.0))
            {
                return false;
            }
            if !self.input_claim_valid() {
                return false;
            }
        }
        match action {
            Action::Word | Action::All | Action::Choice(_) => self.accept(&action, claim),
            Action::Dismiss | Action::Bypass => self.dismiss(action == Action::Bypass, claim),
            Action::Settings => {
                self.emit(json!({"type":"settings","pane":"general"}));
                true
            }
            Action::Quit => {
                self.emit(json!({"type":"quit"}));
                true
            }
            Action::Toggle => {
                if self.config.global_enabled && self.now() < self.paused_until {
                    self.paused_until = self.now();
                    self.attempted = None;
                    self.configure_native();
                } else {
                    let mut c = self.config.clone();
                    c.global_enabled = !c.global_enabled;
                    if self.apply_config(c, true).is_err() {
                        self.fail("Preferences");
                    }
                }
                true
            }
            Action::AppToggle => {
                let Some(info) = self.foreground() else {
                    return false;
                };
                if !self.input_claim_valid() {
                    return false;
                }
                self.toggle_app(&info.app_id, &info.domain)
            }
            Action::Force | Action::Alternatives | Action::Synonyms => {
                if !self.active()
                    || (action == Action::Alternatives && !self.config.alternatives_enabled)
                    || (action == Action::Synonyms && !self.config.synonyms_enabled)
                {
                    return false;
                }
                let Some(s) = self.capture().filter(Snapshot::has_anchor) else {
                    return false;
                };
                let mode = if action == Action::Force {
                    "completion"
                } else if action == Action::Alternatives {
                    "alternatives"
                } else {
                    "synonyms"
                };
                if (mode == "synonyms" && selected_word(&s).is_empty())
                    || (mode != "synonyms" && !s.eligible(1))
                    || (mode == "completion"
                        && (!s.inline_safe()
                            || (self.config.completion_length == "word"
                                && !features::mid_word(s.prefix()))))
                {
                    return false;
                }
                let anchor = if mode == "alternatives"
                    && self
                        .suggestion
                        .snapshot
                        .as_ref()
                        .is_some_and(|p| p.same_key(&s) && p.same_authority(&s))
                {
                    self.suggestion.text.clone()
                } else {
                    String::new()
                };
                if !self.input_claim_valid() {
                    return false;
                }
                self.observe(Some(s.clone()));
                self.submit(&s, mode, anchor)
            }
        }
    }
    fn run_inputs(&mut self) {
        while let Ok(input) = self.inputs.try_recv() {
            let bypass = input.action == Action::Bypass;
            if (input.transaction || bypass) && !self.input_claim_valid() {
                let _ = self.finish_input(false);
                continue;
            }
            let accepted = self.action(input.action, input.claim, input.origin);
            if (input.transaction || bypass) && self.finish_input(accepted && !bypass).is_err() {
                self.fail("Shortcut transaction");
            }
        }
    }
    fn observe_session(&mut self, s: &Snapshot) {
        if self
            .history_suppressed
            .as_ref()
            .is_some_and(|(key, text)| *key == s.authority() && text.as_str() == s.text.as_ref())
        {
            return;
        }
        self.history_suppressed = None;
        if !self.record_allowed(&s.app_id, &s.domain) || s.text.scalar_len() > 32000 {
            self.finish_session();
            return;
        }
        if self
            .session
            .as_ref()
            .is_none_or(|session| session.key != s.authority())
        {
            self.finish_session();
            self.session_serial = self.session_serial.wrapping_add(1);
            self.session = Some(Session {
                key: s.authority(),
                id: format!(
                    "{:016x}{:016x}",
                    chrono::Utc::now().timestamp_micros(),
                    self.session_serial
                ),
                text: s.text.to_string(),
                authored: vec![false; s.text.scalar_len()],
                accepted: false,
            });
        } else {
            let uncertain = self
                .uncertain_provenance
                .as_ref()
                .is_some_and(|(authority, _)| *authority == s.authority());
            let changed = self
                .session
                .as_ref()
                .is_some_and(|session| session.text.as_str() != s.text.as_ref());
            if let Some(session) = &mut self.session {
                update_authored(session, &s.text, !uncertain);
            }
            if changed
                && self
                    .uncertain_provenance
                    .as_ref()
                    .is_some_and(|(authority, revision)| {
                        *authority == s.authority()
                            && self
                                .user_revision()
                                .is_some_and(|current| *revision != current)
                    })
            {
                self.uncertain_provenance = None;
            }
        }
    }
    fn queue_session(&mut self) {
        if let Some(s) = &self.session {
            let record = Record::History {
                id: s.id.clone(),
                app: s.key.app_id.clone(),
                domain: s.key.domain.clone(),
                text: s.text.clone(),
                spans: authored_spans(&s.authored),
            };
            self.push_record(record);
        }
    }
    fn push_record(&mut self, record: Record) {
        if self.records.len() == 128 {
            self.records.pop_front();
        }
        self.records.push_back(record);
    }
    fn finish_session(&mut self) {
        if let Some(s) = &self.session {
            if (s.accepted || self.config.record_unaccepted)
                && self.record_allowed(&s.key.app_id, &s.key.domain)
            {
                self.queue_session();
            }
        }
        self.session = None;
    }
    fn flush_records(&mut self) -> Result<()> {
        let mut failed = false;
        while let Some(record) = self.records.pop_front() {
            let result = match record {
                Record::History {
                    id,
                    app,
                    domain,
                    text,
                    spans,
                } if self.record_allowed(&app, &domain) => {
                    locked(&self.storage).record(&id, &app, &domain, &text, &spans)
                }
                Record::Stats {
                    app,
                    text,
                    completion,
                } if self.config.statistics_enabled => {
                    locked(&self.storage).acceptance(&app, &text, completion)
                }
                _ => Ok(()),
            };
            failed |= result.is_err();
        }
        if failed {
            self.error = "Local history or statistics could not be saved.".into();
            Err(anyhow!(self.error.clone()))
        } else {
            Ok(())
        }
    }
    fn apply_config(&mut self, mut c: Config, persist: bool) -> Result<()> {
        c.validate()?;
        if recording_configured(&c) {
            locked(&self.storage).unlock()?;
        }
        if persist {
            self.readers.persist(&c)?;
        }
        self.finish_session();
        let release = !c.global_enabled
            || self.config.provider_identity() != c.provider_identity()
            || privacy_revoked(&self.config, &c);
        self.config = c;
        self.clear(true, true);
        self.candidate = None;
        self.attempted = None;
        self.failed_correction = None;
        self.capture_after = self.now();
        self.resident = None;
        self.worker
            .set_idle_timeout(self.config.idle_release_seconds);
        self.configure_native();
        if !self.native_ready {
            let _ = self.retry_native();
        }
        self.set_native_shortcuts(self.native_ready && !self.shortcuts_suspended && !self.stopping);
        if release {
            self.release_model();
        }
        if !recording_configured(&self.config) {
            self.session = None;
            locked(&self.storage).lock();
        }
        if !(self.config.typo_suppression || self.config.typo_fixes) {
            if let Some(service) = self.spelling.take() {
                self.spawn_spelling_cleanup(service);
            }
        } else if self.spelling.is_none() {
            self.spelling = Some(self.spelling_factory.as_ref().map_or_else(
                || Arc::new(SpellingService::new(None, None)) as Arc<dyn Spelling>,
                |factory| factory(),
            ));
        }
        self.emit(json!({"type":"config","config":self.config}));
        Ok(())
    }
    fn toggle_app(&mut self, app: &str, host: &str) -> bool {
        if app.is_empty() || excluded(&self.config, app, "") {
            return false;
        }
        let host = domain(host);
        let scope = if host.is_empty() {
            format!("app:{app}")
        } else {
            format!("domain:{host}")
        };
        let desired = !self.allowed(app, &host, "");
        if self.escape_app == app {
            self.escape_app.clear();
        }
        self.temporary.remove(&scope);
        if self.allowed(app, &host, "") != desired {
            self.temporary
                .insert(scope, (desired, self.now() + Duration::from_secs(300)));
        }
        self.finish_session();
        self.clear(true, true);
        self.candidate = None;
        self.attempted = None;
        self.configure_native();
        true
    }
    pub fn command(&mut self, command: Command) {
        if self.stopping {
            return;
        }
        let result: Result<()> = match command {
            Command::Action(action) => {
                let handled = self.action(action.clone(), None, None);
                self.emit(json!({"type":"action_result","action":action,"handled":handled}));
                Ok(())
            }
            Command::ApplyConfig { config, persist } => self.apply_config(config, persist),
            Command::PresentationResult {
                key,
                authority,
                revision,
                visible_text,
                mode,
            } => {
                self.presentation(key, authority, revision, visible_text, mode);
                Ok(())
            }
            Command::Pause { minutes } => {
                self.finish_session();
                let _ = self.flush_records();
                self.paused_until = self.now() + Duration::from_secs(u64::from(minutes) * 60);
                self.timed_pause = true;
                self.clear(true, true);
                self.candidate = None;
                self.release_model();
                self.configure_native();
                self.emit(json!({"type":"pause","minutes":minutes}));
                Ok(())
            }
            Command::Resume => {
                self.paused_until = self.now();
                self.timed_pause = false;
                self.attempted = None;
                let result = if !self.config.global_enabled {
                    let mut c = self.config.clone();
                    c.global_enabled = true;
                    self.apply_config(c, true)
                } else {
                    self.configure_native();
                    Ok(())
                };
                if result.is_ok() {
                    self.emit(json!({"type":"resume"}));
                }
                result
            }
            Command::ToggleApp { app, domain } => {
                let changed = self.toggle_app(&app, &domain);
                self.emit(json!({"type":"app_toggle","changed":changed,"app":app,"domain":domain}));
                Ok(())
            }
            Command::SetModel {
                path,
                prompt_style,
                model_id,
            } => {
                let mut c = self.config.clone();
                c.backend = "gguf".into();
                c.model_path = path;
                c.prompt_style = prompt_style;
                let selected = crate::paths::expand_user(std::ffi::OsStr::new(&c.model_path)).ok();
                c.model_id = model_id.unwrap_or_else(|| {
                    models::presets()
                        .iter()
                        .find(|p| {
                            selected.as_ref().is_some_and(|selected| {
                                models::model_path(p).ok().as_ref() == Some(selected)
                            })
                        })
                        .map_or(String::new(), |p| p.id.clone())
                });
                self.apply_config(c, true)
            }
            Command::SetHistoryEnabled { enabled } => {
                let mut c = self.config.clone();
                c.record_history = enabled;
                self.apply_config(c, true)
            }
            Command::DownloadModel { id, accept_license } => {
                self.download_model(&id, accept_license)
            }
            Command::CancelDownload => {
                if let Some(d) = &self.download {
                    d.cancel.set();
                }
                Ok(())
            }
            Command::Models => match model_inventory() {
                Ok(v) => {
                    self.emit(json!({"type":"models","models":v}));
                    Ok(())
                }
                Err(e) => Err(e),
            },
            Command::HistoryInfo => {
                let value = locked(&self.storage).info();
                value.map(|value| self.emit(json!({"type":"history_info","result":value})))
            }
            Command::HistoryCounts { unlock } => {
                let _ = self.flush_records();
                let value = locked(&self.storage).counts(unlock);
                value.map(|value| self.emit(json!({"type":"history_counts","result":value})))
            }
            Command::Statistics { period, group_by } => {
                let _ = self.flush_records();
                let value = locked(&self.storage).statistics(&period, &group_by);
                value.map(|value| self.emit(json!({"type":"statistics","period":period,"group_by":group_by,"result":value})))
            }
            Command::ClearStatistics => {
                self.records.retain(|r| !matches!(r, Record::Stats { .. }));
                let result = locked(&self.storage).clear_statistics();
                result.map(|_| self.emit(json!({"type":"statistics_cleared"})))
            }
            Command::DeleteHistory { app, domain } => {
                self.delete_history(app.as_deref(), domain.as_deref())
            }
            Command::EnableDocs { token } => {
                self.enable_docs(&token);
                Ok(())
            }
            Command::SuspendForPreview => {
                self.finish_session();
                let _ = self.flush_records();
                self.preview = true;
                self.clear(true, true);
                self.candidate = None;
                self.release_model();
                self.configure_native();
                Ok(())
            }
            Command::ResumeFromPreview => {
                self.preview = false;
                self.capture_after = self.now();
                self.configure_native();
                Ok(())
            }
            Command::SuspendShortcuts { suspended } => {
                self.suspend_shortcuts(suspended);
                Ok(())
            }
            Command::RetryNative => self.retry_native(),
        };
        if result.is_err() {
            self.fail("Requested operation");
        }
        self.refresh_arming();
    }
    fn delete_history(&mut self, app: Option<&str>, host: Option<&str>) -> Result<()> {
        ensure!(
            app.is_none() || host.is_none(),
            "Delete one app or one website at a time."
        );
        let host = host.map(domain);
        let matches = |a: &str, d: &str| {
            app.is_none() && host.is_none() || app == Some(a) || host.as_deref() == Some(d)
        };
        self.records
            .retain(|r| !matches!(r, Record::History { app, domain, .. } if matches(app, domain)));
        if self
            .session
            .as_ref()
            .is_some_and(|s| matches(&s.key.app_id, &s.key.domain))
        {
            self.session = None;
        }
        if let Some(s) = &self.candidate {
            if matches(&s.app_id, &s.domain) {
                self.history_suppressed = Some((s.authority(), s.text.to_string()));
            }
        }
        self.clear(true, true);
        self.candidate = None;
        self.attempted = None;
        self.release_model();
        let result =
            locked(&self.storage).delete(app, host.as_deref(), app.is_some() || host.is_some());
        let count = result?;
        self.emit(json!({"type":"history_deleted","count":count,"app":app,"domain":host}));
        Ok(())
    }
    fn download_model(&mut self, id: &str, accept_license: bool) -> Result<()> {
        models::get_preset(id)?;
        ensure!(
            self.download
                .as_ref()
                .is_none_or(|d| d.thread.is_finished()),
            "Cancel the current model download before starting another."
        );
        if let Some(d) = self.download.take() {
            let _ = d.thread.join();
        }
        let preset = models::get_preset(id)?.clone();
        let id = id.to_owned();
        let cancel = models::DownloadCancellation::new();
        let token = cancel.clone();
        let (sender, receiver) = mpsc::sync_channel(128);
        let initial = json!({"type":"download","id":id,"received":0,"total":preset.size_bytes,"state":"downloading","error":""});
        self.download_status = Some(initial.clone());
        self.emit(initial);
        let thread = thread::Builder::new().name("brotypist-model-download".into()).spawn(move || {
            let progress_sender = sender.clone();
            let result = models::download_model(&id, accept_license, &token, &mut |received,total| { let _ = progress_sender.try_send(json!({"type":"download","id":id,"received":received,"total":total,"state":"downloading","error":""})); });
            let event = match result {
                Ok(path) if !token.is_set() => json!({"type":"download","id":id,"received":preset.size_bytes,"total":preset.size_bytes,"state":"complete","path":path,"prompt_style":preset.prompt_style,"error":""}),
                _ => json!({"type":"download","id":id,"state":if token.is_set() {"cancelled"} else {"error"},"error":if token.is_set() {"Download cancelled."} else {"Model download failed; existing weights were preserved."}}),
            }; let _ = sender.send(event);
        })?;
        self.download = Some(Download {
            cancel,
            thread,
            receiver,
        });
        Ok(())
    }
    fn consume_download(&mut self) {
        let events = self
            .download
            .as_ref()
            .map(|d| d.receiver.try_iter().collect::<Vec<_>>())
            .unwrap_or_default();
        for event in events {
            self.download_status = Some(event.clone());
            if !self.stopping {
                self.emit(event);
            }
        }
        if self
            .download
            .as_ref()
            .is_some_and(|d| d.thread.is_finished())
        {
            if let Some(d) = self.download.take() {
                let _ = d.thread.join();
            }
        }
    }
    fn start_model_migration(&mut self) {
        let needed = config::model_storage_migration_needed(&self.config);
        match needed {
            Ok(false) => {}
            Ok(true) => match ModelMigration::start(&self.config) {
                Ok(migration) => self.model_migration = Some(migration),
                Err(_) if self.error.is_empty() => self.error = MODEL_STORAGE_WARNING.into(),
                Err(_) => {}
            },
            Err(_) if self.error.is_empty() => self.error = MODEL_STORAGE_WARNING.into(),
            Err(_) => {}
        }
    }
    fn consume_model_migration(&mut self) {
        if !self
            .model_migration
            .as_ref()
            .and_then(|migration| migration.thread.as_ref())
            .is_some_and(JoinHandle::is_finished)
        {
            return;
        }
        let mut migration = self
            .model_migration
            .take()
            .expect("finished model migration");
        let outcome = migration
            .thread
            .take()
            .expect("model migration thread")
            .join()
            .unwrap_or_else(|_| Err(anyhow!("Model storage worker stopped.")));
        if self.stopping {
            return;
        }
        match outcome {
            Ok(Some(path))
                if self.config.provider_identity() == migration.provider
                    && self.config.model_id == migration.model_id =>
            {
                // Only the migrated path is adopted. Consent and other settings
                // may have changed while the file was verified in the worker.
                let mut current = self.config.clone();
                current.model_path = path;
                if self.apply_config(current, true).is_err() && self.error.is_empty() {
                    self.error = MODEL_STORAGE_WARNING.into();
                }
            }
            Err(_) if self.error.is_empty() => self.error = MODEL_STORAGE_WARNING.into(),
            _ => {}
        }
    }

    fn refresh_docs(&mut self, info: Option<platform::Foreground>) {
        let Some(revision) = self.native_input_revision() else {
            self.clear_docs();
            return;
        };
        let eligible = info
            .as_ref()
            .is_some_and(|_| self.active() && self.secure_input() == Some(false));
        if self.docs.as_ref().is_some_and(|(_, old, rev)| {
            !eligible
                || info.as_ref().is_none_or(|i| {
                    i.app_id != old.app_id
                        || i.domain != old.domain
                        || i.window_id != old.window_id
                        || i.window_pid != old.window_pid
                        || i.document_id != old.document_id
                })
                || *rev != revision
        }) {
            self.clear_docs();
            self.docs_after = self.docs_after.max(self.now() + Duration::from_secs(2));
        }
        if !eligible || self.candidate.is_some() {
            self.clear_docs();
            return;
        }
        if self.now() < self.docs_after {
            return;
        }
        self.docs_after = self.now() + Duration::from_secs(2);
        let Some(before) = info else {
            return;
        };
        let hint = self
            .bridge
            .as_mut()
            .and_then(|bridge| bridge.docs_hint().ok().flatten());
        let after = self.foreground();
        if self.native_input_revision() != Some(revision)
            || self.secure_input() != Some(false)
            || after.as_ref().is_none_or(|i| {
                i.app_id != before.app_id
                    || i.domain != before.domain
                    || i.window_id != before.window_id
                    || i.window_pid != before.window_pid
                    || i.document_id != before.document_id
            })
        {
            self.clear_docs();
            return;
        }
        if let Some(hint) = hint.filter(|h| {
            h.get("token")
                .and_then(Value::as_str)
                .is_some_and(|t| !t.is_empty())
                && h.get("support").and_then(Value::as_str) == Some("unknown")
                && h.get("app_id").and_then(Value::as_str) == Some(before.app_id.as_str())
        }) {
            if self.docs.as_ref().is_none_or(|(old, _, _)| *old != hint) {
                self.emit(json!({"type":"docs_help","hint":hint}));
            }
            self.docs = Some((hint, before, revision));
        } else {
            self.clear_docs();
        }
    }
    fn enable_docs(&mut self, token: &str) {
        let info = self.foreground();
        let mut result = json!({"status":"unknown","diagnostic":"stale-capability","help_url":""});
        if let Some((hint, old, revision)) = self.docs.take() {
            self.emit(json!({"type":"docs_help","hint":null}));
            if self.active()
                && self.secure_input() == Some(false)
                && self.native_input_revision() == Some(revision)
                && hint.get("token").and_then(Value::as_str) == Some(token)
                && info.as_ref().is_some_and(|i| {
                    i.app_id == old.app_id
                        && i.domain == old.domain
                        && i.window_id == old.window_id
                        && i.window_pid == old.window_pid
                        && i.document_id == old.document_id
                })
            {
                if let Some(Ok(value)) = self
                    .bridge
                    .as_mut()
                    .map(|bridge| bridge.enable_docs_accessibility(token))
                {
                    if value
                        .get("status")
                        .and_then(Value::as_str)
                        .is_some_and(|s| ["enabled", "already_enabled", "unknown"].contains(&s))
                    {
                        result = value;
                    }
                }
            }
        }
        self.capture_after = self.now();
        self.emit(json!({"type":"docs_result","result":result}));
    }
    #[cfg(any(target_os = "macos", test))]
    fn consume_ready(&mut self) {
        if self.stopping || self.shortcuts_suspended {
            return;
        }
        self.consume_host_events();
        self.run_host_actions();
        self.run_inputs();
        if self
            .bridge
            .as_ref()
            .is_some_and(|bridge| bridge.native_edit_pending())
        {
            return;
        }
        self.advance_preparation();
        self.consume_results();
        self.consume_context_updates();
        self.refresh_arming();
        self.whatsapp_state();
    }
    pub fn tick(&mut self) {
        if self
            .bridge
            .as_mut()
            .is_some_and(|bridge| bridge.pump_events().is_err())
        {
            self.fail("Native event transport");
        }
        self.consume_host_events();
        self.consume_download();
        self.consume_model_migration();
        self.reap_spelling();
        if self.stopping {
            let _ = self.shutdown_step();
            return;
        }
        self.run_host_actions();
        self.run_inputs();
        let now = self.now();
        if self.timed_pause && now >= self.paused_until {
            self.timed_pause = false;
            self.attempted = None;
            self.configure_native();
        }
        if now >= self.power_after {
            self.power_after = now + Duration::from_secs(5);
            let power = self.readers.power();
            self.on_battery = power.available && power.has_battery && power.on_battery;
        }
        let previous = self.temporary.len();
        self.temporary.retain(|_, (_, at)| now < *at);
        if previous != self.temporary.len() {
            self.clear(true, true);
            self.candidate = None;
            self.attempted = None;
            self.configure_native();
        }
        if let Some(revision) = self.native_input_revision() {
            let input_pending = self.input_pending.swap(false, Ordering::AcqRel);
            if revision != self.input_revision || input_pending {
                // A non-user native notification revokes the old claim, but
                // an unchanged permitted recapture may request a fresh paint
                // acknowledgement for a completed offer. Never carry it across
                // an intervening user-input revision.
                self.redraw_pending |= self.request.is_none()
                    && self.preparing.is_none()
                    && !self.suggestion.text.is_empty()
                    && self
                        .completion_offer
                        .is_some_and(|offer| Some(offer.revision) == self.user_revision());
                self.input_revision = revision;
                self.capture_after = now;
                self.disarm();
            }
        }
        let foreground = self.foreground();
        if self.active()
            && self
                .bridge
                .as_ref()
                .is_some_and(|bridge| bridge.native_edit_pending())
        {
            let _ = self.flush_records();
            self.refresh_arming();
            return;
        }
        if !self.active() || foreground.is_none() || self.secure_input() != Some(false) {
            self.observe(None);
            self.clear_docs();
        } else if now >= self.capture_after {
            let snapshot = self.capture();
            if !self
                .bridge
                .as_ref()
                .is_some_and(|bridge| bridge.native_edit_pending())
            {
                self.observe(snapshot);
            }
            // Slow native reads must not leave the next routine deadline
            // expired; input notifications and explicit actions still recapture.
            let captured_at = self.now();
            self.capture_after = captured_at
                + if self.request.is_some()
                    || self.preparing.is_some()
                    || captured_at.duration_since(self.candidate_since) < Duration::from_secs(2)
                {
                    Duration::from_millis(50)
                } else {
                    Duration::from_millis(100)
                };
        }
        if self
            .bridge
            .as_ref()
            .is_some_and(|bridge| bridge.native_edit_pending())
        {
            let _ = self.flush_records();
            self.refresh_arming();
            return;
        }
        self.refresh_docs(foreground);
        self.prepare_models();
        self.advance_preparation();
        self.consume_results();
        self.renew_native_presentations();
        self.refresh_context();
        if let Some(s) = self.candidate.clone() {
            self.schedule(&s);
        }
        let _ = self.flush_records();
        self.refresh_arming();
        self.whatsapp_state();
    }
    pub fn status(&self) -> Value {
        let mut status = self.worker.status();
        if !status.is_object() {
            status = json!({});
        }
        if !self.native_ready {
            status["state"] = json!("error");
            status["message"] = json!(if self.native_unavailable_reason.is_empty() {
                "Native input is unavailable. Check permissions and keyboard settings, then retry."
            } else {
                &self.native_unavailable_reason
            });
        } else if !self.active() {
            status["state"] = json!("paused");
            status["message"] = json!(if self.config.global_enabled {
                "Typing suggestions are paused."
            } else {
                "Typing suggestions are disabled."
            });
        } else if !self.error.is_empty() {
            status["state"] = json!("error");
            status["message"] = json!(self.error);
        }
        let temporary = [format!("domain:{}", self.host), format!("app:{}", self.app)]
            .into_iter()
            .find_map(|scope| {
                self.temporary
                    .get(&scope)
                    .filter(|(_, at)| self.now() < *at)
                    .map(|(_, at)| {
                        (
                            scope,
                            at.saturating_duration_since(self.now()).as_secs_f64(),
                        )
                    })
            });
        let fields = json!({"enabled":self.active(),"inline_available":self.candidate.as_ref().is_some_and(Snapshot::inline_safe),
            "inline_message":if self.candidate.as_ref().is_some_and(Snapshot::inline_safe) {"Verified same-line completion is available."} else {"This field does not expose verified same-line rendering; ordinary completion is silent."},
            "context_status":self.context_status,"context_message":context_message(&self.context_status),"current_app_enabled":self.allowed(&self.app,&self.host,""),
            "current_recording":self.record_allowed(&self.app,&self.host),"current_tab_enabled":rule_bool(&self.config,&self.app,&self.host,"tab_enabled",true),
            "secure_input":self.secure_input() != Some(false),"temporary_scope":temporary.as_ref().map(|(scope,_)|scope.as_str()).unwrap_or(""),
            "temporary_remaining":temporary.map(|(_,seconds)|seconds),"escape_suppressed":!self.escape_app.is_empty() && self.escape_app == self.app,
            "native_status":self.bridge.as_ref().map(|bridge| bridge.status()).unwrap_or_else(||json!({"state":"unavailable","available":false,"message":self.native_unavailable_reason})),
            "native_available":self.bridge.is_some(),"native_unavailable_reason":self.native_unavailable_reason,
            "current_app":self.app,"current_domain":self.host,"paused_remaining":self.paused_until.saturating_duration_since(self.now()).as_secs_f64(),
            "on_battery":self.on_battery,"download":self.download_status,"preview":self.preview,
            "native_started":self.native_ready,"native_hooks_started":self.hooks_started,"preview_suspended":self.preview && status.get("release_complete").and_then(Value::as_bool) == Some(true)
                && status.get("worker_pid").is_none_or(Value::is_null) && status.get("terminating").and_then(Value::as_bool) != Some(true),
            "shutdown_complete":self.done.contains("complete")});
        status
            .as_object_mut()
            .expect("status object")
            .extend(fields.as_object().expect("fields").clone());
        status
    }
    fn suspend_shortcuts(&mut self, suspended: bool) {
        self.shortcuts_suspended = suspended;
        self.disarm();
        if suspended {
            self.set_native_shortcuts(false);
        } else {
            self.configure_native();
            self.set_native_shortcuts(self.native_ready && !self.stopping);
            self.redraw_pending = self.native_ready;
            self.capture_after = self.now();
        }
        self.refresh_arming();
    }
    pub fn begin_shutdown(&mut self) {
        if self.stopping {
            return;
        }
        self.finish_session();
        self.stopping = true;
        self.bump_epoch();
        if let Some(migration) = &self.model_migration {
            migration.cancel.set();
        }
        self.preparer.request_stop();
        if let Some(bridge) = &mut self.bridge {
            bridge.set_input_callback(None);
            bridge.bind_native_ui(None);
            bridge.set_shortcuts_enabled(false);
        }
        self.native_ui_callback = None;
        self.clear(true, true);
        self.candidate = None;
        self.pending_edit = None;
        while let Ok(input) = self.inputs.try_recv() {
            if input.transaction || input.action == Action::Bypass {
                let _ = self.finish_input(false);
            }
        }
        if let Some(d) = &self.download {
            d.cancel.set();
        }
        if let Some(s) = self.spelling.take() {
            self.spawn_spelling_cleanup(s);
        }
    }
    fn spawn_spelling_cleanup(&mut self, service: Arc<dyn Spelling>) {
        // Retain the service on spawn failure; no drop-and-forget pending cleanup.
        let holder = Arc::new(Mutex::new(Some(service)));
        let owned = holder.clone();
        match thread::Builder::new()
            .name("brotypist-spelling-cleanup".into())
            .spawn(move || {
                let service = locked(&owned).take().expect("spelling cleanup owner");
                while !service.close() || !service.is_stopped() {
                    thread::sleep(Duration::from_millis(10));
                }
            }) {
            Ok(thread) => self.spelling_cleanup.push(thread),
            Err(_) => {
                self.spelling = locked(&holder).take();
                self.error = "Native spelling cleanup is pending.".into();
            }
        }
    }
    fn reap_spelling(&mut self) {
        let mut i = 0;
        while i < self.spelling_cleanup.len() {
            if self.spelling_cleanup[i].is_finished() {
                let t = self.spelling_cleanup.swap_remove(i);
                let _ = t.join();
            } else {
                i += 1;
            }
        }
    }
    pub fn shutdown_step(&mut self) -> Result<bool> {
        self.begin_shutdown();
        let mut failed = false;
        if !self.done.contains("bridge") {
            match self.bridge.as_mut().map_or(Ok(()), |bridge| bridge.stop()) {
                Ok(()) => {
                    self.done.insert("bridge");
                }
                Err(_) => failed = true,
            }
        }
        if !self.done.contains("worker_signal") {
            match self.worker.request_stop() {
                Ok(()) => {
                    self.done.insert("worker_signal");
                }
                Err(_) => failed = true,
            }
        }
        if self.worker.is_stopped() {
            self.done.insert("worker");
        }
        self.preparer.request_stop();
        if self.preparer.is_stopped() {
            self.done.insert("preparer");
        }
        if let Some(s) = self.spelling.take() {
            self.spawn_spelling_cleanup(s);
        }
        self.reap_spelling();
        if self.spelling.is_none() && self.spelling_cleanup.is_empty() {
            self.done.insert("spelling");
        }
        self.consume_download();
        if self.download.is_none() {
            self.done.insert("download");
        }
        self.consume_model_migration();
        if self.model_migration.is_none() {
            self.done.insert("model_migration");
        }
        if self.done.contains("preparer") && !self.done.contains("records") {
            if self.flush_records().is_ok() {
                self.done.insert("records");
            } else {
                failed = true;
            }
        }
        if self.done.contains("preparer") && !self.done.contains("history") {
            locked(&self.storage).lock();
            self.done.insert("history");
        }
        let complete = [
            "bridge",
            "worker",
            "preparer",
            "spelling",
            "download",
            "model_migration",
            "records",
            "history",
        ]
        .iter()
        .all(|name| self.done.contains(name));
        if complete {
            self.done.insert("complete");
        }
        if failed {
            self.error =
                "Shutdown is incomplete. Try Quit again to finish releasing local resources."
                    .into();
            Err(anyhow!(self.error.clone()))
        } else {
            Ok(complete)
        }
    }
}
impl Engine {
    fn retry_native(&mut self) -> Result<()> {
        self.native_ready = false;
        self.set_native_shortcuts(false);
        self.clear(true, true);
        self.candidate = None;
        self.attempted = None;
        let config = self.native_config();
        if self.bridge.is_none() {
            match (self.native_factory)(&config) {
                Ok(bridge) => {
                    self.bridge = Some(bridge);
                    self.hooks_started = false;
                }
                Err(error) => {
                    self.native_unavailable_reason = error.to_string();
                    self.emit(json!({"type":"native_started","started":false,"reason":self.native_unavailable_reason}));
                    return Err(anyhow!("Native input is unavailable."));
                }
            }
        }
        let Some(bridge) = &mut self.bridge else {
            return Err(anyhow!("Native input is unavailable."));
        };
        bridge.set_input_callback(Some(self.input_callback.clone()));
        bridge.bind_native_ui(self.native_ui_callback.clone());
        if bridge.configure(&config).is_err() {
            self.native_unavailable_reason = "Native configuration could not be applied.".into();
            self.fail("Native configuration");
            return Err(anyhow!("Native configuration could not be applied."));
        }
        if !self.hooks_started {
            self.hooks_started = bridge
                .start_shortcuts(self.shortcut_callback.clone())
                .is_ok();
        }
        self.native_ready = self.hooks_started;
        self.set_native_shortcuts(self.native_ready && !self.shortcuts_suspended && !self.stopping);
        self.capture_after = self.now();
        if self.native_ready {
            self.native_unavailable_reason.clear();
            self.error.clear();
        } else {
            self.native_unavailable_reason =
                "Native shortcuts could not start. Check permissions, then retry.".into();
        }
        self.emit(json!({"type":"native_started","started":self.native_ready,"reason":self.native_unavailable_reason}));
        if self.native_ready {
            Ok(())
        } else {
            Err(anyhow!("Native shortcuts could not start."))
        }
    }
}
impl Engine {
    fn owns_claim(&self, claim: &Claim) -> bool {
        match claim.mode.as_str() {
            "completion" => {
                self.suggestion
                    .snapshot
                    .as_ref()
                    .is_some_and(|s| s.key() == claim.key && s.authority() == claim.authority)
                    && !self.suggestion.text.is_empty()
                    && self.suggestion.text.starts_with(&claim.visible)
            }
            "correction" => self.correction.as_ref().is_some_and(|(s, result)| {
                s.key() == claim.key
                    && s.authority() == claim.authority
                    && result.replacement == claim.visible
            }),
            "alternatives" | "emoji" | "synonyms" => self.choices.as_ref().is_some_and(|c| {
                c.snapshot.key() == claim.key
                    && c.snapshot.authority() == claim.authority
                    && c.mode == claim.mode
            }),
            _ => false,
        }
    }
}
impl Engine {
    fn consume_context_updates(&mut self) {
        for update in self.preparer.context_updates() {
            if self.active()
                && update.epoch == self.current_epoch()
                && self
                    .candidate
                    .as_ref()
                    .is_some_and(|s| s.key() == update.key && s.authority() == update.authority)
            {
                self.context_status = if context_status(&update.status) {
                    update.status
                } else {
                    "error".into()
                };
                self.context_prefetched =
                    Some((update.key, update.authority, update.epoch, self.now()));
            }
        }
    }
    fn refresh_context(&mut self) {
        self.consume_context_updates();
        if !self.active()
            || !(self.config.screen_context || self.config.screenshot_context)
            || self.preparing.is_some()
            || self.request.is_some()
        {
            return;
        }
        let Some(s) = self.candidate.clone() else {
            return;
        };
        if s.protected
            || s.selection_length != 0
            || s.prefix().trim().is_empty()
            || s.window_id.is_empty()
            || s.window_revision.is_empty()
        {
            return;
        }
        if self
            .context_prefetched
            .as_ref()
            .is_some_and(|(key, authority, epoch, at)| {
                *key == s.key()
                    && *authority == s.authority()
                    && *epoch == self.current_epoch()
                    && self.now().saturating_duration_since(*at) <= Duration::from_secs(2)
            })
        {
            return;
        }
        self.context_prefetched = Some((s.key(), s.authority(), self.current_epoch(), self.now()));
        let ticket = self.bridge.as_mut().and_then(|bridge| {
            self.readers
                .context_ticket(&s, &mut **bridge)
                .ok()
                .flatten()
        });
        if let Some(ticket) = ticket {
            self.serial = self
                .serial
                .checked_add(1)
                .expect("context identifier exhausted");
            self.context_status = "pending".into();
            self.preparer.prefetch(ContextPreparation {
                token: self.serial,
                epoch: self.current_epoch(),
                snapshot: s,
                config: self.native_config(),
                ticket,
            });
        } else {
            self.context_status = "unavailable".into();
        }
    }
}
fn native_spelling() -> Arc<dyn Spelling> {
    #[cfg(target_os = "macos")]
    let backend = crate::platform::macos::spelling::spelling_backend();
    #[cfg(not(target_os = "macos"))]
    let backend = None;
    Arc::new(SpellingService::new(backend, None))
}
fn valid_rect(r: &core::Rect) -> bool {
    r.iter().all(|v| v.is_finite()) && r[2] > 0.0 && r[3] > 0.0
}
fn authority_matches(a: &Authority, s: &Snapshot) -> bool {
    a.app_id == s.app_id
        && a.field_id == s.field_id
        && a.domain == s.domain
        && a.window_id == s.window_id
        && a.window_revision == s.window_revision
        && a.window_pid == s.window_pid
}
fn selected_word(s: &Snapshot) -> String {
    // Validation derives NFC only; the original selected spelling is inserted.
    if s.protected
        || !s.can_replace
        || !s.valid_range()
        || s.selection_length == 0
        || s.selection_length > 80
    {
        return String::new();
    }
    let value = s.selected_text();
    let normalized: String = value.nfc().collect();
    for (index, word) in normalized.split(' ').enumerate() {
        if index >= 5 || word.is_empty() {
            return String::new();
        }
        let mut needs_letter = true;
        for c in word.chars() {
            if core::is_alphanumeric(c)
                && unicode_general_category::get_general_category(c)
                    != unicode_general_category::GeneralCategory::DecimalNumber
            {
                needs_letter = false;
            } else if !needs_letter && "’'-".contains(c) {
                needs_letter = true;
            } else {
                return String::new();
            }
        }
        if needs_letter {
            return String::new();
        }
    }
    value.into()
}
fn edit_snapshot(
    before: &Snapshot,
    start: usize,
    length: usize,
    insertion: &str,
) -> Option<Snapshot> {
    let end = start.checked_add(length)?;
    let tail = before.text.scalar_len().checked_sub(end)?;
    let prefix = before.text.slice(0, start)?;
    let suffix = before.text.slice(end, tail)?;
    let mut text = String::with_capacity(prefix.len() + insertion.len() + suffix.len());
    text.push_str(prefix);
    text.push_str(insertion);
    text.push_str(suffix);
    let mut expected = before.clone();
    expected.text = text.into();
    expected.caret = start + insertion.chars().count();
    expected.selection_length = 0;
    Some(expected)
}
fn confirmed_edit(edit: &PendingEdit, readback: &Snapshot) -> bool {
    !readback.protected
        && readback.valid_range()
        && readback.same_authority(&edit.before)
        && readback.text == edit.expected.text
        && readback.caret == edit.expected.caret
        && readback.selection_length == 0
        && readback.can_replace == edit.before.can_replace
        && readback.on_demand_only == edit.before.on_demand_only
}
fn update_authored(session: &mut Session, text: &str, user_authored: bool) {
    if session.text == text {
        return;
    }
    let old_len = session.authored.len();
    let new_len = text.chars().count();
    let start = session
        .text
        .chars()
        .zip(text.chars())
        .take_while(|(a, b)| a == b)
        .count();
    let tail = session
        .text
        .chars()
        .rev()
        .take(old_len - start)
        .zip(text.chars().rev().take(new_len - start))
        .take_while(|(a, b)| a == b)
        .count();
    let old_end = old_len - tail;
    let new_end = new_len - tail;
    let authored = user_authored && (old_end - start).max(new_end - start) <= 64;
    session.authored.splice(
        start..old_end,
        std::iter::repeat_n(authored, new_end - start),
    );
    session.text = text.into();
}
fn authored_spans(flags: &[bool]) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = None;
    for (index, flag) in flags
        .iter()
        .copied()
        .chain(std::iter::once(false))
        .enumerate()
    {
        if flag && start.is_none() {
            start = Some(index);
        } else if !flag {
            if let Some(first) = start.take() {
                spans.push((first, index));
                if spans.len() > 1024 {
                    return Vec::new();
                }
            }
        }
    }
    spans
}
fn privacy_revoked(old: &Config, new: &Config) -> bool {
    [
        (old.global_enabled, new.global_enabled),
        (old.enabled, new.enabled),
        (old.clipboard_context, new.clipboard_context),
        (old.screen_context, new.screen_context),
        (old.screenshot_context, new.screenshot_context),
        (old.screenshot_appearance, new.screenshot_appearance),
        (old.record_history, new.record_history),
        (old.terminal_completion, new.terminal_completion),
    ]
    .into_iter()
    .any(|(a, b)| a && !b)
        || (old.personalization_strength != 0
            && new.personalization_strength == 0
            && recording_configured(old))
        || new
            .excluded_apps
            .iter()
            .any(|app| !old.excluded_apps.contains(app))
        || old
            .app_rules
            .keys()
            .chain(new.app_rules.keys())
            .any(|scope| {
                ["enabled", "record_history"].iter().any(|name| {
                    let default_old = if *name == "enabled" {
                        old.enabled
                    } else {
                        old.record_history
                    };
                    let default_new = if *name == "enabled" {
                        new.enabled
                    } else {
                        new.record_history
                    };
                    old.app_rules
                        .get(scope)
                        .and_then(|r| r.get(*name))
                        .and_then(Value::as_bool)
                        .unwrap_or(default_old)
                        && !new
                            .app_rules
                            .get(scope)
                            .and_then(|r| r.get(*name))
                            .and_then(Value::as_bool)
                            .unwrap_or(default_new)
                })
            })
}
pub fn model_inventory() -> Result<Vec<Value>> {
    models::presets().iter().map(|p| Ok(json!({"id":p.id,"label":p.label,"description":p.description,"path":models::model_path(p)?,"size_bytes":p.size_bytes,
        "installed":models::is_installed(p),"prompt_style":p.prompt_style,"license_name":p.license_name,"license_url":p.license_url,
        "requires_license_acceptance":p.requires_license_acceptance,"availability":models::model_availability(p,models::physical_memory_bytes())}))).collect()
}
fn context_status(s: &str) -> bool {
    [
        "off",
        "pending",
        "ready",
        "empty",
        "unavailable",
        "blocked",
        "stale",
        "error",
        "screen_permission",
    ]
    .contains(&s)
}
fn context_message(s: &str) -> &'static str {
    match s {
        "pending" => "Reading eligible text from the focused window.",
        "ready" => "Optional reference text is available.",
        "empty" => "No relevant optional reference text is available.",
        "unavailable" => "This field does not expose safe focused-window Accessibility context.",
        "blocked" => "Optional context was blocked by current focus or privacy policy.",
        "stale" => "Focused-window context changed and was discarded.",
        "error" => {
            "Optional context could not be read. Check Accessibility permissions or turn it off."
        }
        "screen_permission" => "Screen Recording permission is needed for screenshot context.",
        _ => "Optional context is off.",
    }
}
fn primary_claim_index(claims: &[Arc<Claim>], alternatives: bool) -> Option<usize> {
    for mode in ["correction", "completion"] {
        if let Some(index) = claims.iter().position(|claim| claim.mode == mode) {
            return Some(index);
        }
    }
    claims
        .iter()
        .position(|claim| alternatives || claim.mode != "alternatives")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::InlineRender;
    use crate::platform::{DetachedContextRead, Foreground, Permission, ShortcutHandler};

    #[test]
    fn owner_wake_consumes_prepared_ready_and_result_without_periodic_tick() {
        let mut h = harness(Config::default(), None);
        h.force();
        let pending = h.preparation.borrow_mut().pending.take().unwrap();
        h.preparation.borrow_mut().results.push(Prepared {
            token: pending.token,
            epoch: pending.epoch,
            options: GenerationOptions::default(),
            context_status: "off".into(),
            context_diagnostic: ContextPreparationDiagnostic::default(),
            warning: String::new(),
            reference_expires: None,
        });
        let deadlines = (h.engine.capture_after, h.engine.power_after);
        let model_preparations = h.worker.borrow().prepared;
        let wake = OwnerWake::new(|| {});
        let controller = Controller::with_engine_and_wake(h.engine, wake.clone()).unwrap();
        wake.notify();
        controller.poll_ready();
        assert_eq!(h.worker.borrow().submitted.len(), 1);
        assert!(h.worker.borrow().proceeds.is_empty());
        h.worker.borrow_mut().events.push(WorkerEvent {
            id: 1,
            kind: "ready".into(),
            text: String::new(),
            final_: false,
            model_loaded: true,
            message: String::new(),
        });
        wake.notify();
        controller.poll_ready();
        assert_eq!(h.worker.borrow().proceeds, vec![1]);
        h.worker.borrow_mut().events.push(WorkerEvent {
            id: 1,
            kind: "result".into(),
            text: " arrives tomorrow".into(),
            final_: true,
            model_loaded: true,
            message: String::new(),
        });
        wake.notify();
        let events = controller.poll_ready();
        assert!(
            events
                .iter()
                .any(|e| { e["type"] == "suggestion" && e["text"] == " arrives tomorrow" })
        );
        let engine = controller.engine.borrow();
        assert_eq!((engine.capture_after, engine.power_after), deadlines);
        assert_eq!(h.worker.borrow().prepared, model_preparations);
        drop(engine);
        let captures = h.native.borrow().capture_count;
        assert!(controller.poll_ready().is_empty());
        assert_eq!(h.native.borrow().capture_count, captures);
    }

    #[test]
    fn wake_ready_event_cannot_proceed_after_authority_changes() {
        let mut h = harness(Config::default(), None);
        h.force();
        h.prepared();
        h.native.borrow_mut().snapshot = None;
        h.worker.borrow_mut().events.push(WorkerEvent {
            id: 1,
            kind: "ready".into(),
            text: String::new(),
            final_: false,
            model_loaded: true,
            message: String::new(),
        });
        let wake = OwnerWake::new(|| {});
        let controller = Controller::with_engine_and_wake(h.engine, wake.clone()).unwrap();
        wake.notify();
        controller.poll_ready();
        assert!(h.worker.borrow().proceeds.is_empty());
        assert!(controller.engine.borrow().request.is_none());
    }

    #[test]
    fn command_only_presentation_flush_rechecks_authority_without_running_tick() {
        for invalidation in ["none", "focus", "menu", "shutdown"] {
            let mut h = harness(Config::default(), None);
            h.offer(" arrives tomorrow");
            let offer = h
                .engine
                .drain_events()
                .into_iter()
                .find(|e| e["type"] == "suggestion" && e["text"] == " arrives tomorrow")
                .unwrap();
            let deadline = h.engine.capture_after;
            let preparations = h.worker.borrow().prepared;
            let controller = Controller::with_engine(h.engine).unwrap();
            match invalidation {
                "focus" => h.native.borrow_mut().snapshot = None,
                "menu" => controller.suspend_shortcuts(true).unwrap(),
                "shutdown" => controller.engine.borrow_mut().begin_shutdown(),
                _ => {}
            }
            controller
                .send(Command::PresentationResult {
                    key: serde_json::from_value(offer["key"].clone()).unwrap(),
                    authority: serde_json::from_value(offer["authority"].clone()).unwrap(),
                    revision: offer["revision"].as_u64().unwrap(),
                    visible_text: offer["text"].as_str().unwrap().into(),
                    mode: "completion".into(),
                })
                .unwrap();
            controller.flush_commands();
            let engine = controller.engine.borrow();
            assert_eq!(
                !engine.claims.is_empty(),
                invalidation == "none",
                "{invalidation}"
            );
            if invalidation == "none" {
                assert_eq!(engine.capture_after, deadline);
            }
            assert_eq!(h.worker.borrow().prepared, preparations);
            assert!(h.native.borrow().edits.is_empty());
        }
    }

    #[test]
    fn background_preparation_notifies_after_actual_result_publication() {
        let stored = Arc::new(Mutex::new(Stored::default()));
        let storage: SharedStorage = Arc::new(Mutex::new(Box::new(Memory(stored))));
        let (signal, receive) = mpsc::sync_channel(1);
        let wake = OwnerWake::new(move || signal.try_send(()).unwrap());
        let mut service = AsyncPreparer::new_with_wake(
            storage,
            Arc::new(AtomicU64::new(1)),
            Arc::new(SyntheticSources),
            wake.clone(),
        )
        .unwrap();
        for token in [1, 2] {
            let mut request = preparation(field("The update"), token, 1, None);
            request.config.screen_context = false;
            service.submit(request);
            // A watchdog only: synchronization is the publication notification,
            // not a sleep or repeated polling of the result queue.
            receive.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(wake.take());
            let result = service.poll();
            assert_eq!(result.len(), 1);
            assert_eq!(result[0].token, token);
            assert!(service.poll().is_empty());
            service.cancel();
        }
        service.request_stop();
        service.submit(preparation(field("The update"), 3, 1, None));
        assert!(service.poll().is_empty());
        assert!(receive.try_recv().is_err());
    }

    #[derive(Clone, Copy, Default)]
    enum Outcome {
        #[default]
        Applied,
        AppliedNoGeometry,
        Dispatched,
        Rejected,
        Indeterminate,
        WrongReadback,
    }
    struct NativeState {
        snapshot: Option<Snapshot>,
        revision: u64,
        notification_revision: u64,
        input_at: Option<Instant>,
        secure: Option<bool>,
        hook: Option<ShortcutHandler>,
        wake: Option<Arc<dyn Fn() + Send + Sync>>,
        outcome: Outcome,
        edits: Vec<String>,
        finished: Vec<bool>,
        ui_callback: Option<Arc<dyn Fn(String) + Send + Sync>>,
        capture_count: usize,
        capture_time: Option<(Rc<RefCell<Instant>>, Duration)>,
        stop_calls: usize,
        stop_failures: usize,
        shortcuts: bool,
        start_failures: usize,
        start_calls: usize,
        configure_failures: usize,
        docs: Option<Value>,
        docs_reads: usize,
        metadata: Option<Foreground>,
        enables: usize,
        context: Option<ContextTicket>,
        configured: Config,
        native_available: bool,
        native_clock: Option<Rc<RefCell<Instant>>>,
        native_preedit: Option<platform::NativePreedit>,
        native_choices: Option<platform::NativeChoices>,
        native_choice_limit: usize,
        client_owned: bool,
        client_pending: bool,
        client_arm: bool,
        host_events: Vec<platform::NativeHostEvent>,
    }
    struct Native(Rc<RefCell<NativeState>>);
    impl NativeBridge for Native {
        fn configure(&mut self, c: &Config) -> Result<()> {
            let mut s = self.0.borrow_mut();
            if s.configure_failures > 0 {
                s.configure_failures -= 1;
                return Err(anyhow!("private native configuration"));
            }
            s.configured = c.clone();
            Ok(())
        }
        fn capture(&mut self) -> Result<Option<Snapshot>> {
            let mut s = self.0.borrow_mut();
            s.capture_count += 1;
            if let Some((clock, duration)) = &s.capture_time {
                *clock.borrow_mut() += *duration;
            }
            Ok(if s.client_pending {
                None
            } else {
                s.snapshot.clone()
            })
        }
        fn context_ticket(
            &mut self,
            snapshot: &Snapshot,
        ) -> Result<Option<Box<dyn platform::DetachedContextRead>>> {
            Ok(self
                .0
                .borrow()
                .context
                .as_ref()
                .filter(|ticket| ticket.matches_snapshot(snapshot))
                .cloned()
                .map(|ticket| Box::new(ticket) as Box<dyn platform::DetachedContextRead>))
        }
        fn foreground(&mut self) -> Result<Option<Foreground>> {
            let s = self.0.borrow();
            Ok(s.metadata.clone().or_else(|| {
                s.snapshot.as_ref().map(|s| Foreground {
                    app_id: s.app_id.clone(),
                    domain: s.domain.clone(),
                    window_id: s.window_id.clone(),
                    window_pid: s.window_pid,
                    ..Default::default()
                })
            }))
        }
        fn insert(&mut self, s: &Snapshot, text: &str) -> Result<EditOutcome> {
            self.replace_range(s, s.caret, 0, text)
        }
        fn replace_range(
            &mut self,
            before: &Snapshot,
            start: usize,
            length: usize,
            text: &str,
        ) -> Result<EditOutcome> {
            let mut s = self.0.borrow_mut();
            s.edits.push(text.into());
            let expected = edit_snapshot(before, start, length, text).unwrap();
            Ok(match s.outcome {
                Outcome::Applied => {
                    s.snapshot = Some(expected.clone());
                    EditOutcome::Applied(expected)
                }
                Outcome::AppliedNoGeometry => {
                    let mut readback = expected;
                    readback.caret_rect = None;
                    readback.inline_render = None;
                    readback.native_inline = false;
                    readback.native_anchor.clear();
                    s.snapshot = Some(readback.clone());
                    EditOutcome::Applied(readback)
                }
                Outcome::Dispatched => EditOutcome::Dispatched,
                Outcome::Rejected => EditOutcome::Rejected,
                Outcome::Indeterminate => EditOutcome::Indeterminate,
                Outcome::WrongReadback => EditOutcome::Applied(before.clone()),
            })
        }
        fn range_rect(&mut self, _: &Snapshot, _: usize, _: usize) -> Result<Option<core::Rect>> {
            Ok(Some([10., 20., 40., 18.]))
        }
        fn focused_context(
            &mut self,
            _: &Snapshot,
            _: &dyn Fn() -> bool,
            _: usize,
        ) -> Result<ContextRead> {
            Ok(ContextRead::blocked("accessibility"))
        }
        fn start_shortcuts(&mut self, callback: ShortcutHandler) -> Result<()> {
            let mut s = self.0.borrow_mut();
            s.hook = Some(callback);
            s.start_calls += 1;
            if s.start_failures > 0 {
                s.start_failures -= 1;
                Err(anyhow!("private native startup"))
            } else {
                Ok(())
            }
        }
        fn set_shortcuts_enabled(&mut self, enabled: bool) {
            self.0.borrow_mut().shortcuts = enabled;
        }
        fn set_input_callback(&mut self, callback: Option<Arc<dyn Fn() + Send + Sync>>) {
            self.0.borrow_mut().wake = callback;
        }
        fn bind_native_ui(&mut self, callback: Option<Arc<dyn Fn(String) + Send + Sync>>) {
            self.0.borrow_mut().ui_callback = callback;
        }
        fn client_owns_actions(&self) -> bool {
            self.0.borrow().client_owned
        }
        fn native_edit_pending(&self) -> bool {
            self.0.borrow().client_owned && self.0.borrow().client_pending
        }
        fn present_client_offer(
            &mut self,
            offer: &platform::NativeClientOffer<'_>,
        ) -> Result<Option<platform::NativeHostPresentation>> {
            if !self.0.borrow().client_owned {
                return Ok(None);
            }
            if matches!(offer.mode, "completion" | "correction") {
                Ok(self
                    .present_preedit(&offer.snapshot.native_anchor, offer.text)?
                    .map(platform::NativeHostPresentation::Preedit))
            } else {
                Ok(self
                    .present_choices(&offer.snapshot.native_anchor, offer.values)?
                    .map(platform::NativeHostPresentation::Choices))
            }
        }
        fn arm_client_offer(
            &mut self,
            acceptance: &platform::NativeClientAcceptance<'_>,
        ) -> Result<bool> {
            Ok(self.0.borrow().client_arm
                && match acceptance.presentation {
                    platform::NativeHostPresentation::Preedit(receipt) => {
                        self.native_preedit_valid(receipt)
                    }
                    platform::NativeHostPresentation::Choices(receipt) => {
                        self.native_choices_valid(receipt)
                    }
                })
        }
        fn drain_host_events(&mut self) -> Vec<platform::NativeHostEvent> {
            std::mem::take(&mut self.0.borrow_mut().host_events)
        }
        fn present_preedit(
            &mut self,
            anchor: &str,
            text: &str,
        ) -> Result<Option<platform::NativePreedit>> {
            let mut state = self.0.borrow_mut();
            if !state.native_available
                || state
                    .snapshot
                    .as_ref()
                    .is_none_or(|s| s.native_anchor != anchor)
            {
                return Ok(None);
            }
            let now = state
                .native_clock
                .as_ref()
                .map_or_else(Instant::now, |clock| *clock.borrow());
            let receipt = platform::NativePreedit {
                anchor: anchor.into(),
                text: text.into(),
                revision: state.revision,
                expires_at: now + Duration::from_secs(2),
            };
            state.native_preedit = Some(receipt.clone());
            Ok(Some(receipt))
        }
        fn native_preedit_valid(&self, receipt: &platform::NativePreedit) -> bool {
            let state = self.0.borrow();
            let now = state
                .native_clock
                .as_ref()
                .map_or_else(Instant::now, |clock| *clock.borrow());
            state.native_available
                && state.secure == Some(false)
                && now < receipt.expires_at
                && state.revision == receipt.revision
                && state.native_preedit.as_ref().is_some_and(|current| {
                    current.anchor == receipt.anchor
                        && current.text == receipt.text
                        && current.revision == receipt.revision
                        && current.expires_at == receipt.expires_at
                })
        }
        fn present_choices(
            &mut self,
            anchor: &str,
            values: &[String],
        ) -> Result<Option<platform::NativeChoices>> {
            let mut state = self.0.borrow_mut();
            if !state.native_available
                || state.native_choice_limit == 0
                || state
                    .snapshot
                    .as_ref()
                    .is_none_or(|s| s.native_anchor != anchor)
            {
                return Ok(None);
            }
            let now = state
                .native_clock
                .as_ref()
                .map_or_else(Instant::now, |clock| *clock.borrow());
            let count = values.len().min(state.native_choice_limit);
            let receipt = platform::NativeChoices {
                anchor: anchor.into(),
                values: values[..count].to_vec(),
                revision: state.revision,
                expires_at: now + Duration::from_secs(2),
            };
            state.native_choices = Some(receipt.clone());
            Ok(Some(receipt))
        }
        fn native_choices_valid(&self, receipt: &platform::NativeChoices) -> bool {
            let state = self.0.borrow();
            let now = state
                .native_clock
                .as_ref()
                .map_or_else(Instant::now, |clock| *clock.borrow());
            state.native_available
                && state.secure == Some(false)
                && now < receipt.expires_at
                && state.revision == receipt.revision
                && state.native_choices.as_ref().is_some_and(|current| {
                    current.anchor == receipt.anchor
                        && current.values == receipt.values
                        && current.revision == receipt.revision
                        && current.expires_at == receipt.expires_at
                })
        }
        fn hide_native(&mut self) -> Result<()> {
            let mut state = self.0.borrow_mut();
            state.native_preedit = None;
            state.native_choices = None;
            Ok(())
        }
        fn finish_input(&mut self, accepted: bool) -> Result<()> {
            self.0.borrow_mut().finished.push(accepted);
            Ok(())
        }
        fn input_revision(&self) -> u64 {
            let native = self.0.borrow();
            native.revision.wrapping_add(native.notification_revision)
        }
        fn user_input_revision(&self) -> u64 {
            self.0.borrow().revision
        }
        fn input_timestamp(&self) -> Option<Instant> {
            self.0.borrow().input_at
        }
        fn secure_input_enabled(&self) -> Option<bool> {
            self.0.borrow().secure
        }
        fn request_permission(&mut self, _: &str) -> Result<Permission> {
            Ok(Permission::Unknown)
        }
        fn status(&self) -> Value {
            json!({"native":"synthetic consumer"})
        }
        fn stop(&mut self) -> Result<()> {
            let mut s = self.0.borrow_mut();
            s.stop_calls += 1;
            if s.stop_failures > 0 {
                s.stop_failures -= 1;
                Err(anyhow!("private detail"))
            } else {
                Ok(())
            }
        }
        fn atomic_replacement_supported(&self) -> bool {
            true
        }
        fn docs_hint(&mut self) -> Result<Option<Value>> {
            let mut s = self.0.borrow_mut();
            s.docs_reads += 1;
            Ok(s.docs.clone())
        }
        fn enable_docs_accessibility(&mut self, _: &str) -> Result<Value> {
            self.0.borrow_mut().enables += 1;
            Ok(json!({"status":"unknown"}))
        }
    }
    #[derive(Default)]
    struct WorkerState {
        events: Vec<WorkerEvent>,
        submitted: Vec<(Config, String, String, GenerationOptions)>,
        prepared: usize,
        proceeds: Vec<u64>,
        cancelled: usize,
        released: usize,
        stopped: bool,
        stop_calls: usize,
    }
    struct Inference(Rc<RefCell<WorkerState>>);
    impl Worker for Inference {
        fn prepare(&mut self, _: &Config) -> Result<u64> {
            self.0.borrow_mut().prepared += 1;
            Ok(0)
        }
        fn submit(&mut self, c: &Config, p: &str, s: &str, o: &GenerationOptions) -> Result<u64> {
            let mut w = self.0.borrow_mut();
            w.submitted.push((c.clone(), p.into(), s.into(), o.clone()));
            Ok(w.submitted.len() as u64)
        }
        fn proceed(&mut self, id: u64) -> Result<()> {
            self.0.borrow_mut().proceeds.push(id);
            Ok(())
        }
        fn poll(&mut self) -> Vec<WorkerEvent> {
            std::mem::take(&mut self.0.borrow_mut().events)
        }
        fn cancel(&mut self) -> Result<()> {
            self.0.borrow_mut().cancelled += 1;
            Ok(())
        }
        fn release(&mut self) -> Result<()> {
            self.0.borrow_mut().released += 1;
            Ok(())
        }
        fn set_idle_timeout(&mut self, _: u32) {}
        fn status(&self) -> Value {
            json!({"state":"idle","worker_pid":null,"terminating":false,"release_complete":true})
        }
        fn request_stop(&mut self) -> Result<()> {
            self.0.borrow_mut().stop_calls += 1;
            Ok(())
        }
        fn is_stopped(&mut self) -> bool {
            self.0.borrow().stopped
        }
    }
    #[derive(Default)]
    struct Stored {
        unlocked: bool,
        unlock_fail: bool,
        records: Vec<(String, String, String, Vec<(usize, usize)>)>,
        stats: Vec<(String, bool)>,
        deletes: usize,
        locked: usize,
    }
    struct Memory(Arc<Mutex<Stored>>);
    impl Storage for Memory {
        fn unlock(&mut self) -> Result<()> {
            let mut s = locked(&self.0);
            if s.unlock_fail {
                return Err(anyhow!("secret key failure"));
            }
            s.unlocked = true;
            Ok(())
        }
        fn lock(&mut self) {
            let mut s = locked(&self.0);
            s.unlocked = false;
            s.locked += 1;
        }
        fn record(
            &mut self,
            _: &str,
            app: &str,
            host: &str,
            text: &str,
            spans: &[(usize, usize)],
        ) -> Result<()> {
            locked(&self.0)
                .records
                .push((app.into(), host.into(), text.into(), spans.into()));
            Ok(())
        }
        fn acceptance(&mut self, _: &str, text: &str, new: bool) -> Result<()> {
            locked(&self.0).stats.push((text.into(), new));
            Ok(())
        }
        fn personalization(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: u32,
            _: &dyn Fn(&str, &str) -> bool,
        ) -> Result<String> {
            Ok(String::new())
        }
        fn delete(&mut self, _: Option<&str>, _: Option<&str>, _: bool) -> Result<usize> {
            let mut s = locked(&self.0);
            let n = s.records.len();
            s.records.clear();
            s.deletes += 1;
            Ok(n)
        }
        fn counts(&self, _: bool) -> Result<Value> {
            Ok(json!({"available":true,"apps":{},"domains":{}}))
        }
        fn info(&self) -> Result<Value> {
            Ok(json!({"count":locked(&self.0).records.len()}))
        }
        fn statistics(&self, _: &str, _: &str) -> Result<Value> {
            Ok(json!({"acceptances":locked(&self.0).stats.len()}))
        }
        fn clear_statistics(&mut self) -> Result<()> {
            locked(&self.0).stats.clear();
            Ok(())
        }
    }
    #[derive(Default)]
    struct PreparationQueue {
        pending: Option<Preparation>,
        results: Vec<Prepared>,
        cancelled: usize,
        stopped: bool,
    }
    struct PreparationConsumer(Rc<RefCell<PreparationQueue>>);
    impl Preparer for PreparationConsumer {
        fn submit(&mut self, p: Preparation) {
            self.0.borrow_mut().pending = Some(p);
        }
        fn prefetch(&mut self, _: ContextPreparation) {}
        fn context_updates(&mut self) -> Vec<ContextUpdate> {
            Vec::new()
        }
        fn cancel(&mut self) {
            let mut p = self.0.borrow_mut();
            p.cancelled += 1;
            p.pending = None;
        }
        fn poll(&mut self) -> Vec<Prepared> {
            std::mem::take(&mut self.0.borrow_mut().results)
        }
        fn request_stop(&mut self) {}
        fn is_stopped(&mut self) -> bool {
            self.0.borrow().stopped
        }
    }
    struct Time(Rc<RefCell<Instant>>);
    impl Clock for Time {
        fn now(&self) -> Instant {
            *self.0.borrow()
        }
    }
    struct Services(Rc<RefCell<system::PowerStatus>>);
    impl Readers for Services {
        fn power(&mut self) -> system::PowerStatus {
            self.0.borrow().clone()
        }
        fn persist(&mut self, _: &Config) -> Result<()> {
            Ok(())
        }
    }
    struct PersistedServices(std::path::PathBuf);
    impl Readers for PersistedServices {
        fn power(&mut self) -> system::PowerStatus {
            system::PowerStatus::default()
        }
        fn persist(&mut self, config: &Config) -> Result<()> {
            config::save_config_to(config, &self.0)
        }
    }
    #[derive(Default)]
    struct SpellState {
        ready: bool,
        suspected: bool,
        replacement: String,
        close_calls: usize,
        release: bool,
    }
    struct Dictionary(Arc<Mutex<SpellState>>);
    impl Spelling for Dictionary {
        fn assess(&self, s: &Snapshot) -> Option<SpellingResult> {
            let state = locked(&self.0);
            state.ready.then(|| SpellingResult {
                key: s.key(),
                authority: s.authority(),
                start: 0,
                length: s.caret,
                word: s.prefix().into(),
                replacement: state.replacement.clone(),
                suspected: state.suspected,
                language: "en".into(),
                reason: "native fixture".into(),
                finished: true,
            })
        }
        fn invalidate(&self) {}
        fn close(&self) -> bool {
            let mut s = locked(&self.0);
            s.close_calls += 1;
            s.release
        }
        fn is_stopped(&self) -> bool {
            locked(&self.0).release
        }
    }
    struct Harness {
        engine: Engine,
        native: Rc<RefCell<NativeState>>,
        worker: Rc<RefCell<WorkerState>>,
        preparation: Rc<RefCell<PreparationQueue>>,
        storage: Arc<Mutex<Stored>>,
        time: Rc<RefCell<Instant>>,
        power: Rc<RefCell<system::PowerStatus>>,
    }
    fn field(text: &str) -> Snapshot {
        Snapshot {
            app_id: "writer".into(),
            field_id: "body".into(),
            text: text.into(),
            caret: text.chars().count(),
            caret_rect: Some([10., 20., 1., 18.]),
            field_rect: Some([0., 0., 500., 80.]),
            native_anchor: "verified".into(),
            native_inline: true,
            window_id: "window-1".into(),
            window_revision: "document-1".into(),
            window_pid: 42,
            ..Default::default()
        }
    }
    fn harness(config: Config, spelling: Option<Arc<dyn Spelling>>) -> Harness {
        harness_start(config, spelling, 0)
    }
    fn native_state(config: &Config, start_failures: usize) -> Rc<RefCell<NativeState>> {
        Rc::new(RefCell::new(NativeState {
            snapshot: Some(field("The update")),
            revision: 1,
            notification_revision: 0,
            input_at: None,
            secure: Some(false),
            hook: None,
            wake: None,
            ui_callback: None,
            outcome: Outcome::Applied,
            edits: Vec::new(),
            finished: Vec::new(),
            capture_count: 0,
            capture_time: None,
            stop_calls: 0,
            stop_failures: 0,
            shortcuts: false,
            start_failures,
            start_calls: 0,
            configure_failures: 0,
            docs: None,
            docs_reads: 0,
            metadata: None,
            enables: 0,
            context: None,
            configured: config.clone(),
            native_available: false,
            native_clock: None,
            native_preedit: None,
            native_choices: None,
            native_choice_limit: 0,
            client_owned: false,
            client_pending: false,
            client_arm: true,
            host_events: Vec::new(),
        }))
    }
    fn harness_start(
        config: Config,
        spelling: Option<Arc<dyn Spelling>>,
        start_failures: usize,
    ) -> Harness {
        let native = native_state(&config, start_failures);
        harness_native(config, spelling, native, None)
    }
    fn harness_native(
        config: Config,
        spelling: Option<Arc<dyn Spelling>>,
        native: Rc<RefCell<NativeState>>,
        factory: Option<NativeFactory>,
    ) -> Harness {
        let worker = Rc::new(RefCell::new(WorkerState::default()));
        let preparation = Rc::new(RefCell::new(PreparationQueue::default()));
        let storage = Arc::new(Mutex::new(Stored::default()));
        let time = Rc::new(RefCell::new(Instant::now()));
        let power = Rc::new(RefCell::new(system::PowerStatus::default()));
        let storage_owner: SharedStorage = Arc::new(Mutex::new(Box::new(Memory(storage.clone()))));
        let worker_owner: Box<dyn Worker> = Box::new(Inference(worker.clone()));
        let preparation_owner: Box<dyn Preparer> =
            Box::new(PreparationConsumer(preparation.clone()));
        let readers: Box<dyn Readers> = Box::new(Services(power.clone()));
        let clock: Box<dyn Clock> = Box::new(Time(time.clone()));
        let epoch = Arc::new(AtomicU64::new(1));
        let engine = match factory {
            Some(factory) => Engine::with_native_factory(
                config,
                worker_owner,
                storage_owner,
                preparation_owner,
                spelling,
                readers,
                clock,
                epoch,
                factory,
            ),
            None => Engine::new(
                config,
                Box::new(Native(native.clone())),
                worker_owner,
                storage_owner,
                preparation_owner,
                spelling,
                readers,
                clock,
                epoch,
            ),
        }
        .unwrap();
        Harness {
            engine,
            native,
            worker,
            preparation,
            storage,
            time,
            power,
        }
    }
    impl Harness {
        fn elapse(&mut self, millis: u64) {
            *self.time.borrow_mut() += Duration::from_millis(millis);
        }
        fn type_text(&mut self, text: &str, capture_delay_ms: u64) {
            let wake = {
                let mut native = self.native.borrow_mut();
                native.snapshot = Some(field(text));
                native.revision += 1;
                native.input_at = Some(self.engine.now());
                native.wake.clone().unwrap()
            };
            wake();
            self.elapse(capture_delay_ms);
            self.engine.tick();
        }
        fn observe(&mut self) {
            self.elapse(101);
            self.engine.tick();
        }
        fn force(&mut self) {
            self.engine.command(Command::Action(Action::Force));
            self.engine.tick();
        }
        fn prepared(&mut self) {
            let p = self
                .preparation
                .borrow_mut()
                .pending
                .take()
                .expect("queued preparation");
            self.preparation.borrow_mut().results.push(Prepared {
                token: p.token,
                epoch: p.epoch,
                options: GenerationOptions::default(),
                context_status: "off".into(),
                context_diagnostic: ContextPreparationDiagnostic::default(),
                warning: String::new(),
                reference_expires: None,
            });
            self.engine.tick();
        }
        fn response(&mut self, kind: &str, text: &str, final_: bool) {
            let id = self.worker.borrow().submitted.len() as u64;
            self.worker.borrow_mut().events.push(WorkerEvent {
                id,
                kind: kind.into(),
                text: text.into(),
                final_,
                model_loaded: true,
                message: String::new(),
            });
            self.engine.tick();
        }
        fn offer(&mut self, text: &str) {
            self.force();
            self.prepared();
            self.response("ready", "", false);
            self.response("result", text, true);
        }
        fn drawn(&mut self, visible: &str, mode: &str) {
            let s = self.native.borrow().snapshot.clone().unwrap();
            let native_revision = self.native.borrow().revision;
            let completion_view = mode == "completion"
                || (mode == "alternatives"
                    && self.engine.choices.as_ref().is_some_and(|c| c.anchored));
            let revision = if completion_view {
                self.engine
                    .completion_offer
                    .map_or(native_revision, |offer| offer.serial)
            } else {
                native_revision
            };
            self.engine.command(Command::PresentationResult {
                key: s.key(),
                authority: s.authority(),
                revision,
                visible_text: visible.into(),
                mode: mode.into(),
            });
        }
        fn acknowledge_offer(&mut self, event: &Value) {
            self.engine.command(Command::PresentationResult {
                key: serde_json::from_value(event["key"].clone()).unwrap(),
                authority: serde_json::from_value(event["authority"].clone()).unwrap(),
                revision: event["revision"].as_u64().unwrap(),
                visible_text: event["text"].as_str().unwrap().into(),
                mode: event["mode"].as_str().unwrap().into(),
            });
        }
        fn invoke_hook(&self, action: Action) -> bool {
            let callback = self.native.borrow().hook.clone().unwrap();
            callback(action)
        }
    }
    fn finished_migration(config: &Config, path: String) -> ModelMigration {
        let thread = thread::spawn(move || Ok(Some(path)));
        while !thread.is_finished() {
            thread::yield_now();
        }
        ModelMigration {
            cancel: models::DownloadCancellation::new(),
            thread: Some(thread),
            provider: config.provider_identity(),
            model_id: config.model_id.clone(),
        }
    }

    fn migration_case() -> (tempfile::TempDir, Harness, std::path::PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join("old-model.gguf");
        let migrated = root.path().join("retained-model.gguf");
        std::fs::write(&old, b"storage producer fixture").unwrap();
        std::fs::write(&migrated, b"storage producer fixture").unwrap();
        let config = Config {
            backend: "gguf".into(),
            model_id: "tiny".into(),
            model_path: old.to_str().unwrap().into(),
            ..Config::default()
        };
        let mut h = harness(config, None);
        h.engine.model_migration = Some(finished_migration(
            &h.engine.config,
            migrated.to_str().unwrap().into(),
        ));
        (root, h, migrated)
    }

    #[test]
    fn model_storage_completion_preserves_settings_changed_during_verification() {
        let (root, mut h, migrated) = migration_case();
        let path = root.path().join("settings.json");
        h.engine.readers = Box::new(PersistedServices(path.clone()));
        let mut latest = h.engine.config.clone();
        latest.global_enabled = false;
        latest.record_history = false;
        latest.record_unaccepted = false;
        latest
            .app_rules
            .entry("app:writer".into())
            .or_default()
            .insert("enabled".into(), json!(false));
        h.engine.apply_config(latest.clone(), false).unwrap();
        h.engine.consume_model_migration();
        let saved: Config = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(saved.model_path, migrated.to_str().unwrap());
        assert!(!saved.global_enabled);
        assert!(!saved.record_history);
        assert!(!saved.record_unaccepted);
        assert_eq!(saved.app_rules, latest.app_rules);
        assert_eq!(h.engine.config.model_path, saved.model_path);
        assert!(std::path::Path::new(&latest.model_path).is_file());
    }

    #[test]
    fn model_storage_completion_does_not_override_a_new_model_selection() {
        let (root, mut h, _) = migration_case();
        let path = root.path().join("settings.json");
        h.engine.readers = Box::new(PersistedServices(path.clone()));
        let selected = root.path().join("chosen-model.gguf");
        std::fs::write(&selected, b"new selection fixture").unwrap();
        let mut latest = h.engine.config.clone();
        latest.model_path = selected.to_str().unwrap().into();
        h.engine.apply_config(latest, false).unwrap();
        h.engine.consume_model_migration();
        assert_eq!(h.engine.config.model_path, selected.to_str().unwrap());
        assert!(!path.exists());
        assert_eq!(std::fs::read(selected).unwrap(), b"new selection fixture");
    }

    #[test]
    fn model_storage_persist_failure_keeps_the_existing_selection() {
        let (root, mut h, migrated) = migration_case();
        let original = h.engine.config.model_path.clone();
        let not_directory = root.path().join("not-a-directory");
        std::fs::write(&not_directory, b"owned failure fixture").unwrap();
        h.engine.readers = Box::new(PersistedServices(not_directory.join("settings.json")));
        h.engine.consume_model_migration();
        assert_eq!(h.engine.config.model_path, original);
        assert_eq!(h.engine.error, MODEL_STORAGE_WARNING);
        assert!(std::path::Path::new(&original).is_file());
        assert!(migrated.is_file());
    }

    #[test]
    fn shutdown_cancels_model_storage_work_without_adopting_a_late_result() {
        let (root, mut h, _) = migration_case();
        let original = h.engine.config.model_path.clone();
        let path = root.path().join("settings.json");
        h.engine.readers = Box::new(PersistedServices(path.clone()));
        let cancel = models::DownloadCancellation::new();
        let worker_cancel = cancel.clone();
        let thread = thread::spawn(move || {
            while worker_cancel.check().is_ok() {
                thread::yield_now();
            }
            Ok(Some("must-not-be-selected.gguf".into()))
        });
        h.engine.model_migration = Some(ModelMigration {
            cancel: cancel.clone(),
            thread: Some(thread),
            provider: h.engine.config.provider_identity(),
            model_id: h.engine.config.model_id.clone(),
        });
        h.engine.begin_shutdown();
        assert!(cancel.check().is_err());
        while !h
            .engine
            .model_migration
            .as_ref()
            .unwrap()
            .thread
            .as_ref()
            .unwrap()
            .is_finished()
        {
            thread::yield_now();
        }
        h.engine.consume_model_migration();
        assert_eq!(h.engine.config.model_path, original);
        assert!(!path.exists());
        assert!(std::path::Path::new(&original).is_file());
    }

    fn native_offer(h: &mut Harness, text: &str) {
        {
            let mut state = h.native.borrow_mut();
            state.native_available = true;
            state.native_clock = Some(h.time.clone());
        }
        h.offer(text);
        let snapshot = h.native.borrow().snapshot.clone().unwrap();
        let serial = h.engine.completion_offer.unwrap().serial;
        assert!(
            h.engine
                .present_preedit(
                    &snapshot.key(),
                    &snapshot.authority(),
                    serial,
                    &snapshot.native_anchor,
                    text
                )
                .unwrap()
        );
    }

    fn client_offer(h: &mut Harness) -> (Snapshot, platform::NativeHostPresentation) {
        {
            let mut native = h.native.borrow_mut();
            native.client_owned = true;
            native.native_available = true;
            native.native_clock = Some(h.time.clone());
        }
        h.offer(" is ready");
        let before = h.native.borrow().snapshot.clone().unwrap();
        let serial = h.engine.completion_offer.unwrap().serial;
        assert_eq!(
            h.engine
                .present_client(&before.key(), &before.authority(), serial, "completion"),
            Some(true)
        );
        let receipt = h.native.borrow().native_preedit.clone().unwrap();
        (before, platform::NativeHostPresentation::Preedit(receipt))
    }
    fn client_applied(
        before: &Snapshot,
        after: &Snapshot,
        receipt: &platform::NativeHostPresentation,
        at: Instant,
    ) -> platform::NativeHostEvent {
        platform::NativeHostEvent::Applied(platform::NativeHostEdit {
            presentation: receipt.clone(),
            before: before.clone(),
            after: after.clone(),
            start: before.caret,
            length: 0,
            text: " is ready".into(),
            action: Action::All,
            mode: "completion".into(),
            accepted_at: at,
        })
    }
    #[test]
    fn client_confirmation_counts_once_without_reinsertion_or_global_tab() {
        let mut config = Config::default();
        config.statistics_enabled = true;
        config.trailing_space = false;
        let mut h = harness(config, None);
        let (before, receipt) = client_offer(&mut h);
        assert!(!h.invoke_hook(Action::All));
        let after = field("The update is ready");
        {
            let mut native = h.native.borrow_mut();
            native.snapshot = Some(after.clone());
            native.native_preedit = None;
            native
                .host_events
                .push(client_applied(&before, &after, &receipt, *h.time.borrow()));
            native
                .host_events
                .push(client_applied(&before, &after, &receipt, *h.time.borrow()));
        }
        h.engine.tick();
        assert_eq!(
            h.native.borrow().snapshot.as_ref().unwrap().text.as_ref(),
            "The update is ready"
        );
        assert_eq!(locked(&h.storage).stats, [(" is ready".into(), true)]);
        assert!(h.native.borrow().edits.is_empty());
    }
    #[test]
    fn client_confirmation_survives_later_input_without_repainting_old_text() {
        let mut config = Config::default();
        config.statistics_enabled = true;
        config.trailing_space = false;
        let mut h = harness(config, None);
        let (before, receipt) = client_offer(&mut h);
        let accepted_at = *h.time.borrow();
        let after = field("The update is ready");
        h.elapse(5);
        {
            let mut native = h.native.borrow_mut();
            native.snapshot = Some(field("The update is ready!"));
            native.native_preedit = None;
            native.revision += 1;
            native.input_at = Some(*h.time.borrow());
            native
                .host_events
                .push(client_applied(&before, &after, &receipt, accepted_at));
        }
        h.engine.tick();
        assert_eq!(
            h.native.borrow().snapshot.as_ref().unwrap().text.as_ref(),
            "The update is ready!"
        );
        assert_eq!(
            h.engine.candidate.as_ref().unwrap().text.as_ref(),
            "The update is ready!"
        );
        assert!(h.engine.suggestion.text.is_empty());
        assert_eq!(locked(&h.storage).stats, [(" is ready".into(), true)]);
        assert!(h.native.borrow().edits.is_empty());
    }
    #[test]
    fn pending_client_edit_preserves_its_witness_until_real_confirmation() {
        let mut config = Config::default();
        config.statistics_enabled = true;
        config.trailing_space = false;
        let mut h = harness(config, None);
        let (before, receipt) = client_offer(&mut h);
        let accepted_at = *h.time.borrow();
        {
            let mut native = h.native.borrow_mut();
            native.client_pending = true;
            native.native_preedit = None;
        }
        h.elapse(1000);
        h.engine.tick();
        assert!(h.engine.candidate.as_ref().unwrap().same_key(&before));
        assert_eq!(h.engine.suggestion.text, " is ready");
        assert!(locked(&h.storage).stats.is_empty());
        let after = field("The update is ready");
        {
            let mut native = h.native.borrow_mut();
            native.client_pending = false;
            native.snapshot = Some(after.clone());
            native
                .host_events
                .push(client_applied(&before, &after, &receipt, accepted_at));
        }
        h.engine.tick();
        assert_eq!(
            h.native.borrow().snapshot.as_ref().unwrap().text.as_ref(),
            "The update is ready"
        );
        assert_eq!(locked(&h.storage).stats, [(" is ready".into(), true)]);
    }
    #[test]
    fn revoked_client_confirmation_protects_provenance_without_recording() {
        let mut config = Config::default();
        config.statistics_enabled = true;
        let mut h = harness(config, None);
        let (before, receipt) = client_offer(&mut h);
        let mut disabled = h.engine.config.clone();
        disabled.global_enabled = false;
        h.engine.command(Command::ApplyConfig {
            config: disabled,
            persist: false,
        });
        h.native
            .borrow_mut()
            .host_events
            .push(platform::NativeHostEvent::Indeterminate(
                platform::NativeHostUncertain {
                    presentation: receipt,
                    before: before.clone(),
                    mode: "completion".into(),
                    accepted_at: *h.time.borrow(),
                },
            ));
        h.engine.tick();
        assert!(
            h.engine
                .uncertain_provenance
                .as_ref()
                .is_some_and(|(authority, _)| *authority == before.authority())
        );
        assert!(locked(&h.storage).stats.is_empty());
        assert!(locked(&h.storage).records.is_empty());
        assert!(h.native.borrow().edits.is_empty());
    }
    #[test]
    fn client_app_toggle_reenables_a_disabled_app_without_text_capture_permission() {
        let mut h = harness(Config::default(), None);
        let snapshot = h.native.borrow().snapshot.clone().unwrap();
        h.engine
            .config
            .app_rules
            .entry(format!("app:{}", snapshot.app_id))
            .or_default()
            .insert("enabled".into(), json!(false));
        assert!(!h.engine.allowed(&snapshot.app_id, &snapshot.domain, ""));
        h.native.borrow_mut().client_owned = true;
        h.native
            .borrow_mut()
            .host_events
            .push(platform::NativeHostEvent::Action {
                action: Action::AppToggle,
                origin: Foreground {
                    app_id: snapshot.app_id.clone(),
                    domain: snapshot.domain.clone(),
                    window_id: snapshot.window_id.clone(),
                    window_pid: snapshot.window_pid,
                    ..Default::default()
                },
            });
        h.engine.tick();
        assert!(h.engine.allowed(&snapshot.app_id, &snapshot.domain, ""));
        assert_eq!(
            h.native.borrow().snapshot.as_ref().unwrap().text,
            snapshot.text
        );
    }

    #[test]
    fn native_preedit_acceptance_uses_a_live_protocol_lease_without_a_raster_ack() {
        let mut h = harness(Config::default(), None);
        native_offer(&mut h, " is ready");
        assert!(h.invoke_hook(Action::All));
        h.engine.tick();
        assert_eq!(
            h.native.borrow().snapshot.as_ref().unwrap().text.as_ref(),
            "The update is ready"
        );
        assert_eq!(h.native.borrow().edits, [" is ready"]);
        assert!(!h.invoke_hook(Action::All));
    }

    #[test]
    fn expired_revoked_and_protected_native_offers_cannot_edit_or_dismiss() {
        for transition in ["expired", "revoked", "protected"] {
            let mut h = harness(Config::default(), None);
            native_offer(&mut h, " is ready");
            match transition {
                "expired" => h.elapse(2100),
                "revoked" => h.native.borrow_mut().native_preedit = None,
                "protected" => h.native.borrow_mut().secure = Some(true),
                _ => unreachable!(),
            }
            assert!(!h.engine.accept(&Action::All, None), "{transition}");
            assert!(!h.engine.dismiss(false, None), "{transition}");
            assert!(h.native.borrow().edits.is_empty(), "{transition}");
            assert_eq!(
                h.native.borrow().snapshot.as_ref().unwrap().text.as_ref(),
                "The update"
            );
        }
    }

    #[test]
    fn native_dismissal_expiring_during_capture_does_not_consume_or_suppress() {
        for behavior in ["pause", "app"] {
            let mut config = Config::default();
            config.escape_behavior = behavior.into();
            let mut h = harness(config, None);
            native_offer(&mut h, " is ready");
            h.elapse(1950);
            h.native.borrow_mut().capture_time = Some((h.time.clone(), Duration::from_millis(75)));
            assert!(!h.engine.dismiss(false, None), "{behavior}");
            assert!(h.engine.field_pause.is_none(), "{behavior}");
            assert!(h.engine.escape_app.is_empty(), "{behavior}");
            assert!(h.native.borrow().edits.is_empty());
        }
    }

    #[test]
    fn native_preedit_renewal_survives_original_expiry_and_failure_revokes_acceptance() {
        for available in [true, false] {
            let mut h = harness(Config::default(), None);
            native_offer(&mut h, " is ready");
            h.elapse(1950);
            h.native.borrow_mut().native_available = available;
            h.engine.renew_native_presentations();
            h.elapse(100);
            if available {
                assert!(h.invoke_hook(Action::All));
                h.engine.tick();
                assert_eq!(
                    h.native.borrow().snapshot.as_ref().unwrap().text.as_ref(),
                    "The update is ready"
                );
            } else {
                assert!(!h.invoke_hook(Action::All));
                assert!(h.native.borrow().native_preedit.is_none());
                assert_eq!(
                    h.native.borrow().snapshot.as_ref().unwrap().text.as_ref(),
                    "The update"
                );
            }
        }
    }

    #[test]
    fn renewed_native_preedit_cannot_accept_a_replaced_claim() {
        let mut h = harness(Config::default(), None);
        native_offer(&mut h, " is ready");
        let original = h.engine.claims[0].clone();
        h.elapse(1950);
        h.engine.renew_native_presentations();
        assert!(!h.engine.accept(&Action::All, Some(original)));
        assert!(h.native.borrow().edits.is_empty());
        assert_eq!(
            h.native.borrow().snapshot.as_ref().unwrap().text.as_ref(),
            "The update"
        );
    }

    #[test]
    fn native_choices_never_accept_a_candidate_outside_the_presented_prefix() {
        let mut config = Config::default();
        config.prompt_style = "writing".into();
        let mut h = harness(config, None);
        {
            let mut state = h.native.borrow_mut();
            let mut selected = field("A useful update");
            selected.caret = 2;
            selected.selection_length = 6;
            state.snapshot = Some(selected);
            state.native_available = true;
            state.native_clock = Some(h.time.clone());
            state.native_choice_limit = 1;
        }
        h.engine.command(Command::Action(Action::Synonyms));
        h.engine.tick();
        h.prepared();
        h.response("ready", "", false);
        h.response("result", "[\"helpful\",\"practical\",\"valuable\"]", true);
        let snapshot = h.native.borrow().snapshot.clone().unwrap();
        let revision = h.native.borrow().revision;
        let values = h.engine.choices.as_ref().unwrap().values.clone();
        assert!(
            h.engine
                .present_choices(
                    &snapshot.key(),
                    &snapshot.authority(),
                    revision,
                    &snapshot.native_anchor,
                    "synonyms",
                    &values
                )
                .unwrap()
        );
        assert!(!h.invoke_hook(Action::Choice(1)));
        h.engine.command(Command::Action(Action::Choice(1)));
        h.engine.tick();
        assert!(h.native.borrow().edits.is_empty());
        assert!(
            h.engine
                .present_choices(
                    &snapshot.key(),
                    &snapshot.authority(),
                    revision,
                    &snapshot.native_anchor,
                    "synonyms",
                    &values
                )
                .unwrap()
        );
        assert!(h.invoke_hook(Action::Choice(0)));
        h.engine.tick();
        assert_eq!(
            h.native.borrow().snapshot.as_ref().unwrap().text.as_ref(),
            "A helpful update"
        );
        assert_eq!(h.native.borrow().edits, ["helpful"]);
    }

    #[test]
    fn whatsapp_runtime_diagnostic_is_content_free_and_retained_across_foreground_switches() {
        let mut config = Config::default();
        config.typo_suppression = false;
        config.typo_fixes = false;
        let mut h = harness(config, None);
        h.offer(" synthetic ordinary proposal");
        assert!(!h.engine.whatsapp_diagnostic.observed);
        let mut snapshot = field("SYNTHETIC_PRIVATE_DRAFT");
        snapshot.app_id = WHATSAPP_APP_ID.into();
        snapshot.field_id = "SYNTHETIC_PRIVATE_FIELD".into();
        snapshot.window_id = "SYNTHETIC_PRIVATE_WINDOW".into();
        snapshot.window_revision = "SYNTHETIC_PRIVATE_DOCUMENT".into();
        snapshot.domain = "synthetic-private.invalid".into();
        h.native.borrow_mut().snapshot = Some(snapshot);
        h.force();
        let pending = h.preparation.borrow_mut().pending.take().unwrap();
        h.preparation.borrow_mut().results.push(Prepared {
            token: pending.token,
            epoch: pending.epoch,
            options: GenerationOptions {
                context: "SYNTHETIC_PRIVATE_CONTEXT".into(),
                ..Default::default()
            },
            context_status: "ready".into(),
            context_diagnostic: ContextPreparationDiagnostic {
                screen: ContextCollectorOutcome::Ready,
                attachment: ContextAttachmentOutcome::Attached,
                ..Default::default()
            },
            warning: String::new(),
            reference_expires: None,
        });
        h.engine.tick();
        h.response("ready", "", false);
        h.response("result", " SYNTHETIC_PRIVATE_PROPOSAL", true);
        h.drawn("", "completion");
        assert_eq!(h.engine.whatsapp_diagnostic.counters.rejected_acks, 1);
        assert_eq!(h.engine.whatsapp_diagnostic.counters.rendered_acks, 0);
        assert!(h.engine.claims.is_empty());
        h.drawn(" SYNTHETIC_PRIVATE_PROPOSAL", "completion");
        let diagnostic = &h.engine.whatsapp_diagnostic;
        assert_eq!(diagnostic.last_context_attached, Some(true));
        assert_eq!(diagnostic.counters.submitted, 1);
        assert_eq!(diagnostic.counters.proceeded, 1);
        assert_eq!(diagnostic.counters.rendered_acks, 1);
        assert!(diagnostic.state.suggestion_present);
        assert_eq!(diagnostic.state.claim_count, 1);
        let captures = diagnostic.counters.captures;
        let serialized = serde_json::to_string(diagnostic).unwrap();
        for excluded in [
            "SYNTHETIC_PRIVATE",
            "synthetic-private.invalid",
            "window_id",
            "field_id",
            "\"text\":",
            "\"message\":",
        ] {
            assert!(!serialized.contains(excluded));
        }
        h.native.borrow_mut().snapshot = Some(field("SYNTHETIC_OTHER_APP_DRAFT"));
        h.observe();
        let diagnostic = &h.engine.whatsapp_diagnostic;
        assert_eq!(diagnostic.counters.captures, captures);
        assert_eq!(diagnostic.counters.submitted, 1);
        assert_eq!(diagnostic.last_context_attached, Some(true));
        assert!(!diagnostic.state.foreground_whatsapp);
        assert!(!diagnostic.state.candidate_present);
        assert!(!diagnostic.state.suggestion_present);
        assert_eq!(diagnostic.state.claim_count, 0);
        assert!(
            !serde_json::to_string(diagnostic)
                .unwrap()
                .contains("SYNTHETIC_OTHER")
        );
    }

    #[test]
    fn whatsapp_runtime_diagnostic_distinguishes_attached_context_from_late_readiness_and_revocation()
     {
        let mut config = Config::default();
        config.debounce_ms = 200;
        config.typo_suppression = false;
        config.typo_fixes = false;
        let mut h = harness(config, None);
        let mut snapshot = field("Synthetic draft");
        snapshot.app_id = WHATSAPP_APP_ID.into();
        h.native.borrow_mut().snapshot = Some(snapshot);
        h.force();
        h.prepared();
        assert_eq!(
            h.engine.whatsapp_diagnostic.last_context_attached,
            Some(false)
        );
        h.engine.context_status = "ready".into();
        h.engine.whatsapp_state();
        assert!(h.engine.whatsapp_diagnostic.state.context_read_ready);
        assert_eq!(
            h.engine.whatsapp_diagnostic.last_context_attached,
            Some(false)
        );
        let wake = h.native.borrow().wake.clone().unwrap();
        wake();
        h.observe();
        assert_eq!(h.engine.whatsapp_diagnostic.counters.revoked_epochs, 1);
        assert_eq!(h.engine.whatsapp_diagnostic.counters.submitted, 1);
        assert_eq!(h.engine.whatsapp_diagnostic.counters.proceeded, 0);
        assert!(!h.engine.whatsapp_diagnostic.state.request.present);
        assert!(!h.engine.whatsapp_diagnostic.state.attempted_completion);
        h.response("ready", "", false);
        assert_eq!(h.engine.whatsapp_diagnostic.counters.stale_worker_events, 1);
        assert!(h.worker.borrow().proceeds.is_empty());
    }

    #[test]
    fn submitted_context_diagnostic_preserves_collector_and_attachment_outcomes() {
        let mut config = Config::default();
        config.typo_suppression = false;
        config.typo_fixes = false;
        let mut h = harness(config, None);
        let mut snapshot = field("SYNTHETIC_PRIVATE_DRAFT");
        snapshot.app_id = WHATSAPP_APP_ID.into();
        h.native.borrow_mut().snapshot = Some(snapshot);
        h.force();
        let pending = h.preparation.borrow_mut().pending.take().unwrap();
        h.preparation.borrow_mut().results.push(Prepared {
            token: pending.token,
            epoch: pending.epoch,
            options: GenerationOptions {
                context: "SYNTHETIC_PRIVATE_CONTEXT".into(),
                ..Default::default()
            },
            context_status: "ready".into(),
            context_diagnostic: ContextPreparationDiagnostic {
                screen: ContextCollectorOutcome::Ready,
                clipboard: ContextCollectorOutcome::Empty,
                attachment: ContextAttachmentOutcome::Attached,
            },
            warning: String::new(),
            reference_expires: Some(*h.time.borrow() - Duration::from_millis(1)),
        });
        h.engine.tick();
        let diagnostic = &h.engine.whatsapp_diagnostic;
        assert_eq!(diagnostic.last_context_attached, Some(false));
        assert_eq!(diagnostic.last_context_read_ready, Some(true));
        let context = diagnostic.last_context_preparation.unwrap();
        assert_eq!(context.screen, ContextCollectorOutcome::Ready);
        assert_eq!(context.clipboard, ContextCollectorOutcome::Empty);
        assert_eq!(context.attachment, ContextAttachmentOutcome::Expired);
        h.engine.context_status = "unavailable".into();
        h.engine.whatsapp_state();
        assert_eq!(
            h.engine.whatsapp_diagnostic.last_context_read_ready,
            Some(true)
        );
        let serialized = serde_json::to_string(&h.engine.whatsapp_diagnostic).unwrap();
        assert!(!serialized.contains("SYNTHETIC_PRIVATE"));
    }

    #[test]
    fn ready_after_cold_load_refuses_expired_context_before_writing_proceeds() {
        let mut config = Config::default();
        config.typo_suppression = false;
        config.typo_fixes = false;
        config.personalization_strength = 0;
        let mut h = harness(config, None);
        h.force();
        for (context, expires_after) in [
            ("SYNTHETIC_OLD_CONTEXT", 2000),
            ("SYNTHETIC_FRESH_CONTEXT", 2000),
        ] {
            if context == "SYNTHETIC_FRESH_CONTEXT" {
                h.engine.tick();
            }
            let pending = h.preparation.borrow_mut().pending.take().unwrap();
            h.preparation.borrow_mut().results.push(Prepared {
                token: pending.token,
                epoch: pending.epoch,
                options: GenerationOptions {
                    context: context.into(),
                    ..Default::default()
                },
                context_status: "ready".into(),
                context_diagnostic: ContextPreparationDiagnostic {
                    screen: ContextCollectorOutcome::Ready,
                    attachment: ContextAttachmentOutcome::Attached,
                    ..Default::default()
                },
                warning: String::new(),
                reference_expires: Some(*h.time.borrow() + Duration::from_millis(expires_after)),
            });
            h.engine.tick();
            assert!(h.worker.borrow().proceeds.is_empty());
            if context == "SYNTHETIC_OLD_CONTEXT" {
                h.elapse(2001);
                h.response("ready", "", false);
                assert!(h.worker.borrow().proceeds.is_empty());
                assert!(h.engine.preparing.is_some());
                assert!(h.engine.request.is_none());
            } else {
                h.response("ready", "", false);
                assert_eq!(h.worker.borrow().proceeds, vec![2]);
            }
        }
        let worker = h.worker.borrow();
        assert_eq!(worker.submitted.len(), 2);
        assert_eq!(worker.submitted[0].3.context, "SYNTHETIC_OLD_CONTEXT");
        assert_eq!(worker.submitted[1].3.context, "SYNTHETIC_FRESH_CONTEXT");
    }

    #[test]
    fn context_diagnostic_only_exports_fixed_outcomes() {
        assert_eq!(
            ContextCollectorOutcome::from_status("SYNTHETIC_PRIVATE_ERROR"),
            ContextCollectorOutcome::Error
        );
        assert_eq!(
            context_attachment_outcome(true, false),
            ContextAttachmentOutcome::RankerEmpty
        );
        assert_eq!(
            context_attachment_outcome(false, false),
            ContextAttachmentOutcome::NoReferences
        );
        assert_eq!(
            context_attachment_outcome(true, true),
            ContextAttachmentOutcome::Attached
        );
        let diagnostic = ContextPreparationDiagnostic {
            screen: ContextCollectorOutcome::TimedOut,
            clipboard: ContextCollectorOutcome::AuthorizationStale,
            attachment: ContextAttachmentOutcome::NoReferences,
        };
        assert_eq!(
            serde_json::to_value(diagnostic).unwrap(),
            json!({
                "screen": "timed_out",
                "clipboard": "authorization_stale",
                "attachment": "no_references",
            })
        );
    }

    #[test]
    fn whatsapp_runtime_worker_diagnostic_never_copies_status_strings() {
        let status = json!({
            "state": "SYNTHETIC_PRIVATE_STATE",
            "message": "SYNTHETIC_PRIVATE_ERROR",
            "worker_pid": 42,
            "model_loaded": true,
            "initializing": false,
            "terminating": false,
            "context": "SYNTHETIC_PRIVATE_CONTEXT",
        });
        let diagnostic = serde_json::to_value(DiagnosticWorker::from_status(&status)).unwrap();
        assert_eq!(diagnostic["state"], "unknown");
        assert_eq!(diagnostic["worker_pid"], 42);
        assert_eq!(diagnostic["model_loaded"], true);
        assert!(!diagnostic.to_string().contains("SYNTHETIC_PRIVATE"));
        assert!(diagnostic.get("message").is_none());
        assert!(diagnostic.get("context").is_none());
    }

    #[test]
    fn ready_requires_fresh_native_authority_and_geometry_does_not_regenerate() {
        let mut h = harness(Config::default(), None);
        h.force();
        h.prepared();
        assert!(h.worker.borrow().proceeds.is_empty());
        h.native
            .borrow_mut()
            .snapshot
            .as_mut()
            .unwrap()
            .window_revision = "other-document".into();
        h.response("ready", "", false);
        assert!(h.worker.borrow().proceeds.is_empty());
        assert!(h.native.borrow().edits.is_empty());
        h.offer(" arrives tomorrow");
        let submitted = h.worker.borrow().submitted.len();
        h.native.borrow_mut().snapshot.as_mut().unwrap().caret_rect = Some([20., 20., 1., 18.]);
        h.observe();
        assert_eq!(h.worker.borrow().submitted.len(), submitted);
        assert!(
            h.engine
                .drain_events()
                .iter()
                .any(|v| v["type"] == "suggestion"
                    && v["authority"]["window_revision"] == "other-document")
        );
    }
    #[test]
    fn window_only_change_rejects_output_and_old_presentation_ack() {
        let mut h = harness(Config::default(), None);
        h.offer(" arrives");
        let old = h.native.borrow().snapshot.clone().unwrap();
        h.native.borrow_mut().snapshot.as_mut().unwrap().window_id = "window-2".into();
        h.observe();
        h.engine.command(Command::PresentationResult {
            key: old.key(),
            authority: old.authority(),
            revision: 1,
            visible_text: " arrives".into(),
            mode: "completion".into(),
        });
        assert!(!h.invoke_hook(Action::Word));
        h.engine.command(Command::Action(Action::Word));
        assert!(h.native.borrow().edits.is_empty());
    }
    #[test]
    fn no_hidden_or_truncated_word_can_be_accepted_and_claim_is_single_use() {
        let mut h = harness(Config::default(), None);
        h.offer(" arrives tomorrow");
        assert!(!h.invoke_hook(Action::Word));
        h.drawn(" arr", "completion");
        assert!(h.invoke_hook(Action::Word));
        assert!(!h.invoke_hook(Action::Word));
        h.engine.tick();
        assert!(h.native.borrow().edits.is_empty());
        assert_eq!(h.native.borrow().finished, vec![false]);
        h.drawn(" arrives", "completion");
        assert!(h.invoke_hook(Action::All));
        h.engine.tick();
        assert_eq!(h.native.borrow().edits, vec![" arrives "]);
        assert_eq!(locked(&h.storage).stats.len(), 1);
    }
    #[test]
    fn final_completion_waits_for_pending_spelling_instead_of_losing_explicit_force() {
        let spelling = Arc::new(Mutex::new(SpellState::default()));
        let mut c = Config::default();
        c.typo_suppression = true;
        let mut h = harness(c, Some(Arc::new(Dictionary(spelling.clone()))));
        h.offer(" arrives tomorrow");
        assert!(
            !h.engine
                .drain_events()
                .iter()
                .any(|e| e["type"] == "suggestion")
        );
        locked(&spelling).ready = true;
        h.engine.tick();
        assert!(
            h.engine
                .drain_events()
                .iter()
                .any(|e| e["type"] == "suggestion" && e["text"] == " arrives tomorrow")
        );
        locked(&spelling).release = true;
    }
    #[test]
    fn dispatched_edit_counts_only_after_exact_observation_and_uncertain_is_never_replayed() {
        for outcome in [
            Outcome::Dispatched,
            Outcome::Indeterminate,
            Outcome::WrongReadback,
        ] {
            let mut h = harness(Config::default(), None);
            h.offer(" arrives");
            h.drawn(" arrives", "completion");
            h.native.borrow_mut().outcome = outcome;
            assert!(h.invoke_hook(Action::All));
            h.engine.tick();
            assert!(locked(&h.storage).stats.is_empty());
            assert_eq!(h.native.borrow().finished, vec![true]);
            assert!(!h.invoke_hook(Action::All));
            h.engine.tick();
            assert_eq!(h.native.borrow().edits.len(), 1);
            if matches!(outcome, Outcome::Dispatched) {
                let before = h.native.borrow().snapshot.clone().unwrap();
                h.native.borrow_mut().snapshot =
                    edit_snapshot(&before, before.caret, 0, " arrives");
                h.observe();
                assert_eq!(locked(&h.storage).stats, vec![(" arrives".into(), true)]);
            }
        }
    }
    #[test]
    fn rejected_native_edit_returns_original_shortcut_without_accounting() {
        let mut h = harness(Config::default(), None);
        h.offer(" arrives");
        h.drawn(" arrives", "completion");
        h.native.borrow_mut().outcome = Outcome::Rejected;
        assert!(h.invoke_hook(Action::Word));
        h.engine.tick();
        assert_eq!(h.native.borrow().finished, vec![false]);
        assert!(locked(&h.storage).stats.is_empty());
        assert_eq!(
            h.native.borrow().snapshot.as_ref().unwrap().text.as_ref(),
            "The update"
        );
    }
    #[test]
    fn revoked_clipboard_consent_and_superseded_preparation_cannot_submit() {
        let mut c = Config::default();
        c.clipboard_context = true;
        let mut h = harness(c, None);
        h.force();
        let p = h.preparation.borrow_mut().pending.take().unwrap();
        let mut updated = h.engine.config.clone();
        updated.clipboard_context = false;
        h.engine.command(Command::ApplyConfig {
            config: updated,
            persist: false,
        });
        h.preparation.borrow_mut().results.push(Prepared {
            token: p.token,
            epoch: p.epoch,
            options: GenerationOptions {
                context: "must never submit".into(),
                ..Default::default()
            },
            context_status: "ready".into(),
            context_diagnostic: ContextPreparationDiagnostic::default(),
            warning: String::new(),
            reference_expires: None,
        });
        h.engine.tick();
        assert!(h.worker.borrow().submitted.is_empty());
        assert_eq!(h.worker.borrow().released, 1);
    }
    #[test]
    fn automatic_completion_handoff_retains_current_spelling_verdict() {
        use crate::spelling::{SpellBackend, SpellWorkScope};
        use std::sync::atomic::AtomicUsize;

        struct CountingDictionary(Arc<AtomicUsize>);
        impl SpellBackend for CountingDictionary {
            fn work_scope(&mut self) -> Result<Box<dyn SpellWorkScope>> {
                self.0.fetch_add(1, Ordering::AcqRel);
                Ok(Box::new(()))
            }
            fn languages(&mut self) -> Result<(Vec<String>, Vec<String>)> {
                Ok((vec!["en".into()], vec!["en".into()]))
            }
            fn identify(&mut self, _: &str) -> Result<BTreeMap<String, f64>> {
                Ok(BTreeMap::from([("en".into(), 1.0)]))
            }
            fn misspelled(&mut self, _: &str, _: &str) -> Result<Vec<(usize, usize)>> {
                Ok(Vec::new())
            }
            fn has_completions(&mut self, _: &str, _: &str) -> Result<bool> {
                Ok(true)
            }
            fn suggestions(
                &mut self,
                _: &str,
                _: usize,
                _: usize,
                _: &str,
            ) -> Result<(String, Vec<String>)> {
                Ok((String::new(), Vec::new()))
            }
            fn close(&mut self) -> Result<()> {
                Ok(())
            }
        }

        let mut config = Config::default();
        config.debounce_ms = 200;
        config.typo_suppression = true;
        config.typo_fixes = false;
        let assessments = Arc::new(AtomicUsize::new(0));
        let (ready, completed) = mpsc::channel();
        let spelling = Arc::new(SpellingService::new(
            Some(Box::new(CountingDictionary(assessments.clone()))),
            Some(Arc::new(move || {
                let _ = ready.send(());
            })),
        ));
        let mut h = harness(config, Some(spelling.clone()));
        h.engine.tick();
        assert!(
            h.engine.preparing.is_none(),
            "pending spelling blocks scheduling"
        );
        completed.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(assessments.load(Ordering::Acquire), 1);
        h.elapse(200);
        h.engine.tick();
        assert!(h.engine.preparing.is_some());
        let snapshot = h.native.borrow().snapshot.clone().unwrap();
        assert!(
            spelling.assess(&snapshot).is_some(),
            "unchanged handoff discarded the ready verdict and queued a duplicate assessment"
        );
        h.engine.tick();
        h.prepared();
        h.response("ready", "", false);
        h.response("result", " arrives", false);
        assert_eq!(h.engine.suggestion.text, " arrives");
        assert_eq!(assessments.load(Ordering::Acquire), 1);
        assert!(
            h.engine.claims.is_empty(),
            "offering is not a paint acknowledgement"
        );
        h.drawn(" arrives", "completion");
        assert_eq!(h.engine.claims.len(), 1);

        // Real input must still revoke the stream and its acceptance authority.
        h.native.borrow_mut().snapshot = Some(field("A different draft"));
        h.native.borrow_mut().revision += 1;
        let wake = h.native.borrow().wake.clone().unwrap();
        wake();
        h.engine.tick();
        assert!(h.engine.request.is_none());
        assert!(h.engine.suggestion.text.is_empty());
        assert!(h.engine.claims.is_empty());
    }

    #[test]
    fn slow_capture_deadline_starts_after_completion_but_input_and_force_stay_fresh() {
        let mut config = Config::default();
        config.debounce_ms = 5000;
        config.typo_suppression = false;
        config.typo_fixes = false;
        let mut h = harness(config, None);
        h.native.borrow_mut().capture_time = Some((h.time.clone(), Duration::from_millis(80)));
        h.engine.capture_after = h.engine.now();
        let initial_captures = h.native.borrow().capture_count;
        h.engine.tick();
        let captured = h.native.borrow().capture_count;
        assert_eq!(captured, initial_captures + 1);
        assert_eq!(
            h.engine.capture_after,
            h.engine.now() + Duration::from_millis(50),
            "slow native capture left the next routine deadline already expired"
        );
        h.engine.tick();
        assert_eq!(h.native.borrow().capture_count, captured);
        h.elapse(49);
        h.engine.tick();
        assert_eq!(h.native.borrow().capture_count, captured);
        h.elapse(1);
        h.engine.tick();
        assert_eq!(h.native.borrow().capture_count, captured + 1);
        assert_eq!(
            h.engine.capture_after,
            h.engine.now() + Duration::from_millis(50)
        );

        // A real input callback still bypasses the routine deadline immediately.
        let epoch = h.engine.current_epoch();
        assert!(h.engine.now() < h.engine.capture_after);
        h.type_text("The latest typed draft", 0);
        assert_eq!(h.native.borrow().capture_count, captured + 2);
        assert!(h.engine.current_epoch() > epoch);
        let current = h.native.borrow().snapshot.clone().unwrap();
        assert!(h.engine.candidate.as_ref().unwrap().same_key(&current));
        assert_eq!(h.engine.candidate_revision, h.native.borrow().revision);
        assert!(h.engine.preparing.is_none());

        // Explicit actions must capture new native text even without a wake.
        let latest = field("A fresh explicit draft");
        h.native.borrow_mut().snapshot = Some(latest.clone());
        assert!(h.engine.now() < h.engine.capture_after);
        h.engine.command(Command::Action(Action::Force));
        assert!(h.native.borrow().capture_count > captured + 2);
        let preparation = h.engine.preparing.as_ref().unwrap();
        assert!(preparation.lease.snapshot.same_key(&latest));
        assert_eq!(preparation.lease.revision, h.native.borrow().revision);
        assert!(h.worker.borrow().proceeds.is_empty());
    }

    #[test]
    fn typing_cadence_cancels_obsolete_work_and_paints_only_latest_draft() {
        let mut config = Config::default();
        config.debounce_ms = 150;
        config.typo_suppression = false;
        config.typo_fixes = false;
        let mut h = harness(config, None);
        h.engine.tick();
        for text in ["The update i", "The update is", "The update is "] {
            h.elapse(80);
            h.type_text(text, 20);
            assert!(h.engine.preparing.is_none());
            assert!(h.worker.borrow().submitted.is_empty());
        }
        h.elapse(129);
        h.engine.tick();
        assert!(h.engine.preparing.is_none());
        h.elapse(1);
        h.engine.tick();
        assert!(h.engine.preparing.is_some());
        h.elapse(10);
        h.engine.tick();
        h.prepared();
        h.elapse(10);
        h.response("ready", "", false);
        assert_eq!(h.worker.borrow().proceeds, [1]);
        let cancelled = h.worker.borrow().cancelled;

        // Type again before the first token, then deliver every obsolete event.
        h.elapse(80);
        h.type_text("The update is now", 20);
        let last_key = h.native.borrow().input_at.unwrap();
        assert_eq!(h.engine.candidate_since, last_key);
        assert_eq!(h.worker.borrow().cancelled - cancelled, 1);
        assert!(h.engine.request.is_none());
        for kind in ["ready", "result", "done"] {
            h.worker.borrow_mut().events.push(WorkerEvent {
                id: 1,
                kind: kind.into(),
                text: " obsolete draft".into(),
                final_: true,
                model_loaded: true,
                message: String::new(),
            });
        }
        h.engine.tick();
        assert_eq!(h.worker.borrow().proceeds, [1]);
        assert!(h.engine.suggestion.text.is_empty());
        assert!(h.engine.claims.is_empty());
        assert_eq!(h.engine.candidate_since, last_key);

        h.elapse(129);
        h.engine.tick();
        assert!(h.engine.preparing.is_none());
        h.elapse(1);
        h.engine.tick();
        assert!(h.engine.preparing.is_some());
        let scheduled = h.engine.now();
        h.elapse(10);
        h.engine.tick();
        h.prepared();
        let submitted = h.engine.now();
        assert_eq!(h.worker.borrow().submitted.len(), 2);
        assert_eq!(h.worker.borrow().submitted[1].1, "The update is now");
        h.elapse(10);
        h.response("ready", "", false);
        let proceeded = h.engine.now();
        assert_eq!(h.worker.borrow().proceeds, [1, 2]);
        h.elapse(300);
        h.response("result", " ready for review", false);
        let offered = h.engine.now();
        assert_eq!(h.engine.suggestion.text, " ready for review");
        assert_eq!(
            h.engine.suggestion.snapshot.as_ref().unwrap().text.as_ref(),
            "The update is now"
        );
        assert!(h.engine.claims.is_empty());
        h.elapse(16);
        h.drawn(" ready for review", "completion");
        let painted = h.engine.now();
        assert_eq!(h.engine.claims.len(), 1);
        let elapsed = |at: Instant| at.duration_since(last_key).as_millis() as u64;
        assert_eq!(
            [scheduled, submitted, proceeded, offered, painted].map(elapsed),
            [150, 160, 170, 470, 486]
        );
        println!(
            "{}",
            json!({
                "scenario":"typing_cadence_latest_only",
                "clock":"synthetic",
                "capture_delay_ms":20,
                "debounce_ms":150,
                "injected_generation_ms":300,
                "last_key_to_schedule_ms":elapsed(scheduled),
                "last_key_to_submit_ms":elapsed(submitted),
                "last_key_to_proceed_ms":elapsed(proceeded),
                "last_key_to_offer_ms":elapsed(offered),
                "last_key_to_paint_ack_ms":elapsed(painted),
                "submitted_requests":h.worker.borrow().submitted.len(),
                "typing_cancel_calls":1,
                "obsolete_events_discarded":3,
                "captures":h.native.borrow().capture_count
            })
        );
    }

    #[test]
    fn transient_inline_geometry_rejection_does_not_latch_the_unchanged_draft() {
        for stage in 0..3 {
            let mut config = Config::default();
            config.debounce_ms = 200;
            config.typo_suppression = false;
            config.typo_fixes = false;
            let mut h = harness(config, None);
            h.force();
            if stage >= 1 {
                h.prepared();
            }
            if stage == 2 {
                h.response("ready", "", false);
            }
            let original = h.native.borrow().snapshot.clone().unwrap();
            {
                let mut native = h.native.borrow_mut();
                let snapshot = native.snapshot.as_mut().unwrap();
                snapshot.native_inline = false;
                snapshot.native_anchor.clear();
                snapshot.inline_render = None;
            }
            match stage {
                0 => h.prepared(),
                1 => h.response("ready", "", false),
                _ => h.response("result", " obsolete proposal", true),
            }
            assert!(h.engine.preparing.is_none());
            assert!(h.engine.request.is_none());
            assert!(h.engine.attempted.is_none());
            assert!(h.engine.suggestion.text.is_empty());
            assert!(h.engine.claims.is_empty());
            h.native.borrow_mut().snapshot = Some(original);
            h.observe();
            h.elapse(201);
            h.engine.tick();
            h.engine.tick();
            h.prepared();
            h.response("ready", "", false);
            h.response("result", " arrives", true);
            assert_eq!(h.engine.suggestion.text, " arrives");
            assert!(h.engine.claims.is_empty());
            h.drawn(" arrives", "completion");
            assert!(h.invoke_hook(Action::All));
        }
    }

    #[test]
    fn emoji_choice_zero_requires_visible_ack_and_preserves_exact_trigger_range() {
        for trigger in [":sorriso", "<3"] {
            for can_replace in [false, true] {
                let mut config = Config::default();
                config.emoji_enabled = true;
                config.emoticons_enabled = true;
                config.typo_suppression = false;
                config.typo_fixes = false;
                let mut h = harness(config, None);
                let mut snapshot = field(&format!("Olá  {trigger}"));
                snapshot.can_replace = can_replace;
                h.native.borrow_mut().snapshot = Some(snapshot);
                h.observe();
                assert!(!h.invoke_hook(Action::Choice(0)));
                assert!(h.native.borrow().edits.is_empty());
                if !can_replace {
                    assert!(h.engine.choices.is_none());
                    assert!(
                        h.engine
                            .preparing
                            .as_ref()
                            .is_none_or(|p| p.mode != "emoji")
                    );
                    h.engine.command(Command::Action(Action::Choice(0)));
                    h.engine.tick();
                    assert!(h.native.borrow().edits.is_empty());
                    assert!(h.worker.borrow().submitted.is_empty());
                    continue;
                }
                let values = h.engine.choices.as_ref().unwrap().values.clone();
                h.drawn(&values.join("\n"), "emoji");
                assert!(h.invoke_hook(Action::Choice(0)));
                h.engine.tick();
                assert_eq!(
                    h.native.borrow().snapshot.as_ref().unwrap().text.as_ref(),
                    format!("Olá  {}", values[0])
                );
                assert_eq!(h.native.borrow().edits, vec![values[0].clone()]);
                assert!(h.worker.borrow().submitted.is_empty());
            }
        }
    }

    #[test]
    fn same_draft_native_notification_restarts_only_revoked_inflight_work() {
        for submitted in [false, true] {
            let mut config = Config::default();
            config.debounce_ms = 200;
            config.typo_suppression = false;
            config.typo_fixes = false;
            let mut h = harness(config, None);
            h.force();
            if submitted {
                h.prepared();
            }
            let retired_id = h.engine.request.as_ref().map(|request| request.id);
            let revision = h.native.borrow().revision;
            let cancelled = h.worker.borrow().cancelled;
            // AX value/selection notifications also invalidate the epoch, but
            // need not change either user input revision or the captured draft.
            let wake = h.native.borrow().wake.clone().unwrap();
            wake();
            h.observe();
            assert_eq!(h.native.borrow().revision, revision);
            assert!(h.worker.borrow().cancelled > cancelled);
            assert!(h.engine.preparing.is_none());
            assert!(h.engine.request.is_none());
            assert!(h.engine.attempted.is_none());
            if let Some(id) = retired_id {
                h.worker.borrow_mut().events.push(WorkerEvent {
                    id,
                    kind: "result".into(),
                    text: " obsolete proposal".into(),
                    final_: true,
                    model_loaded: true,
                    message: String::new(),
                });
                h.engine.tick();
                assert!(h.engine.suggestion.text.is_empty());
            }
            h.elapse(201);
            h.engine.tick();
            h.engine.tick();
            assert!(h.preparation.borrow().pending.is_some());
            h.prepared();
            h.response("ready", "", false);
            h.response("result", " arrives", true);
            assert_eq!(h.engine.suggestion.text, " arrives");
            assert!(h.engine.claims.is_empty());
            h.drawn(" arrives", "completion");
            assert!(h.invoke_hook(Action::All));
        }
    }

    #[test]
    fn completed_completion_repaints_after_native_notification_and_accepts_fresh_ack() {
        let mut config = Config::default();
        config.debounce_ms = 5000;
        config.typo_suppression = false;
        config.typo_fixes = false;
        let mut h = harness(config, None);
        h.offer(" ready for review");
        let original = h
            .engine
            .drain_events()
            .into_iter()
            .rev()
            .find(|event| event["type"] == "suggestion" && event["mode"] == "completion")
            .unwrap();
        h.acknowledge_offer(&original);
        assert_eq!(h.engine.claims.len(), 1);
        assert!(h.engine.request.is_none());
        h.elapse(1000);
        h.engine.tick();
        assert_eq!(
            h.engine.claims.len(),
            1,
            "idle time alone must not expire a claim"
        );
        h.engine.drain_events();
        let user_revision = h.native.borrow().revision;
        h.native.borrow_mut().notification_revision += 1;
        let wake = h.native.borrow().wake.clone().unwrap();
        wake();
        assert!(
            !h.invoke_hook(Action::All),
            "notification immediately revokes old authority"
        );
        h.engine.tick();
        assert_eq!(h.native.borrow().revision, user_revision);
        assert!(h.engine.claims.is_empty());
        let refreshed = h
            .engine
            .drain_events()
            .into_iter()
            .find(|event| event["type"] == "suggestion" && event["mode"] == "completion")
            .expect("fresh unchanged capture must request another presentation acknowledgement");
        assert_eq!(refreshed["text"], original["text"]);
        assert!(
            !h.invoke_hook(Action::All),
            "capture alone is not a paint acknowledgement"
        );
        h.acknowledge_offer(&original);
        assert!(
            h.engine.claims.is_empty(),
            "old queued acknowledgement must not authorize a newer same-text presentation"
        );
        assert!(!h.invoke_hook(Action::All));
        h.acknowledge_offer(&refreshed);
        assert_eq!(h.engine.claims.len(), 1);
        h.engine.tick();
        assert_eq!(
            h.engine.claims.len(),
            1,
            "leftover pending notification must not disarm the fresh paint acknowledgement"
        );
        assert_eq!(h.worker.borrow().submitted.len(), 1);
        assert!(h.invoke_hook(Action::All));
        h.engine.tick();
        assert_eq!(h.native.borrow().edits, [" ready for review"]);
        assert_eq!(
            h.native.borrow().snapshot.as_ref().unwrap().text.as_ref(),
            "The update ready for review"
        );
        assert!(h.engine.pending_edit.is_none());
    }

    #[test]
    fn completed_completion_stale_ack_cannot_restore_revoked_input_focus_policy_or_epoch() {
        for change in [
            "typing",
            "same_text_input",
            "focus",
            "policy",
            "epoch",
            "pending",
        ] {
            let mut config = Config::default();
            config.debounce_ms = 5000;
            config.typo_suppression = false;
            config.typo_fixes = false;
            let mut h = harness(config, None);
            h.offer(" ready for review");
            let original = h
                .engine
                .drain_events()
                .into_iter()
                .rev()
                .find(|event| event["type"] == "suggestion" && event["mode"] == "completion")
                .unwrap();
            h.acknowledge_offer(&original);
            assert_eq!(h.engine.claims.len(), 1);
            match change {
                "typing" | "same_text_input" | "focus" => {
                    {
                        let mut native = h.native.borrow_mut();
                        native.revision += 1;
                        if change == "typing" {
                            native.snapshot = Some(field("The update changed"));
                        } else if change == "focus" {
                            native.snapshot.as_mut().unwrap().window_id = "other-window".into();
                        }
                    }
                    let wake = h.native.borrow().wake.clone().unwrap();
                    wake();
                    h.engine.tick();
                    if change == "same_text_input" {
                        // A later harmless notification cannot rehabilitate an
                        // offer that was already revoked by real user input.
                        h.native.borrow_mut().notification_revision += 1;
                        wake();
                        h.engine.tick();
                    }
                }
                "policy" => {
                    h.engine.command(Command::Pause { minutes: 1 });
                    h.engine.tick();
                }
                "epoch" => {
                    h.engine.bump_epoch();
                    h.engine.tick();
                }
                "pending" => {
                    h.native.borrow_mut().notification_revision += 1;
                    let wake = h.native.borrow().wake.clone().unwrap();
                    wake();
                    // The old queued paint acknowledgement arrives before a
                    // native capture has observed this notification.
                }
                _ => unreachable!(),
            }
            let before = h.native.borrow().snapshot.clone().unwrap();
            h.acknowledge_offer(&original);
            assert!(
                h.engine.claims.is_empty(),
                "old paint acknowledgement restored authority after {change}"
            );
            assert!(!h.invoke_hook(Action::All));
            assert!(h.native.borrow().edits.is_empty());
            assert!(
                h.native
                    .borrow()
                    .snapshot
                    .as_ref()
                    .unwrap()
                    .same_key(&before)
            );
            assert_eq!(h.worker.borrow().submitted.len(), 1);
            if change != "pending" {
                assert!(
                    h.engine
                        .drain_events()
                        .iter()
                        .all(|event| event["type"] != "suggestion" || event["mode"] != "completion"),
                    "genuine revocation must not republish the stale completion after {change}"
                );
            }
        }
    }

    #[test]
    fn same_text_after_real_input_revokes_acceptance_but_passthrough_dismissal_keeps_actual_emoji_mode()
     {
        let mut c = Config::default();
        c.escape_behavior = "passthrough".into();
        let mut h = harness(c, None);
        h.native.borrow_mut().snapshot = Some(field("Hi :smile"));
        h.observe();
        let event = h
            .engine
            .drain_events()
            .into_iter()
            .find(|e| e["type"] == "suggestion")
            .unwrap();
        assert_eq!(event["mode"], "emoji");
        let rows = event["choices"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        h.drawn(&rows, "emoji");
        assert!(!h.invoke_hook(Action::Dismiss));
        h.native.borrow_mut().revision += 1;
        let wake = h.native.borrow().wake.clone().unwrap();
        wake();
        h.engine.tick();
        h.observe();
        assert!(
            !h.engine
                .drain_events()
                .iter()
                .any(|e| e["type"] == "suggestion" && e["mode"] == "emoji")
        );
    }
    #[test]
    fn selection_synonyms_replace_only_selected_range_and_alternatives_do_not_steal_tab() {
        let mut c = Config::default();
        c.prompt_style = "writing".into();
        c.alternatives_enabled = true;
        let mut h = harness(c, None);
        let mut selected = field("A useful update");
        selected.caret = 2;
        selected.selection_length = 6;
        h.native.borrow_mut().snapshot = Some(selected);
        h.engine.command(Command::Action(Action::Synonyms));
        h.engine.tick();
        h.prepared();
        h.response("ready", "", false);
        h.response("result", "[\"helpful\",\"practical\",\"valuable\"]", true);
        h.drawn("helpful\npractical", "synonyms");
        h.engine.command(Command::Action(Action::Choice(0)));
        h.engine.tick();
        assert_eq!(
            h.native.borrow().snapshot.as_ref().unwrap().text.as_ref(),
            "A helpful update"
        );
        h.native.borrow_mut().snapshot = Some(field("The update"));
        h.observe();
        h.engine.command(Command::Action(Action::Alternatives));
        h.engine.tick();
        h.prepared();
        h.response("ready", "", false);
        h.response(
            "result",
            "[\" arrives\",\" is ready\",\" will ship\"]",
            true,
        );
        h.drawn(" arrives\n is ready", "alternatives");
        assert!(!h.invoke_hook(Action::Word));
        h.engine.command(Command::Action(Action::Choice(1)));
        h.engine.tick();
        assert_eq!(
            h.native.borrow().snapshot.as_ref().unwrap().text.as_ref(),
            "The update is ready"
        );
    }
    #[test]
    fn anchored_alternatives_reacknowledge_retained_completion_before_acceptance() {
        for abstain in [false, true] {
            let mut config = Config::default();
            config.alternatives_enabled = true;
            let mut h = harness(config, None);
            h.offer(" arrives tomorrow");
            h.drawn(" arrives tomorrow", "completion");
            h.engine.drain_events();
            h.engine.command(Command::Action(Action::Alternatives));
            h.engine.tick();
            assert!(
                h.engine
                    .drain_events()
                    .iter()
                    .any(|event| event["type"] == "suggestion"
                        && event["mode"] == "completion"
                        && event["text"] == " arrives tomorrow")
            );
            assert!(!h.invoke_hook(Action::Word));
            h.drawn(" arrives tomorrow", "completion");
            if abstain {
                h.prepared();
                h.response("ready", "", false);
                h.response("result", "[]", true);
            }
            assert!(h.invoke_hook(Action::Word));
            h.engine.tick();
            assert_eq!(h.native.borrow().edits, vec![" arrives "]);
            assert_eq!(h.engine.suggestion.text, "tomorrow");
        }
    }
    #[test]
    fn anchored_alternatives_echo_completion_view_version_but_accept_native_user_revision() {
        let mut config = Config::default();
        config.alternatives_enabled = true;
        config.typo_suppression = false;
        config.typo_fixes = false;
        let mut h = harness(config, None);
        h.native.borrow_mut().revision = 41;
        h.offer(" arrives tomorrow");
        h.engine.drain_events();
        h.engine.command(Command::Action(Action::Alternatives));
        h.engine.tick();
        h.prepared();
        h.response("ready", "", false);
        h.response("result", "[\" is ready\", \" arrives tonight\"]", true);
        let event = h
            .engine
            .drain_events()
            .into_iter()
            .rev()
            .find(|event| {
                event["type"] == "suggestion"
                    && event["mode"] == "completion"
                    && event["choices_mode"] == "alternatives"
            })
            .unwrap();
        assert_ne!(event["revision"].as_u64().unwrap(), 41);
        h.acknowledge_offer(&event);
        let mut alternatives_ack = event.clone();
        alternatives_ack["mode"] = json!("alternatives");
        alternatives_ack["text"] = json!(" is ready\n arrives tonight");
        h.acknowledge_offer(&alternatives_ack);
        let claim = h
            .engine
            .claims
            .iter()
            .find(|claim| claim.mode == "alternatives")
            .unwrap();
        assert_eq!(claim.revision, 41);
        assert!(h.invoke_hook(Action::Choice(1)));
        h.engine.tick();
        assert_eq!(h.native.borrow().edits, [" arrives tonight"]);
        assert_eq!(
            h.native.borrow().snapshot.as_ref().unwrap().text.as_ref(),
            "The update arrives tonight"
        );
        assert!(h.engine.pending_edit.is_none());
    }

    #[test]
    fn on_demand_append_only_fields_allow_force_but_no_automatic_tools() {
        let mut c = Config::default();
        c.alternatives_enabled = true;
        c.alternatives_mode = "pause".into();
        let mut h = harness(c, None);
        {
            let mut n = h.native.borrow_mut();
            let s = n.snapshot.as_mut().unwrap();
            s.on_demand_only = true;
            s.can_replace = false;
        }
        h.observe();
        h.elapse(5000);
        h.engine.tick();
        assert!(h.preparation.borrow().pending.is_none());
        assert_eq!(h.worker.borrow().prepared, 0);
        h.offer(" arrives");
        h.drawn(" arrives", "completion");
        h.engine.command(Command::Action(Action::All));
        h.engine.tick();
        assert_eq!(h.native.borrow().edits, vec![" arrives"]);
    }
    #[test]
    fn app_session_overrides_expire_without_persisting_and_pause_releases_then_resumes() {
        let mut c = Config::default();
        c.enabled = false;
        c.app_rules.insert(
            "app:writer".into(),
            BTreeMap::from([("enabled".into(), json!(true))]),
        );
        let mut h = harness(c, None);
        h.observe();
        assert_eq!(h.engine.status()["current_app_enabled"], true);
        h.engine.command(Command::ToggleApp {
            app: "writer".into(),
            domain: String::new(),
        });
        assert_eq!(h.engine.status()["current_app_enabled"], false);
        assert_eq!(h.engine.config.app_rules["app:writer"]["enabled"], true);
        h.elapse(300001);
        h.engine.tick();
        assert_eq!(h.engine.status()["current_app_enabled"], true);
        h.engine.command(Command::Pause { minutes: 1 });
        assert_eq!(h.worker.borrow().released, 1);
        assert!(!h.native.borrow().configured.global_enabled);
        h.elapse(60001);
        h.engine.tick();
        assert_eq!(h.engine.status()["enabled"], true);
        assert!(h.native.borrow().configured.global_enabled);
    }
    #[test]
    fn battery_force_only_and_explicit_zero_residency_preserve_load_without_evaluation() {
        let mut c = Config::default();
        c.idle_release_seconds = 0;
        c.battery_on_demand = true;
        let mut h = harness(c, None);
        *h.power.borrow_mut() = system::PowerStatus {
            available: true,
            has_battery: true,
            on_battery: true,
            charging: Some(false),
            percent: Some(18),
        };
        h.observe();
        h.elapse(1000);
        h.engine.tick();
        assert_eq!(h.worker.borrow().prepared, 0);
        assert!(h.worker.borrow().submitted.is_empty());
        h.force();
        h.prepared();
        assert_eq!(h.worker.borrow().submitted.len(), 1);
        assert!(h.worker.borrow().proceeds.is_empty());
        h.engine.command(Command::Pause { minutes: 0 });
        h.power.borrow_mut().on_battery = false;
        h.elapse(5001);
        h.engine.tick();
        assert!(h.worker.borrow().prepared > 0);
        assert_eq!(h.worker.borrow().submitted.len(), 1);
    }
    #[test]
    fn authored_provenance_never_marks_generated_or_initial_text_as_typing() {
        let mut c = Config::default();
        c.record_history = true;
        c.record_unaccepted = true;
        let mut h = harness(c, None);
        h.observe();
        h.native.borrow_mut().snapshot = Some(field("The update is"));
        h.observe();
        h.offer(" ready");
        h.drawn(" ready", "completion");
        h.engine.command(Command::Action(Action::All));
        h.engine.tick();
        let records = &locked(&h.storage).records;
        let record = records.last().unwrap();
        assert_eq!(record.2, "The update is ready");
        assert_eq!(record.3, vec![(10, 13)]);
    }
    #[test]
    fn scoped_deletion_invalidates_derived_context_and_suppresses_unchanged_draft() {
        let mut c = Config::default();
        c.record_history = true;
        c.record_unaccepted = true;
        let mut h = harness(c, None);
        h.observe();
        h.offer(" ready");
        h.drawn(" ready", "completion");
        h.engine.command(Command::Action(Action::All));
        h.engine.tick();
        h.engine.command(Command::DeleteHistory {
            app: Some("writer".into()),
            domain: None,
        });
        assert_eq!(locked(&h.storage).deletes, 1);
        assert!(locked(&h.storage).records.is_empty());
        h.observe();
        h.engine.command(Command::Pause { minutes: 1 });
        assert!(locked(&h.storage).records.is_empty());
        assert!(h.worker.borrow().released >= 1);
    }
    #[test]
    fn unknown_security_never_reads_text_and_history_unlock_failure_does_not_save_consent() {
        let mut h = harness(Config::default(), None);
        h.native.borrow_mut().secure = None;
        h.engine.tick();
        assert_eq!(h.native.borrow().capture_count, 0);
        locked(&h.storage).unlock_fail = true;
        let mut c = h.engine.config.clone();
        c.record_history = true;
        h.engine.command(Command::ApplyConfig {
            config: c,
            persist: true,
        });
        assert!(!h.engine.config.record_history);
        assert!(h.engine.drain_events().iter().any(|e| e["type"] == "error"));
    }
    #[test]
    fn shutdown_retries_only_incomplete_owners_and_never_forgets_spelling_cleanup() {
        let mut h = harness(Config::default(), None);
        h.native.borrow_mut().stop_failures = 1;
        h.engine.begin_shutdown();
        assert!(h.engine.shutdown_step().is_err());
        assert_eq!(h.worker.borrow().stop_calls, 1);
        assert!(!h.engine.shutdown_step().unwrap());
        assert_eq!(h.native.borrow().stop_calls, 2);
        h.worker.borrow_mut().stopped = true;
        h.preparation.borrow_mut().stopped = true;
        assert!(h.engine.shutdown_step().unwrap());
        assert_eq!(h.native.borrow().stop_calls, 2);
        assert_eq!(h.worker.borrow().stop_calls, 1);
    }
    #[test]
    fn nonmutating_shortcut_suspension_and_preview_are_distinct() {
        let mut h = harness(Config::default(), None);
        h.offer(" ready");
        let releases = h.worker.borrow().released;
        h.engine
            .command(Command::SuspendShortcuts { suspended: true });
        assert!(!h.native.borrow().shortcuts);
        assert_eq!(h.worker.borrow().released, releases);
        assert!(!h.invoke_hook(Action::Word));
        h.engine
            .command(Command::SuspendShortcuts { suspended: false });
        assert!(h.native.borrow().shortcuts);
        h.engine.command(Command::SuspendForPreview);
        assert_eq!(h.engine.status()["preview_suspended"], true);
        assert_eq!(h.worker.borrow().released, releases + 1);
        h.engine.command(Command::ResumeFromPreview);
        assert_eq!(h.engine.status()["preview"], false);
    }
    #[test]
    fn authored_scalar_ranges_handle_unicode_replacement_and_bulk_paste_abstention() {
        let s = field("a🖋e\u{301}");
        let mut session = Session {
            key: s.authority(),
            id: "synthetic".into(),
            text: s.text.to_string(),
            authored: vec![false; 4],
            accepted: false,
        };
        update_authored(&mut session, "a🖋e\u{301}界", true);
        assert_eq!(authored_spans(&session.authored), vec![(4, 5)]);
        update_authored(&mut session, &"x".repeat(100), true);
        assert!(authored_spans(&session.authored).is_empty());
    }
    #[test]
    fn correction_dismissal_and_replacement_require_current_authority() {
        let spell = Arc::new(Mutex::new(SpellState {
            ready: true,
            suspected: true,
            replacement: "update".into(),
            release: true,
            ..Default::default()
        }));
        let mut c = Config::default();
        c.typo_fixes = true;
        c.escape_behavior = "passthrough".into();
        let mut h = harness(c, Some(Arc::new(Dictionary(spell))));
        {
            let mut n = h.native.borrow_mut();
            let s = n.snapshot.as_mut().unwrap();
            s.inline_render = Some(InlineRender {
                font_name: "Fixture".into(),
                font_size: 14.,
                baseline: 34.,
                clip_rect: [0., 0., 500., 80.],
                ascent: 12.,
                descent: 3.,
                source: "native".into(),
                ..Default::default()
            });
        }
        h.observe();
        let events = h.engine.drain_events();
        assert!(events.iter().any(|e| e["mode"] == "correction"));
        h.drawn("update", "correction");
        h.engine.command(Command::Action(Action::Dismiss));
        h.observe();
        assert!(
            !h.engine
                .drain_events()
                .iter()
                .any(|e| e["mode"] == "correction")
        );
    }
    #[test]
    fn docs_help_is_metadata_only_single_use_and_document_identity_revokes_it() {
        let mut h = harness(Config::default(), None);
        {
            let mut n = h.native.borrow_mut();
            n.snapshot = None;
            n.metadata = Some(Foreground {
                app_id: "writer".into(),
                domain: "docs.google.com".into(),
                window_id: "docs-window".into(),
                window_pid: 42,
                document_id: "doc-a".into(),
                ..Default::default()
            });
            n.docs = Some(
                json!({"token":"capability-a","app_id":"writer","support":"unknown","help_url":"https://support.google.com/docs/"}),
            );
        }
        h.engine.tick();
        let events = h.engine.drain_events();
        assert!(
            events
                .iter()
                .any(|e| e["type"] == "docs_help" && e["hint"]["token"] == "capability-a")
        );
        assert!(!h.invoke_hook(Action::Word));
        h.engine.tick();
        assert_eq!(h.native.borrow().docs_reads, 1);
        assert!(h.worker.borrow().submitted.is_empty());
        h.engine.command(Command::EnableDocs {
            token: "capability-a".into(),
        });
        h.engine.command(Command::EnableDocs {
            token: "capability-a".into(),
        });
        assert_eq!(h.native.borrow().enables, 1);
        assert!(h.native.borrow().edits.is_empty());
        h.elapse(2001);
        h.engine.tick();
        h.engine.drain_events();
        h.native.borrow_mut().metadata.as_mut().unwrap().document_id = "doc-b".into();
        h.engine.tick();
        h.engine.command(Command::EnableDocs {
            token: "capability-a".into(),
        });
        assert_eq!(h.native.borrow().enables, 1);
        assert!(
            h.engine
                .drain_events()
                .iter()
                .any(|e| e["type"] == "docs_help" && e["hint"].is_null())
        );
    }
    #[test]
    fn pending_spelling_cleanup_remains_owned_until_retry_can_finish() {
        let state = Arc::new(Mutex::new(SpellState::default()));
        let mut h = harness(Config::default(), Some(Arc::new(Dictionary(state.clone()))));
        h.worker.borrow_mut().stopped = true;
        h.preparation.borrow_mut().stopped = true;
        h.engine.begin_shutdown();
        assert!(!h.engine.shutdown_step().unwrap());
        assert!(Arc::strong_count(&state) > 1);
        locked(&state).release = true;
        // Join only the injected cleanup boundary to provide deterministic worker
        // completion; shutdown itself never waits for this service.
        for thread in std::mem::take(&mut h.engine.spelling_cleanup) {
            thread.join().unwrap();
        }
        assert!(locked(&state).close_calls > 0);
        assert!(h.engine.shutdown_step().unwrap());
    }
    #[derive(Clone)]
    struct ContextTicket {
        snapshot: Snapshot,
        gate: Option<Arc<(Mutex<bool>, Condvar)>>,
        entered: Option<SyncSender<()>>,
        text: String,
    }
    impl platform::DetachedContextRead for ContextTicket {
        fn matches_snapshot(&self, s: &Snapshot) -> bool {
            self.snapshot.same_key(s) && self.snapshot.same_authority(s)
        }
        fn matches(&self) -> Result<bool> {
            Ok(true)
        }
        fn read(&self, authorized: &dyn Fn() -> bool, _: usize) -> Result<ContextRead> {
            if let Some(entered) = &self.entered {
                let _ = entered.try_send(());
            }
            if let Some(gate) = &self.gate {
                let state = locked(&gate.0);
                drop(
                    gate.1
                        .wait_timeout_while(state, Duration::from_secs(2), |ready| !*ready)
                        .unwrap(),
                );
            }
            Ok(if authorized() {
                ContextRead {
                    text: self.text.clone(),
                    status: "ready".into(),
                    source: "accessibility".into(),
                }
            } else {
                ContextRead::blocked("accessibility")
            })
        }
    }
    fn preparation(
        snapshot: Snapshot,
        token: u64,
        epoch: u64,
        ticket: Option<Box<dyn platform::DetachedContextRead>>,
    ) -> Preparation {
        let mut config = Config::default();
        config.screen_context = true;
        config.personalization_strength = 0;
        Preparation {
            token,
            epoch,
            snapshot,
            config,
            mode: "completion".into(),
            anchor: String::new(),
            references: Vec::new(),
            context_ticket: ticket,
            authorization_ticket: None,
            hard_disabled_apps: BTreeSet::new(),
        }
    }
    struct SyntheticSources;
    impl BackgroundReaders for SyntheticSources {
        fn clipboard(
            &self,
            _: Arc<dyn Fn() -> bool + Send + Sync>,
            _: Arc<dyn Fn() -> bool + Send + Sync>,
        ) -> Result<String> {
            panic!("clipboard must be independently opted in and authorized")
        }
        fn screen(
            &self,
            snapshot: &Snapshot,
            ticket: &dyn platform::DetachedContextRead,
            guard: &dyn Fn() -> bool,
            max: usize,
            ax: bool,
            _: bool,
        ) -> Result<ContextRead> {
            if !ax || !guard() || !ticket.matches_snapshot(snapshot) || !ticket.matches()? {
                return Ok(ContextRead::blocked("accessibility"));
            }
            let result = ticket.read(guard, max)?;
            if guard() && ticket.matches()? {
                Ok(result)
            } else {
                Ok(ContextRead::blocked("accessibility"))
            }
        }
    }
    #[test]
    fn focused_context_prefetch_handoff_reaches_the_generation_consumer_once() {
        for completed_before_submit in [false, true] {
            let mut config = Config::default();
            config.screen_context = true;
            config.personalization_strength = 0;
            config.typo_suppression = false;
            config.typo_fixes = false;
            config.debounce_ms = 1000;
            let mut h = harness(config, None);
            let snapshot = field("Thank you for the launch review");
            let gate = Arc::new((Mutex::new(completed_before_submit), Condvar::new()));
            let (entered, receive) = mpsc::sync_channel(1);
            {
                let mut native = h.native.borrow_mut();
                native.snapshot = Some(snapshot.clone());
                native.context = Some(ContextTicket {
                    snapshot,
                    gate: Some(gate.clone()),
                    entered: Some(entered),
                    text: "The launch review confirmed Friday's release.".into(),
                });
            }
            let storage: SharedStorage = Arc::new(Mutex::new(Box::new(Memory(h.storage.clone()))));
            let service =
                AsyncPreparer::new(storage, h.engine.epoch.clone(), Arc::new(SyntheticSources))
                    .unwrap();
            let state = service.state.clone();
            h.engine.preparer = Box::new(service);
            h.observe();
            receive.recv_timeout(Duration::from_secs(1)).unwrap();
            if completed_before_submit {
                let locked_state = locked(&state.0);
                let (ready_state, wait) = state
                    .1
                    .wait_timeout_while(locked_state, Duration::from_secs(1), |s| {
                        s.screen_result.is_none()
                    })
                    .unwrap();
                assert!(!wait.timed_out());
                drop(ready_state);
                *locked(&gate.0) = false;
            }
            let epoch = h.engine.current_epoch();
            h.force();
            assert_eq!(h.engine.current_epoch(), epoch);
            if !completed_before_submit {
                *locked(&gate.0) = true;
                gate.1.notify_all();
            }
            // force() also ticks the owner, which may already have consumed a
            // completed handoff. Only wait on the queue while it is unconsumed.
            if h.worker.borrow().submitted.is_empty() {
                let locked_state = locked(&state.0);
                let (locked_state, wait) = state
                    .1
                    .wait_timeout_while(locked_state, Duration::from_secs(1), |s| {
                        s.results.is_empty()
                    })
                    .unwrap();
                assert!(!wait.timed_out());
                drop(locked_state);
            }
            h.engine.tick();
            let submitted = h.worker.borrow();
            assert_eq!(submitted.submitted.len(), 1);
            assert!(submitted.submitted[0].3.context.contains("Friday"));
            drop(submitted);
            h.response("ready", "", false);
            assert_eq!(h.worker.borrow().proceeds, vec![1]);
            assert!(
                receive.try_recv().is_err(),
                "handoff repeated the native read"
            );
            *locked(&gate.0) = true;
            gate.1.notify_all();
            h.engine.preparer.request_stop();
        }
    }
    #[test]
    fn focused_context_prefetch_handoff_refuses_stale_epoch_window_age_and_token() {
        for stale in ["epoch", "window", "age", "token"] {
            let stored = Arc::new(Mutex::new(Stored::default()));
            let storage: SharedStorage = Arc::new(Mutex::new(Box::new(Memory(stored))));
            let epoch = Arc::new(AtomicU64::new(1));
            let mut service =
                AsyncPreparer::new(storage, epoch.clone(), Arc::new(SyntheticSources)).unwrap();
            let snapshot = field("Thank you for the launch review");
            service.prefetch(ContextPreparation {
                token: 1,
                epoch: 1,
                snapshot: snapshot.clone(),
                config: preparation(snapshot.clone(), 1, 1, None).config,
                ticket: Box::new(ContextTicket {
                    snapshot: snapshot.clone(),
                    gate: None,
                    entered: None,
                    text: "OLD_PRIVATE_REFERENCE".into(),
                }),
            });
            let state = locked(&service.state.0);
            let (state, wait) = service
                .state
                .1
                .wait_timeout_while(state, Duration::from_secs(1), |s| s.screen_result.is_none())
                .unwrap();
            assert!(!wait.timed_out());
            drop(state);
            let mut next = snapshot;
            match stale {
                "window" => next.window_id = "another-window".into(),
                "epoch" => epoch.store(2, Ordering::Release),
                "age" => {
                    locked(&service.state.0).screen_result.as_mut().unwrap().3 =
                        Instant::now() - Duration::from_secs(3);
                }
                "token" => {
                    locked(&service.state.0).screen_result.as_mut().unwrap().0 = 99;
                }
                _ => unreachable!(),
            }
            let next_epoch = epoch.load(Ordering::Acquire);
            service.submit(preparation(
                next.clone(),
                2,
                next_epoch,
                Some(Box::new(ContextTicket {
                    snapshot: next,
                    gate: None,
                    entered: None,
                    text: "The launch review confirmed Monday's release.".into(),
                })),
            ));
            let state = locked(&service.state.0);
            let (mut state, wait) = service
                .state
                .1
                .wait_timeout_while(state, Duration::from_secs(1), |s| s.results.is_empty())
                .unwrap();
            assert!(!wait.timed_out());
            let prepared = state.results.pop_front().unwrap();
            drop(state);
            assert_eq!(prepared.token, 2);
            assert!(!prepared.options.context.contains("OLD_PRIVATE_REFERENCE"));
            assert!(prepared.options.context.contains("Monday"));
            service.request_stop();
        }
    }
    #[test]
    fn focused_context_prefetch_handoff_only_delivers_to_latest_consumer() {
        let stored = Arc::new(Mutex::new(Stored::default()));
        let storage: SharedStorage = Arc::new(Mutex::new(Box::new(Memory(stored))));
        let epoch = Arc::new(AtomicU64::new(1));
        let mut service = AsyncPreparer::new(storage, epoch, Arc::new(SyntheticSources)).unwrap();
        let snapshot = field("Thank you for the launch review");
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (entered, receive) = mpsc::sync_channel(1);
        let ticket = ContextTicket {
            snapshot: snapshot.clone(),
            gate: Some(gate.clone()),
            entered: Some(entered),
            text: "The launch review confirmed Friday's release.".into(),
        };
        service.prefetch(ContextPreparation {
            token: 1,
            epoch: 1,
            snapshot: snapshot.clone(),
            config: preparation(snapshot.clone(), 1, 1, None).config,
            ticket: Box::new(ticket.clone()),
        });
        receive.recv_timeout(Duration::from_secs(1)).unwrap();
        service.submit(preparation(
            snapshot.clone(),
            2,
            1,
            Some(Box::new(ticket.clone())),
        ));
        service.submit(preparation(snapshot, 3, 1, Some(Box::new(ticket))));
        *locked(&gate.0) = true;
        gate.1.notify_all();
        let state = locked(&service.state.0);
        let (state, wait) = service
            .state
            .1
            .wait_timeout_while(state, Duration::from_secs(1), |s| s.results.is_empty())
            .unwrap();
        assert!(!wait.timed_out());
        drop(state);
        let mut prepared = service.poll();
        assert_eq!(prepared.len(), 1);
        let prepared = prepared.pop().unwrap();
        assert_eq!(prepared.token, 3);
        assert!(prepared.options.context.contains("Friday"));
        assert!(service.poll().is_empty());
        assert!(
            receive.try_recv().is_err(),
            "latest consumer repeated collection"
        );
        service.request_stop();
    }
    #[test]
    fn focused_context_prefetch_handoff_cancellation_never_releases_old_reference() {
        let stored = Arc::new(Mutex::new(Stored::default()));
        let storage: SharedStorage = Arc::new(Mutex::new(Box::new(Memory(stored))));
        let epoch = Arc::new(AtomicU64::new(1));
        let mut service =
            AsyncPreparer::new(storage, epoch.clone(), Arc::new(SyntheticSources)).unwrap();
        let snapshot = field("Thank you for the launch review");
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (entered, receive) = mpsc::sync_channel(1);
        let ticket = ContextTicket {
            snapshot: snapshot.clone(),
            gate: Some(gate.clone()),
            entered: Some(entered),
            text: "The launch review confirmed Friday's release.".into(),
        };
        service.prefetch(ContextPreparation {
            token: 1,
            epoch: 1,
            snapshot: snapshot.clone(),
            config: preparation(snapshot.clone(), 1, 1, None).config,
            ticket: Box::new(ticket.clone()),
        });
        receive.recv_timeout(Duration::from_secs(1)).unwrap();
        service.submit(preparation(snapshot.clone(), 2, 1, Some(Box::new(ticket))));
        epoch.store(2, Ordering::Release);
        service.cancel();
        service.submit(preparation(snapshot, 3, 2, None));
        let state = locked(&service.state.0);
        let (state, wait) = service
            .state
            .1
            .wait_timeout_while(state, Duration::from_secs(1), |s| s.results.is_empty())
            .unwrap();
        assert!(!wait.timed_out());
        drop(state);
        let mut results = service.poll();
        assert_eq!(results.len(), 1);
        let prepared = results.pop().unwrap();
        assert_eq!(prepared.token, 3);
        assert!(prepared.options.context.is_empty());
        *locked(&gate.0) = true;
        gate.1.notify_all();
        service.request_stop();
    }
    #[test]
    fn focused_context_prefetch_handoff_revalidates_consumer_authority() {
        struct RevokedTicket;
        impl platform::DetachedContextRead for RevokedTicket {
            fn matches_snapshot(&self, _: &Snapshot) -> bool {
                true
            }
            fn matches(&self) -> Result<bool> {
                Ok(false)
            }
            fn read(&self, _: &dyn Fn() -> bool, _: usize) -> Result<ContextRead> {
                panic!("revoked consumer must not collect")
            }
        }
        let stored = Arc::new(Mutex::new(Stored::default()));
        let storage: SharedStorage = Arc::new(Mutex::new(Box::new(Memory(stored))));
        let epoch = Arc::new(AtomicU64::new(1));
        let mut service = AsyncPreparer::new(storage, epoch, Arc::new(SyntheticSources)).unwrap();
        let snapshot = field("Thank you for the launch review");
        service.prefetch(ContextPreparation {
            token: 1,
            epoch: 1,
            snapshot: snapshot.clone(),
            config: preparation(snapshot.clone(), 1, 1, None).config,
            ticket: Box::new(ContextTicket {
                snapshot: snapshot.clone(),
                gate: None,
                entered: None,
                text: "The launch review confirmed Friday's release.".into(),
            }),
        });
        let state = locked(&service.state.0);
        let (state, wait) = service
            .state
            .1
            .wait_timeout_while(state, Duration::from_secs(1), |s| s.screen_result.is_none())
            .unwrap();
        assert!(!wait.timed_out());
        drop(state);
        service.submit(preparation(snapshot, 2, 1, Some(Box::new(RevokedTicket))));
        let state = locked(&service.state.0);
        let (mut state, wait) = service
            .state
            .1
            .wait_timeout_while(state, Duration::from_secs(1), |s| s.results.is_empty())
            .unwrap();
        assert!(!wait.timed_out());
        let prepared = state.results.pop_front().unwrap();
        drop(state);
        assert_eq!(
            prepared.context_diagnostic.screen,
            ContextCollectorOutcome::Stale
        );
        assert!(prepared.options.context.is_empty());
        service.request_stop();
    }
    #[test]
    fn personalization_post_authorization_releases_store_and_refuses_revoked_learning() {
        use std::sync::atomic::AtomicUsize;

        struct PostAuthorization {
            snapshot: Snapshot,
            calls: Arc<AtomicUsize>,
            entered: SyncSender<()>,
            decision: Arc<(Mutex<Option<bool>>, Condvar)>,
        }
        impl platform::DetachedContextRead for PostAuthorization {
            fn matches_snapshot(&self, snapshot: &Snapshot) -> bool {
                self.snapshot.same_key(snapshot) && self.snapshot.same_authority(snapshot)
            }
            fn matches(&self) -> Result<bool> {
                if self.calls.fetch_add(1, Ordering::AcqRel) == 0 {
                    return Ok(true);
                }
                let _ = self.entered.try_send(());
                let (decision, wait) = self
                    .decision
                    .1
                    .wait_timeout_while(
                        locked(&self.decision.0),
                        Duration::from_secs(3),
                        |decision| decision.is_none(),
                    )
                    .unwrap();
                ensure!(
                    !wait.timed_out(),
                    "synthetic post-authorization was not released"
                );
                Ok(decision.unwrap())
            }
            fn read(&self, _: &dyn Fn() -> bool, _: usize) -> Result<ContextRead> {
                panic!("personalization authorization must not read screen or clipboard")
            }
        }

        const TEXT: &str = "We discussed observability and instrumentation during the planning meeting. The observability review needs more instrumentation examples before the next team meeting.";
        for revocation in ["none", "ticket", "epoch"] {
            let directory = tempfile::tempdir().unwrap();
            let mut history =
                HistoryStore::with_path_and_key(directory.path().join("local.sqlite3"), [b'a'; 32]);
            history.unlock().unwrap();
            assert!(
                history
                    .record_identified_at(
                        "synthetic-authored",
                        "writer",
                        "",
                        TEXT,
                        &[(0, TEXT.chars().count())],
                        chrono::Utc::now(),
                    )
                    .unwrap()
            );
            let expected = history
                .personalization("writer", "", TEXT, 50, &|_, _| true)
                .unwrap();
            assert!(
                !expected.is_empty(),
                "fixture must contribute real learned features"
            );
            let storage: SharedStorage = Arc::new(Mutex::new(Box::new(history)));
            let epoch = Arc::new(AtomicU64::new(1));
            let mut service = AsyncPreparer::new(
                storage.clone(),
                epoch.clone(),
                Arc::new(NativeBackgroundReaders),
            )
            .unwrap();
            let decision = Arc::new((Mutex::new(None), Condvar::new()));
            let calls = Arc::new(AtomicUsize::new(0));
            let (entered, receive) = mpsc::sync_channel(1);
            let snapshot = field(TEXT);
            let mut request = preparation(snapshot.clone(), 1, 1, None);
            request.config.record_history = true;
            request.config.personalization_strength = 50;
            request.config.screen_context = false;
            request.config.screenshot_context = false;
            request.config.clipboard_context = false;
            let config = request.config.clone();
            request.authorization_ticket = Some(Box::new(PostAuthorization {
                snapshot: snapshot.clone(),
                calls: calls.clone(),
                entered,
                decision: decision.clone(),
            }));
            service.submit(request);
            receive.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!(calls.load(Ordering::Acquire), 2);

            // Exercise a real encrypted history flush while the preparation's
            // native-authorization boundary is deliberately still blocked.
            let concurrent_flush = match storage.try_lock() {
                Ok(mut store) => {
                    store
                        .record(
                            "synthetic-parallel-flush",
                            "writer",
                            "",
                            TEXT,
                            &[(0, TEXT.chars().count())],
                        )
                        .unwrap();
                    true
                }
                Err(std::sync::TryLockError::WouldBlock) => false,
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    panic!("synthetic history storage was poisoned")
                }
            };
            if revocation == "epoch" {
                epoch.store(2, Ordering::Release);
                service.cancel();
                let mut latest = preparation(snapshot.clone(), 2, 2, None);
                latest.config = config;
                latest.authorization_ticket = Some(Box::new(ContextTicket {
                    snapshot,
                    gate: None,
                    entered: None,
                    text: String::new(),
                }));
                service.submit(latest);
            }
            *locked(&decision.0) = Some(revocation != "ticket");
            decision.1.notify_all();
            let (state, wait) = service
                .state
                .1
                .wait_timeout_while(locked(&service.state.0), Duration::from_secs(2), |state| {
                    state.results.is_empty()
                })
                .unwrap();
            assert!(!wait.timed_out());
            drop(state);
            let results = service.poll();
            service.request_stop();
            drop(service);
            assert!(
                concurrent_flush,
                "post-authorization validation held SharedStorage across its native wait"
            );
            assert_eq!(results.len(), 1);
            let prepared = &results[0];
            assert_eq!(prepared.token, if revocation == "epoch" { 2 } else { 1 });
            match revocation {
                "none" => assert_eq!(prepared.options.personalization, expected),
                "ticket" => assert!(prepared.options.personalization.is_empty()),
                "epoch" => {
                    assert_eq!(prepared.epoch, 2);
                    assert!(!prepared.options.personalization.is_empty());
                }
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn obsolete_blocked_window_reader_does_not_hold_latest_preparation_or_reuse_reference() {
        let stored = Arc::new(Mutex::new(Stored::default()));
        let storage: SharedStorage = Arc::new(Mutex::new(Box::new(Memory(stored))));
        let epoch = Arc::new(AtomicU64::new(1));
        let mut service =
            AsyncPreparer::new(storage, epoch.clone(), Arc::new(SyntheticSources)).unwrap();
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (entered, receive) = mpsc::sync_channel(1);
        let snapshot = field("Thank you for the launch review");
        service.submit(preparation(
            snapshot.clone(),
            1,
            1,
            Some(Box::new(ContextTicket {
                snapshot: snapshot.clone(),
                gate: Some(gate.clone()),
                entered: Some(entered),
                text: "OLD_PRIVATE_REFERENCE".into(),
            })),
        ));
        receive.recv_timeout(Duration::from_secs(1)).unwrap();
        epoch.store(2, Ordering::Release);
        service.cancel();
        service.submit(preparation(snapshot, 2, 2, None));
        let state = locked(&service.state.0);
        let (mut state, wait) = service
            .state
            .1
            .wait_timeout_while(state, Duration::from_secs(1), |s| s.results.is_empty())
            .unwrap();
        assert!(!wait.timed_out());
        let latest = state.results.pop_front().unwrap();
        drop(state);
        assert_eq!(latest.token, 2);
        assert!(latest.options.context.is_empty());
        *locked(&gate.0) = true;
        gate.1.notify_all();
        service.request_stop();
        drop(service);
    }
    #[test]
    fn screen_wait_diagnostic_distinguishes_timeout_from_unmatched_token_without_readiness() {
        for unmatched_token in [false, true] {
            let stored = Arc::new(Mutex::new(Stored::default()));
            let storage: SharedStorage = Arc::new(Mutex::new(Box::new(Memory(stored))));
            let epoch = Arc::new(AtomicU64::new(1));
            let mut service =
                AsyncPreparer::new(storage, epoch, Arc::new(SyntheticSources)).unwrap();
            let gate = Arc::new((Mutex::new(false), Condvar::new()));
            let (entered, receive) = mpsc::sync_channel(1);
            let snapshot = field("Thank you for the launch review");
            service.submit(preparation(
                snapshot.clone(),
                1,
                1,
                Some(Box::new(ContextTicket {
                    snapshot,
                    gate: Some(gate.clone()),
                    entered: Some(entered),
                    text: "SYNTHETIC_PRIVATE_REFERENCE".into(),
                })),
            ));
            receive.recv_timeout(Duration::from_secs(1)).unwrap();
            if unmatched_token {
                locked(&service.state.0).screen_result = Some((
                    99,
                    1,
                    ContextRead {
                        text: "SYNTHETIC_PRIVATE_WRONG_TOKEN".into(),
                        status: "ready".into(),
                        source: "accessibility".into(),
                    },
                    Instant::now(),
                ));
                service.state.1.notify_all();
            }
            let state = locked(&service.state.0);
            let (mut state, wait) = service
                .state
                .1
                .wait_timeout_while(state, Duration::from_secs(1), |s| s.results.is_empty())
                .unwrap();
            assert!(!wait.timed_out());
            let prepared = state.results.pop_front().unwrap();
            drop(state);
            *locked(&gate.0) = true;
            gate.1.notify_all();
            service.request_stop();
            assert!(prepared.options.context.is_empty());
            assert_eq!(prepared.context_status, "unavailable");
            assert_eq!(
                prepared.context_diagnostic.screen,
                if unmatched_token {
                    ContextCollectorOutcome::TokenStale
                } else {
                    ContextCollectorOutcome::TimedOut
                }
            );
            assert_eq!(
                prepared.context_diagnostic.attachment,
                ContextAttachmentOutcome::NoReferences
            );
        }
    }
    #[test]
    fn history_and_statistics_commands_return_results_without_enabling_recording() {
        let mut h = harness(Config::default(), None);
        h.engine.command(Command::HistoryInfo);
        h.engine.command(Command::HistoryCounts { unlock: true });
        h.engine.command(Command::Statistics {
            period: "week".into(),
            group_by: "day".into(),
        });
        h.engine.command(Command::ClearStatistics);
        let events = h.engine.drain_events();
        for kind in [
            "history_info",
            "history_counts",
            "statistics",
            "statistics_cleared",
        ] {
            assert!(events.iter().any(|e| e["type"] == kind));
        }
        assert!(!h.engine.config.record_history);
        assert!(!locked(&h.storage).unlocked);
    }
    #[test]
    fn withdrawn_history_and_statistics_consent_filter_already_queued_acceptances() {
        let mut c = Config::default();
        c.record_history = true;
        let mut h = harness(c, None);
        h.offer(" ready");
        h.drawn(" ready", "completion");
        h.engine.command(Command::Action(Action::All));
        let mut c = h.engine.config.clone();
        c.record_history = false;
        c.statistics_enabled = false;
        h.engine.command(Command::ApplyConfig {
            config: c,
            persist: false,
        });
        h.engine.tick();
        assert!(locked(&h.storage).records.is_empty());
        assert!(locked(&h.storage).stats.is_empty());
    }
    #[test]
    fn uncertain_generated_observation_never_becomes_authored_learning() {
        let mut c = Config::default();
        c.record_history = true;
        c.record_unaccepted = true;
        let mut h = harness(c, None);
        h.observe();
        h.native.borrow_mut().snapshot = Some(field("The update X"));
        h.observe();
        h.offer(" ready");
        h.drawn(" ready", "completion");
        h.native.borrow_mut().outcome = Outcome::Indeterminate;
        h.engine.command(Command::Action(Action::All));
        h.engine.tick();
        h.native.borrow_mut().snapshot = Some(field("The update X ready"));
        h.observe();
        h.engine.command(Command::Pause { minutes: 1 });
        let records = locked(&h.storage);
        assert_eq!(records.records.last().unwrap().3, vec![(10, 12)]);
        assert!(records.stats.is_empty());
    }
    #[test]
    fn dispatched_text_matching_later_user_typing_is_not_native_acceptance() {
        let mut h = harness(Config::default(), None);
        h.offer(" ready");
        h.drawn(" ready", "completion");
        h.native.borrow_mut().outcome = Outcome::Dispatched;
        h.engine.command(Command::Action(Action::All));
        h.engine.tick();
        {
            let mut n = h.native.borrow_mut();
            n.revision += 1;
            n.snapshot = Some(field("The update ready"));
        }
        h.observe();
        assert!(locked(&h.storage).stats.is_empty());
        assert_eq!(h.native.borrow().edits.len(), 1);
    }
    #[test]
    fn partial_remainder_uses_verified_advance_but_waits_for_current_geometry_before_another_edit()
    {
        let mut h = harness(Config::default(), None);
        h.offer(" arrives tomorrow");
        h.drawn(" arrives tomorrow", "completion");
        h.native.borrow_mut().outcome = Outcome::AppliedNoGeometry;
        h.engine.drain_events();
        h.engine.command(Command::Action(Action::Word));
        assert!(
            h.engine
                .drain_events()
                .iter()
                .any(|e| e["type"] == "suggestion"
                    && e["text"] == "tomorrow"
                    && e["advance"] == " arrives ")
        );
        h.drawn("tomorrow", "completion");
        assert!(h.invoke_hook(Action::Word));
        h.engine.tick();
        assert_eq!(h.native.borrow().edits.len(), 1);
        assert_eq!(h.native.borrow().finished, vec![false]);
        assert_eq!(locked(&h.storage).stats.len(), 1);
    }
    #[test]
    fn native_startup_failure_retains_settings_and_shutdown_then_explicit_retry_recovers() {
        let mut h = harness_start(Config::default(), None, 1);
        assert_eq!(h.engine.status()["state"], "error");
        h.engine.tick();
        assert_eq!(h.native.borrow().capture_count, 0);
        assert!(!h.engine.drain_events().iter().any(|e| e["type"] == "error"));
        h.engine.command(Command::Action(Action::Settings));
        assert!(
            h.engine
                .drain_events()
                .iter()
                .any(|e| e["type"] == "settings")
        );
        assert!(!h.native.borrow().shortcuts);
        h.engine.command(Command::RetryNative);
        h.observe();
        assert_eq!(h.engine.status()["native_started"], true);
        assert!(h.native.borrow().capture_count > 0);
        assert_eq!(h.native.borrow().start_calls, 2);
        h.worker.borrow_mut().stopped = true;
        h.preparation.borrow_mut().stopped = true;
        assert!(h.engine.shutdown_step().unwrap());
    }
    #[test]
    fn unavailable_factory_keeps_settings_data_and_owned_preview_without_modal_startup_error() {
        let mut config = Config::default();
        config.typo_suppression = false;
        config.typo_fixes = false;
        let native = native_state(&config, 0);
        let calls = Rc::new(RefCell::new(0usize));
        let attempts = calls.clone();
        let reason = "Wayland input method is not available; select the input method and retry.";
        let factory: NativeFactory = Box::new(move |_| {
            *attempts.borrow_mut() += 1;
            Err(anyhow!(reason))
        });
        let h = harness_native(config, None, native, Some(factory));
        let mut controller = Controller::with_engine(h.engine).unwrap();
        assert_eq!(controller.status()["native_available"], false);
        assert_eq!(controller.status()["native_started"], false);
        assert_eq!(controller.status()["state"], "error");
        assert_eq!(controller.status()["message"], reason);
        assert_eq!(controller.status()["native_status"]["message"], reason);
        assert_eq!(controller.status()["native_unavailable_reason"], reason);
        assert!(controller.native_event_source().is_none());
        assert!(
            !controller
                .present_native("missing", "hidden", false)
                .unwrap()
        );
        assert!(
            !controller
                .show_native_menu(vec![("settings".into(), "Settings".into(), true)])
                .unwrap()
        );
        controller.hide_native().unwrap();
        for _ in 0..3 {
            *h.time.borrow_mut() += Duration::from_millis(200);
            let events = controller.poll();
            assert!(!events.iter().any(|e| e["type"] == "error"));
        }
        assert_eq!(*calls.borrow(), 1);
        assert_eq!(h.native.borrow().capture_count, 0);
        for action in [
            Action::Force,
            Action::All,
            Action::Alternatives,
            Action::Synonyms,
        ] {
            controller.send(Command::Action(action)).unwrap();
        }
        controller.send(Command::Action(Action::Settings)).unwrap();
        controller.send(Command::Action(Action::Quit)).unwrap();
        controller.send(Command::HistoryInfo).unwrap();
        controller
            .send(Command::HistoryCounts { unlock: false })
            .unwrap();
        controller
            .send(Command::Statistics {
                period: "all".into(),
                group_by: "app".into(),
            })
            .unwrap();
        let events = controller.poll();
        for kind in [
            "settings",
            "quit",
            "history_info",
            "history_counts",
            "statistics",
        ] {
            assert!(events.iter().any(|e| e["type"] == kind));
        }
        assert_eq!(
            events
                .iter()
                .filter(|e| e["type"] == "action_result" && e["handled"] == false)
                .count(),
            4
        );
        assert_eq!(h.native.borrow().capture_count, 0);
        assert!(h.native.borrow().edits.is_empty());
        assert!(h.worker.borrow().submitted.is_empty());
        let mut updated = controller.config();
        updated.statistics_enabled = false;
        controller
            .send(Command::ApplyConfig {
                config: updated,
                persist: false,
            })
            .unwrap();
        controller.poll();
        assert!(!controller.config().statistics_enabled);
        assert_eq!(*calls.borrow(), 2);
        assert_eq!(controller.status()["native_unavailable_reason"], reason);
        controller.send(Command::SuspendForPreview).unwrap();
        controller.poll();
        assert_eq!(controller.status()["preview_suspended"], true);
        let preview_worker = Rc::new(RefCell::new(WorkerState::default()));
        let mut preview = crate::preview::PreviewSession::with_worker(
            controller.config(),
            Box::new(Inference(preview_worker.clone())),
            Box::new(Time(h.time.clone())),
        )
        .unwrap();
        preview.set_owner_released(controller.status()["preview_suspended"].as_bool().unwrap());
        preview.update("Dear team", 9, 0).unwrap();
        *h.time.borrow_mut() += Duration::from_secs(1);
        preview.poll();
        assert_eq!(preview_worker.borrow().submitted.len(), 1);
        let options = preview_worker.borrow().submitted[0].3.clone();
        assert!(options.context.is_empty() && options.personalization.is_empty());
        preview_worker.borrow_mut().events.extend([
            WorkerEvent {
                id: 1,
                kind: "ready".into(),
                text: String::new(),
                final_: false,
                model_loaded: true,
                message: String::new(),
            },
            WorkerEvent {
                id: 1,
                kind: "result".into(),
                text: ", thank you".into(),
                final_: true,
                model_loaded: true,
                message: String::new(),
            },
        ]);
        assert_eq!(preview.poll()["suggestion"], ", thank you");
        assert_eq!(preview.accept_visible(true, ", thank you"), ", thank you");
        preview.update("Dear team, thank you", 20, 0).unwrap();
        assert_eq!(preview.poll()["accepted"], 1);
        preview_worker.borrow_mut().stopped = true;
        assert!(preview.close().unwrap());
        {
            let saved = locked(&h.storage);
            assert!(saved.records.is_empty() && saved.stats.is_empty());
        }
        assert_eq!(h.native.borrow().capture_count, 0);
        assert_eq!(*calls.borrow(), 2);
        assert!(!controller.shutdown().unwrap());
        h.worker.borrow_mut().stopped = true;
        h.preparation.borrow_mut().stopped = true;
        assert!(controller.shutdown().unwrap());
        assert_eq!(h.native.borrow().stop_calls, 0);
        assert_eq!(*calls.borrow(), 2);
    }
    #[test]
    fn explicit_factory_retry_restores_retained_ui_binding_only_then_arms_real_hooks() {
        let mut config = Config::default();
        config.typo_suppression = false;
        config.typo_fixes = false;
        let native = native_state(&config, 1);
        let result = native.clone();
        let calls = Rc::new(RefCell::new(0usize));
        let attempts = calls.clone();
        let factory: NativeFactory = Box::new(move |_| {
            *attempts.borrow_mut() += 1;
            if *attempts.borrow() == 1 {
                Err(anyhow!("No supported native input service."))
            } else {
                Ok(Box::new(Native(result.clone())))
            }
        });
        let h = harness_native(config, None, native, Some(factory));
        let mut controller = Controller::with_engine(h.engine).unwrap();
        let (sender, receiver) = mpsc::channel();
        controller.bind_native_ui(Some(Arc::new(move |action| {
            let _ = sender.send(action);
        })));
        assert!(h.native.borrow().ui_callback.is_none());
        assert!(!controller.poll().iter().any(|e| e["type"] == "error"));
        assert_eq!(*calls.borrow(), 1);
        controller.send(Command::RetryNative).unwrap();
        controller.poll();
        assert_eq!(*calls.borrow(), 2);
        assert_eq!(controller.status()["native_available"], true);
        assert_eq!(controller.status()["native_started"], false);
        assert_eq!(h.native.borrow().capture_count, 0);
        assert!(!h.native.borrow().shortcuts);
        assert!(h.native.borrow().wake.is_some());
        h.native.borrow().ui_callback.clone().unwrap()("settings".into());
        assert_eq!(receiver.try_recv().unwrap(), "settings");
        assert!(
            !controller
                .present_native("missing", "hidden", false)
                .unwrap()
        );
        controller.send(Command::RetryNative).unwrap();
        controller.poll();
        assert_eq!(*calls.borrow(), 2);
        assert_eq!(controller.status()["native_started"], true);
        assert_eq!(controller.status()["native_unavailable_reason"], "");
        assert!(h.native.borrow().capture_count > 0);
        assert!(h.native.borrow().shortcuts);
        assert_eq!(h.native.borrow().start_calls, 2);
        assert_eq!(
            h.native.borrow().configured.global_enabled,
            controller.config().global_enabled
        );
        h.worker.borrow_mut().stopped = true;
        h.preparation.borrow_mut().stopped = true;
        assert!(controller.shutdown().unwrap());
        assert!(h.native.borrow().ui_callback.is_none());
        assert_eq!(h.native.borrow().stop_calls, 1);
    }
    #[test]
    fn failed_policy_reconfiguration_never_reads_under_the_previous_policy() {
        let mut h = harness(Config::default(), None);
        h.native.borrow_mut().configure_failures = 2;
        h.engine.command(Command::ApplyConfig {
            config: h.engine.config.clone(),
            persist: false,
        });
        h.engine.tick();
        assert_eq!(h.engine.status()["state"], "error");
        assert_eq!(h.native.borrow().capture_count, 0);
        assert!(!h.native.borrow().shortcuts);
        h.engine.command(Command::RetryNative);
        h.observe();
        assert!(h.native.borrow().capture_count > 0);
    }
}
