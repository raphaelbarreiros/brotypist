//! Native power cards and bounded compositor cues; supplied sampler values only.
use super::macos_overlay::PassivePanel;
use crate::{power::LowBatteryHelpPolicy, system::PowerStatus};
use anyhow::{Result, anyhow};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::*;
use objc2_foundation::{
    MainThreadMarker, NSDictionary, NSLocale, NSNotification, NSNumber, NSObject, NSObjectProtocol,
    NSPoint, NSRect, NSSize, NSString,
};
use objc2_quartz_core::{CABasicAnimation, CAMediaTimingFunction};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PowerPresentation {
    pub state: String,
    pub title: String,
    pub detail: String,
    pub symbol: String,
}
pub fn power_presentation(reading: &PowerStatus) -> PowerPresentation {
    let percent = reading.percent.filter(|p| *p <= 100);
    let level = percent
        .map(|p| format!("{p}%"))
        .unwrap_or_else(|| "Battery level unavailable".into());
    let (state, title, detail, symbol) = if !reading.available {
        (
            "unknown",
            "Power status unavailable".into(),
            "Waiting for a verified power reading.",
            "questionmark.circle",
        )
    } else if !reading.has_battery {
        (
            "absent",
            "No battery".into(),
            "This Mac reports no battery. Battery-only options are inactive.",
            "powerplug",
        )
    } else if reading.on_battery {
        let low = percent.is_some_and(|p| p <= 20);
        (
            if low { "low" } else { "battery" },
            format!("{level} · On battery"),
            "Running on battery. Your battery preferences apply now.",
            if low {
                "battery.25percent"
            } else {
                "battery.75percent"
            },
        )
    } else if reading.charging == Some(true) {
        (
            "charging",
            format!("{level} · Charging"),
            "Plugged in and charging. A little energy for your next idea.",
            "bolt.batteryblock",
        )
    } else if reading.charging == Some(false) {
        let full = percent == Some(100);
        (
            if full { "full" } else { "external" },
            format!(
                "{level} · {}",
                if full { "Fully charged" } else { "Plugged in" }
            ),
            "Connected to power; not charging. Battery-only options are inactive.",
            if full {
                "battery.100percent"
            } else {
                "powerplug"
            },
        )
    } else {
        (
            "external_unknown",
            format!("{level} · External power"),
            "Connected to power; charging status is unavailable.",
            "powerplug",
        )
    };
    PowerPresentation {
        state: state.into(),
        title,
        detail: detail.into(),
        symbol: symbol.into(),
    }
}
pub fn charging_help_text(locale: &str) -> (&'static str, &'static str) {
    if locale.replace('_', "-").to_lowercase().starts_with("pt-br") {
        (
            "Me coloca na tomada?",
            "A bateria está baixinha. Uma carga ajuda a gente a continuar escrevendo.",
        )
    } else {
        (
            "Could you plug me in?",
            "The battery is getting low. A little charge will keep us writing.",
        )
    }
}

