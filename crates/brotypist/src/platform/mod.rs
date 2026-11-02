//! Native observation and mutation authority. Keyboard callbacks may not perform capture or inference.
use crate::{
    config::Config,
    core::{Rect, Snapshot},
};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Instant};

/// A bounded, revocable native preedit capability, NOT evidence of rendered pixels.
/// Only the originating bridge can validate this exact receipt before acceptance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativePreedit {
    pub anchor: String,
    pub text: String,
    pub revision: u64,
    pub expires_at: Instant,
}

/// Exact fully-painted prefix on an owned native panel. Protocol capability only;
/// not a compositor pixel acknowledgment and never authority for hidden candidates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeChoices {
    pub anchor: String,
    pub values: Vec<String>,
    pub revision: u64,
    pub expires_at: Instant,
}

/// A target-owned native client receives the exact already-authorized offer.
/// Offsets are scalar indices in the committed snapshot; transports convert once.
pub struct NativeClientOffer<'a> {
    pub snapshot: &'a Snapshot,
    pub user_input_revision: u64,
    pub mode: &'a str,
    pub text: &'a str,
    pub values: &'a [String],
    pub start: usize,
    pub length: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeHostPresentation {
    Preedit(NativePreedit),
    Choices(NativeChoices),
}

/// Activate only the insertions computed from the actual native-visible prefix.
/// Presentation alone must never consume an acceptance key.
pub struct NativeClientAcceptance<'a> {
    pub presentation: &'a NativeHostPresentation,
    pub word_text: &'a str,
    pub all_text: &'a str,
}

/// A real target-owned edit already happened. The runtime must validate and
/// account for it, never invoke a second insertion.
pub struct NativeHostEdit {
    pub presentation: NativeHostPresentation,
    pub before: Snapshot,
    pub after: Snapshot,
    pub start: usize,
    pub length: usize,
    pub text: String,
    pub action: Action,
    pub mode: String,
    pub accepted_at: Instant,
}

pub struct NativeHostDismiss {
    pub presentation: NativeHostPresentation,
    pub before: Snapshot,
    pub mode: String,
    pub bypass: bool,
    pub accepted_at: Instant,
}

/// A native edit may have been dispatched, but no exact applied readback exists.
/// Preserve provenance without counting acceptance or attempting another edit.
pub struct NativeHostUncertain {
    pub presentation: NativeHostPresentation,
    pub before: Snapshot,
    pub mode: String,
    pub accepted_at: Instant,
}

pub enum NativeHostEvent {
    Applied(NativeHostEdit),
    Indeterminate(NativeHostUncertain),
    Dismissed(NativeHostDismiss),
    Action { action: Action, origin: Foreground },
}

#[cfg(target_os = "linux")]
pub mod client;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
pub mod policy;
#[cfg(target_os = "linux")]
pub mod wayland;
#[cfg(target_os = "windows")]
pub mod windows;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Word,
    All,
    Force,
    AppToggle,
    Toggle,
    Alternatives,
    Synonyms,
    Dismiss,
    Bypass,
    Choice(usize),
    Settings,
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    Allowed,
    Denied,
    Unknown,
}

