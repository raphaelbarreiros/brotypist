"""Nonactivating caret UI. Only the main AppKit run loop touches these views.

Three independent panels never steal focus: the mouse-transparent ghost
(same-line host-font text, optionally a displaced host suffix or a typo
strike-through), the explicit numbered chooser, and the tiny accessory icon.
Every presentation reports the exact text it drew, or '' when it drew nothing.
"""
from __future__ import annotations

import math
from typing import Callable

import AppKit as A
import objc
from Foundation import NSObject

from .resources import asset_path
from .core import InlineRender
from .native.macos import resolve_font, text_width


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
            # Option-click sends the host a plain Tab instead of the menu.
            if A.NSEvent.modifierFlags() & A.NSEventModifierFlagOption:
                self.invoke("plain_tab")
            else:
                self.show_menu(sender)
        elif action:
            self.invoke(action)


def _panel(identifier: str, label: str) -> BroTypistSuggestionPanel:
    panel = BroTypistSuggestionPanel.alloc().initWithContentRect_styleMask_backing_defer_(
        A.NSMakeRect(0, 0, 100, 30),
        A.NSWindowStyleMaskBorderless | A.NSWindowStyleMaskNonactivatingPanel,
        A.NSBackingStoreBuffered, False,
    )
    panel.setIdentifier_(identifier)
    panel.setAccessibilityIdentifier_(identifier)
    panel.setAccessibilityLabel_(label)
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


def _srgb(components):
    red, green, blue = (float(part) for part in components)
    return A.NSColor.colorWithSRGBRed_green_blue_alpha_(red, green, blue, 1)


def ghost_color(view):
    contrast = A.NSWorkspace.sharedWorkspace().accessibilityDisplayShouldIncreaseContrast()
    return _rgb(("9A9CA3" if _dark(view) else "6E7078") if contrast else ("6C6F77" if _dark(view) else "A2A4AA"))


def _measured_ghost_color(render: InlineRender, view):
    """Grey between measured host ink and background; else the system grey."""
    if render.text_rgb is None or render.background_rgb is None:
        return ghost_color(view)
    contrast = A.NSWorkspace.sharedWorkspace().accessibilityDisplayShouldIncreaseContrast()
    share = 0.6 if contrast else 0.42
    return _srgb(tuple(background + (ink - background) * share
                       for ink, background in zip(render.text_rgb, render.background_rgb)))


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
        # Stay outside the field: horizontal clamping could cover earlier lines.
        if (left < A.NSMinX(visible) + 4 or left + width > A.NSMaxX(visible) - 4
                or bottom < A.NSMinY(visible) + 4 or bottom + height > A.NSMaxY(visible) - 4):
            return None
        return A.NSMakeRect(left, bottom, width, height)
    elif caret_bottom - A.NSMinY(visible) < height + 40:
        bottom = caret_bottom + caret_height + 6
    left = max(A.NSMinX(visible) + 8, min(left, A.NSMaxX(visible) - width - 8))
    bottom = max(A.NSMinY(visible) + 8, min(bottom, A.NSMaxY(visible) - height - 8))
    return A.NSMakeRect(left, bottom, width, height)


def fit_text(font, text: str, available: float) -> str:
    """Longest prefix ending at a word boundary that fits; never a cut word."""
    if available <= 0 or not text:
        return ""
    if text_width(font, text) <= available:
        return text
    best = ""
    for index, character in enumerate(text):
        if character.isspace() and text[:index].strip():
            candidate = text[:index]
            if text_width(font, candidate) > available:
                break
            best = candidate
    return best


def _finite_rect(value) -> tuple[float, float, float, float] | None:
    if not isinstance(value, (tuple, list)) or len(value) != 4:
        return None
    try:
        rect = tuple(float(part) for part in value)
    except (TypeError, ValueError):
        return None
    return rect if all(math.isfinite(part) for part in rect) and rect[2] >= 0 and rect[3] > 0 else None


