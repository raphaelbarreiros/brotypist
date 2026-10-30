//! Six-step native onboarding and a capture-free, owned-field inference preview.
//! The preview service never receives the controller or an external native bridge.
use super::macos_settings_contract::{ReadValue, UiAction, UiQueue};
use super::{Kit, Obj, add, frame, identify, obj, rect, set_string, string};
use crate::{
    config::Config,
    core::{Snapshot, TextIndex},
    preview::PreviewSession,
    runtime::Command,
};
use anyhow::{Context, Result, ensure};
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSEvent, NSEventMask, NSTextView};
use objc2_foundation::{MainThreadMarker, NSObjectProtocol, NSRange, NSRect, NSSize, NSString};
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    path::PathBuf,
    ptr::NonNull,
    rc::Rc,
};

const WINDOW_ID: &str = "inklow.welcome";
const SOURCE_URL: &str = "https://github.com/renan-brasilio/brotypist";
const WIDTH: f64 = 760.0;
const HEIGHT: f64 = 580.0;

#[derive(Default)]
struct Ghost {
    text: String,
    snapshot: Option<Snapshot>,
    visible: String,
}
struct EditorState {
    queue: Rc<RefCell<UiQueue>>,
    active: Cell<bool>,
    dirty: Cell<bool>,
    ghost: RefCell<Ghost>,
    shortcuts: RefCell<BTreeMap<String, String>>,
    queued_accept: RefCell<Option<(Snapshot, String)>>,
}
impl EditorState {
    fn clear(&self) {
        *self.ghost.borrow_mut() = Ghost::default();
        self.dirty.set(true);
    }
    fn action(&self, action: UiAction) {
        self.queue.borrow_mut().pending.push_back(action);
    }
}
struct PreviewIvars {
    state: Rc<EditorState>,
}

define_class!(
    // NSTextView owns its text system; these overrides only draw or enqueue intents.
    // No callback holds a pointer to the movable Rust Welcome owner.
    #[unsafe(super = NSTextView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = PreviewIvars]
    struct WelcomePreviewTextView;
    unsafe impl NSObjectProtocol for WelcomePreviewTextView {}
    impl WelcomePreviewTextView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, dirty: NSRect) {
            unsafe { let _: () = msg_send![super(self), drawRect: dirty]; }
            self.draw_ghost();
        }
        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let state = &self.ivars().state;
            if state.active.get() {
                let chord = event_chord(event);
                if chord == "Escape" {
                    state.clear();
                    state.action(UiAction::PreviewDismiss);
                    unsafe { let _: () = msg_send![self, setNeedsDisplay: true]; }
                    return;
                }
                if let Ok(current) = read_editor(self.as_ref()) {
                    if native_focus(self.as_ref()) && current.selection_length == 0 && !marked(self.as_ref()) {
                        let ghost = state.ghost.borrow();
                        let offered = !ghost.visible.is_empty() && ghost.snapshot.as_ref().is_some_and(|s| s.same_key(&current));
                        let shortcuts = state.shortcuts.borrow();
                        let full = if !chord.is_empty() && shortcuts.get("word") == Some(&chord) { Some(false) }
                            else if !chord.is_empty() && shortcuts.get("all") == Some(&chord) { Some(true) }
                            else { None };
                        if offered {
                            if let Some(full) = full {
                                *state.queued_accept.borrow_mut() = Some((current, ghost.visible.clone()));
                                state.action(UiAction::PreviewAccept(full));
                                drop(shortcuts);
                                drop(ghost);
                                state.clear();
                                unsafe { let _: () = msg_send![self, setNeedsDisplay: true]; }
                                return;
                            }
                        }
                    }
                }
                state.clear();
            }
            unsafe { let _: () = msg_send![super(self), keyDown: event]; }
        }
        #[unsafe(method(setMarkedText:selectedRange:replacementRange:))]
        fn set_marked_text(&self, text: &AnyObject, selected: NSRange, replacement: NSRange) {
            if self.ivars().state.active.replace(false) {
                self.ivars().state.clear();
                self.ivars().state.action(UiAction::Deactivated(WINDOW_ID.into()));
            }
            unsafe { let _: () = msg_send![super(self), setMarkedText: text, selectedRange: selected, replacementRange: replacement]; }
        }
        #[unsafe(method(setSelectedRange:))]
        fn set_selected_range(&self, selected: NSRange) {
            unsafe { let _: () = msg_send![super(self), setSelectedRange: selected]; }
            self.revoke_selection(selected);
        }
        #[unsafe(method(setSelectedRange:affinity:stillSelecting:))]
        fn set_selected_range_with_affinity(&self, selected: NSRange, affinity: usize, still_selecting: bool) {
            unsafe { let _: () = msg_send![super(self), setSelectedRange: selected, affinity: affinity, stillSelecting: still_selecting]; }
            self.revoke_selection(selected);
        }
        #[unsafe(method(resignFirstResponder))]
        fn resign_first_responder(&self) -> bool {
            let resigned: bool = unsafe { msg_send![super(self), resignFirstResponder] };
            if resigned && self.ivars().state.active.replace(false) {
                self.ivars().state.clear();
                self.ivars().state.action(UiAction::Deactivated(WINDOW_ID.into()));
            }
            resigned
        }
    }
);
impl WelcomePreviewTextView {
    fn revoke_selection(&self, selected: NSRange) {
        if selected.length != 0 && self.ivars().state.active.replace(false) {
            self.ivars().state.clear();
            self.ivars()
                .state
                .action(UiAction::Deactivated(WINDOW_ID.into()));
        }
    }
    fn new(state: Rc<EditorState>, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(PreviewIvars { state });
        unsafe { msg_send![super(this), initWithFrame: rect(0.0, 0.0, 624.0, 128.0)] }
    }
    fn draw_ghost(&self) {
        let state = &self.ivars().state;
        if !state.active.get() || !native_focus(self.as_ref()) || marked(self.as_ref()) {
            return;
        }
        let Ok(current) = read_editor(self.as_ref()) else {
            return;
        };
        if current.selection_length != 0 {
            return;
        }
        let mut ghost = state.ghost.borrow_mut();
        if ghost.text.is_empty()
            || !ghost
                .snapshot
                .as_ref()
                .is_some_and(|s| s.same_key(&current))
        {
            return;
        }
        ghost.visible.clear();
        unsafe {
            let selection: NSRange = msg_send![self, selectedRange];
            let caret: NSRect = msg_send![self, firstRectForCharacterRange: selection, actualRange: std::ptr::null_mut::<NSRange>()];
            let window: *mut AnyObject = msg_send![self, window];
            if window.is_null() {
                return;
            }
            let window_rect: NSRect = msg_send![window, convertRectFromScreen: caret];
            let mut local: NSRect =
                msg_send![self, convertRect: window_rect, fromView: std::ptr::null::<AnyObject>()];
            local.origin.x += 1.0;
            if !current.suffix().is_empty() {
                local.origin.y += local.size.height + 4.0;
            }
            let mut bounds: NSRect = msg_send![self, visibleRect];
            bounds.origin.x += 12.0;
            bounds.size.width = (bounds.size.width - 24.0).max(0.0);
            let right = bounds.origin.x + bounds.size.width;
            let bottom = bounds.origin.y + bounds.size.height;
            if local.origin.x >= right || local.origin.y >= bottom || local.size.height <= 0.0 {
                return;
            }
            let attributes = obj("NSMutableDictionary");
            let font: *mut AnyObject = msg_send![self, font];
            let color: *mut AnyObject =
                msg_send![AnyClass::get(c"NSColor").unwrap(), secondaryLabelColor];
            if !font.is_null() {
                let _: () = msg_send![&*attributes, setObject: font, forKey: &*NSString::from_str("NSFont")];
            }
            let _: () =
                msg_send![&*attributes, setObject: color, forKey: &*NSString::from_str("NSColor")];
            let draw = rect(
                local.origin.x,
                local.origin.y,
                right - local.origin.x,
                bottom - local.origin.y,
            );
            // Measurement and rendering use the same native layout options. Only
            // complete, actually fitting glyphs become an acceptance authority.
            let visible = fitting_prefix(&ghost.text, |candidate| {
                let measured: NSRect = msg_send![&*NSString::from_str(candidate),
                    boundingRectWithSize: draw.size, options: 3usize, attributes: &*attributes,
                    context: std::ptr::null::<AnyObject>()];
                measured.size.width.is_finite()
                    && measured.size.height.is_finite()
                    && measured.size.width <= draw.size.width
                    && measured.size.height <= draw.size.height
            })
            .to_owned();
            let graphics = AnyClass::get(c"NSGraphicsContext").unwrap();
            let _: () = msg_send![graphics, saveGraphicsState];
            let clip: *mut AnyObject =
                msg_send![AnyClass::get(c"NSBezierPath").unwrap(), bezierPathWithRect: bounds];
            let _: () = msg_send![clip, addClip];
            let _: () = msg_send![&*NSString::from_str(&visible), drawWithRect: draw,
                options: 3usize, attributes: &*attributes, context: std::ptr::null::<AnyObject>()];
            let _: () = msg_send![graphics, restoreGraphicsState];
            ghost.visible = visible;
        }
    }
}

