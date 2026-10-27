"""Native, nonactivating power surfaces; readings are supplied by the shared sampler.

``NativePowerUI.present(reading, allow_help=False)`` is also the small synthetic
visual-proof interface: it changes only this UI, never the OS battery source.
"""
from __future__ import annotations

from dataclasses import dataclass
import time

import AppKit as A
import Quartz
import objc
from Foundation import NSLocale, NSObject

from .power import LowBatteryHelpPolicy


@dataclass(frozen=True)
class PowerPresentation:
    state: str
    title: str
    detail: str
    compact: str
    symbol: str


def power_presentation(reading: dict) -> PowerPresentation:
    """Keep unknown, absent, discharging, charging and externally powered distinct."""
    percent = reading.get("percent")
    percent = percent if type(percent) is int and 0 <= percent <= 100 else None
    level = f"{percent}%" if percent is not None else "Battery level unavailable"
    if not reading.get("available"):
        return PowerPresentation("unknown", "Power status unavailable", "Waiting for a verified power reading.", "?", "questionmark.circle")
    if not reading.get("has_battery"):
        return PowerPresentation("absent", "No battery", "This Mac reports no battery. Battery-only options are inactive.", "AC", "powerplug")
    if reading.get("on_battery"):
        detail = "Running on battery. Your battery preferences apply now."
        state = "low" if percent is not None and percent <= 20 else "battery"
        symbol = "battery.25percent" if state == "low" else "battery.75percent"
        return PowerPresentation(state, level + " · On battery", detail, f"{percent}% ↓" if percent is not None else "? ↓", symbol)
    if reading.get("charging") is True:
        return PowerPresentation("charging", level + " · Charging", "Plugged in and charging. A little energy for your next idea.", f"{percent}% ⚡" if percent is not None else "⚡", "bolt.batteryblock")
    if reading.get("charging") is False:
        full = percent == 100
        return PowerPresentation("full" if full else "external", level + (" · Fully charged" if full else " · Plugged in"), "Connected to power; not charging. Battery-only options are inactive.", "100%" if full else (f"{percent}% · AC" if percent is not None else "AC"), "battery.100percent" if full else "powerplug")
    return PowerPresentation("external_unknown", level + " · External power", "Connected to power; charging status is unavailable.", f"{percent}% · AC" if percent is not None else "AC", "powerplug")


def charging_help_text(locale: str) -> tuple[str, str]:
    if str(locale).replace("_", "-").lower().startswith("pt-br"):
        return "Me coloca na tomada?", "A bateria está baixinha. Uma carga ajuda a gente a continuar escrevendo."
    return "Could you plug me in?", "The battery is getting low. A little charge will keep us writing."


def _label(text, frame, *, small=False):
    label = A.NSTextField.wrappingLabelWithString_(text)
    label.setFrame_(frame)
    label.setFont_(A.NSFont.systemFontOfSize_weight_(11 if small else 13, A.NSFontWeightRegular if small else A.NSFontWeightSemibold))
    label.setTextColor_(A.NSColor.secondaryLabelColor() if small else A.NSColor.labelColor())
    return label


class PowerHelpPanel(A.NSPanel):
    def canBecomeKeyWindow(self):
        return False

    def canBecomeMainWindow(self):
        return False


