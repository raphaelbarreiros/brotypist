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
    policy::{ObservationPolicy, is_browser, is_chromium_bundle, native_domain},
};
use crate::{
    config::Config,
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
    pub fn valid(&self) -> bool {
        self.epoch.load(Ordering::Acquire) == self.issued_epoch
            && (self.revision)() == self.issued_revision
            && !self.dirty.load(Ordering::Acquire)
            && secure_event_input_enabled() == Some(false)
            && ax::trusted()
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
        if !authorized() {
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

pub struct MacBridge {
    owner: ThreadId,
    policy: Arc<ObservationPolicy>,
    input: Input,
    secure_description: String,
    salt: [u8; 32],
    stopped: bool,
    config: Config,
    compatibility: Option<(i32, String, bool)>,
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
    not_send: PhantomData<Rc<()>>,
}
impl MacBridge {
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
        let notifications =
            AppNotifications::new(input.notifier(), dirty.clone(), layout_dirty.clone())?;
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
        if self.layout_dirty.swap(false, Ordering::AcqRel) {
            let (keys, kind) = keyboard_state();
            self.input.set_layout(keys, kind);
        }
        if self.native_dirty.swap(false, Ordering::AcqRel) {
            self.input.clear_composition();
        }
        Ok(())
    }
    pub fn session(&mut self) -> Result<Option<MacSession>> {
        self.owner()?;
        self.refresh_main_state()?;
        if self.stopped {
            return Ok(None);
        }
        let Some((pid, app_id, executable, name)) = frontmost_application() else {
            return Ok(None);
        };
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
            self.input.set_foreground(front);
            return Ok(None);
        }
        let Some(app) = AxElement::application(pid) else {
            return Ok(None);
        };
        let window = app.element("AXFocusedWindow");
        let Some(window_ref) = window.as_ref() else {
            return Ok(None);
        };
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
            None => return Ok(None),
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
        let last = std::cell::Cell::new(None::<Instant>);
        let dirty = self.native_dirty.clone();
        let observer = AxObservation::new(
            pid,
            Box::new(move |name, element| {
                let user = matches!(
                    name,
                    "AXFocusedUIElementChanged" | "AXFocusedWindowChanged" | "AXMainWindowChanged"
                );
                if matches!(name, "AXValueChanged" | "AXSelectedTextChanged") {
                    if element != focused.load(Ordering::Relaxed) {
                        return;
                    }
                    let now = Instant::now();
                    if last
                        .get()
                        .is_some_and(|time| now.duration_since(time) < Duration::from_millis(50))
                    {
                        return;
                    }
                    last.set(Some(now));
                }
                if user {
                    dirty.store(true, Ordering::Release)
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
    fn prepare_accessibility(&mut self, session: &MacSession) {
        let front = &session.foreground;
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
        if !enabled {
            return;
        }
        let app = &session.app;
        if app.settable("AXManualAccessibility") && app.bool("AXManualAccessibility") == Some(false)
        {
            let _ = app.set_bool("AXManualAccessibility", true);
        }
        if (is_chromium_bundle(&front.app_id)
            || exact_member(&front.app_id, CHAT_BUNDLES)
            || forced)
            && app.settable("AXEnhancedUserInterface")
            && app.bool("AXEnhancedUserInterface") == Some(false)
        {
            let _ = app.set_bool("AXEnhancedUserInterface", true);
        }
    }
    fn focused(&mut self) -> Result<Option<Focused>> {
        let Some(session) = self.session()? else {
            return Ok(None);
        };
        let front = &session.foreground;
        if exact_member(&front.app_id, TERMINALS) {
            return Ok(None);
        }
        let probe = exact_member(&front.app_id, CHAT_EDITORS);
        if !self.allowed(&session, probe) {
            return Ok(None);
        }
        self.prepare_accessibility(&session);
        let Some(field) = session.app.element("AXFocusedUIElement") else {
            return Ok(None);
        };
        if field.pid() != Some(front.window_pid) {
            return Ok(None);
        }
        if field.element("AXWindow") != session.window {
            return Ok(None);
        }
        if !field_is_ordinary_writable(&field, &self.secure_description) {
            return Ok(None);
        }
        if probe
            && !self
                .policy
                .rule_bool(&front.app_id, "", "complete_in_code_editors", false)
        {
            let Some(description) = field.string("AXDescription") else {
                return Ok(None);
            };
            if !stock_chat_description(&description) {
                return Ok(None);
            }
        }
        let Some(field_id) = self.field_identity(&field, &session) else {
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
            return Ok(None);
        }
        let Some(native) = field.range("AXSelectedTextRange") else {
            return Ok(None);
        };
        let Some(revalidated) = self.focused()? else {
            return Ok(None);
        };
        if !focus.same(&revalidated) {
            return Ok(None);
        }
        // Value is deliberately the last capability/privacy-gated read.
        let Some(text) = field.string("AXValue") else {
            return Ok(None);
        };
        if text.chars().count() > 1_000_000 {
            return Ok(None);
        }
        let text: TextBuffer = text.into();
        let Some((caret, selection_length)) = text
            .index()
            .utf16_range_to_scalar(native.location, native.length)
        else {
            return Ok(None);
        };
        let caret_rect = geometry::caret_bounds(
            field,
            &self.secure_description,
            &text,
            caret,
            native.location,
        );
        let field_rect = field.frame();
        let (render, evidence) = if native.length == 0 {
            geometry::inline_metrics(
                field,
                &self.secure_description,
                &text,
                caret,
                native.location,
                caret_rect,
                field_rect,
                is_browser(&[&focus.session.foreground.app_id]),
            )
        } else {
            (None, None)
        };
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
            window_bounds: focus.session.window.as_ref().and_then(AxElement::frame),
            can_replace: !uses_unicode_insertion(field, &front.app_id)
                && field.settable("AXSelectedText"),
            inline_render: render,
            ..Default::default()
        };
        if snapshot.inline_render.is_none() && self.config.screenshot_appearance {
            if let Some(evidence) = evidence {
                if let Some(mut adapter) = self.appearance.take() {
                    let styled = adapter.style(&mut self.capture_services(), &snapshot, &evidence);
                    self.appearance = Some(adapter);
                    if let Some(styled) = styled? {
                        snapshot.inline_render = validate_appearance(&snapshot, &evidence, styled);
                    }
                }
            }
        }
        let Some(after) = self.focused()? else {
            return Ok(None);
        };
        if !focus.same(&after) || !self.composition_clear(&after.field) {
            return Ok(None);
        }
        self.focused_hash.store(field.identity(), Ordering::Relaxed);
        Ok(Some((snapshot, focus.field)))
    }
    fn checked_current(&mut self, snapshot: &Snapshot) -> Result<Option<(Snapshot, AxElement)>> {
        Ok(self
            .capture_ordinary()?
            .filter(|(live, _)| live.same_key(snapshot) && live.same_authority(snapshot)))
    }
    fn confirm_edit(
        &mut self,
        before: &Snapshot,
        start: usize,
        length: usize,
        text: &str,
        dispatched: bool,
    ) -> Result<EditOutcome> {
        let Ok(Some((after, _))) = self.capture_ordinary() else {
            return Ok(EditOutcome::Indeterminate);
        };
        let Some(expected) = expected_mutation(before, start, length, text) else {
            return Ok(EditOutcome::Indeterminate);
        };
        Ok(classify_readback(before, &after, &expected, dispatched))
    }
}

impl NativeBridge for MacBridge {
    fn configure(&mut self, config: &Config) -> Result<()> {
        self.owner()?;
        let effective = effective_config(config);
        let policy = Arc::new(ObservationPolicy::new(&effective)?);
        self.policy_epoch.fetch_add(1, Ordering::AcqRel);
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
        if self.stopped {
            return Ok(None);
        }
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
    fn foreground(&mut self) -> Result<Option<Foreground>> {
        self.owner()?;
        if let Some(session) = self.session()? {
            return Ok(Some(session.foreground));
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
        if !valid_insert(snapshot, text) {
            return Ok(EditOutcome::Rejected);
        }
        if exact_member(&snapshot.app_id, TERMINALS) {
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
        let unicode = uses_unicode_insertion(&field, &live.app_id);
        if field.settable("AXSelectedText") && !unicode {
            if !self.input.commit() {
                return Ok(EditOutcome::Rejected);
            }
            return Ok(apply_selected_text(
                &mut NativeSelectedText {
                    bridge: self,
                    field,
                },
                snapshot,
                snapshot.caret,
                0,
                text,
            ));
        }
        if !(field.settable("AXValue") || unicode) || self.input.preflight_unicode().is_err() {
            return Ok(EditOutcome::Rejected);
        }
        if !self.input.commit() {
            return Ok(EditOutcome::Rejected);
        }
        let result = self.input.post_unicode(text);
        self.input.mutation_finished();
        if result.is_err() {
            return Ok(EditOutcome::Indeterminate);
        }
        self.confirm_edit(snapshot, snapshot.caret, 0, text, true)
    }
    fn replace_range(
        &mut self,
        snapshot: &Snapshot,
        start: usize,
        length: usize,
        text: &str,
    ) -> Result<EditOutcome> {
        self.owner()?;
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
        serde_json::json!({"platform":"macos","accessibility":trusted,"secure_input":secure,"input_source":source,"input":self.input.status(),"diagnostic":diagnostic,"stopped":self.stopped})
    }
    fn stop(&mut self) -> Result<()> {
        self.owner()?;
        self.stopped = true;
        self.policy_epoch.fetch_add(1, Ordering::AcqRel);
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
}
impl Drop for MacBridge {
    fn drop(&mut self) {
        if self.owner == thread::current().id() {
            let _ = self.stop();
        }
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
        let result = self.field.set_string("AXSelectedText", text);
        self.bridge.input.mutation_finished();
        result
    }
    fn observe(&mut self) -> Result<Option<Snapshot>> {
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
) -> EditOutcome {
    if let Err(error) = host.write_selected(text) {
        return if mutation_may_have_applied(error) {
            EditOutcome::Indeterminate
        } else {
            EditOutcome::Rejected
        };
    }
    let Ok(Some(after)) = host.observe() else {
        return EditOutcome::Indeterminate;
    };
    let Some(expected) = expected_mutation(before, start, length, text) else {
        return EditOutcome::Indeterminate;
    };
    classify_readback(before, &after, &expected, false)
}
fn classify_readback(
    before: &Snapshot,
    after: &Snapshot,
    expected: &(String, usize),
    dispatched: bool,
) -> EditOutcome {
    if !before.same_authority(after) {
        return EditOutcome::Indeterminate;
    }
    if after.text.as_ref() == expected.0 && after.caret == expected.1 && after.selection_length == 0
    {
        return EditOutcome::Applied(after.clone());
    }
    if before.same_key(after) {
        if dispatched {
            EditOutcome::Dispatched
        } else {
            EditOutcome::Rejected
        }
    } else {
        EditOutcome::Indeterminate
    }
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
        ],
    );
    digest.finalize().into_bytes().into()
}
fn composition_clear_with_source(field: &AxElement, source: InputSourceKind) -> bool {
    match field.attribute("AXMarkedTextRange") {
        Ok(None) => source == InputSourceKind::Layout,
        Ok(Some(value)) => value
            .as_range()
            .is_some_and(|range| range.length == 0 && range.end().is_some()),
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

struct NativeNotificationContext {
    note: Arc<dyn Fn(bool) + Send + Sync>,
    dirty: Arc<AtomicBool>,
    layout_dirty: Arc<AtomicBool>,
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
                if selector == unsafe { sel_registerName(c"layoutChanged:".as_ptr()) } {
                    context.layout_dirty.store(true, Ordering::Release)
                }
                context.dirty.store(true, Ordering::Release);
                (context.note)(true)
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
            for selector in [c"nativeChanged:", c"layoutChanged:"] {
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
    fn noop_setter_is_not_applied_and_never_retried() {
        let before = snapshot();
        let expected = expected_mutation(&before, 13, 0, "!").unwrap();
        assert!(matches!(
            classify_readback(&before, &before, &expected, false),
            EditOutcome::Rejected
        ));
        assert!(matches!(
            classify_readback(&before, &before, &expected, true),
            EditOutcome::Dispatched
        ));
        let mut after = before.clone();
        after.text = expected.0.clone().into();
        after.caret = expected.1;
        assert!(matches!(
            classify_readback(&before, &after, &expected, false),
            EditOutcome::Applied(_)
        ));
        after.window_id = "other".into();
        assert!(matches!(
            classify_readback(&before, &after, &expected, false),
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
            classify_readback(&before, &after, &expected, false),
            EditOutcome::Indeterminate
        ));
        assert!(mutation_may_have_applied(ax::AxError(-25204)));
        assert!(!mutation_may_have_applied(ax::AxError(-25202)));
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
            Ok(Some(self.observed.clone()))
        }
    }
    #[test]
    fn native_setter_readback_cannot_echo_requested_success() {
        let before = snapshot();
        for (mode, applied, rejected) in [
            (HostMode::Apply, true, false),
            (HostMode::Ignore, false, true),
            (HostMode::Reject, false, true),
            (HostMode::ReadError, false, false),
            (HostMode::ChangeOwner, false, false),
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
            let result = apply_selected_text(&mut host, &before, 13, 0, " 😀e\u{301}");
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
        let outcome = apply_selected_text(&mut host, &before, 6, 1, "e\u{301}");
        let EditOutcome::Applied(after) = outcome else {
            panic!("native host readback not confirmed")
        };
        assert_eq!(after.text.as_ref(), "hello e\u{301} world");
        assert_eq!(after.caret, 8);
        assert_eq!(host.writes, 1);
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