#[derive(Clone, Copy)]
enum Notice {
    Display,
    Resigned,
    Active,
    Dismiss,
}
struct ObserverIvars {
    queue: RefCell<Vec<Notice>>,
    active: Cell<bool>,
}
define_class!(
    #[unsafe(super=NSObject)]
    #[thread_kind=MainThreadOnly]
    #[ivars=ObserverIvars]
    struct PowerObserver;
    unsafe impl NSObjectProtocol for PowerObserver {}
    impl PowerObserver {
        #[unsafe(method(accessibilityChanged:))]fn display(&self,_notice:&NSNotification){self.ivars().queue.borrow_mut().push(Notice::Display);}
        #[unsafe(method(sessionResigned:))]fn resign(&self,_notice:&NSNotification){self.ivars().active.set(false);self.ivars().queue.borrow_mut().push(Notice::Resigned);}
        #[unsafe(method(sessionBecameActive:))]fn active(&self,_notice:&NSNotification){self.ivars().active.set(true);self.ivars().queue.borrow_mut().push(Notice::Active);}
        #[unsafe(method(dismissHelp:))]fn dismiss(&self,_sender:Option<&AnyObject>){self.ivars().queue.borrow_mut().push(Notice::Dismiss);}
    }
);
impl PowerObserver {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ObserverIvars {
            queue: RefCell::new(Vec::new()),
            active: Cell::new(true),
        });
        unsafe { msg_send![super(this), init] }
    }
}
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGSessionCopyCurrentDictionary() -> *mut NSDictionary<NSString, AnyObject>;
}
fn session_eligible() -> bool {
    let Some(session) = (unsafe { Retained::from_raw(CGSessionCopyCurrentDictionary()) }) else {
        return false;
    };
    let boolean = |key: &NSString| {
        session.objectForKey(key).is_some_and(|v| unsafe {
            let result: bool = msg_send![&*v, boolValue];
            result
        })
    };
    !boolean(objc2_foundation::ns_string!("CGSSessionScreenIsLocked"))
        && boolean(objc2_foundation::ns_string!("kCGSSessionOnConsoleKey"))
        && boolean(objc2_foundation::ns_string!("kCGSessionLoginDoneKey"))
}
fn frame(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}
fn label(mtm: MainThreadMarker, text: &str, r: NSRect, small: bool) -> Retained<NSTextField> {
    let view = NSTextField::wrappingLabelWithString(&NSString::from_str(text), mtm);
    view.setFrame(r);
    // AppKit exports these immutable font weights as C globals.
    let weight = unsafe {
        if small {
            NSFontWeightRegular
        } else {
            NSFontWeightSemibold
        }
    };
    view.setFont(Some(&NSFont::systemFontOfSize_weight(
        if small { 11. } else { 13. },
        weight,
    )));
    let color = if small {
        NSColor::secondaryLabelColor()
    } else {
        NSColor::labelColor()
    };
    view.setTextColor(Some(&*color));
    view
}
struct Card {
    box_view: Retained<NSBox>,
    icon: Retained<NSImageView>,
    title: Retained<NSTextField>,
    detail: Retained<NSTextField>,
}
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct DisplayPreferences {
    pub reduce_motion: bool,
    pub increase_contrast: bool,
    pub reduce_transparency: bool,
}
fn motion_allowed(flags: DisplayPreferences, closed: bool, session: bool) -> bool {
    !flags.reduce_motion && !flags.increase_contrast && !closed && session
}

