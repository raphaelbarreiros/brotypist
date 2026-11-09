//! Single-main-thread macOS AX authority with independent, capture-free Quartz hooks.
#[path = "macos_ax.rs"]
pub mod ax;
#[path = "macos_docs.rs"]
pub mod docs;
#[path = "macos_geometry.rs"]
pub mod geometry;
#[path = "macos_input.rs"]
pub mod input;
#[path = "macos_language.rs"]
pub mod language;
#[path = "macos_special.rs"]
pub mod special;
#[path = "macos_spelling.rs"]
pub mod spelling;
#[path = "macos_terminal.rs"]
pub mod terminal;
#[path = "macos_vision.rs"]
pub mod vision;

use super::{
    ContextRead, EditOutcome, Foreground, NativeBridge, Permission, ShortcutHandler,
    policy::{ObservationPolicy, is_browser, native_domain},
};
use crate::{
    config::{Config, ScopedPreference},
    core::{InlineRender, Rect, ScalarIndex, Snapshot, TextBuffer},
};
use anyhow::{Result, anyhow, bail, ensure};
use ax::ffi::{
    CGEventCreateKeyboardEvent, CGEventSetFlags, CGRequestListenEventAccess,
    CGRequestPostEventAccess, CGRequestScreenCaptureAccess, ObjcRef, class_addIvar,
    class_addMethod, objc_allocateClassPair, objc_autoreleasePoolPop, objc_autoreleasePoolPush,
    objc_getClass, objc_msgSend, objc_registerClassPair, objc_release, object_getInstanceVariable,
    object_setInstanceVariable, sel_registerName,
};
use ax::{AxElement, AxObservation, AxRange, AxValue, Raw};
use blake2::{
    Blake2sMac,
    digest::{KeyInit, Mac, consts::U16},
};
use input::{Input, InputSourceKind};
use rand::RngCore;
use std::{
    collections::BTreeMap,
    ffi::c_void,
    marker::PhantomData,
    rc::Rc,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, ThreadId},
    time::{Duration, Instant},
};

const TERMINALS: &[&str] = &["com.apple.Terminal", "com.googlecode.iterm2"];
const CHAT_BUNDLES: &[&str] = &[
    "com.hnc.discord",
    "com.hammerandchisel.discord",
    "com.tinyspeck.slackmacgap",
];
const CHAT_EDITORS: &[&str] = &["com.microsoft.vscode", "com.microsoft.vscodeinsiders"];
/// Distributed notification requesting one content-free WhatsApp capture export.
const WHATSAPP_DIAGNOSTIC_NOTIFICATION: &str = "BroTypistExportWhatsAppDiagnostic";
const CHAT_NAMES: &[&str] = &[
    "Chat Input (Agent), edit files in your workspace.",
    "Chat Input (Edit), edit files in your workspace.",
    "Chat Input (Ask), ask questions or type / for topics.",
];

/// Native metadata only; browser document URLs never leave native local reads.
#[derive(Clone, Debug)]
pub struct MacSession {
    pub foreground: Foreground,
    pub name: String,
    pub app: AxElement,
    pub window: Option<AxElement>,
    pub document_id: String,
}
impl MacSession {
    pub fn same_owner(&self, other: &Self) -> bool {
        self.foreground.app_id == other.foreground.app_id
            && self.foreground.window_pid == other.foreground.window_pid
            && self.foreground.window_id == other.foreground.window_id
            && self.foreground.domain == other.foreground.domain
            && self.document_id == other.document_id
    }
}
/// Special native adapters are required implementations, never broad AX fallbacks.
pub trait TerminalIntegration {
    fn configure(&mut self, config: &Config) -> Result<()>;
    fn capture(
        &mut self,
        services: &mut CaptureServices<'_>,
        session: &MacSession,
    ) -> Result<Option<Snapshot>>;
    fn append(
        &mut self,
        services: &mut CaptureServices<'_>,
        snapshot: &Snapshot,
        text: &str,
    ) -> Result<EditOutcome>;
    fn context_matches(
        &mut self,
        services: &mut CaptureServices<'_>,
        snapshot: &Snapshot,
    ) -> Result<bool>;
    fn context_ticket(
        &mut self,
        services: &mut CaptureServices<'_>,
        snapshot: &Snapshot,
    ) -> Result<Option<Box<dyn super::DetachedContextRead>>>;
    fn permission_state(&mut self, app_id: &str) -> Result<String>;
    fn request_access(&mut self, app_id: &str) -> Result<String>;
    fn stop(&mut self) -> Result<()>;
}
pub trait AppearanceIntegration {
    fn configure(&mut self, config: &Config) -> Result<()>;
    fn style(
        &mut self,
        services: &mut CaptureServices<'_>,
        snapshot: &Snapshot,
        evidence: &geometry::AppearanceEvidence,
    ) -> Result<Option<InlineRender>>;
    fn stop(&mut self) -> Result<()>;
}
/// Borrowed access proves all special native work executes on the same owner.
pub struct CaptureServices<'a> {
    bridge: &'a mut MacBridge,
}
impl CaptureServices<'_> {
    pub fn session(&mut self) -> Result<Option<MacSession>> {
        self.bridge.session()
    }
    pub fn allowed(&self, session: &MacSession, editor_probe: bool) -> bool {
        self.bridge.allowed(session, editor_probe)
    }
    pub fn secure_input(&self) -> Option<bool> {
        secure_event_input_enabled()
    }
    pub fn dead_key(&self) -> bool {
        self.bridge.input.dead_key()
    }
    pub fn source_kind(&self) -> InputSourceKind {
        self.bridge.input.source_kind()
    }
    pub fn secure_description(&self) -> &str {
        &self.bridge.secure_description
    }
    pub fn document_identity(&self, value: &str) -> String {
        self.bridge.document_identity(value)
    }
    pub fn field_identity(&self, field: &AxElement, session: &MacSession) -> Option<String> {
        self.bridge.field_identity(field, session)
    }
    pub fn context_matches(&mut self, snapshot: &Snapshot) -> Result<bool> {
        self.bridge.context_matches(snapshot)
    }
    /// Optional-source consent plus independent native snapshot authority.
    pub fn preference_allowed(
        &mut self,
        snapshot: &Snapshot,
        preference: ScopedPreference,
    ) -> Result<bool> {
        Ok(self.preference_consent(snapshot, preference)?
            && self.bridge.context_matches(snapshot)?)
    }
    /// Metadata-only source admission. Special adapters must also validate their
    /// own signed primary-target authority; calling back through the bridge
    /// would lose that authority while the adapter is borrowed out of it.
    pub fn preference_consent(
        &mut self,
        snapshot: &Snapshot,
        preference: ScopedPreference,
    ) -> Result<bool> {
        let Some(session) = self.bridge.session()? else {
            return Ok(false);
        };
        Ok(source_preference_allowed(
            &self.bridge.policy,
            &session.foreground,
            &session.name,
            snapshot,
            preference,
        ))
    }
    pub fn commit_input(&self) -> bool {
        self.bridge.input.commit()
    }
    pub fn preflight_unicode(&self) -> Result<()> {
        self.bridge.input.preflight_unicode()
    }
    pub fn post_unicode(&self, text: &str) -> Result<()> {
        self.bridge.input.preflight_unicode()?;
        self.bridge.input.post_unicode(text)
    }
    pub fn input_revision(&self) -> u64 {
        self.bridge.input.revision()
    }
    pub fn detached_lease(&self, session: &MacSession, snapshot: &Snapshot) -> NativeReadLease {
        self.bridge.detached_lease(session, snapshot)
    }
}

fn source_preference_allowed(
    policy: &ObservationPolicy,
    front: &Foreground,
    name: &str,
    snapshot: &Snapshot,
    preference: ScopedPreference,
) -> bool {
    front.app_id == snapshot.app_id
        && front.domain == snapshot.domain
        && policy
            .preference(
                preference,
                &front.app_id,
                &front.domain,
                &[&front.executable, name],
            )
            .value
}

struct Focused {
    field: AxElement,
    session: MacSession,
    field_id: String,
}
impl Focused {
    fn same(&self, other: &Self) -> bool {
        self.field == other.field
            && self.field_id == other.field_id
            && self.session.same_owner(&other.session)
    }
}

/// A detached read capability contains no draft, AX/Objective-C handle or owner.
/// Each worker creates and drops its own AX references on that worker thread.
#[derive(Clone)]
pub struct NativeReadLease {
    pub foreground: Foreground,
    pub name: String,
    secure_description: String,
    source_kind: InputSourceKind,
    salt: [u8; 32],
    policy: Arc<ObservationPolicy>,
    epoch: Arc<AtomicU64>,
    issued_epoch: u64,
    revision: Arc<dyn Fn() -> u64 + Send + Sync>,
    issued_revision: u64,
    dirty: Arc<AtomicBool>,
    snapshot_binding: [u8; 16],
}
impl NativeReadLease {
    pub fn matches_snapshot(&self, snapshot: &Snapshot) -> bool {
        snapshot_binding(&self.salt, snapshot) == self.snapshot_binding
    }
    pub fn source_kind(&self) -> InputSourceKind {
        self.source_kind
    }
    pub fn secure_description(&self) -> &str {
        &self.secure_description
    }
    pub fn preference_allowed(&self, preference: ScopedPreference) -> bool {
        self.valid()
            && self
                .policy
                .preference(
                    preference,
                    &self.foreground.app_id,
                    &self.foreground.domain,
                    &[&self.foreground.executable, &self.name],
                )
                .value
    }
    pub fn valid(&self) -> bool {
        self.metadata_valid() && secure_event_input_enabled() == Some(false) && ax::trusted()
    }
    /// Primary-target policy and revision authority, independent of optional
    /// source admission. Native secure-input and AX checks remain mandatory.
    fn metadata_valid(&self) -> bool {
        self.epoch.load(Ordering::Acquire) == self.issued_epoch
            && (self.revision)() == self.issued_revision
            && !self.dirty.load(Ordering::Acquire)
            && self.policy.allowed(
                &self.foreground.app_id,
                &self.foreground.domain,
                &[&self.foreground.executable, &self.name],
                exact_member(&self.foreground.app_id, CHAT_EDITORS),
            )
    }
}
#[derive(Clone)]
pub struct MacContextTicket {
    lease: NativeReadLease,
    document_id: String,
    field_id: String,
    field_hash: u64,
    window_hash: u64,
}
impl MacContextTicket {
    pub fn matches_snapshot(&self, snapshot: &Snapshot) -> bool {
        self.lease.matches_snapshot(snapshot)
    }
    fn lease_valid(&self) -> bool {
        self.lease.valid()
    }
    fn resolve(&self) -> Result<Option<(AxElement, AxElement)>> {
        if !self.lease_valid() {
            return Ok(None);
        }
        let Some(system) = AxElement::system_wide() else {
            return Ok(None);
        };
        system.set_timeout(0.25)?;
        // An application's stale "focused field" is not foreground authority.
        // System AX must independently identify this ticket's exact process.
        let Some(foreground) = system.element("AXFocusedApplication") else {
            return Ok(None);
        };
        if foreground.pid() != Some(self.lease.foreground.window_pid) {
            return Ok(None);
        }
        let Some(app) = AxElement::application(self.lease.foreground.window_pid) else {
            return Ok(None);
        };
        app.set_timeout(0.25)?;
        let Some(window) = app.element("AXFocusedWindow") else {
            return Ok(None);
        };
        if window.identity() != self.window_hash
            || window.pid() != Some(self.lease.foreground.window_pid)
            || !safe_visible(&window, &self.lease.secure_description)
        {
            return Ok(None);
        }
        let Some(field) = app.element("AXFocusedUIElement") else {
            return Ok(None);
        };
        if field.identity() != self.field_hash
            || field.pid() != Some(self.lease.foreground.window_pid)
            || field.element("AXWindow") != Some(window.clone())
            || !field_is_ordinary_writable(&field, &self.lease.secure_description)
        {
            return Ok(None);
        }
        if !composition_clear_with_source(&field, self.lease.source_kind) {
            return Ok(None);
        }
        let identity = if is_browser(&[&self.lease.foreground.app_id]) {
            let Some(document) = window.string("AXDocument") else {
                return Ok(None);
            };
            if native_domain(&document).as_deref() != Some(self.lease.foreground.domain.as_str()) {
                return Ok(None);
            }
            let digest = document_digest(&self.lease.salt, &document);
            if digest != self.document_id {
                return Ok(None);
            }
            format!(
                "{}:{}:{}:{digest}",
                self.lease.foreground.window_pid,
                field.identity(),
                window.identity()
            )
        } else {
            let Some((_, document_id)) = session_document_identity(
                &self.lease.salt,
                false,
                &self.lease.foreground.app_id,
                self.lease.foreground.window_pid,
                window.identity(),
                window.string("AXDocument").as_deref(),
                terminal::process_started(self.lease.foreground.window_pid),
            ) else {
                return Ok(None);
            };
            if document_id != self.document_id {
                return Ok(None);
            }
            format!("{}:{}", self.lease.foreground.window_pid, field.identity())
        };
        if identity != self.field_id || !self.lease_valid() {
            return Ok(None);
        }
        Ok(Some((field, window)))
    }
    pub fn matches(&self) -> Result<bool> {
        Ok(self.resolve()?.is_some())
    }
    pub fn read(&self, authorized: &dyn Fn() -> bool, max_chars: usize) -> Result<ContextRead> {
        if !authorized()
            || !self
                .lease
                .preference_allowed(ScopedPreference::ScreenContext)
        {
            return Ok(ContextRead::blocked("accessibility"));
        }
        let Some((field, window)) = self.resolve()? else {
            return Ok(ContextRead::blocked("accessibility"));
        };
        let mut root = window.clone();
        let mut parent = field.element("AXParent");
        let mut ancestors = Vec::new();
        for _ in 0..16 {
            let Some(node) = parent else { break };
            if node == window || ancestors.contains(&node) {
                break;
            }
            if !safe_visible(&node, &self.lease.secure_description) {
                return Ok(ContextRead::blocked("accessibility"));
            }
            if node.string("AXRole").as_deref() == Some("AXWebArea") {
                root = node;
                break;
            }
            parent = node.element("AXParent");
            ancestors.push(node);
        }
        let deadline = Instant::now() + Duration::from_millis(150);
        let mut stack = vec![(root, 0)];
        let mut visited = Vec::<AxElement>::new();
        let mut chunks = Vec::new();
        let mut remaining = max_chars.min(12000);
        let window_bounds = window.frame();
        for _ in 0..128 {
            if remaining == 0 || Instant::now() >= deadline {
                break;
            }
            let Some((element, depth)) = stack.pop() else {
                break;
            };
            if !authorized() || self.resolve()?.is_none() {
                return Ok(ContextRead::blocked("accessibility"));
            }
            if visited.contains(&element) {
                continue;
            }
            visited.push(element.clone());
            if !safe_visible(&element, &self.lease.secure_description) {
                continue;
            }
            let role = element.string("AXRole").unwrap_or_default();
            let bounds = element.frame();
            if let (Some(bounds), Some(window)) = (bounds, window_bounds) {
                if !rect_intersects(bounds, window) {
                    continue;
                }
            }
            if role == "AXStaticText" && bounds.is_some() {
                if !authorized() || !self.lease_valid() {
                    return Ok(ContextRead::blocked("accessibility"));
                }
                if let Some(value) = element.string("AXValue") {
                    if !value.trim().is_empty() {
                        let value: String = value.chars().take(remaining).collect();
                        remaining = remaining.saturating_sub(value.chars().count() + 1);
                        chunks.push(value)
                    }
                }
            }
            if depth < 8 && !matches!(role.as_str(), "AXTextField" | "AXTextArea") {
                if let Some(children) = element.children() {
                    stack.extend(
                        children
                            .into_iter()
                            .take(64)
                            .map(|child| (child, depth + 1)),
                    )
                }
            }
        }
        if !authorized() || self.resolve()?.is_none() {
            return Ok(ContextRead::blocked("accessibility"));
        }
        let text: String = chunks.join("\n").chars().take(max_chars).collect();
        Ok(context_result(
            &text,
            if text.is_empty() { "empty" } else { "ready" },
        ))
    }
}
impl super::DetachedContextRead for MacContextTicket {
    fn read(&self, authorized: &dyn Fn() -> bool, max_chars: usize) -> Result<ContextRead> {
        MacContextTicket::read(self, authorized, max_chars)
    }
    fn matches(&self) -> Result<bool> {
        MacContextTicket::matches(self)
    }
    fn matches_snapshot(&self, snapshot: &Snapshot) -> bool {
        MacContextTicket::matches_snapshot(self, snapshot)
    }
}

