//! Main-thread, nonactivating AppKit presentation. Acknowledgments describe only
//! verified visible glyphs; an ordering request alone never arms host insertion.
//! Complete-draw diagnostics require a containing native damage rectangle, not
//! drawRect's potentially disconnected damage bounds. Fragmented coverage that
//! needs union reconstruction remains unknown; diagnostics never grant acceptance.
use crate::{
    core::{Authority, InlineRender, Rect, SnapshotKey},
    platform::{
        Action,
        macos::input::{ChooserHitRegion, publish_chooser_hit_region},
    },
    runtime::{Command, PaintObservation, PaintReporter, PresentationLane},
};
use anyhow::{Result, anyhow};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::*;
use objc2_core_text::CTLine;
use objc2_foundation::{
    MainThreadMarker, NSAttributedString, NSData, NSDictionary, NSNumber, NSObject,
    NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};
use serde_json::Value;
use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
    io::Write,
    rc::Rc,
    time::Instant,
};

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGContextSetTextPosition(context: *const c_void, x: f64, y: f64);
    fn CGContextSetTextMatrix(context: *mut c_void, matrix: Transform);
    fn CGContextSetFillColorWithColor(context: *mut c_void, color: *const c_void);
    fn CGContextSaveGState(context: *mut c_void);
    fn CGContextRestoreGState(context: *mut c_void);
    fn CGContextClipToRect(context: *mut c_void, rect: NSRect);
}
#[repr(C)]
struct Transform {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    tx: f64,
    ty: f64,
}
#[link(name = "CoreText", kind = "framework")]
unsafe extern "C" {
    fn CTLineDraw(line: *const c_void, context: *const c_void);
}

define_class!(
    #[unsafe(super = NSPanel)]
    #[thread_kind = MainThreadOnly]
    pub(crate) struct PassivePanel;
    impl PassivePanel {
        #[unsafe(method(canBecomeKeyWindow))] fn no_key(&self)->bool { false }
        #[unsafe(method(canBecomeMainWindow))] fn no_main(&self)->bool { false }
    }
);
impl PassivePanel {
    pub(crate) fn create(
        mtm: MainThreadMarker,
        id: &str,
        label: &str,
        level: isize,
        all_spaces: bool,
    ) -> Retained<Self> {
        let panel: Retained<Self> = unsafe {
            msg_send![Self::alloc(mtm), initWithContentRect: rect([0.,0.,100.,30.]), styleMask:NSWindowStyleMask::Borderless|NSWindowStyleMask::NonactivatingPanel, backing:NSBackingStoreType::Buffered, defer:false]
        };
        unsafe {
            panel.setReleasedWhenClosed(false);
        }
        panel.setIdentifier(Some(&NSString::from_str(id)));
        unsafe {
            let _: () = msg_send![&*panel,setAccessibilityIdentifier:&*NSString::from_str(id)];
            let _: () = msg_send![&*panel,setAccessibilityLabel:&*NSString::from_str(label)];
        }
        panel.setLevel(level);
        panel.setHidesOnDeactivate(false);
        panel.setBecomesKeyOnlyIfNeeded(true);
        panel.setFloatingPanel(true);
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setHasShadow(true);
        let mut behavior = NSWindowCollectionBehavior::Transient
            | NSWindowCollectionBehavior::IgnoresCycle
            | NSWindowCollectionBehavior::FullScreenAuxiliary;
        if all_spaces {
            behavior |= NSWindowCollectionBehavior::CanJoinAllSpaces;
        }
        panel.setCollectionBehavior(behavior);
        panel
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PaintIdentity {
    trace_id: u64,
    lane: PresentationLane,
    view_serial: u64,
}
impl PaintIdentity {
    fn from_event(event: &Value, lane: PresentationLane) -> Option<Self> {
        let metadata = if lane == PresentationLane::Chooser
            && matches!(
                event["mode"].as_str().unwrap_or("completion"),
                "completion" | "correction"
            ) {
            &event["choices_diagnostic"]
        } else {
            &event["diagnostic"]
        };
        let expected = match lane {
            PresentationLane::Inline => "inline",
            PresentationLane::Chooser => "chooser",
        };
        if metadata["lane"].as_str()? != expected {
            return None;
        }
        let identity = Self {
            trace_id: metadata["trace_id"].as_u64()?,
            lane,
            view_serial: metadata["view_serial"].as_u64()?,
        };
        (identity.trace_id != 0 && identity.view_serial != 0).then_some(identity)
    }
}

// A callback snapshots this batch before drawing. Replacement never relabels an
// in-flight draw; any observation already queued keeps its original diagnostic IDs.
struct PaintBatch {
    identity: PaintIdentity,
    reporter: PaintReporter,
    active: Cell<bool>,
    required: u16,
    drawn: Cell<u16>,
}
impl PaintBatch {
    fn did_draw(&self, row: u16, visible: bool) {
        if !visible || !self.active.get() || self.drawn.get() == self.required {
            return;
        }
        let drawn = self.drawn.get() | (row & self.required);
        self.drawn.set(drawn);
        if drawn == self.required {
            (self.reporter)(PaintObservation {
                trace_id: self.identity.trace_id,
                lane: self.identity.lane,
                view_serial: self.identity.view_serial,
                painted_at: Instant::now(),
            });
        }
    }
}
#[derive(Default)]
struct PaintBinding(RefCell<Option<Rc<PaintBatch>>>);
impl PaintBinding {
    fn clear(&self) {
        if let Some(batch) = self.0.borrow_mut().take() {
            batch.active.set(false);
        }
    }
    fn bind(
        &self,
        identity: Option<PaintIdentity>,
        reporter: Option<&PaintReporter>,
        required: u16,
    ) {
        if let (Some(identity), Some(reporter)) = (identity, reporter) {
            if required != 0 {
                if self.0.borrow().as_ref().is_some_and(|batch| {
                    batch.identity == identity
                        && batch.required == required
                        && std::sync::Arc::ptr_eq(&batch.reporter, reporter)
                }) {
                    return;
                }
                self.clear();
                self.0.replace(Some(Rc::new(PaintBatch {
                    identity,
                    reporter: reporter.clone(),
                    active: Cell::new(true),
                    required,
                    drawn: Cell::new(0),
                })));
                return;
            }
        }
        self.clear();
    }
    fn snapshot(&self) -> Option<Rc<PaintBatch>> {
        self.0.borrow().clone()
    }
}
fn valid_damage_rect(region: NSRect) -> bool {
    let right = region.origin.x + region.size.width;
    let top = region.origin.y + region.size.height;
    region.origin.x.is_finite()
        && region.origin.y.is_finite()
        && region.size.width.is_finite()
        && region.size.height.is_finite()
        && region.size.width > 0.
        && region.size.height > 0.
        && right.is_finite()
        && top.is_finite()
        && right > region.origin.x
        && top > region.origin.y
}

fn damage_covers_region(damage: &[NSRect], region: NSRect) -> bool {
    if !valid_damage_rect(region) {
        return false;
    }
    // A single containing native rectangle is a sufficient, allocation-free
    // proof. Do not infer coverage from bounds or accumulate partial callbacks.
    let mut covered = false;
    for &drawn in damage {
        if !valid_damage_rect(drawn) {
            return false;
        }
        covered |= contains_rect(drawn, region);
    }
    covered
}

fn native_damage_covers_region(view: &NSView, region: NSRect) -> bool {
    let mut rectangles: *const NSRect = std::ptr::null();
    let mut count: isize = 0;
    // Called only synchronously inside this view's drawRect:. AppKit owns this
    // array and guarantees its lifetime through the callback; never retain it.
    unsafe {
        view.getRectsBeingDrawn_count(&mut rectangles, &mut count);
    }
    if count <= 0
        || rectangles.is_null()
        || !rectangles.is_aligned()
        || count as usize > isize::MAX as usize / std::mem::size_of::<NSRect>()
    {
        return false;
    }
    let damage = unsafe { std::slice::from_raw_parts(rectangles, count as usize) };
    damage_covers_region(damage, region)
}

fn draw_is_visible(view: &NSView, dirty: NSRect, region: NSRect) -> bool {
    !view.isHiddenOrHasHiddenAncestor()
        && valid_damage_rect(dirty)
        && contains_rect(dirty, region)
        && contains_rect(view.visibleRect(), region)
        && view
            .window()
            .is_some_and(|window| window.isVisible() && window.isOnActiveSpace())
        && native_damage_covers_region(view, region)
}
#[derive(Default)]
struct ButtonIvars {
    paint: Option<Rc<PaintBinding>>,
    row: u16,
}
define_class!(
    #[unsafe(super = NSButton)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ButtonIvars]
    struct PassiveButton;
    impl PassiveButton {
        #[unsafe(method(acceptsFirstMouse:))] fn first_mouse(&self,_event:Option<&NSEvent>)->bool { true }
        #[unsafe(method(acceptsFirstResponder))] fn no_responder(&self)->bool { false }
        #[unsafe(method(drawRect:))]
        fn draw(&self, dirty:NSRect) {
            let batch = self.ivars().paint.as_ref().and_then(|paint| paint.snapshot());
            let screen = NSGraphicsContext::currentContext()
                .is_some_and(|context| context.isDrawingToScreen());
            unsafe { let _: () = msg_send![super(self), drawRect:dirty]; }
            if let Some(batch) = batch {
                batch.did_draw(self.ivars().row, screen && draw_is_visible(self, dirty, self.bounds()));
            }
        }
    }
);
impl PassiveButton {
    fn new(
        mtm: MainThreadMarker,
        frame: NSRect,
        paint: Option<Rc<PaintBinding>>,
        row: u16,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ButtonIvars { paint, row });
        unsafe { msg_send![super(this), initWithFrame:frame] }
    }
}
fn choice_action(tag: isize, enabled: &[usize]) -> Option<Action> {
    ((1..=9).contains(&tag) && enabled.contains(&(tag as usize)))
        .then(|| Action::Choice((tag - 1) as usize))
}

struct ActionsIvars {
    queue: RefCell<Vec<Command>>,
    menu: RefCell<Option<Retained<NSMenu>>>,
    enabled: RefCell<Vec<usize>>,
    buttons: RefCell<[Option<Retained<PassiveButton>>; 9]>,
}
define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ActionsIvars]
    struct OverlayActions;
    unsafe impl NSObjectProtocol for OverlayActions {}
    impl OverlayActions {
        #[unsafe(method(pressed:))]
        fn pressed(&self,sender:&NSButton) {
            let tag=sender.tag();
            if tag==100 {
                if NSEvent::modifierFlags_class().contains(NSEventModifierFlags::Option) {
                    self.ivars().queue.borrow_mut().push(Command::Action(Action::Bypass));
                } else if let Some(menu)=self.ivars().menu.borrow().as_ref().cloned() {
                    menu.popUpMenuPositioningItem_atLocation_inView(None, NSPoint::new(0.,-4.), Some(sender));
                }
            } else {
                let mut enabled = self.ivars().enabled.borrow_mut();
                let Some(action) = choice_action(tag, &enabled) else { return };
                let buttons = self.ivars().buttons.borrow();
                let Some(button) = buttons[(tag - 1) as usize].as_ref() else { return };
                // An ordinal belongs to this exact installed control, not to any
                // retained button from a previous publication with the same tag.
                let current: &NSButton = button;
                if !std::ptr::eq(current, sender)
                    || !sender.isEnabled()
                    || sender.isHiddenOrHasHiddenAncestor()
                    || !contains_rect(sender.visibleRect(), sender.bounds())
                    || !sender.window().is_some_and(|window| {
                        window.isVisible() && window.isOnActiveSpace()
                    })
                {
                    return;
                }
                enabled.clear();
                self.ivars().queue.borrow_mut().push(Command::Action(action));
            }
        }
    }
);
impl OverlayActions {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ActionsIvars {
            queue: RefCell::new(Vec::new()),
            menu: RefCell::new(None),
            enabled: RefCell::new(Vec::new()),
            buttons: RefCell::new(std::array::from_fn(|_| None)),
        });
        unsafe { msg_send![super(this), init] }
    }
    fn revoke_choices(&self) {
        self.ivars().enabled.borrow_mut().clear();
        // Also revoke actions queued before replacement but not yet delivered.
        self.ivars()
            .queue
            .borrow_mut()
            .retain(|command| !matches!(command, Command::Action(Action::Choice(_))));
        for button in self.ivars().buttons.borrow_mut().iter_mut() {
            if let Some(button) = button.take() {
                button.setEnabled(false);
                unsafe {
                    button.setTarget(None);
                    button.setAction(None);
                }
            }
        }
    }
}
struct DrawRun {
    text: String,
    x: f64,
    color: Retained<NSColor>,
}
struct GhostIvars {
    font: Retained<NSFont>,
    baseline: f64,
    runs: Vec<DrawRun>,
    fills: Vec<(NSRect, Retained<NSColor>)>,
    clip: NSRect,
    paint: Rc<PaintBinding>,
}
define_class!(
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = GhostIvars]
    struct GhostSurface;
    impl GhostSurface {
        #[unsafe(method(isOpaque))] fn transparent(&self)->bool { false }
        #[unsafe(method(hitTest:))] fn mouse_through(&self,_point:NSPoint)->Option<&NSView> { None }
        #[unsafe(method(drawRect:))]
        fn draw(&self,dirty:NSRect) {
            let batch = self.ivars().paint.snapshot();
            let Some(context)=NSGraphicsContext::currentContext() else {return};
            let cg:*mut objc2_core_graphics::CGContext=unsafe {msg_send![&*context,CGContext]};
            if cg.is_null() {return;}
            let cg = cg.cast::<c_void>();
            unsafe { CGContextSaveGState(cg); CGContextClipToRect(cg, self.ivars().clip); }
            for (frame,color) in &self.ivars().fills { color.setFill(); NSBezierPath::fillRect(*frame); }
            unsafe { CGContextSetTextMatrix(cg,Transform{a:1.,b:0.,c:0.,d:1.,tx:0.,ty:0.}); }
            for run in &self.ivars().runs {
                // CFString/NSString are toll-free bridged. Use CoreText's
                // context-color flag, not an AppKit NSColor as a CGColor value.
                let color_key=unsafe{&*(objc2_core_text::kCTForegroundColorFromContextAttributeName as *const _ as *const NSString)};
                let from_context=NSNumber::new_bool(true);
                let attributes=NSDictionary::from_slices(&[unsafe{NSFontAttributeName},color_key], &[&*self.ivars().font as &AnyObject,&*from_context as &AnyObject]);
                unsafe{let color:*mut objc2_core_graphics::CGColor=msg_send![&*run.color,CGColor];CGContextSetFillColorWithColor(cg,color.cast());}
                let attributed=unsafe {NSAttributedString::new_with_attributes(&NSString::from_str(&run.text),&attributes)};
                let line=unsafe {CTLine::with_attributed_string((&*attributed).as_ref())};
                unsafe {CGContextSetTextPosition(cg,run.x,self.ivars().baseline);CTLineDraw(&*line as *const _ as *const c_void,cg);}
            }
            unsafe { CGContextRestoreGState(cg); }
            if let Some(batch) = batch {
                batch.did_draw(1, context.isDrawingToScreen()
                    && draw_is_visible(self, dirty, self.ivars().clip));
            }
        }
    }
);
impl GhostSurface {
    fn new(mtm: MainThreadMarker, size: NSSize, ivars: GhostIvars) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ivars);
        let view: Retained<Self> =
            unsafe { msg_send![super(this),initWithFrame:NSRect::new(NSPoint::new(0.,0.),size)] };
        unsafe {
            let _: () = msg_send![&*view,setAccessibilityElement:false];
        }
        view
    }
}

