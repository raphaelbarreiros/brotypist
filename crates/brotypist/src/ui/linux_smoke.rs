//! Opt-in, disposable desktop evidence using the actual resident shell and inference worker.
use super::*;
use anyhow::{bail, ensure};
use glib::translate::IntoGlib;
use std::cell::Cell;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

fn rendered_snapshot(window: &gtk::Window) -> Result<gtk::gdk_pixbuf::Pixbuf> {
    use gdk::prelude::WindowExtManual;

    ensure!(
        window.is_visible() && window.is_mapped(),
        "Screenshot requires the mapped canonical GTK window."
    );
    let clock = window
        .frame_clock()
        .context("Mapped GTK window has no frame clock.")?;
    let loop_ = glib::MainLoop::new(None, false);
    let painted = Rc::new(Cell::new(false));
    let ready = painted.clone();
    let done = loop_.clone();
    let handler = clock.connect_after_paint(move |_| {
        ready.set(true);
        done.quit();
    });
    let expired = Rc::new(Cell::new(false));
    let timed_out = expired.clone();
    let done = loop_.clone();
    let timeout = glib::timeout_add_local_once(Duration::from_secs(5), move || {
        timed_out.set(true);
        done.quit();
    });
    // GTK allocates during the frame's layout phase. A positive allocated size alone
    // does not make gtk_widget_draw valid after a pane, label or control changed.
    window.queue_draw();
    clock.request_phase(gdk::FrameClockPhase::AFTER_PAINT);
    loop_.run();
    clock.disconnect(handler);
    if !expired.get() {
        timeout.remove();
    }
    ensure!(
        painted.get() && !expired.get(),
        "GTK did not render the requested frame."
    );
    let width = window.allocated_width();
    let height = window.allocated_height();
    ensure!(width > 0 && height > 0, "Native widget has no allocation.");
    // Read the actual server-side window after paint, not a newly-created blank surface.
    let native = gtk::prelude::WidgetExt::window(window)
        .context("Mapped GTK window has no native drawable.")?;
    let pixels = native
        .pixbuf(0, 0, width, height)
        .context("Cannot read the rendered native GTK window.")?;
    ensure!(
        image_has_content(
            pixels.read_pixel_bytes().as_ref(),
            pixels.width() as usize,
            pixels.height() as usize,
            pixels.rowstride() as usize,
            pixels.n_channels() as usize,
        ),
        "Native screenshot is blank or transparent rather than rendered controls."
    );
    Ok(pixels)
}

fn image_has_content(
    bytes: &[u8],
    width: usize,
    height: usize,
    stride: usize,
    channels: usize,
) -> bool {
    if width == 0 || height == 0 || !matches!(channels, 3 | 4) {
        return false;
    }
    let Some(row_bytes) = width.checked_mul(channels) else {
        return false;
    };
    if stride < row_bytes {
        return false;
    }
    let mut background = None::<[u8; 3]>;
    let mut contrasting = 0usize;
    for y in 0..height {
        let Some(start) = y.checked_mul(stride) else {
            return false;
        };
        let Some(end) = start.checked_add(row_bytes) else {
            return false;
        };
        let Some(row) = bytes.get(start..end) else {
            return false;
        };
        for pixel in row.chunks_exact(channels) {
            if channels == 4 && pixel[3] < 128 {
                continue;
            }
            let base = *background.get_or_insert([pixel[0], pixel[1], pixel[2]]);
            if (0..3).any(|channel| pixel[channel].abs_diff(base[channel]) >= 16) {
                contrasting += 1;
            }
        }
    }
    let Some(area) = width.checked_mul(height) else {
        return false;
    };
    contrasting >= (area / 1000).max(32)
}

fn cancel_owned_dialogs() {
    // This executable owns only the disposable fixture's GTK objects. In particular,
    // unwind Dialog::run even if the expected title/parent did not match the real modal.
    for dialog in gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|window| window.downcast::<gtk::Dialog>().ok())
    {
        dialog.response(gtk::ResponseType::Cancel);
    }
}

