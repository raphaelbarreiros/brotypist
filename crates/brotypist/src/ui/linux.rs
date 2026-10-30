//! GTK-native resident shell. The controller and every GTK object have one owner thread.
use super::{LaunchOptions, settings::SettingsDraft};
use crate::{
    config::Config,
    runtime::{Command, Controller},
    system,
    updates::UpdateService,
};
use anyhow::{Context, Result};
use gtk::prelude::*;
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};
#[path = "linux_models.rs"]
mod models;
#[path = "linux_overlay.rs"]
mod overlay;
#[path = "linux_settings.rs"]
mod settings;
#[path = "linux_tray.rs"]
mod tray;
#[path = "linux_welcome.rs"]
mod welcome;

#[cfg(feature = "native-smoke")]
#[path = "linux_smoke.rs"]
mod native_smoke;
#[cfg(feature = "native-smoke")]
pub fn smoke(output: &std::path::Path) -> Result<i32> {
    native_smoke::run(output)
}
pub(super) const PANES: [&str; 6] = [
    "General",
    "AI Models",
    "Autocomplete",
    "Languages",
    "Privacy",
    "About",
];
#[derive(Clone, Debug)]
pub(super) enum UiAction {
    Settings(String),
    Welcome,
    Models,
    Save,
    Revert,
    CloseSettings,
    CloseWelcome,
    Quit,
    Command(Command),
    Login(bool),
    History(bool),
    RuleHistory(String),
    DeleteHistory(Option<String>),
    ClearStatistics,
    Statistics(String, String),
    UpdateCheck,
    UpdateDownload,
    UpdateCancel,
    UpdateInstall,
    Native(String),
    WelcomeNext,
    WelcomeBack,
    PreviewChanged,
    PreviewKey(String),
}
pub(super) type Sender = mpsc::Sender<UiAction>;
thread_local! {static MODAL_DEPTH:std::cell::Cell<usize>=const{std::cell::Cell::new(0)};}
thread_local! {static APP_ICON:std::cell::OnceCell<gtk::gdk_pixbuf::Pixbuf>=const{std::cell::OnceCell::new()};}
pub(super) fn app_icon() -> Result<gtk::gdk_pixbuf::Pixbuf> {
    APP_ICON.with(|slot| {
        if let Some(icon) = slot.get() {
            return Ok(icon.clone());
        }
        let data = include_bytes!("../../assets/app-24@2x.png");
        let icon = gtk::gdk_pixbuf::Pixbuf::from_read(std::io::Cursor::new(data.as_slice()))?;
        let _ = slot.set(icon.clone());
        Ok(icon)
    })
}
pub(super) fn app_image(size: i32) -> Result<gtk::Image> {
    let icon = app_icon()?;
    let icon = icon
        .scale_simple(size, size, gtk::gdk_pixbuf::InterpType::Bilinear)
        .context("Cannot scale embedded application artwork")?;
    let image = gtk::Image::from_pixbuf(Some(&icon));
    if let Some(a) = image.accessible() {
        a.set_name("BroTypist app icon");
    }
    Ok(image)
}
pub(super) struct ModalGuard;
impl ModalGuard {
    pub fn enter() -> Self {
        MODAL_DEPTH.with(|depth| depth.set(depth.get() + 1));
        Self
    }
}
impl Drop for ModalGuard {
    fn drop(&mut self) {
        MODAL_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}
fn modal_active() -> bool {
    MODAL_DEPTH.with(|depth| depth.get() != 0)
}
pub(super) fn button(label: &str, sender: &Sender, action: UiAction) -> gtk::Button {
    let button = gtk::Button::with_label(label);
    let sender = sender.clone();
    button.connect_clicked(move |_| {
        let _ = sender.send(action.clone());
    });
    button
}
pub(super) fn label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_line_wrap(true);
    label.set_selectable(true);
    label
}
pub(super) fn error(parent: Option<&gtk::Window>, message: impl std::fmt::Display) {
    let _modal = ModalGuard::enter();
    let dialog = gtk::MessageDialog::new(
        parent,
        gtk::DialogFlags::MODAL,
        gtk::MessageType::Warning,
        gtk::ButtonsType::Close,
        &message.to_string(),
    );
    dialog.set_title("BroTypist could not apply this change");
    dialog.run();
    dialog.hide();
    dialog.close();
}
pub(super) fn confirm(parent: Option<&gtk::Window>, title: &str, message: &str) -> bool {
    let _modal = ModalGuard::enter();
    let dialog = gtk::MessageDialog::new(
        parent,
        gtk::DialogFlags::MODAL,
        gtk::MessageType::Question,
        gtk::ButtonsType::None,
        message,
    );
    dialog.set_title(title);
    dialog.add_buttons(&[
        ("Cancel", gtk::ResponseType::Cancel),
        ("Continue", gtk::ResponseType::Accept),
    ]);
    dialog.set_default_response(gtk::ResponseType::Cancel);
    let accepted = dialog.run() == gtk::ResponseType::Accept;
    // close() queues a delete event; hide synchronously before another consent can open.
    dialog.hide();
    dialog.close();
    accepted
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DraftDecision {
    Save,
    Discard,
    Cancel,
}
pub(super) fn close_decision(dirty: bool, decision: DraftDecision, save_succeeded: bool) -> bool {
    !dirty
        || decision == DraftDecision::Discard
        || decision == DraftDecision::Save && save_succeeded
}
pub(super) fn recovery_required(tray_requested: bool, host: bool) -> bool {
    !tray_requested || !host
}
pub(super) fn model_selected(c: &Config) -> bool {
    if c.backend == "gguf" {
        !c.model_path.is_empty() && std::path::Path::new(&c.model_path).is_file()
    } else {
        !c.model.is_empty()
    }
}

struct Shell {
    controller: Controller,
    updates: UpdateService,
    sender: Sender,
    receiver: mpsc::Receiver<UiAction>,
    settings: settings::SettingsWindow,
    welcome: welcome::WelcomeWindow,
    models: models::ModelWindow,
    overlay: overlay::Presentation,
    tray: Option<tray::Tray>,
    tray_available: bool,
    clipboard: Option<system::ClipboardService>,
    power: Option<crate::power::PowerSampler>,
    last_config: Config,
    pending_save: Option<Config>,
    quitting: bool,
    stopped: bool,
    install: Option<Arc<Mutex<crate::update_install::Handoff>>>,
    field_menu: Option<gtk::Menu>,
    field_intent: (String, String),
    menu_tracking: bool,
    last_status: Instant,
    gate_active: bool,
}
impl Shell {
    fn parent(&self) -> gtk::Window {
        if self.welcome.window.is_visible() {
            self.welcome.window.clone()
        } else {
            self.settings.window.clone()
        }
    }
    fn send(&self, command: Command) {
        if let Err(e) = self.controller.send(command) {
            error(Some(&self.parent()), e);
        }
    }
    fn show_settings(&self, pane: &str) {
        self.settings.show_pane(pane);
    }
    fn show_welcome(&mut self) {
        self.welcome.show();
    }
    fn owns_focus(&self) -> bool {
        modal_active()
            || self.settings.window.is_active()
            || self.welcome.window.is_active()
            || self.models.window.is_active()
    }
    fn desired_gate(&self) -> bool {
        self.quitting
            || self.menu_tracking
            || self.settings.recording()
            || self.welcome.preview_active()
            || self.owns_focus()
    }
    fn begin_quit(&mut self) {
        self.quitting = true;
        self.overlay.clear();
        self.welcome.hide();
        self.tray_available = false;
        if let Some(menu) = self.field_menu.take() {
            menu.popdown();
        }
        if let Some(tray) = &self.tray {
            tray.request_stop();
        }
        let _ = self.controller.hide_native();
        // Revoke model/read authorization immediately, before the next poll can observe a host field.
        // Completion/errors remain owned by the retryable shutdown path in tick.
        let _ = self.controller.shutdown();
    }
    fn prompt_draft(&mut self, quitting: bool) -> bool {
        if !self.settings.draft.borrow().is_dirty() {
            return true;
        }
        let _modal = ModalGuard::enter();
        let dialog = gtk::MessageDialog::new(
            Some(&self.parent()),
            gtk::DialogFlags::MODAL,
            gtk::MessageType::Question,
            gtk::ButtonsType::None,
            "Your settings contain unsaved changes.",
        );
        dialog.set_title(if quitting {
            "Save before quitting?"
        } else {
            "Save before closing?"
        });
        dialog.add_buttons(&[
            ("Cancel", gtk::ResponseType::Cancel),
            ("Discard", gtk::ResponseType::Reject),
            ("Save", gtk::ResponseType::Accept),
        ]);
        dialog.set_default_response(gtk::ResponseType::Cancel);
        let choice = dialog.run();
        dialog.hide();
        dialog.close();
        let decision = match choice {
            gtk::ResponseType::Reject => DraftDecision::Discard,
            gtk::ResponseType::Accept => DraftDecision::Save,
            _ => DraftDecision::Cancel,
        };
        let saved = match decision {
            DraftDecision::Discard => self.settings.reset(&self.controller.config()).is_ok(),
            DraftDecision::Save => self.save(),
            DraftDecision::Cancel => false,
        };
        close_decision(true, decision, saved)
    }
    fn save(&mut self) -> bool {
        let candidate = self
            .settings
            .draft
            .borrow()
            .apply_to(&self.controller.config());
        match candidate {
            Err(e) => {
                error(Some(&self.parent()), e);
                false
            }
            Ok(config) => {
                if let Err(e) = self.controller.send(Command::ApplyConfig {
                    config: config.clone(),
                    persist: true,
                }) {
                    error(Some(&self.parent()), e);
                    return false;
                }
                self.pending_save = Some(config.clone());
                self.events();
                // Persistence and validation belong to the controller. A rejected save must keep the draft.
                self.controller.config() == config && self.pending_save.is_none()
            }
        }
    }
    fn action(&mut self, action: UiAction) -> Result<()> {
        match action {
            UiAction::Quit => {
                if !self.quitting && self.prompt_draft(true) {
                    self.begin_quit();
                }
            }
            UiAction::Settings(pane) => self.show_settings(&pane),
            UiAction::Welcome => self.show_welcome(),
            UiAction::Models => {
                self.welcome.close_preview();
                self.models.show();
            }
            UiAction::Save => {
                self.save();
            }
            UiAction::Revert => self.settings.reset(&self.controller.config())?,
            UiAction::CloseSettings => {
                if self.prompt_draft(false) {
                    if self.tray_available {
                        self.settings.window.hide();
                    } else if confirm(
                        Some(&self.parent()),
                        "Quit BroTypist?",
                        "No system tray is available. Quit and stop typing assistance? Cancel keeps Settings available.",
                    ) {
                        self.begin_quit();
                    }
                }
            }
            UiAction::CloseWelcome => {
                if self.tray_available {
                    self.welcome.hide();
                } else if confirm(
                    Some(&self.parent()),
                    "Quit BroTypist?",
                    "No system tray is available. Quit and stop typing assistance?",
                ) && self.prompt_draft(true)
                {
                    self.begin_quit();
                }
            }
            UiAction::Command(command) => self.send(command),
            UiAction::Login(enabled) => {
                let old = self.controller.config().launch_at_login;
                let result = system::set_launch_at_login(enabled);
                if result.is_ok() {
                    let mut config = self.controller.config();
                    config.launch_at_login = enabled;
                    if let Err(e) = self.controller.send(Command::ApplyConfig {
                        config: config.clone(),
                        persist: true,
                    }) {
                        let rollback = system::set_launch_at_login(old);
                        self.settings.refresh_immediate(&self.controller.config());
                        self.welcome.config_changed(&self.controller.config());
                        rollback?;
                        return Err(e);
                    }
                    self.events();
                    if self.controller.config() != config {
                        system::set_launch_at_login(old)?;
                    }
                }
                self.settings.refresh_immediate(&self.controller.config());
                self.welcome.config_changed(&self.controller.config());
                result?;
            }
            UiAction::History(enabled) => {
                if !enabled
                    || confirm(
                        Some(&self.parent()),
                        "Enable encrypted local learning?",
                        "Record eligible authored writing in OS-backed encrypted storage? No plaintext fallback, upload or model-weight training. App recording overrides remain independent.",
                    )
                {
                    self.send(Command::SetHistoryEnabled { enabled });
                    self.events();
                }
                self.settings.refresh_immediate(&self.controller.config());
            }
            UiAction::RuleHistory(scope) | UiAction::DeleteHistory(Some(scope)) => {
                if confirm(
                    Some(&self.parent()),
                    "Forget learned writing?",
                    &format!(
                        "Permanently forget learned writing for {scope}? Aggregate statistics are separate."
                    ),
                ) {
                    let (kind, id) = scope
                        .split_once(':')
                        .context("Select an application or website rule.")?;
                    self.send(Command::DeleteHistory {
                        app: (kind == "app").then(|| id.into()),
                        domain: (kind == "domain").then(|| id.into()),
                    });
                }
            }
            UiAction::DeleteHistory(None) => {
                if confirm(
                    Some(&self.parent()),
                    "Forget all learned writing?",
                    "Permanently remove encrypted writing and derived personalization? Aggregate statistics and the encryption key are kept.",
                ) {
                    self.send(Command::DeleteHistory {
                        app: None,
                        domain: None,
                    });
                }
            }
            UiAction::ClearStatistics => {
                if confirm(
                    Some(&self.parent()),
                    "Clear statistics?",
                    "Delete aggregate statistics? Encrypted history is kept.",
                ) {
                    self.send(Command::ClearStatistics);
                }
            }
            UiAction::Statistics(period, group_by) => {
                self.send(Command::Statistics { period, group_by })
            }
            UiAction::UpdateCheck => {
                self.show_settings("About");
                self.updates.start_check()?;
            }
            UiAction::UpdateDownload => self.updates.start_download()?,
            UiAction::UpdateCancel => self.updates.cancel(),
            UiAction::UpdateInstall => {
                let state = self.updates.snapshot();
                if state["state"] != "ready" {
                    return Ok(());
                }
                if state["install_kind"] == "linux_debian" {
                    if confirm(
                        Some(&self.parent()),
                        "Open system installer?",
                        "Open the verified Debian package? The system installer may request administrator approval. BroTypist cannot report installation success; quit and reopen after it finishes.",
                    ) {
                        self.updates.open_package_installer()?;
                    }
                } else if state["installable"] == true
                    && self.prompt_draft(true)
                    && confirm(
                        Some(&self.parent()),
                        "Install and relaunch?",
                        "Stop typing assistance and install the verified update? Settings, history and models are kept.",
                    )
                {
                    self.install = Some(self.updates.prepare_install()?);
                    self.begin_quit();
                }
            }
            UiAction::Native(id) => self.native(&id)?,
            UiAction::WelcomeNext => {
                if self.welcome.can_continue(&self.controller.config()) {
                    if self.welcome.step() == 4 {
                        let mut c = self.controller.config();
                        c.setup_completed = true;
                        self.send(Command::ApplyConfig {
                            config: c,
                            persist: true,
                        });
                        self.events();
                        if !self.controller.config().setup_completed {
                            return Ok(());
                        }
                    }
                    if self.welcome.step() == 5 {
                        self.welcome.hide();
                        if !self.tray_available {
                            self.show_settings("General");
                        }
                    } else {
                        self.welcome.select(self.welcome.step() + 1);
                    }
                }
            }
            UiAction::WelcomeBack => self.welcome.select(self.welcome.step().saturating_sub(1)),
            UiAction::PreviewChanged => self.welcome.update_preview(&self.controller)?,
            UiAction::PreviewKey(chord) => self.welcome.accept(&chord, &self.controller)?,
        }
        Ok(())
    }
    fn native(&mut self, id: &str) -> Result<()> {
        match id {
            "x11_field_menu" => {
                let status = self.controller.status();
                self.field_intent = (
                    text(&status, "current_app"),
                    text(&status, "current_domain"),
                );
                self.controller.suspend_shortcuts(true)?;
                self.menu_tracking = true;
                let menu = gtk::Menu::new();
                for (title, id, enabled) in [
                    (
                        format!(
                            "Predictions in {}: {}",
                            self.field_intent.0,
                            if status["current_app_enabled"] == true {
                                "Enabled"
                            } else {
                                "Disabled"
                            }
                        ),
                        "field_toggle",
                        !self.field_intent.0.is_empty(),
                    ),
                    (
                        if status["paused_remaining"].as_f64().unwrap_or(0.) > 0. {
                            "Resume predictions"
                        } else {
                            "Pause for 1 hour"
                        }
                        .into(),
                        "field_pause",
                        true,
                    ),
                    ("Settings…".into(), "settings", true),
                ] {
                    let item = gtk::MenuItem::with_label(&title);
                    item.set_sensitive(enabled);
                    let tx = self.sender.clone();
                    let id = id.to_owned();
                    item.connect_activate(move |_| {
                        let _ = tx.send(UiAction::Native(id.clone()));
                    });
                    menu.append(&item);
                }
                let tx = self.sender.clone();
                menu.connect_deactivate(move |_| {
                    let _ = tx.send(UiAction::Native("menu_closed".into()));
                });
                menu.show_all();
                menu.popup_easy(0, gtk::current_event_time());
                self.field_menu = Some(menu);
            }
            "menu" => {
                let status = self.controller.status();
                self.field_intent = (
                    text(&status, "current_app"),
                    text(&status, "current_domain"),
                );
                self.controller.suspend_shortcuts(true)?;
                self.menu_tracking = true;
                let app = &self.field_intent.0;
                let rows = vec![
                    (
                        "field_toggle".into(),
                        format!(
                            "Predictions in {}: {}",
                            if app.is_empty() { "Current App" } else { app },
                            if status["current_app_enabled"] == true {
                                "Enabled"
                            } else {
                                "Disabled"
                            }
                        ),
                        !app.is_empty(),
                    ),
                    (
                        "field_pause".into(),
                        if status["paused_remaining"].as_f64().unwrap_or(0.0) > 0.0 {
                            "Resume predictions"
                        } else {
                            "Pause for 1 hour"
                        }
                        .into(),
                        true,
                    ),
                    ("settings".into(), "Settings…".into(), true),
                ];
                if !self.controller.show_native_menu(rows)? {
                    self.menu_tracking = false;
                    self.controller.suspend_shortcuts(false)?;
                }
            }
            "menu_open" => {
                self.menu_tracking = true;
                self.controller.suspend_shortcuts(true)?;
            }
            "menu_closed" => {
                self.menu_tracking = false;
                self.controller.suspend_shortcuts(false)?;
            }
            "connection_lost" | "context_changed" => {
                self.overlay.clear();
                self.menu_tracking = false;
                self.controller.suspend_shortcuts(false)?;
            }
            "field_toggle" => {
                let (app, domain) = self.field_intent.clone();
                self.send(Command::ToggleApp { app, domain });
            }
            "field_pause" => {
                if self.controller.status()["paused_remaining"]
                    .as_f64()
                    .unwrap_or(0.0)
                    > 0.0
                {
                    self.send(Command::Resume);
                } else {
                    self.send(Command::Pause { minutes: 60 });
                }
            }
            "settings" => self.show_settings("General"),
            "welcome" => self.show_welcome(),
            "models" => self.models.show(),
            "quit" => self.action(UiAction::Quit)?,
            "updates" => self.action(UiAction::UpdateCheck)?,
            "toggle" => {
                let mut c = self.controller.config();
                c.global_enabled = !c.global_enabled;
                self.send(Command::ApplyConfig {
                    config: c,
                    persist: true,
                });
            }
            "resume" => self.send(Command::Resume),
            "pause5" => self.send(Command::Pause { minutes: 5 }),
            "pause15" => self.send(Command::Pause { minutes: 15 }),
            "pause60" => self.send(Command::Pause { minutes: 60 }),
            "tomorrow" => {
                use chrono::{Local, TimeZone};
                let now = Local::now();
                let next = now
                    .date_naive()
                    .succ_opt()
                    .context("Invalid date")?
                    .and_hms_opt(0, 0, 0)
                    .context("Invalid date")?;
                let next = Local
                    .from_local_datetime(&next)
                    .earliest()
                    .context("Unavailable local midnight")?;
                self.send(Command::Pause {
                    minutes: ((next - now).num_seconds().max(1) as u32 + 59) / 60,
                });
            }
            "app_toggle" => {
                let s = self.controller.status();
                self.send(Command::ToggleApp {
                    app: text(&s, "current_app"),
                    domain: text(&s, "current_domain"),
                });
            }
            "force" => self.send(Command::Action(crate::platform::Action::Force)),
            "alternatives" => self.send(Command::Action(crate::platform::Action::Alternatives)),
            _ => {}
        }
        Ok(())
    }
    fn events(&mut self) {
        for event in self.controller.poll() {
            match event["type"].as_str().unwrap_or("") {
                "suggestion" => {
                    if self.owns_focus() {
                        self.overlay.clear();
                        let _ = self.controller.hide_native();
                        continue;
                    }
                    if self.controller.native_event_source().is_some() {
                        // The private Wayland path owns manual controls. Nonempty ordinary inline is never sent.
                        let _ = self.controller.present_native(
                            &text(&event, "native_anchor"),
                            "",
                            self.controller.status()["state"] == "paused",
                        );
                    } else {
                        self.overlay.present(event, &self.controller);
                    }
                }
                "clear" | "hide" => {
                    self.overlay.clear();
                    let _ = self.controller.hide_native();
                }
                "caret" => {
                    if self.owns_focus() {
                        self.overlay.clear();
                        let _ = self.controller.hide_native();
                        continue;
                    }
                    if self.controller.native_event_source().is_some() {
                        if self.controller.config().show_accessory {
                            let _ = self.controller.present_native(
                                &text(&event, "native_anchor"),
                                "",
                                self.controller.status()["state"] == "paused",
                            );
                        } else {
                            let _ = self.controller.hide_native();
                        }
                    } else {
                        self.overlay
                            .caret(&event, self.controller.config().show_accessory);
                    }
                }
                "config" => {
                    let current = self.controller.config();
                    if self.pending_save.as_ref() == Some(&current) {
                        let _ = self.settings.reset(&current);
                        self.pending_save = None;
                    }
                    self.welcome.config_changed(&current);
                    let _ = self.settings.refresh(&current);
                    self.last_config = current;
                }
                "statistics" | "statistics_cleared" | "history_info" | "history_counts"
                | "history_deleted" => self.settings.service_event(&event),
                "error" => {
                    self.pending_save = None;
                    self.settings.notice.set_text(&text(&event, "message"));
                }
                _ => {}
            }
        }
    }
    fn tick(&mut self) {
        if self.stopped {
            return;
        }
        if let Some(c) = &self.clipboard {
            let _ = c.poll();
        }
        let gate = self.desired_gate();
        if gate != self.gate_active {
            let _ = self.controller.suspend_shortcuts(gate);
            self.gate_active = gate;
        }
        while let Ok(action) = self.receiver.try_recv() {
            let presentation = matches!(
                &action,
                UiAction::Command(Command::PresentationResult { .. })
            );
            // A draw acknowledgment must not disarm another already visible chooser/ghost.
            if !presentation {
                let _ = self.controller.suspend_shortcuts(true);
            }
            if let Err(e) = self.action(action) {
                error(Some(&self.parent()), e);
            }
            if !presentation {
                let gate = self.desired_gate();
                let _ = self.controller.suspend_shortcuts(gate);
                self.gate_active = gate;
            }
        }
        self.events();
        self.welcome.tick(&self.controller);
        if self.quitting {
            self.welcome.hide();
            if let Some(tray) = &self.tray {
                tray.request_stop();
            }
            let preview_done = self.welcome.preview_stopped();
            let controller = self.controller.shutdown();
            // Close updater before committing any handoff, never abort an already committed installer.
            let update_done = if self.install.is_some() {
                true
            } else {
                self.updates.close().is_ok()
            };
            match controller {
                Ok(true)
                    if preview_done
                        && update_done
                        && self.tray.as_ref().is_none_or(tray::Tray::is_stopped) =>
                {
                    if let Some(h) = self.install.clone() {
                        let result = h.lock().unwrap().commit();
                        if let Err(e) = result {
                            error(Some(&self.parent()), e);
                            self.install = None;
                            self.quitting = false;
                            return;
                        }
                    }
                    self.controller.bind_native_ui(None);
                    self.tray.take();
                    self.overlay.clear();
                    self.clipboard.take();
                    self.power.take();
                    self.stopped = true;
                    gtk::main_quit();
                    return;
                }
                Err(e) => {
                    self.quitting = false;
                    self.show_settings("General");
                    self.settings.notice.set_text(&e.to_string());
                }
                _ => {}
            }
        }
        if self.last_status.elapsed() >= Duration::from_millis(250) {
            self.last_status = Instant::now();
            let status = self.controller.status();
            let c = self.controller.config();
            self.settings.status(&status, &c, &self.updates.snapshot());
            self.models.refresh(&status, &c);
            if let Some(tray) = &self.tray {
                tray.update(&status, &c);
                let available = tray.available();
                if self.tray_available && !available {
                    self.menu_tracking = false;
                    let _ = self.controller.suspend_shortcuts(false);
                    self.show_settings("General");
                }
                self.tray_available = available;
            }
            self.settings
                .set_recovery(recovery_required(self.tray.is_some(), self.tray_available));
            if c != self.last_config {
                let _ = self.settings.refresh(&c);
                self.welcome.config_changed(&c);
                self.last_config = c;
            }
        }
    }
}
pub(super) fn text(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Object(status)) if key == "native_status" => status
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Native access status unavailable.")
            .into(),
        _ => String::new(),
    }
}