class NativePowerUI(NSObject):
    @objc.python_method
    def configure(self, status_item=None):
        self.status_item = status_item
        self.policy = LowBatteryHelpPolicy()
        self.reading = {"available": False, "has_battery": False, "on_battery": False, "charging": None, "percent": None}
        self.presentation = power_presentation(self.reading)
        self.cards = {}
        self.arrival_view = None
        self.help_panel = None
        self.help_deadline = 0.0
        self.help_showing = False
        self.session_active = True
        self.closed = False
        self.menu_visible = False
        self.pane_visible = False
        self.workspace = A.NSWorkspace.sharedWorkspace()
        center = self.workspace.notificationCenter()
        center.addObserver_selector_name_object_(self, "accessibilityChanged:", A.NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification, None)
        center.addObserver_selector_name_object_(self, "sessionResigned:", A.NSWorkspaceSessionDidResignActiveNotification, None)
        center.addObserver_selector_name_object_(self, "sessionBecameActive:", A.NSWorkspaceSessionDidBecomeActiveNotification, None)
        self._read_accessibility()
        return self

    @objc.python_method
    def _read_accessibility(self):
        self.apply_display_preferences(
            reduce_motion=self.workspace.accessibilityDisplayShouldReduceMotion(),
            increase_contrast=self.workspace.accessibilityDisplayShouldIncreaseContrast(),
            reduce_transparency=self.workspace.accessibilityDisplayShouldReduceTransparency())

    @objc.python_method
    def apply_display_preferences(self, *, reduce_motion, increase_contrast, reduce_transparency):
        """Apply native display signals; also supports isolated synthetic UI proof."""
        self.reduce_motion = bool(reduce_motion)
        self.increase_contrast = bool(increase_contrast)
        self.reduce_transparency = bool(reduce_transparency)
        self.stop_motion()
        for name in self.cards:
            self._render_card(name)
        if self.help_panel:
            self._style_box(self.help_panel.contentView())

    def accessibilityChanged_(self, notification):
        self._read_accessibility()

    def sessionResigned_(self, notification):
        self.session_active = False
        self.dismiss_help()
        self.stop_motion()

    def sessionBecameActive_(self, notification):
        self.session_active = True

    @objc.python_method
    def _style_box(self, box):
        # Solid system materials work in light/dark mode and Reduce Transparency.
        box.setFillColor_(A.NSColor.controlBackgroundColor())
        box.setBorderColor_(A.NSColor.labelColor() if self.increase_contrast else A.NSColor.separatorColor())
        box.setBorderWidth_(2 if self.increase_contrast else 1)

    @objc.python_method
    def make_card(self, name="pane", *, compact=False):
        self.release_card(name)
        height = 46 if compact else 84
        box = A.NSBox.alloc().initWithFrame_(A.NSMakeRect(0, 0, 300, height))
        box.setBoxType_(A.NSBoxCustom)
        box.setContentViewMargins_(A.NSMakeSize(0, 0))
        box.setCornerRadius_(9)
        box.setAccessibilityIdentifier_(f"inklow.power.{name}")
        icon = A.NSImageView.alloc().initWithFrame_(A.NSMakeRect(12, (height - 24) / 2, 28, 24))
        icon.setImageScaling_(A.NSImageScaleProportionallyUpOrDown)
        icon.setWantsLayer_(True)
        title = _label("", A.NSMakeRect(50, height - 29, 236, 20))
        title.setAutoresizingMask_(A.NSViewWidthSizable)
        detail = _label("", A.NSMakeRect(50, 9, 236, 36), small=True)
        detail.setAutoresizingMask_(A.NSViewWidthSizable)
        detail.setHidden_(compact)
        for view in (icon, title, detail):
            box.addSubview_(view)
        self.cards[name] = (box, icon, title, detail)
        self._render_card(name)
        return box

    @objc.python_method
    def release_card(self, name):
        card = self.cards.pop(name, None)
        if card:
            card[1].layer().removeAllAnimations()

    @objc.python_method
    def _render_card(self, name):
        box, icon, title, detail = self.cards[name]
        p = self.presentation
        self._style_box(box)
        title.setStringValue_(p.title)
        detail.setStringValue_(p.detail)
        box.setAccessibilityLabel_(p.title + ". " + p.detail)
        icon.setImage_(A.NSImage.imageWithSystemSymbolName_accessibilityDescription_(p.symbol, p.title))
        color = A.NSColor.systemOrangeColor() if p.state == "low" else A.NSColor.labelColor()
        if p.state == "charging" and not self.increase_contrast:
            color = A.NSColor.systemGreenColor()
        icon.setContentTintColor_(color)

    @objc.python_method
    def _charge_motion(self, view):
        if self.reduce_motion or self.increase_contrast or self.closed or not self.session_active:
            return
        view.setWantsLayer_(True)
        pulse = Quartz.CABasicAnimation.animationWithKeyPath_("opacity")
        pulse.setFromValue_(1.0)
        pulse.setToValue_(0.45)
        pulse.setDuration_(0.6)
        pulse.setAutoreverses_(True)
        pulse.setRepeatCount_(3)
        pulse.setTimingFunction_(Quartz.CAMediaTimingFunction.functionWithName_("easeInEaseOut"))
        view.layer().addAnimation_forKey_(pulse, "inklow.charge")

    @objc.python_method
    def set_visibility(self, *, menu=None, pane=None):
        if menu is not None:
            self.menu_visible = bool(menu)
        if pane is not None:
            self.pane_visible = bool(pane)
            if not pane and self.arrival_view is not None:
                self.arrival_view.layer().removeAnimationForKey_("inklow.arrival")
        for name, card in self.cards.items():
            visible = self.menu_visible if name == "menu" else self.pane_visible
            if not visible:
                card[1].layer().removeAllAnimations()
            elif self.presentation.state == "charging":
                self._charge_motion(card[1])

    @objc.python_method
    def present(self, reading: dict, *, allow_help=False):
        """Render a supplied real or synthetic sample; no hardware access or focus changes."""
        if self.closed:
            return self.presentation
        previous = self.presentation
        self.reading = dict(reading)
        self.presentation = power_presentation(self.reading)
        if previous != self.presentation:
            self.stop_motion()
            for name in self.cards:
                self._render_card(name)
        if self.status_item:
            button = self.status_item.button()
            button.setTitle_(self.presentation.compact)
            button.setImagePosition_(A.NSImageLeft)
        if previous != self.presentation and self.presentation.state == "charging" and previous.state != "charging":
            if self.status_item and self.status_item.isVisible():
                self._charge_motion(self.status_item.button())
            for name, card in self.cards.items():
                if (self.menu_visible if name == "menu" else self.pane_visible):
                    self._charge_motion(card[1])
        if self.presentation.state not in {"low", "battery"}:
            self.dismiss_help()
        # Feed every reading to the policy so genuine external power rearms it.
        # When no resident anchor/session is available, defer consuming a low ask.
        eligible_surface = bool(allow_help and self.status_item and self.status_item.isVisible() and self.session_active)
        if eligible_surface:
            eligible_surface = self._help_anchor() is not None
        discharging = bool(reading.get("available") and reading.get("has_battery") and reading.get("on_battery"))
        if (eligible_surface or not discharging) and self.policy.should_ask(reading):
            self._show_help()
        return self.presentation

    @objc.python_method
    def _help_anchor(self):
        session = Quartz.CGSessionCopyCurrentDictionary()
        if (not session or session.get("CGSSessionScreenIsLocked")
                or not session.get(Quartz.kCGSessionOnConsoleKey, False)
                or not session.get(Quartz.kCGSessionLoginDoneKey, False)):
            return None
        if not self.status_item:
            return None
        button = self.status_item.button()
        window = button.window()
        if window is None or window.screen() is None:
            return None
        return window.convertRectToScreen_(button.convertRect_toView_(button.bounds(), None)), window.screen()

    @objc.python_method
    def _show_help(self):
        # A menu-bar anchor, never the text caret; nonactivating and no Space move.
        target = self._help_anchor()
        if target is None:
            return
        anchor, screen = target
        if self.help_panel is None:
            style = A.NSWindowStyleMaskBorderless | A.NSWindowStyleMaskNonactivatingPanel
            panel = PowerHelpPanel.alloc().initWithContentRect_styleMask_backing_defer_(A.NSMakeRect(0, 0, 306, 112), style, A.NSBackingStoreBuffered, False)
            panel.setReleasedWhenClosed_(False)
            panel.setHidesOnDeactivate_(False)
            panel.setLevel_(A.NSStatusWindowLevel)
            panel.setCollectionBehavior_(A.NSWindowCollectionBehaviorTransient | A.NSWindowCollectionBehaviorIgnoresCycle | A.NSWindowCollectionBehaviorFullScreenAuxiliary)
            panel.setOpaque_(True)
            panel.setBackgroundColor_(A.NSColor.controlBackgroundColor())
            panel.setHasShadow_(True)
            panel.setAccessibilityIdentifier_("inklow.power.help")
            box = A.NSBox.alloc().initWithFrame_(A.NSMakeRect(0, 0, 306, 112))
            box.setBoxType_(A.NSBoxCustom)
            box.setContentViewMargins_(A.NSMakeSize(0, 0))
            box.setCornerRadius_(10)
            self._style_box(box)
            locale = str(NSLocale.currentLocale().localeIdentifier())
            title, detail = charging_help_text(locale)
            box.addSubview_(_label(title, A.NSMakeRect(16, 75, 256, 20)))
            box.addSubview_(_label(detail, A.NSMakeRect(16, 23, 268, 44), small=True))
            close = A.NSButton.buttonWithTitle_target_action_("Dismiss", self, "dismissHelp:")
            close.setImage_(A.NSImage.imageWithSystemSymbolName_accessibilityDescription_("xmark.circle.fill", "Dismiss charging help"))
            close.setImagePosition_(A.NSImageOnly)
            close.setBordered_(False)
            close.setFrame_(A.NSMakeRect(275, 78, 24, 24))
            close.setAccessibilityLabel_("Dismiss charging help")
            close.setToolTip_("Dismiss charging help")
            box.addSubview_(close)
            panel.setContentView_(box)
            self.help_panel = panel
        visible = screen.visibleFrame()
        x = min(max(A.NSMidX(anchor) - 153, A.NSMinX(visible) + 8), A.NSMaxX(visible) - 314)
        y = max(A.NSMinY(visible) + 8, A.NSMinY(anchor) - 120)
        self.help_panel.setFrameOrigin_(A.NSMakePoint(x, y))
        self.help_panel.orderFrontRegardless()
        self.help_showing = True
        self.help_deadline = time.monotonic() + 12
        self.reveal(self.help_panel.contentView())

    def dismissHelp_(self, sender):
        self.dismiss_help()

    @objc.python_method
    def hide_help_surface(self):
        """Native menu tracking replaces the bubble with its keyboard action."""
        if self.help_panel:
            if self.help_panel.contentView().layer():
                self.help_panel.contentView().layer().removeAllAnimations()
            self.help_panel.orderOut_(None)

    @objc.python_method
    def dismiss_help(self):
        if self.help_panel:
            if self.help_panel.contentView().layer():
                self.help_panel.contentView().layer().removeAllAnimations()
            self.help_panel.orderOut_(None)
        self.help_showing = False
        self.help_deadline = 0.0

    @objc.python_method
    def tick(self, now=None):
        if self.help_showing and (time.monotonic() if now is None else now) >= self.help_deadline:
            self.dismiss_help()

    @objc.python_method
    def reveal(self, view):
        """Occasional state/pane arrival: compositor opacity, never typing feedback."""
        if self.closed or not self.session_active:
            return
        start = 0.0
        if self.arrival_view is view and view.layer() is not None:
            live = view.layer().presentationLayer()
            if live is not None:
                start = float(live.opacity())
        if self.arrival_view is not None:
            self.arrival_view.layer().removeAnimationForKey_("inklow.arrival")
        self.arrival_view = view
        view.setWantsLayer_(True)
        fade = Quartz.CABasicAnimation.animationWithKeyPath_("opacity")
        fade.setFromValue_(start)
        fade.setToValue_(1.0)
        fade.setDuration_(0.12 if self.reduce_motion else 0.18)
        fade.setTimingFunction_(Quartz.CAMediaTimingFunction.functionWithControlPoints____(0.23, 1.0, 0.32, 1.0))
        view.layer().addAnimation_forKey_(fade, "inklow.arrival")

    @objc.python_method
    def stop_motion(self):
        if self.status_item and self.status_item.button().layer():
            self.status_item.button().layer().removeAnimationForKey_("inklow.charge")
        for card in self.cards.values():
            card[1].layer().removeAllAnimations()
        if self.help_panel and self.help_panel.contentView().layer():
            self.help_panel.contentView().layer().removeAllAnimations()
        if self.arrival_view is not None:
            self.arrival_view.layer().removeAnimationForKey_("inklow.arrival")

    @objc.python_method
    def close(self):
        if self.closed:
            return
        self.closed = True
        self.dismiss_help()
        self.stop_motion()
        self.workspace.notificationCenter().removeObserver_(self)
        self.cards.clear()
        self.status_item = None
        self.arrival_view = None