fn rect(r: Rect) -> NSRect {
    NSRect::new(NSPoint::new(r[0], r[1]), NSSize::new(r[2], r[3]))
}
fn finite_rect(value: &Value) -> Option<Rect> {
    let values = value.as_array()?;
    if values.len() != 4 {
        return None;
    }
    let r = [
        values[0].as_f64()?,
        values[1].as_f64()?,
        values[2].as_f64()?,
        values[3].as_f64()?,
    ];
    (r.iter().all(|v| v.is_finite()) && r[2] >= 0. && r[3] > 0.).then_some(r)
}
fn rgb(v: [f64; 3]) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(v[0], v[1], v[2], 1.)
}
fn width(font: &NSFont, text: &str) -> f64 {
    let attrs = NSDictionary::from_slices(&[unsafe { NSFontAttributeName }], &[font as &AnyObject]);
    let text =
        unsafe { NSAttributedString::new_with_attributes(&NSString::from_str(text), &attrs) };
    let line = unsafe { CTLine::with_attributed_string((&*text).as_ref()) };
    unsafe {
        line.typographic_bounds(
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    }
}
// These are fixed renderer decisions, not descriptions derived from host content.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(usize)]
enum GhostRejection {
    Closed,
    Caret,
    RenderDecode,
    RenderInvalid,
    InlineDisabled,
    FontMetadata,
    CaretRowContract,
    FontUnavailable,
    FontDescriptor,
    FontIdentity,
    EmptyText,
    MultilineAdvance,
    CorrectionRect,
    CorrectionGeometry,
    CorrectionFit,
    NoWholeWordFits,
    ClipBounds,
    NoScreens,
    NoContainingScreen,
    BackingScale,
    FrameOffscreen,
    FrameOriginReadback,
    FrameContentReadback,
    SuffixBackground,
    SuffixInk,
    NotVisible,
}
impl GhostRejection {
    const ALL: [Self; 26] = [
        Self::Closed,
        Self::Caret,
        Self::RenderDecode,
        Self::RenderInvalid,
        Self::InlineDisabled,
        Self::FontMetadata,
        Self::CaretRowContract,
        Self::FontUnavailable,
        Self::FontDescriptor,
        Self::FontIdentity,
        Self::EmptyText,
        Self::MultilineAdvance,
        Self::CorrectionRect,
        Self::CorrectionGeometry,
        Self::CorrectionFit,
        Self::NoWholeWordFits,
        Self::ClipBounds,
        Self::NoScreens,
        Self::NoContainingScreen,
        Self::BackingScale,
        Self::FrameOffscreen,
        Self::FrameOriginReadback,
        Self::FrameContentReadback,
        Self::SuffixBackground,
        Self::SuffixInk,
        Self::NotVisible,
    ];
}
#[derive(Default, serde::Serialize)]
struct GhostGeometry {
    correction: bool,
    inline: bool,
    text_present: bool,
    advance_present: bool,
    suffix_present: bool,
    ax_caret_row: bool,
    system_font: bool,
    requested_font: String,
    resolved_font: Option<String>,
    font_size: f64,
    system_weight: f64,
    fixed_advance: f64,
    caret: Option<Rect>,
    clip: Option<Rect>,
    baseline: f64,
    ascent: f64,
    descent: f64,
    available_width: Option<f64>,
    drawn_width: Option<f64>,
    glyph_bounds: Option<Rect>,
    scale: Option<f64>,
    requested_frame: Option<Rect>,
    actual_frame: Option<Rect>,
    origin_error: Option<[f64; 2]>,
    drawing_origin: Option<[f64; 2]>,
    surface_visible: Option<bool>,
    on_active_space: Option<bool>,
}
#[derive(Default)]
struct WhatsAppGhostDiagnostic {
    attempts: u64,
    rendered: u64,
    rejections: [u64; 26],
    last_rejection: Option<GhostRejection>,
    last_geometry: Option<GhostGeometry>,
}
impl WhatsAppGhostDiagnostic {
    fn record(&mut self, geometry: GhostGeometry, rejection: Option<GhostRejection>) {
        self.attempts = self.attempts.saturating_add(1);
        if let Some(reason) = rejection {
            let count = &mut self.rejections[reason as usize];
            *count = count.saturating_add(1);
        } else {
            self.rendered = self.rendered.saturating_add(1);
        }
        self.last_rejection = rejection;
        self.last_geometry = Some(geometry);
    }
}
fn whatsapp_ghost_event(event: &Value) -> bool {
    event["key"]["app_id"].as_str() == Some("net.whatsapp.WhatsApp")
        && event["type"].as_str() == Some("suggestion")
        && matches!(
            event["mode"].as_str().unwrap_or("completion"),
            "completion" | "correction"
        )
}
fn font_diagnostic_name(name: &str) -> String {
    // Font metadata only; cap even malformed native metadata. Never copy an event.
    name.chars().filter(|c| !c.is_control()).take(128).collect()
}
fn resolve_font(
    render: &InlineRender,
    diagnostic: &mut Option<GhostGeometry>,
) -> Result<Retained<NSFont>, GhostRejection> {
    if render.font_name.is_empty()
        || !render.font_size.is_finite()
        || !(1.0..=300.0).contains(&render.font_size)
        || !render.system_weight.is_finite()
        || !render.fixed_advance.is_finite()
        || !(0.0..=300.0).contains(&render.fixed_advance)
    {
        return Err(GhostRejection::FontMetadata);
    }
    // The fitted AX row is legal only at a line end, with no repaint of host text.
    if render.source == "macos-ax-caret-row"
        && (!render.system_font || !render.suffix.is_empty() || render.suffix_width != 0.)
    {
        return Err(GhostRejection::CaretRowContract);
    }
    let mut font = if render.system_font {
        Some(NSFont::systemFontOfSize_weight(
            render.font_size,
            render.system_weight,
        ))
    } else if render.font_name.starts_with('.') {
        None
    } else {
        NSFont::fontWithName_size(&NSString::from_str(&render.font_name), render.font_size)
    }
    .ok_or(GhostRejection::FontUnavailable)?;
    if let Some(geometry) = diagnostic.as_mut() {
        geometry.resolved_font = Some(font_diagnostic_name(&font.fontName().to_string()));
    }
    if render.fixed_advance > 0. {
        let value = NSNumber::new_f64(render.fixed_advance);
        let attrs = NSDictionary::from_slices(
            &[unsafe { NSFontFixedAdvanceAttribute }],
            &[&*value as &AnyObject],
        );
        let descriptor = unsafe {
            font.fontDescriptor()
                .fontDescriptorByAddingAttributes(&attrs)
        };
        font = NSFont::fontWithDescriptor_size(&descriptor, render.font_size)
            .ok_or(GhostRejection::FontDescriptor)?;
    }
    if let Some(geometry) = diagnostic.as_mut() {
        geometry.resolved_font = Some(font_diagnostic_name(&font.fontName().to_string()));
    }
    if font.fontName().to_string() != render.font_name {
        return Err(GhostRejection::FontIdentity);
    }
    Ok(font)
}
fn fit_text<'a>(text: &'a str, available: f64, measure: impl Fn(&str) -> f64) -> &'a str {
    if !available.is_finite() || available <= 0. || text.is_empty() {
        return "";
    }
    if measure(text) <= available {
        return text;
    }
    let mut best = "";
    for (i, c) in text.char_indices() {
        if c.is_whitespace() && !text[..i].trim().is_empty() {
            if measure(&text[..i]) > available {
                break;
            }
            best = &text[..i];
        }
    }
    best
}
fn visible_choices(rows: &[String], available: f64, measure: impl Fn(&str) -> f64) -> Vec<&str> {
    let mut shown = Vec::with_capacity(rows.len().min(9));
    if !available.is_finite() || available <= 0. {
        return shown;
    }
    for row in rows {
        // These are single-line buttons and newline-delimited receipts. Reject
        // unsupported lines rather than silently changing an insertable payload.
        if row.is_empty() || row.contains(['\r', '\n', '\u{2028}', '\u{2029}']) {
            continue;
        }
        let measured = measure(&format!("{}   {}", shown.len() + 1, row));
        if measured.is_finite() && measured <= available {
            shown.push(row.as_str());
            if shown.len() == 9 {
                break;
            }
        }
    }
    shown
}
fn visible(panel: &PassivePanel) -> bool {
    panel.isVisible() && panel.isOnActiveSpace()
}
fn cached(identity: &Value, previous: Option<&(Value, String)>, shown: bool) -> bool {
    previous.is_some_and(|(old, text)| old == identity && !text.is_empty() && shown)
}
fn accessory_frame(caret: Rect, top: f64, w: f64, h: f64, visible: NSRect) -> Option<NSRect> {
    // Stay just above-left of the known caret line, never on its text. Prefer
    // the caret itself, not the potentially distant left edge of a wide field.
    let y = top - caret[1] + 2.;
    let frame = rect([caret[0] - w - 2., y, w, h]);
    let safe = NSRect::new(
        NSPoint::new(visible.origin.x + 4., visible.origin.y + 4.),
        NSSize::new(visible.size.width - 8., visible.size.height - 8.),
    );
    // Do not clamp or flip onto the caret, another line, or an adjacent screen.
    contains_rect(safe, frame).then_some(frame)
}
fn position(mtm: MainThreadMarker, r: Rect, w: f64, h: f64, accessory: bool) -> Option<NSRect> {
    let screens = NSScreen::screens(mtm);
    let first = screens.firstObject()?;
    let top = first.frame().origin.y + first.frame().size.height;
    let bottom = top - r[1] - r[3];
    let screen = screens
        .iter()
        .find(|s| contains_point(s.frame(), r[0], bottom))
        .unwrap_or(first);
    let v = screen.visibleFrame();
    if accessory {
        return accessory_frame(r, top, w, h, v);
    }
    let minx = v.origin.x;
    let maxx = minx + v.size.width;
    let miny = v.origin.y;
    let maxy = miny + v.size.height;
    let mut x = r[0];
    let mut y = bottom - h - 6.;
    if bottom - miny < h + 40. {
        y = bottom + r[3] + 6.;
    }
    x = x.max(minx + 8.).min(maxx - w - 8.);
    y = y.max(miny + 8.).min(maxy - h - 8.);
    Some(rect([x, y, w, h]))
}
fn contains_point(r: NSRect, x: f64, y: f64) -> bool {
    x >= r.origin.x
        && x <= r.origin.x + r.size.width
        && y >= r.origin.y
        && y <= r.origin.y + r.size.height
}
fn contains_rect(outer: NSRect, inner: NSRect) -> bool {
    contains_point(outer, inner.origin.x, inner.origin.y)
        && contains_point(
            outer,
            inner.origin.x + inner.size.width,
            inner.origin.y + inner.size.height,
        )
}