/// Diagnostic-only, immutable enrollment. Never available to normal builds.
#[cfg(feature = "native-smoke")]
struct OwnedAxTarget {
    pid: i32,
    started: (u64, u64),
    window: AxElement,
    nonce: String,
    emoji_field: Option<AxElement>,
}
#[cfg(feature = "native-smoke")]
impl OwnedAxTarget {
    fn matches(&self, pid: i32) -> bool {
        pid == self.pid
            && terminal::process_started(pid) == Some(self.started)
            && self.window.pid() == Some(pid)
            && self.window.string("AXTitle").as_deref() == Some(self.nonce.as_str())
            && AxElement::application(pid)
                .and_then(|app| app.element("AXFocusedWindow"))
                .as_ref()
                == Some(&self.window)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum InsertionReason {
    NotIssued,
    PostError,
    SetterError,
    CaptureUnavailable,
    CaptureError,
    ExpectedMutationUnavailable,
    AuthorityChange,
    TextOrCaretOrSelectionMismatch,
    Unchanged,
    Exact,
}

/// Retained only by WhatsApp ordinary completion insertion, never by later
/// captures, other applications, replacements or emoji. No host/error content.
#[derive(Clone, Copy, Debug, serde::Serialize)]
struct InsertionDiagnostic {
    reason: InsertionReason,
    capture_stage: &'static str,
    readback: Option<InsertionReadback>,
    user_revision_unchanged: Option<bool>,
}

#[derive(Clone, Copy, Debug, serde::Serialize)]
struct InsertionReadback {
    authority_match: bool,
    text_match: bool,
    caret_match: bool,
    selection_empty: bool,
    unchanged_readback: bool,
    same_app: bool,
    same_process: bool,
    same_window: bool,
    same_field: bool,
    same_document: bool,
    same_domain: bool,
}

pub struct MacBridge {
    owner: ThreadId,
    policy: Arc<ObservationPolicy>,
    input: Input,
    secure_description: String,
    salt: [u8; 32],
    stopped: bool,
    config: Config,
    compatibility: Option<(i32, String, bool)>,
    compatibility_outcome: serde_json::Value,
    observer: Option<AxObservation>,
    observer_pid: i32,
    focused_hash: Arc<AtomicU64>,
    notifications: Option<AppNotifications>,
    native_dirty: Arc<AtomicBool>,
    layout_dirty: Arc<AtomicBool>,
    policy_epoch: Arc<AtomicU64>,
    terminal: Option<Box<dyn TerminalIntegration>>,
    appearance: Option<Box<dyn AppearanceIntegration>>,
    docs: Option<docs::DocsHelper>,
    capture_diagnostic: &'static str,
    last_ordinary_completion_insert: Option<InsertionDiagnostic>,
    pending_unicode_post: Option<UnicodePostLease>,
    pending_capture_transient: bool,
    capture_front: Option<(i32, String, String, String)>,
    whatsapp_capture: Option<WhatsAppCapture>,
    diagnostic_requested: Arc<AtomicBool>,
    #[cfg(feature = "native-smoke")]
    owned_target: Option<OwnedAxTarget>,
    #[cfg(feature = "native-smoke")]
    owned_notifications: Arc<[AtomicU64; 4]>,
    not_send: PhantomData<Rc<()>>,
}
impl MacBridge {
    /// Enroll only an authenticated, separately spawned diagnostic editor.
    /// Enrollment reads metadata from that PID alone, never the foreground draft.
    #[cfg(feature = "native-smoke")]
    pub fn new_owned(pid: i32, started: (u64, u64), nonce: String) -> Result<Self> {
        ensure!(
            pid != std::process::id() as i32 && nonce.len() == 64,
            "Invalid owned AX enrollment"
        );
        ensure!(
            terminal::process_started(pid) == Some(started),
            "Owned process changed"
        );
        let app = AxElement::application(pid).ok_or_else(|| anyhow!("Owned AX app unavailable"))?;
        let window = app
            .element("AXFocusedWindow")
            .ok_or_else(|| anyhow!("Owned AX window unavailable"))?;
        let target = OwnedAxTarget {
            pid,
            started,
            window,
            nonce,
            emoji_field: None,
        };
        ensure!(
            target.matches(pid),
            "Owned AX window nonce/identity mismatch"
        );
        let mut bridge = Self::new()?;
        bridge.owned_target = Some(target);
        Ok(bridge)
    }
    /// Explicit opt-in for one authenticated owned editor's emoji transaction.
    /// Ordinary owned AX enrollment continues to forbid all keyboard posting.
    #[cfg(feature = "native-smoke")]
    pub fn new_owned_emoji(pid: i32, started: (u64, u64), nonce: String) -> Result<Self> {
        let mut bridge = Self::new_owned(pid, started, nonce)?;
        let field = AxElement::application(pid)
            .and_then(|app| app.element("AXFocusedUIElement"))
            .ok_or_else(|| anyhow!("Owned emoji field unavailable"))?;
        let target = bridge.owned_target.as_mut().expect("owned enrollment");
        ensure!(
            field.pid() == Some(pid)
                && field.element("AXWindow").as_ref() == Some(&target.window)
                && field_is_ordinary_writable(&field, &bridge.secure_description),
            "Owned emoji field identity or safety mismatch"
        );
        target.emoji_field = Some(field.clone());
        ensure!(
            bridge.emoji_replacement_supported(&field, ""),
            "Owned emoji field lacks native replacement capabilities or foreground ownership"
        );
        Ok(bridge)
    }
    #[cfg(feature = "native-smoke")]
    pub fn owned_process_started(pid: i32) -> Option<(u64, u64)> {
        terminal::process_started(pid)
    }
    #[cfg(feature = "native-smoke")]
    pub fn owned_notification_counts(&self) -> [u64; 4] {
        std::array::from_fn(|i| self.owned_notifications[i].load(Ordering::Relaxed))
    }
    /// Paired owned captures differ only in pass-local frame reuse. Both execute
    /// every ordinary security/focus/selection/anchor barrier independently.
    #[cfg(feature = "native-smoke")]
    pub fn owned_frame_read_comparison(&mut self) -> Result<serde_json::Value> {
        ensure!(self.owned_target.is_some(), "Owned AX target required");
        let mut baseline_counts = geometry::FrameReadCounts::default();
        let baseline = self.capture_ordinary_with_reads(false, &mut baseline_counts)?;
        let mut reused_counts = geometry::FrameReadCounts::default();
        let reused = self.capture_ordinary_with_reads(true, &mut reused_counts)?;
        let identical = match (&baseline, &reused) {
            (Some((left, left_field)), Some((right, right_field))) => {
                left == right && left_field == right_field
            }
            _ => false,
        };
        Ok(serde_json::json!({
            "baseline": baseline_counts,
            "reused": reused_counts,
            "both_captured": baseline.is_some() && reused.is_some(),
            "identical_capture": identical,
            "fewer_native_frame_calls": reused_counts.native_calls < baseline_counts.native_calls,
            "pass_local_hit_observed": reused_counts.hits > 0,
            "fresh_barriers": "independent ordinary captures; reuse ends before appearance/focus/anchor",
            "scope": "owned AX child only; no latency or external-host authority claim",
        }))
    }
    /// Read-only raw-range diagnostics for an authenticated owned editor.
    /// Unlike the public correction-range API, zero-length ranges are allowed.
    /// Coordinates are not normalized, and no renderer evidence is manufactured.
    #[cfg(feature = "native-smoke")]
    pub fn owned_tab_row_bounds(
        &mut self,
        snapshot: &Snapshot,
        sample_end: usize,
    ) -> Result<serde_json::Value> {
        self.owner()?;
        ensure!(
            self.owned_target
                .as_ref()
                .is_some_and(|target| target.matches(snapshot.window_pid))
                && self.owned_current(),
            "Authenticated owned AX target required"
        );
        ensure!(
            !snapshot.protected
                && snapshot.selection_length == 0
                && sample_end > 0
                && sample_end <= snapshot.caret
                && snapshot.caret <= snapshot.text.scalar_len(),
            "Invalid owned tab-row scalar range"
        );
        let (live, field) = self
            .checked_current(snapshot)?
            .ok_or_else(|| anyhow!("Owned tab-row snapshot authority changed"))?;
        let revision = self.input.revision();
        let epoch = self.policy_epoch.load(Ordering::Acquire);
        let index = live.text.index();
        let read_range = |start: usize, end: usize| -> Result<serde_json::Value> {
            let location = index
                .scalar_to_utf16(ScalarIndex(start))
                .ok_or_else(|| anyhow!("Invalid owned range start"))?
                .0;
            let finish = index
                .scalar_to_utf16(ScalarIndex(end))
                .ok_or_else(|| anyhow!("Invalid owned range end"))?
                .0;
            let length = finish
                .checked_sub(location)
                .ok_or_else(|| anyhow!("Reversed owned range"))?;
            Ok(serde_json::json!({
                "scalar_start": start,
                "scalar_length": end - start,
                "utf16_location": location,
                "utf16_length": length,
                "rect": field.bounds_for_range(AxRange { location, length }),
            }))
        };
        let output = serde_json::json!({
            "sample": read_range(sample_end.saturating_sub(32), sample_end)?,
            "sample_end": read_range(sample_end, sample_end)?,
            "caret": read_range(live.caret, live.caret)?,
            "gap": read_range(sample_end, live.caret)?,
            "last_sample_glyph": read_range(sample_end - 1, sample_end)?,
            "last_caret_glyph": read_range(live.caret - 1, live.caret)?,
        });
        let (_, after_field) = self
            .checked_current(&live)?
            .ok_or_else(|| anyhow!("Owned tab-row authority changed during range reads"))?;
        ensure!(
            after_field == field
                && self.input.revision() == revision
                && self.policy_epoch.load(Ordering::Acquire) == epoch
                && !self.native_dirty.load(Ordering::Acquire)
                && self
                    .owned_target
                    .as_ref()
                    .is_some_and(|target| target.matches(live.window_pid))
                && self.owned_current(),
            "Owned tab-row range observations lost their authority"
        );
        Ok(output)
    }
    #[cfg(feature = "native-smoke")]
    fn owned_current(&self) -> bool {
        self.owned_target.as_ref().is_none_or(|target| {
            frontmost_application().is_some_and(|(pid, _, _, _)| target.matches(pid))
        })
    }
    #[cfg(feature = "native-smoke")]
    fn owned_emoji_current(&self, field: &AxElement) -> bool {
        self.owned_target.as_ref().is_some_and(|target| {
            target.emoji_field.as_ref() == Some(field)
                && field.pid() == Some(target.pid)
                && field.element("AXWindow").as_ref() == Some(&target.window)
                && frontmost_application().is_some_and(|(pid, _, _, _)| {
                    target.matches(pid)
                        && AxElement::application(pid)
                            .and_then(|app| app.element("AXFocusedUIElement"))
                            .as_ref()
                            == Some(field)
                })
        })
    }
    fn emoji_replacement_supported(&self, field: &AxElement, app_id: &str) -> bool {
        let supported_host = app_id == "net.whatsapp.WhatsApp";
        #[cfg(feature = "native-smoke")]
        let supported_host = if self.owned_target.is_some() {
            self.owned_emoji_current(field)
        } else {
            supported_host
        };
        supported_host
            && matches!(field.attribute("ChromeAXNodeId"), Ok(None))
            && !field.settable("AXSelectedText")
            && field.settable("AXSelectedTextRange")
            && field.settable("AXValue")
    }
    pub fn new() -> Result<Self> {
        ensure!(
            is_main_thread(),
            "macOS native bridge must be constructed on the AppKit main thread"
        );
        let config = Config::default();
        let policy = Arc::new(ObservationPolicy::new(&config)?);
        let input = Input::new();
        let mut salt = [0u8; 32];
        rand::rngs::OsRng
            .try_fill_bytes(&mut salt)
            .map_err(|_| anyhow!("Native document identity entropy unavailable"))?;
        let dirty = Arc::new(AtomicBool::new(true));
        let layout_dirty = Arc::new(AtomicBool::new(true));
        let diagnostic_requested = Arc::new(AtomicBool::new(false));
        let notifications = AppNotifications::new(
            input.notifier(),
            dirty.clone(),
            layout_dirty.clone(),
            diagnostic_requested.clone(),
        )?;
        let mut bridge = Self {
            owner: thread::current().id(),
            policy,
            input,
            secure_description: ax::secure_role_description()
                .ok_or_else(|| anyhow!("Secure Accessibility role description unavailable"))?,
            salt,
            stopped: false,
            config,
            compatibility: None,
            compatibility_outcome: serde_json::Value::Null,
            observer: None,
            observer_pid: 0,
            focused_hash: Arc::new(AtomicU64::new(0)),
            notifications: Some(notifications),
            native_dirty: dirty,
            layout_dirty,
            policy_epoch: Arc::new(AtomicU64::new(1)),
            terminal: None,
            appearance: None,
            docs: Some(docs::DocsHelper::new()),
            capture_diagnostic: "not_observed",
            last_ordinary_completion_insert: None,
            pending_unicode_post: None,
            pending_capture_transient: false,
            capture_front: None,
            whatsapp_capture: None,
            diagnostic_requested,
            #[cfg(feature = "native-smoke")]
            owned_target: None,
            #[cfg(feature = "native-smoke")]
            owned_notifications: Arc::new(std::array::from_fn(|_| AtomicU64::new(0))),
            not_send: PhantomData,
        };
        bridge.refresh_main_state()?;
        Ok(bridge)
    }
    fn owner(&self) -> Result<()> {
        ensure!(
            self.owner == thread::current().id() && is_main_thread(),
            "macOS native ownership changed threads"
        );
        Ok(())
    }
    pub fn capture_services(&mut self) -> CaptureServices<'_> {
        CaptureServices { bridge: self }
    }
    pub fn context_ticket(&mut self, snapshot: &Snapshot) -> Result<Option<MacContextTicket>> {
        self.owner()?;
        if self.stopped || self.input.dead_key() {
            return Ok(None);
        }
        let Some(focus) = self.focused()? else {
            return Ok(None);
        };
        if !self.focus_matches_snapshot(&focus, snapshot) || !self.composition_clear(&focus.field) {
            return Ok(None);
        }
        let Some(window) = focus.session.window.as_ref() else {
            return Ok(None);
        };
        Ok(Some(MacContextTicket {
            lease: self.detached_lease(&focus.session, snapshot),
            document_id: focus.session.document_id.clone(),
            field_id: focus.field_id,
            field_hash: focus.field.identity(),
            window_hash: window.identity(),
        }))
    }
    fn detached_lease(&self, session: &MacSession, snapshot: &Snapshot) -> NativeReadLease {
        NativeReadLease {
            foreground: session.foreground.clone(),
            name: session.name.clone(),
            secure_description: self.secure_description.clone(),
            source_kind: self.input.source_kind(),
            salt: self.salt,
            policy: self.policy.clone(),
            epoch: self.policy_epoch.clone(),
            issued_epoch: self.policy_epoch.load(Ordering::Acquire),
            revision: self.input.revision_reader(),
            issued_revision: self.input.revision(),
            dirty: self.native_dirty.clone(),
            snapshot_binding: snapshot_binding(&self.salt, snapshot),
        }
    }
    pub fn set_terminal_adapter(
        &mut self,
        mut adapter: Box<dyn TerminalIntegration>,
    ) -> Result<()> {
        self.owner()?;
        adapter.configure(&self.config)?;
        if let Some(mut old) = self.terminal.take() {
            if let Err(error) = old.stop() {
                self.terminal = Some(old);
                return Err(error);
            }
        }
        self.terminal = Some(adapter);
        Ok(())
    }
    pub fn set_appearance_adapter(
        &mut self,
        mut adapter: Box<dyn AppearanceIntegration>,
    ) -> Result<()> {
        self.owner()?;
        adapter.configure(&self.config)?;
        if let Some(mut old) = self.appearance.take() {
            if let Err(error) = old.stop() {
                self.appearance = Some(old);
                return Err(error);
            }
        }
        self.appearance = Some(adapter);
        Ok(())
    }
    pub fn terminal_permissions(&mut self) -> Result<serde_json::Value> {
        self.owner()?;
        let adapter = self
            .terminal
            .as_mut()
            .ok_or_else(|| anyhow!("Native terminal integration is unavailable"))?;
        let mut permissions = serde_json::Map::new();
        for (app_id, name) in [
            ("com.apple.Terminal", "Terminal"),
            ("com.googlecode.iterm2", "iTerm"),
        ] {
            permissions.insert(
                app_id.into(),
                serde_json::json!({"name":name,"status":adapter.permission_state(app_id)?}),
            );
        }
        Ok(permissions.into())
    }
    pub fn request_terminal_access(&mut self, app_id: &str) -> Result<String> {
        self.owner()?;
        if self.stopped || !exact_member(app_id, TERMINALS) {
            return Ok("unavailable".into());
        }
        self.terminal
            .as_mut()
            .ok_or_else(|| anyhow!("Native terminal integration is unavailable"))?
            .request_access(app_id)
    }
    pub fn document_identity(&self, value: &str) -> String {
        document_digest(&self.salt, value)
    }
    fn refresh_main_state(&mut self) -> Result<()> {
        self.owner()?;
        self.expire_pending_unicode();
        if self.layout_dirty.swap(false, Ordering::AcqRel) {
            let (keys, kind) = keyboard_state();
            self.input.set_layout(keys, kind);
        }
        if self.native_dirty.swap(false, Ordering::AcqRel) {
            self.input.clear_composition();
        }
        if self.diagnostic_requested.swap(false, Ordering::AcqRel) {
            if let Err(error) = self.export_whatsapp_diagnostic() {
                eprintln!("BroTypist: WhatsApp diagnostic export failed: {error}");
            }
        }
        Ok(())
    }
    pub fn session(&mut self) -> Result<Option<MacSession>> {
        self.owner()?;
        self.refresh_main_state()?;
        if self.stopped {
            return Ok(None);
        }
        self.capture_front = None;
        let Some((pid, app_id, executable, name)) = frontmost_application() else {
            self.capture_diagnostic = "foreground_unavailable";
            return Ok(None);
        };
        // Fence BEFORE compatibility negotiation, window/document reads or any
        // field access. Every capture and mutation recapture re-enters here.
        #[cfg(feature = "native-smoke")]
        if self
            .owned_target
            .as_ref()
            .is_some_and(|target| !target.matches(pid))
        {
            self.capture_diagnostic = "owned_target_refused";
            return Ok(None);
        }
        if [&app_id, &executable, &name]
            .iter()
            .any(|value| value.to_ascii_lowercase().contains("whatsapp"))
        {
            self.capture_front = Some((pid, app_id.clone(), executable.clone(), name.clone()));
        }
        let front = Foreground {
            app_id: app_id.clone(),
            executable: executable.clone(),
            window_pid: pid,
            ..Default::default()
        };
        if pid == std::process::id() as i32
            || app_id.is_empty()
            || !self.policy.global_enabled
            || self.policy.excluded(&[&app_id, &executable, &name])
            || secure_event_input_enabled() != Some(false)
            || !ax::trusted()
        {
            if pid != std::process::id() as i32 {
                self.capture_diagnostic = "foreground_policy_or_security_denied";
            }
            self.input.set_foreground(front);
            return Ok(None);
        }
        let Some(app) = AxElement::application(pid) else {
            self.capture_diagnostic = "foreground_ax_unavailable";
            return Ok(None);
        };
        // Some native applications expose their window tree only after the
        // existing compatibility preparation. Do this after foreground/security
        // authorization, before requiring that tree; ownership checks stay below.
        if self.policy.allowed(
            &app_id,
            "",
            &[&executable, &name],
            exact_member(&app_id, CHAT_EDITORS),
        ) {
            self.prepare_accessibility(&front, &app);
        }
        let window = app.element("AXFocusedWindow");
        let Some(window_ref) = window.as_ref() else {
            self.capture_diagnostic = "focused_window_unavailable";
            return Ok(None);
        };
        #[cfg(feature = "native-smoke")]
        if self
            .owned_target
            .as_ref()
            .is_some_and(|target| window_ref != &target.window || !target.matches(pid))
        {
            self.capture_diagnostic = "owned_window_changed";
            return Ok(None);
        }
        let document = window_ref.string("AXDocument");
        let browser = is_browser(&[&app_id, &executable]);
        let identity = session_document_identity(
            &self.salt,
            browser,
            &app_id,
            pid,
            window_ref.identity(),
            document.as_deref(),
            if browser {
                None
            } else {
                terminal::process_started(pid)
            },
        );
        let (domain, document_id) = match identity {
            Some(identity) => identity,
            // Keep browser metadata/AX-enablement available, but field_identity
            // still refuses all draft/context access without an HTTP(S) document.
            None if browser => (String::new(), String::new()),
            None => {
                self.capture_diagnostic = "window_identity_unverified";
                return Ok(None);
            }
        };
        let window_id = format!("{pid}:{}", window_ref.identity());
        let foreground = Foreground {
            domain,
            window_id,
            document_id: document_id.clone(),
            ..front
        };
        self.input.set_foreground(foreground.clone());
        self.observe(pid);
        Ok(Some(MacSession {
            foreground,
            name,
            app,
            window,
            document_id,
        }))
    }
    fn observe(&mut self, pid: i32) {
        if self.observer_pid == pid {
            return;
        }
        self.observer = None;
        self.observer_pid = 0;
        let note = self.input.notifier();
        let focused = self.focused_hash.clone();
        // Never throttle authority revocation: a second notification can arrive
        // after a fresh lease was issued within the same coalescing interval.
        let dirty = self.native_dirty.clone();
        #[cfg(feature = "native-smoke")]
        let counts = self.owned_notifications.clone();
        let observer = AxObservation::new(
            pid,
            Box::new(move |name, element| {
                #[cfg(feature = "native-smoke")]
                {
                    let index = match name {
                        "AXValueChanged" => Some(0),
                        "AXSelectedTextChanged" => Some(1),
                        "AXMoved" | "AXResized" | "AXWindowMoved" | "AXWindowResized" => Some(2),
                        "AXFocusedUIElementChanged" | "AXFocusedWindowChanged" => Some(3),
                        _ => None,
                    };
                    if let Some(index) = index {
                        counts[index].fetch_add(1, Ordering::Relaxed);
                    }
                }
                let Some(user) = ax_invalidation(name, element, focused.load(Ordering::Relaxed))
                else {
                    return;
                };
                // Only focus invalidation resets composition. Geometry/text
                // still revoke every lease through the input revision below.
                if user {
                    dirty.store(true, Ordering::Release);
                }
                note(user)
            }),
        );
        if let Ok(observer) = observer {
            self.observer = Some(observer);
            self.observer_pid = pid;
        }
    }
    fn allowed(&self, session: &MacSession, editor_probe: bool) -> bool {
        let front = &session.foreground;
        self.policy.allowed(
            &front.app_id,
            &front.domain,
            &[&front.executable, &session.name],
            editor_probe,
        )
    }
    fn prepare_accessibility(&mut self, front: &Foreground, app: &AxElement) {
        let enabled = self
            .policy
            .rule_bool(&front.app_id, "", "improve_compatibility", true);
        let forced = self
            .config
            .app_rules
            .get(&format!("app:{}", front.app_id))
            .and_then(|r| r.get("improve_compatibility"))
            .and_then(serde_json::Value::as_bool)
            == Some(true);
        let target = (front.window_pid, front.app_id.clone(), enabled);
        if self.compatibility.as_ref() == Some(&target) {
            return;
        }
        self.compatibility = Some(target);
        self.compatibility_outcome = serde_json::json!({"enabled":enabled,"forced":forced});
        if !enabled {
            return;
        }
        // AX exposure is a host capability, not a browser/chat bundle property.
        // Electron editors and native lazy AX trees can advertise the same
        // switch. Authorization happened before this call; this negotiates only
        // readable, settable booleans and does not grant field/write authority.
        let manual = enable_accessibility_attribute(app, "AXManualAccessibility");
        let enhanced = enable_accessibility_attribute(app, "AXEnhancedUserInterface");
        self.compatibility_outcome["manual_accessibility"] = manual.into();
        self.compatibility_outcome["enhanced_user_interface"] = enhanced.into();
    }
    fn focused(&mut self) -> Result<Option<Focused>> {
        let Some(session) = self.session()? else {
            return Ok(None);
        };
        let front = &session.foreground;
        if exact_member(&front.app_id, TERMINALS) {
            self.capture_diagnostic = "terminal_adapter";
            return Ok(None);
        }
        let probe = exact_member(&front.app_id, CHAT_EDITORS);
        if !self.allowed(&session, probe) {
            self.capture_diagnostic = "field_policy_denied";
            return Ok(None);
        }
        self.prepare_accessibility(&session.foreground, &session.app);
        let Some(field) = session.app.element("AXFocusedUIElement") else {
            self.capture_diagnostic = "focused_field_unavailable";
            return Ok(None);
        };
        if field.pid() != Some(front.window_pid) {
            self.capture_diagnostic = "focused_field_process_mismatch";
            return Ok(None);
        }
        if field.element("AXWindow") != session.window {
            self.capture_diagnostic = "focused_field_window_mismatch";
            return Ok(None);
        }
        if !field_is_ordinary_writable(&field, &self.secure_description) {
            self.capture_diagnostic = "field_not_ordinary_writable";
            return Ok(None);
        }
        if probe
            && !self
                .policy
                .rule_bool(&front.app_id, "", "complete_in_code_editors", false)
        {
            let Some(description) = field.string("AXDescription") else {
                self.capture_diagnostic = "editor_field_unverified";
                return Ok(None);
            };
            if !stock_chat_description(&description) {
                self.capture_diagnostic = "editor_field_not_chat";
                return Ok(None);
            }
        }
        let Some(field_id) = self.field_identity(&field, &session) else {
            self.capture_diagnostic = "field_identity_unverified";
            return Ok(None);
        };
        Ok(Some(Focused {
            field,
            session,
            field_id,
        }))
    }
    pub fn field_identity(&self, field: &AxElement, session: &MacSession) -> Option<String> {
        let front = &session.foreground;
        let identity = format!("{}:{}", front.window_pid, field.identity());
        if !is_browser(&[&front.app_id]) {
            return Some(identity);
        }
        let window = field.element("AXWindow")?;
        if Some(&window) != session.window.as_ref() {
            return None;
        }
        let document = window.string("AXDocument")?;
        native_domain(&document)?;
        let digest = self.document_identity(&document);
        if digest != session.document_id {
            return None;
        }
        Some(format!("{identity}:{}:{digest}", window.identity()))
    }
    fn composition_clear(&self, field: &AxElement) -> bool {
        if self.input.dead_key() {
            return false;
        }
        composition_clear_with_source(field, self.input.source_kind())
    }
    fn capture_ordinary(&mut self) -> Result<Option<(Snapshot, AxElement)>> {
        self.capture_ordinary_with_reads(true, &mut geometry::FrameReadCounts::default())
    }
    fn capture_ordinary_with_reads(
        &mut self,
        reuse: bool,
        read_counts: &mut geometry::FrameReadCounts,
    ) -> Result<Option<(Snapshot, AxElement)>> {
        if self.stopped || !ax::trusted() {
            return Ok(None);
        }
        self.refresh_main_state()?;
        if self.input.dead_key() {
            return Ok(None);
        }
        let Some(focus) = self.focused()? else {
            return Ok(None);
        };
        let field = &focus.field;
        if !self.composition_clear(field) {
            self.capture_diagnostic = "marked_text_or_input_source_unverified";
            return Ok(None);
        }
        let Some(native) = field.range("AXSelectedTextRange") else {
            self.capture_diagnostic = "selection_range_unavailable";
            return Ok(None);
        };
        let Some(revalidated) = self.focused()? else {
            self.capture_diagnostic = "focus_revalidation";
            return Ok(None);
        };
        if !focus.same(&revalidated) {
            self.capture_diagnostic = "focus_changed_before_value";
            return Ok(None);
        }
        let read_revision = self.input.revision();
        let read_epoch = self.policy_epoch.load(Ordering::Acquire);
        // Value is deliberately the last capability/privacy-gated read.
        self.capture_diagnostic = "field_value_unavailable";
        let Some(text) = field.string("AXValue") else {
            return Ok(None);
        };
        if text.chars().count() > 1_000_000 {
            self.capture_diagnostic = "field_value_too_large";
            return Ok(None);
        }
        let text: TextBuffer = text.into();
        self.capture_diagnostic = "selection_utf16_invalid";
        let Some((caret, selection_length)) = text
            .index()
            .utf16_range_to_scalar(native.location, native.length)
        else {
            return Ok(None);
        };
        let Some(window) = focus.session.window.as_ref() else {
            return Ok(None);
        };
        let current_reads = || {
            (
                self.input.revision(),
                self.policy_epoch.load(Ordering::Acquire),
                self.native_dirty.load(Ordering::Acquire),
            )
        };
        let mut geometry_diagnostic = "selection_active";
        let mut gap_diagnostic = None;
        let Some(geometry::CaptureGeometry {
            caret_rect,
            field_rect,
            window_rect,
            render,
            evidence,
            counts,
        }) = geometry::capture_geometry(
            geometry::CaptureReadScope {
                field,
                window,
                document: &focus.session.document_id,
                pid: focus.session.foreground.window_pid,
                revision: read_revision,
                epoch: read_epoch,
                current: &current_reads,
                reuse,
            },
            &self.secure_description,
            &text,
            caret,
            native,
            is_browser(&[&focus.session.foreground.app_id]),
            &mut geometry_diagnostic,
            if focus.session.foreground.app_id == "net.whatsapp.WhatsApp" {
                Some(&mut gap_diagnostic)
            } else {
                None
            },
        )
        else {
            self.capture_diagnostic = "geometry_read_scope_revoked";
            return Ok(None);
        };
        *read_counts = counts;
        // The pass and its retained frames have already been dropped. Appearance,
        // focus, anchor, publication and edit/readback checks never see its store.
        let front = &focus.session.foreground;
        let mut snapshot = Snapshot {
            app_id: front.app_id.clone(),
            field_id: focus.field_id.clone(),
            text,
            caret,
            selection_length,
            caret_rect,
            domain: front.domain.clone(),
            multiline: field.string("AXRole").as_deref() == Some("AXTextArea"),
            field_rect,
            window_id: front.window_id.clone(),
            window_revision: focus.session.document_id.clone(),
            window_pid: front.window_pid,
            window_bounds: window_rect,
            can_replace: !uses_unicode_insertion(field, &front.app_id)
                && field.settable("AXSelectedText"),
            can_replace_emoji: self.emoji_replacement_supported(field, &front.app_id),
            inline_render: render,
            ..Default::default()
        };
        self.capture_diagnostic = if snapshot.inline_safe() {
            "inline_ready"
        } else if snapshot.caret_rect.is_none() {
            "caret_geometry_unverified"
        } else if snapshot.field_rect.is_none() {
            "field_geometry_unverified"
        } else if snapshot.selection_length != 0 {
            "selection_active"
        } else {
            geometry_diagnostic
        };
        if snapshot.inline_render.is_none()
            && self
                .policy
                .preference(
                    ScopedPreference::ScreenshotAppearance,
                    &snapshot.app_id,
                    &snapshot.domain,
                    &[&front.executable, &focus.session.name],
                )
                .value
        {
            if let Some(evidence) = evidence {
                if let Some(mut adapter) = self.appearance.take() {
                    let styled = adapter.style(&mut self.capture_services(), &snapshot, &evidence);
                    self.appearance = Some(adapter);
                    if let Some(styled) = styled? {
                        snapshot.inline_render = validate_appearance(&snapshot, &evidence, styled);
                        if snapshot.inline_safe() {
                            self.capture_diagnostic = "inline_ready";
                        }
                    }
                }
            }
        }
        let Some(after) = self.focused()? else {
            self.capture_diagnostic = "focus_changed_after_capture";
            return Ok(None);
        };
        if !focus.same(&after) || !self.composition_clear(&after.field) {
            self.capture_diagnostic = "focus_or_composition_changed_after_capture";
            return Ok(None);
        }
        if !geometry::capture_anchor_matches(
            &after.field,
            &self.secure_description,
            &snapshot,
            native,
        ) {
            self.capture_diagnostic = "selection_or_geometry_changed_after_capture";
            return Ok(None);
        }
        if self.input.revision() != read_revision
            || self.policy_epoch.load(Ordering::Acquire) != read_epoch
            || self.native_dirty.load(Ordering::Acquire)
        {
            self.capture_diagnostic = "geometry_read_scope_changed_after_capture";
            return Ok(None);
        }
        self.focused_hash.store(field.identity(), Ordering::Relaxed);
        if let Some(observer) = self.observer.as_mut() {
            observer.update_targets(field, focus.session.window.as_ref());
        }
        // No diagnostic candidate escapes a revoked or incomplete capture.
        self.retain_validated_whatsapp_gap(&snapshot, gap_diagnostic);
        Ok(Some((snapshot, focus.field)))
    }
    fn checked_current(&mut self, snapshot: &Snapshot) -> Result<Option<(Snapshot, AxElement)>> {
        Ok(self
            .capture_ordinary()?
            .filter(|(live, _)| live.same_key(snapshot) && live.same_authority(snapshot)))
    }
    fn record_insertion(
        &mut self,
        before: &Snapshot,
        reason: InsertionReason,
        readback: Option<InsertionReadback>,
        user_revision: Option<u64>,
    ) {
        if before.app_id != "net.whatsapp.WhatsApp" {
            return;
        }
        self.last_ordinary_completion_insert = Some(InsertionDiagnostic {
            reason,
            capture_stage: self.capture_diagnostic,
            readback,
            user_revision_unchanged: user_revision
                .map(|revision| self.input.user_revision() == revision),
        });
    }
    fn capture_native(&mut self) -> Result<Option<Snapshot>> {
        if let Some(session) = self.session()? {
            if exact_member(&session.foreground.app_id, TERMINALS) {
                if !self.config.terminal_completion {
                    return Ok(None);
                }
                let Some(mut adapter) = self.terminal.take() else {
                    bail!("Terminal completion requires the native terminal integration")
                };
                let result = adapter.capture(&mut self.capture_services(), &session);
                self.terminal = Some(adapter);
                return result;
            }
        }
        Ok(self.capture_ordinary()?.map(|(snapshot, _)| snapshot))
    }
    fn ensure_whatsapp_capture(&mut self) -> bool {
        let Some((pid, app_id, executable, name)) = self.capture_front.as_ref() else {
            return false;
        };
        if !self
            .whatsapp_capture
            .as_ref()
            .is_some_and(|capture| capture.pid == *pid && capture.app_id == *app_id)
        {
            self.whatsapp_capture = Some(WhatsAppCapture {
                pid: *pid,
                app_id: app_id.clone(),
                executable: executable.clone(),
                name: name.clone(),
                last_stage: "not_observed",
                stages: BTreeMap::new(),
                compatibility: serde_json::Value::Null,
                last_validated_gap_failure: None,
            });
        }
        true
    }
    /// Retains only content-free observations from a completed, validated
    /// WhatsApp snapshot. Later successful/background captures do not erase it.
    fn retain_validated_whatsapp_gap(
        &mut self,
        snapshot: &Snapshot,
        diagnostic: Option<geometry::GapDiagnostic>,
    ) {
        let Some(diagnostic) = diagnostic else {
            return;
        };
        if snapshot.app_id != "net.whatsapp.WhatsApp" || !self.ensure_whatsapp_capture() {
            return;
        }
        if let Some(capture) = self.whatsapp_capture.as_mut() {
            if capture.pid == snapshot.window_pid && capture.app_id == snapshot.app_id {
                diagnostic.retain_failure(&mut capture.last_validated_gap_failure);
            }
        }
    }
    /// Retains only stage codes for WhatsApp captures; nothing is written until requested.
    fn record_whatsapp_stage(&mut self) {
        if !self.ensure_whatsapp_capture() {
            return;
        }
        let Some(capture) = self.whatsapp_capture.as_mut() else {
            return;
        };
        capture.last_stage = self.capture_diagnostic;
        let count = capture.stages.entry(self.capture_diagnostic).or_insert(0);
        *count = count.saturating_add(1);
        if self
            .compatibility
            .as_ref()
            .is_some_and(|(owner, id, _)| *owner == capture.pid && *id == capture.app_id)
        {
            capture.compatibility = self.compatibility_outcome.clone();
        }
    }
    fn export_whatsapp_diagnostic(&self) -> Result<()> {
        let whatsapp = self.whatsapp_capture.as_ref().map(|capture| {
            serde_json::json!({
                "app_id": capture.app_id,
                "executable": capture.executable,
                "name": capture.name,
                "last_stage": capture.last_stage,
                "stage_counts": capture.stages,
                "compatibility": capture.compatibility,
                "last_validated_gap_failure": capture.last_validated_gap_failure,
                "policy": {
                    "excluded": self.policy.excluded(&[&capture.app_id, &capture.executable, &capture.name]),
                    "allowed": self.policy.allowed(&capture.app_id, "", &[&capture.executable, &capture.name], false),
                    "improve_compatibility": self.policy.rule_bool(&capture.app_id, "", "improve_compatibility", true),
                },
                "accessibility_shape": whatsapp_ax_shape(capture.pid),
            })
        });
        let output = serde_json::json!({
            "schema": "brotypist-whatsapp-capture-diagnostic/1",
            "version": env!("CARGO_PKG_VERSION"),
            "accessibility_trusted": ax::trusted(),
            "secure_input": secure_event_input_enabled(),
            "global_enabled": self.policy.global_enabled,
            "last_capture_stage": self.capture_diagnostic,
            "input": self.input.diagnostic_snapshot(),
            "last_ordinary_completion_insert": self.last_ordinary_completion_insert,
            "whatsapp": whatsapp,
        });
        let directory = crate::paths::config_dir()?.join("diagnostics");
        crate::paths::private_directory(&directory)?;
        let mut temporary = tempfile::Builder::new()
            .prefix(".whatsapp-capture-")
            .tempfile_in(&directory)?;
        {
            use std::{io::Write, os::unix::fs::PermissionsExt};
            temporary
                .as_file()
                .set_permissions(std::fs::Permissions::from_mode(0o600))?;
            serde_json::to_writer_pretty(temporary.as_file_mut(), &output)?;
            temporary.write_all(b"\n")?;
            temporary.as_file().sync_all()?;
        }
        temporary
            .persist(directory.join("whatsapp-capture.json"))
            .map_err(|error| anyhow!("Cannot write WhatsApp diagnostic: {}", error.error))?;
        Ok(())
    }
}

struct WhatsAppCapture {
    pid: i32,
    app_id: String,
    executable: String,
    name: String,
    last_stage: &'static str,
    stages: BTreeMap<&'static str, u32>,
    compatibility: serde_json::Value,
    last_validated_gap_failure: Option<geometry::GapDiagnostic>,
}

/// Describes an AX attribute by type/count only; strings are exposed only for role constants.
fn ax_shape(element: &AxElement, name: &str) -> String {
    match element.attribute(name) {
        Err(error) => format!("error:{}", error.0),
        Ok(None) => "absent".into(),
        Ok(Some(value)) => {
            if let Some(value) = value.as_bool() {
                format!("bool:{value}")
            } else if value.as_element().is_some() {
                "element".into()
            } else if let Some(items) = value.as_elements() {
                format!("array:{}", items.len())
            } else if let Some(text) = value.as_string() {
                if matches!(name, "AXRole" | "AXSubrole") {
                    format!("string:{text}")
                } else {
                    "string".into()
                }
            } else if value.as_number().is_some() {
                "number".into()
            } else {
                "other".into()
            }
        }
    }
}
fn element_shape(element: &AxElement, pid: i32) -> serde_json::Value {
    let mut ancestry = Vec::new();
    let mut current = element.element("AXParent");
    while let Some(parent) = current.take() {
        if ancestry.len() == 24 {
            ancestry.push("truncated".to_string());
            break;
        }
        ancestry.push(ax_shape(&parent, "AXRole"));
        current = parent.element("AXParent");
    }
    serde_json::json!({
        "same_process": element.pid() == Some(pid),
        "role": ax_shape(element, "AXRole"),
        "subrole": ax_shape(element, "AXSubrole"),
        "focused": ax_shape(element, "AXFocused"),
        "enabled": ax_shape(element, "AXEnabled"),
        "window": ax_shape(element, "AXWindow"),
        "top_level": ax_shape(element, "AXTopLevelUIElement"),
        "selected_range": ax_shape(element, "AXSelectedTextRange"),
        "value_settable": element.settable("AXValue"),
        "selected_text_settable": element.settable("AXSelectedText"),
        "ancestry_roles": ancestry,
    })
}
fn whatsapp_ax_shape(pid: i32) -> serde_json::Value {
    let Some(app) = AxElement::application(pid) else {
        return serde_json::json!({"application":"unavailable"});
    };
    let attributes: serde_json::Map<String, serde_json::Value> = [
        "AXRole",
        "AXFrontmost",
        "AXFocusedWindow",
        "AXMainWindow",
        "AXWindows",
        "AXChildren",
        "AXFocusedUIElement",
        "AXManualAccessibility",
        "AXEnhancedUserInterface",
    ]
    .into_iter()
    .map(|name| (name.to_string(), ax_shape(&app, name).into()))
    .collect();
    let windows: Vec<serde_json::Value> = app
        .attribute("AXWindows")
        .ok()
        .flatten()
        .and_then(|value| value.as_elements())
        .unwrap_or_default()
        .iter()
        .take(8)
        .map(|window| {
            serde_json::json!({
                "role": ax_shape(window, "AXRole"),
                "subrole": ax_shape(window, "AXSubrole"),
                "focused": ax_shape(window, "AXFocused"),
                "main": ax_shape(window, "AXMain"),
                "minimized": ax_shape(window, "AXMinimized"),
                "children": ax_shape(window, "AXChildren"),
            })
        })
        .collect();
    let system_focus = AxElement::system_wide()
        .and_then(|system| system.element("AXFocusedUIElement"))
        .filter(|element| element.pid() == Some(pid));
    serde_json::json!({
        "application": attributes,
        "manual_accessibility_settable": app.settable("AXManualAccessibility"),
        "enhanced_user_interface_settable": app.settable("AXEnhancedUserInterface"),
        "windows": windows,
        "application_focused_element": app.element("AXFocusedUIElement").map(|element| element_shape(&element, pid)),
        "system_focused_element": system_focus.map(|element| element_shape(&element, pid)),
    })
}

impl NativeBridge for MacBridge {
    fn configure(&mut self, config: &Config) -> Result<()> {
        self.owner()?;
        let effective = effective_config(config);
        let policy = Arc::new(ObservationPolicy::new(&effective)?);
        self.policy_epoch.fetch_add(1, Ordering::AcqRel);
        self.clear_pending_unicode();
        if let Some(capture) = self.whatsapp_capture.as_mut() {
            capture.last_validated_gap_failure = None;
        }
        if let Some(docs) = self.docs.as_mut() {
            docs.reset()
        }
        if let Some(adapter) = self.terminal.as_mut() {
            adapter.configure(config)?
        }
        if let Some(adapter) = self.appearance.as_mut() {
            adapter.configure(config)?
        }
        self.policy = policy.clone();
        self.input.configure(policy);
        self.config = config.clone();
        self.compatibility = None;
        Ok(())
    }
    fn capture(&mut self) -> Result<Option<Snapshot>> {
        self.owner()?;
        self.expire_pending_unicode();
        self.pending_capture_transient = false;
        if self.stopped {
            self.clear_pending_unicode();
            return Ok(None);
        }
        // A previous failure stage must never authorize this capture's None.
        self.capture_diagnostic = "capture_started";
        let result = self.capture_native();
        self.expire_pending_unicode();
        self.record_whatsapp_stage();
        match &result {
            Ok(Some(snapshot)) => {
                if self.pending_unicode_post.as_ref().is_some_and(|lease| {
                    !lease.matches(snapshot, self.input.user_revision())
                        || lease.policy_epoch != self.policy_epoch.load(Ordering::Acquire)
                }) {
                    self.clear_pending_unicode();
                }
            }
            Ok(None) if transient_insert_capture(self.capture_diagnostic) => {
                self.pending_capture_transient = self.pending_unicode_post.is_some();
            }
            _ => self.clear_pending_unicode(),
        }
        result
    }
    fn pending_edit_capture_is_transient(&mut self, before: &Snapshot, user_revision: u64) -> bool {
        // Only the immediately preceding real capture can supply this proof.
        // Runtime owns the original pending deadline; this does not accept text.
        if !std::mem::take(&mut self.pending_capture_transient) {
            return false;
        }
        self.pending_unicode_scope_matches(before, user_revision)
            .unwrap_or(false)
    }
    fn foreground(&mut self) -> Result<Option<Foreground>> {
        self.owner()?;
        if let Some(session) = self.session()? {
            return Ok(Some(session.foreground));
        }
        #[cfg(feature = "native-smoke")]
        if self.owned_target.is_some() {
            return Ok(None);
        }
        // Excluded/secure fields still expose application identity to App Settings,
        // but no AX element/value is requested to obtain that metadata.
        Ok(frontmost_application()
            .filter(|(pid, _, _, _)| *pid != std::process::id() as i32)
            .map(|(pid, app_id, executable, _)| Foreground {
                app_id,
                executable,
                window_pid: pid,
                ..Default::default()
            }))
    }
    fn insert(&mut self, snapshot: &Snapshot, text: &str) -> Result<EditOutcome> {
        self.owner()?;
        self.clear_pending_unicode();
        let terminal = exact_member(&snapshot.app_id, TERMINALS);
        if !terminal {
            self.record_insertion(snapshot, InsertionReason::NotIssued, None, None);
        }
        if !valid_insert(snapshot, text) {
            return Ok(EditOutcome::Rejected);
        }
        if terminal {
            if !self.config.terminal_completion || !snapshot.field_id.starts_with("terminal:") {
                return Ok(EditOutcome::Rejected);
            }
            let Some(mut adapter) = self.terminal.take() else {
                bail!("Terminal completion requires the native terminal integration")
            };
            let result = adapter.append(&mut self.capture_services(), snapshot, text);
            self.terminal = Some(adapter);
            self.input.mutation_finished();
            return result;
        }
        let Some((live, field)) = self.checked_current(snapshot)? else {
            return Ok(EditOutcome::Rejected);
        };
        if field.set_timeout(0.25).is_err() {
            return Ok(EditOutcome::Rejected);
        }
        let user_revision = self.input.user_revision();
        let unicode = uses_unicode_insertion(&field, &live.app_id);
        if field.settable("AXSelectedText") && !unicode {
            if !self.input.commit() {
                return Ok(EditOutcome::Rejected);
            }
            let (outcome, reason, readback) = apply_selected_text(
                &mut NativeSelectedText {
                    bridge: self,
                    field,
                },
                snapshot,
                snapshot.caret,
                0,
                text,
            );
            self.record_insertion(snapshot, reason, readback, Some(user_revision));
            return Ok(outcome);
        }
        #[cfg(feature = "native-smoke")]
        if self.owned_target.is_some() {
            // This lane never falls back to keyboard posting.
            return Ok(EditOutcome::Rejected);
        }
        if !(field.settable("AXValue") || unicode) || self.input.preflight_unicode().is_err() {
            return Ok(EditOutcome::Rejected);
        }
        let policy_epoch = self.policy_epoch.load(Ordering::Acquire);
        let lease = UnicodePostLease {
            field,
            before: snapshot.key(),
            window_id: snapshot.window_id.clone(),
            window_revision: snapshot.window_revision.clone(),
            window_pid: snapshot.window_pid,
            user_revision,
            policy_epoch,
            // Fixed before commit/post; failed reads and proofs never renew it.
            expires_at: Instant::now() + Duration::from_secs(2),
        };
        if !self.input.commit() {
            return Ok(EditOutcome::Rejected);
        }
        let (outcome, reason, readback) = apply_unicode_text(
            &mut NativeUnicodeText {
                bridge: self,
                lease: Some(lease),
                user_revision,
            },
            snapshot,
            text,
        );
        self.expire_pending_unicode();
        self.record_insertion(snapshot, reason, readback, Some(user_revision));
        if !matches!(&outcome, EditOutcome::Dispatched) {
            self.clear_pending_unicode();
        }
        Ok(outcome)
    }
    fn replace_range(
        &mut self,
        snapshot: &Snapshot,
        start: usize,
        length: usize,
        text: &str,
    ) -> Result<EditOutcome> {
        self.owner()?;
        self.clear_pending_unicode();
        if !valid_replacement(snapshot, start, length, text) {
            return Ok(EditOutcome::Rejected);
        }
        let Some((live, field)) = self.checked_current(snapshot)? else {
            return Ok(EditOutcome::Rejected);
        };
        if !live.can_replace
            || uses_unicode_insertion(&field, &live.app_id)
            || !field.settable("AXSelectedText")
            || field.set_timeout(0.25).is_err()
        {
            return Ok(EditOutcome::Rejected);
        }
        if (start, length) != (snapshot.caret, snapshot.selection_length) {
            if !field.settable("AXSelectedTextRange") {
                return Ok(EditOutcome::Rejected);
            }
            let Some(range) = scalar_native_range(snapshot, start, length) else {
                return Ok(EditOutcome::Rejected);
            };
            #[cfg(feature = "native-smoke")]
            if !self.owned_current() {
                return Ok(EditOutcome::Rejected);
            }
            if !self.input.commit() {
                return Ok(EditOutcome::Rejected);
            }
            let selection = field.set_range("AXSelectedTextRange", range);
            self.input.mutation_finished();
            if selection.is_err() {
                return Ok(EditOutcome::Indeterminate);
            }
            let mut expected = snapshot.clone();
            expected.caret = start;
            expected.selection_length = length;
            if !matches!(self.checked_current(&expected), Ok(Some(_))) {
                return Ok(EditOutcome::Indeterminate);
            }
        }
        if !self.input.commit() {
            return Ok(EditOutcome::Rejected);
        }
        Ok(apply_selected_text(
            &mut NativeSelectedText {
                bridge: self,
                field,
            },
            snapshot,
            start,
            length,
            text,
        )
        .0)
    }
    fn replace_emoji_range(
        &mut self,
        snapshot: &Snapshot,
        start: usize,
        length: usize,
        text: &str,
    ) -> Result<EditOutcome> {
        if snapshot.can_replace {
            return self.replace_range(snapshot, start, length, text);
        }
        self.owner()?;
        self.clear_pending_unicode();
        if !snapshot.can_replace_emoji
            || snapshot.protected
            || !snapshot.valid_range()
            || snapshot.selection_length != 0
            || length == 0
            || start.checked_add(length) != Some(snapshot.caret)
            || text.is_empty()
            || text.contains('\0')
        {
            return Ok(EditOutcome::Rejected);
        }
        // Prepare both the native range and exact readback before any mutation.
        let Some(range) = scalar_native_range(snapshot, start, length) else {
            return Ok(EditOutcome::Rejected);
        };
        let Some(expected) = expected_mutation(snapshot, start, length, text) else {
            return Ok(EditOutcome::Rejected);
        };
        let revision = self.input.user_revision();
        let Some((live, field)) = self.checked_current(snapshot)? else {
            return Ok(EditOutcome::Rejected);
        };
        #[cfg(feature = "native-smoke")]
        if self.owned_target.is_some() && !self.owned_emoji_current(&field) {
            // Only the separate exact-field emoji enrollment may post to its PID.
            return Ok(EditOutcome::Rejected);
        }
        let Some(started) = terminal::process_started(live.window_pid) else {
            return Ok(EditOutcome::Rejected);
        };
        let Some(window) = field.element("AXWindow") else {
            return Ok(EditOutcome::Rejected);
        };
        if !live.can_replace_emoji
            || !self.emoji_replacement_supported(&field, &live.app_id)
            || field.set_timeout(0.25).is_err()
            || self.input.preflight_unicode().is_err()
            || self.input.user_revision() != revision
            || !self.input.commit()
        {
            return Ok(EditOutcome::Rejected);
        }
        let selected = field.set_range("AXSelectedTextRange", range);
        self.input.mutation_finished();
        // From this point even a failed setter may have changed selection.
        // Never replay the initiating key or retry either native mutation.
        if selected.is_err() {
            return Ok(EditOutcome::Indeterminate);
        }
        let Ok(Some((selected, selected_field))) = self.capture_ordinary() else {
            return Ok(EditOutcome::Indeterminate);
        };
        if selected_field != field
            || !selected.matches_pending_emoji_selection(snapshot, start, length)
            || !self.emoji_replacement_supported(&selected_field, &selected.app_id)
            || self.input.user_revision() != revision
            || self.input.preflight_unicode().is_err()
            || !self.input.commit()
        {
            return Ok(EditOutcome::Indeterminate);
        }
        let posted = self
            .input
            .post_unicode_to_pid(text, selected.window_pid, || {
                !self.stopped
                    && ax::trusted()
                    && secure_event_input_enabled() == Some(false)
                    && self.input.user_revision() == revision
                    && self.composition_clear(&selected_field)
                    && terminal::process_started(selected.window_pid) == Some(started)
                    && selected_field.pid() == Some(selected.window_pid)
                    && selected_field.element("AXWindow").as_ref() == Some(&window)
                    && frontmost_application().is_some_and(|(pid, app_id, _, _)| {
                        pid == selected.window_pid && app_id == selected.app_id
                    })
                    && AxElement::application(selected.window_pid)
                        .and_then(|app| app.element("AXFocusedUIElement"))
                        .as_ref()
                        == Some(&selected_field)
                    && self.emoji_replacement_supported(&selected_field, &selected.app_id)
            });
        self.input.mutation_finished();
        if posted.is_err() || self.input.user_revision() != revision {
            return Ok(EditOutcome::Indeterminate);
        }
        let Ok(Some((after, _))) = self.capture_ordinary() else {
            return Ok(EditOutcome::Indeterminate);
        };
        if self.input.user_revision() != revision {
            return Ok(EditOutcome::Indeterminate);
        }
        Ok(classify_emoji_readback(
            snapshot, &after, &expected, true, start, length,
        ))
    }
    fn range_rect(
        &mut self,
        snapshot: &Snapshot,
        start: usize,
        length: usize,
    ) -> Result<Option<Rect>> {
        self.owner()?;
        if snapshot.protected
            || snapshot.caret_rect.is_none()
            || length == 0
            || snapshot
                .text
                .index()
                .scalar_range_to_utf8(start, length)
                .is_none()
        {
            return Ok(None);
        }
        let end = start + length;
        let left = start.min(snapshot.caret);
        let right = end.max(snapshot.caret);
        if snapshot
            .text
            .slice(left, right - left)
            .is_none_or(|span| span.contains(['\r', '\n']))
        {
            return Ok(None);
        }
        let Some((live, field)) = self.checked_current(snapshot)? else {
            return Ok(None);
        };
        let Some(native) = scalar_native_range(&live, start, length) else {
            return Ok(None);
        };
        let Some(bounds) = geometry::range_bounds(&field, &self.secure_description, native) else {
            return Ok(None);
        };
        if live.caret == 0 {
            return Ok(None);
        }
        let Some(row_range) = scalar_native_range(&live, live.caret - 1, 1) else {
            return Ok(None);
        };
        let Some(row) = geometry::range_bounds(&field, &self.secure_description, row_range) else {
            return Ok(None);
        };
        if bounds[2] <= 0. || (bounds[1] - row[1]).abs() > 1. || (bounds[3] - row[3]).abs() > 1. {
            return Ok(None);
        }
        if live.field_rect.is_some_and(|field| {
            bounds[0] < field[0] - 1. || bounds[0] + bounds[2] > field[0] + field[2] + 1.
        }) {
            return Ok(None);
        }
        Ok(Some(bounds))
    }
    fn focused_context(
        &mut self,
        snapshot: &Snapshot,
        authorized: &dyn Fn() -> bool,
        max_chars: usize,
    ) -> Result<ContextRead> {
        self.owner()?;
        if !authorized() {
            return Ok(ContextRead::blocked("accessibility"));
        }
        let Some(ticket) = MacBridge::context_ticket(self, snapshot)? else {
            return Ok(ContextRead::blocked("accessibility"));
        };
        ticket.read(authorized, max_chars)
    }
    fn context_ticket(
        &mut self,
        snapshot: &Snapshot,
    ) -> Result<Option<Box<dyn super::DetachedContextRead>>> {
        self.owner()?;
        if exact_member(&snapshot.app_id, TERMINALS) {
            if !self.config.terminal_completion || !snapshot.field_id.starts_with("terminal:") {
                return Ok(None);
            }
            let Some(mut adapter) = self.terminal.take() else {
                return Ok(None);
            };
            let result = adapter.context_ticket(&mut self.capture_services(), snapshot);
            self.terminal = Some(adapter);
            return result;
        }
        Ok(MacBridge::context_ticket(self, snapshot)?
            .map(|ticket| Box::new(ticket) as Box<dyn super::DetachedContextRead>))
    }
    fn docs_hint(&mut self) -> Result<Option<serde_json::Value>> {
        self.owner()?;
        let allowed = !self.stopped && self.policy.global_enabled;
        let Some(mut docs) = self.docs.take() else {
            return Ok(None);
        };
        let hint = docs.hint(&mut self.capture_services(), &|| allowed);
        self.docs = Some(docs);
        hint.map(serde_json::to_value)
            .transpose()
            .map_err(Into::into)
    }
    fn enable_docs_accessibility(&mut self, token: &str) -> Result<serde_json::Value> {
        self.owner()?;
        let allowed = !self.stopped && self.policy.global_enabled;
        let Some(mut docs) = self.docs.take() else {
            bail!("Docs native helper is unavailable")
        };
        let result = docs.enable(&mut self.capture_services(), token, &|| allowed);
        self.docs = Some(docs);
        Ok(serde_json::to_value(result)?)
    }
    fn atomic_replacement_supported(&self) -> bool {
        true
    }
    fn replace_selection(&mut self, snapshot: &Snapshot, text: &str) -> Result<EditOutcome> {
        if snapshot.selection_length == 0 {
            return Ok(EditOutcome::Rejected);
        }
        self.replace_range(snapshot, snapshot.caret, snapshot.selection_length, text)
    }
    fn context_matches(&mut self, snapshot: &Snapshot) -> Result<bool> {
        self.owner()?;
        if self.stopped || self.input.dead_key() {
            return Ok(false);
        }
        if exact_member(&snapshot.app_id, TERMINALS) {
            if !self.config.terminal_completion || !snapshot.field_id.starts_with("terminal:") {
                return Ok(false);
            }
            let Some(mut adapter) = self.terminal.take() else {
                return Ok(false);
            };
            let result = adapter.context_matches(&mut self.capture_services(), snapshot);
            self.terminal = Some(adapter);
            return result;
        }
        let Some(focus) = self.focused()? else {
            return Ok(false);
        };
        Ok(self.focus_matches_snapshot(&focus, snapshot)
            && self.composition_clear(&focus.field)
            && focus.field.frame().is_some())
    }
    fn pump_events(&mut self) -> Result<()> {
        self.refresh_main_state()
    }
    fn start_shortcuts(&mut self, callback: ShortcutHandler) -> Result<()> {
        self.owner()?;
        ensure!(!self.stopped, "Native integration is stopped");
        self.refresh_main_state()?;
        self.input.start(callback)
    }
    fn set_shortcuts_enabled(&mut self, enabled: bool) {
        self.input
            .set_enabled(enabled && !self.stopped && secure_event_input_enabled() == Some(false))
    }
    fn set_input_callback(&mut self, callback: Option<Arc<dyn Fn() + Send + Sync>>) {
        self.input.set_callback(callback)
    }
    fn finish_input(&mut self, accepted: bool) -> Result<()> {
        self.input.finish(accepted)
    }
    fn input_revision(&self) -> u64 {
        self.input.revision()
    }
    fn user_input_revision(&self) -> u64 {
        self.input.user_revision()
    }
    fn input_timestamp(&self) -> Option<Instant> {
        self.input.timestamp()
    }
    fn secure_input_enabled(&self) -> Option<bool> {
        secure_event_input_enabled()
    }
    fn request_permission(&mut self, kind: &str) -> Result<Permission> {
        self.owner()?;
        let granted = match kind {
            "accessibility" => ax::request_trust(),
            "input_monitoring" => unsafe { CGRequestListenEventAccess() },
            "post_event" => unsafe { CGRequestPostEventAccess() },
            "screen_recording" => unsafe { CGRequestScreenCaptureAccess() },
            _ => return Ok(Permission::Unknown),
        };
        Ok(if granted {
            Permission::Allowed
        } else {
            Permission::Denied
        })
    }
    fn status(&self) -> serde_json::Value {
        let secure = secure_event_input_enabled();
        let trusted = ax::trusted();
        let source = match self.input.source_kind() {
            InputSourceKind::Layout => "layout",
            InputSourceKind::Ime => "ime",
            InputSourceKind::Unknown => "unknown",
        };
        let diagnostic = if self.stopped {
            "stopped"
        } else if secure.is_none() {
            "secure_input_unknown"
        } else if secure == Some(true) {
            "secure_input_active"
        } else if !trusted {
            "accessibility_denied"
        } else if source == "unknown" {
            "input_source_unknown"
        } else if self.input.dead_key() {
            "composition_active"
        } else {
            "native_available"
        };
        let input = self.input.status();
        let keyboard = input["keyboard"].as_str().unwrap_or("unavailable");
        let message = format!(
            "Native input: {diagnostic}. Keyboard: {keyboard}. Last ordinary field capture: {}.",
            self.capture_diagnostic
        );
        serde_json::json!({"platform":"macos","accessibility":trusted,"secure_input":secure,"input_source":source,"input":input,"diagnostic":diagnostic,"capture_diagnostic":self.capture_diagnostic,"message":message,"stopped":self.stopped})
    }
    fn stop(&mut self) -> Result<()> {
        self.owner()?;
        self.stopped = true;
        self.policy_epoch.fetch_add(1, Ordering::AcqRel);
        self.clear_pending_unicode();
        if let Some(capture) = self.whatsapp_capture.as_mut() {
            capture.last_validated_gap_failure = None;
        }
        self.input.set_enabled(false);
        let mut failures = Vec::new();
        if let Err(error) = self.input.stop() {
            failures.push(error.to_string())
        }
        self.observer = None;
        self.observer_pid = 0;
        self.notifications = None;
        if let Some(docs) = self.docs.as_mut() {
            docs.reset()
        }
        if let Some(adapter) = self.terminal.as_mut() {
            if let Err(error) = adapter.stop() {
                failures.push(error.to_string())
            } else {
                self.terminal = None
            }
        }
        if let Some(adapter) = self.appearance.as_mut() {
            if let Err(error) = adapter.stop() {
                failures.push(error.to_string())
            } else {
                self.appearance = None
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(anyhow!(
                "Native cleanup incomplete: {}",
                failures.join("; ")
            ))
        }
    }
}
impl MacBridge {
    fn focus_matches_snapshot(&self, focus: &Focused, snapshot: &Snapshot) -> bool {
        let front = &focus.session.foreground;
        front.app_id == snapshot.app_id
            && front.domain == snapshot.domain
            && front.window_pid == snapshot.window_pid
            && front.window_id == snapshot.window_id
            && focus.field_id == snapshot.field_id
            && focus.session.document_id == snapshot.window_revision
    }
    fn clear_pending_unicode(&mut self) {
        self.pending_unicode_post = None;
        self.pending_capture_transient = false;
    }
    fn expire_pending_unicode(&mut self) {
        // Normal owner pumping must not read the clock when no lease is held.
        if self
            .pending_unicode_post
            .as_ref()
            .is_some_and(|lease| Instant::now() >= lease.expires_at)
        {
            self.clear_pending_unicode();
        }
    }
    fn pending_unicode_scope_matches(
        &mut self,
        before: &Snapshot,
        user_revision: u64,
    ) -> Result<bool> {
        let Some(lease) = self.pending_unicode_post.take() else {
            return Ok(false);
        };
        let stage = self.capture_diagnostic;
        let result = self.fresh_unicode_scope_matches(&lease, before, user_revision);
        let result = finish_unicode_scope_proof(result, lease.expires_at, Instant::now);
        // Metadata probes must not relabel the actual failed capture.
        self.capture_diagnostic = stage;
        if matches!(&result, Ok(true)) {
            self.pending_unicode_post = Some(lease);
        }
        result
    }
    fn fresh_unicode_scope_matches(
        &mut self,
        lease: &UnicodePostLease,
        before: &Snapshot,
        user_revision: u64,
    ) -> Result<bool> {
        self.owner()?;
        if Instant::now() >= lease.expires_at {
            return Ok(false);
        }
        let policy_epoch = self.policy_epoch.load(Ordering::Acquire);
        if self.stopped
            || !lease.matches(before, user_revision)
            || self.input.user_revision() != user_revision
            || policy_epoch != lease.policy_epoch
            || self.input.preflight_unicode().is_err()
        {
            return Ok(false);
        }
        // This fence starts AFTER the failed capture. Own AX value/layout
        // notifications preceding this fresh proof do not revoke it.
        let revision = self.input.revision();
        let Some(focus) = self.focused()? else {
            return Ok(false);
        };
        if focus.field != lease.field || !self.unicode_focus_matches(&focus, before) {
            return Ok(false);
        }
        // focused() independently rechecks foreground/process/document,
        // policy, AX trust, secure input, writability and protection.
        let Some(after) = self.focused()? else {
            return Ok(false);
        };
        Ok(focus.same(&after)
            && self.unicode_focus_matches(&after, before)
            && self.input.preflight_unicode().is_ok()
            && self.input.user_revision() == user_revision
            && self.policy_epoch.load(Ordering::Acquire) == policy_epoch
            && self.input.revision() == revision
            && !self.native_dirty.load(Ordering::Acquire))
    }
    fn unicode_focus_matches(&self, focus: &Focused, before: &Snapshot) -> bool {
        let field = &focus.field;
        let app_id = &focus.session.foreground.app_id;
        let unicode = uses_unicode_insertion(field, app_id);
        self.focus_matches_snapshot(focus, before)
            && !before.protected
            && !before.on_demand_only
            && self.composition_clear(field)
            && field.frame().is_some()
            && field
                .range("AXSelectedTextRange")
                .is_some_and(|range| range.length == 0)
            && (field.settable("AXValue") || unicode)
            && before.can_replace == (!unicode && field.settable("AXSelectedText"))
            && before.can_replace_emoji == self.emoji_replacement_supported(field, app_id)
            && before.multiline == (field.string("AXRole").as_deref() == Some("AXTextArea"))
    }
}
impl Drop for MacBridge {
    fn drop(&mut self) {
        if self.owner == thread::current().id() {
            let _ = self.stop();
        }
    }
}

trait AccessibilityPreparationHost {
    fn settable(&self, name: &str) -> bool;
    fn boolean(&self, name: &str) -> Option<bool>;
    fn enable(&self, name: &str) -> std::result::Result<(), ax::AxError>;
}
impl AccessibilityPreparationHost for AxElement {
    fn settable(&self, name: &str) -> bool {
        AxElement::settable(self, name)
    }
    fn boolean(&self, name: &str) -> Option<bool> {
        self.bool(name)
    }
    fn enable(&self, name: &str) -> std::result::Result<(), ax::AxError> {
        self.set_bool(name, true)
    }
}
fn enable_accessibility_attribute(host: &impl AccessibilityPreparationHost, name: &str) -> String {
    if !host.settable(name) {
        return "not_settable".into();
    }
    match host.boolean(name) {
        Some(false) => match host.enable(name) {
            Ok(()) => "set".into(),
            Err(error) => format!("set_error:{}", error.0),
        },
        Some(true) => "already_true".into(),
        None => "unreadable_not_set".into(),
    }
}

fn exact_member(value: &str, values: &[&str]) -> bool {
    values.iter().any(|item| value.eq_ignore_ascii_case(item))
}
fn stock_chat_description(value: &str) -> bool {
    CHAT_NAMES.iter().any(|name| {
        value == *name
            || value
                .strip_prefix(name)
                .is_some_and(|suffix| suffix.starts_with(['.', ' ']) || suffix.starts_with(", "))
    })
}
fn safe_visible(field: &AxElement, secure_description: &str) -> bool {
    for name in ["AXHidden", "AXProtectedContent"] {
        match field.attribute(name) {
            Ok(None) => {}
            Ok(Some(value)) if value.as_bool() == Some(false) => {}
            _ => return false,
        }
    }
    for (name, forbidden) in [
        ("AXSubrole", "AXSecureTextField"),
        ("AXRoleDescription", secure_description),
    ] {
        match field.attribute(name) {
            Ok(None) => {}
            Ok(Some(value)) if value.as_string().is_some_and(|value| value != forbidden) => {}
            _ => return false,
        }
    }
    true
}
fn field_is_ordinary_writable(field: &AxElement, secure_description: &str) -> bool {
    if !matches!(
        field.string("AXRole").as_deref(),
        Some("AXTextField" | "AXTextArea")
    ) || !safe_visible(field, secure_description)
    {
        return false;
    }
    for name in ["AXEnabled", "AXFocused"] {
        match field.attribute(name) {
            Ok(None) => {}
            Ok(Some(value)) if value.as_bool() == Some(true) => {}
            _ => return false,
        }
    }
    geometry::field_ancestry(field, secure_description).is_some()
        && (field.settable("AXSelectedText") || field.settable("AXValue"))
}
fn uses_unicode_insertion(field: &AxElement, app_id: &str) -> bool {
    exact_member(app_id, CHAT_BUNDLES) || matches!(field.attribute("ChromeAXNodeId"), Ok(Some(_)))
}
fn valid_insert(snapshot: &Snapshot, text: &str) -> bool {
    !text.is_empty()
        && !text.contains('\0')
        && !snapshot.protected
        && snapshot.selection_length == 0
        && snapshot.valid_range()
}
fn valid_replacement(snapshot: &Snapshot, start: usize, length: usize, text: &str) -> bool {
    snapshot.can_replace
        && !snapshot.protected
        && !text.is_empty()
        && !text.contains('\0')
        && snapshot
            .text
            .index()
            .scalar_range_to_utf8(start, length)
            .is_some()
        && snapshot.valid_range()
}
fn effective_config(config: &Config) -> Config {
    let mut effective = config.clone();
    if config.terminal_completion && config.excluded_apps == Config::default().excluded_apps {
        effective
            .excluded_apps
            .retain(|item| !matches!(item.as_str(), "terminal" | "iterm"));
    }
    effective
}
fn scalar_native_range(snapshot: &Snapshot, start: usize, length: usize) -> Option<AxRange> {
    let end = start.checked_add(length)?;
    let location = snapshot.text.index().scalar_to_utf16(ScalarIndex(start))?.0;
    let end = snapshot.text.index().scalar_to_utf16(ScalarIndex(end))?.0;
    Some(AxRange {
        location,
        length: end - location,
    })
}
fn expected_mutation(
    snapshot: &Snapshot,
    start: usize,
    length: usize,
    text: &str,
) -> Option<(String, usize)> {
    let bytes = snapshot.text.index().scalar_range_to_utf8(start, length)?;
    let mut expected = String::with_capacity(snapshot.text.len() - bytes.len() + text.len());
    expected.push_str(&snapshot.text[..bytes.start]);
    expected.push_str(text);
    expected.push_str(&snapshot.text[bytes.end..]);
    Some((expected, start.checked_add(text.chars().count())?))
}
/// Ordinary Unicode delivery and its immediate readback. A successful post is
/// delivery evidence only; native target proof cannot manufacture a Snapshot.
trait UnicodeTextHost {
    fn post(&mut self, text: &str) -> Result<()>;
    fn observe(&mut self) -> Result<Option<Snapshot>>;
    fn capture_stage(&self) -> &'static str;
    fn pending_authority_matches(&mut self, before: &Snapshot) -> Result<bool>;
}
/// Successful ordinary Unicode-post identity, retained only while a readback
/// can still be pending. This private lease is never diagnostic payload.
struct UnicodePostLease {
    field: AxElement,
    before: crate::core::SnapshotKey,
    window_id: String,
    window_revision: String,
    window_pid: i32,
    user_revision: u64,
    policy_epoch: u64,
    expires_at: Instant,
}
impl UnicodePostLease {
    fn matches(&self, before: &Snapshot, user_revision: u64) -> bool {
        self.before.matches(before)
            && self.window_id == before.window_id
            && self.window_revision == before.window_revision
            && self.window_pid == before.window_pid
            && self.user_revision == user_revision
    }
}

/// A proof that began within the lease may finish too late. Check its original
/// deadline after native I/O, before restoring ownership or claiming deferral.
fn finish_unicode_scope_proof(
    proof: Result<bool>,
    expires_at: Instant,
    now: impl FnOnce() -> Instant,
) -> Result<bool> {
    proof.map(|verified| verified && now() < expires_at)
}

fn transient_insert_capture(stage: &str) -> bool {
    // Only the observed final-anchor race is eligible. In particular,
    // field_value_unavailable conflates absent, erroneous and malformed AX
    // values, so aggregate counts cannot make it safe to defer.
    stage == "selection_or_geometry_changed_after_capture"
}

struct NativeUnicodeText<'a> {
    bridge: &'a mut MacBridge,
    lease: Option<UnicodePostLease>,
    user_revision: u64,
}
impl UnicodeTextHost for NativeUnicodeText<'_> {
    fn post(&mut self, text: &str) -> Result<()> {
        let lease = self
            .lease
            .take()
            .ok_or_else(|| anyhow!("Unicode post scope has already been consumed"))?;
        let result = self.bridge.input.post_unicode(text);
        self.bridge.input.mutation_finished();
        if result.is_ok() {
            self.bridge.pending_unicode_post = Some(lease);
        }
        result
    }
    fn observe(&mut self) -> Result<Option<Snapshot>> {
        self.bridge.capture_diagnostic = "edit_readback_started";
        Ok(self
            .bridge
            .capture_ordinary()?
            .map(|(snapshot, _)| snapshot))
    }
    fn capture_stage(&self) -> &'static str {
        self.bridge.capture_diagnostic
    }
    fn pending_authority_matches(&mut self, before: &Snapshot) -> Result<bool> {
        self.bridge
            .pending_unicode_scope_matches(before, self.user_revision)
    }
}
fn apply_unicode_text(
    host: &mut impl UnicodeTextHost,
    before: &Snapshot,
    text: &str,
) -> (EditOutcome, InsertionReason, Option<InsertionReadback>) {
    if host.post(text).is_err() {
        return (EditOutcome::Indeterminate, InsertionReason::PostError, None);
    }
    let after = match host.observe() {
        Ok(Some(after)) => after,
        Ok(None) => {
            let pending = transient_insert_capture(host.capture_stage())
                && valid_insert(before, text)
                && host.pending_authority_matches(before).unwrap_or(false);
            return (
                if pending {
                    EditOutcome::Dispatched
                } else {
                    EditOutcome::Indeterminate
                },
                InsertionReason::CaptureUnavailable,
                None,
            );
        }
        Err(_) => {
            return (
                EditOutcome::Indeterminate,
                InsertionReason::CaptureError,
                None,
            );
        }
    };
    let Some(expected) = expected_mutation(before, before.caret, 0, text) else {
        return (
            EditOutcome::Indeterminate,
            InsertionReason::ExpectedMutationUnavailable,
            None,
        );
    };
    let (outcome, reason, readback) = classify_readback(before, &after, &expected, true);
    (outcome, reason, Some(readback))
}