class GhostSurface(A.NSView):
    """Host-font glyphs at the verified baseline; no card, keycap or shadow."""
    @objc.python_method
    def setup(self, font, baseline, runs, fills=()):
        # runs: (text, x, NSColor); fills: (NSRect, NSColor), painted first.
        self.font, self.baseline, self.runs, self.fills = font, baseline, runs, fills
        self.setAccessibilityElement_(False)
        return self

    def isOpaque(self):
        return False

    def hitTest_(self, point):
        return None

    def drawRect_(self, dirty):
        import CoreText
        import Quartz
        from Foundation import NSAttributedString
        context = A.NSGraphicsContext.currentContext().CGContext()
        for rect, colour in self.fills:
            colour.setFill()
            A.NSBezierPath.fillRect_(rect)
        Quartz.CGContextSetTextMatrix(context, Quartz.CGAffineTransformIdentity)
        for text, x, colour in self.runs:
            Quartz.CGContextSetFillColorWithColor(context, colour.CGColor())
            attributed = NSAttributedString.alloc().initWithString_attributes_(
                text, {"NSFont": self.font, CoreText.kCTForegroundColorFromContextAttributeName: True})
            line = CoreText.CTLineCreateWithAttributedString(attributed)
            # CoreText draws exactly at the baseline, matching text_width().
            Quartz.CGContextSetTextPosition(context, x, self.baseline)
            CoreText.CTLineDraw(line, context)


def _accessory_image():
    """An unboxed template glyph, with physical pixels kept separate from points."""
    size = A.NSMakeSize(18, 16)
    image = A.NSImage.alloc().initWithSize_(size)
    for pixels in (18, 36):
        representation = A.NSBitmapImageRep.imageRepWithContentsOfFile_(
            str(asset_path(f"tray/active-template-{pixels}.png")))
        representation.setSize_(size)
        image.addRepresentation_(representation)
    image.setTemplate_(True)
    return image


