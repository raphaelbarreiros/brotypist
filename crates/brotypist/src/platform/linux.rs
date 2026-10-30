//! Linux AT-SPI observation with a separately owned, replayable X11 hook.
#[path = "linux_atspi.rs"]
mod atspi;
#[path = "linux_x11.rs"]
mod x11;
use self::atspi::*;
use super::{
    ContextRead, DetachedContextRead, EditOutcome, Foreground, NativeBridge, Permission,
    ShortcutHandler, policy::ObservationPolicy,
};
use crate::{
    config::Config,
    core::{InlineRender, Rect, Snapshot},
};
use anyhow::{Result, bail};
use pango::prelude::*;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Default)]
struct InputState {
    revision: AtomicU64,
    user_revision: AtomicU64,
    time: Mutex<Option<Instant>>,
    callback: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}
impl InputState {
    fn note(&self, user: bool) {
        let revision = self.revision.fetch_add(1, Ordering::AcqRel) + 1;
        if user {
            self.user_revision.store(revision, Ordering::Release);
            *self.time.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
        }
        let callback = self
            .callback
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(callback) = callback {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback()));
        }
    }
}
struct Focus {
    object: Object,
    window: Option<Object>,
    pid: i32,
    app: String,
    executable: String,
    metadata: Metadata,
}
fn focused(
    api: &dyn Accessibility,
    policy: &ObservationPolicy,
    include_disabled: bool,
) -> Result<Option<Focus>> {
    let mut remaining = 2000;
    for app in api.children(&api.root(), 256)? {
        let Ok(Some((pid, name, executable))) = api.app_identity(&app) else {
            continue;
        };
        let id = if executable.is_empty() {
            name.clone()
        } else {
            executable.clone()
        };
        if pid <= 0
            || pid == std::process::id() as i32
            || id.is_empty()
            || (!include_disabled && !policy.allowed(&id, "", &[&name, &executable], false))
        {
            continue;
        }
        let mut stack = vec![(app, 0usize, false, None)];
        while let Some((object, depth, active, window)) = stack.pop() {
            if remaining == 0 {
                return Ok(None);
            }
            remaining -= 1;
            let Ok(metadata) = api.metadata(&object) else {
                continue;
            };
            if metadata.has(DEFUNCT) {
                continue;
            }
            let active = active || metadata.has(ACTIVE);
            let window = if metadata.has(ACTIVE) {
                Some(object.clone())
            } else {
                window
            };
            if depth == 1 && !active {
                continue;
            }
            if active && metadata.has(FOCUSED) {
                return Ok(Some(Focus {
                    object,
                    window,
                    pid,
                    app: id,
                    executable,
                    metadata,
                }));
            }
            if let Ok(children) = api.children(&object, 512) {
                for child in children.into_iter().rev() {
                    stack.push((child, depth + 1, active, window.clone()));
                }
            }
        }
    }
    Ok(None)
}
fn checked_text(read: TextRead, reject_selection: bool) -> Option<(String, usize, usize)> {
    let count = usize::try_from(read.count).ok()?;
    if count > 1_000_000 || read.text.chars().count() != count || read.selections.len() > 1 {
        return None;
    }
    if reject_selection && !read.selections.is_empty() {
        return None;
    }
    let (caret, length) = if let Some(&(start, end)) = read.selections.first() {
        let start = usize::try_from(start).ok()?;
        let end = usize::try_from(end).ok()?;
        (start.min(end), start.abs_diff(end))
    } else {
        (usize::try_from(read.caret).ok()?, 0)
    };
    if caret > count || length > count - caret {
        return None;
    }
    Some((read.text, caret, length))
}
fn snapshot_for(
    api: &dyn Accessibility,
    focus: &Focus,
    geometry: bool,
    reject_selection: bool,
) -> Result<Option<Snapshot>> {
    if !focus.metadata.editable() {
        return Ok(None);
    }
    let Some(read) = api.text(&focus.object, reject_selection)? else {
        return Ok(None);
    };
    let Some((text, caret, selection_length)) = checked_text(read, reject_selection) else {
        return Ok(None);
    };
    let caret_rect = if geometry {
        api.character_rect(&focus.object, caret)
            .ok()
            .filter(|r| r[3] > 0.0)
            .map(|mut r| {
                r[2] = r[2].max(1.0);
                r
            })
    } else {
        None
    };
    let window_id = focus
        .window
        .as_ref()
        .map(|o| o.identity(focus.pid))
        .unwrap_or_default();
    let inline_render = if geometry && selection_length == 0 {
        inline_metrics(api, focus, &text, caret, caret_rect)
    } else {
        None
    };
    Ok(Some(Snapshot {
        app_id: focus.app.clone(),
        field_id: focus.object.identity(focus.pid),
        text: text.into(),
        caret,
        selection_length,
        caret_rect,
        multiline: focus.metadata.has(MULTILINE),
        inline_render,
        window_revision: window_id.clone(),
        window_id,
        window_pid: focus.pid,
        can_replace: false,
        ..Default::default()
    }))
}
fn identity_matches(focus: &Focus, snapshot: &Snapshot) -> bool {
    focus.app == snapshot.app_id
        && focus.object.identity(focus.pid) == snapshot.field_id
        && focus
            .window
            .as_ref()
            .is_some_and(|w| w.identity(focus.pid) == snapshot.window_id)
        && !snapshot.window_id.is_empty()
        && focus.metadata.editable()
        && focus.metadata.has(VISIBLE)
        && focus.metadata.has(SHOWING)
}
fn intersect(a: Rect, b: Rect) -> Option<Rect> {
    if !a.iter().chain(b.iter()).all(|n| n.is_finite()) {
        return None;
    }
    let x = a[0].max(b[0]);
    let y = a[1].max(b[1]);
    let right = (a[0] + a[2]).min(b[0] + b[2]);
    let bottom = (a[1] + a[3]).min(b[1] + b[3]);
    (right > x && bottom > y).then_some([x, y, right - x, bottom - y])
}
fn inline_metrics(
    api: &dyn Accessibility,
    focus: &Focus,
    text: &str,
    caret: usize,
    rect: Option<Rect>,
) -> Option<InlineRender> {
    let rect = rect?;
    let previous = text.chars().nth(caret.checked_sub(1)?)?;
    if matches!(previous, '\r' | '\n') || !focus.metadata.inactive_composition() {
        return None;
    }
    let suffix = text.chars().skip(caret).next();
    if suffix.is_some_and(|c| !matches!(c, '\r' | '\n')) {
        return None;
    }
    let (run, start, end) = api.attribute_run(&focus.object, caret - 1).ok()?;
    if start < 0
        || start as usize > caret - 1
        || end <= 0
        || end as usize <= caret - 1
        || run.get("direction")?.as_str() != "ltr"
    {
        return None;
    }
    let name = run.get("family-name")?;
    let size: f64 = run.get("size")?.parse().ok()?;
    if name.is_empty()
        || !(1.0..=300.0).contains(&size)
        || run.get("rise").is_some_and(|v| v != "0")
    {
        return None;
    }
    let weight: i32 = run
        .get("weight")
        .map(String::as_str)
        .unwrap_or("400")
        .parse()
        .ok()?;
    let italic = run.get("style").is_some_and(|v| v == "italic");
    let mut description = pango::FontDescription::new();
    description.set_family(name);
    description.set_size((size * pango::SCALE as f64).round() as i32);
    description.set_weight(unsafe { pango::glib::translate::from_glib(weight) });
    description.set_style(if italic {
        pango::Style::Italic
    } else {
        pango::Style::Normal
    });
    let context = pangocairo::FontMap::default().create_context();
    let font = context.load_font(&description)?;
    if font.describe().family()?.to_lowercase() != name.to_lowercase() {
        return None;
    }
    let metrics = context.metrics(Some(&description), Some(&pango::Language::default()));
    let ascent = metrics.ascent() as f64 / pango::SCALE as f64;
    let descent = metrics.descent() as f64 / pango::SCALE as f64;
    let glyph = api.character_rect(&focus.object, caret - 1).ok()?;
    let mut clip = api.extents(&focus.object).ok()?;
    let mut parent = Some(focus.object.clone());
    for _ in 0..16 {
        let Some(object) = parent else { break };
        let metadata = api.metadata(&object).ok()?;
        if !metadata.has(VISIBLE) || !metadata.has(SHOWING) {
            return None;
        }
        let bounds = api.extents(&object).ok()?;
        if bounds[2] > 0.0 && bounds[3] > 0.0 {
            clip = intersect(clip, bounds)?;
        }
        if metadata.has(ACTIVE) {
            break;
        }
        parent = api.parent(&object).ok()?;
    }
    if (glyph[3] - ascent - descent).abs() > 1.0
        || (glyph[1] - rect[1]).abs() > 1.0
        || (glyph[0] + glyph[2] - rect[0]).abs() > 1.5
    {
        return None;
    }
    let render = InlineRender {
        font_name: name.clone(),
        font_size: size,
        baseline: glyph[1] + ascent,
        clip_rect: clip,
        ascent,
        descent,
        source: "linux-atspi-font-run".into(),
        font_weight: weight,
        italic,
        ..Default::default()
    };
    render.valid(Some(rect)).then_some(render)
}
fn read_context(
    api: &dyn Accessibility,
    policy: &ObservationPolicy,
    snapshot: &Snapshot,
    authorized: &dyn Fn() -> bool,
    max_chars: usize,
) -> Result<ContextRead> {
    let blocked = || ContextRead::blocked("accessibility");
    if !authorized() {
        return Ok(blocked());
    }
    let Some(focus) = focused(api, policy, false)? else {
        return Ok(blocked());
    };
    if !identity_matches(&focus, snapshot) {
        return Ok(blocked());
    }
    let Some(window) = focus.window else {
        return Ok(blocked());
    };
    let mut stack = vec![(window, 0)];
    let mut chunks = Vec::new();
    let mut remaining = max_chars.min(12000);
    let deadline = Instant::now() + Duration::from_millis(150);
    for _ in 0..128 {
        if remaining == 0 || Instant::now() >= deadline {
            break;
        }
        let Some((object, depth)) = stack.pop() else {
            break;
        };
        if !authorized() {
            return Ok(blocked());
        }
        let Ok(metadata) = api.metadata(&object) else {
            continue;
        };
        if metadata.protected()
            || metadata.has(DEFUNCT)
            || !metadata.has(VISIBLE)
            || !metadata.has(SHOWING)
        {
            continue;
        }
        let editable = metadata.has(EDITABLE);
        if !editable && matches!(metadata.role, LABEL | TEXT) {
            if !authorized() {
                return Ok(blocked());
            }
            let value: String = api
                .static_text(&object, remaining)?
                .chars()
                .take(remaining)
                .collect();
            if !value.is_empty() && value != snapshot.text.as_ref() {
                remaining = remaining.saturating_sub(value.chars().count() + 1);
                chunks.push(value);
            }
        }
        if depth < 8 && !editable {
            if let Ok(children) = api.children(&object, 64) {
                for child in children {
                    stack.push((child, depth + 1));
                }
            }
        }
    }
    if !authorized()
        || !focused(api, policy, false)?.is_some_and(|focus| identity_matches(&focus, snapshot))
    {
        return Ok(blocked());
    }
    let text = chunks
        .join("\n")
        .chars()
        .take(max_chars.min(12000))
        .collect::<String>();
    Ok(ContextRead {
        status: if text.is_empty() { "empty" } else { "ready" }.into(),
        text,
        source: "accessibility".into(),
    })
}