struct Scenario {
    step: u32,
    reported_step: Option<u32>,
    entered: Instant,
    started: Instant,
    output: PathBuf,
    checks: Vec<String>,
    images: Vec<String>,
    before: String,
    completed: bool,
    failure: Option<String>,
    cleanup_at: Rc<Cell<Option<Instant>>>,
    prompt_error: Rc<RefCell<Option<String>>>,
    answered_prompts: Rc<Cell<u32>>,
}
impl Scenario {
    fn next(&mut self) {
        self.step += 1;
        self.entered = Instant::now();
    }
    fn check(&mut self, condition: bool, name: &str) -> Result<()> {
        ensure!(condition, "{name}");
        self.checks.push(name.into());
        Ok(())
    }
    fn wait(&self, ready: bool, seconds: u64, what: &str) -> Result<bool> {
        if !ready {
            ensure!(
                self.entered.elapsed() < Duration::from_secs(seconds),
                "Timed out waiting for {what}."
            );
        }
        Ok(ready)
    }
    fn capture(&mut self, window: &gtk::Window, name: &str) -> Result<()> {
        let pixels = rendered_snapshot(window)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(self.output.join(name))?;
        file.write_all(&pixels.save_to_bufferv("png", &[])?)?;
        file.sync_all()?;
        self.images.push(name.into());
        eprintln!(
            "[native-linux-smoke] captured {name} ({}x{})",
            pixels.width(),
            pixels.height()
        );
        Ok(())
    }
    fn answer(&self, parent: &gtk::Window, title: &str, response: gtk::ResponseType) {
        eprintln!(
            "[native-linux-smoke] stage={} awaiting modal {title:?}, parent={:?}",
            self.step,
            parent.title()
        );
        let parent = parent.clone();
        let title = title.to_owned();
        let failure = self.prompt_error.clone();
        let answered = self.answered_prompts.clone();
        let deadline = Instant::now() + Duration::from_secs(10);
        // Dialog::run does not hide its window, and Window::close queues an idle
        // delete event. Consecutive same-title consents can briefly coexist.
        let existing = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|window| window.downcast::<gtk::Dialog>().ok())
            .collect::<Vec<_>>();
        glib::timeout_add_local(Duration::from_millis(10), move || {
            let dialog = gtk::Window::list_toplevels()
                .into_iter()
                .filter_map(|window| window.downcast::<gtk::Dialog>().ok())
                .find(|dialog| {
                    !existing.contains(dialog) && dialog.is_visible() && dialog.is_mapped()
                });
            if let Some(dialog) = dialog {
                let actual_parent = dialog.transient_for();
                if dialog.title().as_deref() == Some(title.as_str())
                    && actual_parent.as_ref() == Some(&parent)
                {
                    let button = dialog
                        .widget_for_response(response)
                        .and_then(|widget| widget.downcast::<gtk::Button>().ok())
                        .filter(|button| button.is_visible() && button.is_sensitive());
                    if let Some(button) = button {
                        eprintln!(
                            "[native-linux-smoke] answering actual modal {title:?}: {response:?}"
                        );
                        button.clicked();
                        answered.set(answered.get() + 1);
                    } else {
                        *failure.borrow_mut() = Some(format!(
                            "Expected actual GTK modal response button is unavailable: {title}, {response:?}"
                        ));
                        cancel_owned_dialogs();
                    }
                } else {
                    *failure.borrow_mut() = Some(format!(
                        "Unexpected owned GTK modal: title={:?}, parent={:?}; expected {title:?}, parent={:?}",
                        dialog.title(),
                        actual_parent.and_then(|window| window.title()),
                        parent.title(),
                    ));
                    cancel_owned_dialogs();
                }
                glib::ControlFlow::Break
            } else if Instant::now() >= deadline {
                *failure.borrow_mut() =
                    Some(format!("Expected actual GTK modal was not shown: {title}"));
                cancel_owned_dialogs();
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
    }
    fn tick(&mut self, s: &mut Shell) -> Result<()> {
        if self.reported_step != Some(self.step) {
            self.reported_step = Some(self.step);
            eprintln!("[native-linux-smoke] entering stage={}", self.step);
        }
        if let Some(error) = self.prompt_error.borrow_mut().take() {
            bail!(error);
        }
        match self.step {
            0 => {
                if !self.wait(s.settings.window.is_mapped(), 10, "Settings mapping")? {
                    return Ok(());
                }
                self.check(
                    !s.tray_available && s.settings.window.is_visible(),
                    "no-tray launch retains visible Settings and Quit recovery",
                )?;
                self.check(
                    !s.controller.config().global_enabled,
                    "normal global capture is disabled throughout the owned-widget fixture",
                )?;
                self.check(s.settings.smoke_inventory(),"every Config field has its canonical native control or explicit service editor")?;
                s.settings.show_pane(PANES[0]);
                self.next();
            }
            1..=6 => {
                let index = (self.step - 1) as usize;
                self.check(
                    s.settings.smoke_pane() == PANES[index],
                    &format!("six-pane navigation: {}", PANES[index]),
                )?;
                self.capture(
                    &s.settings.window,
                    &format!(
                        "settings-{}.png",
                        PANES[index].to_lowercase().replace(' ', "-")
                    ),
                )?;
                s.settings.show_pane(if index < 5 {
                    PANES[index + 1]
                } else {
                    "General"
                });
                self.next();
            }
            7 => {
                s.settings.smoke_control("debounce_ms", json!(900))?;
                self.check(
                    s.settings.draft.borrow().is_dirty()
                        && s.controller.config().debounce_ms == 100,
                    "native draft edits do not modify the controller before Save",
                )?;
                s.settings.smoke_revert();
                self.next();
            }
            8 => {
                self.check(
                    !s.settings.draft.borrow().is_dirty()
                        && s.controller.config().debounce_ms == 100,
                    "Revert discards the native draft without persistence",
                )?;
                s.settings.smoke_control("debounce_ms", json!(500))?;
                s.settings.smoke_save();
                self.next();
            }
            9 => {
                self.check(
                    s.controller.config().debounce_ms == 500
                        && !s.settings.draft.borrow().is_dirty(),
                    "Save applies only after controller-confirmed persistence",
                )?;
                self.check(
                    crate::config::load_config()?.debounce_ms == 500,
                    "Save is readable from the isolated settings file",
                )?;
                s.settings.show_pane("Languages");
                s.settings.smoke_control(
                    "custom_instructions",
                    json!("Use concise sentences in this synthetic test."),
                )?;
                let mut concurrent = s.controller.config();
                concurrent.complete_inside_text = false;
                s.controller.send(Command::ApplyConfig {
                    config: concurrent,
                    persist: false,
                })?;
                self.next();
            }
            10 => {
                let candidate = s.settings.draft.borrow().apply_to(&s.controller.config())?;
                self.check(!candidate.complete_inside_text&&candidate.custom_instructions=="Use concise sentences in this synthetic test.","live configuration rebases without losing the dirty native instructions editor")?;
                s.settings.show_pane("Autocomplete");
                s.settings.window.resize(1020, 740);
                s.settings.show_pane("Languages");
                s.settings.smoke_save();
                self.next();
            }
            11 => {
                let saved = crate::config::load_config()?;
                self.check(
                    !saved.complete_inside_text
                        && saved.custom_instructions
                            == "Use concise sentences in this synthetic test.",
                    "Save preserves concurrent changes across pane navigation and resizing",
                )?;
                s.settings.show_pane("Privacy");
                self.answer(
                    &s.parent(),
                    "Enable encrypted local learning?",
                    gtk::ResponseType::Cancel,
                );
                s.settings.smoke_history(true);
                self.next();
            }
            12 => {
                self.check(!s.controller.config().record_history&&!s.settings.smoke_history_checked(),"Cancel on the actual history consent dialog leaves recording disabled and its native switch unchecked")?;
                self.answer(
                    &s.settings.window,
                    "Allow optional local context?",
                    gtk::ResponseType::Cancel,
                );
                s.settings.smoke_control("clipboard_context", json!(true))?;
                self.check(
                    s.settings.draft.borrow().value("clipboard_context") == Some(&json!(false)),
                    "declining the actual clipboard consent dialog leaves the draft disabled",
                )?;
                self.answer(
                    &s.settings.window,
                    "Allow optional local context?",
                    gtk::ResponseType::Accept,
                );
                s.settings.smoke_control("clipboard_context", json!(true))?;
                self.check(
                    s.settings.draft.borrow().value("clipboard_context") == Some(&json!(true))
                        && s.settings.draft.borrow().value("screen_context") == Some(&json!(false)),
                    "clipboard consent is independent of Accessibility context",
                )?;
                self.check(
                    self.answered_prompts.get() == 3,
                    "consecutive same-title clipboard dialogs receive distinct Cancel and Continue button responses",
                )?;
                self.next();
            }
            13 => {
                self.answer(
                    &s.settings.window,
                    "Allow optional local context?",
                    gtk::ResponseType::Accept,
                );
                s.settings.smoke_control("screen_context", json!(true))?;
                s.settings
                    .smoke_control("clipboard_context", json!(false))?;
                self.check(s.settings.draft.borrow().value("screen_context")==Some(&json!(true))&&s.settings.draft.borrow().value("clipboard_context")==Some(&json!(false)),"revoking clipboard consent does not revoke the independent Accessibility draft")?;
                self.capture(&s.settings.window, "privacy-independent-consents.png")?;
                self.check(!s.controller.config().clipboard_context&&!s.controller.config().screen_context&&!s.controller.config().record_history&&!s.controller.config().global_enabled,"controller context, recording and normal global capture remain disabled while native consent drafts are edited")?;
                s.settings.smoke_revert();
                self.next();
            }
            14 => {
                self.check(
                    !s.settings.draft.borrow().is_dirty(),
                    "Revert revokes unsaved independent context consents",
                )?;
                s.settings.show_pane("General");
                s.settings.smoke_control("debounce_ms", json!(800))?;
                self.answer(
                    &s.parent(),
                    "Save before closing?",
                    gtk::ResponseType::Cancel,
                );
                s.settings.window.close();
                self.next();
            }
            15 => {
                self.check(
                    s.settings.window.is_visible()
                        && s.settings.draft.borrow().is_dirty()
                        && !s.quitting,
                    "Cancel on dirty-window close keeps the resident controls and draft",
                )?;
                self.answer(
                    &s.parent(),
                    "Save before quitting?",
                    gtk::ResponseType::Cancel,
                );
                click_button(&s.settings.window, "Quit BroTypist")?;
                self.next();
            }
            16 => {
                self.check(
                    s.settings.window.is_visible()
                        && s.settings.draft.borrow().is_dirty()
                        && !s.quitting,
                    "guarded Quit Cancel does not discard edits or begin shutdown",
                )?;
                s.settings.smoke_revert();
                s.sender.send(UiAction::Welcome)?;
                self.next();
            }
            17 => {
                self.check(
                    s.welcome.window.is_visible() && s.welcome.step() == 0,
                    "Welcome opens its canonical first step",
                )?;
                self.capture(&s.welcome.window, "welcome-1.png")?;
                s.welcome.smoke_next();
                self.next();
            }
            18 => {
                self.check(s.welcome.step() == 1, "Welcome local-privacy step")?;
                self.capture(&s.welcome.window, "welcome-2.png")?;
                s.welcome.smoke_back();
                self.step = 180;
                self.entered = Instant::now();
            }
            180 => {
                self.check(
                    s.welcome.step() == 0,
                    "Welcome Back navigates through the canonical native button",
                )?;
                s.welcome.smoke_next();
                self.step = 181;
                self.entered = Instant::now();
            }
            181 => {
                self.check(
                    s.welcome.step() == 1,
                    "Welcome returns forward after native Back",
                )?;
                s.welcome.smoke_next();
                self.step = 19;
                self.entered = Instant::now();
            }
            19 => {
                self.check(s.welcome.step() == 2, "Welcome system-access step")?;
                s.welcome.smoke_next();
                self.next();
            }
            20 => {
                self.check(
                    s.welcome.step() == 2,
                    "Welcome cannot bypass explicit access review",
                )?;
                s.welcome.smoke_review(true);
                self.capture(&s.welcome.window, "welcome-3.png")?;
                s.welcome.smoke_next();
                self.next();
            }
            21 => {
                self.check(
                    s.welcome.step() == 3 && model_selected(&s.controller.config()),
                    "Welcome model step references the explicitly supplied existing GGUF",
                )?;
                self.check(
                    !s.welcome.preview_active(),
                    "selecting a model alone does not create a ready preview session",
                )?;
                self.capture(&s.welcome.window, "welcome-4.png")?;
                s.welcome.smoke_next();
                self.next();
            }
            22 => {
                if !self.wait(
                    s.welcome.step() == 4 && s.welcome.smoke_field().has_focus(),
                    10,
                    "owned Welcome field focus",
                )? {
                    return Ok(());
                }
                let field = s.welcome.smoke_field();
                let buffer = field.buffer().context("Missing owned preview document")?;
                buffer.set_text("I would like to");
                buffer.place_cursor(&buffer.end_iter());
                self.next();
            }
            23 => {
                let result = s.welcome.smoke_result();
                if !self.wait(
                    s.welcome.smoke_drawn() && !text(&result, "suggestion").is_empty(),
                    75,
                    "actual English inference and native ghost drawing",
                )? {
                    return Ok(());
                }
                self.check(
                    s.controller.status()["preview_suspended"] == true,
                    "owned inference waits for the normal controller worker to release",
                )?;
                self.capture(&s.welcome.window, "preview-english-visible.png")?;
                let field = s.welcome.smoke_field();
                self.before = field_text(&field)?;
                self.check(
                    key(&field, false)?,
                    "native GTK acceptance key dispatch was handled by the actual Welcome field",
                )?;
                self.next();
            }
            24 => {
                let field = s.welcome.smoke_field();
                let after = field_text(&field)?;
                let result = s.welcome.smoke_result();
                if !self.wait(
                    after != self.before && result["accepted"].as_u64().unwrap_or(0) >= 1,
                    10,
                    "confirmed English widget readback",
                )? {
                    return Ok(());
                }
                self.check(after.starts_with(&self.before),"English acceptance preserves authored prefix and is counted only after actual GTK document readback")?;
                self.capture(&s.welcome.window, "preview-english-accepted.png")?;
                let buffer = field.buffer().unwrap();
                buffer.set_text("Amanhã vamos");
                buffer.place_cursor(&buffer.end_iter());
                self.next();
            }
            25 => {
                let result = s.welcome.smoke_result();
                if !self.wait(
                    s.welcome.smoke_drawn() && !text(&result, "suggestion").is_empty(),
                    75,
                    "actual Portuguese inference and native ghost drawing",
                )? {
                    return Ok(());
                }
                self.capture(&s.welcome.window, "preview-portuguese-visible.png")?;
                let field = s.welcome.smoke_field();
                self.before = field_text(&field)?;
                self.check(key(&field,true)?,"configured Above-Tab scan identity reaches the canonical full-accept GTK handler")?;
                self.next();
            }
            26 => {
                let field = s.welcome.smoke_field();
                let after = field_text(&field)?;
                let result = s.welcome.smoke_result();
                if !self.wait(
                    after != self.before && result["accepted"].as_u64().unwrap_or(0) >= 2,
                    10,
                    "confirmed Portuguese widget readback",
                )? {
                    return Ok(());
                }
                self.check(after.starts_with(&self.before),"Portuguese full acceptance preserves the document and receives native readback confirmation")?;
                self.capture(&s.welcome.window, "preview-portuguese-accepted.png")?;
                let buffer = field.buffer().unwrap();
                buffer.select_range(&buffer.start_iter(), &buffer.iter_at_offset(3));
                self.next();
            }
            27 => {
                self.check(
                    !s.welcome.smoke_drawn()
                        && text(&s.welcome.smoke_result(), "suggestion").is_empty(),
                    "selection invalidates the actual owned-field preview",
                )?;
                let field = s.welcome.smoke_field();
                let buffer = field.buffer().unwrap();
                buffer.place_cursor(&buffer.end_iter());
                field.emit_by_name::<()>("preedit-changed", &[&"synthetic-preedit"]);
                self.next();
            }
            28 => {
                self.check(s.welcome.smoke_composing()&&!s.welcome.smoke_drawn(),"native GTK preedit-changed composition signal cancels and refuses preview acceptance")?;
                let field = s.welcome.smoke_field();
                field.emit_by_name::<()>("preedit-changed", &[&""]);
                s.settings.show_pane("General");
                self.next();
            }
            29 => {
                if !self.wait(
                    s.welcome.preview_stopped() && s.controller.status()["preview"] == false,
                    15,
                    "preview process retirement after focus loss",
                )? {
                    return Ok(());
                }
                self.check(!s.welcome.smoke_drawn(),"focus loss retires the isolated preview before restoring normal controller ownership")?;
                s.sender.send(UiAction::Welcome)?;
                self.next();
            }
            30 => {
                s.welcome.smoke_next();
                self.next();
            }
            31 => {
                if !self.wait(
                    s.welcome.step() == 5 && s.welcome.preview_stopped(),
                    15,
                    "final Welcome step and preview shutdown",
                )? {
                    return Ok(());
                }
                self.check(
                    s.welcome.step() == 5,
                    "Welcome completes all six native steps",
                )?;
                self.capture(&s.welcome.window, "welcome-6.png")?;
                s.welcome.smoke_next();
                self.next();
            }
            32 => {
                self.check(
                    !s.welcome.window.is_visible() && s.settings.window.is_visible() && !s.quitting,
                    "Welcome Finish without a tray returns to visible Settings, not a hidden loop",
                )?;
                self.answer(&s.parent(), "Quit BroTypist?", gtk::ResponseType::Cancel);
                s.settings.window.close();
                self.next();
            }
            33 => {
                self.check(
                    s.settings.window.is_visible() && !s.quitting,
                    "no-tray window-close Cancel retains visible recovery controls",
                )?;
                self.check(
                    self.answered_prompts.get() == 7,
                    "all seven consent, dirty-draft and no-tray prompts were answered through their actual native buttons",
                )?;
                self.completed = true;
                click_button(&s.settings.window, "Quit BroTypist")?;
                self.cleanup_at.set(Some(Instant::now()));
                self.next();
            }
            _ => {}
        }
        Ok(())
    }
}
fn field_text(field: &gtk::TextView) -> Result<String> {
    let b = field
        .buffer()
        .context("Owned preview buffer is unavailable")?;
    Ok(b.text(&b.start_iter(), &b.end_iter(), false)
        .unwrap_or_default()
        .into())
}
fn click_button(root: &impl IsA<gtk::Widget>, title: &str) -> Result<()> {
    fn find(widget: &gtk::Widget, title: &str) -> Option<gtk::Button> {
        if let Some(button) = widget.downcast_ref::<gtk::Button>() {
            if button.label().as_deref() == Some(title)
                && button.is_visible()
                && button.is_sensitive()
            {
                return Some(button.clone());
            }
        }
        widget
            .downcast_ref::<gtk::Container>()
            .and_then(|c| c.children().iter().find_map(|child| find(child, title)))
    }
    find(root.as_ref(), title)
        .with_context(|| format!("Missing actual native button: {title}"))?
        .clicked();
    Ok(())
}
/// Dispatch only into the owned GTK widget. This does not inject global X11 input and is not physical-key evidence.
fn key(field: &gtk::TextView, full: bool) -> Result<bool> {
    ensure!(
        field.has_focus(),
        "Preview acceptance must retain owned-field focus."
    );
    let mut event = gdk::Event::new(gdk::EventType::KeyPress);
    let key = event
        .downcast_mut::<gdk::EventKey>()
        .context("Cannot construct an owned GTK key event")?;
    let raw = key.as_mut();
    raw.keyval = (if full {
        gdk::keys::constants::grave
    } else {
        gdk::keys::constants::Tab
    })
    .into_glib();
    raw.hardware_keycode = if full { 49 } else { 23 };
    raw.state = 0;
    raw.send_event = 1;
    if let Some(window) = gtk::prelude::TextViewExt::window(field, gtk::TextWindowType::Text) {
        use glib::translate::ToGlibPtr;
        raw.window = window.to_glib_full();
    }
    Ok(field.event(&event))
}
fn gate() -> Result<PathBuf> {
    let opted = std::env::var("BROTYPIST_OWNED_DESKTOP_SMOKE").as_deref() == Ok("1")
        || matches!(std::env::var("CI").as_deref(), Ok("true" | "1"));
    ensure!(
        opted,
        "Native UI smoke requires CI or BROTYPIST_OWNED_DESKTOP_SMOKE=1."
    );
    let profile = PathBuf::from(
        std::env::var_os("INKLOW_HOME")
            .context("Native UI smoke requires an explicit empty private INKLOW_HOME.")?,
    );
    ensure!(profile.is_absolute(), "The smoke profile must be absolute.");
    let metadata = fs::symlink_metadata(&profile)?;
    ensure!(
        metadata.is_dir()
            && !metadata.file_type().is_symlink()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.permissions().mode() & 0o077 == 0,
        "Native smoke requires an owned private profile directory."
    );
    for entry in fs::read_dir(&profile)? {
        ensure!(
            entry?.file_name() == "instance.lock",
            "The smoke profile must be empty except for the common single-owner instance lock."
        );
    }
    ensure!(
        !crate::platform::wayland::is_prepared(),
        "This X11 GTK fixture does not claim a private KWin compositor smoke."
    );
    Ok(profile)
}
pub(super) fn run(output: &Path) -> Result<i32> {
    let profile = gate()?;
    ensure!(output.is_absolute(), "Evidence output must be absolute.");
    if !output.exists() {
        fs::create_dir_all(output)?;
    }
    let metadata = fs::symlink_metadata(output)?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "Evidence destination must be a directory, not a symlink."
    );
    ensure!(
        !output.join("receipt.json").exists(),
        "Preserve previous evidence; choose a fresh output directory."
    );
    let prerequisite = (|| -> Result<PathBuf> {
        let model = crate::ui::smoke_model_path()?;
        super::models::validate_import(&model)?;
        Ok(model.canonicalize()?)
    })();
    let model = match prerequisite {
        Ok(model) => model,
        Err(error) => {
            write_receipt(
                output,
                &json!({"platform":"linux","passed":false,"outcome":"unexercised","error":error.to_string(),"assertions":[],"screenshots":[]}),
            )?;
            return Err(error);
        }
    };
    let mut config = Config::default();
    config.model_path = model
        .to_str()
        .context("Smoke model filename must be UTF-8")?
        .into();
    config.prompt_style =
        std::env::var("BROTYPIST_SMOKE_PROMPT_STYLE").unwrap_or_else(|_| "continuation".into());
    config.setup_completed = true;
    config.check_updates_automatically = false;
    config.global_enabled = false;
    config.debounce_ms = 100;
    config.min_chars = 1;
    config.max_tokens = 16;
    config.completion_length = "short".into();
    config.acceleration = "cpu".into();
    config.validate()?;
    let shell = match create_shell(
        config,
        LaunchOptions {
            no_tray: true,
            pane: Some("General".into()),
            ..Default::default()
        },
    ) {
        Ok(shell) => shell,
        Err(error) => {
            write_receipt(
                output,
                &json!({"platform":"linux","passed":false,"outcome":"failed","error":error.to_string(),"assertions":[],"screenshots":[]}),
            )?;
            return Err(error);
        }
    };
    let display = gdk::Display::default().context("GTK display unavailable")?;
    ensure!(
        display.type_().name().contains("X11"),
        "This smoke requires the explicitly owned X11 GTK display."
    );
    let started = Instant::now();
    let cleanup_at = Rc::new(Cell::new(None::<Instant>));
    let prompt_error = Rc::new(RefCell::new(None::<String>));
    let shutdown_started = cleanup_at.clone();
    let failure = prompt_error.clone();
    // Unlike the scenario and resident timers, this callback does not borrow Shell.
    // Dialog::run holds that borrow while dispatching a nested GLib loop.
    let watchdog = glib::timeout_add_local(Duration::from_millis(50), move || {
        if started.elapsed() >= Duration::from_secs(210) && shutdown_started.get().is_none() {
            *failure.borrow_mut() =
                Some("Native UI smoke exceeded its bounded scenario deadline.".into());
            shutdown_started.set(Some(Instant::now()));
        }
        if let Some(at) = shutdown_started.get() {
            cancel_owned_dialogs();
            if at.elapsed() >= Duration::from_secs(20) {
                let mut failure = failure.borrow_mut();
                if failure.is_none() {
                    *failure = Some(
                        "Canonical Quit did not complete its bounded ownership cleanup.".into(),
                    );
                }
                eprintln!("[native-linux-smoke] cleanup deadline expired");
                gtk::main_quit();
            }
        }
        glib::ControlFlow::Continue
    });
    let state = Rc::new(RefCell::new(Scenario {
        step: 0,
        reported_step: None,
        entered: Instant::now(),
        started,
        output: output.into(),
        checks: Vec::new(),
        images: Vec::new(),
        before: String::new(),
        completed: false,
        failure: None,
        cleanup_at,
        prompt_error,
        answered_prompts: Rc::new(Cell::new(0)),
    }));
    let scenario = state.clone();
    let weak = Rc::downgrade(&shell);
    let alive = Rc::new(std::cell::Cell::new(true));
    let running = alive.clone();
    let source = glib::timeout_add_local(Duration::from_millis(120), move || {
        let Some(shell) = weak.upgrade() else {
            running.set(false);
            return glib::ControlFlow::Break;
        };
        let Ok(mut s) = shell.try_borrow_mut() else {
            return glib::ControlFlow::Continue;
        };
        let Ok(mut scenario) = scenario.try_borrow_mut() else {
            return glib::ControlFlow::Continue;
        };
        if s.stopped {
            running.set(false);
            return glib::ControlFlow::Break;
        }
        if scenario.failure.is_none() {
            let result = scenario.tick(&mut s);
            if let Err(error) = result {
                scenario.failure = Some(error.to_string());
                if scenario.cleanup_at.get().is_none() {
                    scenario.cleanup_at.set(Some(Instant::now()));
                }
                eprintln!(
                    "[native-linux-smoke] stage={} failed: {error}",
                    scenario.step
                );
                s.settings.smoke_revert();
                let _ = s.sender.send(UiAction::Quit);
            }
        }
        glib::ControlFlow::Continue
    });
    let result = run_shell(shell.clone());
    watchdog.remove();
    if alive.get() {
        source.remove();
    }
    let mut scenario = state.borrow_mut();
    if scenario.failure.is_none() {
        let error = scenario.prompt_error.borrow_mut().take();
        scenario.failure = error;
    }
    let s = shell.borrow();
    let clean = s.stopped
        && s.controller.status()["shutdown_complete"] == true
        && s.welcome.preview_stopped()
        && s.clipboard.is_none()
        && s.power.is_none();
    if clean {
        scenario.checks.push("actual canonical Quit completed controller, preview, clipboard and power ownership cleanup and exited GTK".into());
    }
    let passed = result.is_ok() && scenario.completed && scenario.failure.is_none() && clean;
    let receipt = json!({"schema":1,"platform":"linux","ui":"GTK3 native canonical resident shell","passed":passed,"outcome":if passed{"passed"}else{"failed"},"error":scenario.failure,"last_stage":scenario.step,"modal_responses":scenario.answered_prompts.get(),"assertions":scenario.checks,"screenshots":scenario.images,"clean_exit":clean,"elapsed_ms":scenario.started.elapsed().as_millis(),"profile":profile,"model":model.file_name().map(|s|s.to_string_lossy().into_owned()),"native_status":s.controller.status()["native_status"],"input":"GTK owned-widget event dispatch; no global synthetic input","limits":["Not physical keyboard or external host-edit proof","Not a real KWin/Wayland compositor scenario","Does not register login, change permissions, read history/credentials, download models or install updates"]});
    write_receipt(output, &receipt)?;
    result?;
    ensure!(
        passed,
        "Native Linux UI smoke failed; inspect receipt.json."
    );
    Ok(0)
}
fn write_receipt(output: &Path, receipt: &Value) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(output.join("receipt.json"))?;
    file.write_all(&serde_json::to_vec_pretty(receipt)?)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_screenshot_requires_visible_rendered_content() {
        let mut pixels = vec![0; 32 * 32 * 3];
        assert!(!image_has_content(&pixels, 32, 32, 96, 3));
        pixels.fill(255);
        assert!(!image_has_content(&pixels, 32, 32, 96, 3));
        pixels[96..192].fill(0);
        assert!(image_has_content(&pixels, 32, 32, 96, 3));
        assert!(!image_has_content(&pixels[..100], 32, 32, 96, 3));
    }

    #[test]
    fn transparent_or_padding_bytes_do_not_count_as_controls() {
        let mut transparent = vec![0; 32 * 32 * 4];
        transparent[..256].fill(255);
        for alpha in transparent.iter_mut().skip(3).step_by(4) {
            *alpha = 0;
        }
        assert!(!image_has_content(&transparent, 32, 32, 128, 4));
        let mut padded = vec![0; 100 * 32];
        for padding in padded.chunks_exact_mut(100) {
            padding[96..].fill(255);
        }
        assert!(!image_has_content(&padded, 32, 32, 100, 3));
    }
}