trait SelectedTextHost {
    fn write_selected(&mut self, text: &str) -> std::result::Result<(), ax::AxError>;
    fn observe(&mut self) -> Result<Option<Snapshot>>;
}
struct NativeSelectedText<'a> {
    bridge: &'a mut MacBridge,
    field: AxElement,
}
impl SelectedTextHost for NativeSelectedText<'_> {
    fn write_selected(&mut self, text: &str) -> std::result::Result<(), ax::AxError> {
        #[cfg(feature = "native-smoke")]
        if !self.bridge.owned_current()
            || self.bridge.owned_target.as_ref().is_some_and(|target| {
                self.field.pid() != Some(target.pid)
                    || self.field.element("AXWindow").as_ref() != Some(&target.window)
            })
        {
            return Err(ax::AxError(-25202));
        }
        let result = self.field.set_string("AXSelectedText", text);
        self.bridge.input.mutation_finished();
        result
    }
    fn observe(&mut self) -> Result<Option<Snapshot>> {
        self.bridge.capture_diagnostic = "edit_readback_started";
        Ok(self
            .bridge
            .capture_ordinary()?
            .map(|(snapshot, _)| snapshot))
    }
}
fn apply_selected_text(
    host: &mut impl SelectedTextHost,
    before: &Snapshot,
    start: usize,
    length: usize,
    text: &str,
) -> (EditOutcome, InsertionReason, Option<InsertionReadback>) {
    if let Err(error) = host.write_selected(text) {
        let outcome = if mutation_may_have_applied(error) {
            EditOutcome::Indeterminate
        } else {
            EditOutcome::Rejected
        };
        return (outcome, InsertionReason::SetterError, None);
    }
    let after = match host.observe() {
        Ok(Some(after)) => after,
        Ok(None) => {
            return (
                EditOutcome::Indeterminate,
                InsertionReason::CaptureUnavailable,
                None,
            );
        }
        Err(_) => {
            return (
                EditOutcome::Indeterminate,
                InsertionReason::CaptureError,
                None,
            );
        }
    };
    let Some(expected) = expected_mutation(before, start, length, text) else {
        return (
            EditOutcome::Indeterminate,
            InsertionReason::ExpectedMutationUnavailable,
            None,
        );
    };
    let (outcome, reason, readback) = classify_readback(before, &after, &expected, false);
    (outcome, reason, Some(readback))
}
fn classify_readback(
    before: &Snapshot,
    after: &Snapshot,
    expected: &(String, usize),
    dispatched: bool,
) -> (EditOutcome, InsertionReason, InsertionReadback) {
    let same_app = before.app_id == after.app_id;
    let same_process = before.window_pid == after.window_pid;
    let same_window = before.window_id == after.window_id;
    let same_field = before.field_id == after.field_id;
    let same_document = before.window_revision == after.window_revision;
    let same_domain = before.domain == after.domain;
    let readback = InsertionReadback {
        authority_match: same_app
            && same_process
            && same_window
            && same_field
            && same_document
            && same_domain,
        text_match: after.text.as_ref() == expected.0,
        caret_match: after.caret == expected.1,
        selection_empty: after.selection_length == 0,
        unchanged_readback: before.same_key(after),
        same_app,
        same_process,
        same_window,
        same_field,
        same_document,
        same_domain,
    };
    // Preserve the existing classification precedence; diagnostics never grant
    // authority, claim dispatch, or retry a mutation.
    let (outcome, reason) = if !readback.authority_match {
        (EditOutcome::Indeterminate, InsertionReason::AuthorityChange)
    } else if readback.text_match && readback.caret_match && readback.selection_empty {
        (EditOutcome::Applied(after.clone()), InsertionReason::Exact)
    } else if readback.unchanged_readback {
        (
            if dispatched {
                EditOutcome::Dispatched
            } else {
                EditOutcome::Rejected
            },
            InsertionReason::Unchanged,
        )
    } else {
        (
            EditOutcome::Indeterminate,
            InsertionReason::TextOrCaretOrSelectionMismatch,
        )
    };
    (outcome, reason, readback)
}
fn classify_emoji_readback(
    before: &Snapshot,
    after: &Snapshot,
    expected: &(String, usize),
    dispatched: bool,
    start: usize,
    length: usize,
) -> EditOutcome {
    if !dispatched
        || !before.can_replace_emoji
        || before.protected
        || after.protected
        || !before.valid_range()
        || !after.valid_range()
        || before.can_replace != after.can_replace
        || before.can_replace_emoji != after.can_replace_emoji
        || before.multiline != after.multiline
        || before.on_demand_only != after.on_demand_only
        || length == 0
        || start
            .checked_add(length)
            .is_none_or(|end| end > before.text.scalar_len())
    {
        return EditOutcome::Indeterminate;
    }
    if after.matches_pending_emoji_selection(before, start, length) {
        return EditOutcome::Dispatched;
    }
    classify_readback(before, after, expected, true).0
}
fn mutation_may_have_applied(error: ax::AxError) -> bool {
    !matches!(
        error.0,
        -25201 | -25202 | -25205 | -25206 | -25211 | -25212 | -25213
    )
}
fn rect_intersects(a: Rect, b: Rect) -> bool {
    a[0] + a[2] > b[0] && a[1] + a[3] > b[1] && a[0] < b[0] + b[2] && a[1] < b[1] + b[3]
}
fn context_result(text: &str, status: &str) -> ContextRead {
    ContextRead {
        text: text.into(),
        status: status.into(),
        source: "accessibility".into(),
    }
}
/// Mutable draft contents and geometry are never document authority. Native apps
/// without AXDocument still have an exact process-incarnation/window binding.
fn session_document_identity(
    salt: &[u8; 32],
    browser: bool,
    app_id: &str,
    pid: i32,
    window: u64,
    document: Option<&str>,
    started: Option<(u64, u64)>,
) -> Option<(String, String)> {
    if pid <= 0 || window == 0 {
        return None;
    }
    if browser {
        let document = document?;
        let domain = native_domain(document)?;
        return Some((domain, document_digest(salt, document)));
    }
    let started = started?;
    let document = document.filter(|value| !value.is_empty());
    Some((
        String::new(),
        document_digest(
            salt,
            &format!("native:{app_id:?}:{pid}:{started:?}:{window}:{document:?}"),
        ),
    ))
}

