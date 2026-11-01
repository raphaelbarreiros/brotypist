//! X11-only nonactivating presentation. Wayland never creates this popup.
use super::*;
use crate::core::{Authority, InlineRender, Rect, SnapshotKey};
use glib::translate::from_glib;
struct Frame {
    event: Value,
    layout: pango::Layout,
    visible: String,
    baseline: f64,
    scale: f64,
    width: i32,
    height: i32,
    x: i32,
    y: i32,
    choices: bool,
}
type ChoiceSurface = (gtk::Window, gtk::DrawingArea, Rc<RefCell<Option<Frame>>>);
pub(super) struct Presentation {
    window: Option<gtk::Window>,
    area: Option<gtk::DrawingArea>,
    frame: Rc<RefCell<Option<Frame>>>,
    choices: Option<ChoiceSurface>,
    accessory: Option<gtk::Window>,
}
fn rect(value: &Value) -> Option<Rect> {
    let r: Rect = serde_json::from_value(value.clone()).ok()?;
    r.iter().all(|v| v.is_finite()).then_some(r)
}
pub(super) fn ordinary_allowed(x11: bool, wayland: bool, event: &Value) -> bool {
    x11 && !wayland
        && event["inline"] == true
        && event["mode"] == "completion"
        && event.get("render").is_some_and(Value::is_object)
}
/// Return only complete rendered grapheme/cluster prefixes whose ink and advance fit.
fn visible_prefix(layout: &pango::Layout, text: &str, width: f64) -> String {
    if width <= 0. || text.is_empty() {
        return String::new();
    }
    layout.set_text(text);
    let mut iter = layout.iter();
    let mut best = 0;
    loop {
        let (_, logical) = iter.cluster_extents();
        let fits = logical.x() >= 0
            && f64::from(logical.x() + logical.width()) / f64::from(pango::SCALE) <= width;
        let more = iter.next_cluster();
        let end = if more {
            iter.index().max(0) as usize
        } else {
            text.len()
        };
        if !fits || end > text.len() || !text.is_char_boundary(end) {
            break;
        }
        best = end;
        if !more {
            break;
        }
    }
    let visible = text[..best].to_owned();
    layout.set_text(&visible);
    let (ink, logical) = layout.pixel_extents();
    if ink.x() < 0 || f64::from(ink.x() + ink.width()) > width || f64::from(logical.width()) > width
    {
        return String::new();
    }
    visible
}
fn ack(event: &Value, visible: &str) -> Option<Command> {
    Some(Command::PresentationResult {
        key: serde_json::from_value::<SnapshotKey>(event.get("key")?.clone()).ok()?,
        authority: serde_json::from_value::<Authority>(event.get("authority")?.clone()).ok()?,
        revision: event.get("revision")?.as_u64()?,
        visible_text: visible.into(),
        mode: event["mode"].as_str().unwrap_or("completion").into(),
    })
}
fn native_monitor(anchor: Rect) -> Option<(gdk::Monitor, f64)> {
    let display = gdk::Display::default()?;
    for index in 0..display.n_monitors() {
        let Some(monitor) = display.monitor(index) else {
            continue;
        };
        let bounds = monitor.geometry();
        let scale = f64::from(monitor.scale_factor().max(1));
        if anchor[0] >= f64::from(bounds.x()) * scale
            && anchor[0] < f64::from(bounds.x() + bounds.width()) * scale
            && anchor[1] >= f64::from(bounds.y()) * scale
            && anchor[1] < f64::from(bounds.y() + bounds.height()) * scale
        {
            return Some((monitor, scale));
        }
    }
    None
}
fn connect_surface(
    window: &gtk::Window,
    area: &gtk::DrawingArea,
    frame: &Rc<RefCell<Option<Frame>>>,
    sender: &Sender,
) {
    let frame = frame.clone();
    let tx = sender.clone();
    let w = window.clone();
    area.connect_draw(move |_, cr| {
        cr.set_operator(cairo::Operator::Source);
        cr.set_source_rgba(0., 0., 0., 0.);
        let _ = cr.paint();
        cr.set_operator(cairo::Operator::Over);
        let borrow = frame.borrow();
        let Some(f) = borrow.as_ref() else {
            return glib::Propagation::Stop;
        };
        if !w.is_mapped()
            || !w.is_visible()
            || w.is_active()
            || w.has_toplevel_focus()
            || f64::from(w.scale_factor()) != f.scale
            || w.allocated_width() != f.width
            || w.allocated_height() != f.height
        {
            return glib::Propagation::Stop;
        }
        let Some(native) = w.window() else {
            return glib::Propagation::Stop;
        };
        let (ok, x, y) = native.origin();
        if !native.is_viewable()
            || native.state().contains(gdk::WindowState::FOCUSED)
            || ok == 0
            || x != f.x
            || y != f.y
        {
            return glib::Propagation::Stop;
        }
        native.raise();
        let _ = cr.save();
        let units = if f.choices {
            1.
        } else {
            cr.scale(1. / f.scale, 1. / f.scale);
            f.scale
        };
        cr.rectangle(
            0.,
            0.,
            f64::from(f.width) * units,
            f64::from(f.height) * units,
        );
        cr.clip();
        if f.choices {
            let style = w.style_context();
            if let Some(bg) = style.lookup_color("theme_bg_color") {
                cr.set_source_rgba(bg.red(), bg.green(), bg.blue(), 0.97);
            } else {
                cr.set_source_rgba(0.95, 0.95, 0.95, 0.97);
            }
            let _ = cr.paint();
            let fg = style.color(gtk::StateFlags::NORMAL);
            cr.set_source_rgb(fg.red(), fg.green(), fg.blue());
            cr.move_to(8., 6.);
        } else {
            cr.set_source_rgb(0.565, 0.573, 0.6);
            cr.move_to(
                0.,
                f.baseline - f64::from(f.layout.baseline()) / f64::from(pango::SCALE),
            );
        }
        pangocairo::functions::show_layout(cr, &f.layout);
        let painted = cr.status().is_ok();
        let _ = cr.restore();
        if painted {
            if let Some(ack) = ack(&f.event, &f.visible) {
                let _ = tx.send(UiAction::Command(ack));
            }
        }
        glib::Propagation::Stop
    });
}
impl Presentation {
    pub fn new(sender: &Sender) -> Self {
        let frame = Rc::new(RefCell::new(None::<Frame>));
        let display = gdk::Display::default();
        let x11 = display
            .as_ref()
            .is_some_and(|d| d.type_().name().contains("X11") && d.supports_input_shapes())
            && !crate::platform::wayland::is_prepared()
            && std::env::var("XDG_SESSION_TYPE").as_deref() != Ok("wayland");
        let composited =
            gdk::Screen::default().is_some_and(|s| s.is_composited() && s.rgba_visual().is_some());
        if !x11 || !composited {
            return Self {
                window: None,
                area: None,
                frame,
                choices: None,
                accessory: None,
            };
        }
        let window = popup(true);
        let area = gtk::DrawingArea::new();
        window.add(&area);
        connect_surface(&window, &area, &frame, sender);
        let choice_window = popup(true);
        let choice_area = gtk::DrawingArea::new();
        choice_window.add(&choice_area);
        let choice_frame = Rc::new(RefCell::new(None));
        connect_surface(&choice_window, &choice_area, &choice_frame, sender);
        let accessory = popup(false);
        let b = gtk::Button::new();
        b.set_image(Some(
            &app_image(18).expect("Validated embedded application artwork"),
        ));
        b.set_can_focus(false);
        if let Some(a) = b.accessible() {
            a.set_name("BroTypist current app controls");
        }
        b.set_tooltip_text(Some("BroTypist — current app controls"));
        let tx = sender.clone();
        b.connect_clicked(move |_| {
            let _ = tx.send(UiAction::Native("x11_field_menu".into()));
        });
        accessory.add(&b);
        Self {
            window: Some(window),
            area: Some(area),
            frame,
            choices: Some((choice_window, choice_area, choice_frame)),
            accessory: Some(accessory),
        }
    }
    pub fn clear(&self) {
        *self.frame.borrow_mut() = None;
        if let Some(w) = &self.window {
            w.hide();
        }
        if let Some((w, _, frame)) = &self.choices {
            *frame.borrow_mut() = None;
            w.hide();
        }
        if let Some(w) = &self.accessory {
            w.hide();
        }
    }
    pub fn present(&self, event: Value, controller: &Controller) {
        let Some(window) = &self.window else {
            return;
        };
        window.hide();
        *self.frame.borrow_mut() = None;
        if let Some((w, _, frame)) = &self.choices {
            *frame.borrow_mut() = None;
            w.hide();
        }
        if controller.native_event_source().is_some() {
            return;
        }
        let is_inline = event["mode"] == "completion";
        if is_inline && !ordinary_allowed(true, false, &event) {
            return;
        }
        if is_inline {
            if event["choices"].as_array().is_some_and(|v| !v.is_empty()) {
                let mut chooser = event.clone();
                chooser["mode"] = event
                    .get("choices_mode")
                    .cloned()
                    .unwrap_or(json!("alternatives"));
                self.choices(chooser);
            }
            self.inline(event);
        } else {
            self.choices(event);
        }
    }
    fn inline(&self, event: Value) {
        let Some(window) = &self.window else {
            return;
        };
        let Some(area) = &self.area else {
            return;
        };
        let Some(caret) = rect(&event["rect"]) else {
            return;
        };
        let Ok(render) = serde_json::from_value::<InlineRender>(event["render"].clone()) else {
            return;
        };
        if !render.valid(Some(caret))
            || render.source != "linux-atspi-font-run"
            || !render.suffix.is_empty()
        {
            return;
        }
        let text = text(&event, "text");
        let text = text.split(['\r', '\n']).next().unwrap_or("");
        if text.is_empty() {
            return;
        }
        let Some((monitor, scale)) = native_monitor(caret) else {
            return;
        };
        // Use exactly the same physical-unit Pango context as the AT-SPI evidence owner.
        let context = pangocairo::FontMap::default().create_context();
        let mut description = pango::FontDescription::new();
        description.set_family(&render.font_name);
        description.set_size((render.font_size * f64::from(pango::SCALE)).round() as i32);
        description.set_weight(unsafe { from_glib(render.font_weight) });
        description.set_style(if render.italic {
            pango::Style::Italic
        } else {
            pango::Style::Normal
        });
        let Some(font) = context.load_font(&description) else {
            return;
        };
        if font
            .describe()
            .family()
            .is_none_or(|name| name.to_lowercase() != render.font_name.to_lowercase())
        {
            return;
        }
        let metrics = context.metrics(Some(&description), Some(&pango::Language::default()));
        let ascent = f64::from(metrics.ascent()) / f64::from(pango::SCALE);
        let descent = f64::from(metrics.descent()) / f64::from(pango::SCALE);
        if (ascent - render.ascent).abs() > 1. || (descent - render.descent).abs() > 1. {
            return;
        }
        let layout = pango::Layout::new(&context);
        layout.set_font_description(Some(&description));
        layout.set_single_paragraph_mode(true);
        layout.set_auto_dir(false);
        let available = render.clip_rect[0] + render.clip_rect[2] - caret[0];
        let visible = visible_prefix(&layout, text, available);
        if visible.is_empty() {
            return;
        }
        if layout.unknown_glyphs_count() != 0 {
            return;
        }
        for line in layout.lines_readonly() {
            for run in line.runs() {
                let item = run.item();
                let analysis = item.analysis();
                if analysis.level() % 2 != 0
                    || analysis
                        .font()
                        .describe()
                        .family()
                        .is_none_or(|name| name.to_lowercase() != render.font_name.to_lowercase())
                {
                    return;
                }
            }
        }
        let (ink, logical) = layout.pixel_extents();
        let offset = render.ascent - f64::from(layout.baseline()) / f64::from(pango::SCALE);
        if ink.width() <= 0
            || ink.height() <= 0
            || offset + f64::from(ink.y()) < 0.
            || offset + f64::from(ink.y() + ink.height()) > render.ascent + render.descent
            || layout.line_count() != 1
        {
            return;
        }
        let native_width = f64::from(logical.width().max(ink.x() + ink.width())).min(available);
        let native_height = render.ascent + render.descent;
        let x = (caret[0] / scale).round() as i32;
        let y = ((render.baseline - render.ascent) / scale).round() as i32;
        let width = (native_width / scale).ceil() as i32;
        let height = (native_height / scale).ceil() as i32;
        // Fractional-origin or row clipping would shift host-relative glyphs; refuse rather than round into another row.
        if (f64::from(x) * scale - caret[0]).abs() > 0.5
            || (f64::from(y) * scale - (render.baseline - render.ascent)).abs() > 0.5
        {
            return;
        }
        let bounds = monitor.geometry();
        if x < bounds.x()
            || y < bounds.y()
            || x + width > bounds.x() + bounds.width()
            || y + height > bounds.y() + bounds.height()
        {
            return;
        }
        *self.frame.borrow_mut() = Some(Frame {
            event,
            layout,
            visible,
            baseline: render.ascent,
            scale,
            width,
            height,
            x,
            y,
            choices: false,
        });
        area.set_size_request(width, height);
        window.resize(width, height);
        window.move_(x, y);
        window.show_all();
        window.move_(x, y);
        area.queue_draw();
    }
    fn choices(&self, event: Value) {
        let Some((window, area, frame)) = &self.choices else {
            return;
        };
        let Some(anchor) = rect(&event["rect"]) else {
            return;
        };
        let choices = event["choices"]
            .as_array()
            .map(|v| {
                v.iter()
                    .filter_map(Value::as_str)
                    .take(9)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if choices.is_empty() || event["mode"] == "completion" {
            return;
        }
        let Some((monitor, scale)) = native_monitor(anchor) else {
            return;
        };
        let layout = area.create_pango_layout(None);
        layout.set_single_paragraph_mode(false);
        let content = choices
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{}. {}", i + 1, c))
            .collect::<Vec<_>>()
            .join("\n");
        layout.set_text(&content);
        let (w, h) = layout.pixel_size();
        let width = w + 16;
        let height = h + 12;
        let x = (anchor[0] / scale).round() as i32;
        let mut y = ((anchor[1] + anchor[3]) / scale).round() as i32 + 6;
        let bounds = monitor.workarea();
        if width > bounds.width() || height > bounds.height() {
            return;
        }
        let x = x.clamp(bounds.x(), bounds.x() + bounds.width() - width);
        if y + height > bounds.y() + bounds.height() {
            y = (anchor[1] / scale).round() as i32 - height - 6;
        }
        if y < bounds.y() {
            return;
        }
        // The acknowledgment is complete choices, not a clipped or elided label.
        let visible = choices.join("\n");
        *frame.borrow_mut() = Some(Frame {
            event,
            layout,
            visible,
            baseline: 0.,
            scale,
            width,
            height,
            x,
            y,
            choices: true,
        });
        area.set_size_request(width, height);
        window.resize(width, height);
        window.move_(x, y);
        window.show_all();
        area.queue_draw();
    }
    pub fn caret(&self, event: &Value, enabled: bool) {
        let Some(window) = &self.accessory else {
            return;
        };
        if !enabled {
            window.hide();
            return;
        }
        let anchor = rect(&event["rect"]).or_else(|| rect(&event["field_rect"]));
        let Some(anchor) = anchor else {
            window.hide();
            return;
        };
        let Some((monitor, scale)) = native_monitor(anchor) else {
            window.hide();
            return;
        };
        let field = rect(&event["field_rect"]);
        let x = (field.unwrap_or(anchor)[0] / scale).round() as i32;
        let y = (anchor[1] / scale).round() as i32;
        let line_end = ((anchor[1] + anchor[3]) / scale).round() as i32;
        let bounds = monitor.workarea();
        let width = 30;
        let height = 28;
        if bounds.width() < width || bounds.height() < height {
            window.hide();
            return;
        }
        let x = x.clamp(bounds.x(), bounds.x() + bounds.width() - width);
        let mut top = y - height - 4;
        if top < bounds.y() {
            top = line_end + 4;
        }
        if top < bounds.y() || top + height > bounds.y() + bounds.height() {
            window.hide();
            return;
        }
        window.move_(x, top);
        window.resize(width, height);
        window.show_all();
    }
}
fn popup(mouse_through: bool) -> gtk::Window {
    let w = gtk::Window::new(gtk::WindowType::Popup);
    w.set_decorated(false);
    w.set_resizable(false);
    w.set_accept_focus(false);
    w.set_focus_on_map(false);
    w.set_skip_taskbar_hint(true);
    w.set_skip_pager_hint(true);
    w.set_keep_above(true);
    w.set_app_paintable(true);
    w.set_type_hint(gdk::WindowTypeHint::Tooltip);
    if let Some(screen) = gtk::prelude::WidgetExt::screen(&w) {
        if let Some(visual) = screen.rgba_visual() {
            w.set_visual(Some(&visual));
        }
    }
    w.connect_realize(move |w| {
        if let Some(native) = w.window() {
            native.set_accept_focus(false);
            native.set_focus_on_map(false);
            if mouse_through {
                let empty = cairo::Region::create();
                native.input_shape_combine_region(&empty, 0, 0);
            }
        }
    });
    w
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ordinary_inline_refuses_wayland_and_unverified_payload() {
        let event = json!({"inline":true,"mode":"completion","render":{}});
        assert!(ordinary_allowed(true, false, &event));
        assert!(!ordinary_allowed(true, true, &event));
        assert!(!ordinary_allowed(false, false, &event));
        assert!(!ordinary_allowed(
            true,
            false,
            &json!({"inline":true,"mode":"completion"})
        ));
    }
    #[test]
    fn acknowledgment_requires_full_runtime_identity() {
        assert!(ack(&json!({"mode":"completion","revision":1}), "text").is_none());
    }
    #[test]
    fn rectangles_refuse_nan_and_wrong_lengths() {
        assert!(rect(&json!([1, 2, 3, 4])).is_some());
        assert!(rect(&json!([1, 2, 3])).is_none());
    }
}

#[cfg(test)]
mod visible_tests {
    use super::*;
    #[test]
    fn acknowledgment_retains_exact_visible_prefix_and_authority() {
        let snapshot = crate::core::Snapshot::default();
        let event = json!({"key":snapshot.key(),"authority":snapshot.authority(),"revision":73,"mode":"completion"});
        match ack(&event, " shown").unwrap() {
            Command::PresentationResult {
                key,
                authority,
                revision,
                visible_text,
                mode,
            } => {
                assert_eq!(key, snapshot.key());
                assert_eq!(authority, snapshot.authority());
                assert_eq!(revision, 73);
                assert_eq!(visible_text, " shown");
                assert_eq!(mode, "completion");
            }
            _ => panic!("Expected a presentation result"),
        }
    }
    #[test]
    fn pango_clipping_never_acknowledges_half_a_combining_cluster() {
        let context = pangocairo::FontMap::default().create_context();
        let layout = pango::Layout::new(&context);
        layout.set_font_description(Some(&pango::FontDescription::from_string("Sans 12")));
        layout.set_text("e\u{301}");
        let (w, _) = layout.pixel_size();
        assert!(w > 0);
        assert!(visible_prefix(&layout, "e\u{301}x", f64::from(w) - 1.).is_empty());
        let text = "e\u{301}x";
        assert_eq!(visible_prefix(&layout, text, 10000.), text);
        assert!(visible_prefix(&layout, text, 0.).is_empty());
    }
}
