"""Nonactivating caret UI. Only the main AppKit run loop touches these views."""
from __future__ import annotations

import math
from typing import Callable

import AppKit as A
import objc
from Foundation import NSObject

from .resources import asset_path
from .core import InlineRender


class BroTypistSuggestionPanel(A.NSPanel):
    def canBecomeKeyWindow(self):
        return False

    def canBecomeMainWindow(self):
        return False


class BroTypistOverlayButton(A.NSButton):
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


def _panel(identifier: str) -> BroTypistSuggestionPanel:
    panel = BroTypistSuggestionPanel.alloc().initWithContentRect_styleMask_backing_defer_(
        A.NSMakeRect(0, 0, 100, 30),
        A.NSWindowStyleMaskBorderless | A.NSWindowStyleMaskNonactivatingPanel,
        A.NSBackingStoreBuffered, False,
    )
    panel.setIdentifier_(identifier)
    panel.setAccessibilityIdentifier_(identifier)
    panel.setAccessibilityLabel_("BroTypist suggestions" if identifier.endswith("suggestion") else "BroTypist accessory")
    panel.setLevel_(A.NSPopUpMenuWindowLevel)
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
        | A.NSWindowCollectionBehaviorTransient
    )
    panel.setReleasedWhenClosed_(False)
    return panel


def _dark(view):
    return view.effectiveAppearance().bestMatchFromAppearancesWithNames_(
        [A.NSAppearanceNameAqua, A.NSAppearanceNameDarkAqua]) == A.NSAppearanceNameDarkAqua


def _rgb(hex_value):
    value = hex_value.lstrip("#")
    return A.NSColor.colorWithSRGBRed_green_blue_alpha_(
        int(value[:2], 16) / 255, int(value[2:4], 16) / 255, int(value[4:6], 16) / 255, 1)


def ghost_color(view):
    contrast = A.NSWorkspace.sharedWorkspace().accessibilityDisplayShouldIncreaseContrast()
    return _rgb(("9A9CA3" if _dark(view) else "6E7078") if contrast else ("6C6F77" if _dark(view) else "A2A4AA"))


class ChipSurface(A.NSVisualEffectView):
    def viewDidChangeEffectiveAppearance(self):
        objc.super(ChipSurface, self).viewDidChangeEffectiveAppearance()
        self.apply_accessibility()

    @objc.python_method
    def apply_accessibility(self):
        if self.layer() is None:
            return
        workspace = A.NSWorkspace.sharedWorkspace()
        contrast = workspace.accessibilityDisplayShouldIncreaseContrast()
        opaque = workspace.accessibilityDisplayShouldReduceTransparency()
        self.setState_(A.NSVisualEffectStateInactive if opaque else A.NSVisualEffectStateActive)
        self.layer().setBackgroundColor_(A.NSColor.windowBackgroundColor().CGColor() if opaque else A.NSColor.clearColor().CGColor())
        self.layer().setBorderWidth_(2 if contrast else 1)
        border = A.NSColor.whiteColor() if _dark(self) else A.NSColor.blackColor()
        self.layer().setBorderColor_(border.colorWithAlphaComponent_(0.8 if contrast else (0.14 if _dark(self) else 0.12)).CGColor())


def _surface(width: float, height: float):
    view = ChipSurface.alloc().initWithFrame_(A.NSMakeRect(0, 0, width, height))
    view.setMaterial_(A.NSVisualEffectMaterialPopover)
    view.setBlendingMode_(A.NSVisualEffectBlendingModeBehindWindow)
    view.setWantsLayer_(True)
    view.layer().setCornerRadius_(8)
    view.layer().setMasksToBounds_(True)
    view.apply_accessibility()
    return view


def _chip_text(text: str) -> str:
    """Presentation only; the controller retains the complete insertion value."""
    clean = " ".join(text.split())
    if len(clean) <= 44:
        return clean
    prefix = clean[:43]
    boundary = 43 if clean[43].isspace() else prefix.rfind(" ")
    return (prefix[:boundary] if boundary > 0 else prefix).rstrip() + "…"