fn document_digest(salt: &[u8; 32], value: &str) -> String {
    let mut digest =
        <Blake2sMac<U16> as KeyInit>::new_from_slice(salt).expect("fixed BLAKE2s key size");
    Mac::update(&mut digest, value.as_bytes());
    let bytes = digest.finalize().into_bytes();
    let mut result = String::with_capacity(32);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in bytes {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 15) as usize] as char)
    }
    result
}
fn snapshot_binding(salt: &[u8; 32], snapshot: &Snapshot) -> [u8; 16] {
    let mut digest =
        <Blake2sMac<U16> as KeyInit>::new_from_slice(salt).expect("fixed BLAKE2s key size");
    for value in [
        snapshot.app_id.as_str(),
        snapshot.field_id.as_str(),
        snapshot.domain.as_str(),
        snapshot.window_id.as_str(),
        snapshot.window_revision.as_str(),
        snapshot.text.as_ref(),
    ] {
        Mac::update(&mut digest, &(value.len() as u64).to_le_bytes());
        Mac::update(&mut digest, value.as_bytes());
    }
    for value in [
        snapshot.caret as u64,
        snapshot.selection_length as u64,
        snapshot.window_pid as u64,
    ] {
        Mac::update(&mut digest, &value.to_le_bytes())
    }
    Mac::update(
        &mut digest,
        &[
            snapshot.protected as u8,
            snapshot.multiline as u8,
            snapshot.on_demand_only as u8,
            snapshot.can_replace as u8,
            snapshot.can_replace_emoji as u8,
        ],
    );
    digest.finalize().into_bytes().into()
}
fn composition_clear_with_source(field: &AxElement, source: InputSourceKind) -> bool {
    match field.attribute("AXMarkedTextRange") {
        Ok(None) => source == InputSourceKind::Layout,
        Ok(Some(value)) => value.marked_text_clear() == Some(true),
        Err(_) => false,
    }
}
fn validate_appearance(
    snapshot: &Snapshot,
    evidence: &geometry::AppearanceEvidence,
    styled: InlineRender,
) -> Option<InlineRender> {
    if !matches!(
        styled.source.as_str(),
        "macos-ax-geometry+vision" | "macos-ax-attributed-run+vision"
    ) {
        return None;
    }
    let candidate = if let Some(base) = &evidence.ax_render {
        if styled.background_rgb.is_none()
            || styled.text_rgb.is_none()
            || (styled.baseline - base.baseline).abs() > 0.75
            || (styled.font_size - base.font_size).abs() > 0.01
        {
            return None;
        }
        let mut candidate = base.clone();
        candidate.source = "macos-ax-attributed-run+vision".into();
        candidate.suffix = evidence
            .suffix
            .as_ref()
            .map(|s| s.text.clone())
            .unwrap_or_default();
        candidate.suffix_width = evidence
            .suffix
            .as_ref()
            .map(|s| s.rect[2])
            .unwrap_or_default();
        candidate.text_rgb = styled.text_rgb;
        candidate.background_rgb = styled.background_rgb;
        candidate
    } else {
        if !styled.suffix.is_empty() || styled.suffix_width != 0. {
            return None;
        }
        styled
    };
    (candidate.valid(snapshot.caret_rect) && candidate.suffix == snapshot.line_suffix())
        .then_some(candidate)
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn TISCopyCurrentKeyboardInputSource() -> Raw;
    fn TISGetInputSourceProperty(source: Raw, key: Raw) -> Raw;
    static kTISPropertyInputSourceType: Raw;
    static kTISTypeKeyboardLayout: Raw;
    static kTISTypeKeyboardInputMode: Raw;
    static kTISTypeKeyboardInputMethodWithoutModes: Raw;
    static kTISTypeKeyboardInputMethodModeEnabled: Raw;
    fn LMGetKbdType() -> u8;
    fn KBGetLayoutType(kind: i16) -> u32;
}
unsafe extern "C" {
    fn pthread_main_np() -> i32;
}
fn is_main_thread() -> bool {
    unsafe { pthread_main_np() != 0 }
}
struct Pool(*mut c_void);
impl Pool {
    fn new() -> Self {
        Self(unsafe { objc_autoreleasePoolPush() })
    }
}
impl Drop for Pool {
    fn drop(&mut self) {
        unsafe { objc_autoreleasePoolPop(self.0) }
    }
}
// All message signatures below are exact AppKit/Foundation Objective-C ABI forms
// on the supported 64-bit macOS target. Objects are kept inside a native pool or
// explicitly retained by the owning service; no borrowed Objective-C object escapes.
unsafe fn send0(object: ObjcRef, selector: &'static std::ffi::CStr) -> ObjcRef {
    let function: unsafe extern "C" fn(ObjcRef, ObjcRef) -> ObjcRef =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(object, sel_registerName(selector.as_ptr())) }
}
unsafe fn send1(object: ObjcRef, selector: &'static std::ffi::CStr, arg: ObjcRef) -> ObjcRef {
    let function: unsafe extern "C" fn(ObjcRef, ObjcRef, ObjcRef) -> ObjcRef =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(object, sel_registerName(selector.as_ptr()), arg) }
}
fn native_string(value: Raw) -> Option<String> {
    unsafe { AxValue::from_borrowed(value) }?.as_string()
}
fn frontmost_application() -> Option<(i32, String, String, String)> {
    let _pool = Pool::new();
    unsafe {
        let workspace = send0(objc_getClass(c"NSWorkspace".as_ptr()), c"sharedWorkspace");
        let application = send0(workspace, c"frontmostApplication");
        if application.is_null() {
            return None;
        }
        let pid_fn: unsafe extern "C" fn(ObjcRef, ObjcRef) -> i32 =
            std::mem::transmute(objc_msgSend as *const ());
        let pid = pid_fn(application, sel_registerName(c"processIdentifier".as_ptr()));
        if pid <= 0 {
            return None;
        }
        let url = send0(application, c"executableURL");
        let executable = if url.is_null() {
            String::new()
        } else {
            native_string(send0(url, c"path")).unwrap_or_default()
        };
        let app_id = native_string(send0(application, c"bundleIdentifier"))
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| executable.clone());
        let name =
            native_string(send0(application, c"localizedName")).unwrap_or_else(|| app_id.clone());
        Some((pid, app_id, executable, name))
    }
}
pub fn secure_event_input_enabled() -> Option<bool> {
    type Detect = unsafe extern "C" fn() -> u8;
    static DETECTOR: LazyLock<Option<Detect>> = LazyLock::new(|| {
        let address =
            unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"IsSecureEventInputEnabled".as_ptr()) };
        if address.is_null() {
            None
        } else {
            Some(unsafe { std::mem::transmute::<*mut c_void, Detect>(address) })
        }
    });
    DETECTOR.map(|function| unsafe { function() != 0 })
}
fn keyboard_state() -> (BTreeMap<u16, String>, InputSourceKind) {
    if !is_main_thread() {
        return (BTreeMap::new(), InputSourceKind::Unknown);
    }
    let _pool = Pool::new();
    let mut keys: BTreeMap<u16, String> = [
        (0, "A"),
        (1, "S"),
        (2, "D"),
        (3, "F"),
        (4, "H"),
        (5, "G"),
        (6, "Z"),
        (7, "X"),
        (8, "C"),
        (9, "V"),
        (11, "B"),
        (12, "Q"),
        (13, "W"),
        (14, "E"),
        (15, "R"),
        (16, "Y"),
        (17, "T"),
        (31, "O"),
        (32, "U"),
        (34, "I"),
        (35, "P"),
        (37, "L"),
        (38, "J"),
        (40, "K"),
        (45, "N"),
        (46, "M"),
        (18, "1"),
        (19, "2"),
        (20, "3"),
        (21, "4"),
        (23, "5"),
        (22, "6"),
        (26, "7"),
        (28, "8"),
        (25, "9"),
        (29, "0"),
        (48, "Tab"),
        (53, "Escape"),
        (49, "Space"),
        (36, "Return"),
        (122, "F1"),
        (120, "F2"),
        (99, "F3"),
        (118, "F4"),
        (96, "F5"),
        (97, "F6"),
        (98, "F7"),
        (100, "F8"),
        (101, "F9"),
        (109, "F10"),
        (103, "F11"),
        (111, "F12"),
    ]
    .into_iter()
    .map(|(key, value)| (key, value.into()))
    .collect();
    match unsafe { KBGetLayoutType(LMGetKbdType() as i16) } {
        0x49534f20 => {
            keys.insert(10, "AboveTab".into());
        }
        0x414e5349 => {
            keys.insert(50, "AboveTab".into());
        }
        _ => {}
    }
    for (&code, name) in keys.iter_mut() {
        if name.len() != 1 || !name.as_bytes()[0].is_ascii_alphabetic() {
            continue;
        }
        let Some(event) = (unsafe {
            AxValue::from_owned(CGEventCreateKeyboardEvent(std::ptr::null_mut(), code, true))
        }) else {
            continue;
        };
        unsafe { CGEventSetFlags(event.as_raw().cast_mut(), 0) };
        let native = unsafe {
            send1(
                objc_getClass(c"NSEvent".as_ptr()),
                c"eventWithCGEvent:",
                event.as_raw().cast_mut(),
            )
        };
        if native.is_null() {
            continue;
        }
        if let Some(character) =
            native_string(unsafe { send0(native, c"charactersIgnoringModifiers") })
        {
            if character.len() == 1 && character.as_bytes()[0].is_ascii_alphabetic() {
                *name = character.to_ascii_uppercase()
            }
        }
    }
    let kind = unsafe {
        let Some(source) = AxValue::from_owned(TISCopyCurrentKeyboardInputSource()) else {
            return (keys, InputSourceKind::Unknown);
        };
        let value = TISGetInputSourceProperty(source.as_raw(), kTISPropertyInputSourceType);
        if value.is_null() {
            InputSourceKind::Unknown
        } else if ax::CFEqual(value, kTISTypeKeyboardLayout) != 0 {
            InputSourceKind::Layout
        } else if [
            kTISTypeKeyboardInputMode,
            kTISTypeKeyboardInputMethodWithoutModes,
            kTISTypeKeyboardInputMethodModeEnabled,
        ]
        .into_iter()
        .any(|kind| !kind.is_null() && ax::CFEqual(value, kind) != 0)
        {
            InputSourceKind::Ime
        } else {
            InputSourceKind::Unknown
        }
    };
    (keys, kind)
}

