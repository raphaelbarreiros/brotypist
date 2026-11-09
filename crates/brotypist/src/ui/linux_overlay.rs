//! X11-only nonactivating presentation. Wayland never creates this popup.
use super::*;
use crate::core::{Authority, InlineRender, Rect, SnapshotKey};
use crate::runtime::{PaintObservation, PaintReporter, PresentationLane};
use glib::translate::{ToGlibPtr, from_glib};
use std::time::Instant;

mod x11_focus {
    use std::os::raw::{c_int, c_uint, c_ulong, c_void};

    unsafe extern "C" {
        pub fn gdk_x11_display_get_xdisplay(display: *mut gdk::ffi::GdkDisplay) -> *mut c_void;
        pub fn gdk_x11_window_get_xid(window: *mut gdk::ffi::GdkWindow) -> c_ulong;
        pub fn gdk_x11_display_error_trap_push(display: *mut gdk::ffi::GdkDisplay);
        pub fn gdk_x11_display_error_trap_pop(display: *mut gdk::ffi::GdkDisplay) -> c_int;
    }

    #[link(name = "X11")]
    unsafe extern "C" {
        pub fn XGetInputFocus(
            display: *mut c_void,
            focus: *mut c_ulong,
            revert_to: *mut c_int,
        ) -> c_int;
        pub fn XQueryTree(
            display: *mut c_void,
            window: c_ulong,
            root: *mut c_ulong,
            parent: *mut c_ulong,
            children: *mut *mut c_ulong,
            child_count: *mut c_uint,
        ) -> c_int;
        pub fn XFree(data: *mut c_void) -> c_int;
    }
}

