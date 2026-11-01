//! Single-thread private KWin connection, XKB grab, and compositor-owned SHM buffers.
#![allow(unsafe_op_in_unsafe_fn)]
use super::super::{ShortcutHandler, policy::ObservationPolicy};
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
    time::Instant,
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
    pub bootstrap_token: Arc<AtomicU64>,
    pub last_bootstrap: (u64, u32),
    bootstrap_identity: (u64, u32),
    pub diagnostic: &'static str,
}
impl Wire {
    pub fn connect(fd: OwnedFd, policy: ObservationPolicy) -> Result<Box<Self>> {
        let mut state = ContextState::default();
        state.generation = 1;
        let mut w = Box::new(Self {
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
            bootstrap_token: Arc::new(AtomicU64::new(1)),
            last_bootstrap: (0, 0),
            bootstrap_identity: (0, 0),
            diagnostic: "KWin private input method ready for guarded manual controls; ordinary inline/preedit is unavailable",
        });
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
    pub fn hide(&mut self) {
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
        self.state.release();
        self.hide();
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
        if self.failed || self.stopped || !self.state.can_commit(state, text) {
            return Ok(false);
        }
        let text = CString::new(text)?;
        self.hide();
        unsafe {
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
        self.panel.regions = frame.regions;
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
    pub fn show_menu(&mut self, actions: Vec<(String, String, bool)>) -> Result<bool> {
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
    unsafe fn keyboard_key(&mut self, serial: u32, time: u32, key: u32, state: u32) {
        let context = self.context;
        if context.is_null() || self.failed {
            return;
        }
        let pressed = state == 1;
        let mut consumed = self.pairs.prior(key as usize, pressed);
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
            self.note(true);
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
                let handled = if self.panel.menu.is_some() {
                    self.menu_key(key_name, self.modifiers)
                } else if self.enabled && self.modifiers & !77 == 0 {
                    let mut mods = [""; 4];
                    let mut count = 0;
                    for (bit, name) in [(4, "Ctrl"), (8, "Alt"), (1, "Shift"), (64, "Super")] {
                        if self.modifiers & bit != 0 {
                            mods[count] = name;
                            count += 1;
                        }
                    }
                    self.policy
                        .key_action(key_name, &mods[..count], "linux", &self.app, &self.domain)
                        .is_some_and(|action| {
                            self.callback.as_ref().is_some_and(|callback| {
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    callback(action)
                                }))
                                .unwrap_or(false)
                            })
                        })
                } else {
                    false
                };
                if handled {
                    consumed = true;
                    self.pairs.consume(key as usize);
                }
            }
        }
        if !consumed && self.context == context && !self.failed {
            wl_proxy_marshal_flags(context, 10, ptr::null(), 1, 0, serial, time, key, state);
            if pressed && !modifier_keysym(sym) {
                self.invalidate(true);
                self.changed();
            }
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
                    self.state.activate();
                    self.hide();
                    self.drop_keyboard();
                    destroy(&mut self.context, Some(0));
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
                1 => self.pairs.clear(),
                2 => {
                    self.invalidate(true);
                    self.pairs.clear();
                    self.changed();
                }
                3 => self.keyboard_key(u(0), u(1), u(2), u(3)),
                4 => {
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
