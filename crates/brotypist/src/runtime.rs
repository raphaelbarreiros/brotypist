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

mod config_apply;
mod native_client;
use crate::config::ScopedPreference;
pub use crate::diagnostics::PresentationLane;
use crate::diagnostics::{
    CaptureTiming, DiagnosticBook, DiagnosticSnapshot, Outcome, Stage, TerminalReport, WorkKind,
};
use crate::process_client::ReleaseTicket;
use crate::ui::settings::SettingsDraft;
pub use config_apply::{
    ConfigApplyReceipt, ConfigApplyStage, ConfigOperationId, PendingConfigApply,
};

#[derive(Default)]
struct WakeSignal {
    callback: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    pending: AtomicBool,
}
impl WakeSignal {
    fn signal(&self) {
        if !self.pending.swap(true, Ordering::AcqRel) {
            let callback = locked(&self.callback).clone();
            if let Some(callback) = callback {
                callback();
            }
        }
    }
    fn rearm(&self) {
        self.pending.store(false, Ordering::Release);
    }
    fn take(&self) -> bool {
        self.pending.swap(false, Ordering::AcqRel)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PaintObservation {
    pub trace_id: u64,
    pub lane: PresentationLane,
    pub view_serial: u64,
    pub painted_at: Instant,
}
pub type PaintReporter = Arc<dyn Fn(PaintObservation) + Send + Sync>;

#[derive(Clone, Debug)]
pub enum Command {
    Action(Action),
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
    fn release(&mut self) -> Result<ReleaseTicket>;
    fn release_complete(&self, _ticket: ReleaseTicket) -> bool {
        false
    }
    fn prepare_for_validation(
        &mut self,
        _config: &Config,
        _ticket: ReleaseTicket,
    ) -> Result<std::num::NonZeroU64> {
        Err(anyhow!("Fresh provider validation is unavailable."))
    }
    fn set_wakeup(&mut self, _callback: Option<Arc<dyn Fn() + Send + Sync>>) {}
    fn take_terminal_report(&mut self) -> Option<TerminalReport> {
        None
    }
    fn lost_terminal_reports(&self) -> u64 {
        0
    }
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
    fn release(&mut self) -> Result<ReleaseTicket> {
        WorkerClient::release(self)
    }
    fn release_complete(&self, ticket: ReleaseTicket) -> bool {
        WorkerClient::release_complete(self, ticket)
    }
    fn prepare_for_validation(
        &mut self,
        config: &Config,
        ticket: ReleaseTicket,
    ) -> Result<std::num::NonZeroU64> {
        WorkerClient::prepare_for_validation(self, config, ticket)
    }
    fn set_wakeup(&mut self, callback: Option<Arc<dyn Fn() + Send + Sync>>) {
        WorkerClient::set_wakeup(self, callback);
    }
    fn take_terminal_report(&mut self) -> Option<TerminalReport> {
        WorkerClient::take_terminal_report(self)
    }
    fn lost_terminal_reports(&self) -> u64 {
        WorkerClient::lost_terminal_reports(self)
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
    fn retain_since(
        &mut self,
        _cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<crate::storage::RetentionResult> {
        Err(anyhow!("Authenticated retention is unavailable."))
    }
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
    fn retain_since(
        &mut self,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<crate::storage::RetentionResult> {
        HistoryStore::retain_since(self, cutoff)
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
    config::normalize_domain(text).unwrap_or_default()
}
fn rules<'a>(
    c: &'a Config,
    app: &str,
    host: &str,
) -> impl Iterator<Item = &'a BTreeMap<String, Value>> {
    c.scoped_rules(app, host)
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
fn preference(c: &Config, kind: ScopedPreference, app: &str, host: &str) -> bool {
    c.resolve_preference(
        kind,
        app,
        host,
        platform::policy::is_browser(&[app]) && c.has_domain_denial(kind),
    )
    .value
}
fn history_use_allowed(c: &Config, app: &str, host: &str) -> bool {
    !app.is_empty()
        && !excluded(c, app, "")
        && preference(c, ScopedPreference::UseHistory, app, host)
        && rule_bool(c, app, host, "enabled", c.enabled)
}
fn history_access_configured(c: &Config) -> bool {
    recording_configured(c)
        || c.use_history
        || c.app_rules
            .values()
            .any(|r| r.get("use_history") == Some(&Value::Bool(true)))
}
fn scope_context(c: &mut Config, app: &str, host: &str, browser: bool) {
    let resolve = |kind| {
        c.resolve_preference(
            kind,
            app,
            host,
            (browser || platform::policy::is_browser(&[app])) && c.has_domain_denial(kind),
        )
        .value
    };
    let values = [
        resolve(ScopedPreference::ClipboardContext),
        resolve(ScopedPreference::ScreenContext),
        resolve(ScopedPreference::ScreenshotContext),
        resolve(ScopedPreference::ScreenshotAppearance),
    ];
    [
        c.clipboard_context,
        c.screen_context,
        c.screenshot_context,
        c.screenshot_appearance,
    ] = values;
}
fn recording_configured(c: &Config) -> bool {
    c.record_history
        || c.app_rules
            .values()
            .any(|r| r.get("record_history") == Some(&Value::Bool(true)))
}

pub trait Spelling: Send + Sync {
    fn assess(&self, snapshot: &Snapshot) -> Option<SpellingResult>;
    fn set_language(&self, language: &str) -> Result<()>;
    fn invalidate(&self);
    fn close(&self) -> bool;
    fn is_stopped(&self) -> bool;
}
impl Spelling for SpellingService {
    fn set_language(&self, language: &str) -> Result<()> {
        SpellingService::set_language(self, language)
    }
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
    fn clipboard(
        &self,
        live: Arc<dyn Fn() -> bool + Send + Sync>,
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
        live: Arc<dyn Fn() -> bool + Send + Sync>,
        authorized: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<String> {
        system::clipboard_text_shared(live, authorized)
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
    pub browser_apps: BTreeSet<String>,
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
    fn set_wakeup(&mut self, _callback: Option<Arc<dyn Fn() + Send + Sync>>) {}
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
    wake: Arc<WakeSignal>,
}
impl AsyncPreparer {
    pub fn new(
        storage: SharedStorage,
        epoch: Arc<AtomicU64>,
        readers: Arc<dyn BackgroundReaders>,
    ) -> Result<Self> {
        let wake = Arc::new(WakeSignal::default());
        let screen_wake = wake.clone();
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
                        screen_wake.signal();
                    }
                }
            })?;
        let shared = state.clone();
        let preparation_wake = wake.clone();
        let thread_result = thread::Builder::new().name("brotypist-writing-preparation".into()).spawn(move || loop {
            let request = {
                let mut s = locked(&shared.0);
                while s.pending.is_none() && !s.stopped { s = shared.1.wait(s).unwrap_or_else(std::sync::PoisonError::into_inner); }
                if s.stopped { break; } s.pending.take().expect("pending request")
            };
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
            let Ok(instructions) = c.resolve_instructions(&snapshot.app_id, &snapshot.domain,
                (request.browser_apps.contains(&snapshot.app_id) || platform::policy::is_browser(&[snapshot.app_id.as_str()]))
                    && c.app_rules.iter().any(|(key, rule)| key.starts_with("domain:") && rule.contains_key("custom_instructions"))) else { continue; };
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
            let source_allowed = |app: &str, host: &str| !request.hard_disabled_apps.contains(app)
                && history_use_allowed(c, app, host)
                && !(request.browser_apps.contains(app) && config::normalize_domain(host).is_err() && c.has_domain_denial(ScopedPreference::UseHistory));
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
            let options = GenerationOptions {
                instructions,
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
                preparation_wake.signal();
            }
        });
        match thread_result {
            Ok(thread) => Ok(Self {
                state,
                wake,
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
    fn set_wakeup(&mut self, callback: Option<Arc<dyn Fn() + Send + Sync>>) {
        *locked(&self.wake.callback) = callback;
        self.wake.rearm();
    }
    fn submit(&mut self, mut request: Preparation) {
        scope_context(
            &mut request.config,
            &request.snapshot.app_id,
            &request.snapshot.domain,
            request.browser_apps.contains(&request.snapshot.app_id),
        );
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
    fn prefetch(&mut self, mut request: ContextPreparation) {
        scope_context(
            &mut request.config,
            &request.snapshot.app_id,
            &request.snapshot.domain,
            false,
        );
        if !(request.config.screen_context || request.config.screenshot_context) {
            return;
        }
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
        self.wake.rearm();
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
    choices: features::ChoiceMap,
    evidence: PresentationEvidence,
}
#[derive(Clone, Copy)]
struct CompletionOffer {
    epoch: u64,
    revision: u64,
    serial: u64,
}
#[derive(Clone, Copy)]
enum ContinuationLifetime {
    RenewalUntil(Instant),
    WhatsAppField,
}
// Proof of an already verified edit, never authority to insert a future offer.
struct ContinuationGuard {
    snapshot: Snapshot,
    revision: u64,
    lifetime: ContinuationLifetime,
}
impl ContinuationGuard {
    fn valid_at(&self, now: Instant) -> bool {
        match self.lifetime {
            ContinuationLifetime::RenewalUntil(expires_at) => now < expires_at,
            ContinuationLifetime::WhatsAppField => true,
        }
    }
    fn is_field_reservation(&self) -> bool {
        matches!(self.lifetime, ContinuationLifetime::WhatsAppField)
    }
}
#[derive(Default)]
struct Armed {
    claims: Vec<Arc<Claim>>,
    origin: Option<Arc<(Snapshot, u64, u64)>>,
    continuation: Option<Arc<ContinuationGuard>>,
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
    continuation: Option<Arc<ContinuationGuard>>,
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
const PENDING_EDIT_CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(2);

enum CaptureObservation {
    Snapshot(Snapshot),
    Deferred {
        edit_at: Instant,
        revision: u64,
        epoch: u64,
    },
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
    trace_id: Option<u64>,
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
    intent_generation: u64,
}
impl ModelMigration {
    fn start(config: &Config, intent_generation: u64) -> Result<Self> {
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
            intent_generation,
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
#[derive(Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum DiagnosticNativeEditOutcome {
    Applied,
    Dispatched,
    Indeterminate,
    Rejected,
    ReadbackMismatch,
    Error,
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
#[derive(serde::Serialize)]
struct DiagnosticInferenceSummary {
    submitted_requests: Option<u64>,
    finished_requests: Option<u64>,
    no_offer_requests: Option<u64>,
    failed_requests: Option<u64>,
    cancelled_requests: Option<u64>,
}
#[derive(serde::Serialize)]
struct DiagnosticTerminalSummary {
    request_id: u64,
    no_offer: Option<bool>,
    cancelled: bool,
    failed: bool,
    reason: crate::diagnostics::TerminalReason,
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
    session_inference: Option<DiagnosticInferenceSummary>,
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
            session_inference: status
                .get("inference")
                .filter(|value| value.is_object())
                .map(|inference| DiagnosticInferenceSummary {
                    submitted_requests: inference.get("submitted_requests").and_then(Value::as_u64),
                    finished_requests: inference.get("finished_requests").and_then(Value::as_u64),
                    no_offer_requests: inference.get("no_offer_requests").and_then(Value::as_u64),
                    failed_requests: inference.get("failed_requests").and_then(Value::as_u64),
                    cancelled_requests: inference.get("cancelled_requests").and_then(Value::as_u64),
                }),
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
    confirmed_completion_edits: u64,
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
    uncertain_provenance_present: bool,
    uncertain_provenance_blocks_candidate: bool,
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
    last_request_terminal: Option<DiagnosticTerminalSummary>,
    last_screen_ticket_available: Option<bool>,
    last_authorization_ticket_available: Option<bool>,
    last_submission_mode: DiagnosticMode,
    last_ack_mode: DiagnosticMode,
    last_ack_visible: bool,
    last_ack_current: bool,
    last_ack_accepted: bool,
    last_completion_native_outcome: Option<DiagnosticNativeEditOutcome>,
    planned_completion_whitespace_only: Option<bool>,
    planned_completion_contains_tab: Option<bool>,
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
}
impl Controller {
    pub fn start(mut config: Config) -> Result<Self> {
        config.validate()?;
        let storage: SharedStorage = Arc::new(Mutex::new(Box::new(HistoryStore::open()?)));
        let epoch = Arc::new(AtomicU64::new(1));
        let preparer = AsyncPreparer::new(
            storage.clone(),
            epoch.clone(),
            Arc::new(NativeBackgroundReaders),
        )?;
        let spelling = (config.typo_suppression || config.typo_fixes).then(native_spelling);
        let mut engine = Engine::with_native_factory(
            config,
            Box::new(WorkerClient::new()?),
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
        Self::with_engine(engine)
    }
    pub fn with_engine(engine: Engine) -> Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(128);
        Ok(Self {
            engine: Rc::new(RefCell::new(engine)),
            sender,
            receiver,
        })
    }
    pub fn diagnostics(&self) -> DiagnosticSnapshot {
        locked(&self.engine.borrow().diagnostics).snapshot()
    }
    pub fn export_diagnostics(&self) -> Result<std::path::PathBuf> {
        system::export_request_diagnostics(&self.diagnostics())
    }
    pub fn paint_reporter(&self) -> PaintReporter {
        let book = self.engine.borrow().diagnostics.clone();
        Arc::new(move |observation| {
            locked(&book).record_draw(
                observation.trace_id,
                observation.lane,
                observation.view_serial,
                observation.painted_at,
            );
        })
    }
    pub fn set_wakeup(&self, callback: Option<Arc<dyn Fn() + Send + Sync>>) {
        let mut engine = self.engine.borrow_mut();
        *locked(&engine.wake.callback) = callback;
        engine.wake.rearm();
        let wake = engine.wake.clone();
        let callback: Arc<dyn Fn() + Send + Sync> = Arc::new(move || wake.signal());
        engine.worker.set_wakeup(Some(callback.clone()));
        engine.preparer.set_wakeup(Some(callback));
    }
    pub fn send(&self, command: Command) -> Result<()> {
        self.sender
            .try_send(command)
            .map_err(|_| anyhow!("Controller command queue is unavailable or full."))?;
        self.engine.borrow().wake.signal();
        Ok(())
    }
    pub fn poll(&self) -> Vec<Value> {
        let mut engine = self.engine.borrow_mut();
        engine.wake.rearm();
        engine.pump_client_confirmations();
        self.process_commands(&mut engine);
        engine.periodic_turn();
        engine.drain_events()
    }
    fn process_commands(&self, engine: &mut Engine) {
        // Bound each owner turn by the command queue's capacity.
        for command in self.receiver.try_iter().take(128) {
            if matches!(&command, Command::PresentationResult { .. }) && !engine.stopping {
                let fresh = engine.capture_observation();
                engine.observe_capture(fresh);
            }
            engine.command(command);
        }
    }
    /// Process renderer acknowledgements without scheduling or consuming publications.
    /// In particular, leave the worker/preparer's pending wake intact.
    pub fn flush_commands(&self) -> Vec<Value> {
        let mut engine = self.engine.borrow_mut();
        engine.pump_client_confirmations();
        self.process_commands(&mut engine);
        engine.drain_events()
    }
    /// Consume actionable publications without routine capture, prefetch, model
    /// preparation, power checks or debounce scheduling.
    pub fn poll_ready(&self) -> Vec<Value> {
        let mut engine = self.engine.borrow_mut();
        if !engine.wake.take() {
            return Vec::new();
        }
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
    continuation: Option<Arc<ContinuationGuard>>,
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
    config_apply: Option<config_apply::Operation>,
    config_retiring: Option<ReleaseTicket>,
    config_release_failed: bool,
    config_promoting: bool,
    config_integrity_cleanup: Vec<JoinHandle<Result<()>>>,
    model_intent_generation: u64,
    migration_apply: Option<PendingConfigApply>,
    wake: Arc<WakeSignal>,
    diagnostics: Arc<Mutex<DiagnosticBook>>,
    diagnostic_views: [u64; 2],
    diagnostic_traces: [Option<u64>; 2],
    last_capture: Option<CaptureTiming>,
    lost_terminal_reports: u64,
    retention_after: Instant,
    retention_pending: Option<(ReleaseTicket, chrono::DateTime<chrono::Utc>)>,
    retention_observation: Option<(String, Authority, String)>,
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
    /// explicit RetryNative/settings operation.
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
        if let Some(service) = &spelling {
            service.set_language(&config.spelling_language)?;
        }
        let now = clock.now();
        let armed = Arc::new(Mutex::new(Armed::default()));
        let input_pending = Arc::new(AtomicBool::new(false));
        let wake_signal = Arc::new(WakeSignal::default());
        let diagnostics = Arc::new(Mutex::new(DiagnosticBook::new()));
        let hook_diagnostics = diagnostics.clone();
        let hook_wake = wake_signal.clone();
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
                        && c.choices.original_index(*index).is_some()
                }),
                Action::Dismiss | Action::Bypass => primary_claim_index(&cache.claims, true),
                _ => None,
            };
            let claim = claim_index.map(|i| cache.claims[i].clone());
            let continuation = if action == Action::Word && claim.is_none() && cache.active {
                cache
                    .continuation
                    .as_ref()
                    .filter(|guard| guard.valid_at(Instant::now()))
                    .cloned()
            } else {
                None
            };
            let guarded = continuation.is_some();
            let mut origin = None;
            let consume = match action {
                Action::Word => claim.is_some() || guarded,
                Action::All | Action::Choice(_) => claim.is_some(),
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
                    continuation,
                    transaction,
                })
                .is_err()
            {
                return false;
            }
            if !guarded {
                cache.claims.clear();
                cache.continuation = None;
            }
            if matches!(action, Action::Dismiss | Action::Bypass) {
                hook_epoch.fetch_add(1, Ordering::AcqRel);
            }
            drop(cache);
            if !guarded {
                locked(&hook_diagnostics).invalidate_all_views();
            }
            hook_wake.signal();
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
        let notification_wake = wake_signal.clone();
        let input_diagnostics = diagnostics.clone();
        let input_callback: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            wake.store(true, Ordering::Release);
            input_epoch.fetch_add(1, Ordering::AcqRel);
            if let Ok(mut c) = cache.try_lock() {
                c.claims.clear();
                c.continuation = None;
            }
            locked(&input_diagnostics).invalidate_all_views();
            notification_wake.signal();
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
            continuation: None,
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
            config_apply: None,
            config_retiring: None,
            config_release_failed: false,
            config_promoting: false,
            config_integrity_cleanup: Vec::new(),
            model_intent_generation: 0,
            migration_apply: None,
            wake: wake_signal,
            diagnostics,
            diagnostic_views: [0, 0],
            diagnostic_traces: [None, None],
            last_capture: None,
            lost_terminal_reports: 0,
            retention_after: now,
            retention_pending: None,
            retention_observation: None,
            whatsapp_diagnostic: WhatsAppRuntimeDiagnostic::default(),
        };
        if history_access_configured(&engine.config) {
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
            state.uncertain_provenance_present = false;
            state.uncertain_provenance_blocks_candidate = false;
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
            uncertain_provenance_present: self
                .uncertain_provenance
                .as_ref()
                .is_some_and(|(authority, _)| authority.app_id == WHATSAPP_APP_ID),
            uncertain_provenance_blocks_candidate: candidate.is_some_and(|s| {
                self.uncertain_provenance
                    .as_ref()
                    .is_some_and(|(authority, revision)| {
                        authority_matches(authority, s) && Some(*revision) == self.user_revision()
                    })
            }),
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
            && self.config_apply.is_none()
            && self.config_retiring.is_none()
            && self.retention_pending.is_none()
            && !self.config_release_failed
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
    fn emit(&mut self, mut event: Value) {
        if event["type"] == "clear" {
            locked(&self.diagnostics).invalidate_all_views();
        }
        if event["type"] == "suggestion" {
            let lane = if matches!(event["mode"].as_str(), Some("completion" | "correction")) {
                PresentationLane::Inline
            } else {
                PresentationLane::Chooser
            };
            event["diagnostic"] = self.publish_diagnostic(lane);
            if lane == PresentationLane::Inline
                && event["choices"].as_array().is_some_and(|v| !v.is_empty())
            {
                event["choices_diagnostic"] = self.publish_diagnostic(PresentationLane::Chooser);
            }
        }
        if self.events.len() == 128 {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }
    fn begin_diagnostic(&mut self, kind: WorkKind, lane: PresentationLane) {
        let index = lane as usize;
        self.finish_diagnostic(lane, Outcome::Superseded);
        self.diagnostic_views[index] = self.diagnostic_views[index]
            .checked_add(1)
            .expect("diagnostic view exhausted");
        self.diagnostic_traces[index] = self.last_capture.and_then(|timing| {
            locked(&self.diagnostics).begin(kind, lane, self.diagnostic_views[index], timing)
        });
    }
    fn publish_diagnostic(&mut self, lane: PresentationLane) -> Value {
        let index = lane as usize;
        if self.diagnostic_traces[index].is_none() {
            self.begin_diagnostic(WorkKind::LocalTool, lane);
        }
        let Some(trace) = self.diagnostic_traces[index] else {
            return Value::Null;
        };
        self.diagnostic_views[index] = self.diagnostic_views[index]
            .checked_add(1)
            .expect("diagnostic view exhausted");
        let view = self.diagnostic_views[index];
        if locked(&self.diagnostics).publish(trace, lane, view, self.now()) {
            json!({"trace_id": trace, "lane": lane, "view_serial": view})
        } else {
            Value::Null
        }
    }
    fn diagnostic_stage(&self, mode: &str, stage: Stage) {
        let lane = if mode == "completion" {
            PresentationLane::Inline
        } else {
            PresentationLane::Chooser
        };
        if let Some(trace) = self.diagnostic_traces[lane as usize] {
            locked(&self.diagnostics).record_stage(trace, stage, self.now());
        }
    }
    fn finish_trace(&mut self, trace: Option<u64>, outcome: Outcome) {
        if let Some(trace) = trace {
            locked(&self.diagnostics).finish(trace, outcome, self.now());
            for current in &mut self.diagnostic_traces {
                if *current == Some(trace) {
                    *current = None;
                }
            }
        }
    }
    fn finish_diagnostic(&mut self, lane: PresentationLane, outcome: Outcome) {
        if let Some(trace) = self.diagnostic_traces[lane as usize].take() {
            // A native client may have committed before its confirmation arrives.
            // Revoking a view must not preempt that exact receipt's accounting.
            if !self.client_holds_trace(trace) {
                self.finish_trace(Some(trace), outcome);
            }
        }
    }
    fn finish_diagnostics(&mut self, outcome: Outcome) {
        self.finish_diagnostic(PresentationLane::Inline, outcome);
        self.finish_diagnostic(PresentationLane::Chooser, outcome);
        locked(&self.diagnostics).invalidate_all_views();
    }
    fn drain_diagnostic_reports(&mut self) {
        while let Some(report) = self.worker.take_terminal_report() {
            if !report.preparing
                && self.whatsapp_diagnostic.last_request_id == Some(report.request_id)
            {
                self.whatsapp_diagnostic.last_request_terminal = Some(DiagnosticTerminalSummary {
                    request_id: report.request_id,
                    no_offer: report.metrics.map(|metrics| metrics.no_offer),
                    cancelled: report.cancelled,
                    failed: report.failed,
                    reason: report.reason,
                });
            }
            locked(&self.diagnostics).attach_terminal(report);
        }
        let lost = self.worker.lost_terminal_reports();
        locked(&self.diagnostics)
            .note_lost_reports(lost.saturating_sub(self.lost_terminal_reports));
        self.lost_terminal_reports = lost;
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
            && self
                .config
                .resolve_preference(
                    ScopedPreference::RecordHistory,
                    app,
                    host,
                    self.browser_apps.contains(app)
                        && self
                            .config
                            .has_domain_denial(ScopedPreference::RecordHistory),
                )
                .value
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
        self.clear_continuation();
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
        match self.capture_observation()? {
            CaptureObservation::Snapshot(snapshot) => Some(snapshot),
            CaptureObservation::Deferred { .. } => None,
        }
    }
    fn capture_observation(&mut self) -> Option<CaptureObservation> {
        let started = self.now();
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
        let mut snapshot = match captured {
            Ok(Some(snapshot)) => snapshot,
            Ok(None) => return self.defer_pending_capture(revision),
            Err(_) => return None,
        };
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
            && !preference(
                &self.config,
                ScopedPreference::ScreenshotAppearance,
                &snapshot.app_id,
                &snapshot.domain,
            )
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
        let finished = self.now();
        let input_notified = self
            .bridge
            .as_ref()
            .and_then(|bridge| bridge.input_timestamp());
        self.last_capture = Some(CaptureTiming {
            input_notified: input_notified.filter(|at| *at <= started),
            started,
            finished,
        });
        for trace in self.diagnostic_traces.into_iter().flatten() {
            locked(&self.diagnostics).record_capture(trace, started, finished);
        }
        Some(CaptureObservation::Snapshot(snapshot))
    }
    fn pending_capture_current(&self, edit_at: Instant, revision: u64) -> bool {
        self.native_ready
            && self.active()
            && !self.shortcuts_suspended
            && self.secure_input() == Some(false)
            && self.user_revision() == Some(revision)
            && !self
                .bridge
                .as_ref()
                .is_some_and(|bridge| bridge.client_owns_actions())
            && self.pending_edit.as_ref().is_some_and(|edit| {
                edit.mode == "completion"
                    && edit.at == edit_at
                    && edit.revision == revision
                    && self.now() < edit.at + PENDING_EDIT_CONFIRMATION_TIMEOUT
            })
    }
    fn defer_pending_capture(&mut self, revision: u64) -> Option<CaptureObservation> {
        let edit_at = self.pending_edit.as_ref()?.at;
        if !self.pending_capture_current(edit_at, revision) {
            return None;
        }
        // Foreground/privacy refusal is not a transient text capture. The
        // backend additionally proves the exact posted field and native policy.
        self.foreground()?;
        if !self.pending_capture_current(edit_at, revision) {
            return None;
        }
        // A failed AX capture may itself advance the notification epoch. Fence
        // the fresh proof, not the epoch from before that failed capture.
        let epoch = self.current_epoch();
        let edit = self.pending_edit.as_ref()?;
        let proved = self
            .bridge
            .as_mut()?
            .pending_edit_capture_is_transient(&edit.before, revision);
        if !proved
            || !self.pending_capture_current(edit_at, revision)
            || self.current_epoch() != epoch
        {
            return None;
        }
        Some(CaptureObservation::Deferred {
            edit_at,
            revision,
            epoch,
        })
    }
    fn observe_capture(&mut self, observation: Option<CaptureObservation>) {
        match observation {
            Some(CaptureObservation::Snapshot(snapshot)) => self.observe(Some(snapshot)),
            Some(CaptureObservation::Deferred {
                edit_at,
                revision,
                epoch,
            }) if self.pending_capture_current(edit_at, revision)
                && self.current_epoch() == epoch => {}
            _ => self.observe(None),
        }
        // A deferred observation neither fabricates a candidate nor confirms an
        // edit. Normal captures continue, and pending_edit still blocks work.
    }
    fn disarm(&mut self) {
        self.claims.clear();
        if let Ok(mut a) = self.armed.try_lock() {
            a.claims.clear();
            a.continuation = None;
        }
    }
    fn clear_continuation(&mut self) {
        self.continuation = None;
        if let Ok(mut armed) = self.armed.try_lock() {
            armed.continuation = None;
        }
    }
    fn finish_continuation_work(&mut self) {
        if !self
            .continuation
            .as_ref()
            .is_some_and(|guard| guard.is_field_reservation())
        {
            self.clear_continuation();
        }
    }
    fn continuation_scope_matches(&self, guard: &ContinuationGuard, s: &Snapshot) -> bool {
        guard.valid_at(self.now())
            && Some(guard.revision) == self.user_revision()
            && guard.snapshot.same_key(s)
            && guard.snapshot.same_authority(s)
    }
    fn completion_eligible(&self, s: &Snapshot) -> bool {
        s.inline_safe()
            && s.eligible(self.config.min_chars as usize)
            && (self.config.complete_inside_text || s.line_suffix().trim().is_empty())
            && (self.config.completion_length != "word" || features::mid_word(s.prefix()))
    }
    fn continuation_allowed(&self, guard: &ContinuationGuard, s: &Snapshot) -> bool {
        if !self.active()
            || !self.native_ready
            || self.shortcuts_suspended
            || self.secure_input() != Some(false)
            || self
                .bridge
                .as_ref()
                .is_some_and(|bridge| bridge.client_owns_actions())
            || self.field_paused(s)
            || self.pending_edit.is_some()
            || !s.has_anchor()
            || self
                .uncertain_provenance
                .as_ref()
                .is_some_and(|(authority, revision)| {
                    authority_matches(authority, s) && Some(*revision) == self.user_revision()
                })
        {
            return false;
        }
        if guard.is_field_reservation() {
            // Verified WhatsApp Tab intent is independent of model work and
            // render geometry. The owner still requires this exact safe draft.
            return s.eligible(1);
        }
        if self.correction.is_some() {
            return false;
        }
        if !self.suggestion.text.is_empty()
            && self
                .suggestion
                .snapshot
                .as_ref()
                .is_some_and(|offered| offered.same_key(s) && offered.same_authority(s))
        {
            return s.eligible(1)
                && (s.inline_safe()
                    || self
                        .advance
                        .as_ref()
                        .is_some_and(|(key, authority, geometry, _)| {
                            key.matches(s)
                                && authority_matches(authority, s)
                                && geometry.inline_safe()
                        }));
        }
        if s.on_demand_only
            || !self.completion_eligible(s)
            || (self.on_battery && self.config.battery_on_demand)
        {
            return false;
        }
        if let Some(preparing) = &self.preparing {
            return preparing.mode == "completion"
                && preparing.lease.matches(s, self.current_epoch())
                && Some(preparing.lease.revision) == self.user_revision();
        }
        if let Some(request) = &self.request {
            return request.mode == "completion"
                && request.lease.matches(s, self.current_epoch())
                && Some(request.lease.revision) == self.user_revision();
        }
        !self.attempted(s, "completion")
    }
    fn consume_continuation(&mut self, guard: &Arc<ContinuationGuard>) -> bool {
        if !self
            .continuation
            .as_ref()
            .is_some_and(|owned| Arc::ptr_eq(owned, guard))
        {
            return false;
        }
        // Internal renewal handoffs may advance the epoch after admission while
        // explicitly retaining this same guard. Unlike a rendered Claim, it
        // authorizes no text: prove its exact scope again in the current epoch.
        let epoch = self.current_epoch();
        let current = self.capture();
        let valid = self
            .continuation
            .as_ref()
            .is_some_and(|owned| Arc::ptr_eq(owned, guard))
            && current.as_ref().is_some_and(|s| {
                self.continuation_scope_matches(guard, s) && self.continuation_allowed(guard, s)
            })
            && self.input_claim_valid()
            && self.current_epoch() == epoch;
        if !valid {
            self.clear_continuation();
        }
        // Do not disarm: a real paint may have arrived since this key was
        // admitted, and only a subsequent key may accept that new claim.
        valid
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
    fn clear_work(&mut self, hide: bool, cancel: bool) {
        self.clear_work_state(hide, cancel, false);
        if let Some(s) = &self.spelling {
            s.invalidate();
        }
    }
    fn clear_work_state(&mut self, hide: bool, cancel: bool, preserve_context: bool) {
        // Model turnover cannot revoke field-session intent. Scope, policy and
        // edit callers continue to use the strong clear/clear_state boundary.
        let reservation = self
            .continuation
            .take()
            .filter(|guard| guard.is_field_reservation());
        self.clear_state(hide, cancel, preserve_context);
        self.continuation = reservation.filter(|guard| {
            self.candidate.as_ref().is_some_and(|s| {
                self.continuation_scope_matches(guard, s) && self.continuation_allowed(guard, s)
            })
        });
    }
    fn clear_state(&mut self, hide: bool, cancel: bool, preserve_context: bool) {
        self.clear_continuation();
        self.finish_diagnostics(if cancel {
            Outcome::Cancelled
        } else {
            Outcome::Superseded
        });
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
        if let Some(edit) = self.pending_edit.take() {
            self.finish_trace(edit.trace_id, Outcome::Cancelled);
        }
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
    fn revoke_inflight(&mut self, hide: bool, current: Option<&Snapshot>) {
        let whatsapp = self.whatsapp_relevant();
        let reservation = self
            .continuation
            .take()
            .filter(|guard| guard.is_field_reservation());
        self.clear(hide, true);
        self.continuation = reservation.filter(|guard| {
            current.is_some_and(|s| {
                self.continuation_scope_matches(guard, s) && self.continuation_allowed(guard, s)
            })
        });
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
            .is_some_and(|(a, at)| authority_matches(a, s) && self.now() < *at)
    }
    fn observe(&mut self, snapshot: Option<Snapshot>) {
        let Some(snapshot) = snapshot else {
            self.clear_continuation();
            if self.candidate.is_some() || self.preparing.is_some() || self.request.is_some() {
                self.finish_session();
                self.clear(true, true);
            }
            self.candidate = None;
            self.attempted = None;
            if let Some(edit) = self.pending_edit.take() {
                self.finish_trace(edit.trace_id, Outcome::Cancelled);
            }
            return;
        };
        if self
            .continuation
            .as_ref()
            .is_some_and(|guard| !self.continuation_scope_matches(guard, &snapshot))
        {
            self.clear_continuation();
        }
        if self.observe_pending_edit(&snapshot) {
            // The selected-only phase belongs to the dispatched emoji, not a
            // new user draft. Capture has already checked global authority.
            return;
        }
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
                self.revoke_inflight(false, Some(&snapshot));
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
            self.begin_diagnostic(WorkKind::RetainedGhost, PresentationLane::Inline);
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
        let replacement_allowed = if mode == "emoji" {
            self.can_replace_emoji(&s)
        } else {
            self.can_replace(&s)
        };
        if (mode == "emoji" || mode == "synonyms" || length != 0 || start != s.caret)
            && !replacement_allowed
        {
            return;
        }
        values.truncate(5);
        if values.is_empty() {
            return;
        }
        if !anchored {
            self.finish_diagnostic(PresentationLane::Inline, Outcome::Superseded);
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
    fn can_replace_emoji(&self, s: &Snapshot) -> bool {
        (s.can_replace || s.can_replace_emoji)
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
        self.clear_work(false, true);
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
        scope_context(
            &mut c,
            &self.app,
            &self.host,
            self.browser_apps.contains(&self.app),
        );
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
        self.finish_continuation_work();
        if mode == "completion" && !s.inline_safe() {
            return false;
        }
        if matches!(mode, "emoji" | "synonyms") && !self.can_replace(s) {
            return false;
        }
        let config = self.effective_config(mode);
        if config
            .resolve_instructions(
                &s.app_id,
                &s.domain,
                self.browser_apps.contains(&s.app_id)
                    && config.app_rules.iter().any(|(scope, rule)| {
                        scope.starts_with("domain:") && rule.contains_key("custom_instructions")
                    }),
            )
            .is_err()
        {
            self.clear_continuation();
            self.mark_attempt(s, mode);
            let lane = if mode == "completion" {
                PresentationLane::Inline
            } else {
                PresentationLane::Chooser
            };
            self.begin_diagnostic(
                if mode == "completion" {
                    WorkKind::Generated
                } else {
                    WorkKind::Alternative
                },
                lane,
            );
            self.finish_diagnostic(lane, Outcome::PrivacyDenied);
            return false;
        }
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
            self.clear_work_state(false, true, preserve_context);
            self.completion_counted = false;
        }
        self.begin_diagnostic(
            if mode == "completion" {
                WorkKind::Generated
            } else {
                WorkKind::Alternative
            },
            if mode == "completion" {
                PresentationLane::Inline
            } else {
                PresentationLane::Chooser
            },
        );
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
        self.whatsapp_stage(DiagnosticStage::Scheduled);
        self.whatsapp_state();
        true
    }
    fn advance_preparation(&mut self) {
        // Main thread acquires metadata only. The context owner independently
        // reacquires AX handles and fences every phase; no AppKit/TIS is moved.
        if self.preparing.as_ref().is_some_and(|p| p.native_pending) {
            let mut p = self.preparing.take().expect("preparation");
            let current = self.capture();
            if !current
                .as_ref()
                .is_some_and(|s| p.lease.matches(s, self.current_epoch()) && s.has_anchor())
            {
                self.whatsapp_revocation(&p.lease, current.as_ref());
                self.revoke_inflight(true, current.as_ref());
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
                    && history_use_allowed(
                        &p.config,
                        &p.lease.snapshot.app_id,
                        &p.lease.snapshot.domain,
                    )) {
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
                self.revoke_inflight(true, current.as_ref());
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
                browser_apps: self.browser_apps.clone(),
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
            self.whatsapp_stage(DiagnosticStage::Prepared);
            let current = self.capture();
            if !current.as_ref().is_some_and(|s| {
                p.lease.matches(s, self.current_epoch())
                    && s.has_anchor()
                    && (p.mode != "completion" || s.inline_safe())
            }) {
                self.whatsapp_revocation(&p.lease, current.as_ref());
                self.revoke_inflight(true, current.as_ref());
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
                    let lane = if p.mode == "completion" {
                        PresentationLane::Inline
                    } else {
                        PresentationLane::Chooser
                    };
                    if let Some(trace) = self.diagnostic_traces[lane as usize] {
                        locked(&self.diagnostics).bind_request(trace, id);
                    }
                    self.diagnostic_stage(&p.mode, Stage::Preparation);
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
                        self.whatsapp_diagnostic.last_request_terminal = None;
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
            let fresh = self.capture_observation();
            self.observe_capture(fresh);
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
                self.revoke_inflight(true, current.as_ref());
                continue;
            }
            let current = current.expect("current lease");
            match event.kind.as_str() {
                "ready" => {
                    self.diagnostic_stage(&request.mode, Stage::Ready);
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
                            request.proceeded = true;
                            self.request = Some(request);
                            self.whatsapp_stage(DiagnosticStage::Proceeded);
                            self.whatsapp_state();
                            self.whatsapp_worker();
                        } else {
                            self.fail("Local inference authorization");
                            self.clear_work(false, true);
                        }
                    } else {
                        self.whatsapp_revocation(&request.lease, fresh.as_ref());
                        self.revoke_inflight(true, fresh.as_ref());
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
                        if choices.is_empty() {
                            self.finish_diagnostic(PresentationLane::Chooser, Outcome::NoOffer);
                        }
                        self.set_choices(current, choices, &mode, start, length, anchored);
                    } else {
                        self.request = Some(request);
                    }
                }
                "error" => {
                    self.finish_diagnostics(Outcome::ProviderFailed);
                    self.whatsapp_stage(DiagnosticStage::WorkerError);
                    self.whatsapp_worker();
                    self.fail("Local inference");
                    self.clear_work(false, false);
                }
                "done" => {
                    request.final_ = true;
                    if request.pending_text.is_some() {
                        self.request = Some(request);
                        self.publish_pending_completion();
                    } else if self.suggestion.text.is_empty() {
                        self.finish_continuation_work();
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
            self.revoke_inflight(true, Some(&s));
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
                self.finish_continuation_work();
                if request.final_ {
                    self.finish_diagnostic(PresentationLane::Inline, Outcome::Ineligible);
                }
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
                    self.revoke_inflight(true, fresh.as_ref());
                    return;
                }
                let text = request.pending_text.take().expect("pending text");
                // A publication owns its pending receipt as well as an already
                // acknowledged one. Finality alone does not change acceptance;
                // text (including generated whitespace), geometry and native
                // authority do. Keep an identical pending view acknowledgeable.
                let identical_publication = request.generation == self.suggestion.generation
                    && text == self.suggestion.text
                    && fresh.as_ref() == Some(&s)
                    && self.suggestion.snapshot.as_ref() == Some(&s)
                    && self.completion_offer.is_some_and(|offer| {
                        offer.epoch == self.current_epoch()
                            && offer.revision == self.candidate_revision
                            && Some(offer.revision) == self.user_revision()
                    });
                if identical_publication {
                    if !request.final_ {
                        self.request = Some(request);
                    }
                    return;
                }
                if text.is_empty() && request.final_ {
                    self.finish_continuation_work();
                    self.finish_diagnostic(PresentationLane::Inline, Outcome::NoOffer);
                }
                if self.suggestion.offer(request.generation, s, text) {
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
            self.completion_eligible(s)
        };
        if self.can_replace_emoji(s) && selected.is_empty() && s.eligible(1) {
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
                        self.finish_continuation_work();
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
            // Timed guards cross only this automatic renewal handoff. WhatsApp
            // field intent also survives same-scope model turnover through
            // clear_work_state; scope and policy clears still revoke both.
            let continuation = if mode == "completion" {
                self.continuation.take()
            } else {
                None
            };
            if self.submit(s, mode, String::new()) {
                self.continuation =
                    continuation.filter(|guard| self.continuation_scope_matches(guard, s));
            }
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
        if self.config_apply.is_some() {
            self.retire_config_apply(ConfigApplyReceipt::Cancelled);
            return;
        }
        match self.worker.release() {
            Ok(ticket) => {
                if self.config_retiring.is_some() {
                    self.config_retiring = Some(ticket);
                }
                if let Some((current, _)) = &mut self.retention_pending {
                    *current = ticket;
                }
            }
            Err(_) => self.fail("Model release"),
        }
    }
    fn presentation_matches_view(
        &self,
        key: &SnapshotKey,
        authority: &Authority,
        revision: u64,
        mode: &str,
    ) -> bool {
        self.candidate
            .as_ref()
            .is_some_and(|s| s.key() == *key && s.authority() == *authority)
            && if mode == "completion"
                || (mode == "alternatives" && self.choices.as_ref().is_some_and(|c| c.anchored))
            {
                self.completion_offer
                    .is_some_and(|offer| offer.serial == revision)
            } else {
                revision == self.candidate_revision
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
        self.presentation_matches_view(key, authority, revision, mode)
            && Some(self.candidate_revision) == self.user_revision()
            && if completion_view {
                self.completion_offer.is_some_and(|offer| {
                    offer.epoch == self.current_epoch() && offer.revision == self.candidate_revision
                }) && !self.input_pending.load(Ordering::Acquire)
                    && self.native_input_revision() == Some(self.input_revision)
            } else {
                true
            }
    }
    fn presentation_choices(
        &self,
        key: &SnapshotKey,
        authority: &Authority,
        visible: &str,
        mode: &str,
    ) -> Option<features::ChoiceMap> {
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
            .then_some(features::ChoiceMap::default()),
            "correction" => self
                .correction
                .as_ref()
                .is_some_and(|(s, r)| {
                    s.key() == *key && s.authority() == *authority && r.replacement == visible
                })
                .then_some(features::ChoiceMap::default()),
            "alternatives" | "synonyms" | "emoji" => {
                let choices = self.choices.as_ref().filter(|choices| {
                    choices.mode == mode
                        && choices.snapshot.key() == *key
                        && choices.snapshot.authority() == *authority
                })?;
                features::match_choice_subsequence(&choices.values, visible.split('\n'))
            }
            _ => None,
        }
    }
    fn native_claim_valid(&self, claim: &Claim) -> bool {
        match &claim.evidence {
            PresentationEvidence::Rendered => true,
            PresentationEvidence::NativePreedit(receipt) => {
                self.bridge.as_ref().is_some_and(|bridge| {
                    bridge.client_owns_actions() && bridge.native_preedit_valid(receipt)
                })
            }
            PresentationEvidence::NativeChoices(receipt) => self
                .bridge
                .as_ref()
                .is_some_and(|bridge| bridge.native_choices_valid(receipt)),
        }
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
        let choices =
            features::match_choice_subsequence(values, receipt.values.iter().map(String::as_str));
        let valid = choices.is_some()
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
            choices: choices.unwrap(),
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
        a.continuation = self
            .continuation
            .as_ref()
            .filter(|guard| {
                a.active
                    && !self.input_pending.load(Ordering::Acquire)
                    && self.native_input_revision() == Some(self.input_revision)
                    && self.candidate.as_ref().is_some_and(|s| {
                        self.continuation_scope_matches(guard, s)
                            && self.continuation_allowed(guard, s)
                    })
            })
            .cloned();
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
        let choices = self.presentation_choices(&key, &authority, &visible, &mode);
        let valid = current && choices.is_some();
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
        // A receipt can replace or revoke only its own publication. Keep view
        // identity separate from eligibility: current empty/invalid receipts
        // still revoke, including while native input or readiness is invalid.
        if self.presentation_matches_view(&key, &authority, revision, &mode) {
            self.claims.retain(|c| c.mode != mode);
        }
        if valid && self.native_ready && !self.shortcuts_suspended {
            self.claims.push(Arc::new(Claim {
                key,
                authority,
                revision: self.candidate_revision,
                epoch: self.current_epoch(),
                visible,
                mode,
                choices: choices.unwrap(),
                evidence: PresentationEvidence::Rendered,
            }));
        }
        self.refresh_arming();
        self.whatsapp_state();
    }
    fn accept(&mut self, action: &Action, supplied: Option<Arc<Claim>>) -> bool {
        self.clear_continuation();
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
            let displayed = match action {
                Action::Choice(i) => *i,
                _ if c.mode != "alternatives" => 0,
                _ => return false,
            };
            let Some(index) = claim.choices.original_index(displayed) else {
                return false;
            };
            if index >= c.values.len()
                || features::match_choice_subsequence(&c.values, claim.visible.split('\n'))
                    != Some(claim.choices)
                || c.mode != claim.mode
                || !c.snapshot.same_key(&current)
                || !c.snapshot.same_authority(&current)
            {
                return false;
            }
            let replacement_allowed = if c.mode == "emoji" {
                self.can_replace_emoji(&current)
            } else {
                self.can_replace(&current)
            };
            if (c.length != 0
                || c.start != current.caret
                || c.mode == "emoji"
                || c.mode == "synonyms")
                && !replacement_allowed
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
                if claim.mode == "emoji" {
                    bridge.replace_emoji_range(&current, start, length, &text)
                } else if start == current.caret && length == 0 {
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
            trace_id: self.diagnostic_traces[if claim.mode == "completion"
                || claim.mode == "correction"
            {
                PresentationLane::Inline as usize
            } else {
                PresentationLane::Chooser as usize
            }]
            .take(),
        };
        let whatsapp_completion =
            edit.before.app_id == WHATSAPP_APP_ID && edit.mode == "completion";
        if whatsapp_completion {
            let diagnostic = &mut self.whatsapp_diagnostic;
            diagnostic.observed = true;
            diagnostic.last_completion_native_outcome = Some(match &outcome {
                Ok(EditOutcome::Applied(_)) => DiagnosticNativeEditOutcome::Applied,
                Ok(EditOutcome::Dispatched) => DiagnosticNativeEditOutcome::Dispatched,
                Ok(EditOutcome::Indeterminate) => DiagnosticNativeEditOutcome::Indeterminate,
                Ok(EditOutcome::Rejected) => DiagnosticNativeEditOutcome::Rejected,
                Err(_) => DiagnosticNativeEditOutcome::Error,
            });
            diagnostic.planned_completion_whitespace_only = Some(edit.text.trim().is_empty());
            diagnostic.planned_completion_contains_tab = Some(edit.text.contains('\t'));
        }
        match outcome {
            Ok(EditOutcome::Applied(readback)) => {
                if confirmed_edit(&edit, &readback) {
                    self.commit_edit(edit, readback);
                    true
                } else {
                    if whatsapp_completion {
                        self.whatsapp_diagnostic.last_completion_native_outcome =
                            Some(DiagnosticNativeEditOutcome::ReadbackMismatch);
                    }
                    self.finish_trace(edit.trace_id, Outcome::Unknown);
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
                self.finish_trace(edit.trace_id, Outcome::Unknown);
                self.clear(true, true);
                self.uncertain_provenance = Some((current.authority(), claim.revision));
                self.mark_attempt(&current, &claim.mode);
                true
            }
            Ok(EditOutcome::Rejected) => {
                self.finish_trace(edit.trace_id, Outcome::AuthorityDenied);
                self.clear(false, true);
                false
            }
        }
    }
    fn observe_pending_edit(&mut self, snapshot: &Snapshot) -> bool {
        let Some(edit) = self.pending_edit.take() else {
            return false;
        };
        if self.now() >= edit.at + PENDING_EDIT_CONFIRMATION_TIMEOUT {
            self.finish_trace(edit.trace_id, Outcome::Unknown);
            return false;
        }
        let selected_emoji = edit.mode == "emoji"
            && snapshot.matches_pending_emoji_selection(&edit.before, edit.start, edit.length);
        if confirmed_edit(&edit, snapshot) && self.user_revision() == Some(edit.revision) {
            self.commit_edit(edit, snapshot.clone());
        } else if ((snapshot.same_key(&edit.before) && snapshot.same_authority(&edit.before))
            || selected_emoji)
            && self.user_revision() == Some(edit.revision)
        {
            self.pending_edit = Some(edit);
            return selected_emoji;
        } else {
            self.finish_trace(edit.trace_id, Outcome::Unknown);
        }
        // Any unrelated observation expires the uncertain dispatch. Never retry it.
        false
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
        self.finish_trace(edit.trace_id, Outcome::Accepted);
        if edit.before.app_id == WHATSAPP_APP_ID && edit.mode == "completion" {
            let count = &mut self.whatsapp_diagnostic.counters.confirmed_completion_edits;
            *count = count.saturating_add(1);
        }
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
                self.begin_diagnostic(WorkKind::RetainedGhost, PresentationLane::Inline);
                self.publish_completion();
            }
        }
        let completion_exhausted = edit.mode == "completion" && self.suggestion.text.is_empty();
        if edit.mode != "completion" || completion_exhausted {
            self.candidate_since = self.now();
        }
        if completion_exhausted {
            // This verified edit created a new draft, not a completed attempt
            // at that draft. Let normal scheduling request its continuation.
            self.attempted = None;
        } else {
            self.mark_attempt(&readback, &edit.mode);
        }
        if edit.mode == "completion" && Some(edit.revision) == self.user_revision() {
            let lifetime = if readback.app_id == WHATSAPP_APP_ID {
                ContinuationLifetime::WhatsAppField
            } else {
                ContinuationLifetime::RenewalUntil(self.now() + Duration::from_secs(2))
            };
            self.continuation = Some(Arc::new(ContinuationGuard {
                snapshot: readback,
                revision: edit.revision,
                lifetime,
            }));
        }
    }
    fn dismiss(&mut self, bypass: bool, claim: Option<Arc<Claim>>) -> bool {
        self.clear_continuation();
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
            self.finish_diagnostic(
                if claim.mode == "completion" || claim.mode == "correction" {
                    PresentationLane::Inline
                } else {
                    PresentationLane::Chooser
                },
                Outcome::Dismissed,
            );
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
                if input.continuation.is_some() {
                    self.clear_continuation();
                }
                let _ = self.finish_input(false);
                continue;
            }
            let accepted = if let Some(guard) = input.continuation {
                self.consume_continuation(&guard)
            } else {
                self.action(input.action, input.claim, input.origin)
            };
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
        if !self.config_promoting && history_access_configured(&c) {
            locked(&self.storage).unlock()?;
        }
        // Select the exact regional policy before publishing the configuration or
        // retiring a disabled dictionary owner.
        let new_spelling = if (c.typo_suppression || c.typo_fixes) && self.spelling.is_none() {
            Some(self.spelling_factory.as_ref().map_or_else(
                || Arc::new(SpellingService::new(None, None)) as Arc<dyn Spelling>,
                |factory| factory(),
            ))
        } else {
            None
        };
        let spelling = self.spelling.as_ref().or(new_spelling.as_ref());
        if let Some(service) = spelling {
            if let Err(error) = service.set_language(&c.spelling_language) {
                if let Some(service) = new_spelling {
                    self.spawn_spelling_cleanup(service);
                }
                return Err(error);
            }
        }
        if persist {
            if let Err(error) = self.readers.persist(&c) {
                if let Some(service) = new_spelling {
                    self.spawn_spelling_cleanup(service);
                }
                if let Some(service) = &self.spelling {
                    service.set_language(&self.config.spelling_language)?;
                }
                return Err(error);
            }
        }
        // Configuration commits finish the session even when model validation has
        // cleared the candidate. Keep its exact observation for deletion/retention.
        if let Some(session) = &self.session {
            self.retention_observation = Some((
                session.id.clone(),
                session.key.clone(),
                session.text.clone(),
            ));
        }
        self.finish_session();
        let release = !c.global_enabled
            || self.config.provider_identity() != c.provider_identity()
            || privacy_revoked(&self.config, &c);
        let retention_changed = c.history_retention_days != self.config.history_retention_days;
        self.config = c;
        if let Some(service) = new_spelling {
            self.spelling = Some(service);
        }
        if retention_changed {
            self.retention_after = self.now();
            self.retention_pending = None;
            if self.config.history_retention_days == 0 {
                self.retention_observation = None;
            }
        }
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
        if release && !self.config_promoting && self.config_apply.is_none() {
            self.release_model();
        }
        if self
            .retention_observation
            .as_ref()
            .is_some_and(|(_, authority, _)| {
                !self.record_allowed(&authority.app_id, &authority.domain)
            })
        {
            self.retention_observation = None;
        }
        if !recording_configured(&self.config) {
            self.session = None;
        }
        if !history_access_configured(&self.config) {
            self.session = None;
            locked(&self.storage).lock();
        }
        if !(self.config.typo_suppression || self.config.typo_fixes) {
            if let Some(service) = self.spelling.take() {
                self.spawn_spelling_cleanup(service);
            }
        }
        self.refresh_arming();
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
        let host = host.map(config::normalize_domain).transpose()?;
        ensure!(
            app.is_none_or(|app| !app.is_empty()),
            "Application identifier cannot be empty."
        );
        let matches = |a: &str, d: &str| {
            app.is_none() && host.is_none()
                || app == Some(a)
                || host
                    .as_ref()
                    .is_some_and(|host| config::normalize_domain(d).ok().as_ref() == Some(host))
        };
        self.records
            .retain(|r| !matches!(r, Record::History { app, domain, .. } if matches(app, domain)));
        if let Some((_, authority, text)) = &self.retention_observation {
            if matches(&authority.app_id, &authority.domain) {
                self.history_suppressed = Some((authority.clone(), text.clone()));
                self.retention_observation = None;
            }
        }
        if self
            .session
            .as_ref()
            .is_some_and(|s| matches(&s.key.app_id, &s.key.domain))
        {
            if let Some(session) = &self.session {
                self.history_suppressed = Some((session.key.clone(), session.text.clone()));
            }
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
    fn enforce_retention(&mut self) -> bool {
        if self.config.history_retention_days == 0 || self.preview {
            return false;
        }
        if self.retention_pending.is_none() {
            if self.now() < self.retention_after {
                return false;
            }
            self.retention_after = self.now() + Duration::from_secs(3600);
            if let Some(session) = &self.session {
                self.retention_observation = Some((
                    session.id.clone(),
                    session.key.clone(),
                    session.text.clone(),
                ));
            }
            self.clear(true, true);
            self.resident = None;
            self.model_activity = None;
            let cutoff = chrono::Utc::now()
                - chrono::Duration::days(i64::from(self.config.history_retention_days));
            match self.worker.release() {
                Ok(ticket) => self.retention_pending = Some((ticket, cutoff)),
                Err(_) => {
                    self.error = "History retention could not release the inference owner.".into();
                    return false;
                }
            }
        }
        let (ticket, cutoff) = self.retention_pending.expect("retention owner");
        if !self.worker.release_complete(ticket) {
            return true;
        }
        self.retention_pending = None;
        let result = locked(&self.storage).retain_since(cutoff);
        match result {
            Ok(result) => {
                self.records.retain(|record| !matches!(record, Record::History { id, .. } if result.deleted_ids.contains(id)));
                if let Some((id, authority, text)) = self.retention_observation.take() {
                    if result.deleted_ids.contains(&id) {
                        self.history_suppressed = Some((authority, text));
                    }
                }
                if self
                    .session
                    .as_ref()
                    .is_some_and(|session| result.deleted_ids.contains(&session.id))
                {
                    self.session = None;
                }
                self.emit(json!({"type":"history_retained","deleted_count":result.deleted_count(),"unknown_timestamp_count":result.unknown_timestamp_count}));
            }
            Err(_) => {
                self.retention_observation = None;
                self.error =
                    "History retention could not run while encrypted history is unavailable."
                        .into();
            }
        }
        self.configure_native();
        false
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
            Ok(true) => match ModelMigration::start(&self.config, self.model_intent_generation) {
                Ok(migration) => self.model_migration = Some(migration),
                Err(_) if self.error.is_empty() => self.error = MODEL_STORAGE_WARNING.into(),
                Err(_) => {}
            },
            Err(_) if self.error.is_empty() => self.error = MODEL_STORAGE_WARNING.into(),
            Err(_) => {}
        }
    }
    fn consume_model_migration(&mut self) {
        if let Some(pending) = self.migration_apply.take() {
            match pending.try_recv() {
                Ok(None) => self.migration_apply = Some(pending),
                Ok(Some(ConfigApplyReceipt::Failed { .. })) | Err(_) if self.error.is_empty() => {
                    self.error = MODEL_STORAGE_WARNING.into()
                }
                _ => {}
            }
        }
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
                    && self.config.model_id == migration.model_id
                    && self.model_intent_generation == migration.intent_generation =>
            {
                // Only the migrated path is adopted. Consent and other settings
                // may have changed while the file was verified in the worker.
                let mut current = self.config.clone();
                current.model_path = path;
                match SettingsDraft::from_configs(&self.config, &current)
                    .and_then(|draft| self.begin_config_apply(draft, true, true))
                {
                    Ok(pending) => self.migration_apply = Some(pending),
                    Err(_) if self.error.is_empty() => self.error = MODEL_STORAGE_WARNING.into(),
                    Err(_) => {}
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
    /// Typed configuration and retention own their worker events until their
    /// exact release/validation receipts complete, including on wake-only turns.
    fn consume_operation_completions(&mut self) -> bool {
        self.drain_diagnostic_reports();
        self.advance_config_apply();
        self.stopping
            || self.config_apply.is_some()
            || self.config_retiring.is_some()
            || self.config_release_failed
            || (self.retention_pending.is_some() && self.enforce_retention())
    }
    fn consume_publications(&mut self) {
        self.advance_preparation();
        self.consume_results();
        self.consume_context_updates();
    }
    fn consume_ready(&mut self) {
        // Shutdown progresses explicitly through shutdown_step or periodic tick.
        if self.stopping {
            return;
        }
        self.consume_host_events();
        if self.consume_operation_completions() {
            self.run_inputs();
            self.refresh_arming();
            return;
        }
        if self.shortcuts_suspended {
            return;
        }
        self.run_host_actions();
        self.run_inputs();
        if self
            .bridge
            .as_ref()
            .is_some_and(|bridge| bridge.native_edit_pending())
        {
            return;
        }
        self.consume_publications();
        self.refresh_arming();
        self.whatsapp_state();
    }
    pub fn tick(&mut self) {
        self.wake.rearm();
        self.periodic_turn();
    }
    fn periodic_turn(&mut self) {
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
        let operation_pending = self.consume_operation_completions();
        if self.stopping {
            let _ = self.shutdown_step();
            return;
        }
        if operation_pending || self.enforce_retention() {
            self.run_inputs();
            self.refresh_arming();
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
            let observation = self.capture_observation();
            if !self
                .bridge
                .as_ref()
                .is_some_and(|bridge| bridge.native_edit_pending())
            {
                self.observe_capture(observation);
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
        self.consume_publications();
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
        status["config_operation"] = self.config_operation_status();
        let browser = self.browser_apps.contains(&self.app)
            || platform::policy::is_browser(&[self.app.as_str()]);
        status["current_browser"] = json!(browser);
        status["current_domain_available"] = json!(config::normalize_domain(&self.host).is_ok());
        let mut preferences = serde_json::Map::new();
        for preference in [
            ScopedPreference::RecordHistory,
            ScopedPreference::UseHistory,
            ScopedPreference::ClipboardContext,
            ScopedPreference::ScreenContext,
            ScopedPreference::ScreenshotContext,
            ScopedPreference::ScreenshotAppearance,
        ] {
            let resolved = self.config.resolve_preference(
                preference,
                &self.app,
                &self.host,
                browser && self.config.has_domain_denial(preference),
            );
            let origin = match resolved.origin {
                config::PreferenceOrigin::Global => "global",
                config::PreferenceOrigin::App => "app",
                config::PreferenceOrigin::Domain => "domain",
                config::PreferenceOrigin::GlobalConsentDenied => "global_consent_denied",
                config::PreferenceOrigin::DomainUnavailable => "domain_unavailable",
            };
            preferences.insert(
                preference.field().into(),
                json!({"value":resolved.value,"origin":origin}),
            );
        }
        status["current_preferences"] = Value::Object(preferences);
        status
    }
    fn suspend_shortcuts(&mut self, suspended: bool) {
        self.clear_continuation();
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
        self.retire_config_apply(ConfigApplyReceipt::Stopped);
        self.retention_pending = None;
        self.retention_observation = None;
        self.stop_client_traces();
        if let Some(edit) = self.pending_edit.take() {
            self.finish_trace(edit.trace_id, Outcome::Stopped);
        }
        self.finish_diagnostics(Outcome::Stopped);
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
        self.advance_config_apply();
        self.drain_diagnostic_reports();
        if self.config_integrity_cleanup.is_empty() {
            self.done.insert("config_integrity");
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
            "config_integrity",
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
                    && features::match_choice_subsequence(&c.values, claim.visible.split('\n'))
                        == Some(claim.choices)
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
            let config = self.effective_config("completion");
            self.preparer.prefetch(ContextPreparation {
                token: self.serial,
                epoch: self.current_epoch(),
                snapshot: s,
                config,
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
        // Selection-only replacement support legitimately disappears when that selection collapses.
        && (readback.can_replace == edit.before.can_replace
            || (edit.before.can_replace
                && !readback.can_replace
                && edit.before.selection_length != 0
                && edit.start == edit.before.caret
                && edit.length == edit.before.selection_length))
        && readback.on_demand_only == edit.before.on_demand_only
        && readback.can_replace_emoji == edit.before.can_replace_emoji
        && readback.multiline == edit.before.multiline
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
        (old.use_history, new.use_history),
        (old.terminal_completion, new.terminal_completion),
    ]
    .into_iter()
    .any(|(a, b)| a && !b)
        || (old.personalization_strength != 0
            && new.personalization_strength == 0
            && history_access_configured(old))
        || new
            .excluded_apps
            .iter()
            .any(|app| !old.excluded_apps.contains(app))
        || old
            .app_rules
            .keys()
            .chain(new.app_rules.keys())
            .any(|scope| {
                [
                    "enabled",
                    "record_history",
                    "use_history",
                    "clipboard_context",
                    "screen_context",
                    "screenshot_context",
                    "screenshot_appearance",
                ]
                .iter()
                .any(|name| {
                    let previous = old
                        .app_rules
                        .get(scope)
                        .and_then(|rule| rule.get(*name))
                        .and_then(Value::as_bool);
                    let next = new
                        .app_rules
                        .get(scope)
                        .and_then(|rule| rule.get(*name))
                        .and_then(Value::as_bool);
                    previous != next && (previous == Some(true) || next == Some(false))
                })
            })
}
pub fn model_inventory() -> Result<Vec<Value>> {
    models::presets().iter().map(|p| Ok(json!({"id":p.id,"label":p.label,"description":p.description,"path":models::model_path(p)?,"size_bytes":p.size_bytes,"minimum_memory_bytes":p.minimum_memory_bytes,
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
    fn ready_only_publications_consume_prepared_ready_and_result_without_periodic_tick() {
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
        let wake = h.engine.wake.clone();
        let controller = Controller::with_engine(h.engine).unwrap();
        wake.signal();
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
            metrics: None,
        });
        wake.signal();
        controller.poll_ready();
        assert_eq!(h.worker.borrow().proceeds, vec![1]);
        h.worker.borrow_mut().events.push(WorkerEvent {
            id: 1,
            kind: "result".into(),
            text: " arrives tomorrow".into(),
            final_: true,
            model_loaded: true,
            message: String::new(),
            metrics: None,
        });
        wake.signal();
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
            metrics: None,
        });
        let wake = h.engine.wake.clone();
        let controller = Controller::with_engine(h.engine).unwrap();
        wake.signal();
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
        let mut service = AsyncPreparer::new(
            storage,
            Arc::new(AtomicU64::new(1)),
            Arc::new(SyntheticSources),
        )
        .unwrap();
        service.set_wakeup(Some(Arc::new(move || signal.try_send(()).unwrap())));
        let wake = service.wake.clone();
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
        Error,
    }
    struct NativeState {
        snapshot: Option<Snapshot>,
        revision: u64,
        notification_revision: u64,
        input_at: Option<Instant>,
        secure: Option<bool>,
        claim_valid: bool,
        hook: Option<ShortcutHandler>,
        wake: Option<Arc<dyn Fn() + Send + Sync>>,
        outcome: Outcome,
        edit_readback: Option<Snapshot>,
        edits: Vec<String>,
        finished: Vec<bool>,
        ui_callback: Option<Arc<dyn Fn(String) + Send + Sync>>,
        capture_count: usize,
        capture_time: Option<(Rc<RefCell<Instant>>, Duration)>,
        capture_unavailable: bool,
        capture_error: bool,
        transient_capture: bool,
        transient_capture_observed: bool,
        transient_scope_valid: bool,
        transient_checks: usize,
        dispatched_scope: Option<(Snapshot, u64)>,
        notify_during_capture: bool,
        notify_during_transient_proof: bool,
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
        native_choice_rows: Option<Vec<String>>,
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
            let result = if s.capture_error {
                Err(anyhow!("SYNTHETIC_PRIVATE_CAPTURE_ERROR"))
            } else {
                Ok(if s.client_pending || s.capture_unavailable {
                    None
                } else {
                    s.snapshot.clone()
                })
            };
            s.transient_capture_observed =
                matches!(&result, Ok(None)) && s.capture_unavailable && s.transient_capture;
            let wake = if s.notify_during_capture {
                s.notify_during_capture = false;
                s.notification_revision += 1;
                s.wake.clone()
            } else {
                None
            };
            drop(s);
            if let Some(wake) = wake {
                wake();
            }
            result
        }
        fn pending_edit_capture_is_transient(
            &mut self,
            before: &Snapshot,
            user_revision: u64,
        ) -> bool {
            let mut s = self.0.borrow_mut();
            s.transient_checks += 1;
            let current_capture = std::mem::take(&mut s.transient_capture_observed);
            let valid = current_capture
                && s.transient_scope_valid
                && s.secure == Some(false)
                && s.revision == user_revision
                && s.dispatched_scope
                    .as_ref()
                    .is_some_and(|(posted, revision)| {
                        *revision == user_revision
                            && posted.same_key(before)
                            && posted.same_authority(before)
                    })
                && s.snapshot.as_ref().is_some_and(|current| {
                    current.same_authority(before)
                        && !current.protected
                        && current.selection_length == 0
                        && current.can_replace == before.can_replace
                        && current.can_replace_emoji == before.can_replace_emoji
                        && current.on_demand_only == before.on_demand_only
                        && current.multiline == before.multiline
                });
            let wake = if s.notify_during_transient_proof {
                s.notify_during_transient_proof = false;
                s.notification_revision += 1;
                s.wake.clone()
            } else {
                None
            };
            drop(s);
            if let Some(wake) = wake {
                wake();
            }
            valid
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
            s.dispatched_scope =
                matches!(s.outcome, Outcome::Dispatched).then(|| (before.clone(), s.revision));
            Ok(match s.outcome {
                Outcome::Applied => {
                    let readback = s.edit_readback.take().unwrap_or(expected);
                    s.snapshot = Some(readback.clone());
                    EditOutcome::Applied(readback)
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
                Outcome::Error => return Err(anyhow!("SYNTHETIC_PRIVATE_NATIVE_ERROR")),
            })
        }
        fn replace_emoji_range(
            &mut self,
            before: &Snapshot,
            start: usize,
            length: usize,
            text: &str,
        ) -> Result<EditOutcome> {
            if !before.can_replace && !before.can_replace_emoji {
                return Ok(EditOutcome::Rejected);
            }
            self.replace_range(before, start, length, text)
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
                let mut state = self.0.borrow_mut();
                if !state.native_available
                    || state
                        .snapshot
                        .as_ref()
                        .is_none_or(|s| s.native_anchor != offer.snapshot.native_anchor)
                {
                    return Ok(None);
                }
                let now = state
                    .native_clock
                    .as_ref()
                    .map_or_else(Instant::now, |clock| *clock.borrow());
                let receipt = platform::NativePreedit {
                    anchor: offer.snapshot.native_anchor.clone(),
                    text: offer.text.into(),
                    revision: state.revision,
                    expires_at: now + Duration::from_secs(2),
                };
                state.native_preedit = Some(receipt.clone());
                Ok(Some(platform::NativeHostPresentation::Preedit(receipt)))
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
        fn native_preedit_valid(&self, receipt: &platform::NativePreedit) -> bool {
            let state = self.0.borrow();
            let now = state
                .native_clock
                .as_ref()
                .map_or_else(Instant::now, |clock| *clock.borrow());
            state.client_owned
                && state.native_available
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
                values: state
                    .native_choice_rows
                    .clone()
                    .unwrap_or_else(|| values[..count].to_vec()),
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
        fn input_claim_valid(&self) -> bool {
            self.0.borrow().claim_valid
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
        terminal_reports: VecDeque<TerminalReport>,
        submitted: Vec<(Config, String, String, GenerationOptions)>,
        prepared: usize,
        proceeds: Vec<u64>,
        cancelled: usize,
        released: usize,
        stopped: bool,
        stop_calls: usize,
        release_ticket: Option<ReleaseTicket>,
        validation_enabled: bool,
        validation_auto: bool,
        hold_release: bool,
        validation_configs: Vec<Config>,
        wake: Option<Arc<dyn Fn() + Send + Sync>>,
        publication_after_poll: Option<WorkerEvent>,
        shortcut_after_poll: Option<ShortcutHandler>,
        shortcut_admissions: Vec<bool>,
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
            let (events, wake, shortcut) = {
                let mut state = self.0.borrow_mut();
                let events = std::mem::take(&mut state.events);
                let wake = state.publication_after_poll.take().and_then(|event| {
                    state.events.push(event);
                    state.wake.clone()
                });
                (events, wake, state.shortcut_after_poll.take())
            };
            if let Some(wake) = wake {
                wake();
            }
            if let Some(shortcut) = shortcut {
                let admitted = shortcut(Action::Word);
                self.0.borrow_mut().shortcut_admissions.push(admitted);
            }
            events
        }
        fn take_terminal_report(&mut self) -> Option<TerminalReport> {
            self.0.borrow_mut().terminal_reports.pop_front()
        }
        fn cancel(&mut self) -> Result<()> {
            self.0.borrow_mut().cancelled += 1;
            Ok(())
        }
        fn release(&mut self) -> Result<ReleaseTicket> {
            let ticket = WorkerClient::new()?.release()?;
            let mut state = self.0.borrow_mut();
            state.released += 1;
            state.release_ticket = Some(ticket);
            Ok(ticket)
        }
        fn release_complete(&self, ticket: ReleaseTicket) -> bool {
            let state = self.0.borrow();
            !state.hold_release && state.release_ticket == Some(ticket)
        }
        fn prepare_for_validation(
            &mut self,
            config: &Config,
            ticket: ReleaseTicket,
        ) -> Result<std::num::NonZeroU64> {
            let mut state = self.0.borrow_mut();
            ensure!(
                state.release_ticket.take() == Some(ticket),
                "Fresh release required."
            );
            ensure!(
                state.validation_enabled,
                "Fresh validation unavailable; ordinary prepare returns zero."
            );
            state.validation_configs.push(config.clone());
            let id = 10000 + state.validation_configs.len() as u64;
            if state.validation_auto {
                state.events.extend([
                    WorkerEvent::new("prepared", id),
                    WorkerEvent::new("done", id),
                ]);
            }
            Ok(std::num::NonZeroU64::new(id).unwrap())
        }
        fn set_wakeup(&mut self, callback: Option<Arc<dyn Fn() + Send + Sync>>) {
            self.0.borrow_mut().wake = callback;
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
        recorded_ids: Vec<String>,
        retention_ids: Vec<String>,
        retention_calls: usize,
    }
    struct Memory(Arc<Mutex<Stored>>);
    impl Storage for Memory {
        fn retain_since(
            &mut self,
            _: chrono::DateTime<chrono::Utc>,
        ) -> Result<crate::storage::RetentionResult> {
            let mut store = locked(&self.0);
            store.retention_calls += 1;
            Ok(crate::storage::RetentionResult {
                deleted_ids: std::mem::take(&mut store.retention_ids),
                unknown_timestamp_count: 0,
            })
        }
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
            id: &str,
            app: &str,
            host: &str,
            text: &str,
            spans: &[(usize, usize)],
        ) -> Result<()> {
            locked(&self.0).recorded_ids.push(id.into());
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
        context_updates: Vec<ContextUpdate>,
        prefetched: usize,
        cancelled: usize,
        stopped: bool,
    }
    struct PreparationConsumer(Rc<RefCell<PreparationQueue>>);
    impl Preparer for PreparationConsumer {
        fn submit(&mut self, p: Preparation) {
            self.0.borrow_mut().pending = Some(p);
        }
        fn prefetch(&mut self, _: ContextPreparation) {
            self.0.borrow_mut().prefetched += 1;
        }
        fn context_updates(&mut self) -> Vec<ContextUpdate> {
            std::mem::take(&mut self.0.borrow_mut().context_updates)
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
        language: String,
    }
    struct Dictionary(Arc<Mutex<SpellState>>);
    impl Spelling for Dictionary {
        fn set_language(&self, language: &str) -> Result<()> {
            locked(&self.0).language = language.into();
            Ok(())
        }
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
    fn whatsapp_field(text: &str) -> Snapshot {
        Snapshot {
            app_id: WHATSAPP_APP_ID.into(),
            can_replace: false,
            can_replace_emoji: true,
            native_inline: false,
            native_anchor: String::new(),
            inline_render: Some(InlineRender {
                font_name: "Fixture".into(),
                font_size: 14.,
                baseline: 34.,
                clip_rect: [0., 0., 500., 80.],
                ascent: 12.,
                descent: 3.,
                source: "native".into(),
                ..Default::default()
            }),
            ..field(text)
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
            claim_valid: true,
            hook: None,
            wake: None,
            ui_callback: None,
            outcome: Outcome::Applied,
            edit_readback: None,
            edits: Vec::new(),
            finished: Vec::new(),
            capture_count: 0,
            capture_time: None,
            capture_unavailable: false,
            capture_error: false,
            transient_capture: false,
            transient_capture_observed: false,
            transient_scope_valid: true,
            transient_checks: 0,
            dispatched_scope: None,
            notify_during_capture: false,
            notify_during_transient_proof: false,
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
            native_choice_rows: None,
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
                metrics: None,
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

    #[test]
    fn publication_racing_ready_drain_rearms_the_next_owner_turn() {
        let mut h = harness(Config::default(), None);
        h.force();
        h.prepared();
        h.engine.drain_events();
        let controller = Controller::with_engine(h.engine).unwrap();
        controller.set_wakeup(Some(Arc::new(|| {})));
        {
            let mut worker = h.worker.borrow_mut();
            worker.events.push(WorkerEvent::new("ready", 1));
            worker.publication_after_poll = Some(WorkerEvent {
                text: " ready tomorrow".into(),
                final_: true,
                ..WorkerEvent::new("result", 1)
            });
        }
        controller.engine.borrow().wake.signal();
        let first = controller.poll_ready();
        assert_eq!(h.worker.borrow().proceeds, [1]);
        assert!(!first.iter().any(|event| event["type"] == "suggestion"));
        assert!(
            controller
                .poll_ready()
                .iter()
                .any(|event| event["type"] == "suggestion" && event["text"] == " ready tomorrow")
        );
        assert!(controller.poll_ready().is_empty());
    }

    #[test]
    fn ready_context_completion_does_not_restart_prefetch_after_its_lease_expires() {
        let mut h = harness(
            Config {
                screen_context: true,
                debounce_ms: 1000,
                ..Config::default()
            },
            None,
        );
        let snapshot = h.native.borrow().snapshot.clone().unwrap();
        h.native.borrow_mut().context = Some(ContextTicket {
            snapshot: snapshot.clone(),
            gate: None,
            entered: None,
            text: "Optional reference".into(),
        });
        h.observe();
        assert_eq!(h.preparation.borrow().prefetched, 1);
        h.preparation
            .borrow_mut()
            .context_updates
            .push(ContextUpdate {
                epoch: h.engine.current_epoch(),
                key: snapshot.key(),
                authority: snapshot.authority(),
                status: "ready".into(),
            });
        let captures = h.native.borrow().capture_count;
        let controller = Controller::with_engine(h.engine).unwrap();
        controller.engine.borrow().wake.signal();
        controller.poll_ready();
        assert_eq!(controller.status()["context_status"], "ready");
        *h.time.borrow_mut() += Duration::from_secs(3);
        controller.engine.borrow().wake.signal();
        controller.poll_ready();
        assert_eq!(h.preparation.borrow().prefetched, 1);
        assert!(h.preparation.borrow().pending.is_none());
        assert!(h.worker.borrow().submitted.is_empty());
        assert_eq!(h.native.borrow().capture_count, captures);
        controller.poll();
        assert_eq!(h.preparation.borrow().prefetched, 2);
    }

    #[test]
    fn ready_turn_consumes_publications_without_routine_work_and_flush_preserves_wake() {
        let mut h = harness(
            Config {
                idle_release_seconds: 0,
                ..Config::default()
            },
            None,
        );
        h.force();
        let request = h.preparation.borrow_mut().pending.take().unwrap();
        h.preparation.borrow_mut().results.push(Prepared {
            token: request.token,
            epoch: request.epoch,
            options: GenerationOptions::default(),
            context_status: "off".into(),
            context_diagnostic: ContextPreparationDiagnostic::default(),
            warning: String::new(),
            reference_expires: None,
        });
        let prepared = h.worker.borrow().prepared;
        *h.power.borrow_mut() = system::PowerStatus {
            available: true,
            has_battery: true,
            on_battery: true,
            charging: None,
            percent: None,
        };
        h.elapse(6000);
        let controller = Controller::with_engine(h.engine).unwrap();
        controller.engine.borrow().wake.signal();
        controller.flush_commands();
        assert!(h.worker.borrow().submitted.is_empty());
        controller.poll_ready();
        assert_eq!(h.worker.borrow().submitted.len(), 1);
        assert_eq!(h.worker.borrow().prepared, prepared);
        assert_eq!(controller.status()["on_battery"], false);

        h.worker
            .borrow_mut()
            .events
            .push(WorkerEvent::new("ready", 1));
        controller.engine.borrow().wake.signal();
        controller.flush_commands();
        assert!(h.worker.borrow().proceeds.is_empty());
        controller.poll_ready();
        assert_eq!(h.worker.borrow().proceeds, vec![1]);
        h.worker.borrow_mut().events.push(WorkerEvent {
            text: " ready tomorrow".into(),
            final_: true,
            ..WorkerEvent::new("result", 1)
        });
        controller.engine.borrow().wake.signal();
        assert!(
            controller
                .poll_ready()
                .iter()
                .any(|event| event["type"] == "suggestion" && event["text"] == " ready tomorrow")
        );
        assert_eq!(h.worker.borrow().prepared, prepared);
        assert_eq!(controller.status()["on_battery"], false);
        let captures = h.native.borrow().capture_count;
        controller.engine.borrow().wake.signal();
        controller.poll_ready();
        assert_eq!(h.native.borrow().capture_count, captures);
        assert!(h.preparation.borrow().pending.is_none());
        controller.poll();
        assert_eq!(controller.status()["on_battery"], true);
    }

    #[test]
    fn command_only_presentation_acknowledgement_rechecks_native_authority() {
        let mut h = harness(Config::default(), None);
        h.offer(" ready tomorrow");
        let offer = h
            .engine
            .drain_events()
            .into_iter()
            .find(|event| event["type"] == "suggestion" && event["text"] == " ready tomorrow")
            .unwrap();
        let controller = Controller::with_engine(h.engine).unwrap();
        // No native notification or periodic capture is needed to reject a stale receipt.
        h.native.borrow_mut().snapshot = Some(field("An unrelated field"));
        controller
            .send(Command::PresentationResult {
                key: serde_json::from_value(offer["key"].clone()).unwrap(),
                authority: serde_json::from_value(offer["authority"].clone()).unwrap(),
                revision: offer["revision"].as_u64().unwrap(),
                visible_text: " ready tomorrow".into(),
                mode: "completion".into(),
            })
            .unwrap();
        controller.flush_commands();
        let hook = h.native.borrow().hook.clone().unwrap();
        assert!(!hook(Action::All));
        assert!(h.native.borrow().edits.is_empty());
        assert!(h.preparation.borrow().pending.is_none());
    }

    #[test]
    fn ready_configuration_completion_keeps_validation_events_owned() {
        let h = harness(
            Config {
                model_id: String::new(),
                model_path: "/owned/model.gguf".into(),
                idle_release_seconds: 0,
                ..Config::default()
            },
            None,
        );
        h.worker.borrow_mut().validation_enabled = true;
        h.worker.borrow_mut().validation_auto = true;
        h.worker.borrow_mut().hold_release = true;
        let controller = Controller::with_engine(h.engine).unwrap();
        let mut draft = SettingsDraft::new(&controller.config()).unwrap();
        draft.set("acceleration", json!("cpu")).unwrap();
        let pending = controller.apply_settings(draft, false).unwrap();
        controller.engine.borrow().wake.signal();
        controller.poll_ready();
        assert!(pending.try_recv().unwrap().is_none());
        assert!(h.worker.borrow().validation_configs.is_empty());
        h.worker.borrow_mut().hold_release = false;
        controller.engine.borrow().wake.signal();
        controller.poll_ready();
        assert_eq!(h.worker.borrow().validation_configs.len(), 1);
        assert!(pending.try_recv().unwrap().is_none());
        controller.engine.borrow().wake.signal();
        controller.poll_ready();
        assert!(matches!(
            pending.try_recv().unwrap(),
            Some(ConfigApplyReceipt::Applied { .. })
        ));
        assert_eq!(controller.config().acceleration, "cpu");
        assert_eq!(h.worker.borrow().prepared, 0);
        assert!(h.worker.borrow().submitted.is_empty());
        assert_eq!(h.native.borrow().capture_count, 0);
    }

    #[test]
    fn ready_turn_finishes_only_existing_retention_release() {
        let h = harness(
            Config {
                history_retention_days: 1,
                ..Config::default()
            },
            None,
        );
        h.worker.borrow_mut().hold_release = true;
        let controller = Controller::with_engine(h.engine).unwrap();
        controller.engine.borrow().wake.signal();
        controller.poll_ready();
        assert_eq!(h.worker.borrow().released, 0);
        assert_eq!(locked(&h.storage).retention_calls, 0);
        controller.poll();
        assert_eq!(h.worker.borrow().released, 1);
        controller.engine.borrow().wake.signal();
        controller.poll_ready();
        assert_eq!(locked(&h.storage).retention_calls, 0);
        h.worker.borrow_mut().hold_release = false;
        controller.engine.borrow().wake.signal();
        controller.poll_ready();
        assert_eq!(locked(&h.storage).retention_calls, 1);
        assert_eq!(h.native.borrow().capture_count, 0);
        assert!(h.worker.borrow().submitted.is_empty());
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
            intent_generation: 0,
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
            model_id: String::new(),
            model_path: old.to_str().unwrap().into(),
            ..Config::default()
        };
        let mut h = harness(config, None);
        h.worker.borrow_mut().validation_enabled = true;
        h.worker.borrow_mut().validation_auto = true;
        h.engine.model_migration = Some(finished_migration(
            &h.engine.config,
            migrated.to_str().unwrap().into(),
        ));
        (root, h, migrated)
    }
    fn settle_config(engine: &mut Engine) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while engine.config_apply.is_some() || engine.migration_apply.is_some() {
            assert!(
                Instant::now() < deadline,
                "configuration operation did not settle"
            );
            engine.tick();
            thread::sleep(Duration::from_millis(1));
        }
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
        settle_config(&mut h.engine);
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
        settle_config(&mut h.engine);
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
            intent_generation: 0,
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
        let trace_id = h.engine.diagnostic_traces[PresentationLane::Inline as usize].unwrap();
        let mut disabled = h.engine.config.clone();
        disabled.global_enabled = false;
        h.engine
            .begin_config_apply(
                SettingsDraft::from_configs(&h.engine.config, &disabled).unwrap(),
                false,
                false,
            )
            .unwrap();
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
        assert_eq!(
            locked(&h.engine.diagnostics)
                .snapshot()
                .traces
                .iter()
                .find(|trace| trace.trace_id == trace_id)
                .unwrap()
                .outcome,
            Some(crate::diagnostics::Outcome::Unknown)
        );
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
    fn manual_transport_without_inline_capability_refuses_ordinary_completion() {
        let mut h = harness(Config::default(), None);
        {
            let mut native = h.native.borrow_mut();
            native.native_available = true;
            native.native_clock = Some(h.time.clone());
            let snapshot = native.snapshot.as_mut().unwrap();
            snapshot.native_inline = false;
            snapshot.inline_render = None;
            snapshot.caret_rect = None;
            snapshot.field_rect = None;
            snapshot.native_anchor = "4:7".into();
        }
        h.force();
        assert!(h.preparation.borrow().pending.is_none());
        assert!(h.worker.borrow().submitted.is_empty());
        h.drawn(" is ready", "completion");
        let snapshot = h.native.borrow().snapshot.clone().unwrap();
        let revision = h.native.borrow().revision;
        assert_eq!(
            h.engine.present_client(
                &snapshot.key(),
                &snapshot.authority(),
                revision,
                "completion",
            ),
            None
        );
        assert!(
            !h.engine
                .present_choices(
                    &snapshot.key(),
                    &snapshot.authority(),
                    revision,
                    &snapshot.native_anchor,
                    "completion",
                    &[" is ready".into()],
                )
                .unwrap()
        );
        assert!(h.engine.claims.is_empty());
        assert!(!h.invoke_hook(Action::Word));
        assert!(!h.invoke_hook(Action::All));
        assert!(h.native.borrow().native_preedit.is_none());
        assert!(h.native.borrow().native_choices.is_none());
        assert!(h.native.borrow().edits.is_empty());
    }

    #[test]
    fn target_owned_presentation_renewal_never_arms_global_acceptance() {
        for available in [true, false] {
            let mut h = harness(Config::default(), None);
            client_offer(&mut h);
            let original = h.engine.claims[0].clone();
            h.elapse(1950);
            h.native.borrow_mut().native_available = available;
            h.engine.renew_native_presentations();
            h.elapse(100);
            assert!(!h.invoke_hook(Action::All));
            if available {
                assert_eq!(h.engine.claims.len(), 1);
                assert!(h.engine.native_claim_valid(&h.engine.claims[0]));
            } else {
                assert!(h.engine.claims.is_empty());
                assert!(h.native.borrow().native_preedit.is_none());
            }
            assert!(!h.engine.accept(&Action::All, Some(original)));
            assert!(h.native.borrow().edits.is_empty());
            assert_eq!(
                h.native.borrow().snapshot.as_ref().unwrap().text.as_ref(),
                "The update"
            );
        }
    }

    #[test]
    fn target_owned_receipt_cannot_authorize_manual_transport_acceptance() {
        let mut h = harness(Config::default(), None);
        client_offer(&mut h);
        let claim = h.engine.claims[0].clone();
        h.native.borrow_mut().client_owned = false;
        assert!(!h.engine.native_claim_valid(&claim));
        assert!(!h.engine.accept(&Action::All, Some(claim)));
        assert!(h.engine.claims.is_empty());
        assert!(h.native.borrow().edits.is_empty());
        assert_eq!(
            h.native.borrow().snapshot.as_ref().unwrap().text.as_ref(),
            "The update"
        );
    }

    #[test]
    fn manual_native_choices_without_inline_keep_dense_ordinals_and_confirmed_insertion() {
        let mut config = Config::default();
        config.alternatives_enabled = true;
        let mut h = harness(config, None);
        {
            let mut native = h.native.borrow_mut();
            native.native_available = true;
            native.native_clock = Some(h.time.clone());
            native.native_choice_limit = 2;
            native.native_choice_rows = Some(vec![" short".into(), "  café  café".into()]);
            let snapshot = native.snapshot.as_mut().unwrap();
            snapshot.native_inline = false;
            snapshot.inline_render = None;
            snapshot.caret_rect = None;
            snapshot.field_rect = None;
            snapshot.can_replace = false;
            snapshot.native_anchor = "4:7".into();
        }
        offer_filtered_alternatives(&mut h);
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
                    "alternatives",
                    &values,
                )
                .unwrap()
        );
        assert!(!h.invoke_hook(Action::Word));
        assert!(!h.invoke_hook(Action::All));
        assert!(!h.invoke_hook(Action::Choice(2)));
        assert!(h.invoke_hook(Action::Choice(1)));
        h.engine.tick();
        assert_eq!(h.native.borrow().edits, ["  café  café"]);
        assert_eq!(
            h.native.borrow().snapshot.as_ref().unwrap().text.as_ref(),
            "The update  café  café"
        );
        assert!(h.native.borrow().native_preedit.is_none());
        assert!(!h.invoke_hook(Action::Choice(1)));
    }

    #[test]
    fn native_choices_never_accept_a_candidate_outside_the_presented_subset() {
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
    fn whatsapp_completion_edit_diagnostics_are_content_free_and_exactly_scoped() {
        for (outcome, label, confirmed, uncertain) in [
            (Outcome::Applied, "applied", 1, false),
            (Outcome::Dispatched, "dispatched", 0, true),
            (Outcome::Indeterminate, "indeterminate", 0, true),
            (Outcome::Rejected, "rejected", 0, false),
            (Outcome::WrongReadback, "readback_mismatch", 0, true),
            (Outcome::Error, "error", 0, true),
        ] {
            for (proposal, whitespace_only, contains_tab) in [
                (" SYNTHETIC_PRIVATE_COMPLETION\t", false, true),
                (" \t", true, true),
                (" SYNTHETIC_PRIVATE_COMPLETION", false, false),
            ] {
                let config = Config {
                    typo_suppression: false,
                    typo_fixes: false,
                    trailing_space: true,
                    statistics_enabled: false,
                    record_history: false,
                    ..Config::default()
                };
                let mut h = harness(config, None);
                let before = whatsapp_field("SYNTHETIC_PRIVATE_DRAFT");
                h.native.borrow_mut().snapshot = Some(before.clone());
                h.offer(proposal);
                h.drawn(proposal, "completion");
                h.native.borrow_mut().outcome = outcome;
                assert!(h.invoke_hook(Action::All));
                h.engine.tick();
                let diagnostic = serde_json::to_value(&h.engine.whatsapp_diagnostic).unwrap();
                assert_eq!(diagnostic["last_completion_native_outcome"], label);
                assert_eq!(
                    diagnostic["planned_completion_whitespace_only"],
                    whitespace_only
                );
                assert_eq!(diagnostic["planned_completion_contains_tab"], contains_tab);
                assert_eq!(
                    diagnostic["counters"]["confirmed_completion_edits"],
                    confirmed
                );
                assert_eq!(
                    diagnostic["state"]["uncertain_provenance_present"],
                    uncertain
                );
                assert_eq!(
                    diagnostic["state"]["uncertain_provenance_blocks_candidate"],
                    uncertain
                );
                assert!(locked(&h.storage).stats.is_empty());
                let confirmed = if matches!(outcome, Outcome::Dispatched) {
                    h.native.borrow_mut().snapshot =
                        edit_snapshot(&before, before.caret, 0, proposal);
                    let wake = h.native.borrow().wake.clone().unwrap();
                    h.native.borrow_mut().notification_revision += 1;
                    wake();
                    h.engine.tick();
                    h.observe();
                    1
                } else {
                    confirmed
                };
                let diagnostic = serde_json::to_value(&h.engine.whatsapp_diagnostic).unwrap();
                assert_eq!(diagnostic["last_completion_native_outcome"], label);
                assert_eq!(
                    diagnostic["counters"]["confirmed_completion_edits"],
                    confirmed
                );
                let serialized = diagnostic.to_string();
                assert!(!serialized.contains("SYNTHETIC_PRIVATE"));
                assert!(!serialized.contains("\"text\":"));
                assert!(!serialized.contains("\"message\":"));

                // A different app's completed edit must not overwrite the last
                // WhatsApp outcome or contribute to its confirmed edit count.
                h.native.borrow_mut().snapshot = Some(field("SYNTHETIC_OTHER_DRAFT"));
                h.native.borrow_mut().outcome = Outcome::Applied;
                h.offer(" unrelated");
                h.drawn(" unrelated", "completion");
                assert!(h.invoke_hook(Action::All));
                h.engine.tick();
                let diagnostic = serde_json::to_value(&h.engine.whatsapp_diagnostic).unwrap();
                assert_eq!(diagnostic["last_completion_native_outcome"], label);
                assert_eq!(
                    diagnostic["planned_completion_whitespace_only"],
                    whitespace_only
                );
                assert_eq!(diagnostic["planned_completion_contains_tab"], contains_tab);
                assert_eq!(
                    diagnostic["counters"]["confirmed_completion_edits"],
                    confirmed
                );
                assert_eq!(diagnostic["state"]["uncertain_provenance_present"], false);
                assert_eq!(
                    diagnostic["state"]["uncertain_provenance_blocks_candidate"],
                    false
                );
            }
        }
    }

    #[test]
    fn whatsapp_uncertain_provenance_diagnostic_distinguishes_presence_from_blocking() {
        let config = Config {
            typo_suppression: false,
            typo_fixes: false,
            record_history: false,
            ..Config::default()
        };
        let mut h = harness(config, None);
        h.native.borrow_mut().snapshot = Some(whatsapp_field("The update"));
        h.offer(" arrives");
        h.drawn(" arrives", "completion");
        h.native.borrow_mut().outcome = Outcome::Indeterminate;
        assert!(h.invoke_hook(Action::All));
        h.engine.tick();
        let diagnostic = serde_json::to_value(&h.engine.whatsapp_diagnostic).unwrap();
        assert_eq!(diagnostic["state"]["uncertain_provenance_present"], true);
        assert_eq!(
            diagnostic["state"]["uncertain_provenance_blocks_candidate"],
            true
        );

        h.native.borrow_mut().revision += 1;
        let wake = h.native.borrow().wake.clone().unwrap();
        wake();
        h.engine.tick();
        let diagnostic = serde_json::to_value(&h.engine.whatsapp_diagnostic).unwrap();
        assert_eq!(diagnostic["state"]["uncertain_provenance_present"], true);
        assert_eq!(
            diagnostic["state"]["uncertain_provenance_blocks_candidate"],
            false
        );
        assert_eq!(h.native.borrow().edits, [" arrives"]);
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
            "inference": {
                "submitted_requests": 8,
                "finished_requests": 7,
                "no_offer_requests": 3,
                "failed_requests": 1,
                "cancelled_requests": 2,
                "last_request": {"message": "SYNTHETIC_PRIVATE_REPORT"},
                "context": "SYNTHETIC_PRIVATE_INFERENCE",
            },
        });
        let diagnostic = serde_json::to_value(DiagnosticWorker::from_status(&status)).unwrap();
        assert_eq!(diagnostic["state"], "unknown");
        assert_eq!(diagnostic["worker_pid"], 42);
        assert_eq!(diagnostic["model_loaded"], true);
        assert_eq!(
            diagnostic["session_inference"],
            json!({
                "submitted_requests": 8,
                "finished_requests": 7,
                "no_offer_requests": 3,
                "failed_requests": 1,
                "cancelled_requests": 2,
            })
        );
        assert!(!diagnostic.to_string().contains("SYNTHETIC_PRIVATE"));
        assert!(diagnostic.get("message").is_none());
        assert!(diagnostic.get("context").is_none());
        let unavailable = serde_json::to_value(DiagnosticWorker::from_status(&json!({
            "inference": {
                "submitted_requests": "SYNTHETIC_PRIVATE_COUNTER",
                "finished_requests": -1,
                "no_offer_requests": null,
                "failed_requests": true,
                "cancelled_requests": 1.5,
            }
        })))
        .unwrap();
        assert_eq!(
            unavailable["session_inference"],
            json!({
                "submitted_requests": null,
                "finished_requests": null,
                "no_offer_requests": null,
                "failed_requests": null,
                "cancelled_requests": null,
            })
        );
        let absent = serde_json::to_value(DiagnosticWorker::from_status(&json!({}))).unwrap();
        assert!(absent["session_inference"].is_null());
    }

    #[test]
    fn whatsapp_terminal_summary_correlates_only_the_latest_owned_request() {
        let config = Config {
            debounce_ms: 5000,
            typo_suppression: false,
            typo_fixes: false,
            ..Config::default()
        };
        let mut h = harness(config, None);
        h.native.borrow_mut().snapshot = Some(whatsapp_field("The update"));
        h.force();
        h.prepared();
        let now = h.engine.now();
        let report = TerminalReport {
            request_id: 1,
            preparing: false,
            metrics: Some(crate::providers::RequestMetrics {
                no_offer: true,
                ..Default::default()
            }),
            cancelled: false,
            failed: false,
            reason: crate::diagnostics::TerminalReason::Completed,
            submitted: now,
            ready: Some(now),
            proceed: Some(now),
            first_result: None,
            terminal: now,
        };
        for unrelated in [
            TerminalReport {
                request_id: 99,
                ..report
            },
            TerminalReport {
                preparing: true,
                ..report
            },
        ] {
            h.worker.borrow_mut().terminal_reports.push_back(unrelated);
            h.engine.tick();
            let diagnostic = serde_json::to_value(&h.engine.whatsapp_diagnostic).unwrap();
            assert!(diagnostic["last_request_terminal"].is_null());
        }
        h.worker.borrow_mut().terminal_reports.push_back(report);
        h.engine.tick();
        let diagnostic = serde_json::to_value(&h.engine.whatsapp_diagnostic).unwrap();
        assert_eq!(
            diagnostic["last_request_terminal"],
            json!({
                "request_id": 1,
                "no_offer": true,
                "cancelled": false,
                "failed": false,
                "reason": "completed",
            })
        );
        h.force();
        h.prepared();
        let diagnostic = serde_json::to_value(&h.engine.whatsapp_diagnostic).unwrap();
        assert_eq!(diagnostic["last_request_id"], 2);
        assert!(diagnostic["last_request_terminal"].is_null());
        h.worker.borrow_mut().terminal_reports.push_back(report);
        h.engine.tick();
        let diagnostic = serde_json::to_value(&h.engine.whatsapp_diagnostic).unwrap();
        assert!(diagnostic["last_request_terminal"].is_null());
        h.worker
            .borrow_mut()
            .terminal_reports
            .push_back(TerminalReport {
                request_id: 2,
                metrics: None,
                cancelled: true,
                reason: crate::diagnostics::TerminalReason::Cancelled,
                ..report
            });
        h.engine.tick();
        let diagnostic = serde_json::to_value(&h.engine.whatsapp_diagnostic).unwrap();
        assert_eq!(diagnostic["last_request_terminal"]["request_id"], 2);
        assert_eq!(diagnostic["last_request_terminal"]["cancelled"], true);
        assert!(diagnostic["last_request_terminal"]["no_offer"].is_null());
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
    fn successive_word_acceptances_renew_completion_after_verified_remainder_exhaustion() {
        let config = Config {
            debounce_ms: 200,
            trailing_space: true,
            typo_suppression: false,
            typo_fixes: false,
            ..Config::default()
        };
        let mut h = harness(config, None);
        assert!(!h.invoke_hook(Action::Word), "ordinary Tab is not claimed");
        h.offer(" arrives tomorrow morning");
        for (visible, expected, caret) in [
            (" arrives tomorrow morning", "The update arrives ", 19),
            ("tomorrow morning", "The update arrives tomorrow ", 28),
            ("morning", "The update arrives tomorrow morning ", 36),
        ] {
            let offer = h
                .engine
                .drain_events()
                .into_iter()
                .rev()
                .find(|event| event["type"] == "suggestion" && event["mode"] == "completion")
                .expect("a retained word must be published");
            assert_eq!(offer["text"], visible);
            assert!(
                !h.invoke_hook(Action::All),
                "real acceptance requires fresh paint"
            );
            h.acknowledge_offer(&offer);
            assert!(h.invoke_hook(Action::Word));
            assert!(!h.invoke_hook(Action::Word), "one key owns one edit");
            h.engine.tick();
            let native = h.native.borrow();
            let snapshot = native.snapshot.as_ref().unwrap();
            assert_eq!(snapshot.text.as_ref(), expected);
            assert_eq!(snapshot.caret, caret);
            assert_eq!(snapshot.selection_length, 0);
            assert!(!snapshot.text.contains('\t'));
        }
        assert!(
            !h.invoke_hook(Action::All),
            "an exhausted offer cannot authorize text acceptance"
        );
        assert!(h.preparation.borrow().pending.is_none());
        h.elapse(199);
        h.engine.tick();
        h.engine.tick();
        assert!(
            h.preparation.borrow().pending.is_none(),
            "normal debounce still applies"
        );
        h.elapse(1);
        h.engine.tick();
        // Scheduling and native preparation run on successive owner turns.
        h.engine.tick();
        h.prepared();
        assert_eq!(h.worker.borrow().submitted.len(), 2);
        assert_eq!(
            h.worker.borrow().submitted[1].1,
            "The update arrives tomorrow morning "
        );
        h.response("ready", "", false);
        h.response("result", "with careful review", true);
        for (visible, expected, caret) in [
            (
                "with careful review",
                "The update arrives tomorrow morning with ",
                41,
            ),
            (
                "careful review",
                "The update arrives tomorrow morning with careful ",
                49,
            ),
            (
                "review",
                "The update arrives tomorrow morning with careful review ",
                56,
            ),
        ] {
            let offer = h
                .engine
                .drain_events()
                .into_iter()
                .rev()
                .find(|event| event["type"] == "suggestion" && event["mode"] == "completion")
                .expect("renewed completion must remain word-accepting");
            assert_eq!(offer["text"], visible);
            h.acknowledge_offer(&offer);
            assert!(h.invoke_hook(Action::Word));
            h.engine.tick();
            let native = h.native.borrow();
            let snapshot = native.snapshot.as_ref().unwrap();
            assert_eq!(snapshot.text.as_ref(), expected);
            assert_eq!(snapshot.caret, caret);
            assert_eq!(snapshot.selection_length, 0);
            assert!(!snapshot.text.contains('\t'));
        }
        assert_eq!(h.native.borrow().finished, [true; 6]);
        h.elapse(2000);
        h.engine.tick();
        assert!(
            !h.invoke_hook(Action::Word),
            "no-suggestion Tab passes through after the bounded guard"
        );
    }
    fn continuation_guard_harness(offer: &str) -> Harness {
        continuation_guard_harness_in_app(offer, WHATSAPP_APP_ID)
    }
    fn continuation_guard_harness_in_app(offer: &str, app_id: &str) -> Harness {
        let config = Config {
            debounce_ms: 150,
            trailing_space: true,
            typo_suppression: false,
            typo_fixes: true,
            statistics_enabled: true,
            screen_context: false,
            screenshot_context: false,
            clipboard_context: false,
            ..Config::default()
        };
        let spelling = Arc::new(Mutex::new(SpellState::default()));
        let mut h = harness(config, Some(Arc::new(Dictionary(spelling))));
        let mut snapshot = whatsapp_field("The update");
        snapshot.app_id = app_id.into();
        h.native.borrow_mut().snapshot = Some(snapshot);
        h.offer(offer);
        h.drawn(offer, "completion");
        assert!(h.invoke_hook(Action::Word));
        h.engine.tick();
        assert_eq!(h.native.borrow().edits, [" arrives "]);
        assert_eq!(locked(&h.storage).stats, [(" arrives ".into(), true)]);
        h.engine.drain_events();
        h
    }

    #[test]
    fn continuation_guard_consumes_gap_tabs_without_editing_or_restarting_work() {
        for gap in ["debounce", "preparation", "model", "paint"] {
            let mut h = continuation_guard_harness(" arrives");
            if gap != "debounce" {
                h.elapse(150);
                h.engine.tick();
                h.engine.tick();
            }
            if matches!(gap, "model" | "paint") {
                h.prepared();
                h.response("ready", "", false);
            }
            if gap == "paint" {
                h.response("result", "tomorrow", true);
            }
            let before = h.native.borrow().snapshot.clone().unwrap();
            let revision = h.native.borrow().revision;
            let submitted = h.worker.borrow().submitted.len();
            let cancelled = h.worker.borrow().cancelled;
            assert!(!h.invoke_hook(Action::All), "only accept-word is guarded");
            assert!(
                h.invoke_hook(Action::Word),
                "{gap}: bounded continuation gap"
            );
            h.engine.tick();
            assert_eq!(h.native.borrow().finished, [true, true]);
            assert_eq!(h.native.borrow().edits, [" arrives "]);
            assert_eq!(h.native.borrow().snapshot.as_ref(), Some(&before));
            assert_eq!(h.native.borrow().revision, revision);
            assert_eq!(locked(&h.storage).stats, [(" arrives ".into(), true)]);
            assert_eq!(h.worker.borrow().submitted.len(), submitted);
            assert_eq!(h.worker.borrow().cancelled, cancelled);
            if gap == "debounce" {
                h.elapse(150);
                h.engine.tick();
                h.engine.tick();
            }
            if matches!(gap, "debounce" | "preparation") {
                h.prepared();
                h.response("ready", "", false);
            }
            if gap != "paint" {
                h.response("result", "tomorrow", true);
            }
            let offer = h
                .engine
                .drain_events()
                .into_iter()
                .rev()
                .find(|event| event["type"] == "suggestion" && event["mode"] == "completion")
                .unwrap();
            assert_eq!(offer["text"], "tomorrow");
            h.acknowledge_offer(&offer);
            assert!(h.invoke_hook(Action::Word), "fresh rendered claim wins");
            h.engine.tick();
            assert_eq!(h.native.borrow().edits, [" arrives ", "tomorrow "]);
            assert_eq!(h.native.borrow().finished, [true, true, true]);
            assert_eq!(locked(&h.storage).stats.len(), 2);
            assert_eq!(h.worker.borrow().submitted.len(), 2);
        }
    }

    #[test]
    fn continuation_guard_keeps_retained_repaint_nonmutating_and_prioritizes_fresh_claim() {
        let mut h = continuation_guard_harness(" arrives tomorrow");
        assert!(h.invoke_hook(Action::Word));
        h.engine.tick();
        assert_eq!(h.native.borrow().edits, [" arrives "]);
        assert_eq!(h.native.borrow().finished, [true, true]);
        assert_eq!(locked(&h.storage).stats, [(" arrives ".into(), true)]);
        h.drawn("tomorrow", "completion");
        assert!(h.invoke_hook(Action::Word));
        h.engine.tick();
        assert_eq!(h.native.borrow().edits, [" arrives ", "tomorrow "]);
        assert_eq!(h.native.borrow().finished, [true, true, true]);
        assert_eq!(
            locked(&h.storage).stats,
            [(" arrives ".into(), true), ("tomorrow ".into(), false)]
        );
        assert_eq!(h.worker.borrow().submitted.len(), 1);
    }

    #[test]
    fn continuation_guard_admitted_during_owner_turn_survives_renewal_handoff_epoch() {
        let mut h = continuation_guard_harness(" arrives");
        let epoch = h.engine.current_epoch();
        let callback = h.native.borrow().hook.clone().unwrap();
        h.worker.borrow_mut().shortcut_after_poll = Some(callback);
        h.elapse(150);
        // The hook fires after this turn drained inputs, but before its automatic
        // scheduling handoff clears old work and advances the internal epoch.
        h.engine.tick();
        assert_eq!(h.worker.borrow().shortcut_admissions, [true]);
        assert!(h.engine.current_epoch() > epoch);
        assert_eq!(h.native.borrow().finished, [true]);
        h.engine.tick();
        assert_eq!(h.native.borrow().finished, [true, true]);
        assert_eq!(h.native.borrow().edits, [" arrives "]);
        assert_eq!(locked(&h.storage).stats, [(" arrives ".into(), true)]);
        h.prepared();
        assert_eq!(h.worker.borrow().submitted.len(), 2);
        assert_eq!(h.worker.borrow().submitted[1].1, "The update arrives ");
    }

    #[test]
    fn continuation_guard_admission_cannot_accept_or_disarm_a_later_painted_offer() {
        let mut h = continuation_guard_harness(" arrives");
        h.elapse(150);
        h.engine.tick();
        h.engine.tick();
        h.prepared();
        h.response("ready", "", false);
        h.response("result", "tomorrow", true);
        let offer = h
            .engine
            .drain_events()
            .into_iter()
            .rev()
            .find(|event| event["type"] == "suggestion" && event["mode"] == "completion")
            .unwrap();
        assert!(h.invoke_hook(Action::Word));
        let native = h.native.clone();
        let storage = h.storage.clone();
        let callback = native.borrow().hook.clone().unwrap();
        let controller = Controller::with_engine(h.engine).unwrap();
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
        assert_eq!(native.borrow().finished, [true]);
        controller.poll();
        assert_eq!(native.borrow().finished, [true, true]);
        assert_eq!(native.borrow().edits, [" arrives "]);
        assert_eq!(locked(&storage).stats, [(" arrives ".into(), true)]);
        assert!(
            callback(Action::Word),
            "the later real claim must remain armed"
        );
        controller.poll();
        assert_eq!(native.borrow().edits, [" arrives ", "tomorrow "]);
        assert_eq!(native.borrow().finished, [true, true, true]);
        assert_eq!(locked(&storage).stats.len(), 2);
    }

    #[test]
    fn continuation_guard_expires_from_acceptance_not_ignored_tabs() {
        let mut h = continuation_guard_harness_in_app(" arrives", "writer");
        assert!(h.invoke_hook(Action::Word));
        h.engine.tick();
        h.elapse(1000);
        h.engine.tick();
        assert!(h.invoke_hook(Action::Word));
        h.engine.tick();
        let token = h.preparation.borrow().pending.as_ref().unwrap().token;
        h.elapse(999);
        h.engine.tick();
        assert!(h.invoke_hook(Action::Word));
        h.engine.tick();
        h.elapse(1);
        h.engine.tick();
        assert!(
            !h.invoke_hook(Action::Word),
            "ordinary Tab resumes at two seconds"
        );
        assert_eq!(h.native.borrow().finished, [true; 4]);
        assert_eq!(h.native.borrow().edits, [" arrives "]);
        assert_eq!(locked(&h.storage).stats, [(" arrives ".into(), true)]);
        assert_eq!(h.worker.borrow().submitted.len(), 1);
        assert_eq!(
            h.preparation.borrow().pending.as_ref().unwrap().token,
            token
        );
    }

    #[test]
    fn continuation_guard_ends_on_terminal_no_offer_or_error_without_retrying() {
        for terminal in ["result", "error"] {
            let mut h = continuation_guard_harness_in_app(" arrives", "writer");
            h.elapse(150);
            h.engine.tick();
            h.engine.tick();
            h.prepared();
            h.response("ready", "", false);
            h.response(terminal, "", true);
            assert!(!h.invoke_hook(Action::Word), "{terminal}");
            h.elapse(500);
            h.engine.tick();
            assert!(!h.invoke_hook(Action::Word), "{terminal}");
            assert_eq!(h.native.borrow().finished, [true]);
            assert_eq!(h.native.borrow().edits, [" arrives "]);
            assert_eq!(h.worker.borrow().submitted.len(), 2);
        }
    }

    #[test]
    fn continuation_guard_replays_admitted_key_after_real_scope_or_policy_revocation() {
        for change in [
            "typing",
            "same_text_input",
            "focus",
            "selection",
            "protected",
            "secure",
            "pause",
            "app_policy",
            "config",
            "suspend",
            "stop",
            "dismiss",
            "native_transaction",
            "expired_capture",
        ] {
            let mut h = if change == "expired_capture" {
                continuation_guard_harness_in_app(" arrives", "writer")
            } else {
                continuation_guard_harness(" arrives")
            };
            assert!(h.invoke_hook(Action::Word), "{change}: initially guarded");
            match change {
                "typing" | "same_text_input" | "focus" | "selection" | "protected" | "secure" => {
                    let wake = h.native.borrow().wake.clone().unwrap();
                    {
                        let mut native = h.native.borrow_mut();
                        match change {
                            "typing" => {
                                let snapshot = native.snapshot.as_mut().unwrap();
                                snapshot.text = "The update arrives X".into();
                                snapshot.caret = 20;
                                native.revision += 1;
                            }
                            "same_text_input" => native.revision += 1,
                            "focus" => {
                                native.snapshot.as_mut().unwrap().window_id = "other-window".into();
                            }
                            "selection" => {
                                let snapshot = native.snapshot.as_mut().unwrap();
                                snapshot.caret = 0;
                                snapshot.selection_length = 3;
                            }
                            "protected" => native.snapshot.as_mut().unwrap().protected = true,
                            "secure" => native.secure = Some(true),
                            _ => unreachable!(),
                        }
                    }
                    wake();
                }
                "pause" => h.engine.command(Command::Pause { minutes: 1 }),
                "app_policy" => h.engine.command(Command::ToggleApp {
                    app: WHATSAPP_APP_ID.into(),
                    domain: String::new(),
                }),
                "config" => h.engine.command(Command::SetHistoryEnabled {
                    enabled: !h.engine.config.record_history,
                }),
                "suspend" => h
                    .engine
                    .command(Command::SuspendShortcuts { suspended: true }),
                "stop" => h.engine.begin_shutdown(),
                "dismiss" => h.engine.command(Command::Action(Action::Dismiss)),
                "native_transaction" => h.native.borrow_mut().claim_valid = false,
                "expired_capture" => {
                    h.native.borrow_mut().capture_time =
                        Some((h.time.clone(), Duration::from_secs(2)));
                }
                _ => unreachable!(),
            }
            h.engine.tick();
            assert_eq!(h.native.borrow().finished, [true, false], "{change}");
            assert_eq!(h.native.borrow().edits, [" arrives "], "{change}");
            assert!(
                !h.invoke_hook(Action::Word),
                "{change}: guard must stay revoked"
            );
        }
    }

    #[test]
    fn continuation_guard_never_starts_without_verified_completion_acceptance() {
        let config = Config {
            typo_suppression: false,
            typo_fixes: false,
            ..Config::default()
        };
        let mut h = harness(config.clone(), None);
        h.native.borrow_mut().snapshot = Some(whatsapp_field("The update"));
        assert!(!h.invoke_hook(Action::Word));
        h.force();
        assert!(!h.invoke_hook(Action::Word));
        h.prepared();
        h.response("ready", "", false);
        h.response("result", " arrives", true);
        assert!(
            !h.invoke_hook(Action::Word),
            "an unpainted first offer is not continuation"
        );
        for outcome in [
            Outcome::Dispatched,
            Outcome::Indeterminate,
            Outcome::WrongReadback,
        ] {
            let mut h = harness(config.clone(), None);
            h.native.borrow_mut().snapshot = Some(whatsapp_field("The update"));
            h.offer(" arrives");
            h.drawn(" arrives", "completion");
            h.native.borrow_mut().outcome = outcome;
            assert!(h.invoke_hook(Action::Word));
            h.engine.tick();
            assert!(!h.invoke_hook(Action::Word));
            assert_eq!(h.native.borrow().edits.len(), 1);
        }
        let mut h = harness(config, None);
        h.native.borrow_mut().snapshot = Some(whatsapp_field("Hi :smile"));
        h.observe();
        h.drawn("😄", "emoji");
        assert!(h.invoke_hook(Action::Choice(0)));
        h.engine.tick();
        assert_eq!(h.native.borrow().edits, ["😄"]);
        assert!(
            !h.invoke_hook(Action::Word),
            "emoji acceptance does not create a guard"
        );
    }

    fn whatsapp_reservation_after_no_offer() -> Harness {
        let mut h = continuation_guard_harness(" arrives");
        h.elapse(150);
        h.engine.tick();
        h.engine.tick();
        h.prepared();
        h.response("ready", "", false);
        h.response("result", "", true);
        h.elapse(2500);
        h.engine.tick();
        assert_eq!(h.worker.borrow().submitted.len(), 2);
        assert_eq!(h.native.borrow().edits, [" arrives "]);
        assert_eq!(locked(&h.storage).stats, [(" arrives ".into(), true)]);
        h.engine.drain_events();
        h
    }

    #[test]
    fn whatsapp_tab_reservation_survives_no_offer_and_idle_without_work_or_mutation() {
        let mut h = whatsapp_reservation_after_no_offer();
        let before = h.native.borrow().snapshot.clone().unwrap();
        let revision = h.native.borrow().revision;
        let cancelled = h.worker.borrow().cancelled;
        for delay in [0, 3000, 30000] {
            h.elapse(delay);
            h.engine.tick();
            assert!(
                !h.invoke_hook(Action::All),
                "only accept-word Tab is reserved"
            );
            assert!(
                h.invoke_hook(Action::Word),
                "verified WhatsApp intent outlives model work"
            );
            h.engine.tick();
            assert_eq!(h.native.borrow().snapshot.as_ref(), Some(&before));
            assert_eq!(h.native.borrow().revision, revision);
            assert_eq!(h.native.borrow().edits, [" arrives "]);
            assert_eq!(locked(&h.storage).stats, [(" arrives ".into(), true)]);
            assert_eq!(h.worker.borrow().submitted.len(), 2);
            assert_eq!(h.worker.borrow().cancelled, cancelled);
            assert!(h.preparation.borrow().pending.is_none());
        }
        assert_eq!(h.native.borrow().finished, [true; 4]);
    }

    #[test]
    fn whatsapp_tab_reservation_cannot_accept_or_disarm_later_paint_after_no_offer() {
        let mut h = whatsapp_reservation_after_no_offer();
        // This is an explicit request, never a retry caused by reserved Tabs.
        h.offer("tomorrow");
        assert_eq!(h.worker.borrow().submitted.len(), 3);
        let offer = h
            .engine
            .drain_events()
            .into_iter()
            .rev()
            .find(|event| event["type"] == "suggestion" && event["mode"] == "completion")
            .unwrap();
        assert!(h.invoke_hook(Action::Word));
        let native = h.native.clone();
        let storage = h.storage.clone();
        let callback = native.borrow().hook.clone().unwrap();
        let controller = Controller::with_engine(h.engine).unwrap();
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
        controller.poll();
        assert_eq!(native.borrow().edits, [" arrives "]);
        assert_eq!(native.borrow().finished, [true, true]);
        assert_eq!(locked(&storage).stats, [(" arrives ".into(), true)]);
        assert!(
            callback(Action::Word),
            "fresh rendered claim takes priority over reservation"
        );
        controller.poll();
        assert_eq!(native.borrow().edits, [" arrives ", "tomorrow "]);
        assert_eq!(native.borrow().finished, [true, true, true]);
        assert_eq!(locked(&storage).stats.len(), 2);
    }

    #[test]
    fn whatsapp_tab_reservation_revokes_scope_and_safety_without_reviving() {
        for change in [
            "typing",
            "same_text_input",
            "caret",
            "selection",
            "focus",
            "field",
            "protected",
            "secure",
            "deactivated",
            "capture_unavailable",
            "pause",
            "policy",
            "config",
            "suspend",
            "stop",
            "escape",
            "native_transaction",
        ] {
            let mut h = whatsapp_reservation_after_no_offer();
            let before = h.native.borrow().snapshot.clone().unwrap();
            assert!(
                h.invoke_hook(Action::Word),
                "{change}: reserve before revocation"
            );
            match change {
                "pause" => h.engine.command(Command::Pause { minutes: 1 }),
                "policy" => h.engine.command(Command::ToggleApp {
                    app: WHATSAPP_APP_ID.into(),
                    domain: String::new(),
                }),
                "config" => h.engine.command(Command::SetHistoryEnabled {
                    enabled: !h.engine.config.record_history,
                }),
                "suspend" => h
                    .engine
                    .command(Command::SuspendShortcuts { suspended: true }),
                "stop" => h.engine.begin_shutdown(),
                "escape" => h.engine.command(Command::Action(Action::Dismiss)),
                _ => {
                    let wake = h.native.borrow().wake.clone().unwrap();
                    {
                        let mut native = h.native.borrow_mut();
                        match change {
                            "typing" => {
                                let snapshot = native.snapshot.as_mut().unwrap();
                                snapshot.text = "The update arrives X".into();
                                snapshot.caret = 20;
                                native.revision += 1;
                            }
                            "same_text_input" => native.revision += 1,
                            "caret" => native.snapshot.as_mut().unwrap().caret -= 1,
                            "selection" => {
                                let snapshot = native.snapshot.as_mut().unwrap();
                                snapshot.caret = 0;
                                snapshot.selection_length = 3;
                            }
                            "focus" => native.snapshot.as_mut().unwrap().window_id = "other".into(),
                            "field" => native.snapshot.as_mut().unwrap().field_id = "other".into(),
                            "protected" => native.snapshot.as_mut().unwrap().protected = true,
                            "secure" => native.secure = Some(true),
                            "deactivated" => native.snapshot = None,
                            "capture_unavailable" => {
                                native.capture_unavailable = true;
                                native.transient_capture = true;
                            }
                            "native_transaction" => native.claim_valid = false,
                            _ => unreachable!(),
                        }
                    }
                    wake();
                }
            }
            h.engine.tick();
            assert_eq!(h.native.borrow().finished, [true, false], "{change}");
            assert_eq!(h.native.borrow().edits, [" arrives "], "{change}");
            {
                let mut native = h.native.borrow_mut();
                native.snapshot = Some(before);
                native.secure = Some(false);
                native.claim_valid = true;
                native.capture_unavailable = false;
            }
            h.elapse(50);
            h.engine.tick();
            assert!(
                !h.invoke_hook(Action::Word),
                "{change}: old intent must not revive"
            );
            assert_eq!(locked(&h.storage).stats, [(" arrives ".into(), true)]);
        }
    }

    #[test]
    fn whatsapp_tab_reservation_is_revoked_by_a_new_unconfirmed_edit() {
        for outcome in [
            Outcome::Dispatched,
            Outcome::Indeterminate,
            Outcome::WrongReadback,
        ] {
            let mut h = whatsapp_reservation_after_no_offer();
            h.offer("tomorrow");
            h.drawn("tomorrow", "completion");
            h.native.borrow_mut().outcome = outcome;
            assert!(h.invoke_hook(Action::Word), "real paint has priority");
            h.engine.tick();
            assert_eq!(h.native.borrow().edits, [" arrives ", "tomorrow "]);
            assert_eq!(locked(&h.storage).stats, [(" arrives ".into(), true)]);
            assert!(
                !h.invoke_hook(Action::Word),
                "old verified intent cannot cover an unknown edit"
            );
            h.elapse(2500);
            h.engine.tick();
            assert!(!h.invoke_hook(Action::Word));
            assert_eq!(locked(&h.storage).stats, [(" arrives ".into(), true)]);
        }
    }

    #[test]
    fn whatsapp_tab_reservation_is_not_extended_to_other_app_ids() {
        for app_id in ["writer", "net.whatsapp.WhatsApp.beta"] {
            let mut h = continuation_guard_harness_in_app(" arrives", app_id);
            h.elapse(2000);
            h.engine.tick();
            assert!(
                !h.invoke_hook(Action::Word),
                "{app_id}: bounded guard expired"
            );
            h.engine.tick();
            h.prepared();
            h.response("ready", "", false);
            h.response("result", "", true);
            assert!(
                !h.invoke_hook(Action::Word),
                "{app_id}: ordinary no-offer passthrough"
            );
            assert_eq!(h.native.borrow().edits, [" arrives "]);
            assert_eq!(h.native.borrow().finished, [true]);
        }
    }
    #[test]
    fn whatsapp_completion_renews_after_unclaimed_tab_changes_prefix() {
        for inserted in ["\t", "    "] {
            for gap in ["debounce", "preparation", "model"] {
                let config = Config {
                    debounce_ms: 150,
                    completion_length: "medium".into(),
                    trailing_space: true,
                    min_chars: 1,
                    complete_inside_text: true,
                    punctuation_mode: "full".into(),
                    typo_suppression: false,
                    typo_fixes: true,
                    screen_context: false,
                    screenshot_context: false,
                    clipboard_context: false,
                    ..Config::default()
                };
                // A pending dictionary must not block an insertion-only field,
                // even though its dedicated emoji capability allows shortcodes.
                let spelling = Arc::new(Mutex::new(SpellState::default()));
                let mut h = harness(config, Some(Arc::new(Dictionary(spelling))));
                let before = whatsapp_field("The update");
                h.native.borrow_mut().snapshot = Some(before.clone());
                h.offer(" arrives");
                let offer = h
                    .engine
                    .drain_events()
                    .into_iter()
                    .find(|event| event["type"] == "suggestion" && event["mode"] == "completion")
                    .unwrap();
                h.acknowledge_offer(&offer);
                assert!(h.invoke_hook(Action::Word));
                h.engine.tick();
                assert_eq!(
                    h.native.borrow().snapshot.as_ref().unwrap().text.as_ref(),
                    "The update arrives "
                );
                assert_eq!(h.native.borrow().finished, [true]);

                // The accepted insertion's delayed AX notification is not a
                // second user edit and must not latch the new draft as tried.
                let wake = h.native.borrow().wake.clone().unwrap();
                h.native.borrow_mut().notification_revision += 1;
                wake();
                h.engine.tick();
                // Authored navigation ends the accepted-edit guard even if the
                // captured text is unchanged. Subsequent Tab is host-owned.
                {
                    let mut native = h.native.borrow_mut();
                    native.revision += 1;
                    native.input_at = Some(h.engine.now());
                }
                wake();
                h.engine.tick();
                let mut retired_preparation = None;
                let mut retired_request = None;
                if gap != "debounce" {
                    h.elapse(150);
                    h.engine.tick();
                    h.engine.tick();
                    if gap == "model" {
                        h.prepared();
                        h.response("ready", "", false);
                        retired_request = h.engine.request.as_ref().map(|request| request.id);
                        assert!(retired_request.is_some());
                    } else {
                        let pending = h.preparation.borrow();
                        let pending = pending.pending.as_ref().expect("renewal preparation");
                        retired_preparation = Some((pending.token, pending.epoch));
                    }
                } else {
                    h.elapse(40);
                }
                assert!(h.engine.suggestion.text.is_empty());
                assert!(
                    !h.invoke_hook(Action::Word),
                    "{gap}: ordinary Tab is not claimed"
                );
                assert_eq!(h.native.borrow().finished, [true]);

                // A physical keydown can wake the owner before the host inserts
                // its tab. Do not replace the field with the generic fixture:
                // capabilities, PID, window and document authority stay intact.
                let revision = h.native.borrow().revision;
                {
                    let mut native = h.native.borrow_mut();
                    native.revision += 1;
                    native.input_at = Some(h.engine.now());
                }
                wake();
                h.engine.tick();
                let captures = h.native.borrow().capture_count;
                h.elapse(10);
                let prefix = format!("The update arrives {inserted}");
                {
                    let mut native = h.native.borrow_mut();
                    let snapshot = native.snapshot.as_mut().unwrap();
                    snapshot.text = prefix.clone().into();
                    snapshot.caret = prefix.chars().count();
                    snapshot.caret_rect = Some([80., 20., 1., 18.]);
                    native.notification_revision += 1;
                }
                wake();
                h.engine.tick();
                assert!(h.native.borrow().capture_count > captures);
                assert_eq!(h.native.borrow().revision, revision + 1);
                let current = h.engine.candidate.as_ref().unwrap();
                assert_eq!(current.prefix(), prefix);
                assert!(current.same_authority(&before));
                assert!(!current.can_replace);
                assert!(current.can_replace_emoji);
                assert!(current.inline_safe());
                assert!(current.eligible(1));
                assert_eq!(h.engine.candidate_revision, revision + 1);
                assert!(h.engine.request.is_none());

                // The host's later selection notification keeps the same user
                // revision and must not restart the ordinary typing debounce.
                h.elapse(5);
                h.native.borrow_mut().notification_revision += 1;
                wake();
                h.engine.tick();
                h.elapse(134);
                h.engine.tick();
                assert!(h.preparation.borrow().pending.is_none());
                assert!(!h.invoke_hook(Action::Word));
                h.elapse(1);
                h.engine.tick();
                h.engine.tick();
                let renewed_token = {
                    let queued = h.preparation.borrow();
                    let queued = queued
                        .pending
                        .as_ref()
                        .expect("automatic renewal after Tab");
                    assert_eq!(queued.snapshot.prefix(), prefix);
                    assert!(queued.snapshot.same_authority(&before));
                    assert!(!queued.snapshot.can_replace);
                    assert!(queued.snapshot.can_replace_emoji);
                    queued.token
                };

                // Late preparation/model delivery for the pre-Tab prefix must
                // neither publish old text nor consume the renewed request.
                if let Some((token, epoch)) = retired_preparation {
                    h.preparation.borrow_mut().results.push(Prepared {
                        token,
                        epoch,
                        options: GenerationOptions::default(),
                        context_status: "off".into(),
                        context_diagnostic: ContextPreparationDiagnostic::default(),
                        warning: String::new(),
                        reference_expires: None,
                    });
                    h.engine.tick();
                    assert_eq!(
                        h.preparation.borrow().pending.as_ref().unwrap().token,
                        renewed_token
                    );
                }
                h.prepared();
                assert_eq!(h.worker.borrow().submitted.last().unwrap().1, prefix);
                if let Some(id) = retired_request {
                    for kind in ["ready", "result", "done"] {
                        h.worker.borrow_mut().events.push(WorkerEvent {
                            id,
                            text: " obsolete".into(),
                            final_: true,
                            ..WorkerEvent::new(kind, id)
                        });
                    }
                    h.engine.tick();
                    assert!(h.engine.suggestion.text.is_empty());
                    assert!(!h.invoke_hook(Action::Word));
                }
                h.response("ready", "", false);
                h.response("result", "tomorrow", true);
                let offer = h
                    .engine
                    .drain_events()
                    .into_iter()
                    .find(|event| event["type"] == "suggestion" && event["mode"] == "completion")
                    .expect("a new prefix receives a new offer without Force");
                assert_eq!(offer["text"], "tomorrow");
                assert!(!h.invoke_hook(Action::Word), "fresh paint is required");
                h.acknowledge_offer(&offer);
                assert!(h.invoke_hook(Action::Word));
                h.engine.tick();
                let native = h.native.borrow();
                let after = native.snapshot.as_ref().unwrap();
                assert_eq!(after.text.as_ref(), format!("{prefix}tomorrow "));
                assert!(after.same_authority(&before));
                assert!(!after.can_replace);
                assert!(after.can_replace_emoji);
                assert_eq!(native.finished, [true, true]);
                assert_eq!(native.edits, [" arrives ", "tomorrow "]);
            }
        }
    }
    #[test]
    fn whatsapp_dispatched_completion_renews_after_confirmation_or_real_typing() {
        for intervening_input in [false, true] {
            let config = Config {
                debounce_ms: 150,
                trailing_space: true,
                typo_suppression: false,
                typo_fixes: true,
                statistics_enabled: true,
                screen_context: false,
                screenshot_context: false,
                clipboard_context: false,
                ..Config::default()
            };
            let spelling = Arc::new(Mutex::new(SpellState::default()));
            let mut h = harness(config, Some(Arc::new(Dictionary(spelling))));
            let before = whatsapp_field("The update");
            h.native.borrow_mut().snapshot = Some(before.clone());
            h.offer(" arrives");
            h.drawn(" arrives", "completion");
            h.native.borrow_mut().outcome = Outcome::Dispatched;
            assert!(h.invoke_hook(Action::Word));
            h.engine.tick();
            assert!(h.engine.pending_edit.is_some());
            assert!(locked(&h.storage).stats.is_empty());
            assert!(!h.invoke_hook(Action::Word));
            let wake = h.native.borrow().wake.clone().unwrap();

            // Dispatch may expose the unchanged value before its final readback.
            // The debounce expiring is not evidence of successful native input.
            h.native.borrow_mut().notification_revision += 1;
            wake();
            h.elapse(150);
            h.engine.tick();
            assert!(h.engine.pending_edit.is_some());
            assert!(h.preparation.borrow().pending.is_none());
            assert_eq!(h.worker.borrow().submitted.len(), 1);
            let prefix = if intervening_input {
                "The update arrives \t"
            } else {
                "The update arrives "
            };
            {
                let mut native = h.native.borrow_mut();
                let snapshot = native.snapshot.as_mut().unwrap();
                snapshot.text = prefix.into();
                snapshot.caret = prefix.chars().count();
                native.notification_revision += 1;
                if intervening_input {
                    native.revision += 1;
                    native.input_at = Some(h.engine.now());
                }
            }
            wake();
            h.engine.tick();
            assert!(h.engine.pending_edit.is_none());
            assert_eq!(
                locked(&h.storage).stats.len(),
                usize::from(!intervening_input),
                "real user input must not become a delayed native acceptance"
            );
            assert_eq!(h.native.borrow().edits, [" arrives "]);
            h.elapse(149);
            h.engine.tick();
            assert!(h.preparation.borrow().pending.is_none());
            h.elapse(1);
            h.engine.tick();
            h.engine.tick();
            h.prepared();
            assert_eq!(h.worker.borrow().submitted[1].1, prefix);
            h.response("ready", "", false);
            h.response("result", "", true);
            for _ in 0..3 {
                h.native.borrow_mut().notification_revision += 1;
                wake();
                h.elapse(1000);
                h.engine.tick();
                assert_eq!(h.invoke_hook(Action::Word), !intervening_input);
                h.engine.tick();
                assert_eq!(
                    locked(&h.storage).stats.len(),
                    usize::from(!intervening_input)
                );
                assert!(h.preparation.borrow().pending.is_none());
                assert_eq!(h.worker.borrow().submitted.len(), 2);
                assert_eq!(h.native.borrow().edits, [" arrives "]);
            }

            // No-offer suppresses duplicate work only for the attempted draft;
            // real typing must still schedule a continuation of the new prefix.
            let typed = format!("{prefix}tomorrow");
            {
                let mut native = h.native.borrow_mut();
                let snapshot = native.snapshot.as_mut().unwrap();
                snapshot.text = typed.clone().into();
                snapshot.caret = typed.chars().count();
                native.revision += 1;
                native.input_at = Some(h.engine.now());
                native.outcome = Outcome::Applied;
            }
            wake();
            h.engine.tick();
            h.elapse(150);
            h.engine.tick();
            h.engine.tick();
            h.prepared();
            assert_eq!(h.worker.borrow().submitted.len(), 3);
            assert_eq!(h.worker.borrow().submitted[2].1, typed);
            h.response("ready", "", false);
            h.response("result", " morning", true);
            h.drawn(" morning", "completion");
            assert!(h.invoke_hook(Action::Word));
            h.engine.tick();
            let native = h.native.borrow();
            let after = native.snapshot.as_ref().unwrap();
            assert_eq!(after.text.as_ref(), format!("{typed} morning "));
            assert!(after.same_authority(&before));
            assert!(!after.can_replace);
            assert!(after.can_replace_emoji);
            assert_eq!(native.edits, [" arrives ", " morning "]);
            assert_eq!(
                native.finished,
                vec![true; if intervening_input { 2 } else { 5 }]
            );
            assert_eq!(
                locked(&h.storage).stats.len(),
                1 + usize::from(!intervening_input)
            );
        }
    }
    fn unconfirmed_completion_harness(outcome: Outcome) -> Harness {
        let config = Config {
            debounce_ms: 150,
            trailing_space: true,
            typo_suppression: false,
            typo_fixes: true,
            statistics_enabled: true,
            record_history: false,
            screen_context: false,
            screenshot_context: false,
            clipboard_context: false,
            ..Config::default()
        };
        let spelling = Arc::new(Mutex::new(SpellState::default()));
        let mut h = harness(config, Some(Arc::new(Dictionary(spelling))));
        h.native.borrow_mut().snapshot = Some(whatsapp_field("The update"));
        h.offer(" arrives");
        h.drawn(" arrives", "completion");
        h.native.borrow_mut().outcome = outcome;
        assert!(h.invoke_hook(Action::Word));
        h.engine.tick();
        assert_eq!(h.native.borrow().edits, [" arrives "]);
        assert!(locked(&h.storage).stats.is_empty());
        h
    }

    #[test]
    fn transient_dispatched_capture_confirms_once_and_renews_only_after_exact_readback() {
        let mut h = unconfirmed_completion_harness(Outcome::Dispatched);
        let before = h.native.borrow().snapshot.clone().unwrap();
        let revision = h.native.borrow().revision;
        let epoch = h.engine.current_epoch();
        {
            let mut native = h.native.borrow_mut();
            native.capture_unavailable = true;
            native.transient_capture = true;
            native.notify_during_capture = true;
        }
        for elapsed in [50, 100, 300] {
            let captures = h.native.borrow().capture_count;
            h.elapse(elapsed);
            h.engine.tick();
            assert!(
                h.native.borrow().capture_count > captures,
                "normal capture must keep running"
            );
            assert!(
                h.engine.pending_edit.is_some(),
                "fresh safe None retains only the pending edit"
            );
            assert!(locked(&h.storage).stats.is_empty());
            assert_eq!(h.native.borrow().edits, [" arrives "]);
            assert_eq!(h.native.borrow().revision, revision);
            assert_eq!(h.worker.borrow().submitted.len(), 1);
            assert!(h.preparation.borrow().pending.is_none());
            assert!(
                !h.invoke_hook(Action::Word),
                "unconfirmed work never creates a Tab guard"
            );
        }
        assert!(
            h.engine.current_epoch() > epoch,
            "the failed capture delivered an AX notification"
        );
        assert!(h.native.borrow().transient_checks >= 3);
        {
            let mut native = h.native.borrow_mut();
            native.capture_unavailable = false;
            native.snapshot = edit_snapshot(&before, before.caret, 0, " arrives ");
            native.notification_revision += 1;
        }
        let wake = h.native.borrow().wake.clone().unwrap();
        wake();
        h.engine.tick();
        assert!(h.engine.pending_edit.is_none());
        assert_eq!(locked(&h.storage).stats, [(" arrives ".into(), true)]);
        assert_eq!(
            h.engine
                .whatsapp_diagnostic
                .counters
                .confirmed_completion_edits,
            1
        );
        assert!(
            h.invoke_hook(Action::Word),
            "only the exact confirmation creates a guard"
        );
        h.engine.tick();
        assert_eq!(h.native.borrow().edits, [" arrives "]);
        assert_eq!(locked(&h.storage).stats, [(" arrives ".into(), true)]);
        h.elapse(150);
        h.engine.tick();
        h.engine.tick();
        h.prepared();
        assert_eq!(h.worker.borrow().submitted.len(), 2);
        assert_eq!(h.worker.borrow().submitted[1].1, "The update arrives ");
        h.response("ready", "", false);
        h.response("result", "tomorrow", true);
        h.native.borrow_mut().outcome = Outcome::Applied;
        h.drawn("tomorrow", "completion");
        assert!(h.invoke_hook(Action::Word));
        h.engine.tick();
        assert_eq!(h.native.borrow().edits, [" arrives ", "tomorrow "]);
        assert_eq!(h.native.borrow().finished, [true, true, true]);
        assert_eq!(locked(&h.storage).stats.len(), 2);
        assert_eq!(
            h.engine
                .whatsapp_diagnostic
                .counters
                .confirmed_completion_edits,
            2
        );
    }

    #[test]
    fn transient_dispatched_capture_revokes_on_scope_security_input_policy_or_proof_race() {
        for change in [
            "focus",
            "secure",
            "deactivated",
            "user",
            "policy",
            "protected",
            "selection",
            "scope_refused",
            "proof_epoch",
        ] {
            let mut h = unconfirmed_completion_harness(Outcome::Dispatched);
            let before = h.native.borrow().snapshot.clone().unwrap();
            {
                let mut native = h.native.borrow_mut();
                native.capture_unavailable = true;
                native.transient_capture = true;
            }
            h.elapse(50);
            h.engine.tick();
            assert!(h.engine.pending_edit.is_some());
            match change {
                "focus" => {
                    h.native.borrow_mut().snapshot.as_mut().unwrap().window_id =
                        "other-window".into();
                }
                "secure" => h.native.borrow_mut().secure = Some(true),
                "deactivated" => h.native.borrow_mut().snapshot = None,
                "user" => h.native.borrow_mut().revision += 1,
                "policy" => h.engine.command(Command::Pause { minutes: 1 }),
                "protected" => h.native.borrow_mut().snapshot.as_mut().unwrap().protected = true,
                "selection" => {
                    h.native
                        .borrow_mut()
                        .snapshot
                        .as_mut()
                        .unwrap()
                        .selection_length = 1
                }
                "scope_refused" => h.native.borrow_mut().transient_scope_valid = false,
                "proof_epoch" => h.native.borrow_mut().notify_during_transient_proof = true,
                _ => unreachable!(),
            }
            h.elapse(50);
            h.engine.tick();
            assert!(h.engine.pending_edit.is_none(), "{change}");
            {
                let mut native = h.native.borrow_mut();
                native.capture_unavailable = false;
                native.secure = Some(false);
                native.transient_scope_valid = true;
                native.snapshot = edit_snapshot(&before, before.caret, 0, " arrives ");
            }
            h.observe();
            assert!(
                locked(&h.storage).stats.is_empty(),
                "{change}: later equality cannot revive it"
            );
            assert_eq!(h.native.borrow().edits, [" arrives "], "{change}");
            assert_eq!(
                h.engine
                    .whatsapp_diagnostic
                    .counters
                    .confirmed_completion_edits,
                0
            );
            assert!(
                !h.invoke_hook(Action::Word),
                "{change}: no verified continuation"
            );
        }
    }

    #[test]
    fn transient_dispatched_capture_deadline_precedes_even_exact_or_slow_readback() {
        for defer_before in [false, true] {
            for slow in [false, true] {
                let mut h = unconfirmed_completion_harness(Outcome::Dispatched);
                let before = h.native.borrow().snapshot.clone().unwrap();
                let started = h.engine.pending_edit.as_ref().unwrap().at;
                if defer_before {
                    {
                        let mut native = h.native.borrow_mut();
                        native.capture_unavailable = true;
                        native.transient_capture = true;
                    }
                    for _ in 0..3 {
                        h.elapse(600);
                        h.engine.tick();
                        assert!(h.engine.pending_edit.is_some());
                        assert_eq!(h.engine.pending_edit.as_ref().unwrap().at, started);
                    }
                }
                let elapsed = h.engine.now().duration_since(started).as_millis() as u64;
                h.elapse(if slow { 1900 - elapsed } else { 2000 - elapsed });
                {
                    let mut native = h.native.borrow_mut();
                    native.capture_unavailable = false;
                    native.snapshot = edit_snapshot(&before, before.caret, 0, " arrives ");
                    if slow {
                        native.capture_time = Some((h.time.clone(), Duration::from_millis(200)));
                    }
                    native.notification_revision += 1;
                }
                let wake = h.native.borrow().wake.clone().unwrap();
                wake();
                h.engine.tick();
                assert!(h.engine.pending_edit.is_none());
                assert!(
                    locked(&h.storage).stats.is_empty(),
                    "late exact readback is not acceptance"
                );
                assert_eq!(h.native.borrow().edits, [" arrives "]);
                assert_eq!(
                    h.engine
                        .whatsapp_diagnostic
                        .counters
                        .confirmed_completion_edits,
                    0
                );
                assert!(!h.invoke_hook(Action::Word));
            }
        }
    }

    #[test]
    fn transient_capture_does_not_rehabilitate_plain_none_errors_or_unknown_edits() {
        for failure in [
            "plain_none",
            "capture_error",
            "indeterminate",
            "readback_mismatch",
        ] {
            let outcome = match failure {
                "indeterminate" => Outcome::Indeterminate,
                "readback_mismatch" => Outcome::WrongReadback,
                _ => Outcome::Dispatched,
            };
            let mut h = unconfirmed_completion_harness(outcome);
            let before = h.native.borrow().snapshot.clone().unwrap();
            {
                let mut native = h.native.borrow_mut();
                native.capture_unavailable = true;
                native.capture_error = failure == "capture_error";
                native.transient_capture = failure != "plain_none";
            }
            h.elapse(50);
            h.engine.tick();
            assert!(h.engine.pending_edit.is_none(), "{failure}");
            if failure != "plain_none" {
                assert_eq!(h.native.borrow().transient_checks, 0, "{failure}");
            }
            {
                let mut native = h.native.borrow_mut();
                native.capture_unavailable = false;
                native.capture_error = false;
                native.snapshot = edit_snapshot(&before, before.caret, 0, " arrives ");
            }
            h.observe();
            assert!(locked(&h.storage).stats.is_empty(), "{failure}");
            assert_eq!(h.native.borrow().edits, [" arrives "], "{failure}");
            assert!(!h.invoke_hook(Action::Word), "{failure}");
        }
    }
    #[test]
    fn full_acceptance_renews_once_and_empty_result_does_not_retry() {
        let config = Config {
            debounce_ms: 200,
            typo_suppression: false,
            typo_fixes: false,
            ..Config::default()
        };
        let mut h = harness(config, None);
        h.offer(" arrives tomorrow");
        let offer = h
            .engine
            .drain_events()
            .into_iter()
            .find(|event| event["type"] == "suggestion" && event["mode"] == "completion")
            .unwrap();
        h.acknowledge_offer(&offer);
        assert!(h.invoke_hook(Action::All));
        h.engine.tick();
        {
            let native = h.native.borrow();
            let snapshot = native.snapshot.as_ref().unwrap();
            assert_eq!(snapshot.text.as_ref(), "The update arrives tomorrow");
            assert_eq!(snapshot.caret, 27);
            assert_eq!(snapshot.selection_length, 0);
            assert_eq!(native.finished, [true]);
        }
        assert!(h.invoke_hook(Action::Word));
        h.engine.tick();
        assert_eq!(h.native.borrow().edits, [" arrives tomorrow"]);
        assert!(!h.invoke_hook(Action::All));
        assert!(h.preparation.borrow().pending.is_none());
        h.elapse(200);
        h.engine.tick();
        // Scheduling and native preparation run on successive owner turns.
        h.engine.tick();
        h.prepared();
        assert_eq!(
            h.worker.borrow().submitted[1].1,
            "The update arrives tomorrow"
        );
        h.response("ready", "", false);
        h.response("result", "", true);
        for _ in 0..3 {
            h.elapse(1000);
            h.engine.tick();
            assert!(!h.invoke_hook(Action::Word));
            assert!(!h.invoke_hook(Action::All));
            assert!(h.preparation.borrow().pending.is_none());
            assert_eq!(h.worker.borrow().submitted.len(), 2);
            assert_eq!(h.native.borrow().edits, [" arrives tomorrow"]);
        }
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
            let trace_id = h.engine.diagnostic_traces[PresentationLane::Inline as usize].unwrap();
            h.drawn(" arrives", "completion");
            h.native.borrow_mut().outcome = outcome;
            assert!(h.invoke_hook(Action::All));
            h.engine.tick();
            assert!(locked(&h.storage).stats.is_empty());
            let snapshot = locked(&h.engine.diagnostics).snapshot();
            assert_eq!(
                snapshot
                    .traces
                    .iter()
                    .find(|trace| trace.trace_id == trace_id)
                    .unwrap()
                    .outcome,
                if matches!(outcome, Outcome::Dispatched) {
                    None
                } else {
                    Some(crate::diagnostics::Outcome::Unknown)
                }
            );
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
                let snapshot = locked(&h.engine.diagnostics).snapshot();
                assert_eq!(
                    snapshot
                        .traces
                        .iter()
                        .find(|trace| trace.trace_id == trace_id)
                        .unwrap()
                        .outcome,
                    Some(crate::diagnostics::Outcome::Accepted)
                );
            }
        }
    }
    #[test]
    fn dispatched_emoji_selection_confirms_once_only_after_exact_glyph_readback() {
        for change in [
            "none",
            "selection",
            "text",
            "authority",
            "revision",
            "emoji_capability",
            "ordinary_capability",
            "mode",
            "multiline",
            "protected",
            "expired",
            "paused",
            "final_capability",
        ] {
            let config = Config {
                emoji_enabled: true,
                statistics_enabled: true,
                typo_suppression: false,
                typo_fixes: false,
                ..Config::default()
            };
            let mut h = harness(config, None);
            let mut before = field("Olá 🖋 :smile:!");
            before.app_id = WHATSAPP_APP_ID.into();
            before.caret = 13;
            before.can_replace = false;
            before.can_replace_emoji = true;
            h.native.borrow_mut().snapshot = Some(before.clone());
            h.observe();
            let offer = h
                .engine
                .drain_events()
                .into_iter()
                .find(|event| event["type"] == "suggestion" && event["mode"] == "emoji")
                .expect("the emoji-only native capability must offer the shortcode");
            assert_eq!(offer["text"], "😄");
            h.acknowledge_offer(&offer);
            h.native.borrow_mut().outcome = Outcome::Dispatched;
            assert!(h.invoke_hook(Action::Choice(0)));
            h.engine.tick();
            assert!(locked(&h.storage).stats.is_empty());
            assert_eq!(h.native.borrow().finished, [true]);

            let mut selected = before.clone();
            selected.caret = 6;
            selected.selection_length = 7;
            match change {
                "selection" => selected.selection_length = 6,
                "text" => selected.text = "Olá 🖋 :smile:?".into(),
                "authority" => selected.window_revision = "other-document".into(),
                "revision" => h.native.borrow_mut().revision += 1,
                "emoji_capability" => selected.can_replace_emoji = false,
                "ordinary_capability" => selected.can_replace = true,
                "mode" => selected.on_demand_only = true,
                "multiline" => selected.multiline = false,
                "protected" => selected.protected = true,
                "expired" => h.elapse(2000),
                "paused" => h.engine.command(Command::Pause { minutes: 1 }),
                _ => {}
            }
            let wake = {
                let mut native = h.native.borrow_mut();
                native.snapshot = Some(selected);
                native.notification_revision += 1;
                native.wake.clone().unwrap()
            };
            wake();
            h.observe();
            assert!(locked(&h.storage).stats.is_empty(), "{change}");
            assert!(!h.invoke_hook(Action::Choice(0)), "{change}");
            assert_eq!(h.native.borrow().edits, ["😄"], "{change}");

            let mut after = before;
            after.text = "Olá 🖋 😄!".into();
            after.caret = 7;
            if change == "final_capability" {
                after.can_replace_emoji = false;
            }
            h.native.borrow_mut().snapshot = Some(after.clone());
            h.observe();
            let expected = if change == "none" {
                vec![("😄".into(), true)]
            } else {
                Vec::new()
            };
            assert_eq!(locked(&h.storage).stats, expected, "{change}");
            after.can_replace_emoji = true;
            h.native.borrow_mut().snapshot = Some(after);
            h.observe();
            assert_eq!(locked(&h.storage).stats, expected, "{change}");
            assert_eq!(h.native.borrow().edits, ["😄"], "{change}");
            let native = h.native.borrow();
            let snapshot = native.snapshot.as_ref().unwrap();
            assert_eq!(snapshot.text.as_ref(), "Olá 🖋 😄!");
            assert_eq!(snapshot.caret, 7);
            assert_eq!(snapshot.selection_length, 0);
        }
    }

    #[test]
    fn emoji_only_capability_does_not_enable_corrections_or_synonyms() {
        for ordinary in [false, true] {
            let spelling = Arc::new(Mutex::new(SpellState {
                ready: true,
                suspected: true,
                replacement: "update".into(),
                release: true,
                ..Default::default()
            }));
            let config = Config {
                prompt_style: "writing".into(),
                emoji_enabled: true,
                synonyms_enabled: true,
                typo_fixes: true,
                typo_suppression: false,
                ..Config::default()
            };
            let mut h = harness(config, Some(Arc::new(Dictionary(spelling))));
            {
                let mut native = h.native.borrow_mut();
                let snapshot = native.snapshot.as_mut().unwrap();
                snapshot.can_replace = ordinary;
                snapshot.can_replace_emoji = true;
                snapshot.inline_render = Some(InlineRender {
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
            assert_eq!(
                h.engine
                    .drain_events()
                    .iter()
                    .any(|event| event["mode"] == "correction"),
                ordinary
            );
            {
                let mut native = h.native.borrow_mut();
                let snapshot = native.snapshot.as_mut().unwrap();
                snapshot.caret = 4;
                snapshot.selection_length = 6;
            }
            h.observe();
            assert_eq!(h.invoke_hook(Action::Synonyms), ordinary);
            h.engine.tick();
            assert_eq!(h.preparation.borrow().pending.is_some(), ordinary);
            assert!(h.native.borrow().edits.is_empty());
        }
    }

    #[test]
    fn selected_replacement_capability_loss_counts_once_only_for_exact_readback() {
        for dispatched in [false, true] {
            for change in [
                "none",
                "authority",
                "protection",
                "text",
                "caret",
                "selection",
                "mode",
                "invalid_range",
            ] {
                let mut config = Config::default();
                config.prompt_style = "writing".into();
                config.statistics_enabled = true;
                let mut h = harness(config, None);
                let mut selected = field("A useful update");
                selected.caret = 2;
                selected.selection_length = 6;
                selected.can_replace = true;
                selected.on_demand_only = true;
                h.native.borrow_mut().snapshot = Some(selected.clone());
                h.engine.command(Command::Action(Action::Synonyms));
                h.engine.tick();
                h.prepared();
                h.response("ready", "", false);
                h.response("result", "[\"helpful\",\"practical\"]", true);
                h.drawn("helpful\npractical", "synonyms");

                let mut exact = selected;
                exact.text = "A helpful update".into();
                exact.caret = 9;
                exact.selection_length = 0;
                exact.can_replace = false;
                let mut readback = exact.clone();
                match change {
                    "authority" => readback.window_revision = "other-document".into(),
                    "protection" => readback.protected = true,
                    "text" => readback.text = "A helpful updates".into(),
                    "caret" => readback.caret = 8,
                    "selection" => readback.selection_length = 1,
                    "mode" => readback.on_demand_only = false,
                    "invalid_range" => readback.caret = usize::MAX,
                    _ => {}
                }
                {
                    let mut native = h.native.borrow_mut();
                    if dispatched {
                        native.outcome = Outcome::Dispatched;
                    } else {
                        native.edit_readback = Some(readback.clone());
                    }
                }
                assert!(h.invoke_hook(Action::Choice(0)));
                h.engine.tick();
                if dispatched {
                    assert!(locked(&h.storage).stats.is_empty());
                    h.native.borrow_mut().snapshot = Some(readback);
                    h.observe();
                }
                let expected = if change == "none" {
                    vec![("helpful".into(), true)]
                } else {
                    Vec::new()
                };
                assert_eq!(locked(&h.storage).stats, expected, "{change}, {dispatched}");
                assert!(!h.invoke_hook(Action::Choice(0)));
                h.engine.command(Command::Action(Action::Choice(0)));
                h.engine.tick();
                // A later matching observation cannot revive a refused or consumed edit.
                h.native.borrow_mut().snapshot = Some(exact);
                h.observe();
                assert_eq!(locked(&h.storage).stats, expected, "{change}, {dispatched}");
                assert_eq!(h.native.borrow().edits, ["helpful"]);
            }
        }
    }

    #[test]
    fn insertion_capability_changes_and_arbitrary_replacement_loss_never_count_or_retry() {
        for (replacement, before_capability) in [(false, false), (false, true), (true, true)] {
            let mut config = Config::default();
            config.statistics_enabled = true;
            config.trailing_space = false;
            config.emoji_enabled = true;
            config.emoticons_enabled = true;
            config.typo_suppression = false;
            config.typo_fixes = false;
            let mut h = harness(config, None);
            let (text, action, mut readback) = if replacement {
                h.native.borrow_mut().snapshot = Some(field("Olá  <3"));
                h.observe();
                let values = h.engine.choices.as_ref().unwrap().values.clone();
                h.drawn(&values.join("\n"), "emoji");
                let text = values[0].clone();
                let readback = field(&format!("Olá  {text}"));
                (text, Action::Choice(0), readback)
            } else {
                h.native.borrow_mut().snapshot.as_mut().unwrap().can_replace = before_capability;
                h.offer(" arrives");
                h.drawn(" arrives", "completion");
                (" arrives".into(), Action::All, field("The update arrives"))
            };
            readback.can_replace = !before_capability;
            h.native.borrow_mut().edit_readback = Some(readback);
            assert!(h.invoke_hook(action.clone()));
            h.engine.tick();
            assert!(locked(&h.storage).stats.is_empty());
            assert!(!h.invoke_hook(action.clone()));
            h.engine.command(Command::Action(action));
            h.engine.tick();
            assert!(locked(&h.storage).stats.is_empty());
            assert_eq!(h.native.borrow().edits, [text]);
        }
    }
    #[test]
    fn rejected_native_edit_returns_original_shortcut_without_accounting() {
        let mut h = harness(Config::default(), None);
        h.offer(" arrives");
        let trace_id = h.engine.diagnostic_traces[PresentationLane::Inline as usize].unwrap();
        h.drawn(" arrives", "completion");
        h.native.borrow_mut().outcome = Outcome::Rejected;
        assert!(h.invoke_hook(Action::Word));
        h.engine.tick();
        assert_eq!(h.native.borrow().finished, vec![false]);
        assert!(locked(&h.storage).stats.is_empty());
        assert_eq!(
            locked(&h.engine.diagnostics)
                .snapshot()
                .traces
                .iter()
                .find(|trace| trace.trace_id == trace_id)
                .unwrap()
                .outcome,
            Some(crate::diagnostics::Outcome::AuthorityDenied)
        );
        assert_eq!(
            h.native.borrow().snapshot.as_ref().unwrap().text.as_ref(),
            "The update"
        );
    }
    #[test]
    fn cancelled_deferred_edit_cannot_accept_a_replacement_trace() {
        let mut h = harness(Config::default(), None);
        h.offer(" arrives");
        let trace_id = h.engine.diagnostic_traces[PresentationLane::Inline as usize].unwrap();
        h.drawn(" arrives", "completion");
        h.native.borrow_mut().outcome = Outcome::Dispatched;
        assert!(h.invoke_hook(Action::All));
        h.engine.tick();
        assert!(h.engine.pending_edit.is_some());
        h.engine.clear(false, true);
        h.offer(" later");
        let replacement = h.engine.diagnostic_traces[PresentationLane::Inline as usize].unwrap();
        let controller = Controller::with_engine(h.engine).unwrap();
        let snapshot = controller.diagnostics();
        assert_eq!(
            snapshot
                .traces
                .iter()
                .find(|trace| trace.trace_id == trace_id)
                .unwrap()
                .outcome,
            Some(crate::diagnostics::Outcome::Cancelled)
        );
        assert_eq!(
            snapshot
                .traces
                .iter()
                .find(|trace| trace.trace_id == replacement)
                .unwrap()
                .outcome,
            None
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
        h.engine
            .begin_config_apply(
                SettingsDraft::from_configs(&h.engine.config, &updated).unwrap(),
                false,
                false,
            )
            .unwrap();
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
                metrics: None,
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
    fn whatsapp_whitespace_draft_renews_when_verified_row_geometry_returns_without_typing() {
        for whitespace in ["\t", "    "] {
            let config = Config {
                debounce_ms: 150,
                trailing_space: true,
                typo_suppression: false,
                typo_fixes: true,
                screen_context: false,
                screenshot_context: false,
                clipboard_context: false,
                ..Config::default()
            };
            let spelling = Arc::new(Mutex::new(SpellState::default()));
            let mut h = harness(config, Some(Arc::new(Dictionary(spelling))));
            let before = whatsapp_field("The update");
            let verified_render = before.inline_render.clone().unwrap();
            h.native.borrow_mut().snapshot = Some(before.clone());
            h.offer(" arrives");
            let original_offer = h
                .engine
                .drain_events()
                .into_iter()
                .find(|event| event["type"] == "suggestion" && event["mode"] == "completion")
                .unwrap();
            h.acknowledge_offer(&original_offer);
            assert!(h.invoke_hook(Action::Word));
            h.engine.tick();
            assert_eq!(h.native.borrow().edits, [" arrives "]);
            h.elapse(2000);
            h.engine.tick();
            // Authored navigation releases WhatsApp's field-session reservation;
            // expiry alone no longer turns a subsequent Tab into host input.
            let wake = h.native.borrow().wake.clone().unwrap();
            {
                let mut native = h.native.borrow_mut();
                native.revision += 1;
                native.input_at = Some(h.engine.now());
            }
            wake();
            h.engine.tick();
            assert!(
                !h.invoke_hook(Action::Word),
                "Tab passes through after authored input releases the reservation"
            );

            let prefix = format!("The update arrives {whitespace}");
            {
                let mut native = h.native.borrow_mut();
                let snapshot = native.snapshot.as_mut().unwrap();
                snapshot.text = prefix.clone().into();
                snapshot.caret = prefix.chars().count();
                snapshot.inline_render = None;
                native.revision += 1;
                native.notification_revision += 1;
                native.input_at = Some(h.engine.now());
            }
            wake();
            h.engine.tick();
            let revision = h.native.borrow().revision;
            let unverified = h.native.borrow().snapshot.clone().unwrap();
            assert!(!unverified.inline_safe());
            assert!(unverified.eligible(1));
            h.elapse(150);
            h.engine.tick();
            assert!(h.preparation.borrow().pending.is_none());
            assert_eq!(h.worker.borrow().submitted.len(), 1);
            assert!(!h.invoke_hook(Action::Word));

            // Nonempty render metadata alone is not proof: invalid row geometry
            // must remain silent even after the original debounce has elapsed.
            let mut invalid_render = verified_render.clone();
            invalid_render.clip_rect[2] = 0.;
            {
                let mut native = h.native.borrow_mut();
                native.snapshot.as_mut().unwrap().inline_render = Some(invalid_render);
                native.notification_revision += 1;
            }
            wake();
            h.elapse(50);
            h.engine.tick();
            assert!(h.preparation.borrow().pending.is_none());
            assert_eq!(h.worker.borrow().submitted.len(), 1);
            assert!(!h.invoke_hook(Action::Word));
            assert_eq!(h.native.borrow().edits, [" arrives "]);

            // Only native row evidence recovers. No authored key, text, caret,
            // capability, PID, field, or window identity changes here.
            {
                let mut native = h.native.borrow_mut();
                native.snapshot.as_mut().unwrap().inline_render = Some(verified_render);
                native.notification_revision += 1;
            }
            wake();
            h.engine.tick();
            let recovered = h.native.borrow().snapshot.clone().unwrap();
            assert!(recovered.same_key(&unverified));
            assert!(recovered.same_authority(&unverified));
            assert!(recovered.inline_safe());
            assert!(!recovered.can_replace);
            assert!(recovered.can_replace_emoji);
            assert_eq!(h.native.borrow().revision, revision);
            // Same-draft evidence recovery uses the already elapsed debounce;
            // it must not require another key or an explicit Force action.
            h.engine.tick();
            h.prepared();
            assert_eq!(h.worker.borrow().submitted.len(), 2);
            assert_eq!(h.worker.borrow().submitted[1].1, prefix);
            h.response("ready", "", false);
            h.response("result", "tomorrow", true);
            let recovered_offer = h
                .engine
                .drain_events()
                .into_iter()
                .find(|event| event["type"] == "suggestion" && event["mode"] == "completion")
                .expect("verified same-draft geometry permits automatic completion");
            assert_eq!(recovered_offer["text"], "tomorrow");
            h.acknowledge_offer(&original_offer);
            assert!(
                !h.invoke_hook(Action::Word),
                "old paint cannot authorize recovery"
            );
            h.acknowledge_offer(&recovered_offer);
            assert!(h.invoke_hook(Action::Word));
            h.engine.tick();
            let native = h.native.borrow();
            assert_eq!(
                native.snapshot.as_ref().unwrap().text.as_ref(),
                format!("{prefix}tomorrow ")
            );
            assert_eq!(native.revision, revision);
            assert_eq!(native.finished, [true, true]);
            assert_eq!(native.edits, [" arrives ", "tomorrow "]);
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
                    metrics: None,
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
    fn offer_filtered_alternatives(h: &mut Harness) {
        h.observe();
        h.engine.command(Command::Action(Action::Alternatives));
        h.engine.tick();
        h.prepared();
        h.response("ready", "", false);
        h.response(
            "result",
            "[\" short\",\" a long candidate excluded by available geometry\",\"  café  café\"]",
            true,
        );
    }

    #[test]
    fn rendered_choices_accept_dense_ordinals_and_reject_entire_invalid_acknowledgments() {
        for keyboard in [false, true] {
            let mut config = Config::default();
            config.alternatives_enabled = true;
            let mut h = harness(config, None);
            offer_filtered_alternatives(&mut h);
            h.drawn(" short\n  café  café", "alternatives");
            assert!(!h.invoke_hook(Action::Word));
            assert!(!h.invoke_hook(Action::Choice(2)));
            if keyboard {
                assert!(h.invoke_hook(Action::Choice(1)));
            } else {
                h.engine.command(Command::Action(Action::Choice(1)));
            }
            h.engine.tick();
            assert_eq!(h.native.borrow().edits, ["  café  café"]);
            assert_eq!(
                h.native.borrow().snapshot.as_ref().unwrap().text.as_ref(),
                "The update  café  café"
            );
        }
        for shown in [
            "",
            " short\nforged",
            "  café  café\n short",
            " short\n short",
            " short\n  café  café\nextra",
            " short\n  café  café",
        ] {
            let mut config = Config::default();
            config.alternatives_enabled = true;
            let mut h = harness(config, None);
            offer_filtered_alternatives(&mut h);
            h.drawn(" short", "alternatives");
            h.drawn(shown, "alternatives");
            assert!(!h.invoke_hook(Action::Choice(0)), "{shown}");
            h.engine.command(Command::Action(Action::Choice(0)));
            h.engine.tick();
            assert!(h.native.borrow().edits.is_empty(), "{shown}");
        }
    }

    #[test]
    fn current_invalid_receipts_revoke_only_their_own_presentation_lane() {
        for mode in ["completion", "alternatives"] {
            for visible in ["", "unpublished row"] {
                let mut config = Config::default();
                config.alternatives_enabled = true;
                config.typo_suppression = false;
                config.typo_fixes = false;
                let mut h = harness(config, None);
                h.offer(" arrives tomorrow");
                h.engine.command(Command::Action(Action::Alternatives));
                h.engine.tick();
                h.prepared();
                h.response("ready", "", false);
                h.response("result", "[\" is ready\",\" arrives tonight\"]", true);
                h.drawn(" arrives tomorrow", "completion");
                h.drawn(" is ready\n arrives tonight", "alternatives");
                h.drawn(visible, mode);
                let (revoked, independent, expected) = if mode == "completion" {
                    (Action::All, Action::Choice(1), " arrives tonight")
                } else {
                    (Action::Choice(1), Action::All, " arrives tomorrow")
                };
                assert!(
                    !h.invoke_hook(revoked),
                    "{mode}: current invalid receipt {visible:?} retained acceptance"
                );
                assert!(
                    h.invoke_hook(independent),
                    "{mode}: current invalid receipt {visible:?} revoked another lane"
                );
                h.engine.tick();
                assert_eq!(h.native.borrow().edits, [expected]);
            }
        }
    }

    #[test]
    fn filtered_choice_native_receipts_preserve_mapping_and_reject_expired_or_forged_rows() {
        for case in [
            "valid",
            "expired",
            "revision",
            "forged",
            "reordered",
            "extra",
        ] {
            let mut config = Config::default();
            config.alternatives_enabled = true;
            let mut h = harness(config, None);
            offer_filtered_alternatives(&mut h);
            let before = h.native.borrow().snapshot.clone().unwrap();
            let values = h.engine.choices.as_ref().unwrap().values.clone();
            {
                let mut n = h.native.borrow_mut();
                n.native_available = true;
                n.native_clock = Some(h.time.clone());
                n.native_choice_limit = 9;
                n.native_choice_rows = Some(match case {
                    "forged" => vec![" short".into(), "forged".into()],
                    "reordered" => vec![values[2].clone(), values[0].clone()],
                    "extra" => vec![values[0].clone(), values[2].clone(), "extra".into()],
                    _ => vec![values[0].clone(), values[2].clone()],
                });
            }
            let accepted = h
                .engine
                .present_choices(
                    &before.key(),
                    &before.authority(),
                    h.engine.candidate_revision,
                    &before.native_anchor,
                    "alternatives",
                    &values,
                )
                .unwrap();
            assert_eq!(accepted, matches!(case, "valid" | "expired" | "revision"));
            match case {
                "expired" => h.elapse(2001),
                "revision" => h.native.borrow_mut().revision += 1,
                _ => {}
            }
            h.engine.command(Command::Action(Action::Choice(1)));
            h.engine.tick();
            if case == "valid" {
                assert_eq!(h.native.borrow().edits, ["  café  café"]);
            } else {
                assert!(h.native.borrow().edits.is_empty(), "{case}");
            }
        }
    }

    #[test]
    fn filtered_target_owned_choice_is_accounted_once_and_never_reinserted() {
        let mut config = Config::default();
        config.alternatives_enabled = true;
        config.statistics_enabled = true;
        let mut h = harness(config, None);
        offer_filtered_alternatives(&mut h);
        let before = h.native.borrow().snapshot.clone().unwrap();
        {
            let mut n = h.native.borrow_mut();
            n.client_owned = true;
            n.native_available = true;
            n.native_clock = Some(h.time.clone());
            n.native_choice_limit = 9;
            n.native_choice_rows = Some(vec![" short".into(), "  café  café".into()]);
        }
        assert_eq!(
            h.engine.present_client(
                &before.key(),
                &before.authority(),
                h.engine.candidate_revision,
                "alternatives",
            ),
            Some(true)
        );
        assert!(!h.invoke_hook(Action::Choice(1)));
        let after = field("The update  café  café");
        let receipt = h.native.borrow().native_choices.clone().unwrap();
        for _ in 0..2 {
            h.native
                .borrow_mut()
                .host_events
                .push(platform::NativeHostEvent::Applied(
                    platform::NativeHostEdit {
                        presentation: platform::NativeHostPresentation::Choices(receipt.clone()),
                        before: before.clone(),
                        after: after.clone(),
                        start: before.caret,
                        length: 0,
                        text: "  café  café".into(),
                        action: Action::Choice(1),
                        mode: "alternatives".into(),
                        accepted_at: *h.time.borrow(),
                    },
                ));
        }
        h.native.borrow_mut().snapshot = Some(after);
        h.engine.tick();
        assert_eq!(locked(&h.storage).stats, [("  café  café".into(), true)]);
        assert_eq!(h.engine.native_client.confirmed, 1);
        assert!(h.native.borrow().edits.is_empty());
    }

    #[test]
    fn filtered_alternatives_leave_tab_with_ordinary_ghost_and_revoke_on_new_input() {
        for stale in [false, true] {
            let mut config = Config::default();
            config.alternatives_enabled = true;
            let mut h = harness(config, None);
            h.offer(" arrives tomorrow");
            h.engine.command(Command::Action(Action::Alternatives));
            h.engine.tick();
            h.prepared();
            h.response("ready", "", false);
            h.response("result", "[\" short\",\" hidden middle\",\" later\"]", true);
            h.drawn(" arrives tomorrow", "completion");
            h.drawn(" short\n later", "alternatives");
            if stale {
                let before = h.native.borrow().snapshot.clone().unwrap();
                let revision = h.engine.completion_offer.unwrap().serial;
                h.native.borrow_mut().revision += 1;
                h.observe();
                h.engine.command(Command::PresentationResult {
                    key: before.key(),
                    authority: before.authority(),
                    revision,
                    visible_text: " short\n later".into(),
                    mode: "alternatives".into(),
                });
                h.engine.command(Command::Action(Action::Choice(1)));
                h.engine.tick();
                assert!(h.native.borrow().edits.is_empty());
            } else {
                assert!(h.invoke_hook(Action::Word));
                h.engine.tick();
                assert_eq!(h.native.borrow().edits, [" arrives "]);
                assert_eq!(h.engine.suggestion.text, "tomorrow");
            }
        }
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
    fn delayed_old_receipts_cannot_authorize_or_revoke_a_newer_acknowledged_view() {
        for mode in ["completion", "alternatives"] {
            for negative in [false, true] {
                let mut config = Config::default();
                config.alternatives_enabled = true;
                config.typo_suppression = false;
                config.typo_fixes = false;
                let mut h = harness(config, None);
                let mut offers = Vec::new();
                for (completion, rows, visible) in [
                    (
                        " arrives tomorrow",
                        "[\" short\",\" later\"]",
                        " short\n later",
                    ),
                    (
                        " ready for review",
                        "[\" current\",\" excluded row\",\"  café  café\"]",
                        " current\n  café  café",
                    ),
                ] {
                    h.offer(completion);
                    if mode == "alternatives" {
                        h.engine.command(Command::Action(Action::Alternatives));
                        h.engine.tick();
                        h.prepared();
                        h.response("ready", "", false);
                        h.response("result", rows, true);
                    }
                    let mut event = h
                        .engine
                        .drain_events()
                        .into_iter()
                        .rev()
                        .find(|event| {
                            event["type"] == "suggestion" && event["mode"] == "completion"
                        })
                        .unwrap();
                    if mode == "alternatives" {
                        assert_eq!(event["choices_mode"], "alternatives");
                        event["mode"] = json!(mode);
                        event["text"] = json!(visible);
                    }
                    if offers.is_empty() {
                        h.acknowledge_offer(&event);
                    }
                    offers.push(event);
                }
                let mut old = offers.remove(0);
                let current = offers.remove(0);
                assert_ne!(old["revision"], current["revision"]);
                assert_eq!(old["key"], current["key"]);
                assert_eq!(old["authority"], current["authority"]);
                if negative {
                    old["text"] = json!("");
                }
                h.acknowledge_offer(&old);
                assert!(!h.invoke_hook(Action::All));
                assert!(!h.invoke_hook(Action::Choice(1)));
                assert!(h.native.borrow().edits.is_empty());

                h.acknowledge_offer(&current);
                h.acknowledge_offer(&old);
                let action = if mode == "alternatives" {
                    Action::Choice(1)
                } else {
                    Action::All
                };
                assert!(
                    h.invoke_hook(action.clone()),
                    "{mode}: delayed negative={negative} receipt revoked the current grant"
                );
                h.engine.tick();
                let expected = if mode == "alternatives" {
                    "  café  café"
                } else {
                    " ready for review"
                };
                assert_eq!(h.native.borrow().edits, [expected]);
                assert_eq!(
                    h.native.borrow().snapshot.as_ref().unwrap().text.as_ref(),
                    format!("The update{expected}")
                );
                h.engine.command(Command::Action(action));
                h.engine.tick();
                assert_eq!(h.native.borrow().edits, [expected]);
                assert!(h.engine.drain_events().iter().any(|event| {
                    event["type"] == "action_result" && event["handled"] == false
                }));
            }
        }
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
        let pending = h
            .engine
            .begin_config_apply(
                SettingsDraft::from_configs(&h.engine.config, &c).unwrap(),
                true,
                false,
            )
            .unwrap();
        assert!(!h.engine.config.record_history);
        assert!(matches!(
            pending.try_recv().unwrap(),
            Some(ConfigApplyReceipt::Failed { .. })
        ));
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
            browser_apps: BTreeSet::new(),
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
    fn preparer_wakes_only_for_current_publications_after_releasing_the_queue() {
        let storage: SharedStorage = Arc::new(Mutex::new(Box::new(Memory(Arc::new(Mutex::new(
            Stored::default(),
        ))))));
        let epoch = Arc::new(AtomicU64::new(1));
        let mut service = AsyncPreparer::new(storage, epoch, Arc::new(SyntheticSources)).unwrap();
        let state = service.state.clone();
        let (published, receive) = mpsc::channel();
        service.set_wakeup(Some(Arc::new(move || {
            let deadline = Instant::now() + Duration::from_secs(1);
            let token = loop {
                if let Ok(state) = state.0.try_lock() {
                    break state
                        .results
                        .back()
                        .map(|result| result.token)
                        .or_else(|| state.screen_result.as_ref().map(|result| result.0));
                }
                if Instant::now() >= deadline {
                    break None;
                }
                thread::yield_now();
            };
            let _ = published.send(token);
        })));
        let snapshot = field("The update");
        let mut request = preparation(snapshot.clone(), 1, 1, None);
        request.config.screen_context = false;
        service.submit(request);
        assert_eq!(
            receive.recv_timeout(Duration::from_secs(2)).unwrap(),
            Some(1)
        );
        assert_eq!(service.poll().len(), 1);
        service.cancel();

        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (entered, began) = mpsc::sync_channel(1);
        let mut config = Config::default();
        config.screen_context = true;
        service.prefetch(ContextPreparation {
            token: 2,
            epoch: 1,
            snapshot: snapshot.clone(),
            config: config.clone(),
            ticket: Box::new(ContextTicket {
                snapshot: snapshot.clone(),
                gate: Some(gate.clone()),
                entered: Some(entered),
                text: "Obsolete reference".into(),
            }),
        });
        began.recv_timeout(Duration::from_secs(1)).unwrap();
        service.cancel();
        service.prefetch(ContextPreparation {
            token: 3,
            epoch: 1,
            snapshot: snapshot.clone(),
            config,
            ticket: Box::new(ContextTicket {
                snapshot,
                gate: None,
                entered: None,
                text: "Current reference".into(),
            }),
        });
        *locked(&gate.0) = true;
        gate.1.notify_all();
        assert_eq!(
            receive.recv_timeout(Duration::from_secs(2)).unwrap(),
            Some(3)
        );
        drop(service);
        assert!(receive.try_recv().is_err());
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
            // Force also turns the owner; a completed handoff may already be consumed.
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
            request.config.use_history = true;
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
        h.engine
            .begin_config_apply(
                SettingsDraft::from_configs(&h.engine.config, &c).unwrap(),
                false,
                false,
            )
            .unwrap();
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
            .apply_settings(
                SettingsDraft::from_configs(&controller.config(), &updated).unwrap(),
                false,
            )
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
                metrics: None,
            },
            WorkerEvent {
                id: 1,
                kind: "result".into(),
                text: ", thank you".into(),
                final_: true,
                model_loaded: true,
                message: String::new(),
                metrics: None,
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
        h.engine
            .begin_config_apply(SettingsDraft::new(&h.engine.config).unwrap(), false, false)
            .unwrap();
        h.engine.tick();
        assert_eq!(h.engine.status()["state"], "error");
        assert_eq!(h.native.borrow().capture_count, 0);
        assert!(!h.native.borrow().shortcuts);
        h.engine.command(Command::RetryNative);
        h.observe();
        assert!(h.native.borrow().capture_count > 0);
    }

    fn receive_config(controller: &Controller, pending: &PendingConfigApply) -> ConfigApplyReceipt {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            controller.poll();
            if let Some(receipt) = pending.try_recv().unwrap() {
                return receipt;
            }
            assert!(
                Instant::now() < deadline,
                "configuration receipt did not arrive"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn wait_validation(controller: &Controller, worker: &Rc<RefCell<WorkerState>>, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while worker.borrow().validation_configs.len() < count {
            controller.poll();
            assert!(Instant::now() < deadline, "fresh validation did not begin");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn loopback_configuration_waits_for_retirement_and_saves_without_server_validation() {
        for backend in ["ollama", "openai"] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("config.json");
            let mut h = harness(
                Config {
                    global_enabled: false,
                    ..Config::default()
                },
                None,
            );
            h.engine.readers = Box::new(PersistedServices(path.clone()));
            let controller = Controller::with_engine(h.engine).unwrap();
            for (endpoint, model) in [
                ("http://127.0.0.1:9", "unavailable-first"),
                ("http://127.0.0.1:10", "unavailable-second"),
            ] {
                let before = controller.config();
                h.worker.borrow_mut().hold_release = true;
                let mut draft = SettingsDraft::new(&before).unwrap();
                draft.set("backend", json!(backend)).unwrap();
                draft.set("endpoint", json!(endpoint)).unwrap();
                draft.set("model", json!(model)).unwrap();
                let pending = controller.apply_settings(draft, true).unwrap();
                controller.poll();
                assert!(pending.try_recv().unwrap().is_none());
                assert_eq!(controller.config(), before);
                h.worker.borrow_mut().hold_release = false;
                let ConfigApplyReceipt::Saved { config } = receive_config(&controller, &pending)
                else {
                    panic!("loopback settings must save without a server");
                };
                assert_eq!(config.backend, backend);
                assert_eq!(config.endpoint, endpoint);
                assert_eq!(config.model, model);
                assert_eq!(
                    serde_json::from_slice::<Config>(&std::fs::read(&path).unwrap()).unwrap(),
                    config
                );
                assert!(h.worker.borrow().validation_configs.is_empty());
                assert_eq!(h.worker.borrow().prepared, 0);
                assert!(h.worker.borrow().submitted.is_empty());
                assert!(pending.try_recv().is_err());
            }
        }
    }

    #[test]
    fn ordinary_model_intent_supersedes_pending_work_before_equal_or_invalid_exit() {
        for replacement in [json!("auto"), json!(false)] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("config.json");
            let mut h = harness(
                Config {
                    model_id: String::new(),
                    model_path: "/owned/custom.gguf".into(),
                    ..Config::default()
                },
                None,
            );
            h.engine.readers = Box::new(PersistedServices(path.clone()));
            h.worker.borrow_mut().validation_enabled = true;
            let controller = Controller::with_engine(h.engine).unwrap();
            let baseline = controller.config();
            let save = controller
                .apply_settings(SettingsDraft::new(&baseline).unwrap(), true)
                .unwrap();
            assert!(matches!(
                save.try_recv().unwrap(),
                Some(ConfigApplyReceipt::Saved { .. })
            ));
            let mut first = SettingsDraft::new(&baseline).unwrap();
            first.set("acceleration", json!("cpu")).unwrap();
            let pending = controller.apply_settings(first, true).unwrap();
            wait_validation(&controller, &h.worker, 1);
            let mut latest = SettingsDraft::new(&controller.config()).unwrap();
            latest.set("acceleration", replacement.clone()).unwrap();
            let latest = controller.apply_settings(latest, true).unwrap();
            assert!(matches!(
                pending.try_recv().unwrap(),
                Some(ConfigApplyReceipt::Superseded)
            ));
            assert!(pending.try_recv().is_err());
            match latest.try_recv().unwrap().unwrap() {
                ConfigApplyReceipt::Saved { config } => {
                    assert_eq!(replacement, json!("auto"));
                    assert_eq!(config, baseline);
                }
                ConfigApplyReceipt::Failed {
                    stage: ConfigApplyStage::Validation,
                    ..
                } => {
                    assert_eq!(replacement, json!(false));
                }
                other => panic!("unexpected newer-intent receipt: {other:?}"),
            }
            h.worker.borrow_mut().events.extend([
                WorkerEvent::new("prepared", 10001),
                WorkerEvent::new("done", 10001),
            ]);
            controller.poll();
            assert_eq!(controller.config(), baseline);
            assert_eq!(
                serde_json::from_slice::<Config>(&std::fs::read(path).unwrap()).unwrap(),
                baseline
            );
            assert_eq!(h.worker.borrow().validation_configs.len(), 1);
        }
    }

    #[test]
    fn deletion_during_model_validation_suppresses_only_the_exact_retained_scope() {
        for save_unrelated in [false, true] {
            for (app, host, matches_scope) in [
                (Some("writer"), None, true),
                (None, Some("EXAMPLE.test."), true),
                (None, Some("child.example.test"), false),
            ] {
                let mut h = harness(
                    Config {
                        record_history: true,
                        record_unaccepted: true,
                        ..Config::default()
                    },
                    None,
                );
                h.native.borrow_mut().snapshot.as_mut().unwrap().domain = "example.test".into();
                h.observe();
                h.worker.borrow_mut().validation_enabled = true;
                let controller = Controller::with_engine(h.engine).unwrap();
                let pending = controller
                    .select_model("/owned/pending.gguf".into(), "writing".into(), None)
                    .unwrap();
                wait_validation(&controller, &h.worker, 1);
                if save_unrelated {
                    let mut draft = SettingsDraft::new(&controller.config()).unwrap();
                    draft
                        .set("custom_instructions", json!("  exact e\u{301}\n  "))
                        .unwrap();
                    let saved = controller.apply_settings(draft, true).unwrap();
                    assert!(matches!(
                        saved.try_recv().unwrap(),
                        Some(ConfigApplyReceipt::Saved { .. })
                    ));
                }
                controller
                    .send(Command::DeleteHistory {
                        app: app.map(str::to_owned),
                        domain: host.map(str::to_owned),
                    })
                    .unwrap();
                controller.poll();
                assert!(matches!(
                    pending.try_recv().unwrap(),
                    Some(ConfigApplyReceipt::Cancelled)
                ));
                let writes = locked(&h.storage).recorded_ids.len();
                let captures = h.native.borrow().capture_count;
                *h.time.borrow_mut() += Duration::from_millis(101);
                controller.poll();
                assert!(h.native.borrow().capture_count > captures);
                controller.send(Command::Pause { minutes: 1 }).unwrap();
                controller.poll();
                let new_writes = locked(&h.storage).recorded_ids.len() - writes;
                assert_eq!(new_writes, usize::from(!matches_scope));
            }
        }
    }

    #[test]
    fn retention_after_pending_model_validation_suppresses_unchanged_session() {
        for complete in [false, true] {
            let mut h = harness(
                Config {
                    record_history: true,
                    record_unaccepted: true,
                    ..Config::default()
                },
                None,
            );
            h.offer(" ready");
            h.drawn(" ready", "completion");
            h.engine.command(Command::Action(Action::All));
            h.engine.tick();
            let id = locked(&h.storage).recorded_ids.last().unwrap().clone();
            let writes = locked(&h.storage).recorded_ids.len();
            locked(&h.storage).retention_ids.push(id);
            h.worker.borrow_mut().validation_enabled = true;
            let controller = Controller::with_engine(h.engine).unwrap();
            let pending = controller
                .select_model("/owned/pending.gguf".into(), "writing".into(), None)
                .unwrap();
            wait_validation(&controller, &h.worker, 1);
            let mut draft = SettingsDraft::new(&controller.config()).unwrap();
            draft.set("history_retention_days", json!(1)).unwrap();
            let applied = controller.apply_settings(draft, false).unwrap();
            assert!(matches!(
                applied.try_recv().unwrap(),
                Some(ConfigApplyReceipt::Applied { .. })
            ));
            if complete {
                h.worker.borrow_mut().events.extend([
                    WorkerEvent::new("prepared", 10001),
                    WorkerEvent::new("done", 10001),
                ]);
                assert!(matches!(
                    receive_config(&controller, &pending),
                    ConfigApplyReceipt::Saved { .. }
                ));
            } else {
                controller
                    .cancel_config_apply(pending.operation_id())
                    .unwrap();
                controller.poll();
            }
            *h.time.borrow_mut() += Duration::from_millis(101);
            controller.poll();
            controller.send(Command::Pause { minutes: 1 }).unwrap();
            controller.poll();
            assert_eq!(locked(&h.storage).retention_calls, 1);
            assert_eq!(locked(&h.storage).recorded_ids.len(), writes);
        }
    }

    #[test]
    fn unchanged_explicit_save_reports_persistence_failure_exactly_once() {
        let root = tempfile::tempdir().unwrap();
        let blocker = root.path().join("not-a-directory");
        std::fs::write(&blocker, b"owned fixture").unwrap();
        let mut h = harness(Config::default(), None);
        h.engine.readers = Box::new(PersistedServices(blocker.join("config.json")));
        let controller = Controller::with_engine(h.engine).unwrap();
        let baseline = controller.config();
        let pending = controller
            .apply_settings(SettingsDraft::new(&baseline).unwrap(), true)
            .unwrap();
        assert!(matches!(
            pending.try_recv().unwrap(),
            Some(ConfigApplyReceipt::Failed {
                stage: ConfigApplyStage::Persistence,
                ..
            })
        ));
        assert_eq!(controller.config(), baseline);
        assert!(pending.try_recv().is_err());
        controller
            .cancel_config_apply(pending.operation_id())
            .unwrap();
        assert!(
            controller
                .poll()
                .iter()
                .all(|event| event["type"] != "error")
        );
    }

    #[test]
    fn model_validation_owns_worker_without_capture_and_rebases_unrelated_save() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.json");
        let mut h = harness(Config::default(), None);
        h.engine.readers = Box::new(PersistedServices(path.clone()));
        h.worker.borrow_mut().validation_enabled = true;
        let controller = Controller::with_engine(h.engine).unwrap();
        let pending = controller
            .select_model(
                "/owned/custom-model.gguf".into(),
                "continuation".into(),
                None,
            )
            .unwrap();
        wait_validation(&controller, &h.worker, 1);
        assert!(!path.exists());
        assert!(h.worker.borrow().submitted.is_empty());
        assert!(h.worker.borrow().proceeds.is_empty());
        assert_eq!(h.native.borrow().capture_count, 0);
        let mut edit = SettingsDraft::new(&controller.config()).unwrap();
        edit.set(
            "custom_instructions",
            json!("  português e\u{301}\nexact bytes  "),
        )
        .unwrap();
        let independent = controller.apply_settings(edit, true).unwrap();
        assert!(matches!(
            independent.try_recv().unwrap(),
            Some(ConfigApplyReceipt::Saved { .. })
        ));
        h.worker.borrow_mut().events.extend([
            WorkerEvent::new("prepared", 10001),
            WorkerEvent::new("done", 10001),
        ]);
        let ConfigApplyReceipt::Saved { config } = receive_config(&controller, &pending) else {
            panic!("expected saved model");
        };
        assert_eq!(config.model_path, "/owned/custom-model.gguf");
        assert_eq!(
            config.custom_instructions,
            "  português e\u{301}\nexact bytes  "
        );
        assert_eq!(
            serde_json::from_slice::<Config>(&std::fs::read(path).unwrap()).unwrap(),
            config
        );
        assert_eq!(h.worker.borrow().validation_configs.len(), 1);
        assert!(pending.try_recv().is_err());
    }

    #[test]
    fn superseded_cancelled_and_stopped_operations_ignore_late_validation_events() {
        let h = harness(Config::default(), None);
        h.worker.borrow_mut().validation_enabled = true;
        let mut controller = Controller::with_engine(h.engine).unwrap();
        let original = controller.config();
        let first = controller
            .select_model("/owned/first.gguf".into(), "continuation".into(), None)
            .unwrap();
        wait_validation(&controller, &h.worker, 1);
        let second = controller
            .select_model("/owned/second.gguf".into(), "continuation".into(), None)
            .unwrap();
        assert!(matches!(
            first.try_recv().unwrap(),
            Some(ConfigApplyReceipt::Superseded)
        ));
        h.worker.borrow_mut().events.extend([
            WorkerEvent::new("prepared", 10001),
            WorkerEvent::new("done", 10001),
        ]);
        wait_validation(&controller, &h.worker, 2);
        controller.poll();
        assert!(second.try_recv().unwrap().is_none());
        assert_eq!(controller.config(), original);
        controller
            .cancel_config_apply(first.operation_id())
            .unwrap();
        assert!(second.try_recv().unwrap().is_none());
        controller
            .cancel_config_apply(second.operation_id())
            .unwrap();
        assert!(matches!(
            second.try_recv().unwrap(),
            Some(ConfigApplyReceipt::Cancelled)
        ));
        assert!(second.try_recv().is_err());
        let third = controller
            .select_model("/owned/third.gguf".into(), "continuation".into(), None)
            .unwrap();
        let _ = controller.shutdown().unwrap();
        assert!(matches!(
            third.try_recv().unwrap(),
            Some(ConfigApplyReceipt::Stopped)
        ));
        assert_eq!(controller.config(), original);
    }

    #[test]
    fn ordinary_zero_prepare_and_done_without_prepared_never_validate_a_model() {
        for fresh_supported in [false, true] {
            let h = harness(Config::default(), None);
            h.worker.borrow_mut().validation_enabled = fresh_supported;
            let controller = Controller::with_engine(h.engine).unwrap();
            let original = controller.config();
            let pending = controller
                .select_model("/owned/rejected.gguf".into(), "continuation".into(), None)
                .unwrap();
            if fresh_supported {
                wait_validation(&controller, &h.worker, 1);
                h.worker
                    .borrow_mut()
                    .events
                    .push(WorkerEvent::new("done", 10001));
            }
            assert!(matches!(
                receive_config(&controller, &pending),
                ConfigApplyReceipt::Failed {
                    stage: ConfigApplyStage::Preparation,
                    ..
                }
            ));
            assert_eq!(controller.config(), original);
            assert_eq!(h.worker.borrow().prepared, 0);
        }
    }

    #[test]
    fn identical_model_selection_revalidates_and_catalogue_mismatch_never_prepares() {
        let root = tempfile::tempdir().unwrap();
        let bad = root.path().join("mismatch.gguf");
        std::fs::write(&bad, b"not catalogue bytes").unwrap();
        let config = Config {
            model_id: String::new(),
            model_path: "/owned/same.gguf".into(),
            ..Config::default()
        };
        let h = harness(config.clone(), None);
        h.worker.borrow_mut().validation_enabled = true;
        h.worker.borrow_mut().validation_auto = true;
        let controller = Controller::with_engine(h.engine).unwrap();
        let same = controller
            .select_model(config.model_path.clone(), config.prompt_style.clone(), None)
            .unwrap();
        assert!(matches!(
            receive_config(&controller, &same),
            ConfigApplyReceipt::Saved { .. }
        ));
        assert_eq!(h.worker.borrow().validation_configs.len(), 1);
        let mismatch = controller
            .select_model(
                bad.to_string_lossy().into_owned(),
                "writing".into(),
                Some("tiny".into()),
            )
            .unwrap();
        assert!(matches!(
            receive_config(&controller, &mismatch),
            ConfigApplyReceipt::Failed {
                stage: ConfigApplyStage::Integrity,
                ..
            }
        ));
        assert_eq!(h.worker.borrow().validation_configs.len(), 1);
        assert_eq!(controller.config().model_path, config.model_path);
    }

    #[test]
    fn preview_ownership_refuses_model_selection_and_spelling_policy_is_exact() {
        let state = Arc::new(Mutex::new(SpellState::default()));
        let config = Config {
            spelling_language: "pt-BR".into(),
            ..Config::default()
        };
        let h = harness(config, Some(Arc::new(Dictionary(state.clone()))));
        assert_eq!(locked(&state).language, "pt-BR");
        let controller = Controller::with_engine(h.engine).unwrap();
        controller.send(Command::SuspendForPreview).unwrap();
        controller.poll();
        let pending = controller
            .select_model("/owned/model.gguf".into(), "writing".into(), None)
            .unwrap();
        assert!(matches!(
            pending.try_recv().unwrap(),
            Some(ConfigApplyReceipt::Failed {
                stage: ConfigApplyStage::Ownership,
                ..
            })
        ));
        let mut draft = SettingsDraft::new(&controller.config()).unwrap();
        draft.set("spelling_language", json!("pt-PT")).unwrap();
        assert!(matches!(
            controller
                .apply_settings(draft, false)
                .unwrap()
                .try_recv()
                .unwrap(),
            Some(ConfigApplyReceipt::Applied { .. })
        ));
        assert_eq!(locked(&state).language, "pt-PT");
        assert!(h.worker.borrow().validation_configs.is_empty());
    }

    #[test]
    fn failed_regional_policy_save_preserves_committed_dictionary_and_configuration() {
        let root = tempfile::tempdir().unwrap();
        let blocker = root.path().join("not-a-directory");
        std::fs::write(&blocker, b"owned fixture").unwrap();
        let state = Arc::new(Mutex::new(SpellState::default()));
        let mut h = harness(
            Config {
                spelling_language: "pt-BR".into(),
                typo_fixes: true,
                ..Config::default()
            },
            Some(Arc::new(Dictionary(state.clone()))),
        );
        h.engine.readers = Box::new(PersistedServices(blocker.join("config.json")));
        let controller = Controller::with_engine(h.engine).unwrap();
        let before = controller.config();
        let mut draft = SettingsDraft::new(&before).unwrap();
        draft.set("spelling_language", json!("pt-PT")).unwrap();
        let pending = controller.apply_settings(draft, true).unwrap();
        assert!(matches!(
            pending.try_recv().unwrap(),
            Some(ConfigApplyReceipt::Failed {
                stage: ConfigApplyStage::Persistence,
                ..
            })
        ));
        assert_eq!(controller.config(), before);
        assert_eq!(locked(&state).language, "pt-BR");
        assert_eq!(locked(&state).close_calls, 0);
    }

    #[test]
    fn invalid_domain_deletion_has_no_side_effect_and_unicode_domain_is_canonical() {
        let h = harness(Config::default(), None);
        let controller = Controller::with_engine(h.engine).unwrap();
        controller
            .send(Command::DeleteHistory {
                app: None,
                domain: Some("https://example.test/path".into()),
            })
            .unwrap();
        let invalid = controller.poll();
        assert!(invalid.iter().any(|event| event["type"] == "error"));
        assert_eq!(locked(&h.storage).deletes, 0);
        assert_eq!(h.worker.borrow().released, 0);
        controller
            .send(Command::DeleteHistory {
                app: None,
                domain: Some("BÜCHER.example.".into()),
            })
            .unwrap();
        let deleted = controller
            .poll()
            .into_iter()
            .find(|event| event["type"] == "history_deleted")
            .unwrap();
        assert_eq!(deleted["domain"], "xn--bcher-kva.example");
        assert_eq!(locked(&h.storage).deletes, 1);
    }

    #[test]
    fn metadata_only_wake_preserves_native_input_time_and_unknown_is_not_zero() {
        for observed_input in [false, true] {
            let mut h = harness(Config::default(), None);
            let input_at = h.engine.now();
            if observed_input {
                h.native.borrow_mut().input_at = Some(input_at);
            }
            h.elapse(7);
            let wake = h.native.borrow().wake.clone().unwrap();
            wake();
            h.elapse(4);
            h.force();
            let controller = Controller::with_engine(h.engine).unwrap();
            let snapshot = controller.diagnostics();
            let trace = snapshot.traces.last().unwrap();
            assert_eq!(trace.input_wait_us, observed_input.then_some(11000));
        }
    }

    #[test]
    fn native_confirmation_finalizes_original_trace_after_view_replacement() {
        let mut h = harness(
            Config {
                trailing_space: false,
                ..Config::default()
            },
            None,
        );
        let (before, receipt) = client_offer(&mut h);
        let trace_id = h.engine.diagnostic_traces[PresentationLane::Inline as usize].unwrap();
        let accepted_at = h.engine.now();
        h.engine.clear(false, true);
        h.offer(" elsewhere");
        let replacement = h.engine.diagnostic_traces[PresentationLane::Inline as usize].unwrap();
        assert_ne!(trace_id, replacement);
        let after = field("The update is ready");
        h.native.borrow_mut().host_events.push(client_applied(
            &before,
            &after,
            &receipt,
            accepted_at,
        ));
        h.engine.consume_host_events();
        let controller = Controller::with_engine(h.engine).unwrap();
        let snapshot = controller.diagnostics();
        assert_eq!(
            snapshot
                .traces
                .iter()
                .find(|trace| trace.trace_id == trace_id)
                .unwrap()
                .outcome,
            Some(crate::diagnostics::Outcome::Accepted)
        );
        assert_eq!(
            snapshot
                .traces
                .iter()
                .find(|trace| trace.trace_id == replacement)
                .unwrap()
                .outcome,
            None
        );
    }

    #[test]
    fn ready_turn_retains_standalone_preparation_causes_without_capture() {
        let h = harness(Config::default(), None);
        let now = h.engine.now();
        for (index, reason) in [
            crate::diagnostics::TerminalReason::Completed,
            crate::diagnostics::TerminalReason::Failed,
            crate::diagnostics::TerminalReason::Released,
        ]
        .into_iter()
        .enumerate()
        {
            h.worker
                .borrow_mut()
                .terminal_reports
                .push_back(TerminalReport {
                    request_id: index as u64 + 1,
                    preparing: true,
                    metrics: None,
                    cancelled: reason == crate::diagnostics::TerminalReason::Released,
                    failed: reason == crate::diagnostics::TerminalReason::Failed,
                    reason,
                    submitted: now,
                    ready: None,
                    proceed: None,
                    first_result: None,
                    terminal: now + Duration::from_micros(23),
                });
        }
        let controller = Controller::with_engine(h.engine).unwrap();
        controller.engine.borrow().wake.signal();
        controller.poll_ready();
        assert_eq!(h.native.borrow().capture_count, 0);
        let snapshot = controller.diagnostics();
        assert!(snapshot.traces.is_empty());
        assert_eq!(snapshot.preparations.len(), 3);
        assert_eq!(snapshot.unmatched_reports, 0);
        assert_eq!(snapshot.preparations[0].terminal_us, Some(23));
        assert!(snapshot.preparations[1].failed);
        assert!(snapshot.preparations[2].cancelled);
    }

    #[test]
    fn publication_receipt_is_not_paint_and_old_lane_view_cannot_paint_replacement() {
        let mut h = harness(Config::default(), None);
        h.force();
        h.prepared();
        h.response("ready", "", false);
        h.response("result", " ready", false);
        let offered = h
            .engine
            .drain_events()
            .into_iter()
            .filter(|event| event["type"] == "suggestion")
            .last()
            .unwrap();
        h.acknowledge_offer(&offered);
        let controller = Controller::with_engine(h.engine).unwrap();
        let trace = offered["diagnostic"]["trace_id"].as_u64().unwrap();
        let view = offered["diagnostic"]["view_serial"].as_u64().unwrap();
        assert!(
            controller
                .diagnostics()
                .traces
                .iter()
                .find(|item| item.trace_id == trace)
                .unwrap()
                .first_draw_us
                .is_none()
        );
        let reporter = controller.paint_reporter();
        reporter(PaintObservation {
            trace_id: trace,
            lane: PresentationLane::Chooser,
            view_serial: view,
            painted_at: *h.time.borrow(),
        });
        assert!(
            controller
                .diagnostics()
                .traces
                .iter()
                .find(|item| item.trace_id == trace)
                .unwrap()
                .first_draw_us
                .is_none()
        );
        let notification = h.native.borrow().wake.clone().unwrap();
        notification();
        reporter(PaintObservation {
            trace_id: trace,
            lane: PresentationLane::Inline,
            view_serial: view,
            painted_at: *h.time.borrow(),
        });
        assert!(
            controller
                .diagnostics()
                .traces
                .iter()
                .find(|item| item.trace_id == trace)
                .unwrap()
                .first_draw_us
                .is_none()
        );
        assert_eq!(controller.diagnostics().rejected_draws, 2);
    }

    #[test]
    fn history_use_does_not_imply_recording_and_source_denials_are_independent() {
        let config = Config {
            use_history: true,
            record_history: false,
            screen_context: true,
            clipboard_context: true,
            screenshot_context: false,
            screenshot_appearance: true,
            ..Config::default()
        };
        let h = harness(config, None);
        let controller = Controller::with_engine(h.engine).unwrap();
        let mut draft = SettingsDraft::new(&controller.config()).unwrap();
        draft.set("app_rules", json!({"app:writer":{"screen_context":false,"clipboard_context":false,"screenshot_context":true}})).unwrap();
        let pending = controller.apply_settings(draft, false).unwrap();
        assert!(matches!(
            pending.try_recv().unwrap(),
            Some(ConfigApplyReceipt::Applied { .. })
        ));
        controller.send(Command::Action(Action::Force)).unwrap();
        controller.poll();
        let request = h.preparation.borrow_mut().pending.take().unwrap();
        assert!(!request.config.screen_context);
        assert!(!request.config.clipboard_context);
        assert!(!request.config.screenshot_context);
        assert!(request.config.screenshot_appearance);
        assert!(request.config.use_history);
        assert!(!request.config.record_history);
        controller.send(Command::HistoryInfo).unwrap();
        assert!(
            controller
                .poll()
                .iter()
                .any(|event| event["type"] == "history_info" && event["result"]["count"] == 0)
        );
    }

    #[test]
    fn model_save_failure_retires_prepared_owner_without_installing_candidate() {
        let root = tempfile::tempdir().unwrap();
        let blocker = root.path().join("not-directory");
        std::fs::write(&blocker, b"owned fixture").unwrap();
        let mut h = harness(Config::default(), None);
        h.engine.readers = Box::new(PersistedServices(blocker.join("settings.json")));
        h.worker.borrow_mut().validation_enabled = true;
        h.worker.borrow_mut().validation_auto = true;
        let controller = Controller::with_engine(h.engine).unwrap();
        let before = controller.config();
        let pending = controller
            .select_model("/owned/prepared.gguf".into(), "continuation".into(), None)
            .unwrap();
        assert!(matches!(
            receive_config(&controller, &pending),
            ConfigApplyReceipt::Failed {
                stage: ConfigApplyStage::Persistence,
                ..
            }
        ));
        assert_eq!(controller.config(), before);
        assert_eq!(h.worker.borrow().validation_configs.len(), 1);
        assert!(h.worker.borrow().released >= 2);
        assert!(h.worker.borrow().submitted.is_empty());
        assert!(!blocker.join("settings.json").exists());
    }

    #[test]
    fn retention_zero_does_nothing_and_deleted_session_cannot_requeue_itself() {
        let config = Config {
            record_history: true,
            record_unaccepted: true,
            ..Config::default()
        };
        let mut h = harness(config, None);
        h.offer(" ready");
        h.drawn(" ready", "completion");
        h.engine.command(Command::Action(Action::All));
        h.engine.tick();
        assert_eq!(locked(&h.storage).retention_calls, 0);
        let id = locked(&h.storage).recorded_ids.last().unwrap().clone();
        let writes = locked(&h.storage).recorded_ids.len();
        let statistics = locked(&h.storage).stats.len();
        locked(&h.storage).retention_ids.push(id);
        let controller = Controller::with_engine(h.engine).unwrap();
        let mut draft = SettingsDraft::new(&controller.config()).unwrap();
        draft.set("history_retention_days", json!(1)).unwrap();
        assert!(matches!(
            controller
                .apply_settings(draft, false)
                .unwrap()
                .try_recv()
                .unwrap(),
            Some(ConfigApplyReceipt::Applied { .. })
        ));
        let events = controller.poll();
        assert!(
            events
                .iter()
                .any(|event| event["type"] == "history_retained" && event["deleted_count"] == 1)
        );
        controller.send(Command::Pause { minutes: 1 }).unwrap();
        controller.poll();
        assert_eq!(locked(&h.storage).retention_calls, 1);
        assert_eq!(locked(&h.storage).recorded_ids.len(), writes);
        assert_eq!(locked(&h.storage).stats.len(), statistics);
    }

    #[test]
    fn retained_ghost_and_alternative_work_have_independent_diagnostic_lanes() {
        let config = Config {
            alternatives_enabled: true,
            ..Config::default()
        };
        let mut h = harness(config, None);
        h.offer(" ready tomorrow");
        h.drawn(" ready tomorrow", "completion");
        h.type_text("The update ready", 10);
        h.drawn(" tomorrow", "completion");
        h.engine.command(Command::Action(Action::Alternatives));
        h.engine.tick();
        h.prepared();
        h.response("ready", "", false);
        h.response("result", "tomorrow\nsoon", true);
        let controller = Controller::with_engine(h.engine).unwrap();
        let snapshot = controller.diagnostics();
        assert!(
            snapshot
                .traces
                .iter()
                .any(|trace| trace.kind == WorkKind::RetainedGhost
                    && trace.lane == PresentationLane::Inline
                    && trace.request_id.is_none())
        );
        assert!(
            snapshot
                .traces
                .iter()
                .any(|trace| trace.kind == WorkKind::Alternative
                    && trace.lane == PresentationLane::Chooser
                    && trace.request_id == Some(2))
        );
        assert!(
            snapshot
                .traces
                .iter()
                .any(|trace| trace.kind == WorkKind::Generated && trace.request_id == Some(1))
        );
    }

    #[test]
    fn identical_pending_publication_preserves_its_receipt_until_final_and_done() {
        let mut h = harness(Config::default(), None);
        h.force();
        h.prepared();
        h.response("ready", "", false);
        h.response("result", " ready tomorrow", false);
        let offered = h
            .engine
            .drain_events()
            .into_iter()
            .find(|event| event["type"] == "suggestion")
            .unwrap();
        // Both repeated streaming output and finalization must retain this
        // exact publication even before the overlay acknowledges its receipt.
        h.response("result", " ready tomorrow", false);
        h.response("result", " ready tomorrow", true);
        h.response("done", "", false);
        assert!(
            h.engine
                .drain_events()
                .iter()
                .all(|event| event["type"] != "suggestion")
        );
        h.acknowledge_offer(&offered);
        h.engine.command(Command::Action(Action::All));
        assert_eq!(
            h.native.borrow().snapshot.as_ref().unwrap().text.as_ref(),
            "The update ready tomorrow"
        );
    }

    #[test]
    fn generated_final_space_refreshes_payload_once_before_native_acknowledgment() {
        let mut h = harness(
            Config {
                trailing_space: false,
                ..Config::default()
            },
            None,
        );
        h.force();
        h.prepared();
        h.response("ready", "", false);
        h.response("result", " ready tomorrow", false);
        let original = h
            .engine
            .drain_events()
            .into_iter()
            .find(|event| event["type"] == "suggestion")
            .unwrap();
        h.acknowledge_offer(&original);
        h.response("result", " ready tomorrow ", false);
        h.response("result", " ready tomorrow ", true);
        h.response("done", "", false);
        let publications: Vec<_> = h
            .engine
            .drain_events()
            .into_iter()
            .filter(|event| event["type"] == "suggestion")
            .collect();
        assert_eq!(publications.len(), 1);
        let final_offer = &publications[0];
        assert_eq!(final_offer["text"], " ready tomorrow ");
        assert_eq!(final_offer["key"], original["key"]);
        assert_eq!(final_offer["authority"], original["authority"]);
        assert_ne!(final_offer["revision"], original["revision"]);
        h.acknowledge_offer(&original);
        h.engine.command(Command::Action(Action::All));
        assert_eq!(
            h.native.borrow().snapshot.as_ref().unwrap().text.as_ref(),
            "The update"
        );
        h.acknowledge_offer(final_offer);
        h.engine.command(Command::Action(Action::All));
        assert_eq!(
            h.native.borrow().snapshot.as_ref().unwrap().text.as_ref(),
            "The update ready tomorrow"
        );
    }

    #[test]
    fn identical_final_preserves_current_cropped_ack_without_republication() {
        let mut h = harness(Config::default(), None);
        h.force();
        h.prepared();
        h.response("ready", "", false);
        h.response("result", " ready tomorrow", false);
        h.drawn(" ready", "completion");
        h.engine.drain_events();
        h.response("result", " ready tomorrow", true);
        assert!(
            h.engine
                .drain_events()
                .iter()
                .all(|event| event["type"] != "suggestion")
        );
        h.engine.command(Command::Action(Action::All));
        assert_eq!(
            h.native.borrow().snapshot.as_ref().unwrap().text.as_ref(),
            "The update ready "
        );
    }

    #[test]
    fn disabling_retention_cancels_pending_release_without_deleting() {
        let config = Config {
            history_retention_days: 1,
            ..Config::default()
        };
        let h = harness(config, None);
        h.worker.borrow_mut().hold_release = true;
        let controller = Controller::with_engine(h.engine).unwrap();
        controller.poll();
        assert_eq!(locked(&h.storage).retention_calls, 0);
        assert_eq!(h.native.borrow().capture_count, 0);
        let mut draft = SettingsDraft::new(&controller.config()).unwrap();
        draft.set("history_retention_days", json!(0)).unwrap();
        assert!(matches!(
            controller
                .apply_settings(draft, false)
                .unwrap()
                .try_recv()
                .unwrap(),
            Some(ConfigApplyReceipt::Applied { .. })
        ));
        h.worker.borrow_mut().hold_release = false;
        controller.poll();
        assert_eq!(locked(&h.storage).retention_calls, 0);
    }

    #[test]
    fn unavailable_browser_domain_denies_scoped_sources_and_instruction_generation() {
        let mut config = Config {
            use_history: true,
            record_history: true,
            clipboard_context: true,
            ..Config::default()
        };
        config.app_rules.insert(
            "domain:private.test".into(),
            BTreeMap::from([
                ("use_history".into(), json!(false)),
                ("record_history".into(), json!(false)),
                ("clipboard_context".into(), json!(false)),
                (
                    "custom_instructions".into(),
                    json!("Private-site instruction"),
                ),
            ]),
        );
        let h = harness(config, None);
        {
            let mut native = h.native.borrow_mut();
            let snapshot = native.snapshot.as_mut().unwrap();
            snapshot.app_id = "custom-browser-app".into();
            let metadata = Foreground {
                app_id: snapshot.app_id.clone(),
                executable: "chrome".into(),
                window_id: snapshot.window_id.clone(),
                window_pid: snapshot.window_pid,
                ..Default::default()
            };
            native.metadata = Some(metadata);
        }
        let controller = Controller::with_engine(h.engine).unwrap();
        controller.send(Command::Action(Action::Force)).unwrap();
        controller.poll();
        let status = controller.status();
        assert_eq!(status["current_browser"], true);
        assert_eq!(status["current_domain_available"], false);
        assert_eq!(
            status["current_preferences"]["use_history"]["origin"],
            "domain_unavailable"
        );
        assert_eq!(
            status["current_preferences"]["record_history"]["value"],
            false
        );
        assert_eq!(
            status["current_preferences"]["clipboard_context"]["value"],
            false
        );
        assert!(h.preparation.borrow().pending.is_none());
        assert!(h.worker.borrow().submitted.is_empty());
        assert!(
            controller
                .diagnostics()
                .traces
                .iter()
                .any(|trace| trace.outcome == Some(crate::diagnostics::Outcome::PrivacyDenied))
        );
    }

    #[test]
    fn invalid_new_model_intent_supersedes_inflight_selection() {
        let h = harness(Config::default(), None);
        h.worker.borrow_mut().validation_enabled = true;
        let controller = Controller::with_engine(h.engine).unwrap();
        let before = controller.config();
        let pending = controller
            .select_model("/owned/first.gguf".into(), "continuation".into(), None)
            .unwrap();
        wait_validation(&controller, &h.worker, 1);
        let invalid = controller
            .select_model("/owned/second.gguf".into(), "not-a-template".into(), None)
            .unwrap();
        assert!(matches!(
            pending.try_recv().unwrap(),
            Some(ConfigApplyReceipt::Superseded)
        ));
        assert!(matches!(
            invalid.try_recv().unwrap(),
            Some(ConfigApplyReceipt::Failed {
                stage: ConfigApplyStage::Validation,
                ..
            })
        ));
        h.worker.borrow_mut().events.extend([
            WorkerEvent::new("prepared", 10001),
            WorkerEvent::new("done", 10001),
        ]);
        controller.poll();
        assert_eq!(controller.config(), before);
    }
}