fn accessory_image(mtm: MainThreadMarker) -> Result<Retained<NSImage>> {
    // Rasterized from assets/menubar/menubar-active.svg at 1x/2x with AppKit;
    // preserve the supplied face and its 18×16-point template canvas.
    let image = NSImage::initWithSize(NSImage::alloc(), NSSize::new(18., 16.));
    for bytes in [
        include_bytes!("../../assets/active-template-18.png").as_slice(),
        include_bytes!("../../assets/active-template-36.png").as_slice(),
    ] {
        let data = NSData::with_bytes(bytes);
        let representation = NSBitmapImageRep::imageRepWithData(&data)
            .ok_or_else(|| anyhow!("Invalid embedded accessory image"))?;
        representation.setSize(NSSize::new(18., 16.));
        image.addRepresentation(&representation);
    }
    let _ = mtm;
    image.setTemplate(true);
    Ok(image)
}

pub struct OverlayOwner {
    mtm: MainThreadMarker,
    ghost: Retained<PassivePanel>,
    chooser: Retained<PassivePanel>,
    accessory: Retained<PassivePanel>,
    button: Retained<PassiveButton>,
    target: Retained<OverlayActions>,
    show_accessory: bool,
    caret: Option<Rect>,
    field: Option<Rect>,
    docs: Option<Rect>,
    ghost_state: Option<(Value, String)>,
    chooser_state: Option<(Value, String)>,
    ghost_event: Option<Value>,
    chooser_event: Option<Value>,
    environment: Value,
    whatsapp_diagnostic: WhatsAppGhostDiagnostic,
    paint_reporter: Option<PaintReporter>,
    ghost_paint: Rc<PaintBinding>,
    chooser_paint: Rc<PaintBinding>,
    closed: bool,
}
impl OverlayOwner {
    pub fn new() -> Result<Self> {
        let mtm = MainThreadMarker::new()
            .ok_or_else(|| anyhow!("OverlayOwner requires the main AppKit thread"))?;
        let ghost = PassivePanel::create(
            mtm,
            "inklow.suggestion",
            "BroTypist suggestion",
            NSPopUpMenuWindowLevel,
            true,
        );
        ghost.setHasShadow(false);
        ghost.setIgnoresMouseEvents(true);
        let chooser = PassivePanel::create(
            mtm,
            "inklow.chooser",
            "BroTypist suggestions",
            NSPopUpMenuWindowLevel,
            true,
        );
        let accessory = PassivePanel::create(
            mtm,
            "inklow.accessory",
            "BroTypist accessory",
            NSPopUpMenuWindowLevel,
            true,
        );
        accessory.setHasShadow(false);
        let target = OverlayActions::new(mtm);
        let button = PassiveButton::new(mtm, rect([1., 1., 22., 22.]), None, 0);
        button.setBordered(false);
        button.setTag(100);
        button.setImagePosition(NSCellImagePosition::ImageOnly);
        button.setContentTintColor(Some(&NSColor::secondaryLabelColor()));
        let accessory_image = accessory_image(mtm)?;
        button.setImage(Some(&*accessory_image));
        button.setIdentifier(Some(&NSString::from_str("inklow.accessory.menu")));
        unsafe {
            button.setTarget(Some(&target));
            button.setAction(Some(sel!(pressed:)));
            let _: () = msg_send![&*button,setAccessibilityIdentifier:&*NSString::from_str("inklow.accessory.menu")];
        }
        let surface = NSView::initWithFrame(NSView::alloc(mtm), rect([0., 0., 24., 24.]));
        surface.addSubview(&button);
        accessory.setContentView(Some(&surface));
        let mut this = Self {
            mtm,
            ghost,
            chooser,
            accessory,
            button,
            target,
            show_accessory: true,
            caret: None,
            field: None,
            docs: None,
            ghost_state: None,
            chooser_state: None,
            ghost_event: None,
            chooser_event: None,
            environment: Value::Null,
            whatsapp_diagnostic: WhatsAppGhostDiagnostic::default(),
            paint_reporter: None,
            ghost_paint: Rc::default(),
            chooser_paint: Rc::default(),
            closed: false,
        };
        this.environment = this.environment_signature();
        this.label(false);
        Ok(this)
    }
    /// Observations originate only in successful native draw callbacks.
    pub fn set_paint_reporter(&mut self, reporter: Option<PaintReporter>) {
        self.ghost_paint.clear();
        self.chooser_paint.clear();
        self.paint_reporter = reporter;
    }
    #[cfg(feature = "native-smoke")]
    pub fn smoke_ghost_diagnostic(&self) -> Value {
        serde_json::json!({
            "rejection":self.whatsapp_diagnostic.last_rejection,
            "geometry":self.whatsapp_diagnostic.last_geometry,
        })
    }
    /// Explicit notification only. The retained snapshot has fixed fields and is
    /// collected solely for the exact native WhatsApp suggestion app identity.
    pub fn export_whatsapp_diagnostic(&self) -> Result<()> {
        let diagnostic = &self.whatsapp_diagnostic;
        let rejections: Vec<_> = GhostRejection::ALL.iter().map(|reason| {
            serde_json::json!({"reason":reason,"count":diagnostic.rejections[*reason as usize]})
        }).collect();
        let value = serde_json::json!({
            "schema":"brotypist-whatsapp-renderer-diagnostic/1",
            "version":env!("CARGO_PKG_VERSION"),
            "app_id":"net.whatsapp.WhatsApp",
            "observed":diagnostic.attempts != 0,
            "attempts":diagnostic.attempts,
            "rendered":diagnostic.rendered,
            "rejections":rejections,
            "last_rejection":diagnostic.last_rejection,
            "last_geometry":diagnostic.last_geometry,
        });
        let directory = crate::paths::config_dir()?.join("diagnostics");
        crate::paths::private_directory(&directory)?;
        let mut temporary = tempfile::Builder::new()
            .prefix(".inklow-")
            .tempfile_in(&directory)?;
        serde_json::to_writer_pretty(&mut temporary, &value)?;
        temporary.write_all(b"\n")?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(directory.join("renderer-whatsapp.json"))
            .map_err(|error| error.error)?;
        Ok(())
    }
    pub fn configure(&mut self, show_accessory: bool, _accept_key: &str) {
        self.show_accessory = show_accessory;
        self.place_accessory();
    }
    /// Content-free native hit regions for the currently visible, complete rows.
    /// This is geometry only: runtime still validates the current edit authority.
    pub fn choice_targets(&self) -> Option<ChooserHitRegion> {
        let (identity, text) = self.chooser_state.as_ref()?;
        if self.closed
            || text.is_empty()
            || !visible(&self.chooser)
            || identity.get(4) != Some(&self.environment)
        {
            return None;
        }
        let window_number = self.chooser.windowNumber();
        if window_number <= 0 {
            return None;
        }
        let screens = NSScreen::screens(self.mtm);
        let first = screens.firstObject()?;
        let top = first.frame().origin.y + first.frame().size.height;
        let enabled = self.target.ivars().enabled.borrow();
        let mut region = ChooserHitRegion {
            window_number,
            button_rects: [[0.; 4]; 9],
            button_count: 0,
        };
        let buttons = self.target.ivars().buttons.borrow();
        for (index, button) in buttons.iter().enumerate() {
            if !enabled.contains(&(index + 1)) {
                continue;
            }
            let button = button.as_ref()?;
            if !button.isEnabled()
                || button.isHidden()
                || button.window()?.windowNumber() != window_number
            {
                return None;
            }
            let frame = self
                .chooser
                .convertRectToScreen(button.convertRect_toView(button.bounds(), None));
            let bounds = [
                frame.origin.x,
                top - frame.origin.y - frame.size.height,
                frame.size.width,
                frame.size.height,
            ];
            if !bounds.iter().all(|value| value.is_finite()) || bounds[2] <= 0. || bounds[3] <= 0. {
                return None;
            }
            region.button_rects[region.button_count] = bounds;
            region.button_count += 1;
        }
        (region.button_count != 0).then_some(region)
    }

