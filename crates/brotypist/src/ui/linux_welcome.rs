use super::*;
use std::cell::Cell;
#[derive(Clone)]
struct Paint {
    text: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}
pub(super) struct WelcomeWindow {
    pub window: gtk::Window,
    stack: gtk::Stack,
    index: Cell<usize>,
    reviewed: gtk::CheckButton,
    next: gtk::Button,
    back: gtk::Button,
    hint: gtk::Label,
    native: gtk::Label,
    model: gtk::Label,
    summary: gtk::Label,
    field: gtk::TextView,
    ghost: gtk::DrawingArea,
    paint: Rc<RefCell<Option<Paint>>>,
    drawn: Rc<Cell<bool>>,
    composing: Rc<Cell<bool>>,
    session: Option<crate::preview::PreviewSession>,
    closing: bool,
    suspended: bool,
    try_status: gtk::Label,
    config: Config,
    sender: Sender,
    accepted_keys: Rc<RefCell<Vec<String>>>,
    last_owned: Option<(String, usize, usize)>,
    login: gtk::CheckButton,
    loading: Rc<Cell<bool>>,
    key_hint: gtk::Label,
}
const TITLES: [&str; 6] = [
    "Welcome to BroTypist",
    "Your writing stays local",
    "System access",
    "Choose a model",
    "Try it",
    "You're all set",
];
fn page(stack: &gtk::Stack, index: usize) -> gtk::Box {
    let area = gtk::ScrolledWindow::new(None::<&gtk::Adjustment>, None::<&gtk::Adjustment>);
    area.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    let p = gtk::Box::new(gtk::Orientation::Vertical, 16);
    p.set_margin_start(48);
    p.set_margin_end(48);
    p.set_margin_top(28);
    p.set_margin_bottom(24);
    let title = label(TITLES[index]);
    title.style_context().add_class("title");
    p.pack_start(&title, false, false, 0);
    area.add(&p);
    stack.add_named(&area, &index.to_string());
    p
}
pub(super) fn preview_eligible(
    focused: bool,
    composing: bool,
    selection: usize,
    line_end: bool,
) -> bool {
    focused && !composing && selection == 0 && line_end
}
impl WelcomeWindow {
    pub fn new(config: &Config, sender: &Sender) -> Self {
        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        window.set_title("Welcome to BroTypist");
        window.set_default_size(780, 620);
        window.set_size_request(760, 580);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
        window.add(&root);
        let stack = gtk::Stack::new();
        root.pack_start(&stack, true, true, 0);
        let p = page(&stack, 0);
        p.pack_start(
            &app_image(112).expect("Validated embedded application artwork"),
            false,
            false,
            0,
        );
        for text in [
            "A cursor, then the words that come next.",
            "Runs on this computer — a local model; no account or cloud service required.",
            "Stays out of the way — faint suggestions in supported apps. Accept what helps; keep typing to ignore the rest.",
            "Yours to control — choose a model and review access, optional context and encrypted history.",
        ] {
            p.pack_start(&label(text), false, false, 0);
        }
        let p = page(&stack, 1);
        for text in [
            "Text before cursor → local model → suggestion",
            "Protected fields and excluded applications are skipped. Unsupported fields remain silent rather than guessing text access or geometry.",
            "Local GGUF predictions do not upload writing. Local servers must use literal loopback URLs; their own logging and network behavior remain server-owned.",
            "Clipboard, focused-window Accessibility context and encrypted learning are independent opt-ins. Per-app recording overrides may enable recording even when the default is off. Review and delete history in Privacy.",
        ] {
            p.pack_start(&label(text), false, false, 0);
        }
        p.pack_start(
            &button(
                "Review Privacy settings…",
                sender,
                UiAction::Settings("Privacy".into()),
            ),
            false,
            false,
            0,
        );
        let p = page(&stack, 2);
        let native = label("");
        p.pack_start(&native, false, false, 0);
        p.pack_start(&label("X11 uses AT-SPI and replayable key grabs. Opening a permissions page is not proof of access. KWin Wayland requires explicitly selecting BroTypist in Virtual Keyboard settings; manual controls only, ordinary inline/preedit disabled. No generic popup fallback or universal compositor support is claimed."),false,false,0);
        let permissions = gtk::Button::with_label("Open system access settings…");
        permissions.connect_clicked(|_| {
            if let Err(e) = system::open_permissions("accessibility") {
                error(None, e)
            }
        });
        p.pack_start(&permissions, false, false, 0);
        p.pack_start(
            &button(
                "Review / install KDE input-method entry…",
                sender,
                UiAction::Settings("Privacy".into()),
            ),
            false,
            false,
            0,
        );
        let reviewed = gtk::CheckButton::with_label(
            "I have reviewed the access requirements for this desktop",
        );
        p.pack_start(&reviewed, false, false, 0);
        let p = page(&stack, 3);
        p.pack_start(&label("Choose an existing pinned catalog model or a local GGUF file. Downloads, license acceptance and model changes happen only on explicit request. Selecting a model does not make it ready or run inference."),false,false,0);
        let model = label("");
        p.pack_start(&model, false, false, 0);
        p.pack_start(
            &button(
                "Choose or download a local model…",
                sender,
                UiAction::Models,
            ),
            false,
            false,
            0,
        );
        p.pack_start(
            &button(
                "Configure local server…",
                sender,
                UiAction::Settings("AI Models".into()),
            ),
            false,
            false,
            0,
        );
        let p = page(&stack, 4);
        p.pack_start(&label("Type below to use your actual selected local model. Only this owned field is sent: no external context, history or insertion into another app. Try English or Portuguese."),false,false,0);
        let overlay = gtk::Overlay::new();
        let field = gtk::TextView::new();
        field.set_wrap_mode(gtk::WrapMode::WordChar);
        field.set_left_margin(12);
        field.set_right_margin(12);
        field.set_top_margin(12);
        field.set_bottom_margin(12);
        field.set_widget_name("welcome_try_field");
        let scroll = gtk::ScrolledWindow::new(None::<&gtk::Adjustment>, None::<&gtk::Adjustment>);
        scroll.set_min_content_height(180);
        scroll.add(&field);
        overlay.add(&scroll);
        let ghost = gtk::DrawingArea::new();
        ghost.set_halign(gtk::Align::Fill);
        ghost.set_valign(gtk::Align::Fill);
        ghost.set_hexpand(true);
        ghost.set_vexpand(true);
        overlay.add_overlay(&ghost);
        overlay.set_overlay_pass_through(&ghost, true);
        p.pack_start(&overlay, true, true, 0);
        let paint = Rc::new(RefCell::new(None::<Paint>));
        let drawn = Rc::new(Cell::new(false));
        let composing = Rc::new(Cell::new(false));
        let paint_draw = paint.clone();
        let drawn_draw = drawn.clone();
        let f = field.clone();
        ghost.connect_draw(move |_, cr| {
            drawn_draw.set(false);
            if let Some(p) = paint_draw.borrow().as_ref() {
                if !f.has_focus() {
                    return glib::Propagation::Proceed;
                }
                let layout = f.create_pango_layout(Some(&p.text));
                let (w, h) = layout.pixel_size();
                if f64::from(w) > p.width || f64::from(h) > p.height {
                    return glib::Propagation::Proceed;
                }
                let _ = cr.save();
                cr.rectangle(p.x, p.y, p.width, p.height);
                cr.clip();
                cr.set_source_rgb(0.565, 0.573, 0.6);
                cr.move_to(p.x, p.y);
                pangocairo::functions::show_layout(cr, &layout);
                let ok = cr.status().is_ok();
                let _ = cr.restore();
                drawn_draw.set(ok);
            }
            glib::Propagation::Proceed
        });
        let try_status = label("Waiting for you to type…");
        p.pack_start(&try_status, false, false, 0);
        let key_hint = label(&format!(
            "{}: next word · {}: full suggestion · Escape: dismiss",
            config
                .shortcuts
                .get("word")
                .map(String::as_str)
                .unwrap_or("Unassigned"),
            config
                .shortcuts
                .get("all")
                .map(String::as_str)
                .unwrap_or("Unassigned")
        ));
        p.pack_start(&key_hint, false, false, 0);
        let p = page(&stack, 5);
        p.pack_start(&label("Use the resident menu to pause, resume, open Settings or manage models. Quit stops typing assistance; it never changes KDE's selected input method."),false,false,0);
        let summary = label("");
        p.pack_start(&summary, false, false, 0);
        p.pack_start(&label("Without a tray host, finishing returns to visible Settings. Closing Settings or Welcome offers a real Quit action; Quit is always visible below."),false,false,0);
        let login = gtk::CheckButton::with_label("Launch BroTypist at login");
        login.set_active(config.launch_at_login);
        p.pack_start(&login, false, false, 0);
        let loading = Rc::new(Cell::new(false));
        let guard = loading.clone();
        let tx = sender.clone();
        login.connect_toggled(move |c| {
            if !guard.get() {
                let _ = tx.send(UiAction::Login(c.is_active()));
            }
        });
        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        footer.set_margin_start(24);
        footer.set_margin_end(24);
        footer.set_margin_bottom(16);
        footer.pack_start(
            &button("Quit BroTypist", sender, UiAction::Quit),
            false,
            false,
            0,
        );
        let back = button("Back", sender, UiAction::WelcomeBack);
        footer.pack_start(&back, false, false, 0);
        let hint = label("");
        footer.pack_start(&hint, true, true, 0);
        let next = button("Get started", sender, UiAction::WelcomeNext);
        footer.pack_end(&next, false, false, 0);
        root.pack_end(&footer, false, false, 0);
        let tx = sender.clone();
        window.connect_delete_event(move |_, _| {
            let _ = tx.send(UiAction::CloseWelcome);
            glib::Propagation::Stop
        });
        let tx = sender.clone();
        window.connect_key_press_event(move |_, e| {
            if e.state().contains(gdk::ModifierType::CONTROL_MASK)
                && e.keyval() == gdk::keys::constants::q
            {
                let _ = tx.send(UiAction::Quit);
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        let tx = sender.clone();
        let clear = paint.clone();
        let drawn_clear = drawn.clone();
        let g = ghost.clone();
        field.buffer().unwrap().connect_changed(move |_| {
            *clear.borrow_mut() = None;
            drawn_clear.set(false);
            g.queue_draw();
            let _ = tx.send(UiAction::PreviewChanged);
        });
        let tx = sender.clone();
        let clear = paint.clone();
        let drawn_clear = drawn.clone();
        let g = ghost.clone();
        field.buffer().unwrap().connect_mark_set(move |_, _, _| {
            *clear.borrow_mut() = None;
            drawn_clear.set(false);
            g.queue_draw();
            let _ = tx.send(UiAction::PreviewChanged);
        });
        for focus_in in [true, false] {
            let tx = sender.clone();
            let clear = paint.clone();
            let drawn_clear = drawn.clone();
            let g = ghost.clone();
            if focus_in {
                field.connect_focus_in_event(move |_, _| {
                    *clear.borrow_mut() = None;
                    drawn_clear.set(false);
                    g.queue_draw();
                    let _ = tx.send(UiAction::PreviewChanged);
                    glib::Propagation::Proceed
                });
            } else {
                field.connect_focus_out_event(move |_, _| {
                    *clear.borrow_mut() = None;
                    drawn_clear.set(false);
                    g.queue_draw();
                    let _ = tx.send(UiAction::PreviewChanged);
                    glib::Propagation::Proceed
                });
            }
        }
        let tx = sender.clone();
        let ime = composing.clone();
        let clear = paint.clone();
        let drawn_clear = drawn.clone();
        let g = ghost.clone();
        field.connect_preedit_changed(move |_, text| {
            ime.set(!text.is_empty());
            *clear.borrow_mut() = None;
            drawn_clear.set(false);
            g.queue_draw();
            let _ = tx.send(UiAction::PreviewChanged);
        });
        let accepted_keys = Rc::new(RefCell::new(Vec::new()));
        let keys = accepted_keys.clone();
        let tx = sender.clone();
        let ime = composing.clone();
        let visible = drawn.clone();
        field.connect_key_press_event(move |_, event| {
            if ime.get() {
                return glib::Propagation::Proceed;
            }
            if event.keyval() == gdk::keys::constants::Escape {
                let _ = tx.send(UiAction::PreviewKey("Escape".into()));
                return glib::Propagation::Stop;
            }
            if visible.get() {
                if let Ok(chord) = settings::event_chord(event) {
                    if keys.borrow().contains(&chord) {
                        let _ = tx.send(UiAction::PreviewKey(chord));
                        return glib::Propagation::Stop;
                    }
                }
            }
            glib::Propagation::Proceed
        });
        if let Some(a) = field.accessible() {
            a.set_name("Try local predictions");
        }
        if let Some(a) = ghost.accessible() {
            a.set_name("Local model suggestion");
        }
        let mut s = Self {
            window,
            stack,
            index: Cell::new(0),
            reviewed,
            next,
            back,
            hint,
            native,
            model,
            summary,
            field,
            ghost,
            paint,
            drawn,
            composing,
            session: None,
            closing: false,
            suspended: false,
            try_status,
            config: config.clone(),
            sender: sender.clone(),
            accepted_keys,
            last_owned: None,
            login,
            loading,
            key_hint,
        };
        s.config_changed(config);
        s.select(0);
        s
    }
    pub fn step(&self) -> usize {
        self.index.get()
    }
    pub fn select(&mut self, index: usize) {
        self.close_preview();
        let index = index.min(5);
        self.index.set(index);
        self.stack.set_visible_child_name(&index.to_string());
        self.back.set_sensitive(index > 0);
        self.next.set_label(
            [
                "Get started",
                "Continue",
                "Continue",
                "Continue",
                "Finish",
                "Start typing",
            ][index],
        );
        self.hint.set_text(&format!("Step {} of 6", index + 1));
        if index == 4 {
            self.field.grab_focus();
            let _ = self.sender.send(UiAction::PreviewChanged);
        }
    }
    pub fn show(&mut self) {
        self.window.show_all();
        self.window.present();
    }
    pub fn hide(&mut self) {
        self.close_preview();
        self.window.hide();
    }
    pub fn can_continue(&self, c: &Config) -> bool {
        (self.step() != 2 || self.reviewed.is_active()) && (self.step() != 3 || model_selected(c))
    }
    pub fn config_changed(&mut self, c: &Config) {
        if self.config.provider_identity() != c.provider_identity()
            || self.config.prompt_style != c.prompt_style
            || self.config.custom_instructions != c.custom_instructions
        {
            self.close_preview();
        }
        self.config = c.clone();
        *self.accepted_keys.borrow_mut() = ["word", "all"]
            .iter()
            .filter_map(|k| c.shortcuts.get(*k))
            .filter(|c| !c.is_empty())
            .cloned()
            .collect();
        self.loading.set(true);
        self.login.set_active(c.launch_at_login);
        self.loading.set(false);
        self.summary.set_text(&format!(
            "Model: {}\nNext word: {}\nFull suggestion: {}\nDismiss: Escape",
            if c.backend == "gguf" {
                &c.model_path
            } else {
                &c.model
            },
            c.shortcuts
                .get("word")
                .map(String::as_str)
                .unwrap_or("Unassigned"),
            c.shortcuts
                .get("all")
                .map(String::as_str)
                .unwrap_or("Unassigned")
        ));
        self.key_hint.set_text(&format!(
            "{}: next word · {}: full suggestion · Escape: dismiss",
            c.shortcuts
                .get("word")
                .map(String::as_str)
                .unwrap_or("Unassigned"),
            c.shortcuts
                .get("all")
                .map(String::as_str)
                .unwrap_or("Unassigned")
        ));
    }
    fn owned(&self) -> (String, usize, usize) {
        let buffer = self.field.buffer().unwrap();
        let text = buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .unwrap_or_default()
            .to_string();
        if let Some((a, b)) = buffer.selection_bounds() {
            (
                text,
                a.offset() as usize,
                (b.offset() - a.offset()) as usize,
            )
        } else {
            let caret = buffer
                .get_insert()
                .map(|mark| buffer.iter_at_mark(&mark).offset() as usize)
                .unwrap_or(0);
            (text, caret, 0)
        }
    }
    pub fn preview_active(&self) -> bool {
        self.session.is_some() || self.suspended
    }
    pub fn update_preview(&mut self, controller: &Controller) -> Result<()> {
        *self.paint.borrow_mut() = None;
        self.drawn.set(false);
        self.ghost.queue_draw();
        let wanted = self.window.is_visible()
            && self.step() == 4
            && self.field.has_focus()
            && !self.composing.get();
        if !wanted {
            self.close_preview();
            return Ok(());
        }
        if self.closing {
            return Ok(());
        }
        if self.session.is_none() {
            controller.send(Command::SuspendForPreview)?;
            self.suspended = true;
            match crate::preview::PreviewSession::new(controller.config()) {
                Ok(session) => self.session = Some(session),
                Err(e) => {
                    controller.send(Command::ResumeFromPreview)?;
                    self.suspended = false;
                    return Err(e);
                }
            }
        }
        let (text, caret, length) = self.owned();
        if let Some(p) = &mut self.session {
            p.set_active(wanted);
            p.set_owner_released(controller.status()["preview_suspended"] == true);
            p.update(&text, caret, length)?;
        }
        self.last_owned = Some((text, caret, length));
        Ok(())
    }
    pub fn close_preview(&mut self) {
        self.closing = self.session.is_some();
        if let Some(p) = &mut self.session {
            p.set_active(false);
            let _ = p.close();
        }
        *self.paint.borrow_mut() = None;
        self.drawn.set(false);
        self.ghost.queue_draw();
        self.last_owned = None;
    }
    pub fn preview_stopped(&self) -> bool {
        self.session.is_none()
    }
    pub fn accept(&mut self, chord: &str, controller: &Controller) -> Result<()> {
        let owned = self.owned();
        let unchanged = self.last_owned.as_ref() == Some(&owned);
        let (_, caret, selection) = &owned;
        let visible = self.drawn.get() && self.paint.borrow().is_some();
        if chord == "Escape" {
            if let Some(p) = &mut self.session {
                p.dismiss();
            }
            *self.paint.borrow_mut() = None;
            self.drawn.set(false);
            self.ghost.queue_draw();
            return Ok(());
        }
        if !visible
            || !unchanged
            || !preview_eligible(
                self.field.has_focus(),
                self.composing.get(),
                *selection,
                true,
            )
        {
            self.update_preview(controller)?;
            return Ok(());
        }
        let full = self.config.shortcuts.get("all").is_some_and(|c| c == chord);
        let word = self
            .config
            .shortcuts
            .get("word")
            .is_some_and(|c| c == chord);
        if !full && !word {
            return Ok(());
        }
        let Some(p) = &mut self.session else {
            return Ok(());
        };
        let insertion = {
            let frame = self.paint.borrow();
            let Some(frame) = frame.as_ref() else {
                return Ok(());
            };
            p.accept_visible(full, &frame.text)
        };
        if insertion.is_empty() {
            return Ok(());
        }
        let buffer = self.field.buffer().unwrap();
        let mut at = buffer.iter_at_offset(*caret as i32);
        buffer.insert(&mut at, &insertion);
        buffer.place_cursor(&at);
        // Re-read actual widget text and scalar selection. The preview counts only exact readback.
        self.update_preview(controller)
    }
    pub fn tick(&mut self, controller: &Controller) {
        let status = controller.status();
        self.native.set_text(&text(&status, "native_status"));
        self.model.set_text(&format!("Selected: {}\n{}",if self.config.backend=="gguf"{&self.config.model_path}else{&self.config.model},if model_selected(&self.config){"Selected file/configuration exists. Availability is checked by actual Try it inference."}else{"No model is selected. Choose one to continue."}));
        self.next.set_sensitive(self.can_continue(&self.config));
        if self.closing {
            let closed = self
                .session
                .as_mut()
                .is_none_or(|p| p.close().unwrap_or(false));
            if closed {
                self.session = None;
                self.closing = false;
                if self.suspended {
                    let _ = controller.send(Command::ResumeFromPreview);
                    self.suspended = false;
                }
                if self.window.is_visible() && self.step() == 4 && self.field.has_focus() {
                    let _ = self.update_preview(controller);
                }
            }
            return;
        }
        if !self.window.is_visible()
            || self.step() != 4
            || !self.field.has_focus()
            || self.composing.get()
        {
            if self.session.is_some() {
                self.close_preview();
            }
            return;
        }
        if self.session.is_none() {
            let _ = self.update_preview(controller);
        }
        let Some(p) = &mut self.session else {
            return;
        };
        p.set_owner_released(status["preview_suspended"] == true);
        let result = p.poll();
        self.try_status.set_text(&format!(
            "{}\nConfirmed accepts: {}",
            text(&result, "message"),
            result["accepted"].as_u64().unwrap_or(0)
        ));
        let suggestion = text(&result, "suggestion");
        let buffer = self.field.buffer().unwrap();
        let Some(mark) = buffer.get_insert() else {
            return;
        };
        let caret = buffer.iter_at_mark(&mark);
        let selected = buffer.selection_bounds().is_some();
        if suggestion.is_empty()
            || selected
            || !caret.ends_line()
            || suggestion.contains(['\r', '\n'])
        {
            *self.paint.borrow_mut() = None;
            self.drawn.set(false);
            self.ghost.queue_draw();
            return;
        }
        let rect = self.field.iter_location(&caret);
        let (x, y) =
            self.field
                .buffer_to_window_coords(gtk::TextWindowType::Widget, rect.x(), rect.y());
        let (tx, ty) = self
            .field
            .translate_coordinates(&self.ghost, x, y)
            .unwrap_or((x, y));
        let layout = self.field.create_pango_layout(Some(&suggestion));
        let (w, h) = layout.pixel_size();
        let width = f64::from(self.ghost.allocated_width() - tx - 12);
        let height = f64::from(self.ghost.allocated_height() - ty);
        // Never arm a whole suggestion when some of it is clipped. No ellipsis counts as visible text.
        self.drawn.set(false);
        if tx < 0
            || ty < 0
            || f64::from(w) > width
            || f64::from(h) > height
            || width <= 0.
            || height <= 0.
        {
            *self.paint.borrow_mut() = None;
        } else {
            *self.paint.borrow_mut() = Some(Paint {
                text: suggestion,
                x: f64::from(tx),
                y: f64::from(ty),
                width,
                height,
            });
        }
        self.ghost.queue_draw();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selection_focus_and_ime_refuse_preview() {
        assert!(preview_eligible(true, false, 0, true));
        for (focus, ime, selection, end) in [
            (false, false, 0, true),
            (true, true, 0, true),
            (true, false, 1, true),
            (true, false, 0, false),
        ] {
            assert!(!preview_eligible(focus, ime, selection, end));
        }
    }
    #[test]
    fn welcome_has_exact_six_steps() {
        assert_eq!(TITLES.len(), 6);
        assert_eq!(TITLES[3], "Choose a model");
        assert_eq!(TITLES[4], "Try it");
    }
}

#[cfg(feature = "native-smoke")]
impl WelcomeWindow {
    pub(super) fn smoke_next(&self) {
        self.next.clicked();
    }
    pub(super) fn smoke_back(&self) {
        self.back.clicked();
    }
    pub(super) fn smoke_review(&self, reviewed: bool) {
        self.reviewed.set_active(reviewed);
    }
    pub(super) fn smoke_field(&self) -> gtk::TextView {
        self.field.clone()
    }
    pub(super) fn smoke_drawn(&self) -> bool {
        self.drawn.get() && self.paint.borrow().is_some()
    }
    pub(super) fn smoke_composing(&self) -> bool {
        self.composing.get()
    }
    pub(super) fn smoke_result(&mut self) -> Value {
        self.session
            .as_mut()
            .map(|s| s.poll())
            .unwrap_or(json!({"state":"closed","accepted":0,"suggestion":""}))
    }
}