class CaretOverlay:
    """Ghost, chooser and accessory are independent panels; none steals focus."""

    def __init__(self, invoke: Callable[[str], bool], menu_factory, show_accessory=True,
                 on_present: Callable[[object, str, str], None] | None = None):
        self.menu_factory = menu_factory
        self.on_present = on_present
        self.accept_key = ""
        self._active_menu = None
        self.show_accessory = show_accessory
        self.ghost = _panel("inklow.suggestion", "BroTypist suggestion")
        self.ghost.setHasShadow_(False)
        self.ghost.setIgnoresMouseEvents_(True)
        self.chooser = _panel("inklow.chooser", "BroTypist suggestions")
        self.accessory = _panel("inklow.accessory", "BroTypist accessory")
        self.target = OverlayActions.alloc().init().setup(invoke, self._menu)
        self.rect = None
        self.field_rect = None
        self._docs_anchor = None
        self._ghost_state = None    # (presentation identity, visible text)
        self._chooser_state = None
        self.accessory.setHasShadow_(False)
        surface = A.NSView.alloc().initWithFrame_(A.NSMakeRect(0, 0, 24, 24))
        button = BroTypistOverlayButton.alloc().initWithFrame_(A.NSMakeRect(1, 1, 22, 22))
        button.setIdentifier_("inklow.accessory.menu")
        button.setAccessibilityIdentifier_("inklow.accessory.menu")
        button.setAccessibilityLabel_("BroTypist menu")
        button.setToolTip_("BroTypist — typing settings and pause controls. Option-click sends a normal Tab.")
        button.setContentTintColor_(A.NSColor.secondaryLabelColor())
        button.setImage_(_accessory_image())
        button.setImagePosition_(A.NSImageOnly)
        button.setBordered_(False)
        button.setTag_(100)
        button.setTarget_(self.target)
        button.setAction_("pressed:")
        self.target.actions[100] = "menu"
        surface.addSubview_(button)
        self.accessory_button = button
        self.accessory.setContentView_(surface)

    def _menu(self, sender):
        self._active_menu = self.menu_factory()
        try:
            self._active_menu.popUpMenuPositioningItem_atLocation_inView_(
                None, A.NSMakePoint(0, -4), sender)
        finally:
            self._active_menu = None

    def _report(self, key, visible: str, mode: str) -> None:
        callback = self.on_present
        if callback is None or key is None:
            return
        try:
            callback(key, visible, mode)
        except Exception:
            pass

    def configure(self, show_accessory: bool, accept_key: str = ""):
        self.accept_key = {"Tab": "⇥", "AboveTab": "Above Tab"}.get(accept_key, accept_key)
        if bool(show_accessory) == self.show_accessory:
            return
        self.show_accessory = bool(show_accessory)
        if not self.show_accessory:
            self.accessory.orderOut_(None)
        elif self._docs_anchor is not None:
            self._place_docs_help()
        elif self.rect is not None or self.field_rect is not None:
            self.caret(self.rect, self.field_rect)

    def caret(self, rect, field_rect=None):
        if getattr(self, "_docs_anchor", None) is not None:
            self.clear(hide_accessory=True)
        self._docs_anchor = None
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

    def _place_docs_help(self):
        if not self.show_accessory or self._docs_anchor is None:
            self.accessory.orderOut_(None)
            return
        frame = _position(self._docs_anchor, 24, 24, accessory=True)
        if frame is None:
            self.accessory.orderOut_(None)
            return
        self.accessory.setFrame_display_(frame, True)
        self.accessory.orderFrontRegardless()

    def docs_help(self, hint):
        """Place only the accessory at document metadata, never a caret/ghost."""
        if hint is None:
            if self._docs_anchor is not None:
                self.clear(hide_accessory=True)
            return
        anchor = _finite_rect(hint.get("anchor")) if isinstance(hint, dict) else None
        self.clear(hide_accessory=True)
        if anchor is None:
            return
        self._docs_anchor = anchor
        self.accessory_button.setAccessibilityLabel_("BroTypist Google Docs accessibility help")
        self.accessory_button.setToolTip_(
            "Google Docs accessibility help — open the menu for setup. "
            "Completion support is not verified. Option-click sends a normal Tab.")
        self._place_docs_help()

    def present(self, event: dict):
        key = event.get("key")
        mode = str(event.get("mode") or "completion")
        rect = event.get("rect")
        choices = [str(s) for s in event.get("choices", []) if str(s)][:9]
        chooser_mode = str(event.get("choices_mode") or mode) if mode in ("completion", "correction") else mode
        if not rect:
            self.clear(hide_accessory=True)
            self._report(key, "", mode)
            if choices and chooser_mode != mode:
                self._report(key, "", chooser_mode)
            return
        self.caret(rect, event.get("field_rect"))
        if mode in ("completion", "correction"):
            identity = (str(event.get("text", "")), mode, tuple(rect), repr(event.get("render")),
                        event.get("inline"), str(event.get("advance") or ""), repr(event.get("correction")))
            if (self._ghost_state is None or self._ghost_state[0] != identity
                    or not self._ghost_state[1] or not self.ghost.isVisible()
                    or not self.ghost.isOnActiveSpace()):
                self._ghost_state = (identity, self._present_ghost(event, mode))
            self._report(key, self._ghost_state[1], mode)
            if choices:
                self._present_chooser(event, choices, chooser_mode, key)
            return
        self._present_chooser(event, choices or [str(event.get("text", ""))], mode, key)

    def _present_chooser(self, event, rows, mode, key):
        rows = [row for row in rows if row][:9]
        rect = event.get("rect")
        identity = (tuple(rows), mode, tuple(rect), bool(event.get("field_anchor")))
        if self._chooser_state is not None and self._chooser_state[0] == identity:
            self._report(key, self._chooser_state[1], mode)
            return
        if not rows:
            self.chooser.orderOut_(None)
            self._chooser_state = None
            self._report(key, "", mode)
            return
        visible_rows = [_chip_text(value) for value in rows]
        width = min(480, max(180, max(len(value) for value in visible_rows) * 7 + 60))
        row_height = 30
        height = row_height * len(rows) + 8
        surface = _surface(width, height)
        self.target.actions = {100: "menu"}
        fully_visible = []
        for index, value in enumerate(rows):
            button = BroTypistOverlayButton.alloc().initWithFrame_(
                A.NSMakeRect(10, height - (index + 1) * row_height - 4, width - 20, row_height))
            title = f"{index + 1}   {visible_rows[index]}"
            font = A.NSFont.systemFontOfSize_(13)
            complete = (all(fully_visible) and visible_rows[index] == value
                        and text_width(font, title) <= width - 40)
            fully_visible.append(complete)
            button.setTitle_(title)
            button.setFont_(font)
            button.setContentTintColor_(A.NSColor.labelColor())
            button.setAlignment_(A.NSTextAlignmentLeft)
            button.setBordered_(False)
            button.cell().setWraps_(False)
            button.cell().setLineBreakMode_(A.NSLineBreakByTruncatingTail)
            button.setIdentifier_(f"inklow.suggestion.choice.{index + 1}")
            button.setAccessibilityIdentifier_(f"inklow.suggestion.choice.{index + 1}")
            button.setAccessibilityLabel_(f"Accept choice {index + 1}: {value}")
            button.setToolTip_(value)
            tag = index + 1
            button.setTag_(tag)
            button.setTarget_(self.target)
            button.setAction_("pressed:")
            button.setEnabled_(complete)
            if complete:
                self.target.actions[tag] = f"choice:{index + 1}"
            surface.addSubview_(button)
        self.chooser.setContentView_(surface)
        card_rect = (rect[0], rect[1], 0, rect[3]) if event.get("field_anchor") else rect
        self.chooser.setFrame_display_(_position(card_rect, width, height), True)
        self.chooser.orderFrontRegardless()
        # The acknowledgment protocol arms a contiguous prefix only. Never
        # claim a truncated label (or a later row beyond it) was fully drawn.
        count = next((i for i, complete in enumerate(fully_visible) if not complete), len(rows))
        visible = "\n".join(rows[:count])
        self._chooser_state = (identity, visible)
        self._report(key, visible, mode)

    def _hide_ghost(self) -> str:
        self.ghost.orderOut_(None)
        return ""

    def _present_ghost(self, event, mode) -> str:
        """Draw same-line ghost text; return exactly what is visible, or ''."""
        payload, caret = event.get("render"), _finite_rect(event.get("rect"))
        if (not isinstance(payload, dict) or caret is None
                or (mode == "completion" and not event.get("inline"))):
            return self._hide_ghost()
        try:
            render = InlineRender(**payload)
            if not render.valid(caret):
                return self._hide_ghost()
            font = resolve_font(render)
            if font is None:
                return self._hide_ghost()
            text = str(event.get("text") or "").split("\n", 1)[0].split("\r", 1)[0]
            advance = str(event.get("advance") or "")
            if not text or "\n" in advance or "\r" in advance:
                return self._hide_ghost()
            # After a word is accepted the host has moved its text; the
            # remainder starts after that word in the same verified run.
            x = caret[0] + (text_width(font, advance) if advance else 0.0)
            clip_x, clip_y, clip_w, clip_h = (float(n) for n in render.clip_rect)
            available = clip_x + clip_w - x - render.suffix_width
            left, gap, strike = x, 0.0, None
            if mode == "correction":
                correction = event.get("correction")
                strike = _finite_rect(correction.get("rect")) if isinstance(correction, dict) else None
                # The typo lies on the caret's own row and line, left of the fix.
                if (strike is None or strike[2] <= 0 or advance
                        or not strike[1] < render.baseline <= strike[1] + strike[3] + 1
                        or strike[0] < clip_x or strike[0] + strike[2] > x + 1.5):
                    return self._hide_ghost()
                if abs(strike[0] + strike[2] - x) <= 1.5:
                    gap = text_width(font, " ")
                visible = text if text_width(font, text) + gap <= available else ""
                left = strike[0]
            else:
                visible = fit_text(font, text, available)
            if not visible:
                return self._hide_ghost()
            ghost_width = text_width(font, visible)
            right = x + gap + ghost_width + render.suffix_width
            displaced = bool(render.suffix.strip())
            top, bottom = render.baseline - render.ascent, render.baseline + render.descent
            if displaced:
                # Repaint the whole caret row so displaced host glyphs leave no trace.
                top, bottom = min(top, caret[1]), max(bottom, caret[1] + caret[3])
            if (left < clip_x or right > clip_x + clip_w + 0.5
                    or top < clip_y or bottom > clip_y + clip_h + 1):
                return self._hide_ghost()
            screens = list(A.NSScreen.screens())
            if not screens:
                return self._hide_ghost()
            origin_top = A.NSMaxY(screens[0].frame())
            cocoa_left, cocoa_bottom = left, origin_top - bottom
            screen = next((s for s in screens if A.NSPointInRect(
                A.NSMakePoint(cocoa_left, cocoa_bottom), s.frame())), None)
            if screen is None:
                return self._hide_ghost()
            # Align the window to device pixels; draw at the exact sub-pixel offset.
            scale = float(screen.backingScaleFactor() or 1.0)
            frame_x = math.floor(cocoa_left * scale) / scale
            frame_y = math.floor(cocoa_bottom * scale) / scale
            dx, dy = cocoa_left - frame_x, cocoa_bottom - frame_y
            width = math.ceil((right - left + dx) * scale) / scale
            height = math.ceil((bottom - top + dy) * scale) / scale
            frame = A.NSMakeRect(frame_x, frame_y, max(1.0, width), max(1.0, height))
            # A fully clipped/occluded screen row must never become a popup.
            if not any(A.NSContainsRect(item.frame(), frame) for item in screens):
                return self._hide_ghost()
            surface = GhostSurface.alloc().initWithFrame_(A.NSMakeRect(0, 0, frame.size.width, frame.size.height))
            colour = _measured_ghost_color(render, surface)
            baseline = dy + (bottom - render.baseline)
            ghost_x = dx + (x - left) + gap
            runs = [(visible, ghost_x, colour)]
            fills = []
            if displaced:
                fills.append((A.NSMakeRect(dx + (x - left), dy, right - x, bottom - top),
                              _srgb(render.background_rgb)))
                runs.append((render.suffix, ghost_x + ghost_width, _srgb(render.text_rgb)))
            if strike is not None:
                thickness = max(1.0, float(font.underlineThickness()))
                middle = baseline + float(font.xHeight()) / 2
                fills.append((A.NSMakeRect(dx, middle - thickness / 2, strike[2], thickness), colour))
            surface.setup(font, baseline, runs, tuple(fills))
            self.ghost.setContentView_(surface)
            self.ghost.setFrame_display_(frame, True)
            self.ghost.orderFrontRegardless()
            return visible if self.ghost.isVisible() and self.ghost.isOnActiveSpace() else ""
        except (TypeError, ValueError, KeyError, AttributeError):
            return self._hide_ghost()

    def clear(self, *, hide_accessory=False, panel=None):
        if panel in (None, "ghost"):
            self.ghost.orderOut_(None)
            self._ghost_state = None
        if panel in (None, "chooser"):
            self.chooser.orderOut_(None)
            self._chooser_state = None
        if hide_accessory:
            self.rect = None
            self.field_rect = None
            self._docs_anchor = None
            button = getattr(self, "accessory_button", None)
            if button is not None:
                button.setAccessibilityLabel_("BroTypist menu")
                button.setToolTip_(
                    "BroTypist — typing settings and pause controls. Option-click sends a normal Tab.")
            self.accessory.orderOut_(None)

    def close(self):
        self.clear(hide_accessory=True)
        self.ghost.close()
        self.chooser.close()
        self.accessory.close()