    /// The shell builds this minimal native menu with captured app/site/token
    /// intents and retains its own action target through synchronous tracking.
    pub fn set_field_menu(&mut self, menu: &NSMenu) {
        self.target
            .ivars()
            .menu
            .replace(unsafe { Retained::retain(menu as *const _ as *mut _) });
    }
    fn environment_signature(&self) -> Value {
        let screens: Vec<Value> = NSScreen::screens(self.mtm)
            .iter()
            .map(|screen| {
                let frame = screen.frame();
                let visible = screen.visibleFrame();
                serde_json::json!([
                    frame.origin.x,
                    frame.origin.y,
                    frame.size.width,
                    frame.size.height,
                    visible.origin.x,
                    visible.origin.y,
                    visible.size.width,
                    visible.size.height,
                    screen.backingScaleFactor()
                ])
            })
            .collect();
        let workspace = NSWorkspace::sharedWorkspace();
        let appearance = self
            .ghost
            .effectiveAppearance()
            .bestMatchFromAppearancesWithNames(&objc2_foundation::NSArray::from_slice(&[
                unsafe { NSAppearanceNameAqua },
                unsafe { NSAppearanceNameDarkAqua },
            ]))
            .map(|name| name.to_string());
        serde_json::json!([
            screens,
            appearance,
            workspace.accessibilityDisplayShouldIncreaseContrast(),
            workspace.accessibilityDisplayShouldReduceTransparency()
        ])
    }
    fn label(&mut self, docs: bool) {
        let label = if docs {
            "BroTypist Google Docs accessibility help"
        } else {
            "BroTypist menu"
        };
        let tip = if docs {
            "Google Docs accessibility help — open the menu for setup. Completion support is not verified. Option-click sends a normal Tab."
        } else {
            "BroTypist — typing settings and pause controls. Option-click sends a normal Tab."
        };
        unsafe {
            let _: () = msg_send![&*self.button,setAccessibilityLabel:&*NSString::from_str(label)];
        }
        self.button.setToolTip(Some(&NSString::from_str(tip)));
    }
    fn place_accessory(&mut self) {
        let anchor = self.docs.or(self.caret).or(self.field);
        if self.show_accessory {
            if let Some(frame) = anchor.and_then(|r| position(self.mtm, r, 24., 24., true)) {
                self.accessory.setFrame_display(frame, true);
                self.accessory.orderFrontRegardless();
                return;
            }
        }
        self.accessory.orderOut(None);
    }
    fn clear(&mut self, hide: bool, panel: Option<&str>) {
        if panel.is_none() || panel == Some("ghost") {
            self.ghost_paint.clear();
            self.ghost.orderOut(None);
            self.ghost_state = None;
            self.ghost_event = None;
        }
        if panel.is_none() || panel == Some("chooser") {
            self.chooser_paint.clear();
            publish_chooser_hit_region(None);
            self.chooser.orderOut(None);
            self.target.revoke_choices();
            self.chooser_state = None;
            self.chooser_event = None;
        }
        if hide {
            self.caret = None;
            self.field = None;
            self.docs = None;
            self.label(false);
            self.accessory.orderOut(None);
        }
    }
    fn ack(event: &Value, text: String, mode: &str) -> Option<Command> {
        Some(Command::PresentationResult {
            key: serde_json::from_value::<SnapshotKey>(event.get("key")?.clone()).ok()?,
            authority: serde_json::from_value::<Authority>(event.get("authority")?.clone()).ok()?,
            revision: event.get("revision")?.as_u64()?,
            visible_text: text,
            mode: mode.into(),
        })
    }
    pub fn handle(&mut self, event: &Value) -> Result<Vec<Command>> {
        if self.closed {
            if whatsapp_ghost_event(event) {
                self.whatsapp_diagnostic
                    .record(GhostGeometry::default(), Some(GhostRejection::Closed));
            }
            return Ok(Vec::new());
        }
        let mut commands = Vec::new();
        let environment = self.environment_signature();
        if environment != self.environment {
            publish_chooser_hit_region(None);
            self.environment = environment;
            self.ghost_paint.clear();
            self.chooser_paint.clear();
            self.ghost.orderOut(None);
            self.chooser.orderOut(None);
            self.accessory.orderOut(None);
        }
        match event["type"].as_str().unwrap_or("") {
            "clear" => self.clear(
                event["hide_accessory"].as_bool().unwrap_or(false),
                event["panel"].as_str(),
            ),
            "caret" => {
                self.docs = None;
                self.label(false);
                self.caret = finite_rect(&event["rect"]);
                self.field = finite_rect(&event["field_rect"]);
                if self.caret.is_none() && self.field.is_none() {
                    self.clear(true, None);
                } else {
                    self.place_accessory();
                }
            }
            "docs_help" => {
                if event["hint"].is_null() {
                    if self.docs.is_some() {
                        self.clear(true, None);
                    }
                } else {
                    self.clear(true, None);
                    self.docs = finite_rect(&event["hint"]["anchor"]);
                    self.label(true);
                    self.place_accessory();
                }
            }
            "suggestion" => {
                let mode = event["mode"].as_str().unwrap_or("completion");
                let choices: Vec<String> = event["choices"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .take(9)
                    .map(str::to_owned)
                    .collect();
                let choice_mode = if matches!(mode, "completion" | "correction") {
                    event["choices_mode"].as_str().unwrap_or(mode)
                } else {
                    mode
                };
                if finite_rect(&event["rect"]).is_none() {
                    if whatsapp_ghost_event(event) {
                        self.whatsapp_diagnostic.record(
                            GhostGeometry {
                                correction: mode == "correction",
                                inline: event["inline"].as_bool() == Some(true),
                                ..Default::default()
                            },
                            Some(GhostRejection::Caret),
                        );
                    }
                    self.clear(true, None);
                    if let Some(c) = Self::ack(event, String::new(), mode) {
                        commands.push(c);
                    }
                    if !choices.is_empty() && choice_mode != mode {
                        if let Some(c) = Self::ack(event, String::new(), choice_mode) {
                            commands.push(c);
                        }
                    }
                    return Ok(commands);
                }
                self.docs = None;
                self.label(false);
                self.caret = finite_rect(&event["rect"]);
                self.field = finite_rect(&event["field_rect"]);
                self.place_accessory();
                if matches!(mode, "completion" | "correction") {
                    let identity = serde_json::json!([
                        event["text"],
                        mode,
                        event["rect"],
                        event["render"],
                        event["inline"],
                        event["advance"],
                        event["correction"],
                        self.environment_signature()
                    ]);
                    if !cached(&identity, self.ghost_state.as_ref(), visible(&self.ghost)) {
                        self.ghost_paint.clear();
                        self.ghost_paint = Rc::default();
                        self.ghost_paint.bind(
                            PaintIdentity::from_event(event, PresentationLane::Inline),
                            self.paint_reporter.as_ref(),
                            1,
                        );
                        let text = self.present_ghost(event, mode);
                        self.ghost_state = Some((identity, text));
                    } else {
                        self.ghost_paint.bind(
                            PaintIdentity::from_event(event, PresentationLane::Inline),
                            self.paint_reporter.as_ref(),
                            1,
                        );
                    }
                    self.ghost_event = Some(event.clone());
                    if let Some(c) = Self::ack(
                        event,
                        self.ghost_state
                            .as_ref()
                            .map(|s| s.1.clone())
                            .unwrap_or_default(),
                        mode,
                    ) {
                        commands.push(c);
                    }
                    if !choices.is_empty() {
                        self.present_chooser(event, &choices, choice_mode, &mut commands);
                    }
                } else {
                    let rows = if choices.is_empty() {
                        vec![event["text"].as_str().unwrap_or("").to_owned()]
                    } else {
                        choices
                    };
                    self.present_chooser(event, &rows, mode, &mut commands);
                }
            }
            _ => {}
        }
        commands.extend(self.target.ivars().queue.take());
        Ok(commands)
    }
    fn present_chooser(
        &mut self,
        event: &Value,
        rows: &[String],
        mode: &str,
        commands: &mut Vec<Command>,
    ) {
        let font = NSFont::systemFontOfSize(13.);
        let rows = visible_choices(rows, 440., |s| width(&font, s));
        let paint_identity = PaintIdentity::from_event(event, PresentationLane::Chooser);
        let required = (1u16 << rows.len()) - 1;
        let identity = serde_json::json!([
            rows,
            mode,
            event["rect"],
            event["field_anchor"],
            self.environment_signature(),
            event["key"],
            event["authority"],
            event["revision"]
        ]);
        let repaint = !cached(
            &identity,
            self.chooser_state.as_ref(),
            visible(&self.chooser),
        );
        if repaint {
            self.chooser_paint.clear();
            self.chooser_paint = Rc::default();
        }
        self.chooser_paint
            .bind(paint_identity, self.paint_reporter.as_ref(), required);
        if repaint {
            publish_chooser_hit_region(None);
            self.chooser.orderOut(None);
            self.target.revoke_choices();
            let text = if rows.is_empty() {
                String::new()
            } else {
                let w = (rows
                    .iter()
                    .enumerate()
                    .map(|(i, row)| width(&font, &format!("{}   {}", i + 1, row)))
                    .fold(0., f64::max)
                    + 40.)
                    .clamp(180., 480.);
                let h = rows.len() as f64 * 30. + 8.;
                let surface = NSVisualEffectView::initWithFrame(
                    NSVisualEffectView::alloc(self.mtm),
                    rect([0., 0., w, h]),
                );
                surface.setMaterial(NSVisualEffectMaterial::Popover);
                surface.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
                let workspace = NSWorkspace::sharedWorkspace();
                surface.setState(
                    if workspace.accessibilityDisplayShouldReduceTransparency() {
                        NSVisualEffectState::Inactive
                    } else {
                        NSVisualEffectState::Active
                    },
                );
                surface.setWantsLayer(true);
                unsafe {
                    if let Some(layer) = surface.layer() {
                        let _: () = msg_send![&*layer,setCornerRadius:8.0f64];
                        let _: () = msg_send![&*layer,setMasksToBounds:true];
                        let border: f64 = if workspace.accessibilityDisplayShouldIncreaseContrast()
                        {
                            2.
                        } else {
                            1.
                        };
                        let _: () = msg_send![&*layer,setBorderWidth:border];
                        let color: *mut objc2_core_graphics::CGColor =
                            msg_send![&*NSColor::separatorColor(), CGColor];
                        let _: () = msg_send![&*layer,setBorderColor:color];
                        if workspace.accessibilityDisplayShouldReduceTransparency() {
                            let bg: *mut objc2_core_graphics::CGColor =
                                msg_send![&*NSColor::windowBackgroundColor(), CGColor];
                            let _: () = msg_send![&*layer,setBackgroundColor:bg];
                        }
                    }
                }
                self.target
                    .ivars()
                    .enabled
                    .replace((1..=rows.len()).collect());
                for (i, row) in rows.iter().enumerate() {
                    let button = PassiveButton::new(
                        self.mtm,
                        rect([10., h - (i + 1) as f64 * 30. - 4., w - 20., 30.]),
                        Some(self.chooser_paint.clone()),
                        1 << i,
                    );
                    button.setTitle(&NSString::from_str(&format!("{}   {}", i + 1, row)));
                    button.setFont(Some(&font));
                    button.setContentTintColor(Some(&NSColor::labelColor()));
                    button.setAlignment(NSTextAlignment::Left);
                    button.setBordered(false);
                    button.setEnabled(true);
                    button.setTag((i + 1) as isize);
                    button.setIdentifier(Some(&NSString::from_str(&format!(
                        "inklow.suggestion.choice.{}",
                        i + 1
                    ))));
                    button.setToolTip(Some(&NSString::from_str(row)));
                    unsafe {
                        button.setTarget(Some(&self.target));
                        button.setAction(Some(sel!(pressed:)));
                        let _: () = msg_send![&*button,setAccessibilityIdentifier:&*NSString::from_str(&format!("inklow.suggestion.choice.{}",i+1))];
                        let _: () = msg_send![&*button,setAccessibilityLabel:&*NSString::from_str(&format!("Accept choice {}: {}",i+1,row))];
                        if let Some(cell) = button.cell() {
                            let _: () = msg_send![&*cell,setWraps:false];
                            let _: () = msg_send![&*cell,setLineBreakMode:2usize];
                        }
                    }
                    surface.addSubview(&button);
                    self.target.ivars().buttons.borrow_mut()[i] = Some(button);
                }
                self.chooser.setContentView(Some(&surface));
                if let Some(mut anchor) = finite_rect(&event["rect"]) {
                    if event["field_anchor"].as_bool() == Some(true) {
                        anchor[2] = 0.;
                    }
                    if let Some(frame) = position(self.mtm, anchor, w, h, false) {
                        self.chooser.setFrame_display(frame, true);
                        self.chooser.orderFrontRegardless();
                        self.chooser.displayIfNeeded();
                    }
                }
                if visible(&self.chooser) {
                    rows.join("\n")
                } else {
                    self.chooser_paint.clear();
                    self.target.revoke_choices();
                    String::new()
                }
            };
            self.chooser_state = Some((identity, text));
        }
        self.chooser_event = Some(event.clone());
        publish_chooser_hit_region(self.choice_targets());
        if let Some(c) = Self::ack(
            event,
            self.chooser_state
                .as_ref()
                .map(|s| s.1.clone())
                .unwrap_or_default(),
            mode,
        ) {
            commands.push(c);
        }
    }
    fn present_ghost(&mut self, event: &Value, mode: &str) -> String {
        let mut diagnostic = whatsapp_ghost_event(event).then(|| GhostGeometry {
            correction: mode == "correction",
            inline: event["inline"].as_bool() == Some(true),
            text_present: event["text"].as_str().is_some_and(|s| !s.is_empty()),
            advance_present: event["advance"].as_str().is_some_and(|s| !s.is_empty()),
            ..Default::default()
        });
        let result = (|| -> Result<String, GhostRejection> {
            let caret = finite_rect(&event["rect"]).ok_or(GhostRejection::Caret)?;
            if let Some(geometry) = diagnostic.as_mut() {
                geometry.caret = Some(caret);
            }
            let render: InlineRender = serde_json::from_value(event["render"].clone())
                .map_err(|_| GhostRejection::RenderDecode)?;
            if let Some(geometry) = diagnostic.as_mut() {
                geometry.requested_font = font_diagnostic_name(&render.font_name);
                geometry.font_size = render.font_size;
                geometry.system_font = render.system_font;
                geometry.system_weight = render.system_weight;
                geometry.fixed_advance = render.fixed_advance;
                geometry.ax_caret_row = render.source == "macos-ax-caret-row";
                geometry.suffix_present = !render.suffix.is_empty();
                geometry.clip = Some(render.clip_rect);
                geometry.baseline = render.baseline;
                geometry.ascent = render.ascent;
                geometry.descent = render.descent;
            }
            if !render.valid(Some(caret)) {
                return Err(GhostRejection::RenderInvalid);
            }
            if mode == "completion" && event["inline"].as_bool() != Some(true) {
                return Err(GhostRejection::InlineDisabled);
            }
            let font = resolve_font(&render, &mut diagnostic)?;
            let text = event["text"]
                .as_str()
                .unwrap_or("")
                .split(['\n', '\r'])
                .next()
                .unwrap_or("");
            let advance = event["advance"].as_str().unwrap_or("");
            if text.is_empty() {
                return Err(GhostRejection::EmptyText);
            }
            if advance.contains(['\r', '\n']) {
                return Err(GhostRejection::MultilineAdvance);
            }
            let x = caret[0] + width(&font, advance);
            let [cx, cy, cw, ch] = render.clip_rect;
            let available = cx + cw - x - render.suffix_width;
            if let Some(geometry) = diagnostic.as_mut() {
                geometry.available_width = Some(available);
            }
            let mut left = x;
            let mut gap = 0.;
            let mut strike = None;
            let drawn = if mode == "correction" {
                let r = finite_rect(&event["correction"]["rect"])
                    .ok_or(GhostRejection::CorrectionRect)?;
                if r[2] <= 0.
                    || !advance.is_empty()
                    || !(r[1] < render.baseline && render.baseline <= r[1] + r[3] + 1.)
                    || r[0] < cx
                    || r[0] + r[2] > x + 1.5
                {
                    return Err(GhostRejection::CorrectionGeometry);
                }
                if (r[0] + r[2] - x).abs() <= 1.5 {
                    gap = width(&font, " ");
                }
                if width(&font, text) + gap > available {
                    return Err(GhostRejection::CorrectionFit);
                }
                left = r[0];
                strike = Some(r);
                text
            } else {
                fit_text(text, available, |s| width(&font, s))
            };
            if drawn.is_empty() {
                return Err(GhostRejection::NoWholeWordFits);
            }
            let ghost_width = width(&font, drawn);
            if let Some(geometry) = diagnostic.as_mut() {
                geometry.drawn_width = Some(ghost_width);
            }
            let right = x + gap + ghost_width + render.suffix_width;
            let displaced = !render.suffix.trim().is_empty();
            let mut top = render.baseline - render.ascent;
            let mut bottom = render.baseline + render.descent;
            if displaced {
                top = top.min(caret[1]);
                bottom = bottom.max(caret[1] + caret[3]);
            }
            if let Some(geometry) = diagnostic.as_mut() {
                geometry.glyph_bounds = Some([left, top, right - left, bottom - top]);
            }
            if left < cx || right > cx + cw + 0.5 || top < cy || bottom > cy + ch + 1. {
                return Err(GhostRejection::ClipBounds);
            }
            let screens = NSScreen::screens(self.mtm);
            let first = screens.firstObject().ok_or(GhostRejection::NoScreens)?;
            let origin_top = first.frame().origin.y + first.frame().size.height;
            let cocoa_bottom = origin_top - bottom;
            let screen = screens
                .iter()
                .find(|s| contains_point(s.frame(), left, cocoa_bottom))
                .ok_or(GhostRejection::NoContainingScreen)?;
            let scale = screen.backingScaleFactor();
            if let Some(geometry) = diagnostic.as_mut() {
                geometry.scale = Some(scale);
            }
            if !scale.is_finite() || scale <= 0. {
                return Err(GhostRejection::BackingScale);
            }
            // AppKit may round window origins to whole logical points even on
            // Retina screens. Outward-round this transparent window in points;
            // the drawing offsets preserve the exact host glyph coordinates.
            let fx = left.floor();
            let fy = cocoa_bottom.floor();
            let mut dx = left - fx;
            let mut dy = cocoa_bottom - fy;
            let frame = rect([
                fx,
                fy,
                (right - left + dx).ceil(),
                (bottom - top + dy).ceil(),
            ]);
            if let Some(geometry) = diagnostic.as_mut() {
                geometry.requested_frame = Some([
                    frame.origin.x,
                    frame.origin.y,
                    frame.size.width,
                    frame.size.height,
                ]);
            }
            if !screens.iter().any(|s| contains_rect(s.frame(), frame)) {
                return Err(GhostRejection::FrameOffscreen);
            }
            self.ghost.setFrame_display(frame, false);
            // NSWindow can quantize origins. Correct the owned drawing origin,
            // never host geometry; strict readback bounds the subpoint residual.
            let actual = self.ghost.frame();
            let ex = fx - actual.origin.x;
            let ey = fy - actual.origin.y;
            if let Some(geometry) = diagnostic.as_mut() {
                geometry.actual_frame = Some([
                    actual.origin.x,
                    actual.origin.y,
                    actual.size.width,
                    actual.size.height,
                ]);
                geometry.origin_error = Some([ex, ey]);
            }
            if ex.abs() > 1. / scale + 0.001 || ey.abs() > 1. / scale + 0.001 {
                return Err(GhostRejection::FrameOriginReadback);
            }
            dx += ex;
            dy += ey;
            if let Some(geometry) = diagnostic.as_mut() {
                geometry.drawing_origin = Some([dx, dy]);
            }
            if dx < 0.
                || dy < 0.
                || right - left + dx > actual.size.width + 0.001
                || bottom - top + dy > actual.size.height + 0.001
            {
                return Err(GhostRejection::FrameContentReadback);
            }
            let workspace = NSWorkspace::sharedWorkspace();
            let contrast = workspace.accessibilityDisplayShouldIncreaseContrast();
            let color = if let (Some(ink), Some(bg)) = (render.text_rgb, render.background_rgb) {
                let share = if contrast { 0.6 } else { 0.42 };
                rgb(std::array::from_fn(|i| bg[i] + (ink[i] - bg[i]) * share))
            } else {
                let dark = self
                    .ghost
                    .effectiveAppearance()
                    .bestMatchFromAppearancesWithNames(&objc2_foundation::NSArray::from_slice(&[
                        unsafe { NSAppearanceNameAqua },
                        unsafe { NSAppearanceNameDarkAqua },
                    ]))
                    .is_some_and(|n| {
                        n.to_string() == unsafe { NSAppearanceNameDarkAqua }.to_string()
                    });
                rgb(match (dark, contrast) {
                    (true, true) => [154. / 255., 156. / 255., 163. / 255.],
                    (false, true) => [110. / 255., 112. / 255., 120. / 255.],
                    (true, false) => [108. / 255., 111. / 255., 119. / 255.],
                    _ => [162. / 255., 164. / 255., 170. / 255.],
                })
            };
            let baseline = dy + bottom - render.baseline;
            let gx = dx + x - left + gap;
            let mut runs = vec![DrawRun {
                text: drawn.into(),
                x: gx,
                color: color.clone(),
            }];
            let mut fills = Vec::new();
            if displaced {
                fills.push((
                    rect([dx + x - left, dy, right - x, bottom - top]),
                    rgb(render
                        .background_rgb
                        .ok_or(GhostRejection::SuffixBackground)?),
                ));
                runs.push(DrawRun {
                    text: render.suffix.clone(),
                    x: gx + ghost_width,
                    color: rgb(render.text_rgb.ok_or(GhostRejection::SuffixInk)?),
                });
            }
            if let Some(strike) = strike {
                let thickness = font.underlineThickness().max(1.);
                let middle = baseline + font.xHeight() / 2.;
                fills.push((
                    rect([dx, middle - thickness / 2., strike[2], thickness]),
                    color,
                ));
            }
            let surface = GhostSurface::new(
                self.mtm,
                actual.size,
                GhostIvars {
                    font,
                    baseline,
                    runs,
                    fills,
                    clip: rect([dx, dy, right - left, bottom - top]),
                    paint: self.ghost_paint.clone(),
                },
            );
            self.ghost.setContentView(Some(&surface));
            self.ghost.orderFrontRegardless();
            self.ghost.displayIfNeeded();
            if let Some(geometry) = diagnostic.as_mut() {
                geometry.surface_visible = Some(self.ghost.isVisible());
                geometry.on_active_space = Some(self.ghost.isOnActiveSpace());
            }
            if !visible(&self.ghost) {
                return Err(GhostRejection::NotVisible);
            }
            Ok(drawn.to_owned())
        })();
        if let Some(geometry) = diagnostic {
            self.whatsapp_diagnostic
                .record(geometry, result.as_ref().err().copied());
        }
        match result {
            Ok(text) => text,
            Err(_) => {
                self.ghost_paint.clear();
                self.ghost.orderOut(None);
                String::new()
            }
        }
    }
    pub fn tick(&mut self) -> Result<Vec<Command>> {
        if self.closed {
            return Ok(Vec::new());
        }
        let mut commands = Vec::new();
        let environment = self.environment_signature();
        if environment != self.environment {
            publish_chooser_hit_region(None);
            self.environment = environment;
            self.ghost_paint.clear();
            self.chooser_paint.clear();
            self.ghost.orderOut(None);
            self.chooser.orderOut(None);
            self.accessory.orderOut(None);
        }
        // Revoke stale authority immediately when native visibility/Space is lost.
        // A subsequent same-content event retries rendering rather than caching refusal.
        if !visible(&self.ghost) {
            self.ghost_paint.clear();
            if let Some((_, text)) = self.ghost_state.as_mut() {
                if !text.is_empty() {
                    text.clear();
                    if let Some(event) = &self.ghost_event {
                        if let Some(c) = Self::ack(
                            event,
                            String::new(),
                            event["mode"].as_str().unwrap_or("completion"),
                        ) {
                            commands.push(c);
                        }
                    }
                }
            }
        }
        if !visible(&self.chooser) {
            self.chooser_paint.clear();
            self.target.revoke_choices();
            publish_chooser_hit_region(None);
            if let Some((_, text)) = self.chooser_state.as_mut() {
                if !text.is_empty() {
                    text.clear();
                    if let Some(event) = &self.chooser_event {
                        let mode = event["mode"].as_str().unwrap_or("completion");
                        let mode = if matches!(mode, "completion" | "correction") {
                            event["choices_mode"].as_str().unwrap_or(mode)
                        } else {
                            mode
                        };
                        if let Some(c) = Self::ack(event, String::new(), mode) {
                            commands.push(c);
                        }
                    }
                }
            }
        }
        publish_chooser_hit_region(self.choice_targets());
        for command in self.target.ivars().queue.take() {
            if let Command::Action(Action::Choice(index)) = &command {
                let buttons = self.target.ivars().buttons.borrow();
                let Some(button) = buttons.get(*index).and_then(Option::as_ref) else {
                    continue;
                };
                if !button.isEnabled()
                    || button.isHiddenOrHasHiddenAncestor()
                    || !contains_rect(button.visibleRect(), button.bounds())
                    || !button
                        .window()
                        .is_some_and(|window| window.windowNumber() == self.chooser.windowNumber())
                {
                    continue;
                }
            }
            commands.push(command);
        }
        Ok(commands)
    }
    pub fn close(&mut self) -> Result<()> {
        if !self.closed {
            self.clear(true, None);
            self.target.ivars().menu.take();
            self.target.ivars().queue.borrow_mut().clear();
            unsafe {
                self.button.setTarget(None);
                self.button.setAction(None);
            }
            self.ghost.setContentView(None);
            self.chooser.setContentView(None);
            self.accessory.setContentView(None);
            self.ghost.close();
            self.chooser.close();
            self.accessory.close();
            self.closed = true;
        }
        Ok(())
    }
}
impl Drop for OverlayOwner {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn paint_sink() -> (
        PaintReporter,
        std::sync::Arc<std::sync::Mutex<Vec<PaintObservation>>>,
    ) {
        let observations = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = observations.clone();
        let reporter: PaintReporter = std::sync::Arc::new(move |observation| {
            sink.lock().unwrap().push(observation);
        });
        (reporter, observations)
    }
    fn paint_id(trace_id: u64, lane: PresentationLane) -> PaintIdentity {
        PaintIdentity {
            trace_id,
            lane,
            view_serial: trace_id + 100,
        }
    }
    // These are damage-geometry and batch-bookkeeping regressions, not evidence
    // that AppKit delivered a draw callback or displayed pixels.
    #[test]
    fn damage_coverage_rejects_disconnected_strips_holes_and_partial_draws() {
        let region = rect([0., 0., 100., 20.]);
        let edges = [rect([0., 0., 10., 20.]), rect([90., 0., 10., 20.])];
        assert!(!damage_covers_region(&edges, region));
        let hole = [
            edges[0],
            edges[1],
            rect([10., 0., 80., 5.]),
            rect([10., 15., 80., 5.]),
        ];
        assert!(!damage_covers_region(&hole, region));
        assert!(!damage_covers_region(&[rect([0., 0., 99., 20.])], region));
        assert!(!damage_covers_region(&[], region));
        assert!(damage_covers_region(&[region], region));
        assert!(damage_covers_region(&[rect([-1., -1., 102., 22.])], region));
        // A union of individually partial rectangles is conservatively unknown.
        assert!(!damage_covers_region(
            &[rect([0., 0., 50., 20.]), rect([50., 0., 50., 20.])],
            region,
        ));
    }
    #[test]
    fn damage_coverage_rejects_invalid_and_unrepresentable_geometry() {
        let region = rect([0., 0., 100., 20.]);
        for invalid in [
            rect([f64::NAN, 0., 100., 20.]),
            rect([0., 0., f64::INFINITY, 20.]),
            rect([0., 0., -100., 20.]),
            rect([0., 0., 100., 0.]),
            rect([f64::MAX, 0., f64::MAX, 20.]),
            rect([f64::MAX, 0., 1., 20.]),
        ] {
            assert!(!damage_covers_region(&[invalid], region));
            assert!(!damage_covers_region(&[region, invalid], region));
            assert!(!damage_covers_region(&[region], invalid));
        }
    }
    #[test]
    fn damage_coverage_cannot_complete_stale_or_partially_drawn_chooser_rows() {
        let (reporter, observations) = paint_sink();
        let binding = PaintBinding::default();
        let region = rect([0., 0., 100., 20.]);
        binding.bind(
            Some(paint_id(1, PresentationLane::Chooser)),
            Some(&reporter),
            3,
        );
        let stale = binding.snapshot().unwrap();
        binding.bind(
            Some(paint_id(2, PresentationLane::Chooser)),
            Some(&reporter),
            3,
        );
        stale.did_draw(3, damage_covers_region(&[region], region));
        assert!(observations.lock().unwrap().is_empty());
        let current = binding.snapshot().unwrap();
        current.did_draw(1, damage_covers_region(&[region], region));
        current.did_draw(
            2,
            damage_covers_region(
                &[rect([0., 0., 10., 20.]), rect([90., 0., 10., 20.])],
                region,
            ),
        );
        assert!(observations.lock().unwrap().is_empty());
        current.did_draw(2, damage_covers_region(&[rect([0., 0., 50., 20.])], region));
        current.did_draw(
            2,
            damage_covers_region(&[rect([50., 0., 50., 20.])], region),
        );
        // Separate partial callbacks cannot accumulate into complete row evidence.
        assert!(observations.lock().unwrap().is_empty());
        current.did_draw(2, damage_covers_region(&[region], region));
        current.did_draw(2, damage_covers_region(&[region], region));
        let observations = observations.lock().unwrap();
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].trace_id, 2);
        assert_eq!(observations[0].lane, PresentationLane::Chooser);
        assert_eq!(observations[0].view_serial, 102);
    }
    #[test]
    fn delayed_native_observation_cannot_paint_a_replacement_in_diagnostic_book() {
        use crate::diagnostics::{CaptureTiming, DiagnosticBook, WorkKind};
        let mut book = DiagnosticBook::new();
        let capture = || {
            let now = Instant::now();
            CaptureTiming {
                input_notified: None,
                started: now,
                finished: now,
            }
        };
        let old = book
            .begin(WorkKind::Generated, PresentationLane::Inline, 1, capture())
            .unwrap();
        assert!(book.publish(old, PresentationLane::Inline, 1, Instant::now()));
        let (reporter, observations) = paint_sink();
        let binding = PaintBinding::default();
        binding.bind(
            Some(PaintIdentity {
                trace_id: old,
                lane: PresentationLane::Inline,
                view_serial: 1,
            }),
            Some(&reporter),
            1,
        );
        binding.snapshot().unwrap().did_draw(1, true);
        // Runtime advances before draining the already queued observation.
        let current = book
            .begin(WorkKind::Generated, PresentationLane::Inline, 2, capture())
            .unwrap();
        assert!(book.publish(current, PresentationLane::Inline, 2, Instant::now()));
        {
            let observations = observations.lock().unwrap();
            let observation = &observations[0];
            assert!(!book.record_draw(
                observation.trace_id,
                observation.lane,
                observation.view_serial,
                observation.painted_at
            ));
        }
        binding.bind(
            Some(PaintIdentity {
                trace_id: current,
                lane: PresentationLane::Inline,
                view_serial: 2,
            }),
            Some(&reporter),
            1,
        );
        assert!(
            book.snapshot()
                .traces
                .iter()
                .all(|trace| trace.first_draw_us.is_none())
        );
        binding.snapshot().unwrap().did_draw(1, true);
        let observations = observations.lock().unwrap();
        assert_eq!(observations.len(), 2);
        let observation = &observations[1];
        assert!(book.record_draw(
            observation.trace_id,
            observation.lane,
            observation.view_serial,
            observation.painted_at
        ));
        let snapshot = book.snapshot();
        assert_eq!(snapshot.rejected_draws, 1);
        assert!(snapshot.traces[0].first_draw_us.is_none());
        assert!(snapshot.traces[1].first_draw_us.is_some());
    }
    #[test]
    fn paint_requires_a_visible_draw_not_cached_acknowledgment() {
        let (reporter, observations) = paint_sink();
        let binding = PaintBinding::default();
        let identity = paint_id(1, PresentationLane::Inline);
        binding.bind(Some(identity), Some(&reporter), 1);
        // Binding/rebinding a cached offer is not a native paint callback.
        binding.bind(Some(identity), Some(&reporter), 1);
        assert!(observations.lock().unwrap().is_empty());
        let draw = binding.snapshot().unwrap();
        draw.did_draw(1, false);
        assert!(observations.lock().unwrap().is_empty());
        draw.did_draw(1, true);
        draw.did_draw(1, true);
        let observations = observations.lock().unwrap();
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].trace_id, 1);
        assert_eq!(observations[0].view_serial, 101);
        assert_eq!(observations[0].lane, PresentationLane::Inline);
    }
    #[test]
    fn replacement_and_hide_revoke_pending_draws_without_relabeling_queued_reports() {
        let (reporter, observations) = paint_sink();
        let binding = PaintBinding::default();
        binding.bind(
            Some(paint_id(1, PresentationLane::Inline)),
            Some(&reporter),
            1,
        );
        let old = binding.snapshot().unwrap();
        old.did_draw(1, true);
        binding.bind(
            Some(paint_id(2, PresentationLane::Inline)),
            Some(&reporter),
            1,
        );
        old.did_draw(1, true);
        let current = binding.snapshot().unwrap();
        binding.clear();
        current.did_draw(1, true);
        assert!(binding.snapshot().is_none());
        let observations = observations.lock().unwrap();
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].trace_id, 1);
        assert_eq!(observations[0].view_serial, 101);
    }
    #[test]
    fn chooser_waits_for_every_current_row_and_is_independent_of_inline() {
        let (reporter, observations) = paint_sink();
        let inline = PaintBinding::default();
        let chooser = PaintBinding::default();
        inline.bind(
            Some(paint_id(1, PresentationLane::Inline)),
            Some(&reporter),
            1,
        );
        chooser.bind(
            Some(paint_id(2, PresentationLane::Chooser)),
            Some(&reporter),
            7,
        );
        let stale = chooser.snapshot().unwrap();
        stale.did_draw(1, true);
        stale.did_draw(2, true);
        assert!(observations.lock().unwrap().is_empty());
        chooser.bind(
            Some(paint_id(3, PresentationLane::Chooser)),
            Some(&reporter),
            7,
        );
        stale.did_draw(4, true);
        assert!(observations.lock().unwrap().is_empty());
        let current = chooser.snapshot().unwrap();
        current.did_draw(4, true);
        current.did_draw(2, true);
        current.did_draw(2, true);
        assert!(observations.lock().unwrap().is_empty());
        inline.snapshot().unwrap().did_draw(1, true);
        inline.clear();
        current.did_draw(1, true);
        let observations = observations.lock().unwrap();
        assert_eq!(observations.len(), 2);
        assert_eq!(
            (observations[0].trace_id, observations[0].lane),
            (1, PresentationLane::Inline)
        );
        assert_eq!(
            (observations[1].trace_id, observations[1].lane),
            (3, PresentationLane::Chooser)
        );
    }
    #[test]
    fn diagnostic_ids_are_lane_specific_and_never_native_revisions() {
        let event = serde_json::json!({
            "mode":"correction", "revision":900,
            "diagnostic":{"trace_id":1,"lane":"inline","view_serial":101},
            "choices_diagnostic":{"trace_id":2,"lane":"chooser","view_serial":102}
        });
        assert_eq!(
            PaintIdentity::from_event(&event, PresentationLane::Inline),
            Some(paint_id(1, PresentationLane::Inline))
        );
        assert_eq!(
            PaintIdentity::from_event(&event, PresentationLane::Chooser),
            Some(paint_id(2, PresentationLane::Chooser))
        );
        assert!(
            PaintIdentity::from_event(
                &serde_json::json!({"revision":900}),
                PresentationLane::Inline
            )
            .is_none()
        );
        let standalone = serde_json::json!({
            "mode":"alternatives",
            "diagnostic":{"trace_id":3,"lane":"chooser","view_serial":103}
        });
        assert_eq!(
            PaintIdentity::from_event(&standalone, PresentationLane::Chooser),
            Some(paint_id(3, PresentationLane::Chooser))
        );
        assert!(PaintIdentity::from_event(&standalone, PresentationLane::Inline).is_none());
        let invalid = serde_json::json!({
            "diagnostic":{"trace_id":0,"lane":"inline","view_serial":103}
        });
        assert!(PaintIdentity::from_event(&invalid, PresentationLane::Inline).is_none());
    }
    #[test]
    fn numbered_clicks_use_the_same_zero_based_choices_as_keyboard_keys() {
        for number in 1..=9 {
            assert_eq!(
                choice_action(number, &[number as usize]),
                Some(Action::Choice((number - 1) as usize))
            );
        }
        // A sole emoticon result must select index zero, not a nonexistent second row.
        assert_eq!(choice_action(1, &[1]), Some(Action::Choice(0)));
        for tag in [-1, 0, 2, 10, 100, isize::MAX] {
            assert_eq!(choice_action(tag, &[1]), None);
        }
        assert_eq!(choice_action(1, &[]), None);
    }
    #[test]
    fn accessory_stays_two_points_above_the_caret_line() {
        let screen = rect([0., 0., 800., 600.]);
        let frame = accessory_frame([300., 200., 1., 18.], 600., 24., 24., screen).unwrap();
        assert_eq!(frame, rect([274., 402., 24., 24.]));
    }
    #[test]
    fn accessory_refuses_screen_edges_instead_of_covering_text() {
        let screen = rect([0., 0., 800., 600.]);
        let place = |caret| accessory_frame(caret, 600., 24., 24., screen);
        assert_eq!(place([30., 30., 0., 18.]), Some(rect([4., 572., 24., 24.])));
        assert!(place([29.9, 30., 0., 18.]).is_none());
        assert!(place([30., 29.9, 0., 18.]).is_none());
        assert!(place([798.1, 200., 0., 18.]).is_none());
        assert!(place([300., 598.1, 0., 1.]).is_none());
        // Negative-origin secondary screens use the same exact visible-frame gate.
        assert_eq!(
            accessory_frame(
                [-600., 200., 1., 18.],
                600.,
                24.,
                24.,
                rect([-800., 0., 800., 600.]),
            ),
            Some(rect([-626., 402., 24., 24.]))
        );
    }
    #[test]
    fn whole_word_clipping() {
        let measure = |s: &str| s.chars().count() as f64;
        assert_eq!(fit_text(" see you tomorrow", 9., measure), " see you");
        assert_eq!(fit_text(" see", 3., measure), "");
        assert_eq!(fit_text("  amanhã depois", 9., measure), "  amanhã");
        assert_eq!(fit_text("abc", f64::NAN, measure), "");
    }
    #[test]
    fn chooser_skips_overlong_rows_and_keeps_exact_dense_choices() {
        let rows = vec![
            "  amanhã  cedo ".into(),
            "W".repeat(100),
            " later 😀".into(),
        ];
        let shown = visible_choices(&rows, 40., |s| s.chars().count() as f64);
        assert_eq!(shown, ["  amanhã  cedo ", " later 😀"]);
        assert_eq!(shown.join("\n"), "  amanhã  cedo \n later 😀");
        assert_eq!(choice_action(2, &[1, 2]), Some(Action::Choice(1)));
        assert_eq!(choice_action(3, &[1, 2]), None);
    }
    #[test]
    fn chooser_measures_raw_whitespace_and_does_not_elide_fitting_rows() {
        let rows = vec![" ".repeat(40) + "later", "a".repeat(45), "last".into()];
        assert_eq!(
            visible_choices(&rows, 49., |s| s.chars().count() as f64),
            [rows[0].as_str(), rows[1].as_str(), "last"]
        );
        assert_eq!(
            visible_choices(&rows, 12., |s| s.chars().count() as f64),
            ["last"]
        );
        assert!(visible_choices(&rows, f64::NAN, |_| 1.).is_empty());
    }
    #[test]
    fn invalid_rectangles() {
        assert!(finite_rect(&serde_json::json!([1, 2, 0, 16])).is_some());
        for v in [
            serde_json::json!([1, 2, -1, 16]),
            serde_json::json!([1, 2, 1, 0]),
            serde_json::json!([1, 2, 3]),
            Value::Null,
        ] {
            assert!(finite_rect(&v).is_none());
        }
    }
    #[test]
    fn hidden_or_refused_content_retries() {
        let id = serde_json::json!(["text", 1]);
        let old = (id.clone(), "text".into());
        assert!(cached(&id, Some(&old), true));
        assert!(!cached(&id, Some(&old), false));
        assert!(!cached(&id, Some(&(id.clone(), String::new())), true));
    }
    #[test]
    fn acknowledgment_requires_all_source_authority() {
        let snapshot = crate::core::Snapshot::default();
        let event = serde_json::json!({"key":snapshot.key(),"authority":snapshot.authority(),"revision":17});
        assert!(SelfTest::ack(&event));
        let mut missing = event.clone();
        missing.as_object_mut().unwrap().remove("authority");
        assert!(!SelfTest::ack(&missing));
    }
    struct SelfTest;
    impl SelfTest {
        fn ack(v: &Value) -> bool {
            OverlayOwner::ack(v, " visible".into(), "completion").is_some()
        }
    }
}