def _position(rect, width, height, *, accessory=False, field_rect=None):
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
    left = x - width - 6 if accessory else x
    bottom = origin_top - y + 6 if accessory else caret_bottom - height - 6
    if accessory:
        if left < A.NSMinX(visible) + 4:
            left = max(x, A.NSMinX(visible) + 4)
            if field_rect:
                bottom = origin_top - min(y, float(field_rect[1])) + 6
        if bottom + height > A.NSMaxY(visible) - 4:
            if field_rect and x - width - 6 >= A.NSMinX(visible) + 4:
                left = x - width - 6
            elif field_rect and float(field_rect[0]) + float(field_rect[2]) + width + 6 <= A.NSMaxX(visible) - 4:
                left = float(field_rect[0]) + float(field_rect[2]) + 6
            else:
                return None
            bottom = max(A.NSMinY(visible) + 4, origin_top - y - height)
    elif caret_bottom - A.NSMinY(visible) < height + 40:
        bottom = caret_bottom + caret_height + 6
    left = max(A.NSMinX(visible) + 8, min(left, A.NSMaxX(visible) - width - 8))
    bottom = max(A.NSMinY(visible) + 8, min(bottom, A.NSMaxY(visible) - height - 8))
    return A.NSMakeRect(left, bottom, width, height)


class GhostSurface(A.NSView):
    """Draw only host-font glyphs at the verified baseline; no card or keycap."""
    @objc.python_method
    def setup(self, text, font, baseline):
        self.text, self.font, self.baseline = text, font, baseline
        self.setAccessibilityElement_(False)
        return self

    def isOpaque(self):
        return False

    def hitTest_(self, point):
        return None

    def drawRect_(self, dirty):
        attributes = {A.NSFontAttributeName: self.font,
                      A.NSForegroundColorAttributeName: ghost_color(self)}
        # NSString's origin is the text line's lower-left (including descent).
        from Foundation import NSString
        NSString.stringWithString_(self.text).drawAtPoint_withAttributes_(
            A.NSMakePoint(0, self.baseline + float(self.font.descender())), attributes)


