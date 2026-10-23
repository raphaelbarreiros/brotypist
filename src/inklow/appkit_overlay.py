"""Nonactivating caret UI. Only the main AppKit run loop touches these views."""
from __future__ import annotations

import math
from typing import Callable

import AppKit as A
import objc
from Foundation import NSObject, NSAttributedString, NSDictionary

from .resources import asset_path


class InklowSuggestionPanel(A.NSPanel):
    def canBecomeKeyWindow(self):
        return False

    def canBecomeMainWindow(self):
        return False


class InklowOverlayButton(A.NSButton):
    def acceptsFirstMouse_(self, event):
        return True

    def acceptsFirstResponder(self):
        return False


class OverlayActions(NSObject):
    @objc.python_method
    def setup(self, invoke, show_menu):
        self.invoke = invoke
        self.show_menu = show_menu
        self.actions = {}
        return self

    def pressed_(self, sender):
        action = self.actions.get(int(sender.tag()))
        if action == "menu":
            self.show_menu(sender)
        elif action:
            self.invoke(action)


def _panel(identifier: str) -> InklowSuggestionPanel:
    panel = InklowSuggestionPanel.alloc().initWithContentRect_styleMask_backing_defer_(
        A.NSMakeRect(0, 0, 100, 30),
        A.NSWindowStyleMaskBorderless | A.NSWindowStyleMaskNonactivatingPanel,
        A.NSBackingStoreBuffered, False,
    )
    panel.setIdentifier_(identifier)
    panel.setAccessibilityIdentifier_(identifier)
    panel.setAccessibilityLabel_("Inklow suggestions" if identifier.endswith("suggestion") else "Inklow accessory")
    panel.setLevel_(A.NSFloatingWindowLevel)
    panel.setHidesOnDeactivate_(False)
    panel.setBecomesKeyOnlyIfNeeded_(True)
    panel.setFloatingPanel_(True)
    panel.setOpaque_(False)
    panel.setBackgroundColor_(A.NSColor.clearColor())
    panel.setHasShadow_(True)
    panel.setCollectionBehavior_(
        A.NSWindowCollectionBehaviorCanJoinAllSpaces
        | A.NSWindowCollectionBehaviorFullScreenAuxiliary
        | A.NSWindowCollectionBehaviorIgnoresCycle
    )
    panel.setReleasedWhenClosed_(False)
    return panel


def _surface(width: float, height: float):
    view = A.NSVisualEffectView.alloc().initWithFrame_(A.NSMakeRect(0, 0, width, height))
    view.setMaterial_(A.NSVisualEffectMaterialPopover)
    view.setBlendingMode_(A.NSVisualEffectBlendingModeBehindWindow)
    view.setState_(A.NSVisualEffectStateActive)
    view.setWantsLayer_(True)
    view.layer().setCornerRadius_(7)
    view.layer().setMasksToBounds_(True)
    return view


def _position(rect, width, height, *, accessory=False):
    """Convert global AX top-left points to Cocoa bottom-left points."""
    x, y, caret_width, caret_height = (float(n) for n in rect)
    screens = list(A.NSScreen.screens())
    if not screens:
        return A.NSMakeRect(x, y, width, height)
    origin_top = A.NSMaxY(screens[0].frame())
    caret_bottom = origin_top - y - caret_height
    screen = next((s for s in screens if A.NSPointInRect(
        A.NSMakePoint(x, caret_bottom), s.frame())), screens[0])
    visible = screen.visibleFrame()
    left = x - width - 4 if accessory else x + caret_width
    bottom = caret_bottom - height - 4
    if bottom < A.NSMinY(visible):
        bottom = caret_bottom + caret_height + 4
    left = max(A.NSMinX(visible) + 4, min(left, A.NSMaxX(visible) - width - 4))
    bottom = max(A.NSMinY(visible) + 4, min(bottom, A.NSMaxY(visible) - height - 4))
    return A.NSMakeRect(left, bottom, width, height)


