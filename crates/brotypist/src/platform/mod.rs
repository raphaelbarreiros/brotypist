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
    let mut bridge: Box<dyn NativeBridge> = {
        let mut native = macos::MacBridge::new()?;
        macos::special::attach(&mut native)?;
        Box::new(native)
    };
    #[cfg(target_os = "windows")]
    let mut bridge: Box<dyn NativeBridge> = Box::new(windows::WindowsBridge::new()?);
    #[cfg(target_os = "linux")]
    let mut bridge: Box<dyn NativeBridge> = if wayland::is_prepared() {
        Box::new(wayland::WaylandBridge::new()?)
    } else {
        Box::new(linux::LinuxBridge::new()?)
    };
    bridge.configure(config)?;
    Ok(bridge)
}