fn fitting_prefix(text: &str, fits: impl Fn(&str) -> bool) -> &str {
    if fits(text) {
        return text;
    }
    let mut low = 0;
    let mut high = text.chars().count();
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        let end = text
            .char_indices()
            .nth(middle)
            .map_or(text.len(), |(byte, _)| byte);
        if fits(&text[..end]) {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    let end = text
        .char_indices()
        .nth(low)
        .map_or(text.len(), |(byte, _)| byte);
    &text[..end]
}

// NSEvent copies the block, and this owner retains both it and its removal token.
// The weak state capture cannot keep a window/editor alive after Welcome drops.
struct InputMonitor {
    token: Obj,
    _handler: RcBlock<dyn Fn(NonNull<NSEvent>) -> *mut NSEvent>,
}
impl InputMonitor {
    fn new(state: &Rc<EditorState>) -> Result<Self> {
        let weak = Rc::downgrade(state);
        let handler = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
            if let Some(state) = weak.upgrade() {
                if state.active.get() {
                    state.clear();
                    state.queued_accept.borrow_mut().take();
                }
            }
            event.as_ptr()
        });
        let mask = NSEventMask::LeftMouseDown
            | NSEventMask::RightMouseDown
            | NSEventMask::OtherMouseDown
            | NSEventMask::ScrollWheel;
        let token =
            unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &handler) }
                .context("Cannot install the owned welcome field input monitor")?;
        Ok(Self {
            token,
            _handler: handler,
        })
    }
}
impl Drop for InputMonitor {
    fn drop(&mut self) {
        unsafe {
            NSEvent::removeMonitor(&self.token);
        }
    }
}