class CaretOverlay:
    """Suggestion and accessory are independent panels; neither steals focus."""

    def __init__(self, invoke: Callable[[str], bool], menu, show_accessory=True):
        self.menu = menu
        self.show_accessory = show_accessory
        self.suggestion = _panel("inklow.suggestion")
        self.accessory = _panel("inklow.accessory")
        self.target = OverlayActions.alloc().init().setup(invoke, self._menu)
        self.rect = None
        self._event_key = None
        surface = _surface(27, 27)
        button = InklowOverlayButton.alloc().initWithFrame_(A.NSMakeRect(1, 1, 25, 25))
        button.setIdentifier_("inklow.accessory.menu")
        button.setAccessibilityIdentifier_("inklow.accessory.menu")
        button.setAccessibilityLabel_("Inklow menu")
        button.setToolTip_("Inklow — typing settings and pause controls")
        image = A.NSImage.alloc().initWithContentsOfFile_(str(asset_path("tray/active-template-18.png")))
        image.setTemplate_(True)
        button.setImage_(image)
        button.setImagePosition_(A.NSImageOnly)
        button.setBordered_(False)
        button.setTag_(100)
        button.setTarget_(self.target)
        button.setAction_("pressed:")
        self.target.actions[100] = "menu"
        surface.addSubview_(button)
        self.accessory.setContentView_(surface)

    def _menu(self, sender):
        # popUpMenu does not activate Inklow or change the target app's caret.
        self.menu.popUpMenuPositioningItem_atLocation_inView_(
            None, A.NSMakePoint(0, -4), sender)

    def configure(self, show_accessory: bool):
        if bool(show_accessory) == self.show_accessory:
            return
        self.show_accessory = bool(show_accessory)
        if not self.show_accessory:
            self.accessory.orderOut_(None)
        elif self.rect is not None:
            self.caret(self.rect)

    def caret(self, rect):
        if not rect or len(rect) != 4 or not all(math.isfinite(float(n)) for n in rect):
            self.clear(hide_accessory=True)
            return
        self.rect = tuple(rect)
        if self.show_accessory:
            self.accessory.setFrame_display_(_position(rect, 27, 27, accessory=True), True)
            self.accessory.orderFrontRegardless()

    def _present_inline(self, text, rect):
        """Paint ghost text on the caret baseline, without a detached card."""
        screens = list(A.NSScreen.screens())
        if not screens:
            return False
        font = A.NSFont.systemFontOfSize_(max(11, min(32, float(rect[3]) * 0.82)))
        attributes = NSDictionary.dictionaryWithDictionary_({
            A.NSFontAttributeName: font,
            # Mid-gray remains legible over both light and dark target fields.
            A.NSForegroundColorAttributeName: A.NSColor.colorWithCalibratedWhite_alpha_(0.46, 1),
        })
        title = NSAttributedString.alloc().initWithString_attributes_(text, attributes)
        width = math.ceil(title.size().width) + 6
        height = math.ceil(max(float(rect[3]) + 2, title.size().height))
        top = A.NSMaxY(screens[0].frame())
        bottom = top - float(rect[1]) - float(rect[3])
        left = float(rect[0]) + float(rect[2])
        screen = next((s for s in screens if A.NSPointInRect(A.NSMakePoint(left, bottom), s.frame())), screens[0])
        if width > A.NSMaxX(screen.visibleFrame()) - left - 6:
            return False
        surface = A.NSView.alloc().initWithFrame_(A.NSMakeRect(0, 0, width, height))
        button = InklowOverlayButton.alloc().initWithFrame_(A.NSMakeRect(0, 0, width, height))
        button.setAttributedTitle_(title)
        button.setAlignment_(A.NSTextAlignmentLeft)
        button.setBordered_(False)
        button.setFocusRingType_(A.NSFocusRingTypeNone)
        button.setAccessibilityIdentifier_("inklow.suggestion.accept")
        button.setAccessibilityLabel_(f"Accept completion: {text}")
        button.setTag_(1)
        button.setTarget_(self.target)
        button.setAction_("pressed:")
        self.target.actions = {100: "menu", 1: "all"}
        surface.addSubview_(button)
        self.suggestion.setContentView_(surface)
        self.suggestion.setHasShadow_(False)
        self.suggestion.setFrame_display_(A.NSMakeRect(left, bottom - (height - float(rect[3])) / 2, width, height), True)
        self.suggestion.orderFrontRegardless()
        return True

    def present(self, event: dict):
        rect = event.get("rect")
        if not rect:
            self.clear(hide_accessory=True)
            return
        self.caret(rect)
        choices = [str(s) for s in event.get("choices", []) if str(s)][:9]
        text = str(event.get("text", ""))
        key = (text, tuple(choices), event.get("mode"), tuple(rect))
        if key == self._event_key:
            return
        self._event_key = key
        rows = choices or ([text] if text else [])
        if not rows:
            self.clear()
            return
        if not choices and event.get("mode", "completion") == "completion" and event.get("inline", True):
            if self._present_inline(text, rect):
                return
        self.suggestion.setHasShadow_(True)
        width = min(480, max(220, max(len(s) for s in rows) * 7 + 58))
        row_height = 32 if choices else min(108, max(34, math.ceil(len(text) * 7 / (width - 24)) * 19 + 12))
        height = row_height * len(rows) + 8
        surface = _surface(width, height)
        self.target.actions = {100: "menu"}
        for index, value in enumerate(rows):
            button = InklowOverlayButton.alloc().initWithFrame_(
                A.NSMakeRect(6, height - (index + 1) * row_height - 4, width - 12, row_height))
            title = f"{index + 1}   {value}" if choices else value
            button.setTitle_(title)
            button.setFont_(A.NSFont.systemFontOfSize_(14))
            button.setAlignment_(A.NSTextAlignmentLeft)
            button.setBordered_(False)
            button.cell().setWraps_(True)
            button.cell().setLineBreakMode_(A.NSLineBreakByWordWrapping)
            button.setIdentifier_(f"inklow.suggestion.choice.{index + 1}" if choices else "inklow.suggestion.accept")
            button.setAccessibilityIdentifier_(f"inklow.suggestion.choice.{index + 1}" if choices else "inklow.suggestion.accept")
            button.setAccessibilityLabel_(f"Accept choice {index + 1}: {value}" if choices else f"Accept completion: {value}")
            button.setToolTip_("Click to insert into the original text field")
            tag = index + 1
            button.setTag_(tag)
            button.setTarget_(self.target)
            button.setAction_("pressed:")
            self.target.actions[tag] = f"choice:{index + 1}" if choices else "all"
            surface.addSubview_(button)
        self.suggestion.setContentView_(surface)
        self.suggestion.setFrame_display_(_position(rect, width, height), True)
        self.suggestion.orderFrontRegardless()

    def clear(self, *, hide_accessory=False):
        self.suggestion.orderOut_(None)
        self._event_key = None
        if hide_accessory:
            self.rect = None
            self.accessory.orderOut_(None)

    def close(self):
        self.clear(hide_accessory=True)
        self.suggestion.close()
        self.accessory.close()
