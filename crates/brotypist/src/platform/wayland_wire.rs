//! Single-thread private KWin connection, XKB grab, and compositor-owned SHM buffers.
#![allow(unsafe_op_in_unsafe_fn)]
use super::super::{
    Action, NativeChoices, NativePreedit, ShortcutHandler, policy::ObservationPolicy,
};
use super::{
    panel::{MenuItem, Panel},
    protocol::*,
};
use anyhow::{Result, bail};
use libc::{c_int, c_void};
use std::{
    ffi::{CStr, CString},
    os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd},
    ptr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use zeroize::Zeroize;
const MAX_TEXT: usize = 1024 * 1024;
const MAX_IMAGE: usize = 64 * 1024 * 1024;
const MAX_KEY: usize = 768;
const REGISTRY: usize = 1;
const METHOD_TAG: usize = 2;
const CONTEXT_TAG: usize = 3;
const SEAT: usize = 4;
const POINTER: usize = 5;
const KEYBOARD: usize = 6;
const BUFFER: usize = 7;
const SHM: usize = 8;
#[derive(Default)]
pub(super) struct ContextState {
    pub generation: u32,
    pub serial: u32,
    pub context_id: u64,
    pub active: bool,
    pub known: bool,
    pub safe: bool,
    pub surrounding: bool,
    pub ready: bool,
    pub initial: bool,
    text: Vec<u8>,
    cursor: usize,
}
impl ContextState {
    fn clear_text(&mut self) {
        self.text.zeroize();
        self.text.clear();
        self.cursor = 0;
        self.surrounding = false;
    }
    fn invalidate(&mut self, erase: bool) {
        self.ready = false;
        self.generation = self.generation.wrapping_add(1).max(1);
        if erase {
            self.clear_text();
        }
    }
    fn release(&mut self) {
        self.invalidate(true);
        self.active = false;
        self.known = false;
        self.safe = false;
        self.serial = 0;
        self.initial = false;
    }
    fn activate(&mut self) {
        self.release();
        self.active = true;
        self.initial = true;
        self.context_id = self.context_id.wrapping_add(1).max(1);
    }
    fn content(&mut self, hint: u32, purpose: u32) {
        let safe = hint & (0xc0 | !0x3ff) == 0 && purpose <= 12 && purpose != 8;
        self.invalidate(!self.known || self.safe != safe);
        self.known = true;
        self.safe = safe;
    }
    fn surrounding(&mut self, text: &[u8], cursor: usize, anchor: usize) {
        self.invalidate(true);
        if !self.known
            || !self.safe
            || cursor != anchor
            || text.len() > MAX_TEXT
            || text.contains(&0)
        {
            return;
        }
        let Ok(s) = std::str::from_utf8(text) else {
            return;
        };
        if !s.is_char_boundary(cursor) {
            return;
        }
        self.text.extend_from_slice(text);
        self.cursor = cursor;
        self.surrounding = true;
    }
    fn commit_state(&mut self, serial: u32) {
        if self.ready {
            self.invalidate(false);
        }
        self.serial = serial;
        self.ready = self.active && self.known && self.safe && self.surrounding;
        if self.ready {
            self.initial = false;
        }
    }
    pub fn state(&self) -> Option<(u32, u32)> {
        self.ready.then_some((self.generation, self.serial))
    }
    fn can_commit(&self, state: (u32, u32), text: &str) -> bool {
        self.state() == Some(state)
            && !text.is_empty()
            && text.len() <= 4079
            && !text.contains('\0')
    }
    pub fn phase(&self) -> u32 {
        if self.active && self.initial {
            if !self.known {
                1
            } else if self.safe {
                2
            } else {
                0
            }
        } else {
            0
        }
    }
    pub fn matches(&self, state: (u32, u32), text: &str, cursor: usize) -> bool {
        if self.state() != Some(state)
            || text.len() > MAX_TEXT
            || text.contains('\0')
            || !text.is_char_boundary(cursor)
            || (self.text.is_empty() && !text.is_empty())
        {
            return false;
        }
        let suffix = self.text.len() - self.cursor;
        if self.cursor > cursor || suffix > text.len() - cursor {
            return false;
        }
        self.text[..self.cursor] == text.as_bytes()[cursor - self.cursor..cursor]
            && self.text[self.cursor..] == text.as_bytes()[cursor..cursor + suffix]
    }
}
impl Drop for ContextState {
    fn drop(&mut self) {
        self.clear_text();
    }
}
struct KeyPairs {
    consumed: [bool; MAX_KEY],
}
impl Default for KeyPairs {
    fn default() -> Self {
        Self {
            consumed: [false; MAX_KEY],
        }
    }
}
impl KeyPairs {
    fn prior(&mut self, key: usize, pressed: bool) -> bool {
        let Some(slot) = self.consumed.get_mut(key) else {
            return false;
        };
        let old = *slot;
        if !pressed {
            *slot = false;
        }
        old
    }
    fn consume(&mut self, key: usize) {
        if let Some(slot) = self.consumed.get_mut(key) {
            *slot = true;
        }
    }
    fn clear(&mut self) {
        self.consumed.fill(false);
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct KeyEdge {
    serial: u32,
    time: u32,
    key: u32,
    state: u32,
}
struct PendingInput {
    context: u64,
    authority: (u32, u32),
    key: u32,
    edges: [KeyEdge; 32],
    edge_count: usize,
    released: bool,
    expires_at: Instant,
    valid: bool,
    dispatched: bool,
    requires_edit: bool,
}
impl PendingInput {
    fn new(
        context: u64,
        authority: (u32, u32),
        edge: KeyEdge,
        requires_edit: bool,
        now: Instant,
    ) -> Self {
        let mut pending = Self {
            context,
            authority,
            key: edge.key,
            edges: [KeyEdge::default(); 32],
            edge_count: 0,
            released: false,
            expires_at: now + Duration::from_millis(500),
            valid: true,
            dispatched: false,
            requires_edit,
        };
        pending.hold(edge);
        pending
    }
    fn hold(&mut self, edge: KeyEdge) -> bool {
        // The lease owns ONE physical press/release pair. A later press of the
        // same key is new user input, not part of this pending acceptance.
        if self.released {
            return false;
        }
        if edge.key == self.key && edge.state == 0 {
            self.released = true;
        }
        if !self.valid || edge.key != self.key || self.edge_count == self.edges.len() {
            return false;
        }
        self.edges[self.edge_count] = edge;
        self.edge_count += 1;
        true
    }
    fn drain(&mut self, mut forward: impl FnMut(KeyEdge)) {
        for edge in &self.edges[..self.edge_count] {
            forward(*edge);
        }
        self.edge_count = 0;
    }
    fn valid(&self, context: u64, authority: Option<(u32, u32)>, now: Instant) -> bool {
        self.valid
            && self.context == context
            && Some(self.authority) == authority
            && now < self.expires_at
    }
    fn consumes(&self, accepted: bool, now: Instant) -> bool {
        self.dispatched || (accepted && self.valid && now < self.expires_at && !self.requires_edit)
    }
}
struct PreeditLayout {
    length: u32,
    cursor: i32,
    style: u32,
    reset_commit: &'static CStr,
}
fn preedit_layout(text: &str) -> Option<PreeditLayout> {
    // Protocol offsets are UTF-8 bytes, not Unicode scalar or grapheme indices.
    (!text.is_empty() && text.len() <= 4079 && !text.contains('\0')).then_some(PreeditLayout {
        length: text.len() as u32,
        cursor: 0,
        style: 3,
        reset_commit: c"",
    })
}
fn preedit_scope_valid(
    receipt: &NativePreedit,
    owned: Option<&NativePreedit>,
    state: Option<(u32, u32)>,
    user_revision: u64,
    now: Instant,
) -> bool {
    now < receipt.expires_at
        && receipt.revision == user_revision
        && owned == Some(receipt)
        && state.is_some_and(|state| anchor(state) == receipt.anchor)
}
fn native_choice_index(id: &str, generation: u64, visible: usize) -> Option<usize> {
    let mut fields = id.strip_prefix("native-choice:")?.split(':');
    let offered_generation = fields.next()?.parse::<u64>().ok()?;
    let index = fields.next()?.parse::<usize>().ok()?;
    (fields.next().is_none() && offered_generation == generation && index < visible)
        .then_some(index)
}
struct Mapping {
    pointer: *mut u8,
    size: usize,
}
impl Mapping {
    unsafe fn wipe(&mut self) {
        std::slice::from_raw_parts_mut(self.pointer, self.size).zeroize();
    }
}
impl Drop for Mapping {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.pointer.cast(), self.size);
        }
    }
}
#[derive(Default)]
struct Buffer {
    proxy: Proxy,
    mapping: Option<Mapping>,
    width: i32,
    height: i32,
    stride: i32,
    busy: bool,
    erase: bool,
}
impl Buffer {
    fn release(&mut self) {
        self.busy = false;
        if self.erase {
            if let Some(m) = &mut self.mapping {
                unsafe {
                    m.wipe();
                }
            }
            self.erase = false;
        }
    }
    fn hide(&mut self) {
        self.erase = true;
        if !self.busy {
            self.release();
        }
    }
    fn retire(&mut self) {
        unsafe {
            destroy(&mut self.proxy, Some(0));
            if !self.busy {
                if let Some(m) = &mut self.mapping {
                    m.wipe();
                }
            }
        }
        self.mapping = None;
        self.busy = false;
        self.erase = false;
    }
    fn matches(&self, w: i32, h: i32, s: i32) -> bool {
        !self.proxy.is_null() && self.width == w && self.height == h && self.stride == s
    }
}
impl Drop for Buffer {
    fn drop(&mut self) {
        self.retire();
    }
}
fn valid_image(width: i32, height: i32, stride: i32, scale: i32) -> Option<usize> {
    if !(1..=16384).contains(&width)
        || !(1..=16384).contains(&height)
        || stride < width * 4
        || stride % 4 != 0
        || !(1..=16).contains(&scale)
        || width % scale != 0
        || height % scale != 0
    {
        return None;
    }
    (stride as usize)
        .checked_mul(height as usize)
        .filter(|s| *s <= MAX_IMAGE)
}
pub(super) type UiCallback = Arc<dyn Fn(String) + Send + Sync>;
pub(super) struct Wire {
    display: Proxy,
    registry: Proxy,
    compositor: Proxy,
    shm: Proxy,
    seat: Proxy,
    pointer: Proxy,
    method: Proxy,
    context: Proxy,
    panel_proxy: Proxy,
    panel_surface: Proxy,
    surface: Proxy,
    keyboard: Proxy,
    xkb: Proxy,
    keymap: Proxy,
    keys: Proxy,
    modifiers: u32,
    ids: [u32; 5],
    buffers: [Buffer; 2],
    pairs: KeyPairs,
    pub state: ContextState,
    pub panel: Panel,
    pub failed: bool,
    pub stopped: bool,
    pub wants_write: bool,
    visible: bool,
    pointer_inside: bool,
    pointer_xy: [i32; 2],
    pub policy: ObservationPolicy,
    pub callback: Option<ShortcutHandler>,
    pub enabled: bool,
    pub ui_callback: Option<UiCallback>,
    pub input_callback: Option<Arc<dyn Fn() + Send + Sync>>,
    pub revision: u64,
    pub user_revision: u64,
    pub timestamp: Option<Instant>,
    pub app: String,
    pub domain: String,
    choices: Option<NativeChoices>,
    menu_generation: u64,
    render_generation: u64,
    choices_generation: (u64, u64),
    pub bootstrap_token: Arc<AtomicU64>,
    pub last_bootstrap: (u64, u32),
    bootstrap_identity: (u64, u32),
    pub diagnostic: &'static str,
    preedit: Option<NativePreedit>,
    pending_input: Option<PendingInput>,
    finished_claim_valid: bool,
    #[cfg(test)]
    forwarded: Option<Vec<KeyEdge>>,
}
impl Wire {
    fn allocate_state(policy: ObservationPolicy) -> Box<Self> {
        let mut state = ContextState::default();
        state.generation = 1;
        Box::new(Self {
            display: ptr::null_mut(),
            registry: ptr::null_mut(),
            compositor: ptr::null_mut(),
            shm: ptr::null_mut(),
            seat: ptr::null_mut(),
            pointer: ptr::null_mut(),
            method: ptr::null_mut(),
            context: ptr::null_mut(),
            panel_proxy: ptr::null_mut(),
            panel_surface: ptr::null_mut(),
            surface: ptr::null_mut(),
            keyboard: ptr::null_mut(),
            xkb: ptr::null_mut(),
            keymap: ptr::null_mut(),
            keys: ptr::null_mut(),
            modifiers: 128,
            ids: [0; 5],
            buffers: Default::default(),
            pairs: Default::default(),
            state,
            panel: Default::default(),
            failed: false,
            stopped: false,
            wants_write: false,
            visible: false,
            pointer_inside: false,
            pointer_xy: [0; 2],
            policy,
            callback: None,
            enabled: true,
            ui_callback: None,
            input_callback: None,
            revision: 0,
            user_revision: 0,
            timestamp: None,
            app: String::new(),
            domain: String::new(),
            choices: None,
            menu_generation: 0,
            render_generation: 0,
            choices_generation: (0, 0),
            bootstrap_token: Arc::new(AtomicU64::new(1)),
            last_bootstrap: (0, 0),
            bootstrap_identity: (0, 0),
            diagnostic: "KWin private input method; guarded native preedit is a protocol capability, not a pixel acknowledgment",
            preedit: None,
            pending_input: None,
            finished_claim_valid: true,
            #[cfg(test)]
            forwarded: None,
        })
    }
    pub fn connect(fd: OwnedFd, policy: ObservationPolicy) -> Result<Box<Self>> {
        let mut w = Self::allocate_state(policy);
        unsafe {
            // Ownership is transferred even if libwayland allocation/connection fails. Never consult WAYLAND_DISPLAY.
            w.display = wl_display_connect_to_fd(fd.into_raw_fd());
            if w.display.is_null() {
                bail!("Private KWin input-method connection unavailable");
            }
            w.xkb = xkb_context_new(0);
            if w.xkb.is_null() {
                bail!("XKB context unavailable");
            }
            w.registry = wl_proxy_marshal_flags(
                w.display,
                1,
                &raw const wl_registry_interface,
                1,
                0,
                ptr::null::<c_void>(),
            );
            w.listen(w.registry, REGISTRY)?;
            if wl_display_roundtrip(w.display) < 0
                || w.failed
                || w.method.is_null()
                || w.panel_proxy.is_null()
                || w.compositor.is_null()
                || w.shm.is_null()
            {
                bail!("KWin private input-method globals unavailable");
            }
            if wl_display_roundtrip(w.display) < 0 || w.failed {
                bail!("KWin private input-method initialization failed");
            }
            w.surface = wl_proxy_marshal_flags(
                w.compositor,
                0,
                &raw const wl_surface_interface,
                wl_proxy_get_version(w.compositor),
                0,
                ptr::null::<c_void>(),
            );
            if w.surface.is_null() {
                bail!("KWin input panel surface unavailable");
            }
            w.panel_surface = wl_proxy_marshal_flags(
                w.panel_proxy,
                0,
                &PANEL_SURFACE,
                1,
                0,
                ptr::null::<c_void>(),
                w.surface,
            );
            if w.panel_surface.is_null() {
                bail!("KWin private panel unavailable");
            }
            request0(w.panel_surface, 1);
            w.flush()?;
        }
        Ok(w)
    }
    unsafe fn listen(&mut self, proxy: Proxy, tag: usize) -> Result<()> {
        if proxy.is_null()
            || wl_proxy_add_dispatcher(
                proxy,
                Some(dispatch),
                tag as *const c_void,
                self as *mut Self as *mut c_void,
            ) != 0
        {
            bail!("Wayland event owner unavailable");
        }
        Ok(())
    }
    pub fn fd(&self) -> Option<(i32, bool)> {
        (!self.failed && !self.stopped && !self.display.is_null())
            .then(|| unsafe { (wl_display_get_fd(self.display), self.wants_write) })
    }
    pub fn note(&mut self, user: bool) {
        self.revision = self.revision.wrapping_add(1);
        if user {
            self.user_revision = self.revision;
            self.timestamp = Some(Instant::now());
        }
        if let Some(callback) = &self.input_callback {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback()));
        }
    }
    fn ui(&self, action: &str) {
        if let Some(callback) = &self.ui_callback {
            let _ =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback(action.into())));
        }
    }
    fn changed(&mut self) {
        let identity = (self.state.context_id, self.state.phase());
        if identity != self.bootstrap_identity {
            self.bootstrap_identity = identity;
            self.bootstrap_token.fetch_add(1, Ordering::AcqRel);
        }
        if self
            .preedit
            .as_ref()
            .is_some_and(|p| !self.preedit_valid(p))
        {
            self.clear_preedit();
        }
        if self.pending_input.as_ref().is_some_and(|p| {
            p.valid
                && !p.dispatched
                && !p.valid(self.state.context_id, self.state.state(), Instant::now())
        }) {
            self.reject_pending();
        }
        self.note(true);
        if self.panel.snapshot.as_ref().is_some_and(|s| {
            self.state
                .state()
                .is_none_or(|v| s.native_anchor != anchor(v))
        }) {
            self.hide();
        }
        if self.state.ready {
            self.ui("context_changed");
        }
    }
    pub fn preedit_valid(&self, receipt: &NativePreedit) -> bool {
        !self.failed
            && !self.stopped
            && preedit_scope_valid(
                receipt,
                self.preedit.as_ref(),
                self.state.state(),
                self.user_revision,
                Instant::now(),
            )
    }
    pub fn owns_preedit(&self) -> bool {
        self.preedit.as_ref().is_some_and(|p| self.preedit_valid(p))
    }
    pub fn choices_valid(&self, receipt: &NativeChoices) -> bool {
        !self.failed
            && !self.stopped
            && self.visible
            && !self.panel.pending
            && self.panel.choice_mode
            && self.choices.as_ref() == Some(receipt)
            && self.choices_generation == (self.menu_generation, self.render_generation)
            && receipt.revision == self.user_revision
            && Instant::now() < receipt.expires_at
            && self
                .state
                .state()
                .is_some_and(|state| anchor(state) == receipt.anchor)
            && receipt.values.len() == self.panel.visible_choices
            && self.panel.menu.as_ref().is_some_and(|rows| {
                rows.iter()
                    .take(receipt.values.len())
                    .map(|row| &row.label)
                    .eq(receipt.values.iter())
            })
    }
    fn clear_choices(&mut self) {
        if let Some(mut receipt) = self.choices.take() {
            for text in &mut receipt.values {
                text.zeroize();
            }
        }
        self.menu_generation = self.menu_generation.wrapping_add(1).max(1);
    }
    pub fn hide_choices(&mut self) {
        self.clear_choices();
        self.panel.clear();
        self.hide_surface();
    }
    pub fn present_choices(
        &mut self,
        snapshot: crate::core::Snapshot,
        values: &[String],
    ) -> Result<Option<NativeChoices>> {
        if self.failed
            || self.stopped
            || values.is_empty()
            || values.len() > 32
            || values.iter().any(|v| v.is_empty() || v.contains('\0'))
            || values.iter().map(String::len).sum::<usize>() > 65536
            || self
                .state
                .state()
                .is_none_or(|state| anchor(state) != snapshot.native_anchor)
        {
            self.hide();
            return Ok(None);
        }
        if let Some(receipt) = &self.choices {
            if receipt.anchor == snapshot.native_anchor
                && self
                    .panel
                    .menu
                    .as_ref()
                    .is_some_and(|rows| rows.iter().map(|row| &row.label).eq(values.iter()))
                && self.choices_valid(receipt)
            {
                return Ok(Some(receipt.clone()));
            }
        }
        self.hide_choices();
        self.panel.snapshot = Some(snapshot);
        self.panel.choice_mode = true;
        self.panel.menu = Some(
            values
                .iter()
                .enumerate()
                .map(|(index, value)| MenuItem {
                    action: format!("native-choice:{}:{index}", self.menu_generation),
                    label: value.clone(),
                    enabled: true,
                })
                .collect(),
        );
        self.panel.selected = Some(0);
        if !self.render()? || self.panel.pending || self.panel.visible_choices == 0 {
            self.hide_choices();
            return Ok(None);
        }
        let receipt = NativeChoices {
            anchor: self
                .panel
                .snapshot
                .as_ref()
                .expect("owned choice snapshot")
                .native_anchor
                .clone(),
            values: values[..self.panel.visible_choices].to_vec(),
            revision: self.user_revision,
            expires_at: Instant::now() + Duration::from_secs(2),
        };
        self.choices_generation = (self.menu_generation, self.render_generation);
        self.choices = Some(receipt.clone());
        Ok(Some(receipt))
    }
    fn choice_action(&self, id: &str) -> Option<Action> {
        let receipt = self
            .choices
            .as_ref()
            .filter(|receipt| self.choices_valid(receipt))?;
        native_choice_index(id, self.menu_generation, receipt.values.len()).map(Action::Choice)
    }
    fn activate_choice(&mut self, id: &str) {
        let accepted = self.choice_action(id).is_some_and(|action| {
            self.callback.as_ref().is_some_and(|callback| {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback(action)))
                    .unwrap_or(false)
            })
        });
        if !accepted {
            self.hide();
        }
    }
    pub fn present_preedit(
        &mut self,
        state: (u32, u32),
        text: &str,
    ) -> Result<Option<NativePreedit>> {
        let Some(layout) = preedit_layout(text) else {
            self.clear_preedit();
            return Ok(None);
        };
        if self.failed
            || self.stopped
            || self.context.is_null()
            || !self.state.can_commit(state, text)
        {
            self.clear_preedit();
            return Ok(None);
        }
        if let Some(receipt) = &self.preedit {
            if receipt.anchor == anchor(state)
                && receipt.text == text
                && self.preedit_valid(receipt)
            {
                return Ok(Some(receipt.clone()));
            }
        }
        let text_c = CString::new(text)?;
        self.clear_preedit();
        unsafe {
            // INACTIVE styling distinguishes prediction from committed input. Cursor stays
            // at the insertion point; the reset-commit string MUST remain empty.
            wl_proxy_marshal_flags(
                self.context,
                3,
                ptr::null(),
                1,
                0,
                0u32,
                layout.length,
                layout.style,
            );
            wl_proxy_marshal_flags(self.context, 4, ptr::null(), 1, 0, layout.cursor);
            wl_proxy_marshal_flags(
                self.context,
                2,
                ptr::null(),
                1,
                0,
                state.1,
                text_c.as_ptr(),
                layout.reset_commit.as_ptr(),
            );
        }
        self.flush()?;
        let receipt = NativePreedit {
            anchor: anchor(state),
            text: text.into(),
            revision: self.user_revision,
            expires_at: Instant::now() + Duration::from_secs(2),
        };
        self.preedit = Some(receipt.clone());
        Ok(Some(receipt))
    }
    fn clear_preedit(&mut self) {
        let Some(mut owned) = self.preedit.take() else {
            return;
        };
        owned.text.zeroize();
        if !self.context.is_null() {
            unsafe {
                wl_proxy_marshal_flags(self.context, 4, ptr::null(), 1, 0, 0i32);
                wl_proxy_marshal_flags(
                    self.context,
                    2,
                    ptr::null(),
                    1,
                    0,
                    self.state.serial,
                    c"".as_ptr(),
                    c"".as_ptr(),
                );
            }
        }
    }
    pub fn set_shortcuts_enabled(&mut self, enabled: bool) {
        self.enabled = enabled && !self.stopped && !self.failed;
        self.panel.pressed = None;
        if !self.enabled {
            self.clear_preedit();
            self.clear_choices();
            if self.panel.choice_mode {
                // A manual accessory click suspends shortcuts before requesting its
                // menu. Revoke predictions, but retain the origin for fresh recapture.
                let origin = self.panel.snapshot.take();
                self.panel.clear();
                self.panel.snapshot = origin;
                self.hide_surface();
            }
        }
    }
    pub fn input_claim_valid(&self) -> bool {
        self.finished_claim_valid
            && self
                .pending_input
                .as_ref()
                .is_none_or(|p| p.valid(self.state.context_id, self.state.state(), Instant::now()))
    }
    unsafe fn forward_edge(&mut self, edge: KeyEdge) {
        #[cfg(test)]
        if let Some(forwarded) = &mut self.forwarded {
            forwarded.push(edge);
            return;
        }
        if !self.context.is_null() && !self.failed {
            wl_proxy_marshal_flags(
                self.context,
                10,
                ptr::null(),
                1,
                0,
                edge.serial,
                edge.time,
                edge.key,
                edge.state,
            );
        }
    }
    fn reject_pending(&mut self) {
        let Some(mut pending) = self.pending_input.take() else {
            return;
        };
        pending.valid = false;
        self.finished_claim_valid = false;
        if pending.dispatched {
            if !pending.released {
                self.pairs.consume(pending.key as usize);
            }
        } else if pending.context == self.state.context_id {
            pending.drain(|edge| unsafe {
                self.forward_edge(edge);
            });
        }
        pending.edge_count = 0;
        self.pending_input = Some(pending);
    }
    pub fn finish_input(&mut self, accepted: bool) -> Result<()> {
        if let Some(mut pending) = self.pending_input.take() {
            if pending.consumes(accepted, Instant::now()) {
                if !pending.released {
                    self.pairs.consume(pending.key as usize);
                }
            } else if pending.context == self.state.context_id {
                pending.drain(|edge| unsafe {
                    self.forward_edge(edge);
                });
            }
        }
        self.finished_claim_valid = true;
        self.flush()
    }
    pub fn hide(&mut self) {
        self.clear_choices();
        self.clear_preedit();
        let had_menu = self.panel.clear();
        self.hide_surface();
        if had_menu {
            self.ui("menu_closed");
        }
    }
    fn hide_surface(&mut self) {
        unsafe {
            if self.visible && !self.surface.is_null() {
                wl_proxy_marshal_flags(
                    self.surface,
                    1,
                    ptr::null(),
                    wl_proxy_get_version(self.surface),
                    0,
                    ptr::null::<c_void>(),
                    0i32,
                    0i32,
                );
                request0(self.surface, 6);
            }
        }
        self.visible = false;
        self.pointer_inside = false;
        for buffer in &mut self.buffers {
            buffer.hide();
        }
    }
    fn invalidate(&mut self, erase: bool) {
        self.clear_preedit();
        self.state.invalidate(erase);
        self.hide_surface();
    }
    // KWin owns a release-capable keyboard resource even though the v1 constructor
    // proxy inherits version 1. Send release (opcode 0); local destroy leaves its grab alive.
    unsafe fn drop_keyboard(&mut self) {
        destroy(&mut self.keyboard, Some(0));
        if !self.keys.is_null() {
            xkb_state_unref(self.keys);
            self.keys = ptr::null_mut();
        }
        if !self.keymap.is_null() {
            xkb_keymap_unref(self.keymap);
            self.keymap = ptr::null_mut();
        }
        self.modifiers = 128;
        self.pairs.clear();
    }
    unsafe fn release_context(&mut self) {
        self.reject_pending();
        self.hide();
        self.state.release();
        self.drop_keyboard();
        destroy(&mut self.context, Some(0));
    }
    fn fail(&mut self) {
        if self.failed {
            return;
        }
        self.failed = true;
        self.bootstrap_token.fetch_add(1, Ordering::AcqRel);
        self.callback = None;
        self.enabled = false;
        self.diagnostic =
            "The KWin private input-method connection closed; native typing is unavailable";
        unsafe {
            self.release_context();
            if !self.display.is_null() {
                wl_display_flush(self.display);
            }
        }
        self.note(true);
        self.ui("connection_lost");
    }
    pub fn flush(&mut self) -> Result<()> {
        if self.failed || self.stopped {
            bail!("Private KWin connection is unavailable");
        }
        #[cfg(test)]
        if self.forwarded.is_some() {
            return Ok(());
        }
        unsafe {
            if wl_display_flush(self.display) >= 0 {
                self.wants_write = false;
                return Ok(());
            }
        }
        if std::io::Error::last_os_error().raw_os_error() == Some(libc::EAGAIN) {
            self.wants_write = true;
            Ok(())
        } else {
            self.fail();
            bail!("Private KWin output failed")
        }
    }
    pub fn pump(&mut self) -> Result<()> {
        if self.failed || self.stopped {
            bail!("Private KWin connection is unavailable");
        }
        if self
            .preedit
            .as_ref()
            .is_some_and(|p| !self.preedit_valid(p))
        {
            self.clear_preedit();
            self.note(false);
        }
        if self
            .choices
            .as_ref()
            .is_some_and(|receipt| !self.choices_valid(receipt))
        {
            self.hide();
            self.note(false);
        }
        if self.pending_input.as_ref().is_some_and(|p| {
            p.valid
                && !p.dispatched
                && !p.valid(self.state.context_id, self.state.state(), Instant::now())
        }) {
            self.reject_pending();
            self.hide();
            self.note(true);
        }
        unsafe {
            while wl_display_prepare_read(self.display) != 0 {
                if wl_display_dispatch_pending(self.display) < 0 || self.failed {
                    self.fail();
                    bail!("Private KWin dispatch failed");
                }
            }
            let mut poll = libc::pollfd {
                fd: wl_display_get_fd(self.display),
                events: libc::POLLIN,
                revents: 0,
            };
            let result = loop {
                let r = libc::poll(&mut poll, 1, 0);
                if r >= 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                    break r;
                }
            };
            if result < 0 || poll.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                wl_display_cancel_read(self.display);
                self.fail();
                bail!("Private KWin connection disconnected");
            }
            if poll.revents & libc::POLLIN != 0 {
                if wl_display_read_events(self.display) < 0 {
                    self.fail();
                    bail!("Private KWin read failed");
                }
            } else {
                wl_display_cancel_read(self.display);
            }
            if wl_display_dispatch_pending(self.display) < 0 || self.failed {
                self.fail();
                bail!("Private KWin dispatch failed");
            }
        }
        self.flush()
    }
    pub fn commit(&mut self, state: (u32, u32), text: &str) -> Result<bool> {
        if self.failed
            || self.stopped
            || !self.state.can_commit(state, text)
            || !self.input_claim_valid()
        {
            return Ok(false);
        }
        let text = CString::new(text)?;
        self.hide();
        unsafe {
            if let Some(pending) = &mut self.pending_input {
                pending.dispatched = true;
            }
            wl_proxy_marshal_flags(self.context, 1, ptr::null(), 1, 0, state.1, text.as_ptr());
        }
        self.invalidate(true);
        self.changed();
        // A flush error after marshaling is indeterminate, never a safe automatic retry.
        self.flush()?;
        Ok(true)
    }
    pub fn render(&mut self) -> Result<bool> {
        if self.failed
            || self.stopped
            || self.panel.snapshot.as_ref().is_none_or(|s| {
                self.state
                    .state()
                    .is_none_or(|v| s.native_anchor != anchor(v))
            })
        {
            return Ok(false);
        }
        let mut frame = self.panel.frame()?;
        if self.panel.choice_mode && frame.choice_count == 0 {
            return Ok(false);
        }
        let width = frame.image.width();
        let height = frame.image.height();
        let stride = frame.image.stride();
        let Some(size) = valid_image(width, height, stride, frame.scale) else {
            return Ok(false);
        };
        if frame.scale != 1 && unsafe { wl_proxy_get_version(self.surface) } < 3 {
            return Ok(false);
        }
        let slot = self
            .buffers
            .iter()
            .position(|b| !b.busy && b.matches(width, height, stride))
            .or_else(|| self.buffers.iter().rposition(|b| !b.busy));
        let Some(slot) = slot else {
            self.panel.pending = true;
            self.panel.regions.clear();
            self.panel.pressed = None;
            return Ok(true);
        };
        if !self.buffers[slot].matches(width, height, stride) {
            self.allocate(slot, width, height, stride, size)?;
        }
        let b = &mut self.buffers[slot];
        let pixels = frame.image.data()?;
        let Some(mapping) = &mut b.mapping else {
            return Ok(false);
        };
        unsafe {
            ptr::copy_nonoverlapping(pixels.as_ptr(), mapping.pointer, size);
            b.erase = false;
            let version = wl_proxy_get_version(self.surface);
            if version >= 3 {
                wl_proxy_marshal_flags(self.surface, 8, ptr::null(), version, 0, frame.scale);
            }
            wl_proxy_marshal_flags(
                self.surface,
                1,
                ptr::null(),
                version,
                0,
                b.proxy,
                0i32,
                0i32,
            );
            wl_proxy_marshal_flags(
                self.surface,
                2,
                ptr::null(),
                version,
                0,
                0i32,
                0i32,
                width / frame.scale,
                height / frame.scale,
            );
            b.busy = true;
            self.visible = true;
            request0(self.surface, 6);
        }
        self.panel.pending = false;
        self.render_generation = self.render_generation.wrapping_add(1).max(1);
        self.panel.visible_choices = frame.choice_count;
        if self.choices.as_ref().is_some_and(|receipt| {
            receipt.values.len() == frame.choice_count
                && self.panel.menu.as_ref().is_some_and(|rows| {
                    rows.iter()
                        .take(frame.choice_count)
                        .map(|row| &row.label)
                        .eq(receipt.values.iter())
                })
        }) {
            self.choices_generation = (self.menu_generation, self.render_generation);
        }
        self.panel.regions = std::mem::take(&mut frame.regions);
        self.flush()?;
        Ok(true)
    }
    fn allocate(
        &mut self,
        slot: usize,
        width: i32,
        height: i32,
        stride: i32,
        size: usize,
    ) -> Result<()> {
        unsafe {
            self.buffers[slot].retire();
            let fd = libc::memfd_create(c"inklow-input-panel".as_ptr(), libc::MFD_CLOEXEC);
            if fd < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            let fd = OwnedFd::from_raw_fd(fd);
            if libc::ftruncate(fd.as_raw_fd(), size as libc::off_t) != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            let pointer = libc::mmap(
                ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            );
            if pointer == libc::MAP_FAILED {
                return Err(std::io::Error::last_os_error().into());
            }
            let mapping = Mapping {
                pointer: pointer.cast(),
                size,
            };
            let mut pool = wl_proxy_marshal_flags(
                self.shm,
                0,
                &raw const wl_shm_pool_interface,
                1,
                0,
                ptr::null::<c_void>(),
                fd.as_raw_fd(),
                size as i32,
            );
            if pool.is_null() {
                bail!("KWin SHM pool unavailable");
            }
            let proxy = wl_proxy_marshal_flags(
                pool,
                0,
                &raw const wl_buffer_interface,
                1,
                0,
                ptr::null::<c_void>(),
                0i32,
                width,
                height,
                stride,
                0u32,
            );
            destroy(&mut pool, Some(1));
            if proxy.is_null() {
                bail!("KWin SHM buffer unavailable");
            }
            if let Err(error) = self.listen(proxy, BUFFER) {
                wl_proxy_destroy(proxy);
                return Err(error);
            }
            self.buffers[slot] = Buffer {
                proxy,
                mapping: Some(mapping),
                width,
                height,
                stride,
                busy: false,
                erase: false,
            };
            Ok(())
        }
    }
    fn prepare_menu(&mut self, actions: Vec<(String, String, bool)>) {
        self.clear_preedit();
        self.clear_choices();
        if self.panel.choice_mode {
            let snapshot = self.panel.snapshot.take();
            self.panel.clear();
            self.panel.snapshot = snapshot;
        }
        self.panel.choice_mode = false;
        self.panel.menu = Some(
            actions
                .into_iter()
                .map(|(action, label, enabled)| MenuItem {
                    action,
                    label,
                    enabled,
                })
                .collect(),
        );
        self.panel.page = 0;
        self.panel.selected = self
            .panel
            .menu
            .as_ref()
            .and_then(|m| m.iter().position(|i| i.enabled));
        self.panel.pressed = None;
    }
    pub fn show_menu(&mut self, actions: Vec<(String, String, bool)>) -> Result<bool> {
        self.prepare_menu(actions);
        self.render()
    }
    fn render_or_hide(&mut self) {
        if !matches!(self.render(), Ok(true)) {
            self.hide();
        }
    }
    fn menu_key(&mut self, key: &str, modifiers: u32) -> bool {
        if self.panel.menu.is_none() || modifiers != 0 {
            return false;
        }
        if key == "Escape" {
            if self.panel.choice_mode {
                return false;
            }
            self.panel.menu = None;
            self.panel.pressed = None;
            self.render_or_hide();
            self.ui("menu_closed");
            return true;
        }
        if self.panel.navigate(key) {
            self.render_or_hide();
            return true;
        }
        if key == "Return" {
            let action = self
                .panel
                .selected
                .and_then(|i| self.panel.menu.as_ref()?.get(i))
                .filter(|i| i.enabled)
                .map(|i| i.action.clone());
            if let Some(action) = action {
                self.panel.menu = None;
                self.hide();
                self.ui(&action);
            }
            return true;
        }
        false
    }
    fn pointer_button(&mut self, button: u32, state: u32) {
        if button != 272
            || !self.pointer_inside
            || !self.visible
            || !self.state.ready
            || self.failed
        {
            return;
        }
        let Some(snapshot) = &self.panel.snapshot else {
            return;
        };
        let expected = snapshot.native_anchor.clone();
        if self.state.state().is_none_or(|s| anchor(s) != expected) {
            self.hide();
            return;
        }
        let Some(action) =
            self.panel
                .pointer_action(&expected, self.pointer_xy[0], self.pointer_xy[1], state)
        else {
            return;
        };
        if self.panel.choice_mode {
            self.activate_choice(&action);
            return;
        }
        if self.panel.menu.is_some() {
            match action.as_str() {
                "page:previous" => {
                    self.panel.page = self.panel.page.saturating_sub(1);
                    self.panel.selected = None;
                    self.render_or_hide();
                }
                "page:next" => {
                    self.panel.page += 1;
                    self.panel.selected = None;
                    self.render_or_hide();
                }
                "menu" => {
                    self.panel.menu = None;
                    self.render_or_hide();
                    self.ui("menu_closed");
                }
                _ => {
                    self.panel.menu = None;
                    self.hide();
                    self.ui(&action);
                }
            }
        } else if action == "menu" {
            self.ui("menu");
        }
    }
    unsafe fn keymap(&mut self, format: u32, fd: i32, size: u32) {
        let fd = OwnedFd::from_raw_fd(fd);
        let mut map = ptr::null_mut();
        let mut info: libc::stat = std::mem::zeroed();
        if format == 1
            && size > 0
            && size as usize <= MAX_TEXT
            && libc::fstat(fd.as_raw_fd(), &mut info) == 0
            && info.st_size >= size as i64
        {
            let bytes = libc::mmap(
                ptr::null_mut(),
                size as usize,
                libc::PROT_READ,
                libc::MAP_PRIVATE,
                fd.as_raw_fd(),
                0,
            );
            if bytes != libc::MAP_FAILED {
                if *(bytes.cast::<u8>().add(size as usize - 1)) == 0 {
                    map = xkb_keymap_new_from_string(self.xkb, bytes.cast(), 1, 0);
                }
                libc::munmap(bytes, size as usize);
            }
        }
        let state = if !map.is_null() {
            xkb_state_new(map)
        } else {
            ptr::null_mut()
        };
        if !self.keys.is_null() {
            xkb_state_unref(self.keys);
        }
        if !self.keymap.is_null() {
            xkb_keymap_unref(self.keymap);
        }
        self.keys = state;
        self.keymap = map;
        self.modifiers = self.canonical_modifiers();
        self.reject_pending();
        self.hide();
        self.note(true);
    }
    unsafe fn canonical_modifiers(&self) -> u32 {
        if self.keys.is_null() || self.keymap.is_null() {
            return 128;
        }
        let mut result = 0;
        let mut canonical = 0;
        const EFFECTIVE: u32 = 1 << 3;
        for (name, bit) in [(c"Shift", 1), (c"Control", 4), (c"Mod1", 8), (c"Mod4", 64)] {
            let index = xkb_keymap_mod_get_index(self.keymap, name.as_ptr());
            if index < 32 {
                canonical |= 1 << index;
                if xkb_state_mod_index_is_active(self.keys, index, EFFECTIVE) > 0 {
                    result |= bit;
                }
            }
        }
        for name in [c"LevelThree", c"LevelFive", c"AltGr"] {
            if xkb_state_mod_name_is_active(self.keys, name.as_ptr(), EFFECTIVE) > 0 {
                result |= 128;
            }
        }
        for name in [c"Lock", c"Mod2"] {
            let index = xkb_keymap_mod_get_index(self.keymap, name.as_ptr());
            if index < 32 {
                canonical |= 1 << index;
            }
        }
        if (xkb_state_serialize_mods(self.keys, EFFECTIVE) & 0xff) & !canonical != 0 {
            result |= 128;
        }
        for i in 8..xkb_keymap_num_mods(self.keymap) {
            if xkb_state_mod_index_is_active(self.keys, i, EFFECTIVE) <= 0 {
                continue;
            }
            let name = xkb_keymap_mod_get_name(self.keymap, i);
            if name.is_null()
                || !matches!(
                    CStr::from_ptr(name).to_bytes(),
                    b"Alt" | b"Meta" | b"Super" | b"NumLock" | b"Control" | b"Shift"
                )
            {
                result |= 128;
            }
        }
        result
    }
    fn recognize_key(&mut self, key_name: &str, edge: KeyEdge) -> bool {
        if !self.state.ready {
            return false;
        }
        let handled = if self.panel.menu.is_some()
            && (!self.panel.choice_mode || matches!(key_name, "Up" | "Down" | "Home" | "End"))
        {
            self.menu_key(key_name, self.modifiers)
        } else if self.enabled
            && self.finished_claim_valid
            && self.pending_input.is_none()
            && self.modifiers & !77 == 0
        {
            let mut mods = [""; 4];
            let mut count = 0;
            for (bit, name) in [(4, "Ctrl"), (8, "Alt"), (1, "Shift"), (64, "Super")] {
                if self.modifiers & bit != 0 {
                    mods[count] = name;
                    count += 1;
                }
            }
            let action = if self.panel.choice_mode && key_name == "Return" && self.modifiers == 0 {
                self.panel
                    .selected
                    .and_then(|index| self.panel.menu.as_ref()?.get(index))
                    .and_then(|row| self.choice_action(&row.action))
            } else {
                self.policy
                    .key_action(key_name, &mods[..count], "linux", &self.app, &self.domain)
            };
            if let Some(action) = action {
                let requires_edit =
                    matches!(action, Action::Word | Action::All | Action::Choice(_));
                let handled = self.callback.as_ref().is_some_and(|callback| {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback(action)))
                        .unwrap_or(false)
                });
                if handled {
                    self.pending_input = Some(PendingInput::new(
                        self.state.context_id,
                        self.state.state().expect("ready keyboard context"),
                        edge,
                        requires_edit,
                        Instant::now(),
                    ));
                }
                handled
            } else {
                false
            }
        } else {
            false
        };
        if handled && self.pending_input.is_none() {
            self.pairs.consume(edge.key as usize);
        }
        handled
    }
    unsafe fn keyboard_key(&mut self, serial: u32, time: u32, key: u32, state: u32) {
        let context = self.context;
        if self.failed {
            return;
        }
        if context.is_null() {
            #[cfg(test)]
            if self.forwarded.is_none() {
                return;
            }
            #[cfg(not(test))]
            return;
        }
        let edge = KeyEdge {
            serial,
            time,
            key,
            state,
        };
        if let Some(pending) = &mut self.pending_input {
            if pending.hold(edge) {
                return;
            }
            self.reject_pending();
        }
        let pressed = state == 1;
        // KWin can expose compositor-repeat (state 2) even on the v1-created
        // grab. Only state 0 releases a consumed hardware pair.
        let mut consumed = self.pairs.prior(key as usize, state != 0);
        let mut sym = if !self.keys.is_null() && (key as usize) < MAX_KEY {
            xkb_state_key_get_one_sym(self.keys, key + 8)
        } else {
            0
        };
        if self.state.ready
            && !self.keys.is_null()
            && (key as usize) < MAX_KEY
            && !consumed
            && pressed
        {
            let layout = xkb_state_key_get_layout(self.keys, key + 8);
            let mut base: *const u32 = ptr::null();
            if layout != u32::MAX
                && xkb_keymap_key_get_syms_by_level(self.keymap, key + 8, layout, 0, &mut base) == 1
                && !base.is_null()
                && matches!(*base,0x30..=0x39|0x41..=0x5a|0x61..=0x7a)
            {
                sym = *base;
            }
            let mut name = [0i8; 128];
            if xkb_keysym_get_name(sym, name.as_mut_ptr(), name.len()) > 0 {
                let raw = CStr::from_ptr(name.as_ptr()).to_str().unwrap_or("");
                let mut one = [0u8; 1];
                let key_name = normalize_key(raw, key + 8, &mut one);
                if self.recognize_key(key_name, edge) {
                    consumed = true;
                }
            }
        }
        if !consumed && self.context == context && !self.failed {
            // Recognition must precede revocation: a held acceptance key preserves its
            // armed claim, every ordinary edge revokes before reaching the application.
            if state != 0 && !modifier_keysym(sym) {
                self.invalidate(true);
                self.changed();
            } else {
                if self.panel.choice_mode {
                    self.hide();
                } else {
                    self.clear_preedit();
                }
                self.note(true);
            }
            self.forward_edge(edge);
        }
    }
    unsafe fn event(
        &mut self,
        tag: usize,
        target: Proxy,
        opcode: u32,
        args: *mut Argument,
    ) -> Result<()> {
        let u = |i: usize| (*args.add(i)).u;
        let o = |i: usize| (*args.add(i)).o;
        match tag {
            REGISTRY => match opcode {
                0 => {
                    let name = u(0);
                    let interface = CStr::from_ptr((*args.add(1)).s).to_bytes();
                    let version = u(2);
                    let (slot, descriptor, cap, tag) = match interface {
                        b"zwp_input_method_v1" => (0, &METHOD as *const Interface, 1, METHOD_TAG),
                        b"zwp_input_panel_v1" => (1, &PANEL as *const Interface, 1, 0),
                        b"wl_compositor" => (2, &raw const wl_compositor_interface, 4, 0),
                        b"wl_shm" => (3, &raw const wl_shm_interface, 1, SHM),
                        b"wl_seat" => (4, &raw const wl_seat_interface, 5, SEAT),
                        _ => return Ok(()),
                    };
                    if self.ids[slot] != 0 {
                        return Ok(());
                    }
                    let proxy = wl_proxy_marshal_flags(
                        self.registry,
                        0,
                        descriptor,
                        version.min(cap),
                        0,
                        name,
                        (*descriptor).name,
                        version.min(cap),
                        ptr::null::<c_void>(),
                    );
                    if proxy.is_null() {
                        bail!("Private KWin global binding failed");
                    }
                    self.ids[slot] = name;
                    match slot {
                        0 => self.method = proxy,
                        1 => self.panel_proxy = proxy,
                        2 => self.compositor = proxy,
                        3 => self.shm = proxy,
                        4 => self.seat = proxy,
                        _ => unreachable!(),
                    };
                    if tag != 0 {
                        self.listen(proxy, tag)?;
                    }
                }
                1 => {
                    if self.ids.contains(&u(0)) {
                        self.fail();
                    }
                }
                _ => {}
            },
            METHOD_TAG => match opcode {
                0 => {
                    // Retire the OLD owned preedit/held key against its own context
                    // before changing the generation or installing the new object.
                    self.release_context();
                    self.state.activate();
                    self.context = o(0);
                    self.listen(self.context, CONTEXT_TAG)?;
                    self.changed();
                }
                1 => {
                    if o(0) == self.context {
                        self.release_context();
                        self.changed();
                    }
                }
                _ => {}
            },
            CONTEXT_TAG if target == self.context => {
                match opcode {
                    0 => {
                        let text = (*args).s;
                        self.hide_surface();
                        if self.state.known && self.state.safe && !text.is_null() {
                            let len = libc::strnlen(text, MAX_TEXT + 1);
                            if len <= MAX_TEXT {
                                self.state.surrounding(
                                    std::slice::from_raw_parts(text.cast(), len),
                                    u(1) as usize,
                                    u(2) as usize,
                                );
                            } else {
                                self.state.invalidate(true);
                            }
                        } else {
                            self.state.invalidate(true);
                        }
                    }
                    1 => self.invalidate(true),
                    2 => {
                        self.state.content(u(0), u(1));
                        self.hide_surface();
                        if self.state.safe && self.keyboard.is_null() {
                            self.keyboard = wl_proxy_marshal_flags(
                                self.context,
                                9,
                                &raw const wl_keyboard_interface,
                                1,
                                0,
                                ptr::null::<c_void>(),
                            );
                            self.listen(self.keyboard, KEYBOARD)?;
                        }
                    }
                    3 => self.invalidate(true),
                    4 => {
                        self.state.commit_state(u(0));
                        if !self.state.ready {
                            self.hide_surface();
                        }
                    }
                    _ => return Ok(()),
                }
                self.changed();
            }
            SEAT if opcode == 0 => {
                if u(0) & 1 != 0 && self.pointer.is_null() {
                    self.pointer = wl_proxy_marshal_flags(
                        self.seat,
                        0,
                        &raw const wl_pointer_interface,
                        wl_proxy_get_version(self.seat),
                        0,
                        ptr::null::<c_void>(),
                    );
                    self.listen(self.pointer, POINTER)?;
                } else if u(0) & 1 == 0 && !self.pointer.is_null() {
                    let release = (wl_proxy_get_version(self.pointer) >= 3).then_some(1);
                    destroy(&mut self.pointer, release);
                    self.pointer_inside = false;
                }
            }
            POINTER => match opcode {
                0 => {
                    self.pointer_inside = o(1) == self.surface && self.visible && self.state.ready;
                    self.pointer_xy = [(*args.add(2)).i / 256, (*args.add(3)).i / 256];
                }
                1 => self.pointer_inside = false,
                2 => self.pointer_xy = [(*args.add(1)).i / 256, (*args.add(2)).i / 256],
                3 => self.pointer_button(u(2), u(3)),
                _ => {}
            },
            KEYBOARD => match opcode {
                0 => self.keymap(u(0), (*args.add(1)).h, u(2)),
                1 => {
                    self.reject_pending();
                    self.hide();
                    self.pairs.clear();
                    self.note(true);
                }
                2 => {
                    self.invalidate(true);
                    self.pairs.clear();
                    self.changed();
                }
                3 => self.keyboard_key(u(0), u(1), u(2), u(3)),
                4 => {
                    // Modifier changes must follow any rejected held key, never overtake it.
                    self.reject_pending();
                    if self.panel.choice_mode {
                        self.hide();
                    } else {
                        self.clear_preedit();
                    }
                    self.note(true);
                    if !self.keys.is_null() {
                        xkb_state_update_mask(self.keys, u(1), u(2), u(3), 0, 0, u(4));
                    }
                    self.modifiers = self.canonical_modifiers();
                    if !self.context.is_null() && !self.failed {
                        wl_proxy_marshal_flags(
                            self.context,
                            11,
                            ptr::null(),
                            1,
                            0,
                            u(0),
                            u(1),
                            u(2),
                            u(3),
                            u(4),
                        );
                    }
                }
                _ => {}
            },
            BUFFER if opcode == 0 => {
                if let Some(buffer) = self.buffers.iter_mut().find(|b| b.proxy == target) {
                    buffer.release();
                }
                if self.panel.pending && self.state.ready && !self.failed && !self.render()? {
                    self.hide();
                }
            }
            SHM => {}
            _ => {}
        }
        Ok(())
    }
    pub fn close(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        self.bootstrap_token.fetch_add(1, Ordering::AcqRel);
        self.callback = None;
        self.ui_callback = None;
        self.input_callback = None;
        unsafe {
            self.release_context();
            if !self.pointer.is_null() {
                let release = (wl_proxy_get_version(self.pointer) >= 3).then_some(1);
                destroy(&mut self.pointer, release);
            }
            destroy(&mut self.panel_surface, None);
            destroy(&mut self.surface, Some(0));
            for buffer in &mut self.buffers {
                buffer.retire();
            }
            destroy(&mut self.panel_proxy, None);
            destroy(&mut self.method, None);
            if !self.seat.is_null() {
                let release = (wl_proxy_get_version(self.seat) >= 5).then_some(3);
                destroy(&mut self.seat, release);
            }
            destroy(&mut self.shm, None);
            destroy(&mut self.compositor, None);
            destroy(&mut self.registry, None);
            if !self.xkb.is_null() {
                xkb_context_unref(self.xkb);
                self.xkb = ptr::null_mut();
            }
            if !self.display.is_null() {
                wl_display_flush(self.display);
                wl_display_disconnect(self.display);
                self.display = ptr::null_mut();
            }
        }
    }
}
impl Drop for Wire {
    fn drop(&mut self) {
        self.close();
    }
}
unsafe extern "C" fn dispatch(
    implementation: *const c_void,
    target: Proxy,
    opcode: u32,
    _message: *const Message,
    args: *mut Argument,
) -> c_int {
    let owner = wl_proxy_get_user_data(target).cast::<Wire>();
    if owner.is_null() {
        return -1;
    }
    // Destruction is never called from a consumer callback; only owner pump/stop may retire the Box.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        (*owner).event(implementation as usize, target, opcode, args)
    }));
    if !matches!(result, Ok(Ok(()))) {
        (*owner).fail();
        return -1;
    }
    0
}
pub(super) fn anchor(state: (u32, u32)) -> String {
    format!("{}:{}", state.0, state.1)
}
fn normalize_key<'a>(name: &'a str, key: u32, one: &'a mut [u8; 1]) -> &'a str {
    if key == 49 {
        return "AboveTab";
    }
    match name {
        "ISO_Left_Tab" => "Tab",
        "space" => "Space",
        "KP_Enter" => "Return",
        _ if name.len() == 1 && name.as_bytes()[0].is_ascii_alphanumeric() => {
            one[0] = name.as_bytes()[0].to_ascii_uppercase();
            std::str::from_utf8(one).unwrap_or("")
        }
        "Tab" | "Escape" | "Space" | "Return" | "Up" | "Down" | "Home" | "End" | "F1" | "F2"
        | "F3" | "F4" | "F5" | "F6" | "F7" | "F8" | "F9" | "F10" | "F11" | "F12" => name,
        _ => "",
    }
}
fn modifier_keysym(sym: u32) -> bool {
    matches!(sym, 0xffe1..=0xffee | 0xfe03 | 0xfe11 | 0xff7e | 0xff7f)
}
#[cfg(test)]
mod tests {
    use super::*;
    // Exercise the real consumers with a test-only protocol edge sink. No display,
    // toolkit, presentation acknowledgment or native edit is fabricated by this fixture.
    fn consumer_wire() -> Box<Wire> {
        let policy = ObservationPolicy::new(&crate::config::Config::default()).unwrap();
        let mut wire = Wire::allocate_state(policy);
        wire.forwarded = Some(Vec::new());
        wire.modifiers = 0;
        wire.state = ready("owned", 5);
        wire
    }
    #[test]
    fn accessory_click_suspend_then_manual_menu_retains_revalidated_origin() {
        let mut wire = consumer_wire();
        let origin = crate::core::Snapshot {
            text: "owned".into(),
            caret: 5,
            native_anchor: anchor(wire.state.state().unwrap()),
            ..Default::default()
        };
        wire.panel.snapshot = Some(origin.clone());
        wire.panel.regions.push(super::super::panel::Region {
            rect: [0, 0, 20, 20],
            action: "menu".into(),
            enabled: true,
        });
        wire.preedit = Some(NativePreedit {
            anchor: origin.native_anchor.clone(),
            text: " prediction".into(),
            revision: wire.user_revision,
            expires_at: Instant::now() + Duration::from_secs(2),
        });
        let actions = Arc::new(std::sync::Mutex::new(Vec::new()));
        let received = actions.clone();
        wire.ui_callback = Some(Arc::new(move |action| {
            received.lock().unwrap().push(action)
        }));
        wire.visible = true;
        wire.pointer_inside = true;
        wire.pointer_xy = [1, 1];
        wire.pointer_button(272, 1);
        wire.pointer_button(272, 0);
        assert_eq!(*actions.lock().unwrap(), ["menu"]);
        wire.set_shortcuts_enabled(false);
        assert!(!wire.enabled);
        assert!(wire.preedit.is_none() && wire.choices.is_none());
        assert!(wire.panel.snapshot.as_ref().unwrap().same_key(&origin));
        // show_native_menu performs its unchanged fresh native recapture before
        // calling this same menu-construction consumer.
        wire.prepare_menu(vec![("settings".into(), "Settings".into(), true)]);
        assert!(
            wire.panel
                .snapshot
                .as_ref()
                .unwrap()
                .same_authority(&origin)
        );
        assert_eq!(wire.panel.menu.as_ref().unwrap()[0].action, "settings");
        assert!(!wire.panel.choice_mode);
    }
    #[test]
    fn choices_escape_uses_held_runtime_dismiss_for_every_mode_and_escape_behavior() {
        for mode in ["emoji", "synonyms", "alternatives"] {
            for behavior in ["pause", "app", "passthrough"] {
                let mut wire = consumer_wire();
                wire.panel.choice_mode = true;
                wire.panel.menu = Some(vec![MenuItem {
                    action: "native-choice:1:0".into(),
                    label: mode.into(),
                    enabled: true,
                }]);
                let actions = Arc::new(std::sync::Mutex::new(Vec::new()));
                let received = actions.clone();
                let consume = behavior != "passthrough";
                wire.callback = Some(Arc::new(move |action| {
                    received.lock().unwrap().push(action);
                    consume
                }));
                let press = KeyEdge {
                    key: 1,
                    ..edge(21, 1)
                };
                let release = KeyEdge {
                    key: 1,
                    ..edge(22, 0)
                };
                let revision = wire.revision;
                assert_eq!(wire.recognize_key("Escape", press), consume);
                assert_eq!(*actions.lock().unwrap(), [Action::Dismiss]);
                assert!(wire.panel.menu.is_some()); // No local hide/repaint/reoffer.
                assert_eq!(wire.revision, revision);
                if !consume {
                    unsafe {
                        wire.keyboard_key(press.serial, press.time, press.key, press.state);
                    }
                }
                unsafe {
                    wire.keyboard_key(release.serial, release.time, release.key, release.state);
                }
                wire.finish_input(consume).unwrap();
                let expected = if consume {
                    vec![]
                } else {
                    vec![press, release]
                };
                assert_eq!(wire.forwarded.as_ref().unwrap(), &expected);
                assert_eq!(*actions.lock().unwrap(), [Action::Dismiss]);
            }
        }
    }
    #[test]
    fn double_tab_pairs_before_first_finish_forward_second_pair_without_replaying_native_edit() {
        for (dispatched, accepted) in [(false, false), (true, true), (true, false)] {
            let mut wire = consumer_wire();
            wire.callback = Some(Arc::new(|_| true));
            assert!(wire.recognize_key("Tab", edge(21, 1)));
            assert!(wire.pending_input.as_ref().unwrap().requires_edit);
            wire.pending_input.as_mut().unwrap().dispatched = dispatched;
            for edge in [edge(22, 0), edge(23, 1), edge(24, 0)] {
                unsafe {
                    wire.keyboard_key(edge.serial, edge.time, edge.key, edge.state);
                }
            }
            assert!(!wire.input_claim_valid());
            wire.finish_input(accepted).unwrap();
            let expected = if dispatched {
                vec![edge(23, 1), edge(24, 0)]
            } else {
                vec![edge(21, 1), edge(22, 0), edge(23, 1), edge(24, 0)]
            };
            assert_eq!(wire.forwarded.as_ref().unwrap(), &expected);
            assert!(!wire.pairs.prior(15, false));
            wire.finish_input(false).unwrap();
            assert_eq!(wire.forwarded.as_ref().unwrap(), &expected);
        }
    }
    #[test]
    fn native_choice_actions_bind_original_indices_to_current_menu_and_visible_prefix() {
        assert_eq!(native_choice_index("native-choice:7:0", 7, 3), Some(0));
        assert_eq!(native_choice_index("native-choice:7:2", 7, 3), Some(2));
        for id in [
            "native-choice:6:0",
            "native-choice:7:3",
            "native-choice:7:0:extra",
            "native-choice:7:-1",
            "native-choice:7:",
            "menu",
        ] {
            assert_eq!(native_choice_index(id, 7, 3), None);
        }
        assert_eq!(native_choice_index("native-choice:7:0", 7, 0), None);
    }
    #[test]
    fn prediction_plan_styles_utf8_bytes_and_never_commits_on_focus_reset() {
        let text = "é😀e\u{301}";
        let layout = preedit_layout(text).unwrap();
        assert_eq!(layout.length, 9);
        assert_eq!(layout.cursor, 0);
        assert_eq!(layout.style, 3);
        assert!(layout.reset_commit.to_bytes().is_empty());
        assert!(preedit_layout("").is_none());
        assert!(preedit_layout("a\0b").is_none());
        assert!(preedit_layout(&"é".repeat(2040)).is_none());
        assert_eq!(
            preedit_layout(&format!("{}x", "é".repeat(2039)))
                .unwrap()
                .length,
            4079
        );
    }
    #[test]
    fn preedit_capability_is_exact_revocable_and_bounded_not_a_pixel_receipt() {
        let now = Instant::now();
        let receipt = NativePreedit {
            anchor: anchor((4, 7)),
            text: " model text".into(),
            revision: 19,
            expires_at: now + Duration::from_secs(2),
        };
        assert!(preedit_scope_valid(
            &receipt,
            Some(&receipt),
            Some((4, 7)),
            19,
            now
        ));
        assert!(!preedit_scope_valid(
            &receipt,
            Some(&receipt),
            Some((4, 8)),
            19,
            now
        ));
        assert!(!preedit_scope_valid(
            &receipt,
            Some(&receipt),
            Some((5, 7)),
            19,
            now
        ));
        assert!(!preedit_scope_valid(
            &receipt,
            Some(&receipt),
            Some((4, 7)),
            20,
            now
        ));
        assert!(!preedit_scope_valid(&receipt, None, Some((4, 7)), 19, now));
        assert!(!preedit_scope_valid(
            &receipt,
            Some(&receipt),
            None,
            19,
            now
        ));
        assert!(!preedit_scope_valid(
            &receipt,
            Some(&receipt),
            Some((4, 7)),
            19,
            receipt.expires_at
        ));
        let mut changed = receipt.clone();
        changed.text.push('!');
        assert!(!preedit_scope_valid(
            &changed,
            Some(&receipt),
            Some((4, 7)),
            19,
            now
        ));
    }
    fn edge(serial: u32, state: u32) -> KeyEdge {
        KeyEdge {
            serial,
            time: serial * 10,
            key: 15,
            state,
        }
    }
    #[test]
    fn held_acceptance_preserves_claim_and_rejection_forwards_original_edges_once_in_order() {
        let now = Instant::now();
        let mut pending = PendingInput::new(3, (4, 7), edge(21, 1), true, now);
        assert!(pending.valid(3, Some((4, 7)), now));
        assert!(pending.hold(edge(22, 0)));
        assert!(!pending.consumes(false, now));
        assert!(!pending.consumes(true, now)); // Runtime success without native dispatch is insufficient.
        pending.valid = false;
        let mut forwarded = Vec::new();
        pending.drain(|edge| forwarded.push(edge));
        pending.drain(|edge| forwarded.push(edge));
        assert_eq!(forwarded, [edge(21, 1), edge(22, 0)]);
        assert!(pending.released);
        assert!(!pending.valid(3, Some((4, 7)), now));
    }
    #[test]
    fn dispatched_uncertain_edit_consumes_acceptance_without_replay_even_after_revocation() {
        let now = Instant::now();
        let mut pending = PendingInput::new(3, (4, 7), edge(21, 1), true, now);
        pending.dispatched = true;
        pending.valid = false;
        assert!(pending.consumes(false, now + Duration::from_secs(1)));
        assert!(!pending.hold(edge(22, 0)));
        assert!(pending.released);
    }
    #[test]
    fn held_shortcut_expires_and_never_absorbs_unrelated_or_unbounded_edges() {
        let now = Instant::now();
        let mut pending = PendingInput::new(3, (4, 7), edge(21, 1), false, now);
        assert!(pending.consumes(true, now));
        assert!(!pending.consumes(true, pending.expires_at));
        assert!(!pending.valid(4, Some((4, 7)), now));
        assert!(!pending.valid(3, Some((4, 8)), now));
        assert!(!pending.valid(3, Some((4, 7)), pending.expires_at));
        assert!(!pending.hold(KeyEdge {
            key: 16,
            ..edge(22, 1)
        }));
        for serial in 22..53 {
            assert!(pending.hold(edge(serial, 1)));
        }
        assert!(!pending.hold(edge(53, 1)));
        let mut forwarded = Vec::new();
        pending.drain(|edge| forwarded.push(edge.serial));
        assert_eq!(forwarded, (21..53).collect::<Vec<_>>());
    }
    #[test]
    fn commit_authority_rejects_stale_serials_sensitive_state_and_oversized_utf8() {
        let mut s = ready("hello", 5);
        let state = s.state().unwrap();
        assert!(s.can_commit(state, " world"));
        assert!(!s.can_commit(state, ""));
        assert!(!s.can_commit(state, "a\0b"));
        assert!(s.can_commit(state, &"x".repeat(4079)));
        assert!(!s.can_commit(state, &"é".repeat(2040)));
        assert!(!s.can_commit((state.0, state.1 + 1), "x"));
        s.content(0x40, 0);
        assert!(!s.can_commit(state, "x"));
        assert!(s.text.is_empty());
    }
    #[test]
    fn bootstrap_phase_never_invents_safe_purpose_or_surrounding() {
        let mut s = ContextState::default();
        s.activate();
        assert_eq!(s.phase(), 1);
        s.commit_state(0);
        assert!(!s.ready);
        s.content(0, 0);
        assert_eq!(s.phase(), 2);
        s.commit_state(0);
        assert!(!s.ready);
        s.surrounding(b"", 0, 0);
        s.commit_state(0);
        assert!(s.ready);
        assert_eq!(s.phase(), 0);
        s.release();
        assert_eq!(s.phase(), 0);
        assert!(!s.active);
    }
    #[test]
    fn shortcut_identity_keeps_physical_above_tab_and_unshifted_letters() {
        let mut one = [0];
        assert_eq!(normalize_key("asciitilde", 49, &mut one), "AboveTab");
        assert_eq!(normalize_key("a", 38, &mut one), "A");
        assert_eq!(normalize_key("ISO_Left_Tab", 23, &mut one), "Tab");
        assert_eq!(normalize_key("KP_Enter", 104, &mut one), "Return");
        assert_eq!(normalize_key("dead_acute", 48, &mut one), "");
    }
    fn ready(text: &str, cursor: usize) -> ContextState {
        let mut s = ContextState::default();
        s.activate();
        s.content(0, 0);
        s.surrounding(text.as_bytes(), cursor, cursor);
        s.commit_state(7);
        s
    }
    #[test]
    fn purpose_and_sensitive_hints_gate_surrounding_before_retention() {
        for (hint, purpose) in [(0, 8), (0, 13), (0x40, 0), (0x80, 0), (0x400, 0)] {
            let mut s = ContextState::default();
            s.activate();
            s.surrounding(b"secret", 6, 6);
            assert!(s.text.is_empty());
            s.content(hint, purpose);
            s.surrounding(b"secret", 6, 6);
            s.commit_state(1);
            assert!(s.state().is_none() && s.text.is_empty());
        }
        for purpose in [0, 1, 7, 9, 12] {
            let mut s = ContextState::default();
            s.activate();
            s.content(0, purpose);
            s.surrounding(b"safe", 4, 4);
            s.commit_state(1);
            assert!(s.ready);
        }
    }
    #[test]
    fn repeated_serials_and_reactivation_never_revive_anchors() {
        let mut s = ready("hello", 5);
        let first = s.state().unwrap();
        let id = s.context_id;
        s.commit_state(7);
        assert_ne!(s.state(), Some(first));
        assert_eq!(s.context_id, id);
        s.activate();
        assert_ne!(s.context_id, id);
        assert_eq!(s.phase(), 1);
        assert!(s.state().is_none() && s.text.is_empty());
    }
    #[test]
    fn utf8_excerpt_matches_bytes_at_caret_not_character_offsets() {
        let s = ready("é😀x", 6);
        let v = s.state().unwrap();
        assert!(s.matches(v, "prefix é😀xyz", 13));
        assert!(!s.matches(v, "prefix é😀xyz", 9));
        assert!(!s.matches(v, "prefix é😀zzz", 13));
        assert!(!s.matches((v.0 + 1, v.1), "prefix é😀xyz", 13));
        assert!(!ready("", 0).matches(ready("", 0).state().unwrap(), "x", 1));
    }
    #[test]
    fn selection_invalid_utf8_and_interior_cursor_erase_previous_excerpt() {
        for (bytes, cursor, anchor) in [
            (b"secret".as_slice(), 6, 0),
            (&[0xff], 1, 1),
            ("é".as_bytes(), 1, 1),
            (b"a\0b".as_slice(), 3, 3),
        ] {
            let mut s = ready("old", 3);
            s.surrounding(bytes, cursor, anchor);
            assert!(!s.ready && s.text.is_empty() && !s.surrounding);
        }
    }
    #[test]
    fn consumed_press_release_pair_survives_privacy_invalidation() {
        let mut pairs = KeyPairs::default();
        assert!(!pairs.prior(42, true));
        pairs.consume(42);
        assert!(pairs.prior(42, true));
        assert!(pairs.prior(42, false));
        assert!(!pairs.prior(42, false));
        assert!(!pairs.prior(900, true));
        pairs.consume(20);
        pairs.clear();
        assert!(!pairs.prior(20, false));
    }
    #[test]
    fn image_bounds_refuse_overflow_stride_and_scale() {
        assert_eq!(valid_image(44, 44, 176, 1), Some(7744));
        for args in [
            (0, 44, 176, 1),
            (44, 44, 175, 1),
            (45, 44, 180, 2),
            (16384, 16384, 65536, 1),
            (44, 44, 176, 17),
        ] {
            assert!(valid_image(args.0, args.1, args.2, args.3).is_none());
        }
    }
    #[test]
    fn idle_pixels_are_zeroized_and_busy_pixels_only_after_release() {
        unsafe {
            let p = libc::mmap(
                ptr::null_mut(),
                4096,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            );
            assert_ne!(p, libc::MAP_FAILED);
            let mut b = Buffer {
                mapping: Some(Mapping {
                    pointer: p.cast(),
                    size: 4096,
                }),
                busy: true,
                ..Default::default()
            };
            ptr::write_bytes(p, 0x7f, 4096);
            b.hide();
            assert_eq!(*p.cast::<u8>(), 0x7f);
            assert!(b.erase && b.busy);
            b.release();
            assert_eq!(*p.cast::<u8>(), 0);
            assert!(!b.erase && !b.busy);
            ptr::write_bytes(p, 0x7f, 4096);
            b.hide();
            assert_eq!(*p.cast::<u8>(), 0);
        }
    }
}