class CaretOverlay:
    """Suggestion and accessory are independent panels; neither steals focus."""

    def __init__(self, invoke: Callable[[str], bool], menu_factory, show_accessory=True):
        self.menu_factory = menu_factory
        self.accept_key = ""
        self._active_menu = None
        self.show_accessory = show_accessory
        self.suggestion = _panel("inklow.suggestion")
        self.accessory = _panel("inklow.accessory")
        self.target = OverlayActions.alloc().init().setup(invoke, self._menu)
        self.rect = None
        self.field_rect = None
        self._event_key = None
        self.accessory.setHasShadow_(False)
        surface = A.NSView.alloc().initWithFrame_(A.NSMakeRect(0, 0, 24, 24))
        button = BroTypistOverlayButton.alloc().initWithFrame_(A.NSMakeRect(1, 1, 22, 22))
        button.setIdentifier_("inklow.accessory.menu")
        button.setAccessibilityIdentifier_("inklow.accessory.menu")
        button.setAccessibilityLabel_("BroTypist menu")
        button.setToolTip_("BroTypist — typing settings and pause controls")
        button.setContentTintColor_(A.NSColor.secondaryLabelColor())
        image = A.NSImage.alloc().initWithContentsOfFile_(str(asset_path("tray/active-template-18.png")))
        image.setTemplate_(True)
        image.setSize_(A.NSMakeSize(18, 16))
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
        self._active_menu = self.menu_factory()
        try:
            self._active_menu.popUpMenuPositioningItem_atLocation_inView_(
                None, A.NSMakePoint(0, -4), sender)
        finally:
            self._active_menu = None

    def configure(self, show_accessory: bool, accept_key: str = ""):
        self.accept_key = {"Tab": "⇥", "AboveTab": "Above Tab"}.get(accept_key, accept_key)
        if bool(show_accessory) == self.show_accessory:
            return
        self.show_accessory = bool(show_accessory)
        if not self.show_accessory:
            self.accessory.orderOut_(None)
        elif self.rect is not None or self.field_rect is not None:
            self.caret(self.rect, self.field_rect)

    def caret(self, rect, field_rect=None):
        anchor = field_rect or rect
        if (not anchor or len(anchor) != 4
                or not all(math.isfinite(float(n)) for n in anchor)
                or float(anchor[2]) < 0 or float(anchor[3]) <= 0):
            self.clear(hide_accessory=True)
            return
        self.rect = tuple(rect) if rect else None
        self.field_rect = tuple(field_rect) if field_rect else None
        if self.show_accessory:
            line = rect or anchor
            placement = (anchor[0], line[1], anchor[2], line[3])
            frame = _position(placement, 24, 24, accessory=True, field_rect=field_rect)
            if frame is None:
                self.accessory.orderOut_(None)
                return
            self.accessory.setFrame_display_(frame, True)
            self.accessory.orderFrontRegardless()


    def present(self, event: dict):
        rect = event.get("rect")
        if not rect:
            self.clear(hide_accessory=True)
            return
        self.caret(rect, event.get("field_rect"))
        choices = [str(s) for s in event.get("choices", []) if str(s)][:9]
        text = str(event.get("text", ""))
        key = (text, tuple(choices), event.get("mode"), tuple(rect),
               bool(event.get("field_anchor")), self.accept_key, repr(event.get("render")))
        if key == self._event_key:
            return
        self._event_key = key
        rows = choices or ([text] if text else [])
        if not rows:
            self.clear()
            return
        if event.get("mode") == "completion" or not choices:
            self._present_inline(event)
            return
        self.suggestion.setIgnoresMouseEvents_(False)
        # Explicit alternatives are a separate interactive surface.
        self.suggestion.setHasShadow_(True)
        visible_rows = [_chip_text(value) for value in rows]
        width = min(480, max(180, max(len(value) for value in visible_rows) * 7 + 60))
        row_height = 30
        height = row_height * len(rows) + 8
        surface = _surface(width, height)
        self.target.actions = {100: "menu"}
        for index, value in enumerate(rows):
            button = BroTypistOverlayButton.alloc().initWithFrame_(
                A.NSMakeRect(10, height - (index + 1) * row_height - 4, width - 20, row_height))
            title = f"{index + 1}   {visible_rows[index]}"
            button.setTitle_(title)
            button.setFont_(A.NSFont.systemFontOfSize_(13))
            button.setContentTintColor_(A.NSColor.labelColor())
            button.setAlignment_(A.NSTextAlignmentLeft)
            button.setBordered_(False)
            button.cell().setWraps_(False)
            button.cell().setLineBreakMode_(A.NSLineBreakByTruncatingTail)
            button.setIdentifier_(f"inklow.suggestion.choice.{index + 1}")
            button.setAccessibilityIdentifier_(f"inklow.suggestion.choice.{index + 1}")
            button.setAccessibilityLabel_(f"Accept choice {index + 1}: {value}")
            button.setToolTip_("Click to insert into the original text field")
            tag = index + 1
            button.setTag_(tag)
            button.setTarget_(self.target)
            button.setAction_("pressed:")
            self.target.actions[tag] = f"choice:{index + 1}"
            surface.addSubview_(button)
        self.suggestion.setContentView_(surface)
        card_rect = (rect[0], rect[1], 0, rect[3]) if event.get("field_anchor") else rect
        self.suggestion.setFrame_display_(_position(card_rect, width, height), True)
        self.suggestion.orderFrontRegardless()

    def _present_inline(self, event):
        self.suggestion.orderOut_(None)
        payload, rect = event.get("render"), event.get("rect")
        if not event.get("inline") or not isinstance(payload, dict) or not rect:
            return
        try:
            render = InlineRender(**payload)
            if not render.valid(rect):
                return
            font = A.NSFont.fontWithName_size_(render.font_name, render.font_size)
            if font is None or str(font.fontName()) != render.font_name:
                return
            text = str(event.get("text") or "").split("\n", 1)[0].split("\r", 1)[0]
            if not text:
                return
            from Foundation import NSString
            width = float(NSString.stringWithString_(text).sizeWithAttributes_(
                {A.NSFontAttributeName: font}).width)
            x = float(rect[0])
            clip_x, clip_y, clip_w, clip_h = render.clip_rect
            # Never clamp or wrap a completion to a different host position.
            width = min(width, clip_x + clip_w - x)
            top, bottom = render.baseline - render.ascent, render.baseline + render.descent
            if width <= 0 or top < clip_y or bottom > clip_y + clip_h + 1:
                return
            screens = list(A.NSScreen.screens())
            if not screens:
                return
            origin_top = A.NSMaxY(screens[0].frame())
            height = max(1, math.ceil(bottom - top))
            frame = A.NSMakeRect(x, origin_top - bottom, max(1, width), height)
            # A fully clipped/occluded screen row must never become a popup.
            if not any(A.NSContainsRect(screen.frame(), frame) for screen in screens):
                return
            surface = GhostSurface.alloc().initWithFrame_(A.NSMakeRect(0, 0, width, height))
            surface.setup(text, font, render.descent)
            self.suggestion.setHasShadow_(False)
            self.suggestion.setIgnoresMouseEvents_(True)
            self.suggestion.setContentView_(surface)
            self.suggestion.setFrame_display_(frame, True)
            self.suggestion.orderFrontRegardless()
        except (TypeError, ValueError, KeyError):
            return

    def clear(self, *, hide_accessory=False):
        self.suggestion.orderOut_(None)
        self._event_key = None
        if hide_accessory:
            self.rect = None
            self.field_rect = None
            self.accessory.orderOut_(None)

    def close(self):
        self.clear(hide_accessory=True)
        self.suggestion.close()
        self.accessory.close()