/// Notification callbacks revoke evidence using metadata only. A matching PID
/// never promotes a notification's element to focused-field authority.
fn ax_invalidation(name: &str, element: u64, focused: u64) -> Option<bool> {
    match name {
        "AXFocusedUIElementChanged" | "AXFocusedWindowChanged" | "AXMainWindowChanged" => {
            Some(true)
        }
        "AXValueChanged" | "AXSelectedTextChanged" if focused != 0 && element == focused => {
            Some(false)
        }
        "AXWindowMoved"
        | "AXWindowResized"
        | "AXUIElementDestroyed"
        | "AXLayoutChanged"
        | "AXMoved"
        | "AXResized" => Some(false),
        _ => None,
    }
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRunLoopGetMain() -> ax::ffi::RunLoopRef;
    fn CFRunLoopWakeUp(runloop: ax::ffi::RunLoopRef);
}

fn wake_diagnostic_owner() {
    // SAFETY: CoreFoundation owns the process main run loop for its lifetime.
    // Waking it posts no input and does not invoke the authority invalidator.
    unsafe { CFRunLoopWakeUp(CFRunLoopGetMain()) };
}

struct NativeNotificationContext {
    note: Arc<dyn Fn(bool) + Send + Sync>,
    dirty: Arc<AtomicBool>,
    layout_dirty: Arc<AtomicBool>,
    diagnostic: Arc<AtomicBool>,
}
impl NativeNotificationContext {
    fn request_diagnostic(&self, wake_owner: impl FnOnce()) {
        self.diagnostic.store(true, Ordering::Release);
        wake_owner();
    }
    fn invalidate(&self, layout_changed: bool) {
        if layout_changed {
            self.layout_dirty.store(true, Ordering::Release);
        }
        self.dirty.store(true, Ordering::Release);
        (self.note)(true);
    }
}