/// Delivered input is not an applied host edit. Indeterminate edits must never be retried automatically.
#[derive(Clone, Debug)]
pub enum EditOutcome {
    Applied(Snapshot),
    Dispatched,
    Rejected,
    Indeterminate,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Foreground {
    pub app_id: String,
    pub executable: String,
    pub domain: String,
    pub window_id: String,
    pub window_pid: i32,
    pub document_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextRead {
    pub text: String,
    pub status: String,
    pub source: String,
}
impl ContextRead {
    pub fn blocked(source: &str) -> Self {
        Self {
            text: String::new(),
            status: "blocked".into(),
            source: source.into(),
        }
    }
}

pub type ShortcutHandler = Arc<dyn Fn(Action) -> bool + Send + Sync>;

/// A plain-metadata request. Reacquire native handles on the reader's own thread.
pub trait DetachedContextRead: Send {
    fn read(&self, authorized: &dyn Fn() -> bool, max_chars: usize) -> Result<ContextRead>;
    fn matches(&self) -> Result<bool>;
    fn matches_snapshot(&self, snapshot: &Snapshot) -> bool;
}

/// Created and used on one platform-owner thread; not required to be Send.
/// OS handles and native callbacks remain owned until stop acknowledges their release.
pub trait NativeBridge {
    fn configure(&mut self, config: &Config) -> Result<()>;
    fn capture(&mut self) -> Result<Option<Snapshot>>;
    fn foreground(&mut self) -> Result<Option<Foreground>>;
    fn insert(&mut self, snapshot: &Snapshot, text: &str) -> Result<EditOutcome>;
    fn replace_range(
        &mut self,
        snapshot: &Snapshot,
        start: usize,
        length: usize,
        text: &str,
    ) -> Result<EditOutcome>;
    fn range_rect(
        &mut self,
        snapshot: &Snapshot,
        start: usize,
        length: usize,
    ) -> Result<Option<Rect>>;
    fn focused_context(
        &mut self,
        snapshot: &Snapshot,
        authorized: &dyn Fn() -> bool,
        max_chars: usize,
    ) -> Result<ContextRead>;
    fn context_ticket(
        &mut self,
        _snapshot: &Snapshot,
    ) -> Result<Option<Box<dyn DetachedContextRead>>> {
        Ok(None)
    }
    fn start_shortcuts(&mut self, callback: ShortcutHandler) -> Result<()>;
    fn set_shortcuts_enabled(&mut self, enabled: bool);
    fn set_input_callback(&mut self, callback: Option<Arc<dyn Fn() + Send + Sync>>);
    fn finish_input(&mut self, accepted: bool) -> Result<()>;
    fn input_claim_valid(&self) -> bool {
        true
    }
    fn input_revision(&self) -> u64;
    fn user_input_revision(&self) -> u64;
    fn input_timestamp(&self) -> Option<Instant>;
    /// None means unavailable detection, which callers must treat as blocking.
    fn secure_input_enabled(&self) -> Option<bool>;
    fn request_permission(&mut self, kind: &str) -> Result<Permission>;
    fn status(&self) -> serde_json::Value;
    fn stop(&mut self) -> Result<()>;
    fn atomic_replacement_supported(&self) -> bool {
        !cfg!(target_os = "linux")
    }
    /// Metadata-only helper; absent on platforms without the baseline Docs integration.
    fn docs_hint(&mut self) -> Result<Option<serde_json::Value>> {
        Ok(None)
    }
    fn enable_docs_accessibility(&mut self, _token: &str) -> Result<serde_json::Value> {
        anyhow::bail!("Google Docs accessibility setup is unavailable on this platform.")
    }
    /// Compositor-owned bridges override these; other native backends have no transport to pump.
    fn pump_events(&mut self) -> Result<()> {
        Ok(())
    }
    fn native_event_source(&self) -> Option<(i32, bool)> {
        None
    }
    fn bind_native_ui(&mut self, _callback: Option<Arc<dyn Fn(String) + Send + Sync>>) {}
    /// Rendering ownership is distinct from a wake/event FD supplied by a wrapper.
    fn uses_native_presentation(&self) -> bool {
        self.native_event_source().is_some()
    }
    /// Target-owned clients handle their own visible-offer keys atomically.
    /// Their receipts must never arm a second, global input consumer.
    fn client_owns_actions(&self) -> bool {
        false
    }
    fn native_edit_pending(&self) -> bool {
        false
    }
    fn present_client_offer(
        &mut self,
        _offer: &NativeClientOffer<'_>,
    ) -> Result<Option<NativeHostPresentation>> {
        Ok(None)
    }
    fn arm_client_offer(&mut self, _acceptance: &NativeClientAcceptance<'_>) -> Result<bool> {
        Ok(false)
    }
    fn drain_host_events(&mut self) -> Vec<NativeHostEvent> {
        Vec::new()
    }
    fn show_native_menu(&mut self, _rows: Vec<(String, String, bool)>) -> Result<bool> {
        Ok(false)
    }

    fn context_matches(&mut self, snapshot: &Snapshot) -> Result<bool> {
        Ok(self
            .capture()?
            .is_some_and(|current| current.same_key(snapshot) && current.same_authority(snapshot)))
    }
    fn replace_selection(&mut self, snapshot: &Snapshot, text: &str) -> Result<EditOutcome> {
        self.replace_range(snapshot, snapshot.caret, snapshot.selection_length, text)
    }
    /// Only compositor-owned input methods implement this; normal OS overlays are separate UI owners.
    fn present_native(&mut self, _anchor: &str, _text: &str, _paused: bool) -> Result<bool> {
        Ok(false)
    }
    fn present_preedit(&mut self, _anchor: &str, _text: &str) -> Result<Option<NativePreedit>> {
        Ok(None)
    }
    fn native_preedit_valid(&self, _receipt: &NativePreedit) -> bool {
        false
    }
    fn present_choices(
        &mut self,
        _anchor: &str,
        _values: &[String],
    ) -> Result<Option<NativeChoices>> {
        Ok(None)
    }
    fn native_choices_valid(&self, _receipt: &NativeChoices) -> bool {
        false
    }
    fn set_native_panel_environment(
        &mut self,
        _width: i32,
        _height: i32,
        _scale: i32,
        _dark: bool,
    ) -> Result<()> {
        Ok(())
    }
    fn hide_native(&mut self) -> Result<()> {
        Ok(())
    }
}

pub fn create_bridge(config: &Config) -> Result<Box<dyn NativeBridge>> {
    #[cfg(target_os = "macos")]
    let bridge: Box<dyn NativeBridge> = {
        let mut native = macos::MacBridge::new()?;
        macos::special::attach(&mut native)?;
        Box::new(native)
    };
    #[cfg(target_os = "windows")]
    let bridge: Box<dyn NativeBridge> = Box::new(windows::WindowsBridge::new()?);
    #[cfg(target_os = "linux")]
    let base: Result<Box<dyn NativeBridge>> = if wayland::is_prepared() {
        wayland::WaylandBridge::new().map(|bridge| Box::new(bridge) as Box<dyn NativeBridge>)
    } else {
        linux::LinuxBridge::new().map(|bridge| Box::new(bridge) as Box<dyn NativeBridge>)
    };
    #[cfg(target_os = "linux")]
    let bridge = client::wrap(config, base)?;
    let mut bridge = bridge;
    bridge.configure(config)?;
    Ok(bridge)
}