pub fn launch(config: Config, options: LaunchOptions) -> Result<i32> {
    run_shell(create_shell(config, options)?)
}
fn create_shell(config: Config, options: LaunchOptions) -> Result<Rc<RefCell<Shell>>> {
    // prepare_input_method is deliberately the caller's pre-toolkit responsibility.
    gtk::init().context("GTK could not connect to the current Linux display")?;
    gtk::Window::set_default_icon_name("com.inklow.Inklow");
    gtk::Window::set_default_icon_list(&[app_icon()?]);
    let power = crate::power::PowerSampler::start()?;
    let clipboard = system::ClipboardService::install()?;
    let controller = Controller::start(config.clone())?;
    let updates = UpdateService::new()?;
    let (sender, receiver) = mpsc::channel();
    let native_sender = sender.clone();
    controller.bind_native_ui(Some(Arc::new(move |id| {
        let _ = native_sender.send(UiAction::Native(id));
    })));
    let settings = settings::SettingsWindow::new(&config, &sender)?;
    let welcome = welcome::WelcomeWindow::new(&config, &sender);
    let models = models::ModelWindow::new(&sender);
    let overlay = overlay::Presentation::new(&sender);
    let tray = if options.no_tray {
        None
    } else {
        tray::Tray::start(sender.clone()).ok()
    };
    let tray_available = tray.as_ref().is_some_and(tray::Tray::available);
    let shell = Rc::new(RefCell::new(Shell {
        controller,
        updates,
        sender,
        receiver,
        settings,
        welcome,
        models,
        overlay,
        tray,
        tray_available,
        clipboard: Some(clipboard),
        power: Some(power),
        last_config: config.clone(),
        pending_save: None,
        quitting: false,
        stopped: false,
        install: None,
        field_intent: Default::default(),
        field_menu: None,
        menu_tracking: false,
        gate_active: false,
        last_status: Instant::now(),
    }));
    for window in [
        shell.borrow().settings.window.clone(),
        shell.borrow().welcome.window.clone(),
        shell.borrow().models.window.clone(),
    ] {
        let weak = Rc::downgrade(&shell);
        window.connect_focus_in_event(move |_, _| {
            if let Some(shell) = weak.upgrade() {
                if let Ok(mut s) = shell.try_borrow_mut() {
                    let _ = s.controller.suspend_shortcuts(true);
                    s.gate_active = true;
                }
            }
            glib::Propagation::Proceed
        });
    }
    {
        let mut s = shell.borrow_mut();
        if options.pane.as_deref() == Some("welcome") {
            s.show_welcome();
        } else {
            if !tray_available || options.pane.is_some() {
                s.show_settings(options.pane.as_deref().unwrap_or("General"));
            }
            if !config.setup_completed || !model_selected(&config) {
                s.show_welcome();
            }
        }
        if config.check_updates_automatically {
            let _ = s.updates.start_automatic_check();
        }
    }
    Ok(shell)
}
fn run_shell(shell: Rc<RefCell<Shell>>) -> Result<i32> {
    // Readiness watching borrows the transport's fd; never dup, close or create a second owner.
    let watched = Rc::new(RefCell::new(None::<(i32, bool, glib::SourceId)>));
    let weak = Rc::downgrade(&shell);
    let watches = watched.clone();
    let timer = glib::timeout_add_local(Duration::from_millis(25), move || {
        let Some(shell) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        let fd = {
            let Ok(mut s) = shell.try_borrow_mut() else {
                return glib::ControlFlow::Continue;
            };
            s.tick();
            if s.stopped {
                return glib::ControlFlow::Break;
            }
            s.controller.native_event_source()
        };
        let old = watches.borrow().as_ref().map(|(fd, out, _)| (*fd, *out));
        if fd != old {
            if let Some((_, _, source)) = watches.borrow_mut().take() {
                source.remove();
            }
            if let Some((fd, out)) = fd {
                let weak = Rc::downgrade(&shell);
                let conditions = glib::IOCondition::IN
                    | glib::IOCondition::HUP
                    | glib::IOCondition::ERR
                    | if out {
                        glib::IOCondition::OUT
                    } else {
                        glib::IOCondition::empty()
                    };
                let active = watches.clone();
                let source = glib::source::unix_fd_add_local(fd, conditions, move |_, _| {
                    let Some(shell) = weak.upgrade() else {
                        active.borrow_mut().take();
                        return glib::ControlFlow::Break;
                    };
                    let next = {
                        let Ok(mut s) = shell.try_borrow_mut() else {
                            return glib::ControlFlow::Continue;
                        };
                        s.tick();
                        s.controller.native_event_source()
                    };
                    if next != Some((fd, out)) {
                        active.borrow_mut().take();
                        glib::ControlFlow::Break
                    } else {
                        glib::ControlFlow::Continue
                    }
                });
                *watches.borrow_mut() = Some((fd, out, source));
            }
        }
        glib::ControlFlow::Continue
    });
    // Startup health is acknowledged only after the native GUI loop has dispatched.
    glib::idle_add_local_once(|| {
        let _ = crate::update_install::acknowledge_startup();
    });
    gtk::main();
    // The timer may have removed itself on completed shutdown.
    if !shell.borrow().stopped {
        timer.remove();
    }
    if let Some((_, _, source)) = watched.borrow_mut().take() {
        source.remove();
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn draft_cancel_and_failed_save_do_not_close() {
        assert!(!close_decision(true, DraftDecision::Cancel, true));
        assert!(!close_decision(true, DraftDecision::Save, false));
        assert!(close_decision(true, DraftDecision::Discard, false));
        assert!(close_decision(true, DraftDecision::Save, true));
    }
    #[test]
    fn tray_disabled_or_lost_requires_visible_recovery() {
        assert!(recovery_required(false, true));
        assert!(recovery_required(true, false));
        assert!(!recovery_required(true, true));
    }
    #[test]
    fn staged_save_rebases_live_model_and_rules() {
        let c = Config::default();
        let mut d = SettingsDraft::new(&c).unwrap();
        d.set("debounce_ms", json!(900)).unwrap();
        let mut live = c.clone();
        live.model_path = "/synthetic/selected.gguf".into();
        live.clipboard_context = true;
        let applied = d.apply_to(&live).unwrap();
        assert_eq!(applied.model_path, live.model_path);
        assert!(applied.clipboard_context);
        assert_eq!(applied.debounce_ms, 900);
        assert!(d.is_dirty());
    }
    #[test]
    fn nested_native_modals_restore_capture_gate_on_every_exit() {
        assert!(!modal_active());
        {
            let _outer = ModalGuard::enter();
            assert!(modal_active());
            {
                let _inner = ModalGuard::enter();
                assert!(modal_active());
            }
            assert!(modal_active());
        }
        assert!(!modal_active());
    }
}