#[cfg(test)]
mod diagnostic_tests {
    use super::*;

    #[test]
    fn renderer_diagnostic_requires_exact_whatsapp_suggestion_identity() {
        let mut event = serde_json::json!({
            "type":"suggestion","mode":"completion",
            "key":{"app_id":"net.whatsapp.WhatsApp"},
            "text":"PRIVATE_SENTINEL","render":{"suffix":"PRIVATE_SENTINEL"},
            "window_title":"PRIVATE_SENTINEL","domain":"PRIVATE_SENTINEL"
        });
        assert!(whatsapp_ghost_event(&event));
        for app in ["", "net.whatsapp.WhatsApp.helper", "owned.synthetic.inline"] {
            event["key"]["app_id"] = Value::from(app);
            assert!(!whatsapp_ghost_event(&event));
        }
        event["key"]["app_id"] = Value::from("net.whatsapp.WhatsApp");
        event["mode"] = Value::from("emoji");
        assert!(!whatsapp_ghost_event(&event));
        event["mode"] = Value::from("correction");
        assert!(whatsapp_ghost_event(&event));
        event["type"] = Value::from("caret");
        assert!(!whatsapp_ghost_event(&event));
    }

    #[test]
    fn renderer_diagnostic_reasons_are_bounded_and_accounted_without_event_storage() {
        let mut diagnostic = WhatsAppGhostDiagnostic::default();
        for (index, reason) in GhostRejection::ALL.into_iter().enumerate() {
            assert_eq!(reason as usize, index);
            diagnostic.record(GhostGeometry::default(), Some(reason));
        }
        assert_eq!(diagnostic.attempts, 26);
        assert_eq!(diagnostic.rejections, [1; 26]);
        assert_eq!(diagnostic.rendered, 0);
        diagnostic.record(
            GhostGeometry {
                suffix_present: true,
                requested_font: ".AppleSystemUIFont".into(),
                caret: Some([100.125, 200.875, 1., 18.]),
                ..Default::default()
            },
            None,
        );
        assert_eq!(diagnostic.rendered, 1);
        assert_eq!(diagnostic.last_rejection, None);
        let geometry = serde_json::to_value(diagnostic.last_geometry.as_ref().unwrap()).unwrap();
        for forbidden in [
            "text",
            "suffix",
            "suffix_width",
            "advance",
            "window_title",
            "domain",
            "key",
            "authority",
            "event",
            "field_id",
        ] {
            assert!(geometry.get(forbidden).is_none());
        }
        assert_eq!(geometry["suffix_present"], true);
        assert_eq!(
            geometry["caret"],
            serde_json::json!([100.125, 200.875, 1., 18.])
        );
        assert_eq!(font_diagnostic_name(&"A".repeat(1000)).len(), 128);
        assert_eq!(font_diagnostic_name("Font\nName\0"), "FontName");
        diagnostic.attempts = u64::MAX;
        diagnostic.rejections[GhostRejection::Caret as usize] = u64::MAX;
        diagnostic.record(GhostGeometry::default(), Some(GhostRejection::Caret));
        assert_eq!(diagnostic.attempts, u64::MAX);
        assert_eq!(
            diagnostic.rejections[GhostRejection::Caret as usize],
            u64::MAX
        );
    }
}