struct AppNotifications {
    observer: ObjcRef,
    centers: Vec<ObjcRef>,
    context: Box<NativeNotificationContext>,
}
impl AppNotifications {
    fn new(
        note: Arc<dyn Fn(bool) + Send + Sync>,
        dirty: Arc<AtomicBool>,
        layout_dirty: Arc<AtomicBool>,
        diagnostic: Arc<AtomicBool>,
    ) -> Result<Self> {
        let _pool = Pool::new();
        unsafe extern "C" fn changed(object: ObjcRef, selector: ObjcRef, _notification: ObjcRef) {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut raw = std::ptr::null_mut();
                unsafe { object_getInstanceVariable(object, c"_context".as_ptr(), &mut raw) };
                if raw.is_null() {
                    return;
                }
                let context = unsafe { &*(raw as *const NativeNotificationContext) };
                if selector == unsafe { sel_registerName(c"diagnosticRequested:".as_ptr()) } {
                    context.request_diagnostic(wake_diagnostic_owner);
                    return;
                }
                context.invalidate(
                    selector == unsafe { sel_registerName(c"layoutChanged:".as_ptr()) },
                );
            }));
        }
        static OBSERVER_CLASS: LazyLock<usize> = LazyLock::new(|| unsafe {
            let class = objc_allocateClassPair(
                objc_getClass(c"NSObject".as_ptr()),
                c"BroTypistNativeNotificationOwner".as_ptr(),
                0,
            );
            if class.is_null() {
                return 0;
            }
            if !class_addIvar(
                class,
                c"_context".as_ptr(),
                std::mem::size_of::<ObjcRef>(),
                std::mem::align_of::<ObjcRef>().trailing_zeros() as u8,
                c"^v".as_ptr(),
            ) {
                return 0;
            }
            let implementation: unsafe extern "C" fn() =
                std::mem::transmute(changed as unsafe extern "C" fn(ObjcRef, ObjcRef, ObjcRef));
            for selector in [
                c"nativeChanged:",
                c"layoutChanged:",
                c"diagnosticRequested:",
            ] {
                if !class_addMethod(
                    class,
                    sel_registerName(selector.as_ptr()),
                    implementation,
                    c"v@:@".as_ptr(),
                ) {
                    return 0;
                }
            }
            objc_registerClassPair(class);
            class as usize
        });
        let class = *OBSERVER_CLASS as ObjcRef;
        ensure!(!class.is_null(), "Native notification observer unavailable");
        let observer = unsafe { send0(send0(class, c"alloc"), c"init") };
        ensure!(
            !observer.is_null(),
            "Native notification observer allocation failed"
        );
        let mut context = Box::new(NativeNotificationContext {
            note,
            dirty,
            layout_dirty,
            diagnostic,
        });
        unsafe {
            object_setInstanceVariable(
                observer,
                c"_context".as_ptr(),
                (&mut *context as *mut NativeNotificationContext).cast(),
            )
        };
        let workspace =
            unsafe { send0(objc_getClass(c"NSWorkspace".as_ptr()), c"sharedWorkspace") };
        let workspace_center = unsafe { send0(workspace, c"notificationCenter") };
        let distributed = unsafe {
            send0(
                objc_getClass(c"NSDistributedNotificationCenter".as_ptr()),
                c"defaultCenter",
            )
        };
        let centers = vec![workspace_center, distributed];
        let register: unsafe extern "C" fn(ObjcRef, ObjcRef, ObjcRef, ObjcRef, ObjcRef, ObjcRef) =
            unsafe { std::mem::transmute(objc_msgSend as *const ()) };
        for (center, name, selector) in [
            (
                workspace_center,
                "NSWorkspaceDidActivateApplicationNotification",
                c"nativeChanged:",
            ),
            (
                distributed,
                "com.apple.Carbon.TISNotifySelectedKeyboardInputSourceChanged",
                c"layoutChanged:",
            ),
            (
                distributed,
                WHATSAPP_DIAGNOSTIC_NOTIFICATION,
                c"diagnosticRequested:",
            ),
        ] {
            let native = AxValue::string(name)
                .ok_or_else(|| anyhow!("Native notification name unavailable"))?;
            unsafe {
                register(
                    center,
                    sel_registerName(c"addObserver:selector:name:object:".as_ptr()),
                    observer,
                    sel_registerName(selector.as_ptr()),
                    native.as_raw().cast_mut(),
                    std::ptr::null_mut(),
                )
            }
        }
        Ok(Self {
            observer,
            centers,
            context,
        })
    }
}
impl Drop for AppNotifications {
    fn drop(&mut self) {
        for &center in &self.centers {
            unsafe { send1(center, c"removeObserver:", self.observer) };
        }
        unsafe {
            object_setInstanceVariable(self.observer, c"_context".as_ptr(), std::ptr::null_mut());
            objc_release(self.observer)
        }
        let _ = &self.context;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_notifications_revoke_layout_destroyed_and_exact_field_evidence() {
        assert_eq!(
            ax_invalidation("AXFocusedUIElementChanged", 9, 7),
            Some(true)
        );
        for name in [
            "AXWindowMoved",
            "AXWindowResized",
            "AXUIElementDestroyed",
            "AXLayoutChanged",
            "AXMoved",
            "AXResized",
        ] {
            assert_eq!(ax_invalidation(name, 9, 7), Some(false));
        }
        assert_eq!(ax_invalidation("AXValueChanged", 7, 7), Some(false));
        assert_eq!(ax_invalidation("AXSelectedTextChanged", 7, 7), Some(false));
        assert_eq!(ax_invalidation("AXValueChanged", 9, 7), None);
        assert_eq!(ax_invalidation("AXValueChanged", 0, 0), None);
        assert_eq!(ax_invalidation("AXUnknown", 7, 7), None);
    }
    struct CompatibilityHost {
        writable: bool,
        state: Option<bool>,
        error: Option<i32>,
        reads: std::cell::Cell<usize>,
        writes: std::cell::Cell<usize>,
    }
    impl AccessibilityPreparationHost for CompatibilityHost {
        fn settable(&self, _: &str) -> bool {
            self.writable
        }
        fn boolean(&self, _: &str) -> Option<bool> {
            self.reads.set(self.reads.get() + 1);
            self.state
        }
        fn enable(&self, _: &str) -> std::result::Result<(), ax::AxError> {
            self.writes.set(self.writes.get() + 1);
            self.error.map_or(Ok(()), |code| Err(ax::AxError(code)))
        }
    }
    #[test]
    fn accessibility_preparation_uses_host_capabilities_not_bundle_identity() {
        // A newly encountered native/Electron host needs no bundle allowlist.
        for name in ["AXManualAccessibility", "AXEnhancedUserInterface"] {
            for (writable, state, error, expected, reads, writes) in [
                (true, Some(false), None, "set", 1, 1),
                (true, Some(true), None, "already_true", 1, 0),
                (true, None, None, "unreadable_not_set", 1, 0),
                (false, Some(false), None, "not_settable", 0, 0),
                (false, None, None, "not_settable", 0, 0),
                (true, Some(false), Some(-25204), "set_error:-25204", 1, 1),
            ] {
                let host = CompatibilityHost {
                    writable,
                    state,
                    error,
                    reads: std::cell::Cell::new(0),
                    writes: std::cell::Cell::new(0),
                };
                assert_eq!(enable_accessibility_attribute(&host, name), expected);
                assert_eq!(host.reads.get(), reads);
                assert_eq!(host.writes.get(), writes);
            }
        }
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            app_id: "editor".into(),
            field_id: "field".into(),
            text: "hello 😀 world".into(),
            caret: 13,
            window_id: "1234:window".into(),
            window_pid: 1234,
            ..Default::default()
        }
    }
    #[test]
    fn native_window_without_document_has_stable_context_authority() {
        let salt = [7; 32];
        let identity = |app, pid, window, document, started| {
            session_document_identity(&salt, false, app, pid, window, document, started)
        };
        let (domain, revision) = identity("editor", 1234, 9, None, Some((100, 1))).unwrap();
        assert!(domain.is_empty());
        assert!(!revision.is_empty());
        assert_eq!(
            Some((domain.clone(), revision.clone())),
            identity("editor", 1234, 9, None, Some((100, 1)))
        );
        for other in [
            identity("editor", 1234, 10, None, Some((100, 1))),
            identity("editor", 1235, 9, None, Some((100, 1))),
            identity("editor", 1234, 9, None, Some((101, 1))),
            identity("other-editor", 1234, 9, None, Some((100, 1))),
            identity("editor", 1234, 9, Some("file:///other.txt"), Some((100, 1))),
        ] {
            assert_ne!(other.unwrap().1, revision);
        }
        assert!(identity("editor", 1234, 9, None, None).is_none());
        let mut before = snapshot();
        before.window_revision = revision;
        let binding = snapshot_binding(&salt, &before);
        let mut moved = before.clone();
        moved.caret_rect = Some([20.0, 30.0, 1.0, 12.0]);
        moved.window_bounds = Some([0.0, 0.0, 800.0, 600.0]);
        assert!(before.same_authority(&moved));
        assert_eq!(binding, snapshot_binding(&salt, &moved));
        // Context's exact draft binding remains independent of stable authority.
        moved.text = "changed draft".into();
        assert_ne!(binding, snapshot_binding(&salt, &moved));
    }