fn event_chord(event: &NSEvent) -> String {
    unsafe {
        let code: u16 = msg_send![event, keyCode];
        let flags: usize = msg_send![event, modifierFlags];
        let mut modifiers = [""; 4];
        let mut count = 0;
        for (name, bit) in [
            ("Ctrl", 1usize << 18),
            ("Alt", 1usize << 19),
            ("Shift", 1usize << 17),
            ("Super", 1usize << 20),
        ] {
            if flags & bit != 0 {
                modifiers[count] = name;
                count += 1;
            }
        }
        let mut key = crate::shortcuts::mac_key_name(code as u32);
        if key.is_empty() {
            let characters: *mut NSString = msg_send![event, charactersIgnoringModifiers];
            if !characters.is_null() {
                key = (&*characters).to_string();
            }
        }
        crate::shortcuts::shortcut_from_event(&key, &modifiers[..count], "macos", Some(code as u32))
            .unwrap_or_default()
    }
}
fn marked(editor: &AnyObject) -> bool {
    unsafe { msg_send![editor, hasMarkedText] }
}
fn native_focus(editor: &AnyObject) -> bool {
    unsafe {
        let window: *mut AnyObject = msg_send![editor, window];
        if window.is_null() {
            return false;
        }
        let visible: bool = msg_send![window, isVisible];
        let key: bool = msg_send![window, isKeyWindow];
        let responder: *mut AnyObject = msg_send![window, firstResponder];
        visible && key && std::ptr::eq(responder as *const AnyObject, editor as *const AnyObject)
    }
}
fn owned_snapshot(text: impl Into<String>, range: NSRange) -> Result<Snapshot> {
    let text: String = text.into();
    let mut snapshot = Snapshot {
        app_id: "brotypist.preview".into(),
        field_id: "welcome-try-it".into(),
        window_id: WINDOW_ID.into(),
        text: text.into(),
        ..Snapshot::default()
    };
    let (caret, selection_length) = snapshot
        .text_index()
        .utf16_range_to_scalar(range.location, range.length)
        .context("The preview selection is not a complete UTF-16 character boundary")?;
    snapshot.caret = caret;
    snapshot.selection_length = selection_length;
    Ok(snapshot)
}
fn read_editor(editor: &AnyObject) -> Result<Snapshot> {
    let range: NSRange = unsafe { msg_send![editor, selectedRange] };
    owned_snapshot(string(editor), range)
}
fn inserted_snapshot(before: &Snapshot, insertion: &str) -> Snapshot {
    let mut expected = before.clone();
    expected.text = format!("{}{}{}", before.prefix(), insertion, before.suffix()).into();
    expected.caret += insertion.chars().count();
    expected.selection_length = 0;
    expected
}
fn expanded_path(path: &str) -> PathBuf {
    if path == "~" {
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| path.into());
    }
    if let Some(tail) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(tail);
        }
    }
    path.into()
}
fn same_model_path(left: &str, right: &str) -> bool {
    let left = expanded_path(left);
    let right = expanded_path(right);
    left == right
        || match (left.canonicalize(), right.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
}
fn inventory_items(inventory: &Value) -> &[Value] {
    inventory.as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
fn permission(status: &Value, key: &str) -> bool {
    status
        .get("permissions")
        .and_then(|v| v.get(key))
        .and_then(Value::as_bool)
        == Some(true)
}
fn permissions_ready(status: &Value) -> bool {
    permission(status, "accessibility") && permission(status, "input_monitoring")
}
fn model_choice(config: &Config, inventory: &Value, pending: Option<&str>) -> String {
    let items = inventory_items(inventory);
    if let Some(id) = pending.filter(|id| items.iter().any(|m| field(m, "id") == *id)) {
        return id.into();
    }
    if config.backend != "gguf" {
        return String::new();
    }
    if !config.model_path.is_empty() {
        return items
            .iter()
            .find(|m| same_model_path(field(m, "path"), &config.model_path))
            .map(|m| field(m, "id").into())
            .unwrap_or_default();
    }
    crate::models::recommended_preset(crate::models::physical_memory_bytes())
        .id
        .clone()
}
fn model_ready(config: &Config, status: &Value, inventory: &Value, pending: Option<&str>) -> bool {
    if !field(status, "backend_error").is_empty() {
        return false;
    }
    let choice = model_choice(config, inventory, pending);
    if !choice.is_empty() {
        let Some(model) = inventory_items(inventory)
            .iter()
            .find(|m| field(m, "id") == choice)
        else {
            return false;
        };
        if model.get("installed").and_then(Value::as_bool) != Some(true)
            || config.backend != "gguf"
            || !same_model_path(field(model, "path"), &config.model_path)
        {
            return false;
        }
    }
    if config.backend == "gguf" {
        !config.model_path.is_empty() && expanded_path(&config.model_path).is_file()
    } else {
        !config.model.trim().is_empty() && crate::config::local_endpoint(&config.endpoint).is_ok()
    }
}
fn set_enabled(control: &AnyObject, enabled: bool) {
    unsafe {
        let _: () = msg_send![control, setEnabled: enabled];
    }
}
fn set_hidden(control: &AnyObject, hidden: bool) {
    unsafe {
        let _: () = msg_send![control, setHidden: hidden];
    }
}
fn set_title(control: &AnyObject, title: &str) {
    unsafe {
        let _: () = msg_send![control, setTitle: &*NSString::from_str(title)];
    }
}

pub(super) struct Welcome {
    kit: Kit,
    window: Obj,
    root: Obj,
    controls: BTreeMap<&'static str, Obj>,
    step: usize,
    state: Rc<EditorState>,
    editor: Option<Retained<WelcomePreviewTextView>>,
    _monitor: InputMonitor,
    session: Option<PreviewSession>,
    session_config: Option<Config>,
    stopping: bool,
    suspended: bool,
    awaiting_resume: bool,
    resume_as_action: bool,
    commands: Vec<Command>,
    draft: String,
    caret: usize,
    language: String,
    model_options: Vec<(String, Value)>,
    closing: bool,
}
impl Welcome {
    pub(super) fn new(kit: Kit) -> Result<Self> {
        MainThreadMarker::new().context("Welcome must be created on the AppKit main thread")?;
        let state = Rc::new(EditorState {
            queue: kit.queue.clone(),
            active: Cell::new(false),
            dirty: Cell::new(false),
            ghost: RefCell::new(Ghost::default()),
            shortcuts: RefCell::new(BTreeMap::new()),
            queued_accept: RefCell::new(None),
        });
        let monitor = InputMonitor::new(&state)?;
        let window = kit.window("Welcome to BroTypist", WINDOW_ID, WIDTH, HEIGHT, false);
        let root = kit.view(rect(0.0, 0.0, WIDTH, HEIGHT));
        unsafe {
            let _: () = msg_send![&*window, setContentView: &*root];
            let _: () = msg_send![&*window, setTitleVisibility: 1usize];
        }
        Ok(Self {
            kit,
            window,
            root,
            controls: BTreeMap::new(),
            step: 0,
            state,
            editor: None,
            _monitor: monitor,
            session: None,
            session_config: None,
            stopping: false,
            suspended: false,
            awaiting_resume: false,
            resume_as_action: false,
            commands: Vec::new(),
            draft: String::new(),
            caret: 0,
            language: "en".into(),
            model_options: Vec::new(),
            closing: false,
        })
    }
    pub(super) fn visible(&self) -> bool {
        unsafe { msg_send![&*self.window, isVisible] }
    }
    pub(super) fn show(
        &mut self,
        config: &Config,
        status: &Value,
        inventory: &Value,
        pending_model: Option<&str>,
    ) -> Result<Vec<Command>> {
        self.stop_preview(false)?;
        self.step = 0;
        self.closing = false;
        self.render(config, status, inventory, pending_model)?;
        super::present_window(&self.window, true);
        self.refresh(config, status, inventory, pending_model)
    }
    pub(super) fn navigate(
        &mut self,
        delta: i32,
        config: &Config,
        status: &Value,
        inventory: &Value,
        pending_model: Option<&str>,
    ) -> Result<Vec<Command>> {
        if delta > 0
            && ((self.step == 2 && !permissions_ready(status))
                || (self.step == 3 && !model_ready(config, status, inventory, pending_model)))
        {
            return self.refresh(config, status, inventory, pending_model);
        }
        self.stop_preview(false)?;
        if self.step == 5 && delta > 0 {
            self.kit
                .queue
                .borrow_mut()
                .pending
                .push_back(UiAction::Set("setup_completed".into(), json!(true)));
            self.close()?;
        } else {
            self.step = (self.step as i64 + delta as i64).clamp(0, 5) as usize;
            self.render(config, status, inventory, pending_model)?;
        }
        self.refresh(config, status, inventory, pending_model)
    }
    pub(super) fn refresh(
        &mut self,
        config: &Config,
        status: &Value,
        inventory: &Value,
        pending_model: Option<&str>,
    ) -> Result<Vec<Command>> {
        let was_stopping = self.stopping;
        if self.stopping {
            self.reap_preview()?;
        }
        if self.closing && self.session.is_none() {
            unsafe {
                let _: () = msg_send![&*self.window, orderOut: std::ptr::null::<AnyObject>()];
            }
        }
        if self.awaiting_resume && status.get("preview").and_then(Value::as_bool) == Some(false) {
            self.awaiting_resume = false;
        }
        if !self.visible() {
            self.stop_preview(false)?;
            return Ok(std::mem::take(&mut self.commands));
        }
        match self.step {
            1 => {
                for (key, value) in [
                    ("screen_context", config.screen_context),
                    ("clipboard_context", config.clipboard_context),
                    ("screenshot_context", config.screenshot_context),
                    ("screenshot_appearance", config.screenshot_appearance),
                ] {
                    if let Some(control) = self.controls.get(key) {
                        unsafe {
                            let _: () = msg_send![&**control, setState: isize::from(value)];
                        }
                    }
                }
            }
            2 => self.refresh_permissions(config, status),
            3 => self.refresh_models(config, status, inventory, pending_model),
            4 if !was_stopping => self.refresh_preview(config, status)?,
            5 => self.refresh_finish(config, status),
            _ => {}
        }
        Ok(std::mem::take(&mut self.commands))
    }
    pub(super) fn preview_accept(&mut self, full: bool) -> Result<()> {
        let Some(editor) = self.editor.as_ref() else {
            return Ok(());
        };
        if self.step != 4
            || self.stopping
            || !self.state.active.get()
            || !native_focus(editor.as_ref())
            || marked(editor.as_ref())
        {
            return Ok(());
        }
        let before = read_editor(editor.as_ref())?;
        if before.selection_length != 0 {
            return Ok(());
        }
        let queued = self.state.queued_accept.borrow_mut().take();
        let visible = match queued {
            Some((snapshot, visible)) if snapshot.same_key(&before) => visible,
            Some(_) => return Ok(()),
            None => {
                let ghost = self.state.ghost.borrow();
                if !ghost.snapshot.as_ref().is_some_and(|s| s.same_key(&before)) {
                    return Ok(());
                }
                ghost.visible.clone()
            }
        };
        if visible.is_empty() {
            return Ok(());
        }
        let Some(session) = self.session.as_mut() else {
            return Ok(());
        };
        session.update(&before.text, before.caret, before.selection_length)?;
        let insertion = session.accept_visible(full, &visible);
        if insertion.is_empty() {
            return Ok(());
        }
        let expected = inserted_snapshot(&before, &insertion);
        // Recheck the actual field immediately before the native mutation. Inserting
        // with the actual replacement range participates in AppKit undo and IME.
        if !native_focus(editor.as_ref())
            || marked(editor.as_ref())
            || !read_editor(editor.as_ref()).is_ok_and(|actual| actual.same_key(&before))
        {
            session.dismiss();
            self.set_ghost("", None);
            return Ok(());
        }
        let selection: NSRange = unsafe { msg_send![&**editor, selectedRange] };
        unsafe {
            let _: () = msg_send![&**editor, insertText: &*NSString::from_str(&insertion), replacementRange: selection];
        }
        let actual = match read_editor(editor.as_ref()) {
            Ok(actual) => actual,
            Err(error) => {
                session.dismiss();
                self.set_ghost("", None);
                return Err(error);
            }
        };
        let confirmed =
            native_focus(editor.as_ref()) && !marked(editor.as_ref()) && expected.same_key(&actual);
        if confirmed {
            session.update(&actual.text, actual.caret, actual.selection_length)?;
            self.draft = actual.text.to_string();
            self.caret = actual.caret;
        } else {
            // Do not fabricate acceptance or blindly undo a different native edit.
            session.dismiss();
            session.update(&actual.text, actual.caret, actual.selection_length)?;
        }
        self.set_ghost("", None);
        ensure!(
            confirmed,
            "The preview field did not confirm the exact accepted insertion; the suggestion was discarded"
        );
        Ok(())
    }
    pub(super) fn preview_dismiss(&mut self) {
        if let Some(session) = self.session.as_mut() {
            session.dismiss();
        }
        self.state.queued_accept.borrow_mut().take();
        self.set_ghost("", None);
    }
    pub(super) fn preview_language(&mut self, lang: &str) -> Result<()> {
        ensure!(
            matches!(lang, "en" | "pt"),
            "Choose English or Portuguese for the local preview"
        );
        if self.step != 4 {
            return Ok(());
        }
        self.stop_preview(true)?;
        self.language = lang.into();
        // Only this explicit action seeds document text. Entry/model selection do not.
        self.draft = if lang == "pt" {
            "Eu estava pensando que poderíamos"
        } else {
            "I was thinking we could"
        }
        .into();
        self.caret = self.draft.chars().count();
        if let Some(editor) = self.editor.as_ref() {
            set_string(editor.as_ref(), &self.draft);
            let range = NSRange::new(self.draft.encode_utf16().count(), 0);
            unsafe {
                let _: () = msg_send![&**editor, setSelectedRange: range];
                let _: bool = msg_send![&*self.window, makeFirstResponder: &**editor];
            }
        }
        if let Some(popup) = self.controls.get("language") {
            unsafe {
                let _: () = msg_send![&**popup, selectItemAtIndex: if lang == "pt" { 1isize } else { 0isize }];
            }
        }
        Ok(())
    }
    pub(super) fn window_deactivated(&mut self) -> Result<()> {
        self.stop_preview(true)?;
        Ok(())
    }
    pub(super) fn close(&mut self) -> Result<bool> {
        self.closing = true;
        if !self.stop_preview(true)? {
            return Ok(false);
        }
        unsafe {
            let _: () = msg_send![&*self.window, orderOut: std::ptr::null::<AnyObject>()];
        }
        Ok(true)
    }
    fn remember_editor(&mut self) {
        if let Some(editor) = self.editor.as_ref() {
            if let Ok(snapshot) = read_editor(editor.as_ref()) {
                self.draft = snapshot.text.to_string();
                self.caret = snapshot.caret;
            }
        }
    }
    fn stop_preview(&mut self, as_action: bool) -> Result<bool> {
        self.remember_editor();
        self.state.active.set(false);
        self.state.queued_accept.borrow_mut().take();
        self.set_ghost("", None);
        if self.session.is_none() {
            return Ok(true);
        }
        self.stopping = true;
        self.resume_as_action |= as_action;
        if let Some(session) = self.session.as_mut() {
            session.set_active(false);
        }
        self.reap_preview()
    }
    fn reap_preview(&mut self) -> Result<bool> {
        if let Some(session) = self.session.as_mut() {
            if !session.close()? {
                return Ok(false);
            }
        }
        self.session = None;
        self.session_config = None;
        self.stopping = false;
        if self.suspended {
            self.suspended = false;
            self.awaiting_resume = true;
            if self.resume_as_action {
                self.kit
                    .queue
                    .borrow_mut()
                    .pending
                    .push_back(UiAction::PreviewReleased);
            } else {
                self.commands.push(Command::ResumeFromPreview);
            }
        }
        self.resume_as_action = false;
        Ok(true)
    }
    fn set_ghost(&self, text: &str, snapshot: Option<Snapshot>) {
        {
            let ghost = self.state.ghost.borrow();
            if !self.state.dirty.get()
                && ghost.text == text
                && match (&ghost.snapshot, &snapshot) {
                    (None, None) => true,
                    (Some(before), Some(after)) => before.same_key(after),
                    _ => false,
                }
            {
                return;
            }
        }
        *self.state.ghost.borrow_mut() = Ghost {
            text: text.into(),
            snapshot,
            visible: String::new(),
        };
        self.state.dirty.set(false);
        if let Some(editor) = self.editor.as_ref() {
            unsafe {
                let _: () = msg_send![&**editor, setNeedsDisplay: true];
                let help = if text.is_empty() {
                    "Type to request a local suggestion.".into()
                } else {
                    format!("Suggestion: {text}")
                };
                let _: () = msg_send![&**editor, setAccessibilityHelp: &*NSString::from_str(&help)];
            }
        }
    }
    fn preview_config(config: &Config, language: &str) -> Config {
        let mut preview = config.clone();
        preview.record_history = false;
        preview.record_unaccepted = false;
        preview.statistics_enabled = false;
        preview.clipboard_context = false;
        preview.screen_context = false;
        preview.screenshot_context = false;
        preview.screenshot_appearance = false;
        let directive = if language == "pt" {
            "For this isolated welcome preview, continue the user's draft in Portuguese.\n"
        } else {
            "For this isolated welcome preview, continue the user's draft in English.\n"
        };
        // Config's instruction limit counts scalars, not bytes. Keep the language
        // directive within that limit even for a maximum-length saved preference.
        let remaining = 8000 - directive.chars().count();
        if let Some((byte, _)) = preview.custom_instructions.char_indices().nth(remaining) {
            preview.custom_instructions.truncate(byte);
        }
        preview.custom_instructions.insert_str(0, directive);
        preview
    }
    fn refresh_preview(&mut self, config: &Config, status: &Value) -> Result<()> {
        let Some(editor) = self.editor.as_ref() else {
            return Ok(());
        };
        if !native_focus(editor.as_ref()) {
            self.stop_preview(false)?;
            self.message(
                "preview_status",
                "Click the typing field to resume the local preview.",
            );
            return Ok(());
        }
        // IME composition and a selection revoke inference authority, not just ink.
        if marked(editor.as_ref()) {
            self.stop_preview(false)?;
            self.message(
                "preview_status",
                "Finish composing text to resume the local preview.",
            );
            return Ok(());
        }
        let current = match read_editor(editor.as_ref()) {
            Ok(current) => current,
            Err(error) => {
                self.stop_preview(false)?;
                return Err(error);
            }
        };
        if current.selection_length != 0 {
            self.stop_preview(false)?;
            self.message(
                "preview_status",
                "Place the caret without selecting text to resume.",
            );
            return Ok(());
        }
        if self
            .session_config
            .as_ref()
            .is_some_and(|old| old != config)
        {
            self.stop_preview(false)?;
            return Ok(());
        }
        if self.stopping || self.awaiting_resume || self.closing {
            return Ok(());
        }
        if self.session.is_none() {
            self.session = Some(PreviewSession::new(Self::preview_config(
                config,
                &self.language,
            ))?);
            self.session_config = Some(config.clone());
            self.suspended = true;
            self.commands.push(Command::SuspendForPreview);
            self.state.active.set(true);
            // Never reuse the status preceding the SuspendForPreview command.
            self.message(
                "preview_status",
                "Releasing the typing model for this isolated preview…",
            );
            return Ok(());
        }
        if *self.state.shortcuts.borrow() != config.shortcuts {
            *self.state.shortcuts.borrow_mut() = config.shortcuts.clone();
        }
        self.state.active.set(true);
        let session = self.session.as_mut().unwrap();
        session.set_active(true);
        session.set_owner_released(
            status.get("preview_suspended").and_then(Value::as_bool) == Some(true),
        );
        session.update(&current.text, current.caret, current.selection_length)?;
        let result = session.poll();
        let suggestion = field(&result, "suggestion");
        // Polling is synchronous, but retain an actual native readback boundary so
        // an event/result alone never becomes authority for display or insertion.
        let editor = self.editor.as_ref().unwrap();
        let actual = match read_editor(editor.as_ref()) {
            Ok(actual) => actual,
            Err(error) => {
                self.stop_preview(false)?;
                return Err(error);
            }
        };
        if !native_focus(editor.as_ref()) || marked(editor.as_ref()) || !current.same_key(&actual) {
            self.stop_preview(false)?;
            return Ok(());
        }
        self.set_ghost(suggestion, (!suggestion.is_empty()).then_some(current));
        self.message("preview_status", field(&result, "message"));
        Ok(())
    }
    fn message(&self, key: &str, text: &str) {
        if let Some(label) = self.controls.get(key) {
            set_string(label, text);
        }
    }
    fn label(&self, text: &str, x: f64, y: f64, w: f64, h: f64, size: f64, bold: bool) -> Obj {
        let label = self.kit.label(text, rect(x, y, w, h), size, bold);
        add(&self.root, &label);
        label
    }
    fn button(&self, title: &str, id: &str, x: f64, y: f64, w: f64, action: UiAction) -> Obj {
        let button = self.kit.button(
            title,
            &format!("inklow.welcome.{id}"),
            rect(x, y, w, 30.0),
            action,
        );
        add(&self.root, &button);
        button
    }
    fn card(&self, title: &str, body: &str, y: f64, height: f64) {
        let box_view = obj("NSBox");
        frame(&box_view, &rect(56.0, y, 648.0, height));
        unsafe {
            let _: () = msg_send![&*box_view, setBoxType: 4usize];
            let _: () = msg_send![&*box_view, setBorderType: 0usize];
            let _: () = msg_send![&*box_view, setCornerRadius: 10.0f64];
            let color: *mut AnyObject =
                msg_send![AnyClass::get(c"NSColor").unwrap(), controlBackgroundColor];
            let _: () = msg_send![&*box_view, setFillColor: color];
        }
        add(&self.root, &box_view);
        self.label(title, 72.0, y + 12.0, 610.0, 22.0, 13.0, true);
        self.label(
            body,
            72.0,
            y + 36.0,
            610.0,
            (height - 40.0).max(18.0),
            12.0,
            false,
        );
    }
    fn heading(&self, title: &str, subtitle: &str) {
        self.label(title, 56.0, 76.0, 648.0, 42.0, 26.0, true);
        self.label(subtitle, 56.0, 125.0, 648.0, 60.0, 13.0, false);
    }
    fn render(
        &mut self,
        config: &Config,
        status: &Value,
        inventory: &Value,
        pending: Option<&str>,
    ) -> Result<()> {
        self.state.active.set(false);
        self.kit.release_bindings(&self.root);
        self.controls.clear();
        self.editor = None;
        self.model_options.clear();
        self.root = self.kit.view(rect(0.0, 0.0, WIDTH, HEIGHT));
        unsafe {
            let _: () = msg_send![&*self.window, setContentView: &*self.root];
        }
        self.label(
            &format!("Step {} of 6", self.step + 1),
            440.0,
            18.0,
            108.0,
            18.0,
            11.0,
            false,
        );
        for index in 0..6 {
            let segment = obj("NSBox");
            frame(
                &segment,
                &rect(552.0 + index as f64 * 32.0, 25.0, 28.0, 4.0),
            );
            unsafe {
                let _: () = msg_send![&*segment, setBoxType: 4usize];
                let _: () = msg_send![&*segment, setBorderType: 0usize];
                let _: () = msg_send![&*segment, setCornerRadius: 2.0f64];
                let color: *mut AnyObject = if index <= self.step {
                    msg_send![AnyClass::get(c"NSColor").unwrap(), labelColor]
                } else {
                    msg_send![AnyClass::get(c"NSColor").unwrap(), separatorColor]
                };
                let _: () = msg_send![&*segment, setFillColor: color];
            }
            add(&self.root, &segment);
        }
        let divider = obj("NSBox");
        frame(&divider, &rect(0.0, 516.0, WIDTH, 1.0));
        unsafe {
            let _: () = msg_send![&*divider, setBoxType: 2usize];
        }
        add(&self.root, &divider);
        let back = self.button("Back", "back", 32.0, 534.0, 84.0, UiAction::WelcomeStep(-1));
        set_hidden(&back, self.step == 0);
        let title = [
            "Get started",
            "Continue",
            "Continue",
            "Continue",
            "Finish",
            "Start typing",
        ][self.step];
        let next = self.button(title, "next", 580.0, 534.0, 148.0, UiAction::WelcomeStep(1));
        // Return belongs to the editor on the try-it step, not the default button.
        unsafe {
            let _: () = msg_send![&*next, setKeyEquivalent: &*NSString::from_str(if self.step == 4 { "" } else { "\r" })];
        }
        self.controls.insert("next", next);
        let hint = [
            "Set up at your own pace",
            "Optional context is off by default",
            "Allow both permissions to continue",
            "Choose a ready local model to continue",
            "Use your configured acceptance keys",
            "Find BroTypist in the menu bar",
        ][self.step];
        self.label(hint, 138.0, 541.0, 424.0, 22.0, 12.0, false);
        match self.step {
            0 => self.step_intro(),
            1 => self.step_context(config),
            2 => self.step_permissions(config, status),
            3 => self.step_models(config, status, inventory, pending),
            4 => self.step_try(config)?,
            5 => self.step_finish(config, status),
            _ => unreachable!(),
        }
        Ok(())
    }
    fn step_intro(&self) {
        let icon = obj("NSImageView");
        frame(&icon, &rect(324.0, 65.0, 112.0, 112.0));
        unsafe {
            let app: *mut AnyObject =
                msg_send![AnyClass::get(c"NSApplication").unwrap(), sharedApplication];
            let image: *mut AnyObject = msg_send![app, applicationIconImage];
            let _: () = msg_send![&*icon, setImage: image];
        }
        add(&self.root, &icon);
        let welcome = self.label("Welcome to", 56.0, 183.0, 648.0, 30.0, 26.0, true);
        let mark = self.label("BroTypist", 56.0, 218.0, 648.0, 40.0, 32.0, true);
        unsafe {
            let raw: *mut AnyObject =
                msg_send![AnyClass::get(c"NSMutableAttributedString").unwrap(), alloc];
            let raw: *mut AnyObject =
                msg_send![raw, initWithString: &*NSString::from_str("BroTypist")];
            let lockup = Retained::from_raw(raw).expect("Native welcome wordmark");
            let font: *mut AnyObject = msg_send![&*mark, font];
            let color: *mut AnyObject =
                msg_send![AnyClass::get(c"NSColor").unwrap(), secondaryLabelColor];
            let _: () = msg_send![&*lockup, addAttribute: &*NSString::from_str("NSFont"), value: font, range: NSRange::new(0, 9)];
            let _: () = msg_send![&*lockup, addAttribute: &*NSString::from_str("NSColor"), value: color, range: NSRange::new(3, 6)];
            let _: () = msg_send![&*mark, setAttributedStringValue: &*lockup];
        }
        let tagline = self.label(
            "A little help with the words that come next.",
            56.0,
            266.0,
            648.0,
            25.0,
            13.0,
            false,
        );
        unsafe {
            for text in [&welcome, &mark, &tagline] {
                let _: () = msg_send![&**text, setAlignment: 1usize];
            }
        }
        self.card(
            "Runs locally",
            "Use a model on this Mac or a configured local server. No account required.",
            304.0,
            62.0,
        );
        self.card("Stays out of the way", "Faint inline suggestions in supported fields. Accept what you want, or keep typing to ignore them.", 374.0, 62.0);
        self.card(
            "Source on GitHub",
            "Public source code. No account needed to browse.",
            444.0,
            62.0,
        );
        self.button(
            "View source",
            "source",
            558.0,
            454.0,
            130.0,
            UiAction::Url(SOURCE_URL.into()),
        );
    }
    fn step_context(&mut self, config: &Config) {
        self.heading("Your writing stays local", "Predictions run on this Mac. BroTypist does not send your writing to a remote inference service, and writing history stays off unless you enable it in Personalization.");
        self.card("Text before cursor  →  Local model  →  Suggestion", &format!("Up to {} preceding characters from a supported, focused field. Password fields and excluded apps are skipped before capture.", config.context_chars), 192.0, 72.0);
        self.label("Optional context — each choice asks for confirmation and can be changed later in Context.", 56.0, 278.0, 648.0, 20.0, 12.0, true);
        for (index, (key, title, value)) in [
            (
                "screen_context",
                "Use focused-window text (Accessibility)",
                config.screen_context,
            ),
            (
                "clipboard_context",
                "Use the clipboard",
                config.clipboard_context,
            ),
            (
                "screenshot_context",
                "Use screenshots for context (Screen Recording)",
                config.screenshot_context,
            ),
            (
                "screenshot_appearance",
                "Use screenshots to improve suggestion appearance (Screen Recording)",
                config.screenshot_appearance,
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let control = self.kit.checkbox(
                title,
                &format!("inklow.welcome.{key}"),
                rect(64.0, 306.0 + index as f64 * 34.0, 620.0, 30.0),
                value,
                UiAction::Set(key.into(), Value::Null),
            );
            add(&self.root, &control);
            self.controls.insert(key, control);
        }
        self.label("Screenshots are processed in memory on this Mac and discarded immediately; they are never saved or uploaded. Accessibility text is always tried first.", 72.0, 446.0, 616.0, 46.0, 11.5, false);
    }
    fn step_permissions(&mut self, config: &Config, status: &Value) {
        self.heading("Two macOS permissions", "Approve BroTypist in System Settings. These cards show the actual permission state, not a saved preference.");
        for (key, title, detail, y) in [
            (
                "accessibility",
                "Accessibility",
                "Reads supported text near the caret and inserts only after you accept.",
                196.0,
            ),
            (
                "input_monitoring",
                "Input Monitoring",
                "Listens for typing and configured acceptance shortcuts. Secure fields remain protected.",
                292.0,
            ),
        ] {
            self.card(title, detail, y, 86.0);
            let button = self.button(
                "Open System Settings…",
                key,
                488.0,
                y + 9.0,
                200.0,
                UiAction::Permission(key.into()),
            );
            self.controls.insert(key, button);
        }
        self.card(
            "Screen & System Audio Recording (optional)",
            "Needed only if you turned on a screenshot option. Not required for typing.",
            388.0,
            72.0,
        );
        let button = self.button(
            "Request Access…",
            "screen_recording",
            488.0,
            397.0,
            200.0,
            UiAction::RequestScreen,
        );
        self.controls.insert("screen_recording", button);
        self.label("macOS may require a restart after granting access. Screen Recording is never required to continue.", 72.0, 470.0, 616.0, 40.0, 11.5, false);
        self.refresh_permissions(config, status);
    }
    fn refresh_permissions(&self, config: &Config, status: &Value) {
        for key in ["accessibility", "input_monitoring"] {
            if let Some(button) = self.controls.get(key) {
                let allowed = permission(status, key);
                set_title(
                    button,
                    if allowed {
                        "✓ Allowed"
                    } else {
                        "Open System Settings…"
                    },
                );
                set_enabled(button, !allowed);
            }
        }
        if let Some(button) = self.controls.get("screen_recording") {
            let allowed = permission(status, "screen_recording");
            let wanted = config.screenshot_context || config.screenshot_appearance;
            set_title(
                button,
                if allowed {
                    "✓ Allowed"
                } else if wanted {
                    "Request Access…"
                } else {
                    "Not needed"
                },
            );
            set_enabled(button, wanted && !allowed);
        }
        if let Some(next) = self.controls.get("next") {
            set_enabled(next, permissions_ready(status));
        }
    }
    fn step_models(
        &mut self,
        config: &Config,
        status: &Value,
        inventory: &Value,
        pending: Option<&str>,
    ) {
        self.heading("Choose your local model", "Choose a catalogue model or import a local file. Nothing downloads until you ask. Choosing a model does not run a prediction.");
        self.label(
            "The recommendation is based on this Mac’s memory. You can change models later.",
            56.0,
            189.0,
            648.0,
            22.0,
            12.0,
            false,
        );
        let popup = self.kit.popup(
            "inklow.welcome.modelChoice",
            "Local model",
            rect(56.0, 219.0, 648.0, 30.0),
            &[],
            &Value::Null,
            UiAction::ModelSelect(String::new()),
        );
        add(&self.root, &popup);
        self.controls.insert("model_choice", popup);
        let description = self.label("", 56.0, 260.0, 648.0, 44.0, 12.0, false);
        self.controls.insert("model_description", description);
        let request = self.button(
            "Download Model",
            "requestModel",
            56.0,
            315.0,
            224.0,
            UiAction::ModelDownload(String::new()),
        );
        let import = self.button(
            "Import Local File…",
            "importModel",
            292.0,
            315.0,
            184.0,
            UiAction::ModelImport,
        );
        self.controls.insert("model_request", request);
        self.controls.insert("model_import", import);
        self.button(
            "Advanced Settings…",
            "models",
            488.0,
            315.0,
            216.0,
            UiAction::Navigate("models".into()),
        );
        let selected = self.label("", 56.0, 361.0, 648.0, 25.0, 13.0, true);
        self.controls.insert("selected_model", selected);
        let model_status = self.label("", 56.0, 396.0, 648.0, 48.0, 12.0, false);
        self.controls.insert("model_status", model_status);
        let progress = self.kit.progress(
            "inklow.welcome.modelProgress",
            rect(56.0, 454.0, 516.0, 16.0),
        );
        add(&self.root, &progress);
        self.controls.insert("model_progress", progress);
        let cancel = self.button(
            "Cancel",
            "cancelDownload",
            588.0,
            446.0,
            116.0,
            UiAction::ModelCancel,
        );
        self.controls.insert("model_cancel", cancel);
        self.label(
            "Downloads are verified before use. Imported files stay where you choose them.",
            56.0,
            484.0,
            648.0,
            22.0,
            11.5,
            false,
        );
        self.refresh_models(config, status, inventory, pending);
    }
    fn refresh_models(
        &mut self,
        config: &Config,
        status: &Value,
        inventory: &Value,
        pending: Option<&str>,
    ) {
        let choice = model_choice(config, inventory, pending);
        let recommended =
            &crate::models::recommended_preset(crate::models::physical_memory_bytes()).id;
        let mut ordered: Vec<&Value> = inventory_items(inventory).iter().collect();
        ordered.sort_by_key(|model| {
            (
                field(model, "id") != recommended,
                model.get("installed").and_then(Value::as_bool) != Some(true),
            )
        });
        let mut options = Vec::new();
        if config.backend != "gguf"
            || (!config.model_path.is_empty()
                && !ordered
                    .iter()
                    .any(|m| same_model_path(field(m, "path"), &config.model_path)))
        {
            options.push((
                format!(
                    "{} — {}",
                    self.model_name(config, inventory),
                    if config.backend == "gguf" {
                        "local file"
                    } else {
                        "local server"
                    }
                ),
                json!(""),
            ));
        }
        for model in ordered {
            let id = field(model, "id");
            let state = if model.get("installed").and_then(Value::as_bool) == Some(true) {
                "Installed".into()
            } else {
                format!(
                    "{:.2} GB download",
                    model
                        .get("size_bytes")
                        .and_then(Value::as_f64)
                        .unwrap_or(0.0)
                        / 1_000_000_000.0
                )
            };
            options.push((
                format!(
                    "{}{} · {state}",
                    field(model, "label"),
                    if id == recommended {
                        " — Recommended"
                    } else {
                        ""
                    }
                ),
                json!(id),
            ));
        }
        if let Some(popup) = self.controls.get("model_choice") {
            unsafe {
                if options != self.model_options {
                    let _: () = msg_send![&**popup, removeAllItems];
                    for (title, _) in &options {
                        let _: () =
                            msg_send![&**popup, addItemWithTitle: &*NSString::from_str(title)];
                    }
                    self.kit.rebind(
                        popup,
                        UiAction::ModelSelect(String::new()),
                        ReadValue::Choice(options.iter().map(|(_, value)| value.clone()).collect()),
                    );
                    self.model_options = options;
                }
                let index = self
                    .model_options
                    .iter()
                    .position(|(_, id)| id == &json!(choice))
                    .map_or(-1, |n| n as isize);
                let _: () = msg_send![&**popup, selectItemAtIndex: index];
            }
        }
        let model = inventory_items(inventory)
            .iter()
            .find(|m| field(m, "id") == choice);
        let download = status.get("download").unwrap_or(&Value::Null);
        let active = field(download, "state") == "downloading";
        let ready = model_ready(config, status, inventory, pending);
        if let Some(next) = self.controls.get("next") {
            set_enabled(next, ready);
        }
        self.message(
            "selected_model",
            &format!("Current: {}", self.model_name(config, inventory)),
        );
        let details = if let Some(model) = model {
            if !field(model, "availability").is_empty() {
                field(model, "availability").to_owned()
            } else if model
                .get("requires_license_acceptance")
                .and_then(Value::as_bool)
                == Some(true)
                && model.get("installed").and_then(Value::as_bool) != Some(true)
            {
                format!(
                    "{} You’ll be asked to accept {} before downloading.",
                    field(model, "description"),
                    field(model, "license_name")
                )
            } else {
                field(model, "description").to_owned()
            }
        } else if config.backend == "gguf" {
            "Your imported file is used in place.".into()
        } else {
            "Your configured local server is kept. Try it checks inference; setup does not test the connection.".into()
        };
        self.message("model_description", &details);
        if let Some(request) = self.controls.get("model_request") {
            let installed =
                model.is_some_and(|m| m.get("installed").and_then(Value::as_bool) == Some(true));
            set_title(
                request,
                if installed {
                    "Use Model"
                } else {
                    "Download Model"
                },
            );
            let available = model.is_some_and(|m| field(m, "availability").is_empty());
            set_enabled(
                request,
                model.is_some() && !active && (installed || available),
            );
            self.kit.rebind(
                request,
                if installed {
                    UiAction::ModelSelect(choice.clone())
                } else {
                    UiAction::ModelDownload(choice.clone())
                },
                ReadValue::None,
            );
        }
        if let Some(import) = self.controls.get("model_import") {
            set_enabled(import, !active);
        }
        if let Some(cancel) = self.controls.get("model_cancel") {
            set_hidden(cancel, !active);
            set_enabled(cancel, active);
        }
        let received = download
            .get("received")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let total = download.get("total").and_then(Value::as_f64).unwrap_or(0.0);
        if let Some(progress) = self.controls.get("model_progress") {
            set_hidden(progress, !active);
            unsafe {
                let _: () = msg_send![&**progress, setIndeterminate: active && total <= 0.0];
                let _: () = msg_send![&**progress, setMaxValue: total.max(1.0)];
                let _: () =
                    msg_send![&**progress, setDoubleValue: received.clamp(0.0, total.max(1.0))];
                if active && total <= 0.0 {
                    let _: () =
                        msg_send![&**progress, startAnimation: std::ptr::null::<AnyObject>()];
                } else {
                    let _: () =
                        msg_send![&**progress, stopAnimation: std::ptr::null::<AnyObject>()];
                }
            }
        }
        let message = if active {
            let name = inventory_items(inventory)
                .iter()
                .find(|m| field(m, "id") == field(download, "id"))
                .map(|m| field(m, "label"))
                .unwrap_or("model");
            let progress = if total > 0.0 {
                format!(
                    "{:.1} / {:.1} MB",
                    received / 1_000_000.0,
                    total / 1_000_000.0
                )
            } else {
                format!("{:.1} MB", received / 1_000_000.0)
            };
            format!("Downloading {name}… {progress}. Verification follows the download.")
        } else if matches!(field(download, "state"), "error" | "cancelled") {
            [
                field(download, "error"),
                field(download, "message"),
                "Download stopped. You can try again.",
            ]
            .into_iter()
            .find(|s| !s.is_empty())
            .unwrap()
            .into()
        } else if !field(status, "backend_error").is_empty() {
            field(status, "backend_error").into()
        } else if ready {
            if config.backend == "gguf" {
                "Local model file and inference support are ready. Try it next."
            } else {
                "Local server configured. Try it next to check inference."
            }
            .into()
        } else if let Some(model) = model {
            if model.get("installed").and_then(Value::as_bool) == Some(true) {
                "Choose Use Model to make this installed model current.".into()
            } else {
                [
                    field(model, "availability"),
                    "This model is not installed. Choose Download Model to begin.",
                ]
                .into_iter()
                .find(|s| !s.is_empty())
                .unwrap()
                .into()
            }
        } else {
            "The current local model is not ready. Import an existing file or choose a catalogue model.".into()
        };
        self.message("model_status", &message);
    }
    fn step_try(&mut self, config: &Config) -> Result<()> {
        self.heading("Try it", "Type in this field to ask your actual local model for a suggestion. This preview does not read or insert into another application, and does not record writing history.");
        self.label(
            "Try your own words, or choose an example language:",
            56.0,
            192.0,
            430.0,
            24.0,
            12.0,
            false,
        );
        let language = self.kit.popup(
            "inklow.welcome.previewLanguage",
            "Preview example language",
            rect(492.0, 188.0, 212.0, 30.0),
            &[
                ("English (EN)".into(), json!("en")),
                ("Português (PT)".into(), json!("pt")),
            ],
            &json!(self.language),
            UiAction::PreviewLanguage(String::new()),
        );
        add(&self.root, &language);
        self.controls.insert("language", language);
        let scroll = obj("NSScrollView");
        frame(&scroll, &rect(56.0, 230.0, 648.0, 130.0));
        let mtm =
            MainThreadMarker::new().context("Preview must be created on the AppKit main thread")?;
        let editor = WelcomePreviewTextView::new(self.state.clone(), mtm);
        identify(
            editor.as_ref(),
            "inklow.welcome.preview",
            "Try local predictions. Type your own text.",
        );
        unsafe {
            let font: *mut AnyObject =
                msg_send![AnyClass::get(c"NSFont").unwrap(), systemFontOfSize: 17.0f64];
            let _: () = msg_send![&*editor, setFont: font];
            let _: () = msg_send![&*editor, setRichText: false];
            let _: () = msg_send![&*editor, setAllowsUndo: true];
            let _: () = msg_send![&*editor, setAutomaticTextCompletionEnabled: false];
            let _: () = msg_send![&*editor, setHorizontallyResizable: false];
            let _: () = msg_send![&*editor, setVerticallyResizable: true];
            let _: () = msg_send![&*editor, setTextContainerInset: NSSize::new(12.0, 12.0)];
            let supports_inline: bool =
                msg_send![&*editor, respondsToSelector: sel!(setAutomaticInlinePredictionEnabled:)];
            if supports_inline {
                let _: () = msg_send![&*editor, setAutomaticInlinePredictionEnabled: false];
            }
            let container: *mut AnyObject = msg_send![&*editor, textContainer];
            let _: () = msg_send![container, setWidthTracksTextView: true];
            set_string(editor.as_ref(), &self.draft);
            let caret = TextIndex::new(&self.draft)
                .scalar_to_utf16(crate::core::ScalarIndex(
                    self.caret.min(self.draft.chars().count()),
                ))
                .unwrap();
            let _: () = msg_send![&*editor, setSelectedRange: NSRange::new(caret.0, 0)];
            let _: () = msg_send![&*scroll, setHasVerticalScroller: true];
            let _: () = msg_send![&*scroll, setBorderType: 2usize];
            let _: () = msg_send![&*scroll, setDocumentView: &*editor];
        }
        add(&self.root, &scroll);
        self.label("A faint suffix is drawn in this field; it becomes document text only when you accept. Continue typing to ignore it.", 56.0, 380.0, 648.0, 45.0, 12.0, false);
        let word = config
            .shortcuts
            .get("word")
            .filter(|s| !s.is_empty())
            .map(String::as_str)
            .unwrap_or("Unassigned");
        let all = config
            .shortcuts
            .get("all")
            .filter(|s| !s.is_empty())
            .map(String::as_str)
            .unwrap_or("Unassigned");
        self.label(
            &format!("{word}: next word  ·  {all}: full suggestion  ·  Escape: dismiss"),
            56.0,
            439.0,
            648.0,
            30.0,
            12.0,
            false,
        );
        let status = self.label(
            "Waiting for you to type…",
            56.0,
            476.0,
            648.0,
            26.0,
            12.0,
            false,
        );
        self.controls.insert("preview_status", status);
        *self.state.shortcuts.borrow_mut() = config.shortcuts.clone();
        unsafe {
            let _: bool = msg_send![&*self.window, makeFirstResponder: &*editor];
        }
        self.editor = Some(editor);
        Ok(())
    }
    fn model_name(&self, config: &Config, inventory: &Value) -> String {
        if config.backend != "gguf" {
            return format!("{} ({})", config.model, config.backend);
        }
        if let Some(model) = inventory_items(inventory).iter().find(|m| {
            !config.model_path.is_empty() && same_model_path(field(m, "path"), &config.model_path)
        }) {
            return field(model, "label").into();
        }
        if config.model_path.is_empty() {
            "No model selected".into()
        } else {
            config.model_path.clone()
        }
    }
    fn step_finish(&mut self, config: &Config, status: &Value) {
        self.heading("You’re all set", "BroTypist lives in your menu bar. Click its caret glyph to pause predictions, control the current app, or open Settings.");
        let glyph = self.label("⌃", 342.0, 186.0, 76.0, 64.0, 52.0, true);
        unsafe {
            let _: () = msg_send![&*glyph, setAlignment: 1usize];
        }
        let word = config
            .shortcuts
            .get("word")
            .filter(|s| !s.is_empty())
            .map(String::as_str)
            .unwrap_or("Unassigned");
        let all = config
            .shortcuts
            .get("all")
            .filter(|s| !s.is_empty())
            .map(String::as_str)
            .unwrap_or("Unassigned");
        self.card(
            "Your typing setup",
            &format!(
                "Model: {}\nNext word: {word}   ·   Full suggestion: {all}",
                if config.backend == "gguf" {
                    &config.model_path
                } else {
                    &config.model
                }
            ),
            262.0,
            86.0,
        );
        self.card("macOS inline predictive text", "", 360.0, 78.0);
        let inline = self.label("", 72.0, 396.0, 400.0, 36.0, 12.0, false);
        self.controls.insert("inline_status", inline);
        self.button(
            "Keyboard Settings…",
            "keyboard",
            488.0,
            369.0,
            200.0,
            UiAction::Permission("keyboard".into()),
        );
        let login = self.kit.checkbox(
            "Launch BroTypist at login",
            "inklow.welcome.launchAtLogin",
            rect(56.0, 456.0, 320.0, 30.0),
            status
                .get("launch_at_login")
                .and_then(Value::as_bool)
                .unwrap_or(config.launch_at_login),
            UiAction::Set("launch_at_login".into(), Value::Null),
        );
        add(&self.root, &login);
        self.controls.insert("login", login);
        self.refresh_finish(config, status);
    }
    fn refresh_finish(&self, config: &Config, status: &Value) {
        self.message("inline_status", match status.get("inline_prediction_enabled").and_then(Value::as_bool) {
            Some(true) => "Enabled — its grey text competes with BroTypist. Turn off Show inline predictive text in Keyboard → Text Input → Edit….",
            Some(false) => "Disabled — no conflict.",
            None => "Couldn’t be detected. BroTypist only reads this setting and never changes it.",
        });
        if let Some(login) = self.controls.get("login") {
            let value = status
                .get("launch_at_login")
                .and_then(Value::as_bool)
                .unwrap_or(config.launch_at_login);
            unsafe {
                let _: () = msg_send![&**login, setState: isize::from(value)];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn displayed_prefix_never_includes_hidden_or_partial_utf8_text() {
        let text = "ação 😀 next";
        assert_eq!(fitting_prefix(text, |s| s.chars().count() <= 6), "ação 😀");
        assert_eq!(fitting_prefix(text, str::is_empty), "");
        assert_eq!(fitting_prefix(text, |_| true), text);
        assert_eq!(fitting_prefix("", |_| true), "");
        for width in 0..=text.chars().count() {
            let visible = fitting_prefix(text, |s| s.chars().count() <= width);
            assert!(text.starts_with(visible));
            assert!(visible.chars().count() <= width);
            assert!(text.is_char_boundary(visible.len()));
        }
    }
    #[test]
    fn native_ranges_reject_surrogates_and_preserve_scalar_selection() {
        let text = "A😀ação";
        let snapshot = owned_snapshot(text, NSRange::new(3, 2)).unwrap();
        assert_eq!(snapshot.caret, 2);
        assert_eq!(snapshot.selection_length, 2);
        assert!(owned_snapshot(text, NSRange::new(2, 0)).is_err());
        assert!(owned_snapshot(text, NSRange::new(1, 1)).is_err());
        assert!(owned_snapshot(text, NSRange::new(99, 0)).is_err());
        assert!(owned_snapshot(text, NSRange::new(1, usize::MAX)).is_err());
    }
    #[test]
    fn insertion_confirmation_checks_document_caret_and_selection() {
        let before = owned_snapshot("Olá 😀 fim", NSRange::new(7, 0)).unwrap();
        let expected = inserted_snapshot(&before, "ação ");
        let actual = owned_snapshot("Olá 😀 ação fim", NSRange::new(12, 0)).unwrap();
        assert!(expected.same_key(&actual));
        assert!(
            !expected.same_key(&owned_snapshot("Olá 😀 ação fim", NSRange::new(7, 0)).unwrap())
        );
        assert!(
            !expected.same_key(&owned_snapshot("Olá 😀 ação fim!", NSRange::new(12, 0)).unwrap())
        );
        assert!(
            !expected.same_key(&owned_snapshot("Olá 😀 ação fim", NSRange::new(12, 1)).unwrap())
        );
    }
    #[test]
    fn both_actual_permissions_are_required_but_screen_is_optional() {
        assert!(!permissions_ready(
            &json!({"permissions":{"accessibility":true}})
        ));
        assert!(!permissions_ready(
            &json!({"accessibility":true,"input_monitoring":true})
        ));
        assert!(permissions_ready(
            &json!({"permissions":{"accessibility":true,"input_monitoring":true,"screen_recording":false}})
        ));
    }
    #[test]
    fn pending_missing_model_cannot_borrow_old_readiness() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let config = Config {
            model_path: file.path().to_string_lossy().into_owned(),
            ..Config::default()
        };
        let inventory = json!([
            {"id":"old","path":config.model_path,"installed":true},
            {"id":"new","path":"/missing-new.gguf","installed":false}
        ]);
        assert!(model_ready(&config, &json!({}), &inventory, None));
        assert!(!model_ready(
            &config,
            &json!({"model_loaded":true}),
            &inventory,
            Some("new")
        ));
        assert!(!model_ready(
            &config,
            &json!({"backend_error":"Unavailable"}),
            &inventory,
            None
        ));
    }
    #[test]
    fn missing_import_cannot_borrow_worker_loaded_status() {
        let config = Config {
            model_path: "/missing-import.gguf".into(),
            ..Config::default()
        };
        assert!(!model_ready(
            &config,
            &json!({"model_loaded":true}),
            &json!([]),
            None
        ));
    }
    #[test]
    fn local_server_setup_does_not_make_a_network_request() {
        let config = Config {
            backend: "ollama".into(),
            ..Config::default()
        };
        assert!(model_ready(&config, &json!({}), &json!([]), None));
        assert!(!model_ready(
            &Config {
                model: String::new(),
                ..config.clone()
            },
            &json!({}),
            &json!([]),
            None
        ));
        assert!(!model_ready(
            &Config {
                endpoint: "https://remote.example".into(),
                ..config
            },
            &json!({}),
            &json!([]),
            None
        ));
    }
    #[test]
    fn preview_configuration_is_ephemeral_capture_free_and_language_bounded() {
        let config = Config {
            custom_instructions: "é".repeat(8000),
            record_history: true,
            record_unaccepted: true,
            statistics_enabled: true,
            clipboard_context: true,
            screen_context: true,
            screenshot_context: true,
            screenshot_appearance: true,
            ..Config::default()
        };
        let original = config.clone();
        for (language, expected) in [("en", "English"), ("pt", "Portuguese")] {
            let mut preview = Welcome::preview_config(&config, language);
            assert!(preview.custom_instructions.contains(expected));
            assert_eq!(preview.custom_instructions.chars().count(), 8000);
            assert!(
                !preview.record_history
                    && !preview.record_unaccepted
                    && !preview.statistics_enabled
            );
            assert!(!preview.clipboard_context && !preview.screen_context);
            assert!(!preview.screenshot_context && !preview.screenshot_appearance);
            preview.validate().unwrap();
        }
        assert_eq!(config, original);
    }
}