fn insertion_range(snapshot: &Snapshot, start: usize, length: usize) -> bool {
    length == 0
        && start == snapshot.caret
        && snapshot.selection_length == 0
        && snapshot
            .text
            .index()
            .scalar_range_to_utf8(start, length)
            .is_some()
}

fn insert_text(
    api: &dyn Accessibility,
    policy: &ObservationPolicy,
    snapshot: &Snapshot,
    text: &str,
) -> Result<EditOutcome> {
    if text.is_empty()
        || text.contains('\0')
        || snapshot.protected
        || snapshot.selection_length != 0
    {
        return Ok(EditOutcome::Rejected);
    }
    let Some(focus) = focused(api, policy, false)? else {
        return Ok(EditOutcome::Rejected);
    };
    let Some(current) = snapshot_for(api, &focus, false, false)? else {
        return Ok(EditOutcome::Rejected);
    };
    if !current.same_key(snapshot) || !current.same_authority(snapshot) {
        return Ok(EditOutcome::Rejected);
    }
    let bytes = i32::try_from(text.len())?;
    let caret = i32::try_from(current.caret)?;
    match api.insert(&focus.object, caret, text, bytes) {
        Ok(false) => Ok(EditOutcome::Rejected),
        Err(_) => Ok(EditOutcome::Indeterminate),
        Ok(true) => {
            let next = current
                .caret
                .checked_add(text.chars().count())
                .and_then(|v| i32::try_from(v).ok());
            if let Some(next) = next {
                let _ = api.set_caret(&focus.object, next);
            }
            // Setter success is delivery, not confirmed host text; never retry a caret failure.
            Ok(EditOutcome::Dispatched)
        }
    }
}