    #[test]
    fn browser_documents_keep_exact_url_binding_and_refuse_unknown_documents() {
        let salt = [7; 32];
        let binding = |document| {
            session_document_identity(
                &salt,
                true,
                "com.apple.Safari",
                1234,
                9,
                document,
                Some((100, 1)),
            )
        };
        let first = binding(Some("https://example.com/first")).unwrap();
        let second = binding(Some("https://example.com/second")).unwrap();
        assert_eq!(first.0, second.0);
        assert_ne!(first.1, second.1);
        assert!(binding(None).is_none());
        assert!(binding(Some("file:///local.html")).is_none());
        let mut before = snapshot();
        before.domain = first.0;
        before.window_revision = first.1;
        let mut other = before.clone();
        other.window_revision = second.1;
        assert!(!before.same_authority(&other));
        assert_ne!(
            snapshot_binding(&salt, &before),
            snapshot_binding(&salt, &other)
        );
        other = before.clone();
        other.window_id = "1234:other-window".into();
        assert!(!before.same_authority(&other));
        assert_ne!(
            snapshot_binding(&salt, &before),
            snapshot_binding(&salt, &other)
        );
    }

    #[test]
    fn exact_chat_capability_is_not_a_name_heuristic() {
        for app in CHAT_BUNDLES {
            assert!(exact_member(app, CHAT_BUNDLES))
        }
        assert!(!exact_member("org.example.discord-notes", CHAT_BUNDLES));
        assert!(!exact_member("org.example.slack-editor", CHAT_BUNDLES));
        assert!(!stock_chat_description("Chat Input"));
        assert!(stock_chat_description(CHAT_NAMES[0]));
        assert!(!stock_chat_description(
            "Chat Input (Agent), edit files in your workspace.evil"
        ));
    }
    #[test]
    fn scalar_replacement_maps_astral_without_rewriting_suffix() {
        let before = snapshot();
        assert_eq!(
            scalar_native_range(&before, 6, 1),
            Some(AxRange {
                location: 6,
                length: 2
            })
        );
        assert_eq!(
            expected_mutation(&before, 6, 1, "sun"),
            Some(("hello sun world".into(), 9))
        );
        assert_eq!(expected_mutation(&before, usize::MAX, 1, "x"), None)
    }
    #[test]
    fn insertion_readback_reasons_preserve_outcomes_and_expose_only_match_booleans() {
        let before = snapshot();
        let expected = expected_mutation(&before, before.caret, 0, "!").unwrap();
        let (outcome, reason, unchanged) = classify_readback(&before, &before, &expected, true);
        assert!(matches!(outcome, EditOutcome::Dispatched));
        assert_eq!(reason, InsertionReason::Unchanged);
        assert!(unchanged.authority_match && unchanged.unchanged_readback);
        assert!(!unchanged.text_match && !unchanged.caret_match);

        let mut exact = before.clone();
        exact.text = expected.0.clone().into();
        exact.caret = expected.1;
        exact.selection_length = 0;
        let (outcome, reason, matches) = classify_readback(&before, &exact, &expected, true);
        assert!(matches!(outcome, EditOutcome::Applied(_)));
        assert_eq!(reason, InsertionReason::Exact);
        assert!(matches.text_match && matches.caret_match && matches.selection_empty);
        assert!(!matches.unchanged_readback);

        for component in 0..6 {
            let mut changed = exact.clone();
            match component {
                0 => changed.app_id.push_str("-changed"),
                1 => changed.window_pid += 1,
                2 => changed.window_id.push_str("-changed"),
                3 => changed.field_id.push_str("-changed"),
                4 => changed.window_revision.push_str("-changed"),
                5 => changed.domain.push_str("-changed"),
                _ => unreachable!(),
            }
            let (outcome, reason, matches) = classify_readback(&before, &changed, &expected, true);
            assert!(matches!(outcome, EditOutcome::Indeterminate));
            assert_eq!(reason, InsertionReason::AuthorityChange);
            assert!(!matches.authority_match);
            assert!(matches.text_match && matches.caret_match && matches.selection_empty);
            for (index, same) in [
                matches.same_app,
                matches.same_process,
                matches.same_window,
                matches.same_field,
                matches.same_document,
                matches.same_domain,
            ]
            .into_iter()
            .enumerate()
            {
                assert_eq!(same, index != component);
            }
        }
        for component in 0..3 {
            let mut changed = exact.clone();
            match component {
                0 => changed.text = "different synthetic text".into(),
                1 => changed.caret = before.caret,
                2 => changed.selection_length = 1,
                _ => unreachable!(),
            }
            let (outcome, reason, matches) = classify_readback(&before, &changed, &expected, true);
            assert!(matches!(outcome, EditOutcome::Indeterminate));
            assert_eq!(reason, InsertionReason::TextOrCaretOrSelectionMismatch);
            assert!(matches.authority_match);
            assert_eq!(matches.text_match, component != 0);
            assert_eq!(matches.caret_match, component != 1);
            assert_eq!(matches.selection_empty, component != 2);
        }
    }
    #[test]
    fn noop_setter_is_not_applied_and_never_retried() {
        let before = snapshot();
        let expected = expected_mutation(&before, 13, 0, "!").unwrap();
        assert!(matches!(
            classify_readback(&before, &before, &expected, false).0,
            EditOutcome::Rejected
        ));
        assert!(matches!(
            classify_readback(&before, &before, &expected, true).0,
            EditOutcome::Dispatched
        ));
        let mut after = before.clone();
        after.text = expected.0.clone().into();
        after.caret = expected.1;
        assert!(matches!(
            classify_readback(&before, &after, &expected, false).0,
            EditOutcome::Applied(_)
        ));
        after.window_id = "other".into();
        assert!(matches!(
            classify_readback(&before, &after, &expected, false).0,
            EditOutcome::Indeterminate
        ));
    }
    #[test]
    fn changed_selection_or_host_rejection_is_not_confirmation() {
        let before = snapshot();
        let expected = expected_mutation(&before, 6, 1, "sun").unwrap();
        let mut after = before.clone();
        after.caret = 6;
        after.selection_length = 1;
        assert!(matches!(
            classify_readback(&before, &after, &expected, false).0,
            EditOutcome::Indeterminate
        ));
        assert!(mutation_may_have_applied(ax::AxError(-25204)));
        assert!(!mutation_may_have_applied(ax::AxError(-25202)));
    }
    #[test]
    fn dispatched_emoji_selection_waits_for_exact_native_readback() {
        let mut before = snapshot();
        before.text = "Olá 😀 :sorriso tail".into();
        before.caret = 14;
        before.can_replace = false;
        before.can_replace_emoji = true;
        let expected = expected_mutation(&before, 6, 8, "😄").unwrap();
        let mut selected = before.clone();
        selected.caret = 6;
        selected.selection_length = 8;
        assert!(matches!(
            classify_emoji_readback(&before, &selected, &expected, true, 6, 8),
            EditOutcome::Dispatched
        ));
        assert!(matches!(
            classify_emoji_readback(&before, &selected, &expected, false, 6, 8),
            EditOutcome::Indeterminate
        ));
        assert!(matches!(
            classify_readback(&before, &selected, &expected, true).0,
            EditOutcome::Indeterminate
        ));
        let mut after = selected.clone();
        after.text = "Olá 😀 😄 tail".into();
        after.caret = 7;
        after.selection_length = 0;
        assert!(matches!(
            classify_emoji_readback(&before, &after, &expected, true, 6, 8),
            EditOutcome::Applied(_)
        ));
        for changed in 0..5 {
            let mut unrelated = selected.clone();
            match changed {
                0 => unrelated.caret = 5,
                1 => unrelated.selection_length = 7,
                2 => unrelated.text = "Olá 😀 :sorriso edit".into(),
                3 => unrelated.window_id = "other".into(),
                _ => unrelated.can_replace_emoji = false,
            }
            assert!(matches!(
                classify_emoji_readback(&before, &unrelated, &expected, true, 6, 8),
                EditOutcome::Indeterminate
            ));
        }
    }
    #[test]
    fn protected_append_only_and_invalid_ranges_refuse_before_native_calls() {
        let mut before = snapshot();
        before.protected = true;
        assert!(!valid_insert(&before, "!"));
        assert!(!valid_replacement(&before, 0, 1, "x"));
        before.protected = false;
        before.can_replace = false;
        assert!(valid_insert(&before, "!"));
        assert!(!valid_replacement(&before, 0, 1, "x"));
        before.can_replace = true;
        assert!(!valid_replacement(&before, 12, 2, "x"));
        assert!(!valid_insert(&before, "\0"));
    }
    #[derive(Clone, Copy)]
    enum HostMode {
        Apply,
        Ignore,
        Reject,
        ReadError,
        ReadUnavailable,
        ChangeOwner,
    }
    struct MutationHost {
        observed: Snapshot,
        selection: AxRange,
        mode: HostMode,
        writes: usize,
    }
    impl SelectedTextHost for MutationHost {
        fn write_selected(&mut self, text: &str) -> std::result::Result<(), ax::AxError> {
            self.writes += 1;
            if matches!(self.mode, HostMode::Reject) {
                return Err(ax::AxError(-25202));
            }
            if matches!(self.mode, HostMode::Ignore) {
                return Ok(());
            }
            // This host interprets native UTF-16 selection independently of
            // the Rust proposal/expected-output path, just as an AX provider does.
            let (start, length) = self
                .observed
                .text
                .index()
                .utf16_range_to_scalar(self.selection.location, self.selection.length)
                .unwrap();
            let bytes = self
                .observed
                .text
                .index()
                .scalar_range_to_utf8(start, length)
                .unwrap();
            let mut actual = self.observed.text.to_string();
            actual.replace_range(bytes, text);
            self.observed.text = actual.into();
            self.observed.caret = start + text.chars().count();
            self.observed.selection_length = 0;
            if matches!(self.mode, HostMode::ChangeOwner) {
                self.observed.window_id = "different-window".into()
            }
            Ok(())
        }
        fn observe(&mut self) -> Result<Option<Snapshot>> {
            if matches!(self.mode, HostMode::ReadError) {
                bail!("synthetic native transport failure")
            }
            if matches!(self.mode, HostMode::ReadUnavailable) {
                return Ok(None);
            }
            Ok(Some(self.observed.clone()))
        }
    }
    struct UnicodeHost {
        after: Option<Snapshot>,
        stage: &'static str,
        post_error: bool,
        capture_error: bool,
        scope: Option<bool>,
        scope_timing: Option<(Instant, Instant)>,
        posts: usize,
        observations: usize,
        scope_reads: usize,
    }
    impl UnicodeHost {
        fn transient() -> Self {
            Self {
                after: None,
                stage: "selection_or_geometry_changed_after_capture",
                post_error: false,
                capture_error: false,
                scope: Some(true),
                scope_timing: None,
                posts: 0,
                observations: 0,
                scope_reads: 0,
            }
        }
    }
    impl UnicodeTextHost for UnicodeHost {
        fn post(&mut self, _: &str) -> Result<()> {
            self.posts += 1;
            ensure!(!self.post_error, "synthetic post failure");
            Ok(())
        }
        fn observe(&mut self) -> Result<Option<Snapshot>> {
            self.observations += 1;
            ensure!(!self.capture_error, "synthetic capture failure");
            Ok(self.after.clone())
        }
        fn capture_stage(&self) -> &'static str {
            self.stage
        }
        fn pending_authority_matches(&mut self, _: &Snapshot) -> Result<bool> {
            self.scope_reads += 1;
            let proof = self
                .scope
                .ok_or_else(|| anyhow!("synthetic scope read failure"));
            match self.scope_timing {
                Some((expires_at, completed_at)) => {
                    finish_unicode_scope_proof(proof, expires_at, || completed_at)
                }
                None => proof,
            }
        }
    }
    #[test]
    fn unicode_post_scope_expiry_cannot_restart_the_original_readback_window() {
        let posted_at = Instant::now();
        let expires_at = posted_at + Duration::from_secs(2);
        // A previous successful proof does not renew the original deadline.
        assert!(
            finish_unicode_scope_proof(
                Ok(true),
                expires_at,
                || posted_at + Duration::from_secs(1),
            )
            .unwrap()
        );
        let before = snapshot();
        for (completed_at, dispatched) in [
            (expires_at - Duration::from_nanos(1), true),
            (expires_at, false),
            (expires_at + Duration::from_nanos(1), false),
            (expires_at + Duration::from_secs(2), false),
        ] {
            let mut host = UnicodeHost::transient();
            host.scope_timing = Some((expires_at, completed_at));
            let (outcome, reason, readback) = apply_unicode_text(&mut host, &before, "!");
            assert_eq!(matches!(outcome, EditOutcome::Dispatched), dispatched);
            if !dispatched {
                assert!(matches!(outcome, EditOutcome::Indeterminate));
            }
            assert_eq!(reason, InsertionReason::CaptureUnavailable);
            assert!(readback.is_none());
            assert_eq!((host.posts, host.observations, host.scope_reads), (1, 1, 1));
        }
    }
    #[test]
    fn unicode_post_transient_capture_requires_readback_without_reposting() {
        let before = snapshot();
        let mut host = UnicodeHost::transient();
        let (outcome, reason, readback) = apply_unicode_text(&mut host, &before, "!");
        assert!(matches!(outcome, EditOutcome::Dispatched));
        assert_eq!(reason, InsertionReason::CaptureUnavailable);
        assert!(readback.is_none()); // No fabricated applied Snapshot.
        assert_eq!((host.posts, host.observations, host.scope_reads), (1, 1, 1));
    }
    #[test]
    fn unicode_post_transient_capture_refuses_revoked_or_unreadable_scope() {
        let before = snapshot();
        // Fresh-scope refusal/error is the native boundary for target, policy,
        // permission, authored-revision, protected-field and IME revocation.
        // This tests classification, not an imitation of those native reads.
        for scope in [Some(false), None] {
            let mut host = UnicodeHost::transient();
            host.scope = scope;
            let (outcome, reason, readback) = apply_unicode_text(&mut host, &before, "!");
            assert!(matches!(outcome, EditOutcome::Indeterminate));
            assert_eq!(reason, InsertionReason::CaptureUnavailable);
            assert!(readback.is_none());
            assert_eq!((host.posts, host.observations, host.scope_reads), (1, 1, 1));
        }
    }
    #[test]
    fn unicode_post_transient_capture_cannot_rehabilitate_invalid_original_input() {
        for invalid in 0..3 {
            let mut before = snapshot();
            match invalid {
                0 => before.protected = true,
                1 => before.selection_length = 1,
                2 => before.caret = before.text.scalar_len() + 1,
                _ => unreachable!(),
            }
            let mut host = UnicodeHost::transient();
            let (outcome, _, readback) = apply_unicode_text(&mut host, &before, "!");
            assert!(matches!(outcome, EditOutcome::Indeterminate));
            assert!(readback.is_none());
            assert_eq!((host.posts, host.observations, host.scope_reads), (1, 1, 0));
        }
    }
    #[test]
    fn unicode_post_never_converts_errors_or_unproven_capture_absence_to_dispatch() {
        let before = snapshot();
        for stage in [
            // AX string extraction collapses errors/malformed values to None.
            "field_value_unavailable",
            "geometry_read_scope_revoked",
            "selection_utf16_invalid",
            "marked_text_or_input_source_unverified",
            "focus_changed_after_capture",
            "field_not_ordinary_writable",
            "not_observed",
        ] {
            let mut host = UnicodeHost::transient();
            host.stage = stage;
            let (outcome, _, readback) = apply_unicode_text(&mut host, &before, "!");
            assert!(matches!(outcome, EditOutcome::Indeterminate));
            assert!(readback.is_none());
            assert_eq!((host.posts, host.observations, host.scope_reads), (1, 1, 0));
        }
        for post_error in [false, true] {
            let mut host = UnicodeHost::transient();
            host.post_error = post_error;
            host.capture_error = !post_error;
            let (outcome, reason, readback) = apply_unicode_text(&mut host, &before, "!");
            assert!(matches!(outcome, EditOutcome::Indeterminate));
            assert_eq!(
                reason,
                if post_error {
                    InsertionReason::PostError
                } else {
                    InsertionReason::CaptureError
                }
            );
            assert!(readback.is_none());
            assert_eq!(host.posts, 1);
            assert_eq!(host.observations, usize::from(!post_error));
            assert_eq!(host.scope_reads, 0);
        }
    }
    #[test]
    fn unicode_post_full_readback_keeps_exact_and_negative_classifications() {
        let before = snapshot();
        let expected = expected_mutation(&before, before.caret, 0, "!").unwrap();
        for change in 0..6 {
            let mut after = before.clone();
            after.text = expected.0.clone().into();
            after.caret = expected.1;
            match change {
                0 => {}
                1 => after = before.clone(),
                2 => after.field_id.push_str("-other"),
                3 => after.text = "different synthetic text".into(),
                4 => after.caret = before.caret,
                5 => after.selection_length = 1,
                _ => unreachable!(),
            }
            let mut host = UnicodeHost::transient();
            host.after = Some(after);
            let (outcome, _, readback) = apply_unicode_text(&mut host, &before, "!");
            match change {
                0 => assert!(matches!(outcome, EditOutcome::Applied(_))),
                1 => assert!(matches!(outcome, EditOutcome::Dispatched)),
                _ => assert!(matches!(outcome, EditOutcome::Indeterminate)),
            }
            assert!(readback.is_some());
            assert_eq!((host.posts, host.observations, host.scope_reads), (1, 1, 0));
        }
    }
    #[test]
    fn native_setter_readback_cannot_echo_requested_success() {
        let before = snapshot();
        for (mode, applied, rejected, expected_reason) in [
            (HostMode::Apply, true, false, InsertionReason::Exact),
            (HostMode::Ignore, false, true, InsertionReason::Unchanged),
            (HostMode::Reject, false, true, InsertionReason::SetterError),
            (
                HostMode::ReadError,
                false,
                false,
                InsertionReason::CaptureError,
            ),
            (
                HostMode::ReadUnavailable,
                false,
                false,
                InsertionReason::CaptureUnavailable,
            ),
            (
                HostMode::ChangeOwner,
                false,
                false,
                InsertionReason::AuthorityChange,
            ),
        ] {
            let mut host = MutationHost {
                observed: before.clone(),
                selection: AxRange {
                    location: 14,
                    length: 0,
                },
                mode,
                writes: 0,
            };
            let (result, reason, readback) =
                apply_selected_text(&mut host, &before, 13, 0, " 😀e\u{301}");
            assert_eq!(reason, expected_reason);
            assert_eq!(
                readback.is_some(),
                matches!(
                    mode,
                    HostMode::Apply | HostMode::Ignore | HostMode::ChangeOwner
                )
            );
            let diagnostic = InsertionDiagnostic {
                reason,
                capture_stage: "edit_readback_started",
                readback,
                user_revision_unchanged: Some(true),
            };
            let serialized = serde_json::to_string(&diagnostic).unwrap();
            assert!(!serialized.contains(before.text.as_ref()));
            assert!(!serialized.contains("synthetic native transport failure"));
            assert!(!serialized.contains("different-window"));
            if let Some(readback) = readback {
                assert!(
                    serde_json::to_value(readback)
                        .unwrap()
                        .as_object()
                        .unwrap()
                        .values()
                        .all(serde_json::Value::is_boolean)
                );
            }
            assert_eq!(matches!(result, EditOutcome::Applied(_)), applied);
            assert_eq!(matches!(result, EditOutcome::Rejected), rejected);
            if !applied && !rejected {
                assert!(matches!(result, EditOutcome::Indeterminate))
            }
            assert_eq!(host.writes, 1); // No AX/Unicode retry, even on no-op/read failure.
        }
    }
    #[test]
    fn native_selection_replacement_preserves_astral_and_combining_boundaries() {
        let before = snapshot();
        let mut host = MutationHost {
            observed: before.clone(),
            selection: AxRange {
                location: 6,
                length: 2,
            },
            mode: HostMode::Apply,
            writes: 0,
        };
        let (outcome, _, _) = apply_selected_text(&mut host, &before, 6, 1, "e\u{301}");
        let EditOutcome::Applied(after) = outcome else {
            panic!("native host readback not confirmed")
        };
        assert_eq!(after.text.as_ref(), "hello e\u{301} world");
        assert_eq!(after.caret, 8);
        assert_eq!(host.writes, 1);
    }
    #[test]
    fn native_source_consent_resolves_defaults_global_app_domain_and_independence() {
        let mut snapshot = Snapshot {
            app_id: "com.apple.Terminal".into(),
            ..Default::default()
        };
        for (settings, domain, expected) in [
            (serde_json::json!({}), "", false),
            (serde_json::json!({"screenshot_appearance": true}), "", true),
            (
                serde_json::json!({"screenshot_appearance": false,
                "app_rules": {"app:com.apple.Terminal": {"screenshot_appearance": true}}}),
                "",
                false,
            ),
            (
                serde_json::json!({"screenshot_appearance": true,
                "app_rules": {"app:com.apple.Terminal": {"screenshot_appearance": false}}}),
                "",
                false,
            ),
            (
                serde_json::json!({"screenshot_appearance": true,
                "app_rules": {"domain:example.test": {"screenshot_appearance": false}}}),
                "example.test",
                false,
            ),
            (
                serde_json::json!({"screenshot_appearance": false,
                "app_rules": {"domain:example.test": {"screenshot_appearance": true}}}),
                "example.test",
                false,
            ),
            (
                serde_json::json!({"screenshot_appearance": true,
                "app_rules": {
                    "app:com.apple.Terminal": {"screenshot_appearance": false},
                    "domain:example.test": {"screenshot_appearance": true}
                }}),
                "example.test",
                true,
            ),
            (
                serde_json::json!({"screenshot_appearance": false,
                "screen_context": true, "screenshot_context": true,
                "clipboard_context": true, "use_history": true}),
                "",
                false,
            ),
            (
                serde_json::json!({"screenshot_appearance": true,
                "screen_context": false, "screenshot_context": false,
                "clipboard_context": false, "use_history": false}),
                "",
                true,
            ),
        ] {
            let mut config: Config = serde_json::from_value(settings).unwrap();
            config.terminal_completion = true;
            let policy = ObservationPolicy::new(&effective_config(&config)).unwrap();
            snapshot.domain = domain.into();
            let front = Foreground {
                app_id: snapshot.app_id.clone(),
                domain: domain.into(),
                ..Default::default()
            };
            assert_eq!(
                source_preference_allowed(
                    &policy,
                    &front,
                    "Terminal",
                    &snapshot,
                    ScopedPreference::ScreenshotAppearance
                ),
                expected
            );
            let mut wrong_target = snapshot.clone();
            wrong_target.app_id = "other.app".into();
            assert!(!source_preference_allowed(
                &policy,
                &front,
                "Terminal",
                &wrong_target,
                ScopedPreference::ScreenshotAppearance
            ));
            wrong_target = snapshot.clone();
            wrong_target.domain = "other.test".into();
            assert!(!source_preference_allowed(
                &policy,
                &front,
                "Terminal",
                &wrong_target,
                ScopedPreference::ScreenshotAppearance
            ));
        }
    }
    #[test]
    fn terminal_opt_in_never_overrides_customized_exclusions() {
        let mut config = Config::default();
        let factory = config.excluded_apps.clone();
        assert_eq!(effective_config(&config).excluded_apps, factory);
        config.terminal_completion = true;
        let effective = effective_config(&config);
        assert!(
            !effective
                .excluded_apps
                .iter()
                .any(|item| matches!(item.as_str(), "terminal" | "iterm"))
        );
        assert_eq!(config.excluded_apps, factory);
        config.excluded_apps.push("private-editor".into());
        assert_eq!(
            effective_config(&config).excluded_apps,
            config.excluded_apps
        );
    }
    fn revoked_ticket(snapshot: &Snapshot) -> MacContextTicket {
        MacContextTicket {
            lease: NativeReadLease {
                foreground: Foreground {
                    app_id: snapshot.app_id.clone(),
                    window_pid: snapshot.window_pid,
                    window_id: snapshot.window_id.clone(),
                    ..Default::default()
                },
                name: "Synthetic".into(),
                secure_description: "secure".into(),
                source_kind: InputSourceKind::Layout,
                salt: [7; 32],
                policy: Arc::new(ObservationPolicy::new(&Config::default()).unwrap()),
                epoch: Arc::new(AtomicU64::new(2)),
                issued_epoch: 1,
                revision: Arc::new(|| 0),
                issued_revision: 0,
                dirty: Arc::new(AtomicBool::new(false)),
                snapshot_binding: snapshot_binding(&[7; 32], snapshot),
            },
            document_id: String::new(),
            field_id: snapshot.field_id.clone(),
            field_hash: 1,
            window_hash: 2,
        }
    }
    #[test]
    fn detached_primary_freshness_is_source_independent_and_revoked_before_native_reads() {
        let snapshot = Snapshot {
            app_id: "com.apple.Terminal".into(),
            field_id: "terminal:unsent-input".into(),
            ..Default::default()
        };
        for settings in [
            serde_json::json!({"screen_context": false}),
            serde_json::json!({"screen_context": true,
                "app_rules": {"app:com.apple.Terminal": {"screen_context": false}}}),
            serde_json::json!({"screen_context": true,
                "app_rules": {"domain:example.test": {"screen_context": false}}}),
        ] {
            let mut config: Config = serde_json::from_value(settings).unwrap();
            config.terminal_completion = true;
            config.clipboard_context = true;
            config.use_history = true;
            let mut lease = revoked_ticket(&snapshot).lease;
            lease.foreground.domain = "example.test".into();
            lease.policy = Arc::new(ObservationPolicy::new(&effective_config(&config)).unwrap());
            lease.epoch.store(lease.issued_epoch, Ordering::Release);
            assert!(lease.metadata_valid());
            assert!(
                !lease
                    .policy
                    .preference(
                        ScopedPreference::ScreenContext,
                        &snapshot.app_id,
                        "example.test",
                        &["Terminal"]
                    )
                    .value
            );
            for source in [
                ScopedPreference::ClipboardContext,
                ScopedPreference::UseHistory,
            ] {
                assert!(
                    lease
                        .policy
                        .preference(source, &snapshot.app_id, "example.test", &["Terminal"])
                        .value
                );
            }
            lease.dirty.store(true, Ordering::Release);
            assert!(!lease.valid());
            lease.dirty.store(false, Ordering::Release);
            lease.issued_revision = 1;
            assert!(!lease.valid());
            lease.issued_revision = 0;
            lease.epoch.fetch_add(1, Ordering::AcqRel);
            assert!(!lease.valid());
            lease.epoch.store(lease.issued_epoch, Ordering::Release);
            config.global_enabled = false;
            lease.policy = Arc::new(ObservationPolicy::new(&effective_config(&config)).unwrap());
            assert!(!lease.valid());
        }
    }
    #[test]
    fn detached_tickets_bind_exact_draft_authority_without_retaining_it() {
        fn send_only<T: Send>() {}
        send_only::<MacContextTicket>();
        send_only::<NativeReadLease>();
        let mut before = snapshot();
        before.text = "a😀e\u{301}".into();
        before.caret = 4;
        let ticket = revoked_ticket(&before);
        assert!(ticket.matches_snapshot(&before));
        let mut changed = before.clone();
        changed.text = "a😀é".into();
        assert!(!ticket.matches_snapshot(&changed));
        changed = before.clone();
        changed.window_revision = "same-site-new-document".into();
        assert!(!ticket.matches_snapshot(&changed));
        changed = before.clone();
        changed.caret_rect = Some([200., 300., 1., 20.]);
        assert!(ticket.matches_snapshot(&changed));
        assert!(!ticket.matches().unwrap()); // Revoked epoch short-circuits before any native read.
        assert_eq!(ticket.read(&|| true, 2400).unwrap().status, "blocked");
        assert_eq!(ticket.read(&|| false, 2400).unwrap().status, "blocked");
    }
    #[test]
    fn document_ids_are_salted_and_navigation_specific() {
        assert_ne!(
            document_digest(&[1; 32], "https://example.com/a"),
            document_digest(&[1; 32], "https://example.com/b")
        );
        assert_ne!(
            document_digest(&[1; 32], "https://example.com/a"),
            document_digest(&[2; 32], "https://example.com/a")
        );
        assert_eq!(document_digest(&[1; 32], "https://example.com/a").len(), 32);
    }
}