pub struct PowerUi {
    mtm: MainThreadMarker,
    observer: Retained<PowerObserver>,
    workspace: Retained<NSWorkspace>,
    policy: LowBatteryHelpPolicy,
    help_anchor_button: Option<Retained<NSStatusBarButton>>,
    cards: BTreeMap<String, Card>,
    presentation: PowerPresentation,
    reading: PowerStatus,
    flags: DisplayPreferences,
    pane_visible: bool,
    help: Option<Retained<PassivePanel>>,
    help_deadline: Option<Instant>,
    help_showing: bool,
    arrival: Option<Retained<NSView>>,
    closed: bool,
}
impl PowerUi {
    pub fn new() -> Result<Self> {
        let mtm = MainThreadMarker::new()
            .ok_or_else(|| anyhow!("PowerUi requires the main AppKit thread"))?;
        let observer = PowerObserver::new(mtm);
        let workspace = NSWorkspace::sharedWorkspace();
        let center = workspace.notificationCenter();
        unsafe {
            center.addObserver_selector_name_object(
                &observer,
                sel!(accessibilityChanged:),
                Some(NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification),
                None,
            );
            center.addObserver_selector_name_object(
                &observer,
                sel!(sessionResigned:),
                Some(NSWorkspaceSessionDidResignActiveNotification),
                None,
            );
            center.addObserver_selector_name_object(
                &observer,
                sel!(sessionBecameActive:),
                Some(NSWorkspaceSessionDidBecomeActiveNotification),
                None,
            );
        }
        let reading = PowerStatus::default();
        let mut this = Self {
            mtm,
            observer,
            workspace,
            policy: LowBatteryHelpPolicy::new(),
            help_anchor_button: None,
            cards: BTreeMap::new(),
            presentation: power_presentation(&reading),
            reading,
            flags: DisplayPreferences::default(),
            pane_visible: false,
            help: None,
            help_deadline: None,
            help_showing: false,
            arrival: None,
            closed: false,
        };
        this.read_preferences();
        Ok(this)
    }
    pub fn set_help_anchor(&mut self, button: Option<&NSStatusBarButton>) {
        self.dismiss_help();
        self.help_anchor_button = button.map(|button| unsafe {
            Retained::retain(button as *const _ as *mut _).expect("borrowed help anchor")
        });
    }
    pub fn make_card(&mut self, name: &str) -> Retained<NSView> {
        self.release_card(name);
        let h = 84.;
        let box_view = NSBox::initWithFrame(NSBox::alloc(self.mtm), frame(0., 0., 300., h));
        box_view.setBoxType(NSBoxType::Custom);
        box_view.setContentViewMargins(NSSize::new(0., 0.));
        box_view.setCornerRadius(9.);
        unsafe {
            let _: () = msg_send![&*box_view,setAccessibilityIdentifier:&*NSString::from_str(&format!("inklow.power.{name}"))];
        }
        let icon = NSImageView::initWithFrame(
            NSImageView::alloc(self.mtm),
            frame(12., (h - 24.) / 2., 28., 24.),
        );
        icon.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
        icon.setWantsLayer(true);
        let title = label(self.mtm, "", frame(50., h - 29., 236., 20.), false);
        title.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
        let detail = label(self.mtm, "", frame(50., 9., 236., 36.), true);
        detail.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
        box_view.addSubview(&icon);
        box_view.addSubview(&title);
        box_view.addSubview(&detail);
        self.cards.insert(
            name.into(),
            Card {
                box_view: box_view.clone(),
                icon,
                title,
                detail,
            },
        );
        self.render_card(name);
        unsafe {
            Retained::retain(&*box_view as *const NSBox as *mut NSView).expect("retained card view")
        }
    }
    pub fn release_card(&mut self, name: &str) {
        if let Some(card) = self.cards.remove(name) {
            if let Some(layer) = card.icon.layer() {
                layer.removeAllAnimations();
            }
        }
    }
    fn style_box(&self, box_view: &NSBox) {
        box_view.setFillColor(&NSColor::controlBackgroundColor());
        let border = if self.flags.increase_contrast {
            NSColor::labelColor()
        } else {
            NSColor::separatorColor()
        };
        box_view.setBorderColor(&*border);
        box_view.setBorderWidth(if self.flags.increase_contrast { 2. } else { 1. });
    }
    fn render_card(&self, name: &str) {
        let Some(card) = self.cards.get(name) else {
            return;
        };
        let p = &self.presentation;
        self.style_box(&card.box_view);
        card.title.setStringValue(&NSString::from_str(&p.title));
        card.detail.setStringValue(&NSString::from_str(&p.detail));
        unsafe {
            let _: () = msg_send![&*card.box_view,setAccessibilityLabel:&*NSString::from_str(&format!("{}. {}",p.title,p.detail))];
        }
        card.icon.setImage(
            NSImage::imageWithSystemSymbolName_accessibilityDescription(
                &NSString::from_str(&p.symbol),
                Some(&NSString::from_str(&p.title)),
            )
            .as_deref(),
        );
        let tint = if p.state == "low" {
            NSColor::systemOrangeColor()
        } else if p.state == "charging" && !self.flags.increase_contrast {
            NSColor::systemGreenColor()
        } else {
            NSColor::labelColor()
        };
        card.icon.setContentTintColor(Some(&*tint));
    }
    fn read_preferences(&mut self) {
        self.apply_display_preferences(DisplayPreferences {
            reduce_motion: self.workspace.accessibilityDisplayShouldReduceMotion(),
            increase_contrast: self.workspace.accessibilityDisplayShouldIncreaseContrast(),
            reduce_transparency: self
                .workspace
                .accessibilityDisplayShouldReduceTransparency(),
        });
    }
    pub fn apply_display_preferences(&mut self, flags: DisplayPreferences) {
        self.flags = flags;
        self.stop_motion();
        for name in self.cards.keys() {
            self.render_card(name);
        }
        if let Some(panel) = &self.help {
            if let Some(view) = panel.contentView() {
                unsafe {
                    let _: () = msg_send![&*view,setFillColor:&*NSColor::controlBackgroundColor()];
                    let _: () = msg_send![&*view,setBorderColor:&*if flags.increase_contrast{NSColor::labelColor()}else{NSColor::separatorColor()}];
                    let border = if flags.increase_contrast { 2.0f64 } else { 1. };
                    let _: () = msg_send![&*view,setBorderWidth:border];
                }
            }
        }
    }
    fn charge_motion(&self, view: &NSView) {
        if !motion_allowed(self.flags, self.closed, self.observer.ivars().active.get()) {
            return;
        }
        view.setWantsLayer(true);
        let Some(layer) = view.layer() else {
            return;
        };
        let animation =
            CABasicAnimation::animationWithKeyPath(Some(&NSString::from_str("opacity")));
        unsafe {
            animation.setFromValue(Some(&NSNumber::new_f64(1.)));
            animation.setToValue(Some(&NSNumber::new_f64(0.45)));
            let _: () = msg_send![&*animation,setDuration:0.6f64];
            let _: () = msg_send![&*animation,setAutoreverses:true];
            let _: () = msg_send![&*animation,setRepeatCount:3.0f32];
        }
        animation.setTimingFunction(Some(&CAMediaTimingFunction::functionWithName(unsafe {
            objc2_quartz_core::kCAMediaTimingFunctionEaseInEaseOut
        })));
        layer.addAnimation_forKey(&animation, Some(&NSString::from_str("inklow.charge")));
    }
    pub fn set_pane_visible(&mut self, visible: bool) {
        self.pane_visible = visible;
        if !visible {
            if let Some(view) = &self.arrival {
                if let Some(layer) = view.layer() {
                    layer.removeAnimationForKey(&NSString::from_str("inklow.arrival"));
                }
            }
        }
        for card in self.cards.values() {
            if !visible {
                if let Some(layer) = card.icon.layer() {
                    layer.removeAllAnimations();
                }
            } else if self.presentation.state == "charging" {
                self.charge_motion(&card.icon);
            }
        }
    }
    pub fn present(&mut self, reading: &PowerStatus, allow_help: bool) -> PowerPresentation {
        self.drain_notices();
        if self.closed {
            return self.presentation.clone();
        }
        let previous = self.presentation.clone();
        self.reading = reading.clone();
        self.presentation = power_presentation(reading);
        if previous != self.presentation {
            self.stop_motion();
            for name in self.cards.keys() {
                self.render_card(name);
            }
        }
        if previous != self.presentation
            && self.presentation.state == "charging"
            && previous.state != "charging"
        {
            for card in self.cards.values() {
                if self.pane_visible {
                    self.charge_motion(&card.icon);
                }
            }
        }
        if !matches!(self.presentation.state.as_str(), "low" | "battery") {
            self.dismiss_help();
        }
        let eligible = allow_help && self.help_anchor().is_some();
        let discharging = reading.available && reading.has_battery && reading.on_battery;
        // No anchor must not consume the discharge latch. Genuine known AC is
        // independently sufficient to rearm the shared policy, even without %.
        if (eligible || !discharging) && self.policy.should_ask(reading) {
            self.show_help();
        }
        self.presentation.clone()
    }
    fn help_anchor(&self) -> Option<(NSRect, Retained<NSScreen>)> {
        if self.closed || !self.observer.ivars().active.get() || !session_eligible() {
            return None;
        }
        let button = self.help_anchor_button.as_ref()?;
        let window = button.window()?;
        if !window.isVisible() {
            return None;
        }
        let screen = window.screen()?;
        let r = window.convertRectToScreen(button.convertRect_toView(button.bounds(), None));
        if ![r.origin.x, r.origin.y, r.size.width, r.size.height]
            .iter()
            .all(|v| v.is_finite())
            || r.size.width <= 0.
            || r.size.height <= 0.
        {
            return None;
        }
        Some((r, screen))
    }
    fn show_help(&mut self) {
        let Some((anchor, screen)) = self.help_anchor() else {
            return;
        };
        if self.help.is_none() {
            let panel = PassivePanel::create(
                self.mtm,
                "inklow.power.help",
                "Charging help",
                NSStatusWindowLevel,
                false,
            );
            panel.setOpaque(true);
            panel.setBackgroundColor(Some(&NSColor::controlBackgroundColor()));
            panel.setFrame_display(frame(0., 0., 306., 112.), false);
            let box_view = NSBox::initWithFrame(NSBox::alloc(self.mtm), frame(0., 0., 306., 112.));
            box_view.setBoxType(NSBoxType::Custom);
            box_view.setContentViewMargins(NSSize::new(0., 0.));
            box_view.setCornerRadius(10.);
            self.style_box(&box_view);
            let locale = NSLocale::currentLocale().localeIdentifier().to_string();
            let (title, detail) = charging_help_text(&locale);
            box_view.addSubview(&label(self.mtm, title, frame(16., 75., 256., 20.), false));
            box_view.addSubview(&label(self.mtm, detail, frame(16., 23., 268., 44.), true));
            let button = unsafe {
                NSButton::buttonWithTitle_target_action(
                    &NSString::from_str("Dismiss"),
                    Some(&self.observer),
                    Some(sel!(dismissHelp:)),
                    self.mtm,
                )
            };
            button.setImage(
                NSImage::imageWithSystemSymbolName_accessibilityDescription(
                    &NSString::from_str("xmark.circle.fill"),
                    Some(&NSString::from_str("Dismiss charging help")),
                )
                .as_deref(),
            );
            button.setImagePosition(NSCellImagePosition::ImageOnly);
            button.setBordered(false);
            button.setFrame(frame(275., 78., 24., 24.));
            button.setToolTip(Some(&NSString::from_str("Dismiss charging help")));
            unsafe {
                let _: () = msg_send![&*button,setAccessibilityLabel:&*NSString::from_str("Dismiss charging help")];
            }
            box_view.addSubview(&button);
            panel.setContentView(Some(&box_view));
            self.help = Some(panel);
        }
        let v = screen.visibleFrame();
        let x = (anchor.origin.x + anchor.size.width / 2. - 153.)
            .max(v.origin.x + 8.)
            .min(v.origin.x + v.size.width - 314.);
        let y = (anchor.origin.y - 120.).max(v.origin.y + 8.);
        let panel = self.help.as_ref().unwrap();
        panel.setFrameOrigin(NSPoint::new(x, y));
        panel.orderFrontRegardless();
        if panel.isVisible() && panel.isOnActiveSpace() {
            self.help_showing = true;
            self.help_deadline = Some(Instant::now() + Duration::from_secs(12));
            let view = panel.contentView();
            if let Some(view) = view {
                self.reveal(&view);
            }
        }
    }
    pub fn help_showing(&self) -> bool {
        self.help_showing
    }
    pub fn hide_help_surface(&mut self) {
        if let Some(panel) = &self.help {
            if let Some(view) = panel.contentView() {
                if let Some(layer) = view.layer() {
                    layer.removeAllAnimations();
                }
            }
            panel.orderOut(None);
        }
    }
    pub fn dismiss_help(&mut self) {
        self.hide_help_surface();
        self.help_showing = false;
        self.help_deadline = None;
    }
    pub fn reveal(&mut self, view: &NSView) {
        if self.closed || !self.observer.ivars().active.get() {
            return;
        }
        let mut start = 0.;
        if let Some(previous) = &self.arrival {
            if std::ptr::eq(&**previous, view) {
                if let Some(layer) = view.layer() {
                    if let Some(live) = unsafe { layer.presentationLayer() } {
                        start = live.opacity() as f64;
                    }
                }
            }
            if let Some(layer) = previous.layer() {
                layer.removeAnimationForKey(&NSString::from_str("inklow.arrival"));
            }
        }
        self.arrival = unsafe { Retained::retain(view as *const _ as *mut _) };
        view.setWantsLayer(true);
        let Some(layer) = view.layer() else {
            return;
        };
        let animation =
            CABasicAnimation::animationWithKeyPath(Some(&NSString::from_str("opacity")));
        unsafe {
            animation.setFromValue(Some(&NSNumber::new_f64(start)));
            animation.setToValue(Some(&NSNumber::new_f64(1.)));
            let duration = if self.flags.reduce_motion {
                0.12f64
            } else {
                0.18
            };
            let _: () = msg_send![&*animation,setDuration:duration];
        }
        animation.setTimingFunction(Some(&CAMediaTimingFunction::functionWithControlPoints(
            0.23, 1., 0.32, 1.,
        )));
        layer.addAnimation_forKey(&animation, Some(&NSString::from_str("inklow.arrival")));
    }
    pub fn stop_motion(&self) {
        for card in self.cards.values() {
            if let Some(layer) = card.icon.layer() {
                layer.removeAllAnimations();
            }
        }
        if let Some(panel) = &self.help {
            if let Some(view) = panel.contentView() {
                if let Some(layer) = view.layer() {
                    layer.removeAllAnimations();
                }
            }
        }
        if let Some(view) = &self.arrival {
            if let Some(layer) = view.layer() {
                layer.removeAnimationForKey(&NSString::from_str("inklow.arrival"));
            }
        }
    }
    fn drain_notices(&mut self) {
        let notices = self.observer.ivars().queue.take();
        for notice in notices {
            match notice {
                Notice::Display => self.read_preferences(),
                Notice::Resigned => {
                    self.dismiss_help();
                    self.stop_motion();
                }
                Notice::Active => {}
                Notice::Dismiss => self.dismiss_help(),
            }
        }
    }
    pub fn tick(&mut self) {
        self.drain_notices();
        if self.help_showing
            && (!self.observer.ivars().active.get()
                || !session_eligible()
                || self
                    .help_deadline
                    .is_some_and(|deadline| Instant::now() >= deadline))
        {
            self.dismiss_help();
            self.stop_motion();
        }
    }
    pub fn close(&mut self) -> Result<()> {
        if !self.closed {
            self.closed = true;
            self.dismiss_help();
            self.stop_motion();
            unsafe {
                self.workspace
                    .notificationCenter()
                    .removeObserver(&self.observer);
            }
            self.cards.clear();
            self.help_anchor_button = None;
            self.arrival = None;
            if let Some(panel) = self.help.take() {
                panel.close();
            }
            self.observer.ivars().queue.borrow_mut().clear();
        }
        Ok(())
    }
}
impl Drop for PowerUi {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn reading(percent: Option<u32>) -> PowerStatus {
        PowerStatus {
            available: true,
            has_battery: true,
            on_battery: true,
            charging: Some(false),
            percent,
        }
    }
    #[test]
    fn truthful_battery_states() {
        for (p, state) in [
            (Some(0), "low"),
            (Some(20), "low"),
            (Some(21), "battery"),
            (None, "battery"),
            (Some(101), "battery"),
        ] {
            let presentation = power_presentation(&reading(p));
            assert_eq!(presentation.state, state);
            if p.is_none() || p == Some(101) {
                assert!(!presentation.title.contains('%'));
            }
        }
        assert_eq!(power_presentation(&PowerStatus::default()).state, "unknown");
        assert_eq!(
            power_presentation(&PowerStatus {
                on_battery: false,
                charging: None,
                ..reading(Some(80))
            })
            .state,
            "external_unknown"
        );
    }
    #[test]
    fn charging_transition_is_distinct() {
        let low = reading(Some(19));
        let charging = PowerStatus {
            on_battery: false,
            charging: Some(true),
            ..low.clone()
        };
        assert_ne!(power_presentation(&low), power_presentation(&charging));
        assert_eq!(power_presentation(&charging).state, "charging");
        assert_eq!(
            power_presentation(&PowerStatus {
                on_battery: false,
                percent: Some(100),
                ..low
            })
            .state,
            "full"
        );
    }
    #[test]
    fn unknown_does_not_consume_or_rearm_policy() {
        let mut policy = LowBatteryHelpPolicy::new();
        assert!(!policy.should_ask(&reading(None)));
        assert!(policy.should_ask(&reading(Some(20))));
        assert!(!policy.should_ask(&PowerStatus::default()));
        assert!(!policy.should_ask(&reading(Some(19))));
        assert!(!policy.should_ask(&PowerStatus {
            on_battery: false,
            charging: None,
            percent: None,
            ..reading(Some(19))
        }));
        assert!(policy.should_ask(&reading(Some(19))));
    }
    #[test]
    fn bounded_motion_guards() {
        let flags = DisplayPreferences::default();
        assert!(motion_allowed(flags, false, true));
        assert!(!motion_allowed(flags, true, true));
        assert!(!motion_allowed(flags, false, false));
        assert!(!motion_allowed(
            DisplayPreferences {
                reduce_motion: true,
                ..flags
            },
            false,
            true
        ));
        assert!(!motion_allowed(
            DisplayPreferences {
                increase_contrast: true,
                ..flags
            },
            false,
            true
        ));
        assert!(0.6 * 3. * 2. <= 5.);
    }
    #[test]
    fn localized_help() {
        assert_eq!(charging_help_text("pt_BR").0, "Me coloca na tomada?");
        assert_eq!(charging_help_text("pt-PT").0, "Could you plug me in?");
    }
}