/// Only immutable observation metadata crosses threads. Each operation creates
/// and drops its own D-Bus connection on the context-reader thread.
struct LinuxContextRead {
    snapshot: Snapshot,
    policy: ObservationPolicy,
}
impl DetachedContextRead for LinuxContextRead {
    fn read(&self, authorized: &dyn Fn() -> bool, max_chars: usize) -> Result<ContextRead> {
        if !authorized() {
            return Ok(ContextRead::blocked("accessibility"));
        }
        let api = Atspi::connect()?;
        read_context(&api, &self.policy, &self.snapshot, authorized, max_chars)
    }
    fn matches(&self) -> Result<bool> {
        let api = Atspi::connect()?;
        Ok(focused(&api, &self.policy, false)?
            .is_some_and(|focus| identity_matches(&focus, &self.snapshot)))
    }
    fn matches_snapshot(&self, snapshot: &Snapshot) -> bool {
        self.snapshot.same_key(snapshot) && self.snapshot.same_authority(snapshot)
    }
}

pub struct LinuxBridge {
    api: Atspi,
    policy: ObservationPolicy,
    config: Config,
    hook: Option<x11::Hook>,
    input: Arc<InputState>,
    accessibility_only: bool,
    stopped: bool,
    diagnostic: String,
    observed: Option<Snapshot>,
}
impl LinuxBridge {
    pub fn new() -> Result<Self> {
        if std::env::var("XDG_SESSION_TYPE").is_ok_and(|s| s.eq_ignore_ascii_case("wayland"))
            || std::env::var_os("WAYLAND_DISPLAY").is_some()
        {
            bail!(
                "Wayland requires the explicitly selected native input method; XWayland interception is unsupported"
            );
        }
        if std::env::var_os("DISPLAY").is_none() {
            bail!("No X11 DISPLAY; launch in an X11 desktop session");
        }
        x11::check_connection()?;
        let mut this = Self::accessibility_only()?;
        this.accessibility_only = false;
        this.diagnostic="Linux AT-SPI/X11 ready. Inline requires explicit inactive IME state and matching Pango/native metrics. Browser domain metadata is unavailable.".into();
        Ok(this)
    }
    pub fn accessibility_only() -> Result<Self> {
        let config = Config::default();
        let policy = ObservationPolicy::new(&config)?;
        Ok(Self {
            api: Atspi::connect()?,
            policy,
            config,
            hook: None,
            input: Arc::default(),
            accessibility_only: true,
            stopped: false,
            diagnostic:
                "AT-SPI accessibility ready; no keyboard interception or compositor authority"
                    .into(),
            observed: None,
        })
    }
    pub fn capture_accessibility(
        &mut self,
        geometry: bool,
        reject_selection: bool,
    ) -> Result<Option<Snapshot>> {
        if self.stopped {
            return Ok(None);
        }
        let Some(focus) = focused(&self.api, &self.policy, false)? else {
            self.observed = None;
            return Ok(None);
        };
        let snapshot = snapshot_for(&self.api, &focus, geometry, reject_selection)?;
        if let Some(current) = &snapshot {
            if self
                .observed
                .as_ref()
                .is_some_and(|old| !old.same_key(current) || !old.same_authority(current))
            {
                self.input.note(true);
            }
            self.observed = Some(current.clone());
        }
        if let Some(hook) = &self.hook {
            hook.identity(focus.app);
        }
        Ok(snapshot)
    }
    pub fn accessibility_context_matches(&mut self, snapshot: &Snapshot) -> Result<bool> {
        if self.stopped {
            return Ok(false);
        }
        Ok(focused(&self.api, &self.policy, false)?
            .is_some_and(|focus| identity_matches(&focus, snapshot)))
    }
    pub fn accessibility_focused_context(
        &mut self,
        snapshot: &Snapshot,
        authorized: &dyn Fn() -> bool,
        max_chars: usize,
    ) -> Result<ContextRead> {
        if self.stopped {
            return Ok(ContextRead::blocked("accessibility"));
        }
        read_context(&self.api, &self.policy, snapshot, authorized, max_chars)
    }
}
impl NativeBridge for LinuxBridge {
    fn configure(&mut self, config: &Config) -> Result<()> {
        self.policy = ObservationPolicy::new(config)?;
        self.config = config.clone();
        if let Some(hook) = &self.hook {
            hook.configure(config.clone())?;
        }
        Ok(())
    }
    fn capture(&mut self) -> Result<Option<Snapshot>> {
        self.capture_accessibility(true, false)
    }
    fn foreground(&mut self) -> Result<Option<Foreground>> {
        if self.stopped {
            return Ok(None);
        }
        Ok(focused(&self.api, &self.policy, true)?.map(|f| Foreground {
            app_id: f.app,
            executable: f.executable,
            domain: String::new(),
            window_id: f.window.map(|w| w.identity(f.pid)).unwrap_or_default(),
            window_pid: f.pid,
            ..Default::default()
        }))
    }
    fn insert(&mut self, snapshot: &Snapshot, text: &str) -> Result<EditOutcome> {
        if self.stopped {
            return Ok(EditOutcome::Rejected);
        }
        let outcome = insert_text(&self.api, &self.policy, snapshot, text)?;
        if matches!(outcome, EditOutcome::Dispatched) {
            self.input.note(false);
            self.observed = None;
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
        if !insertion_range(snapshot, start, length) {
            self.diagnostic="AT-SPI atomic range replacement unavailable; destructive delete/insert fallback refused".into();
            return Ok(EditOutcome::Rejected);
        }
        self.insert(snapshot, text)
    }
    fn range_rect(
        &mut self,
        _snapshot: &Snapshot,
        _start: usize,
        _length: usize,
    ) -> Result<Option<Rect>> {
        // Baseline AT-SPI adapter exposes caret/glyph geometry only, never a range replacement anchor.
        Ok(None)
    }
    fn focused_context(
        &mut self,
        snapshot: &Snapshot,
        authorized: &dyn Fn() -> bool,
        max_chars: usize,
    ) -> Result<ContextRead> {
        self.accessibility_focused_context(snapshot, authorized, max_chars)
    }
    fn context_ticket(
        &mut self,
        snapshot: &Snapshot,
    ) -> Result<Option<Box<dyn DetachedContextRead>>> {
        // Wayland needs live compositor agreement, which cannot be established by a detached AT-SPI reader.
        if self.accessibility_only
            || self.stopped
            || !self.accessibility_context_matches(snapshot)?
        {
            return Ok(None);
        }
        Ok(Some(Box::new(LinuxContextRead {
            snapshot: snapshot.clone(),
            policy: self.policy.clone(),
        })))
    }
    fn context_matches(&mut self, snapshot: &Snapshot) -> Result<bool> {
        self.accessibility_context_matches(snapshot)
    }
    fn start_shortcuts(&mut self, callback: ShortcutHandler) -> Result<()> {
        if self.stopped {
            bail!("Linux bridge is stopped");
        }
        if self.accessibility_only {
            bail!("Accessibility-only adapter has no X11 keyboard authority");
        }
        if self.hook.is_some() {
            bail!("X11 shortcut owner already started");
        }
        self.hook = Some(x11::Hook::start(
            self.config.clone(),
            callback,
            self.input.clone(),
        )?);
        Ok(())
    }
    fn set_shortcuts_enabled(&mut self, enabled: bool) {
        if let Some(hook) = &self.hook {
            hook.enable(enabled && !self.stopped);
        }
    }
    fn set_input_callback(&mut self, callback: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self
            .input
            .callback
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = if self.stopped { None } else { callback };
    }
    fn finish_input(&mut self, accepted: bool) -> Result<()> {
        if let Some(hook) = &self.hook {
            hook.finish(accepted)?;
        }
        Ok(())
    }
    fn input_revision(&self) -> u64 {
        self.input.revision.load(Ordering::Acquire)
    }
    fn user_input_revision(&self) -> u64 {
        self.input.user_revision.load(Ordering::Acquire)
    }
    fn input_timestamp(&self) -> Option<Instant> {
        *self.input.time.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn secure_input_enabled(&self) -> Option<bool> {
        Some(false)
    } // Linux has no global secure-input mode; AT-SPI privacy gates remain mandatory.
    fn request_permission(&mut self, kind: &str) -> Result<Permission> {
        Ok(match kind {
            "accessibility" => Permission::Allowed,
            "input" | "keyboard" if !self.accessibility_only => Permission::Allowed,
            _ => Permission::Unknown,
        })
    }
    fn status(&self) -> serde_json::Value {
        serde_json::json!({"platform":"linux","available":!self.stopped,"accessibility":true,"accessibility_only":self.accessibility_only,"domain_observable":false,"message":self.diagnostic,"hook":self.hook.as_ref().map(|h|h.status())})
    }
    fn stop(&mut self) -> Result<()> {
        self.stopped = true;
        *self
            .input
            .callback
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        if let Some(hook) = &mut self.hook {
            hook.stop()?;
        }
        self.hook = None;
        Ok(())
    }
}
impl Drop for LinuxBridge {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(test)]
#[path = "linux_tests.rs"]
mod tests;