/// Current server keyboard focus, including native descendants of this popup.
/// GDK's FOCUSED state describes active decorations and cannot prove this.
/// None means unobservable, not unfocused: callers must refuse it.
pub(super) fn keyboard_focus_within(native: &gdk::Window) -> Option<bool> {
    let display = native.display();
    if !display.type_().name().contains("X11") || native.is_destroyed() {
        return None;
    }
    // GTK owns this connection. Only use it synchronously on GTK's UI thread;
    // trap BadWindow from a disappearing focus target instead of killing GTK.
    unsafe {
        let gdk_display = display.to_glib_none().0;
        let xdisplay = x11_focus::gdk_x11_display_get_xdisplay(gdk_display);
        if xdisplay.is_null() {
            return None;
        }
        let popup = x11_focus::gdk_x11_window_get_xid(native.to_glib_none().0);
        if popup == 0 {
            return None;
        }
        x11_focus::gdk_x11_display_error_trap_push(gdk_display);
        let observed = (|| {
            let mut focus = 0;
            let mut revert_to = 0;
            if x11_focus::XGetInputFocus(xdisplay, &mut focus, &mut revert_to) == 0 || focus <= 1 {
                // None and PointerRoot do not identify a stable focused window.
                return None;
            }
            // Bound work even if the native tree is unexpectedly deep/changing.
            for _ in 0..64 {
                if focus == popup {
                    return Some(true);
                }
                let mut root = 0;
                let mut parent = 0;
                let mut children = std::ptr::null_mut();
                let mut child_count = 0;
                let ok = x11_focus::XQueryTree(
                    xdisplay,
                    focus,
                    &mut root,
                    &mut parent,
                    &mut children,
                    &mut child_count,
                );
                // XQueryTree always requests a child list even when only its
                // parent is needed. Do not copy or retain that Xlib allocation.
                if !children.is_null() {
                    x11_focus::XFree(children.cast());
                }
                if ok == 0 || root == 0 {
                    return None;
                }
                if focus == root && parent == 0 {
                    return Some(false);
                }
                if parent == 0 || parent == focus {
                    return None;
                }
                if parent == root {
                    // The root cannot be inside the popup. Avoid requesting
                    // its potentially large child list just to prove this.
                    return Some(root == popup);
                }
                focus = parent;
            }
            None
        })();
        let error = x11_focus::gdk_x11_display_error_trap_pop(gdk_display);
        if error == 0 { observed } else { None }
    }
}

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
    paint_identity: Option<PaintIdentity>,
    #[cfg(feature = "native-smoke")]
    draw_evidence: std::cell::Cell<DrawEvidence>,
    #[cfg(feature = "native-smoke")]
    readiness_evidence: std::cell::Cell<ReadinessEvidence>,
}
type ChoiceSurface = (gtk::Window, gtk::DrawingArea, Rc<RefCell<Option<Frame>>>);
/// Bounded diagnostics from the real draw callback; never retain offered text.
#[cfg(feature = "native-smoke")]
#[derive(Clone, Copy)]
struct DrawEvidence {
    callbacks: u32,
    status: &'static str,
    complete_clip: bool,
    clip_checked: bool,
    widget: Option<WidgetGuardEvidence>,
    native: Option<NativeGuardEvidence>,
    map_events: u32,
    configure_events: u32,
}
#[cfg(feature = "native-smoke")]
impl Default for DrawEvidence {
    fn default() -> Self {
        Self {
            callbacks: 0,
            status: "not-drawn",
            complete_clip: false,
            clip_checked: false,
            widget: None,
            native: None,
            map_events: 0,
            configure_events: 0,
        }
    }
}
#[cfg(feature = "native-smoke")]
#[derive(Clone, Copy, serde::Serialize)]
struct WidgetGuardEvidence {
    mapped: bool,
    visible: bool,
    active: bool,
    toplevel_focus: bool,
    scale: i32,
    allocation: [i32; 2],
}
#[cfg(feature = "native-smoke")]
#[derive(Clone, Copy, serde::Serialize)]
struct NativeGuardEvidence {
    viewable: bool,
    state: u32,
    // GDK's active-decoration state, not an X11 GetInputFocus observation.
    gdk_focused: bool,
    keyboard_focus_within: Option<bool>,
    origin_valid: i32,
    origin: [i32; 2],
}
#[cfg(feature = "native-smoke")]
impl NativeGuardEvidence {
    fn capture(native: &gdk::Window) -> Self {
        let (origin_valid, x, y) = native.origin();
        let state = native.state();
        Self {
            viewable: native.is_viewable(),
            state: state.bits(),
            gdk_focused: state.contains(gdk::WindowState::FOCUSED),
            keyboard_focus_within: keyboard_focus_within(native),
            origin_valid,
            origin: [x, y],
        }
    }
}
#[cfg(feature = "native-smoke")]
#[derive(Clone, Copy, serde::Serialize)]
struct ReadinessEvent {
    after_draw_callbacks: u32,
    native: Option<NativeGuardEvidence>,
}
#[cfg(feature = "native-smoke")]
#[derive(Clone, Copy, Default, serde::Serialize)]
struct ReadinessEvidence {
    map_events: u32,
    configure_events: u32,
    last_map: Option<ReadinessEvent>,
    last_configure: Option<ReadinessEvent>,
}
#[derive(Clone, Copy)]
struct PaintIdentity {
    trace_id: u64,
    lane: PresentationLane,
    view_serial: u64,
}
impl PaintIdentity {
    fn from_event(event: &Value, lane: PresentationLane) -> Option<Self> {
        let diagnostic = event.get("diagnostic")?;
        let expected_lane = match lane {
            PresentationLane::Inline => "inline",
            PresentationLane::Chooser => "chooser",
        };
        if diagnostic.get("lane")?.as_str()? != expected_lane {
            return None;
        }
        let trace_id = diagnostic.get("trace_id")?.as_u64()?;
        let view_serial = diagnostic.get("view_serial")?.as_u64()?;
        (trace_id != 0).then_some(Self {
            trace_id,
            lane,
            view_serial,
        })
    }
    fn observation(self, painted_at: Instant) -> PaintObservation {
        PaintObservation {
            trace_id: self.trace_id,
            lane: self.lane,
            view_serial: self.view_serial,
            painted_at,
        }
    }
}
pub(super) struct Presentation {
    window: Option<gtk::Window>,
    area: Option<gtk::DrawingArea>,
    frame: Rc<RefCell<Option<Frame>>>,
    choices: Option<ChoiceSurface>,
    accessory: Option<gtk::Window>,
    paint_reporter: Rc<RefCell<Option<PaintReporter>>>,
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
/// Fit exact rows, not prefixes: a rejected offer must not hide a later eligible row.
fn choice_layout(
    layout: &pango::Layout,
    choices: &[&str],
    max_width: i32,
    max_height: i32,
) -> Option<(String, i32, i32)> {
    layout.set_single_paragraph_mode(false);
    layout.set_width(-1);
    layout.set_ellipsize(pango::EllipsizeMode::None);
    let mut content = String::new();
    let mut visible = String::new();
    let mut count = 0;
    let mut size = (0, 0);
    for value in choices.iter().take(9) {
        // The ordinary overlay receipt uses newline-delimited values.
        if value.is_empty() || value.contains(['\n', '\r', '\0']) {
            continue;
        }
        let previous = content.len();
        if count != 0 {
            content.push('\n');
        }
        use std::fmt::Write;
        let _ = write!(content, "{}. {}", count + 1, value);
        layout.set_text(&content);
        let (ink, logical) = layout.pixel_extents();
        let width = (ink.x() + ink.width()).max(logical.x() + logical.width()) + 16;
        let height = (ink.y() + ink.height()).max(logical.y() + logical.height()) + 12;
        if layout.unknown_glyphs_count() != 0
            || ink.x() < -8
            || ink.y() < -6
            || width > max_width
            || height > max_height
        {
            content.truncate(previous);
            continue;
        }
        if count != 0 {
            visible.push('\n');
        }
        visible.push_str(value);
        count += 1;
        size = (width, height);
    }
    layout.set_text(&content);
    (count != 0).then_some((visible, size.0, size.1))
}
/// Prove coverage from Cairo's actual rectangular clip, not its bounding box.
/// Unrepresentable clips are unobserved; they do not make drawing an error.
fn complete_clip(cr: &cairo::Context, width: i32, height: i32) -> bool {
    if width <= 0 || height <= 0 {
        return false;
    }
    let Ok(rectangles) = cr.copy_clip_rectangle_list() else {
        return false;
    };
    let width = f64::from(width);
    let height = f64::from(height);
    if rectangles.iter().any(|r| {
        !r.x().is_finite()
            || !r.y().is_finite()
            || !r.width().is_finite()
            || !r.height().is_finite()
            || !(r.x() + r.width()).is_finite()
            || !(r.y() + r.height()).is_finite()
            || r.width() < 0.
            || r.height() < 0.
    }) {
        return false;
    }
    // Sweep every vertical boundary. Within each slab the covering rectangles
    // are constant; merge their vertical spans without rounding away holes or
    // allocating an auxiliary region. Cairo normally supplies disjoint spans.
    let mut left = 0.;
    while left < width {
        let mut right = width;
        for r in rectangles.iter() {
            for boundary in [r.x(), r.x() + r.width()] {
                if boundary > left && boundary < right {
                    right = boundary;
                }
            }
        }
        let mut top = 0.;
        while top < height {
            let mut bottom = top;
            for r in rectangles.iter() {
                if r.x() <= left
                    && r.x() + r.width() >= right
                    && r.y() <= top
                    && r.y() + r.height() > bottom
                {
                    bottom = r.y() + r.height();
                }
            }
            if bottom == top {
                return false;
            }
            top = bottom;
        }
        left = right;
    }
    true
}

fn connect_surface(
    window: &gtk::Window,
    area: &gtk::DrawingArea,
    frame: &Rc<RefCell<Option<Frame>>>,
    sender: &Sender,
    paint_reporter: &Rc<RefCell<Option<PaintReporter>>>,
) {
    #[cfg(feature = "native-smoke")]
    {
        // Observe readiness without queuing a diagnostic-only redraw: counts
        // show whether MapNotify/ConfigureNotify arrived after the last draw.
        let map_frame = frame.clone();
        window.connect_map_event(move |window, _| {
            if let Some(frame) = map_frame.borrow().as_ref() {
                let mut evidence = frame.readiness_evidence.get();
                evidence.map_events = evidence.map_events.saturating_add(1);
                evidence.last_map = Some(ReadinessEvent {
                    after_draw_callbacks: frame.draw_evidence.get().callbacks,
                    native: window.window().as_ref().map(NativeGuardEvidence::capture),
                });
                frame.readiness_evidence.set(evidence);
            }
            glib::Propagation::Proceed
        });
        let configure_frame = frame.clone();
        window.connect_configure_event(move |window, _| {
            if let Some(frame) = configure_frame.borrow().as_ref() {
                let mut evidence = frame.readiness_evidence.get();
                evidence.configure_events = evidence.configure_events.saturating_add(1);
                evidence.last_configure = Some(ReadinessEvent {
                    after_draw_callbacks: frame.draw_evidence.get().callbacks,
                    native: window.window().as_ref().map(NativeGuardEvidence::capture),
                });
                frame.readiness_evidence.set(evidence);
            }
            glib::Propagation::Proceed.into()
        });
    }
    let frame = frame.clone();
    let tx = sender.clone();
    let w = window.clone();
    let paint_reporter = paint_reporter.clone();
    area.connect_draw(move |_, cr| {
        cr.set_operator(cairo::Operator::Source);
        cr.set_source_rgba(0., 0., 0., 0.);
        let _ = cr.paint();
        cr.set_operator(cairo::Operator::Over);
        let borrow = frame.borrow();
        let Some(f) = borrow.as_ref() else {
            return glib::Propagation::Stop;
        };
        let mapped = w.is_mapped();
        let visible = w.is_visible();
        let active = w.is_active();
        let toplevel_focus = w.has_toplevel_focus();
        let scale = w.scale_factor();
        let width = w.allocated_width();
        let height = w.allocated_height();
        #[cfg(feature = "native-smoke")]
        f.draw_evidence.set(DrawEvidence {
            callbacks: f.draw_evidence.get().callbacks.saturating_add(1),
            status: "widget-visibility-focus-scale-or-allocation-refused",
            complete_clip: false,
            clip_checked: false,
            widget: Some(WidgetGuardEvidence {
                mapped,
                visible,
                active,
                toplevel_focus,
                scale,
                allocation: [width, height],
            }),
            native: None,
            map_events: f.readiness_evidence.get().map_events,
            configure_events: f.readiness_evidence.get().configure_events,
        });
        if !mapped
            || !visible
            || active
            || toplevel_focus
            || f64::from(scale) != f.scale
            || width != f.width
            || height != f.height
        {
            return glib::Propagation::Stop;
        }
        #[cfg(feature = "native-smoke")]
        f.draw_evidence.set(DrawEvidence {
            status: "native-drawable-unavailable",
            ..f.draw_evidence.get()
        });
        let Some(native) = w.window() else {
            return glib::Propagation::Stop;
        };
        let (ok, x, y) = native.origin();
        let native_viewable = native.is_viewable();
        let keyboard_focus = keyboard_focus_within(&native);
        #[cfg(feature = "native-smoke")]
        let native_state = native.state();
        #[cfg(feature = "native-smoke")]
        f.draw_evidence.set(DrawEvidence {
            status: "native-visibility-focus-or-origin-refused",
            native: Some(NativeGuardEvidence {
                viewable: native_viewable,
                state: native_state.bits(),
                gdk_focused: native_state.contains(gdk::WindowState::FOCUSED),
                keyboard_focus_within: keyboard_focus,
                origin_valid: ok,
                origin: [x, y],
            }),
            ..f.draw_evidence.get()
        });
        if !native_viewable || keyboard_focus != Some(false) || ok == 0 || x != f.x || y != f.y {
            return glib::Propagation::Stop;
        }
        // Chooser receipts also require complete coverage. Otherwise avoid the
        // native clip query unless an active diagnostic observer needs it.
        let diagnostic_pending = f.paint_identity.is_some() && paint_reporter.borrow().is_some();
        let complete_surface_draw =
            (f.choices || diagnostic_pending) && complete_clip(cr, f.width, f.height);
        let complete_choices_draw = !f.choices || complete_surface_draw;
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
        let restored = cr.restore().is_ok();
        #[cfg(feature = "native-smoke")]
        f.draw_evidence.set(DrawEvidence {
            status: if painted && restored {
                "painted"
            } else {
                "cairo-error"
            },
            complete_clip: complete_surface_draw,
            clip_checked: f.choices || diagnostic_pending,
            ..f.draw_evidence.get()
        });
        // Diagnostic identity belongs to this exact frame, never a later event or receipt.
        // Partial invalidations can acknowledge inline text as before, but cannot claim
        // a complete first paint for either diagnostic lane.
        let observation = if painted && restored && complete_surface_draw {
            f.paint_identity
                .map(|identity| identity.observation(Instant::now()))
        } else {
            None
        };
        if painted && complete_choices_draw {
            if let Some(ack) = ack(&f.event, &f.visible) {
                let _ = tx.send(UiAction::Command(ack));
            }
        }
        drop(borrow);
        if let Some(observation) = observation {
            let reporter = paint_reporter.borrow().clone();
            if let Some(reporter) = reporter {
                reporter(observation);
            }
        }
        glib::Propagation::Stop
    });
}
impl Presentation {
    pub fn new(sender: &Sender) -> Self {
        let frame = Rc::new(RefCell::new(None::<Frame>));
        let paint_reporter = Rc::new(RefCell::new(None));
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
                paint_reporter,
            };
        }
        let window = popup(true);
        let area = gtk::DrawingArea::new();
        window.add(&area);
        connect_surface(&window, &area, &frame, sender, &paint_reporter);
        let choice_window = popup(true);
        let choice_area = gtk::DrawingArea::new();
        choice_window.add(&choice_area);
        let choice_frame = Rc::new(RefCell::new(None));
        connect_surface(
            &choice_window,
            &choice_area,
            &choice_frame,
            sender,
            &paint_reporter,
        );
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
            paint_reporter,
        }
    }
    pub fn set_paint_reporter(&self, reporter: Option<PaintReporter>) {
        *self.paint_reporter.borrow_mut() = reporter;
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
    #[cfg(feature = "native-smoke")]
    pub(super) fn smoke_choice_surface(&self) -> Option<(gtk::Window, gtk::DrawingArea)> {
        self.choices
            .as_ref()
            .map(|(window, area, _)| (window.clone(), area.clone()))
    }
    #[cfg(feature = "native-smoke")]
    pub(super) fn smoke_choice_draw_evidence(&self) -> Value {
        let Some((window, area, frame)) = &self.choices else {
            return json!({"status":"surface-unavailable"});
        };
        let borrow = frame.borrow();
        let Some(frame) = borrow.as_ref() else {
            return json!({"status":"frame-unavailable"});
        };
        let evidence = frame.draw_evidence.get();
        json!({
            "callbacks": evidence.callbacks,
            "status": evidence.status,
            "complete_clip": evidence.complete_clip,
            "clip_checked": evidence.clip_checked,
            // These are the values used by the guards, not later GTK getters.
            "at_draw": {
                "widget": evidence.widget,
                "native": evidence.native,
                "map_events": evidence.map_events,
                "configure_events": evidence.configure_events,
            },
            "readiness": frame.readiness_evidence.get(),
            "expected": [frame.x, frame.y, frame.width, frame.height],
            "window_allocation": [window.allocated_width(), window.allocated_height()],
            "area_allocation": [area.allocated_width(), area.allocated_height()],
            "scale": window.scale_factor(),
            "expected_scale": frame.scale,
            "area_has_window": area.has_window(),
            "area_mapped": area.is_mapped(),
            "area_shares_parent_drawable": area.window() == window.window(),
            "mapped": window.is_mapped(),
            "focused": window.is_active() || window.has_toplevel_focus(),
            "keyboard_focus_within": window.window().and_then(|native| keyboard_focus_within(&native)),
            "native_origin": window.window().map(|native| {
                let (valid, x, y) = native.origin();
                json!({"valid":valid != 0,"x":x,"y":y,"viewable":native.is_viewable()})
            }),
        })
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
        // A connected native-client broker also supplies a wake FD when no
        // client owns the target. Only rendering ownership excludes X11 paint.
        if controller.uses_native_presentation() {
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
                chooser["diagnostic"] = event
                    .get("choices_diagnostic")
                    .cloned()
                    .unwrap_or(Value::Null);
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
            paint_identity: PaintIdentity::from_event(&event, PresentationLane::Inline),
            #[cfg(feature = "native-smoke")]
            draw_evidence: Default::default(),
            #[cfg(feature = "native-smoke")]
            readiness_evidence: Default::default(),
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
                    .take(9)
                    .filter_map(Value::as_str)
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
        let bounds = monitor.workarea();
        let x = (anchor[0] / scale).round() as i32;
        let below = ((anchor[1] + anchor[3]) / scale).round() as i32 + 6;
        let above = (anchor[1] / scale).round() as i32 - 6;
        let available_height = (bounds.y() + bounds.height() - below)
            .max(above - bounds.y())
            .min(bounds.height());
        let Some((visible, width, height)) =
            choice_layout(&layout, &choices, bounds.width(), available_height)
        else {
            return;
        };
        let mut y = below;
        let x = x.clamp(bounds.x(), bounds.x() + bounds.width() - width);
        if y + height > bounds.y() + bounds.height() {
            y = (anchor[1] / scale).round() as i32 - height - 6;
        }
        if y < bounds.y() {
            return;
        }
        *frame.borrow_mut() = Some(Frame {
            paint_identity: PaintIdentity::from_event(&event, PresentationLane::Chooser),
            #[cfg(feature = "native-smoke")]
            draw_evidence: Default::default(),
            #[cfg(feature = "native-smoke")]
            readiness_evidence: Default::default(),
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
    fn cairo_damage_regions_require_complete_surface_coverage() {
        let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, 100, 40).unwrap();
        let cr = cairo::Context::new(&surface).unwrap();
        // Real Cairo clipping and drawing, including two disconnected edge strips
        // whose extents misleadingly enclose the entire surface.
        cr.rectangle(0., 0., 10., 40.);
        cr.rectangle(90., 0., 10., 40.);
        cr.clip();
        cr.paint().unwrap();
        assert_eq!(cr.clip_extents().unwrap(), (0., 0., 100., 40.));
        assert!(!complete_clip(&cr, 100, 40));

        cr.reset_clip();
        cr.set_fill_rule(cairo::FillRule::EvenOdd);
        cr.rectangle(0., 0., 100., 40.);
        cr.rectangle(30., 10., 40., 20.);
        cr.clip();
        cr.paint().unwrap();
        assert_eq!(cr.clip_extents().unwrap(), (0., 0., 100., 40.));
        assert!(!complete_clip(&cr, 100, 40));

        cr.reset_clip();
        cr.set_fill_rule(cairo::FillRule::Winding);
        // A complete region need not be returned as one rectangle: the top
        // half extends farther right than the bottom half.
        cr.rectangle(0., 0., 100., 20.);
        cr.rectangle(0., 20., 80., 20.);
        cr.clip();
        cr.paint().unwrap();
        assert!(complete_clip(&cr, 80, 40));
        assert!(!complete_clip(&cr, 100, 40));

        cr.reset_clip();
        cr.arc(50., 20., 20., 0., std::f64::consts::TAU);
        cr.clip();
        cr.paint().unwrap();
        assert!(cr.copy_clip_rectangle_list().is_err());
        assert!(!complete_clip(&cr, 100, 40));
        assert!(cr.status().is_ok());
    }
    #[test]
    fn acknowledgment_requires_full_runtime_identity() {
        assert!(ack(&json!({"mode":"completion","revision":1}), "text").is_none());
    }
    #[test]
    fn paint_identity_is_lane_bound_and_never_uses_native_revision() {
        let mut event = json!({
            "revision": 999,
            "diagnostic": {"trace_id": 7, "lane": "inline", "view_serial": 3},
            "choices_diagnostic": {"trace_id": 8, "lane": "chooser", "view_serial": 4}
        });
        let inline = PaintIdentity::from_event(&event, PresentationLane::Inline).unwrap();
        assert!(PaintIdentity::from_event(&event, PresentationLane::Chooser).is_none());
        event["diagnostic"] = event["choices_diagnostic"].clone();
        let chooser = PaintIdentity::from_event(&event, PresentationLane::Chooser).unwrap();
        // Replacing the event cannot relabel an already captured frame's observation.
        event["diagnostic"] = json!({"trace_id": 9, "lane": "inline", "view_serial": 5});
        let at = Instant::now();
        let inline = inline.observation(at);
        let chooser = chooser.observation(at);
        assert_eq!((inline.trace_id, inline.view_serial), (7, 3));
        assert_eq!(inline.lane, PresentationLane::Inline);
        assert_eq!(inline.painted_at, at);
        assert_eq!((chooser.trace_id, chooser.view_serial), (8, 4));
        assert_eq!(chooser.lane, PresentationLane::Chooser);
        assert_eq!(chooser.painted_at, at);
    }
    #[test]
    fn malformed_diagnostics_never_change_presentation_authority() {
        let snapshot = crate::core::Snapshot::default();
        for diagnostic in [
            Value::Null,
            json!({"trace_id": 0, "lane": "inline", "view_serial": 1}),
            json!({"trace_id": 1, "lane": "chooser", "view_serial": 1}),
            json!({"trace_id": 1, "lane": "inline", "view_serial": -1}),
            json!({"trace_id": 1, "lane": "inline"}),
        ] {
            let event = json!({
                "key": snapshot.key(), "authority": snapshot.authority(),
                "revision": 73, "mode": "completion", "diagnostic": diagnostic
            });
            assert!(PaintIdentity::from_event(&event, PresentationLane::Inline).is_none());
            match ack(&event, " e\u{301} ").unwrap() {
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
                    assert_eq!(visible_text, " e\u{301} ");
                    assert_eq!(mode, "completion");
                }
                _ => panic!("Expected a presentation result"),
            }
        }
        let diagnostic_only = json!({
            "diagnostic": {"trace_id": 1, "lane": "inline", "view_serial": 1}
        });
        assert!(PaintIdentity::from_event(&diagnostic_only, PresentationLane::Inline).is_some());
        assert!(ack(&diagnostic_only, "text").is_none());
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
    fn choices_keep_exact_later_rows_after_width_or_height_rejection() {
        let context = pangocairo::FontMap::default().create_context();
        let layout = pango::Layout::new(&context);
        layout.set_font_description(Some(&pango::FontDescription::from_string("Sans 12")));
        let long = "too wide ".repeat(1000);
        let choices = ["  first  ", long.as_str(), " e\u{301} last "];
        let (visible, width, height) = choice_layout(&layout, &choices, 300, 200).unwrap();
        assert_eq!(visible, "  first  \n e\u{301} last ");
        assert_eq!(layout.text().as_str(), "1.   first  \n2.  e\u{301} last ");
        assert!(width <= 300 && height <= 200);
        let tall = "line\n".repeat(1000);
        let choices = ["first", tall.as_str(), "last"];
        assert_eq!(
            choice_layout(&layout, &choices, 300, 200).unwrap().0,
            "first\nlast"
        );
        assert!(choice_layout(&layout, &choices, 1, 1).is_none());
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
