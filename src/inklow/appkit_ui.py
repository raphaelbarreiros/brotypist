"""The macOS menu-first shell; this module never imports a Qt component.

Accessibility identifiers use ``inklow.<pane>.<setting>``. The AppKit timer is
sole owner of controller events and overlays; keyboard callbacks never own UI.
"""
from __future__ import annotations

from copy import deepcopy
from collections import deque
from dataclasses import replace
import math
import os
from pathlib import Path
import time
import threading
from urllib.parse import urlsplit

import AppKit as A
import objc
from Foundation import NSAttributedString, NSBundle, NSIndexSet, NSObject, NSRunLoop, NSRunLoopCommonModes, NSTimer, NSURL

from . import __version__, system
from .appkit_overlay import CaretOverlay, _dark, _rgb
from .resources import APP_NAME, REPOSITORY_URL, asset_path
from .runtime import Controller, ShutdownPending
from .updates import UpdateService
from .update_install import HEALTH_NONCE, HEALTH_PATH, acknowledge_startup
from .shortcuts import mac_key_name, normalize_shortcut, shortcut_from_event

PANES = (
    ("setup", "Setup", "checklist"),
    ("general", "General", "gearshape"),
    ("context", "Context", "text.viewfinder"),
    ("personalization", "Personalization", "person.text.rectangle"),
    ("emoji", "Emoji", "face.smiling"),
    ("shortcuts", "Shortcuts", "command"),
    ("battery", "Battery", "battery.75percent"),
    ("appSettings", "App Settings", "square.grid.2x2"),
    ("labs", "Labs", "flask"),
    ("statistics", "Statistics", "chart.bar.xaxis"),
    ("about", "About", "info.circle"),
)
# Sidebar groups mirror the native settings layout; "support" opens externally.
SIDEBAR_GROUPS = (
    ("setup",),
    ("general", "context", "personalization", "emoji", "shortcuts", "battery", "appSettings"),
    ("labs", "statistics"),
    ("about", "support"),
)
SUPPORT_URL = REPOSITORY_URL + "/issues"
RELEASES_URL = REPOSITORY_URL + "/releases"
URL_SCHEME = "brotypist"
_SIDEBAR_COLORS = {
    "setup": (255, 149, 0), "general": (142, 142, 147), "context": (0, 122, 255),
    "personalization": (175, 82, 222), "emoji": (255, 204, 0), "shortcuts": (88, 86, 214),
    "battery": (52, 199, 89), "appSettings": (90, 200, 250), "labs": (255, 45, 85),
    "statistics": (31, 169, 122), "about": (142, 142, 147), "support": (0, 122, 255),
}
_CONSENT = {
    "screen_context": ("Use focused-window text for context?",
                       "BroTypist will read readable Accessibility text from the verified focused window only for eligible suggestion requests. Context is not saved or sent to a remote service. Protected content and excluded apps are skipped. You can turn this off at any time."),
    "clipboard_context": ("Use the clipboard for context?",
                          "BroTypist will read the clipboard only for eligible suggestion requests. Context is not saved or sent to a remote service. Protected content and excluded apps are skipped. You can turn this off at any time."),
    "screenshot_context": ("Use screenshots for context?",
                           "For eligible suggestion requests, BroTypist captures the focused window and recognizes its text on this Mac. The image is processed in memory and discarded immediately; it is never saved or uploaded. Accessibility text is still used first. macOS requires Screen & System Audio Recording permission and may show its recording indicator. You can turn this off at any time."),
    "screenshot_appearance": ("Use screenshots to improve suggestion appearance?",
                              "BroTypist captures the focused window on this Mac, then crops it in memory to measure the text’s font, size and baseline around the caret, so grey suggestions line up with your text. The image is discarded immediately; it is never saved or uploaded. Suggestions stay hidden when the measurement cannot be verified. macOS requires Screen & System Audio Recording permission and may show its recording indicator."),
}
_SCREENSHOT_FIELDS = ("screenshot_context", "screenshot_appearance")
_TERMINAL_HOSTS = {
    "terminal_automation": ("com.apple.Terminal", "Terminal"),
    "iterm_automation": ("com.googlecode.iterm2", "iTerm2"),
}
_AUTOMATION_LABELS = {
    "granted": "Granted", "denied": "Denied", "not-determined": "Not requested",
    "not-running": "Not running", "unavailable": "Unavailable",
}


def _canonical_pane(value):
    key = str(value or "")
    for pane, _title, _symbol in PANES:
        if key == pane or key.casefold() == pane.casefold():
            return pane
    return None


def _url_target(text):
    """Map brotypist://settings/<pane> (or brotypist://<pane>) to a pane; only navigation."""
    parts = urlsplit(str(text))
    if parts.scheme.casefold() != URL_SCHEME:
        return None
    segments = [segment for segment in [parts.netloc, *parts.path.split("/")] if segment]
    if not segments:
        return "general"
    if segments[-1].casefold() == "welcome":
        return "welcome"
    return _canonical_pane(segments[-1]) or "general"


def _gb(size_bytes):
    return f"{float(size_bytes or 0) / 1_000_000_000:.1f} GB"


def _minutes(seconds):
    value = float(seconds or 0) / 60
    return f"{value:.1f}" if 0 < value < 10 else f"{value:.0f}"


class FlippedView(A.NSView):
    def isFlipped(self):
        return True


class ShortcutRecorder(A.NSButton):
    @objc.python_method
    def configure(self, value, callback, recording_changed):
        self.value = value
        self.callback = callback
        self._recording_callback = recording_changed
        self._monitor = None
        self._monitor_handler = None
        self.recording = False
        self.setTitle_(value or "Record Shortcut…")
        self.setTarget_(self)
        self.setAction_("beginRecording:")
        self.setToolTip_("Click, then press a key combination. Escape cancels; Delete clears.")
    @objc.python_method
    def recording_changed(self, recording):
        if self._monitor is not None:
            A.NSEvent.removeMonitor_(self._monitor)
            self._monitor = None
            self._monitor_handler = None
        if recording:
            def capture(event):
                self.keyDown_(event)
                return None
            self._monitor_handler = capture
            self._monitor = A.NSEvent.addLocalMonitorForEventsMatchingMask_handler_(
                A.NSEventMaskKeyDown, capture)
        self._recording_callback(recording)


    def beginRecording_(self, sender):
        self.recording = not self.recording
        self.setTitle_("Type shortcut…" if self.recording else self.value or "Record Shortcut…")
        if self.recording:
            self.window().makeFirstResponder_(self)
        self.recording_changed(self.recording)

    def acceptsFirstResponder(self):
        return True

    def resignFirstResponder(self):
        self.recording = False
        self.recording_changed(False)
        self.setTitle_(self.value or "Record Shortcut…")
        return True

    def performKeyEquivalent_(self, event):
        if self.recording:
            self.keyDown_(event)
            return True
        return False

    def keyDown_(self, event):
        if not self.recording:
            objc.super(ShortcutRecorder, self).keyDown_(event)
            return
        code = int(event.keyCode())
        modifiers_mask = (A.NSEventModifierFlagControl | A.NSEventModifierFlagOption
                          | A.NSEventModifierFlagShift | A.NSEventModifierFlagCommand)
        if code == 53 and not event.modifierFlags() & modifiers_mask:
            self.recording = False
            self.recording_changed(False)
            self.setTitle_(self.value or "Record Shortcut…")
            return
        if code in (51, 117):
            value = ""
        else:
            flags = event.modifierFlags()
            modifiers = [name for name, mask in (
                ("Ctrl", A.NSEventModifierFlagControl), ("Alt", A.NSEventModifierFlagOption),
                ("Shift", A.NSEventModifierFlagShift), ("Super", A.NSEventModifierFlagCommand),
            ) if flags & mask]
            mapped = mac_key_name(code)
            key = mapped if len(mapped) > 1 else str(event.charactersIgnoringModifiers() or "")
            try:
                value = shortcut_from_event(key, modifiers, platform="macos", keycode=code)
                value = normalize_shortcut(value)
            except ValueError:
                A.NSBeep()
                return
        self.recording = False
        self.recording_changed(False)
        if self.callback(value):
            self.value = value
        self.setTitle_(self.value or "Record Shortcut…")


def _sidebar_icon(key, symbol_name, label):
    red, green, blue = _SIDEBAR_COLORS.get(key, (142, 142, 147))
    color = A.NSColor.colorWithSRGBRed_green_blue_alpha_(red / 255, green / 255, blue / 255, 1)
    symbol = (A.NSImage.imageWithSystemSymbolName_accessibilityDescription_(symbol_name, label)
              or A.NSImage.imageWithSystemSymbolName_accessibilityDescription_("circle", label))
    configuration = A.NSImageSymbolConfiguration.configurationWithPaletteColors_([A.NSColor.whiteColor()])
    symbol = symbol.imageWithSymbolConfiguration_(configuration)
    image = A.NSImage.alloc().initWithSize_(A.NSMakeSize(20, 20))
    image.lockFocus()
    try:
        color.setFill()
        A.NSBezierPath.bezierPathWithRoundedRect_xRadius_yRadius_(A.NSMakeRect(0, 0, 20, 20), 5, 5).fill()
        symbol.drawInRect_fromRect_operation_fraction_(A.NSMakeRect(3, 3, 14, 14), A.NSZeroRect, A.NSCompositingOperationSourceOver, 1)
    finally:
        image.unlockFocus()
    image.setTemplate_(False)
    return image


class TableSource(NSObject, protocols=[objc.protocolNamed("NSTableViewDataSource"), objc.protocolNamed("NSTableViewDelegate")]):
    @objc.python_method
    def configure(self, rows, columns, callback=None, sidebar=False, link=None):
        self.rows, self.columns = rows, columns
        self.callback, self.sidebar, self.link = callback, sidebar, link
        self.icons = {}
        self.badges = {}
        return self

    def numberOfRowsInTableView_(self, table):
        return len(self.rows)

    def tableView_objectValueForTableColumn_row_(self, table, column, row):
        return str(self.rows[row].get(str(column.identifier()), ""))

    def tableView_heightOfRow_(self, table, row):
        return 10.0 if self.sidebar and self.rows[row].get("kind") == "separator" else float(table.rowHeight())

    def tableView_shouldSelectRow_(self, table, row):
        if not self.sidebar:
            return True
        kind = self.rows[row].get("kind")
        # Open the external link on a click only, never while arrowing through the list.
        event = A.NSApplication.sharedApplication().currentEvent()
        clicked = event is not None and event.type() in (A.NSEventTypeLeftMouseDown, A.NSEventTypeLeftMouseUp)
        if kind == "link" and self.link and clicked:
            self.link(self.rows[row])
        return kind == "pane"

    def tableView_viewForTableColumn_row_(self, table, column, row):
        identifier = str(column.identifier())
        if self.sidebar and self.rows[row].get("kind") == "separator":
            return A.NSView.alloc().initWithFrame_(A.NSMakeRect(0, 0, column.width(), 10))
        cell = table.makeViewWithIdentifier_owner_(identifier, self)
        if cell is None:
            cell = A.NSTableCellView.alloc().initWithFrame_(A.NSMakeRect(0, 0, column.width(), 28))
            cell.setIdentifier_(identifier)
            offset = 30 if self.sidebar else 6
            label = A.NSTextField.labelWithString_("")
            label.setFrame_(A.NSMakeRect(offset, 5, column.width() - offset - (26 if self.sidebar else 5), 20))
            label.setFont_(A.NSFont.systemFontOfSize_(13))
            label.setAutoresizingMask_(A.NSViewWidthSizable)
            cell.setTextField_(label)
            cell.addSubview_(label)
            if self.sidebar:
                image = A.NSImageView.alloc().initWithFrame_(A.NSMakeRect(5, 5, 20, 20))
                cell.setImageView_(image)
                cell.addSubview_(image)
                badge = A.NSTextField.labelWithString_("●")
                badge.setFrame_(A.NSMakeRect(column.width() - 22, 6, 14, 18))
                badge.setTextColor_(A.NSColor.systemOrangeColor())
                badge.setFont_(A.NSFont.systemFontOfSize_(9))
                badge.setTag_(7)
                badge.setAccessibilityLabel_("Pending step")
                cell.addSubview_(badge)
        cell.textField().setStringValue_(str(self.rows[row].get(identifier, "")))
        if self.sidebar:
            row_data = self.rows[row]
            key = row_data["id"]
            if key not in self.icons:
                self.icons[key] = _sidebar_icon(key, row_data["symbol"], row_data["label"])
            cell.imageView().setImage_(self.icons[key])
            badge = cell.viewWithTag_(7)
            if badge is not None:
                badge.setHidden_(not self.badges.get(key))
            label = row_data["label"] + (" ↗" if row_data.get("kind") == "link" else "")
            cell.textField().setStringValue_(label)
            cell.setAccessibilityIdentifier_(f"inklow.sidebar.{key}")
            cell.textField().setAccessibilityIdentifier_(f"inklow.sidebar.{key}")
        return cell

    def tableViewSelectionDidChange_(self, notification):
        row = int(notification.object().selectedRow())
        if self.callback and 0 <= row < len(self.rows):
            self.callback(self.rows[row])


class RuleListSource(NSObject, protocols=[objc.protocolNamed("NSTableViewDataSource"), objc.protocolNamed("NSTableViewDelegate")]):
    """App Settings list: icons, summaries, selection and drag-to-add."""

    @objc.python_method
    def configure(self, shell):
        self.shell = shell
        self.rows = []
        return self

    def numberOfRowsInTableView_(self, table):
        return len(self.rows)

    def tableView_viewForTableColumn_row_(self, table, column, row):
        cell = table.makeViewWithIdentifier_owner_("rule", self)
        if cell is None:
            cell = A.NSTableCellView.alloc().initWithFrame_(A.NSMakeRect(0, 0, column.width(), 44))
            cell.setIdentifier_("rule")
            image = A.NSImageView.alloc().initWithFrame_(A.NSMakeRect(8, 7, 30, 30))
            cell.setImageView_(image)
            cell.addSubview_(image)
            title = A.NSTextField.labelWithString_("")
            title.setFrame_(A.NSMakeRect(46, 22, column.width() - 54, 17))
            title.setFont_(A.NSFont.systemFontOfSize_(13))
            title.setAutoresizingMask_(A.NSViewWidthSizable)
            title.setLineBreakMode_(A.NSLineBreakByTruncatingTail)
            cell.setTextField_(title)
            cell.addSubview_(title)
            summary = A.NSTextField.labelWithString_("")
            summary.setFrame_(A.NSMakeRect(46, 5, column.width() - 54, 15))
            summary.setFont_(A.NSFont.systemFontOfSize_(11))
            summary.setTextColor_(A.NSColor.secondaryLabelColor())
            summary.setAutoresizingMask_(A.NSViewWidthSizable)
            summary.setLineBreakMode_(A.NSLineBreakByTruncatingTail)
            summary.setTag_(2)
            cell.addSubview_(summary)
        data = self.rows[row]
        cell.imageView().setImage_(data["icon"])
        cell.textField().setStringValue_(data["title"])
        cell.viewWithTag_(2).setStringValue_(data["summary"])
        cell.setAccessibilityIdentifier_(f"inklow.appSettings.rule.{data['key']}")
        return cell

    def tableViewSelectionDidChange_(self, notification):
        row = int(notification.object().selectedRow())
        if getattr(self.shell, "rendering", False) or not 0 <= row < len(self.rows):
            return
        key = self.rows[row]["key"]
        if key != self.shell.selected_rule:
            # Rebuilding the pane replaces this table, so never do it inside its own event.
            self.shell._defer(lambda: self.shell._select_rule(key))

    @objc.python_method
    def _dropped(self, info):
        items = []
        urls = info.draggingPasteboard().readObjectsForClasses_options_([NSURL], {}) or []
        for url in urls:
            if url.isFileURL():
                path = str(url.path() or "")
                bundle = NSBundle.bundleWithPath_(path) if path.endswith(".app") else None
                identifier = str(bundle.bundleIdentifier() or "") if bundle else ""
                if identifier:
                    items.append((0, identifier))
            elif str(url.scheme() or "").casefold() in {"http", "https"} and url.host():
                items.append((1, str(url.host())))
        return items

    def tableView_validateDrop_proposedRow_proposedDropOperation_(self, table, info, row, operation):
        if not self._dropped(info):
            return A.NSDragOperationNone
        table.setDropRow_dropOperation_(-1, A.NSTableViewDropOn)
        return A.NSDragOperationCopy

    @objc.typedSelector(b"B@:@@qQ")
    def tableView_acceptDrop_row_dropOperation_(self, table, info, row, operation):
        items = self._dropped(info)
        if items:
            self.shell._defer(lambda: self.shell._add_rules(items))
        return bool(items)


class BarChartView(A.NSView):
    """Simple native bar chart for the selected statistics metric."""

    def isFlipped(self):
        return True

    @objc.python_method
    def set_bars(self, bars, formatter):
        self.bars = list(bars)
        self.formatter = formatter
        summary = "; ".join(f"{label}: {formatter(value)}" for label, value in self.bars[-12:])
        self.setAccessibilityLabel_("Statistics chart. " + (summary or "No accepted completions in this range."))
        self.setNeedsDisplay_(True)

    def drawRect_(self, dirty):
        bounds = self.bounds()
        width, height = A.NSWidth(bounds), A.NSHeight(bounds)
        A.NSColor.controlBackgroundColor().setFill()
        A.NSBezierPath.bezierPathWithRoundedRect_xRadius_yRadius_(bounds, 10, 10).fill()
        bars = getattr(self, "bars", [])
        small = {A.NSFontAttributeName: A.NSFont.systemFontOfSize_(10),
                 A.NSForegroundColorAttributeName: A.NSColor.secondaryLabelColor()}
        if not bars or not any(value for _label, value in bars):
            text = NSAttributedString.alloc().initWithString_attributes_("No accepted completions in this range", small)
            size = text.size()
            text.drawAtPoint_(A.NSMakePoint((width - size.width) / 2, (height - size.height) / 2))
            return
        top, bottom, left, right = 22, 26, 14, 14
        maximum = max(value for _label, value in bars) or 1
        slot = (width - left - right) / len(bars)
        bar_width = max(2.0, min(36.0, slot * 0.7))
        A.NSColor.controlAccentColor().setFill()
        for index, (_label, value) in enumerate(bars):
            bar_height = (height - top - bottom) * float(value) / maximum
            x = left + index * slot + (slot - bar_width) / 2
            if bar_height > 0:
                A.NSBezierPath.bezierPathWithRoundedRect_xRadius_yRadius_(
                    A.NSMakeRect(x, height - bottom - bar_height, bar_width, bar_height), 3, 3).fill()
        step = max(1, math.ceil(len(bars) / 8))
        for index in range(0, len(bars), step):
            text = NSAttributedString.alloc().initWithString_attributes_(str(bars[index][0])[:14], small)
            x = left + index * slot + slot / 2 - text.size().width / 2
            text.drawAtPoint_(A.NSMakePoint(max(2, min(width - text.size().width - 2, x)), height - bottom + 7))
        peak = NSAttributedString.alloc().initWithString_attributes_("Max " + self.formatter(maximum), small)
        peak.drawAtPoint_(A.NSMakePoint(left, 5))


def _label(text, frame, *, small=False, bold=False):
    label = A.NSTextField.wrappingLabelWithString_(str(text))
    label.setFrame_(frame)
    size = 11.5 if small else 13
    label.setFont_(A.NSFont.systemFontOfSize_weight_(size, A.NSFontWeightSemibold if bold else A.NSFontWeightRegular))
    if small:
        label.setTextColor_(A.NSColor.secondaryLabelColor())
    label.setSelectable_(False)
    return label


def _identify(control, identifier, label=None):
    control.setIdentifier_(identifier)
    control.setAccessibilityIdentifier_(identifier)
    if label:
        control.setAccessibilityLabel_(label)
    return control


class Form:
    """Small native grouped-form layout, using system colors and control metrics."""

    def __init__(self, shell, view, pane):
        self.shell, self.view, self.pane = shell, view, pane
        self.width = A.NSWidth(view.bounds()) - 48
        self.y = 24
        self.group_view = None
        self.group_y = 0
        self.row_y = 0

    def section(self, title=""):
        self.end_group()
        if title:
            self.view.addSubview_(_label(title.upper(), A.NSMakeRect(28, self.y, self.width - 8, 20), small=True, bold=True))
            self.y += 27
        box = A.NSBox.alloc().initWithFrame_(A.NSMakeRect(24, self.y, self.width, 10))
        box.setBoxType_(A.NSBoxCustom)
        box.setBorderType_(A.NSNoBorder)
        box.setFillColor_(A.NSColor.controlBackgroundColor())
        box.setCornerRadius_(10)
        box.setContentViewMargins_(A.NSMakeSize(0, 0))
        self.group_view = FlippedView.alloc().initWithFrame_(A.NSMakeRect(0, 0, self.width, 10))
        box.setContentView_(self.group_view)
        self.view.addSubview_(box)
        self.box, self.group_y, self.row_y = box, self.y, 0
        return self

    def end_group(self):
        if self.group_view is not None:
            self.box.setFrameSize_(A.NSMakeSize(self.width, self.row_y))
            self.group_view.setFrameSize_(A.NSMakeSize(self.width, self.row_y))
            self.y = self.group_y + self.row_y + 22
            self.group_view = None

    def row(self, title, detail="", control=None, *, height=None, control_width=220):
        if self.group_view is None:
            self.section()
        if height is None:
            height = 46 if not detail else 70 + max(0, len(detail) - 135) // 74 * 14
        if self.row_y:
            divider = A.NSBox.alloc().initWithFrame_(A.NSMakeRect(14, self.row_y, self.width - 28, 1))
            divider.setBoxType_(A.NSBoxSeparator)
            self.group_view.addSubview_(divider)
        title_width = self.width - 38 - control_width if control is not None else self.width - 28
        self.group_view.addSubview_(_label(title, A.NSMakeRect(14, self.row_y + 11, title_width, 20)))
        if detail:
            self.group_view.addSubview_(_label(detail, A.NSMakeRect(14, self.row_y + 34, self.width - 28, height - 39), small=True))
        if control is not None:
            control_height = min(46, height - 16) if isinstance(control, A.NSTextField) and not control.isEditable() else 27
            control.setFrame_(A.NSMakeRect(self.width - 14 - control_width, self.row_y + 8, control_width, control_height))
            self.group_view.addSubview_(control)
        self.row_y += height
        return control

    def note(self, text):
        self.end_group()
        height = 18 + max(1, (len(text) + 76) // 77) * 15
        self.view.addSubview_(_label(text, A.NSMakeRect(28, self.y, self.width - 8, height), small=True))
        self.y += height + 12

    def custom(self, view, height):
        self.end_group()
        view.setFrame_(A.NSMakeRect(24, self.y, self.width, height))
        self.view.addSubview_(view)
        self.y += height + 16

    def finish(self):
        self.end_group()
        self.view.setFrameSize_(A.NSMakeSize(self.width + 48, max(568, self.y)))


class BroTypistApplication(NSObject):
    @objc.python_method
    def configure(self, config, background, no_tray, pane=None):
        self.initial_config = config
        self.background, self.no_tray = background, no_tray
        self.controller = Controller(config)
        self.controller.show_settings = self.show_settings
        self.controller.set_clipboard_reader(system.clipboard_text)
        self.controller.set_screen_reader(system.screen_text)
        self.controller.set_battery_reader(system.on_battery)
        self.bindings = {}
        self.next_tag = 0
        self.controls = {}
        self.drafts = {}
        self.editors = {}
        self.model_inventory = []
        self.model_selection = None
        self.requested_pane = "welcome" if pane == "welcome" else _canonical_pane(pane)
        self.pane = self.requested_pane if self.requested_pane not in {None, "welcome"} else "general"
        self.last_external_app = ""
        self.app_names = {}
        self.app_icons = {}
        self.stat_period, self.stat_group, self.stat_metric = "week", "day", "completions"
        self.selected_rule = ""
        self.rule_query = ""
        self.history_counts = None
        self.last_status = None
        self.last_stats_refresh = 0.0
        self.last_setup_refresh = 0.0
        self.stopping = False
        self.updates = UpdateService()
        self.update_status_label = self.update_progress = None
        self.update_buttons = {}
        self.update_handoff = None
        self.update_committed = False
        self.termination_requested = False
        self.termination_waiting = False
        self.termination_drafts_approved = False
        self.shutdown_complete = False
        self.shutdown_next_retry = 0.0
        self.shutdown_error = ""
        self.startup_initialized = False
        self.startup_acknowledged = False
        self.startup_failed = False
        self.shutdown_started = False
        self._wake_lock = threading.Lock()
        self._wake_pending = False
        self.window = None
        self.timer = None
        self.status_item = None
        self.menu = None
        self.tracking_menus = set()
        self.pending_menu_actions = deque()
        self.menu_shortcuts_were_enabled = False
        self.resume_menu_shortcuts = False
        self.overlay = None
        self.docs_help_hint = None
        self.last_error = ""
        self.welcome = None
        self.status_images = {}
        self.model_description = self.download_progress = self.download_label = None
        return self

    def applicationDidFinishLaunching_(self, notification):
        app = A.NSApplication.sharedApplication()
        app.setActivationPolicy_(A.NSApplicationActivationPolicyAccessory)
        app.setApplicationIconImage_(A.NSImage.alloc().initWithContentsOfFile_(str(asset_path("app.icns"))))
        self._build_main_menu()
        self._build_status_menu()
        self.overlay = CaretOverlay(self.controller.invoke, self._accessory_menu,
                                    self.controller.config.show_accessory,
                                    on_present=self.controller.presentation_result)
        if not self.no_tray:
            try:
                self.status_item = A.NSStatusBar.systemStatusBar().statusItemWithLength_(A.NSSquareStatusItemLength)
                if self.status_item is None or self.status_item.button() is None:
                    raise RuntimeError("The macOS menu bar is unavailable. Settings will stay accessible instead.")
                self.status_item.setMenu_(self.menu)
                self.status_item.setAutosaveName_("com.inklow.Inklow")
                self.status_item.setBehavior_(0)
                self.status_item.setVisible_(True)
                _identify(self.status_item.button(), "inklow.statusItem", f"{APP_NAME}, active")
            except Exception as exc:
                self.no_tray = True
                self.last_error = str(exc)
        self.timer = NSTimer.timerWithTimeInterval_target_selector_userInfo_repeats_(0.025, self, "tick:", None, True)
        self.timer.setTolerance_(0.008)
        NSRunLoop.mainRunLoop().addTimer_forMode_(self.timer, NSRunLoopCommonModes)
        self.controller.set_wake_callback(self._wake_controller)
        self._refresh_status()
        self._refresh_updates()
        if self.controller.config.check_updates_automatically:
            self.updates.start_check(automatic=True)
        if self.requested_pane == "welcome" or (not self.requested_pane and (not self.controller.config.setup_completed or not self._model_ready())):
            self.show_welcome()
        elif self.requested_pane or self.no_tray or self.status_item is None:
            self.show_settings(self.pane)
        self.startup_initialized = True
        if self.last_error and (self.window or self.welcome):
            self._error(self.last_error)

    @objc.typedSelector(b"v@:@@")
    def application_openURLs_(self, application, urls):
        for url in urls or []:
            target = _url_target(str(url.absoluteString() or ""))
            if target is None:
                continue
            if self.overlay is None:
                # Launched by the URL: applicationDidFinishLaunching opens it.
                self.requested_pane = target
                if target != "welcome":
                    self.pane = target
            elif target == "welcome":
                self.show_welcome()
            else:
                self.show_settings(target)

    @objc.typedSelector(b"Z@:@Z")
    def applicationShouldHandleReopen_hasVisibleWindows_(self, application, visible):
        # Reopening the app (Finder, Spotlight, Dock) always recovers a window.
        if not visible and not self.stopping:
            if not self.controller.config.setup_completed:
                self.show_welcome()
            else:
                self.show_settings(self.pane)
        return True

    @objc.python_method
    def _bind(self, control, callback):
        self.next_tag += 1
        control.setTag_(self.next_tag)
        control.setTarget_(self)
        control.setAction_("perform:")
        self.bindings[self.next_tag] = callback
        return control

    def perform_(self, sender):
        callback = self.bindings.get(int(sender.tag()))
        if callback:
            if isinstance(sender, A.NSMenuItem):
                self.pending_menu_actions.append((callback, sender))
                return
            try:
                callback(sender)
            except Exception as exc:
                self._error(str(exc))

    @objc.python_method
    def _defer(self, callback):
        """Run after the current AppKit event, for actions that rebuild their own view."""
        def run(sender):
            try:
                callback()
            except Exception as exc:
                self._error(str(exc))
        self.pending_menu_actions.append((run, None))

    @objc.python_method
    def _button(self, title, identifier, callback):
        button = A.NSButton.buttonWithTitle_target_action_(title, None, None)
        _identify(button, identifier, title)
        return self._bind(button, lambda sender: callback())

    @objc.python_method
    def _open_url(self, url):
        A.NSWorkspace.sharedWorkspace().openURL_(NSURL.URLWithString_(url))

    @objc.python_method
    def _menu_item(self, menu, title, identifier, callback=None, key=""):
        item = A.NSMenuItem.alloc().initWithTitle_action_keyEquivalent_(title, None, key)
        item.setIdentifier_(identifier)
        if callback:
            self._bind(item, lambda sender: callback())
        menu.addItem_(item)
        return item

    @objc.python_method
    def _build_main_menu(self):
        main = A.NSMenu.alloc().initWithTitle_(APP_NAME)
        root = self._menu_item(main, APP_NAME, "inklow.appMenu")
        application = A.NSMenu.alloc().initWithTitle_(APP_NAME)
        self._menu_item(application, "Settings…", "inklow.main.settings", lambda: self.show_settings("general"), ",")
        application.addItem_(A.NSMenuItem.separatorItem())
        self._menu_item(application, f"Quit {APP_NAME}", "inklow.main.quit", lambda: A.NSApplication.sharedApplication().terminate_(None), "q")
        root.setSubmenu_(application)
        edit = self._menu_item(main, "Edit", "inklow.editMenu")
        edits = A.NSMenu.alloc().initWithTitle_("Edit")
        for title, selector, key in (("Undo", "undo:", "z"), ("Cut", "cut:", "x"), ("Copy", "copy:", "c"), ("Paste", "paste:", "v"), ("Select All", "selectAll:", "a")):
            item = A.NSMenuItem.alloc().initWithTitle_action_keyEquivalent_(title, selector, key)
            edits.addItem_(item)
        edit.setSubmenu_(edits)
        navigation = self._menu_item(main, "View", "inklow.viewMenu")
        destinations = A.NSMenu.alloc().initWithTitle_("View")
        for index, (pane, title, symbol) in enumerate(PANES, 1):
            self._menu_item(destinations, title, f"inklow.navigation.{pane}",
                            lambda p=pane: self.show_settings(p), str(index) if index <= 9 else "")
        navigation.setSubmenu_(destinations)
        close = A.NSMenuItem.alloc().initWithTitle_action_keyEquivalent_("Close Window", "performClose:", "w")
        application.addItem_(close)
        A.NSApplication.sharedApplication().setMainMenu_(main)
        self.main_menu = main

    @objc.python_method
    def _build_status_menu(self):
        self.menu = A.NSMenu.alloc().initWithTitle_(APP_NAME)
        self.menu.setAutoenablesItems_(False)
        self.menu.setDelegate_(self)
        header = A.NSMenuItem.alloc().initWithTitle_action_keyEquivalent_(APP_NAME, None, "")
        panel = FlippedView.alloc().initWithFrame_(A.NSMakeRect(0, 0, 300, 150))
        panel.setAccessibilityLabel_(f"{APP_NAME} status and local model")
        icon = A.NSImageView.alloc().initWithFrame_(A.NSMakeRect(14, 12, 28, 28))
        icon.setImage_(A.NSImage.alloc().initWithContentsOfFile_(str(asset_path("app.icns"))))
        panel.addSubview_(icon)
        panel.addSubview_(_label(APP_NAME, A.NSMakeRect(52, 10, 160, 20), bold=True))
        self.menu_badge = _label("", A.NSMakeRect(180, 12, 110, 20), small=True)
        self.menu_subtitle = _label("", A.NSMakeRect(52, 33, 234, 31), small=True)
        model_card = A.NSBox.alloc().initWithFrame_(A.NSMakeRect(8, 70, 284, 76))
        model_card.setBoxType_(A.NSBoxCustom)
        model_card.setCornerRadius_(8)
        model_card.setBorderWidth_(2 if A.NSWorkspace.sharedWorkspace().accessibilityDisplayShouldIncreaseContrast() else 1)
        model_card.setBorderColor_(A.NSColor.separatorColor())
        model_card.setFillColor_(A.NSColor.controlBackgroundColor())
        panel.addSubview_(model_card)
        self.menu_model = _label("", A.NSMakeRect(18, 78, 264, 36))
        self.menu_model.setFont_(A.NSFont.monospacedSystemFontOfSize_weight_(12, A.NSFontWeightMedium))
        self.menu_backend = _label("", A.NSMakeRect(18, 116, 264, 25), small=True)
        for label in (self.menu_badge, self.menu_subtitle, self.menu_model, self.menu_backend):
            panel.addSubview_(label)
        header.setView_(panel)
        self.menu.addItem_(header)
        self.status_line = self._menu_item(self.menu, APP_NAME, "inklow.menu.status")
        self.status_line.setEnabled_(False)
        self.secure_item = self._menu_item(self.menu, "Secure Input is on — shortcuts can’t be detected", "inklow.menu.secureInput", lambda: self.show_settings("shortcuts"))
        self.secure_item.setToolTip_("Another app turned on macOS Secure Input (usually a password or terminal field). macOS hides typing from BroTypist until it ends.")
        self.download_item = self._menu_item(self.menu, "Downloading model…", "inklow.menu.download", lambda: self.show_settings("general"))
        self.attention_item = self._menu_item(self.menu, "Open Setup", "inklow.menu.attention", lambda: self.show_settings("setup"))
        self.pause_item = self._menu_item(self.menu, "Predictions", "inklow.menu.toggle", self.controller.toggle_enabled)
        self.resume_item = self._menu_item(self.menu, "Resume now", "inklow.menu.resume", self.controller.toggle_enabled)
        self.hour_item = self._menu_item(self.menu, "Pause for 1 hour", "inklow.menu.pause.60", lambda: self.controller.pause(60))
        self.tomorrow_item = self._menu_item(self.menu, "Pause until tomorrow", "inklow.menu.pause.tomorrow", self._pause_until_tomorrow)
        self.menu.addItem_(A.NSMenuItem.separatorItem())
        self.current_item = self._menu_item(self.menu, "Turn off in Current App", "inklow.menu.currentApp", self.controller.toggle_current_app)
        self.record_item = self._menu_item(self.menu, "Record Writing in Current App", "inklow.menu.record", lambda: self._toggle_scope_from_status("record_history"))
        self.tab_item = self._menu_item(self.menu, "Accept with Tab in Current App", "inklow.menu.tab", lambda: self._toggle_scope_from_status("tab_enabled"))
        self._menu_item(self.menu, "Suggest Now", "inklow.menu.force", self.controller.force)
        self.menu.addItem_(A.NSMenuItem.separatorItem())
        self.stats_line = self._menu_item(self.menu, "Today", "inklow.menu.stats", lambda: self.show_settings("statistics"))
        self._menu_item(self.menu, "Settings…", "inklow.menu.settings", lambda: self.show_settings("general"), ",")
        self.update_item = self._menu_item(self.menu, "Check for Updates…", "inklow.menu.checkUpdates", self._check_updates)
        self._menu_item(self.menu, f"Quit {APP_NAME}", "inklow.menu.quit", lambda: A.NSApplication.sharedApplication().terminate_(None), "q")
        self._build_field_menu()
        self.menu_binding_limit = self.next_tag

    @objc.python_method
    def _pause_until_tomorrow(self):
        from datetime import datetime, timedelta
        now = datetime.now().astimezone()
        tomorrow = (now + timedelta(days=1)).replace(hour=0, minute=0, second=0, microsecond=0)
        self.controller.pause(max(1, int((tomorrow - now).total_seconds() / 60)))

    @objc.python_method
    def _build_field_menu(self):
        # Compact: only the current app's controls and Settings. Option-click on
        # the accessory sends a plain Tab (owned by the overlay, not this menu).
        self.field_menu = A.NSMenu.alloc().initWithTitle_("")
        self.field_menu.setAutoenablesItems_(False)
        self.field_menu.setDelegate_(self)
        # These bindings live with the application, like the status menu's.
        # Opening a field menu only replaces their captured context, not tags.
        self.field_app_item = self._menu_item(
            self.field_menu, "", "inklow.field.currentApp", self.controller.toggle_current_app)
        self.field_record_item = self._menu_item(
            self.field_menu, "", "inklow.field.record", lambda: self._toggle_scope_from_status("record_history"))
        self.field_tab_item = self._menu_item(
            self.field_menu, "", "inklow.field.tab", lambda: self._toggle_scope_from_status("tab_enabled"))
        self.field_docs_item = self._menu_item(
            self.field_menu, "Google Docs accessibility setup…", "inklow.field.docsAccessibility",
            lambda: self._enable_docs_accessibility(None))
        self.field_docs_item.setHidden_(True)
        self.field_menu.addItem_(A.NSMenuItem.separatorItem())
        self._menu_item(self.field_menu, "Settings…", "inklow.field.settings",
                        lambda: self.show_settings("general"))

    @objc.python_method
    def _scope_key(self, app, domain):
        """The rule a current-app control edits: the website when known, else the app."""
        if domain:
            from .config import normalize_domain
            try:
                return "domain:" + normalize_domain(domain)
            except ValueError:
                return ""
        return f"app:{app}" if app else ""

    @objc.python_method
    def _scope_name(self, app, domain, limit=30):
        name = domain or self._app_name(app) or "Current App"
        return name[:limit - 1] + "…" if len(name) > limit else name

    @objc.python_method
    def _refresh_scope_items(self, record_item, tab_item, status):
        app = str(status.get("current_app") or "")
        domain = str(status.get("current_domain") or "")
        scope = self._scope_key(app, domain) if app else ""
        name = self._scope_name(app, domain)
        recording = bool(status.get("current_recording"))
        tab = bool(status.get("current_tab_enabled", True))
        record_item.setTitle_(f"Record Writing in {name}")
        record_item.setState_(A.NSControlStateValueOn if recording and scope else A.NSControlStateValueOff)
        record_item.setEnabled_(bool(scope))
        record_item.setToolTip_("Saved in App Settings. Turning recording on asks for confirmation first.")
        self.bindings[int(record_item.tag())] = (
            lambda sender: self._set_scope_rule(scope, "record_history", not recording))
        tab_item.setTitle_(f"Accept with Tab in {name}")
        tab_item.setState_(A.NSControlStateValueOn if tab and scope else A.NSControlStateValueOff)
        tab_item.setEnabled_(bool(scope))
        tab_item.setToolTip_("Saved in App Settings. Other acceptance shortcuts keep working.")
        self.bindings[int(tab_item.tag())] = (
            lambda sender: self._set_scope_rule(scope, "tab_enabled", not tab))

    @objc.python_method
    def _toggle_scope_from_status(self, field):
        status = self.controller.status()
        app = str(status.get("current_app") or "")
        scope = self._scope_key(app, str(status.get("current_domain") or "")) if app else ""
        current = bool(status.get("current_recording")) if field == "record_history" else bool(status.get("current_tab_enabled", True))
        self._set_scope_rule(scope, field, not current)

    @objc.python_method
    def _set_scope_rule(self, scope, field, value):
        if not scope:
            raise ValueError("Switch to an app or website first.")
        if field == "record_history" and value and not self._confirm_recording():
            return False
        rules = deepcopy(self.controller.config.app_rules)
        rules.setdefault(scope, {})[field] = bool(value)
        self._change(app_rules=rules)
        if self.window and self.window.isVisible() and self.pane == "appSettings":
            self._show_pane("appSettings")
        return True

    @objc.python_method
    def _refresh_field_menu(self):
        status = self.controller.status()
        app = str(status.get("current_app") or "")
        domain = str(status.get("current_domain") or "")
        allowed = bool(status.get("current_app_enabled", self.controller.config.enabled))
        name = self._scope_name(app, "")
        remaining = status.get("temporary_remaining")
        suffix = f" ({max(1, math.ceil(float(remaining) / 60))} min left)" if remaining and status.get("temporary_scope") else ""
        self.field_app_item.setTitle_(f"Completions in {name}{suffix}")
        self.field_app_item.setState_(A.NSControlStateValueOn if allowed and app else A.NSControlStateValueOff)
        self.field_app_item.setEnabled_(bool(app))
        self.field_app_item.setToolTip_("Turns completions off or on here for a few minutes. Saved rules are in App Settings.")
        self.bindings[int(self.field_app_item.tag())] = (
            lambda sender: self.controller.toggle_current_app(app=app, domain=domain))
        self._refresh_scope_items(self.field_record_item, self.field_tab_item, status)
        hint = getattr(self, "docs_help_hint", None)
        self.field_docs_item.setHidden_(hint is None)
        self.field_docs_item.setEnabled_(hint is not None)
        token = hint["token"] if hint else None
        self.bindings[int(self.field_docs_item.tag())] = (
            lambda sender: self._enable_docs_accessibility(token))

    @objc.python_method
    def _enable_docs_accessibility(self, token):
        result = self.controller.enable_docs_accessibility(token)
        status = result.get("status", "unknown")
        alert = A.NSAlert.alloc().init()
        if status in {"enabled", "already_enabled"}:
            alert.setMessageText_("Google Docs accessibility is enabled")
            alert.setInformativeText_(
                "Google Docs reported its accessibility setting as enabled. "
                "BroTypist still needs a readable field and verified inline caret before suggesting; "
                "this is not confirmation that completion works in this document.")
        else:
            alert.setMessageText_("Google Docs accessibility could not be confirmed")
            alert.setInformativeText_(
                "Automatic setup is unavailable or the document/focus changed. "
                "In Google Docs, open Tools → Accessibility, enable both screen reader "
                "support and braille support, then click OK. "
                "BroTypist has not confirmed completion support in this document.")
        alert.addButtonWithTitle_("OK")
        alert.runModal()

    @objc.python_method
    def _accessory_menu(self):
        if self.field_menu not in self.tracking_menus:
            self._refresh_field_menu()
        return self.field_menu

    def menuWillOpen_(self, menu):
        if not self.shutdown_started and not self.tracking_menus and not self.resume_menu_shortcuts:
            self.menu_shortcuts_were_enabled = bool(self.controller.bridge._enabled)
        self.tracking_menus.add(menu)
        if not self.shutdown_started:
            self.controller.bridge.set_shortcuts_enabled(False)
        try:
            if menu == getattr(self, "field_menu", None):
                self._refresh_field_menu()
            else:
                self._refresh_status()
                self._refresh_menu_stats()
        except Exception as exc:
            self.status_line.setTitle_((str(exc)[:41] + "…") if len(str(exc)) > 42 else str(exc))

    def menuDidClose_(self, menu):
        self.tracking_menus.discard(menu)
        if not self.tracking_menus and not self.shutdown_started:
            self.resume_menu_shortcuts = True

    @objc.python_method
    def _wake_controller(self):
        with self._wake_lock:
            if self.stopping or self._wake_pending:
                return
            self._wake_pending = True
        try:
            with objc.autorelease_pool():
                self.performSelectorOnMainThread_withObject_waitUntilDone_("wakeTick:", None, False)
        except Exception:
            with self._wake_lock:
                self._wake_pending = False
            raise

    def wakeTick_(self, sender):
        # Only the queued wake acknowledges this flag. Timer ticks must not
        # reopen the queue while an earlier selector is still waiting.
        with self._wake_lock:
            self._wake_pending = False
        self.tick_(None)

    def tick_(self, timer):
        if self.stopping or self.tracking_menus:
            return
        if not self.startup_initialized:
            return
        if self.termination_requested:
            self._advance_shutdown()
            return
        if self.startup_failed:
            return
        if not self.startup_acknowledged:
            # This is the first real event-loop turn after native UI setup,
            # before controller/welcome ticks can start model subprocesses.
            try:
                expected_ack = bool(os.environ.get(HEALTH_PATH) or os.environ.get(HEALTH_NONCE))
                if not acknowledge_startup() and expected_ack:
                    raise RuntimeError("The updated app did not acknowledge startup.")
            except Exception:
                self.last_error = "The updated app could not confirm a healthy startup. Quitting without starting local inference."
                self.startup_failed = True
                A.NSApplication.sharedApplication().terminate_(None)
                return
            self.startup_acknowledged = True
            try:
                self.controller.start()
            except Exception as exc:
                self.last_error = str(exc)
                self._error(self.last_error)
        try:
            while self.pending_menu_actions:
                callback, sender = self.pending_menu_actions.popleft()
                callback(sender)
                if self.stopping or self.termination_requested:
                    return
            self.controller.tick()
            if self.resume_menu_shortcuts:
                self.resume_menu_shortcuts = False
                if not self.shutdown_started:
                    self.controller.bridge.set_shortcuts_enabled(self.menu_shortcuts_were_enabled)
            for event in self.controller.drain_events():
                kind = event.get("type")
                if self.overlay is None and kind in {"suggestion", "caret", "docs_help", "clear"}:
                    continue
                if kind == "suggestion":
                    self.overlay.present(event)
                elif kind == "caret":
                    self.overlay.caret(event.get("rect"), event.get("field_rect"))
                elif kind == "docs_help":
                    self.docs_help_hint = event.get("hint")
                    self.overlay.docs_help(self.docs_help_hint)
                elif kind == "clear":
                    self.overlay.clear(hide_accessory=bool(event.get("hide_accessory")), panel=event.get("panel"))
                elif kind == "settings":
                    self.show_settings(event.get("pane", "general"))
            now = time.monotonic()
            if now - getattr(self, "_last_status_refresh", 0) >= 0.1:
                self._last_status_refresh = now
                if not self.shutdown_started and self.controller.config.check_updates_automatically:
                    self.updates.start_check(automatic=True)
                self._refresh_updates()
                self._refresh_status()
            if self.welcome and not self.shutdown_started:
                self.welcome.tick()
            if self.window and self.window.isVisible() and self.pane == "statistics" and time.monotonic() - self.last_stats_refresh > 5:
                self._refresh_statistics()
        except Exception as exc:
            message = str(exc)
            if message != self.last_error:
                self.last_error = message
                self.status_line.setTitle_((message[:41] + "…") if len(message) > 42 else message)
                self.status_line.setToolTip_(message)

    @objc.python_method
    def _refresh_menu_stats(self):
        stats = self.controller.stats("week", "day")
        today = stats.get("today", {})
        self.stats_line.setTitle_(f"Today: {int(today.get('completions', 0))} completions · {int(today.get('words', 0))} words")

    @objc.python_method
    def _model_ready(self):
        config = self.controller.config
        return config.backend != "gguf" or bool(config.model_path and Path(config.model_path).is_file())

    @objc.python_method
    def _refresh_status(self):
        status = self.controller.status()
        enabled = bool(status.get("enabled"))
        state = str(status.get("state", "idle"))
        message = str(status.get("message") or state.capitalize())
        if self.shutdown_error:
            enabled, state, message = False, "error", self.shutdown_error
        if enabled and state in {"idle", "ready"} and not status.get("inline_available", True):
            message = str(status.get("inline_message") or message)
        if enabled:
            permissions = self._permission_state()
            if not permissions["accessibility"]:
                state, message = "permission", "Accessibility permission required — open Setup"
            elif not permissions["input_monitoring"]:
                state, message = "permission", "Input Monitoring required for acceptance shortcuts — open Setup"
            if state != "permission" and not self._model_ready():
                state, message = "empty", "No model installed — choose a model in General"
        status = {**status, "state": state, "message": message}
        self.status_line.setTitle_((message[:41] + "…") if len(message) > 42 else message)
        self.status_line.setToolTip_(message)
        paused = float(status.get("paused_until", 0)) > time.monotonic()
        loading = state in {"loading", "starting", "warming"}
        needs_attention = state in {"error", "permission", "empty"}
        unavailable = enabled and not paused and not loading and not needs_attention and not status.get("inline_available", True)
        icon_state = "off" if needs_attention else ("loading" if loading and enabled else ("paused" if paused else ("active" if enabled else "off")))
        badge = "▲ Needs attention" if needs_attention else ("◌ Loading" if loading and enabled else (
            "■ Paused" if paused else ("Unavailable here" if unavailable else ("● Active" if enabled else "○ Off"))))
        self.pause_item.setTitle_("Enable predictions")
        self.pause_item.setState_(A.NSControlStateValueOn if self.controller.config.global_enabled else A.NSControlStateValueOff)
        self.resume_item.setHidden_(not paused)
        self.hour_item.setHidden_(paused or not enabled)
        self.tomorrow_item.setHidden_(paused or not enabled)
        self.attention_item.setHidden_(not needs_attention)
        self.secure_item.setHidden_(not status.get("secure_input"))
        download = status.get("download") or {}
        downloading = download.get("state") == "downloading"
        self.download_item.setHidden_(not downloading)
        if downloading:
            done = float(download.get("received", 0) or 0)
            total = float(download.get("total", 0) or 0)
            self.download_item.setTitle_(f"Downloading model… {int(done * 100 / total)}%" if total else "Downloading model…")
        self.menu_badge.setStringValue_(badge)
        color = A.NSColor.labelColor()
        if needs_attention:
            color = A.NSColor.systemRedColor()
        elif paused:
            color = _rgb("F0B65A" if _dark(self.menu_badge) else "8A5A00")
        elif loading and enabled:
            color = A.NSColor.systemBlueColor()
        elif enabled and not unavailable:
            color = _rgb("6FDDB2" if _dark(self.menu_badge) else "0B6B4C")
        self.menu_badge.setTextColor_(color)
        model_name = self._model_name()
        self.menu_model.setStringValue_(model_name)
        backend = {"gguf": "On-device · GGUF / llama.cpp", "ollama": "Local loopback · Ollama", "openai": "Local loopback · OpenAI-compatible"}[self.controller.config.backend]
        self.menu_backend.setStringValue_(backend + (" · worker ready" if status.get("model_loaded") else " · on demand"))
        subtitle = message
        if paused:
            from datetime import datetime, timedelta
            resumes = datetime.now() + timedelta(seconds=max(0, float(status["paused_until"]) - time.monotonic()))
            subtitle = f"Resumes at {resumes:%H:%M}"
        elif enabled and not needs_attention and not loading and status.get("inline_available", True):
            subtitle = f"Predicting in {self._app_name(str(status.get('current_app') or '')) or 'eligible text fields'}"
        self.menu_subtitle.setStringValue_(subtitle)
        app = str(status.get("current_app", ""))
        if app:
            self.last_external_app = app
        active = bool(status.get("current_app_enabled", self.controller.config.enabled))
        display_name = self._scope_name(app, "", 24)
        self.current_item.setTitle_(("Pause" if active else "Resume") + f" in {display_name}")
        self.current_item.setToolTip_("Temporarily toggles completions here for a few minutes. Saved rules are in App Settings.")
        self.current_item.setEnabled_(bool(app))
        self.current_item.setState_(A.NSControlStateValueOn if active and app else A.NSControlStateValueOff)
        self._refresh_scope_items(self.record_item, self.tab_item, status)
        if self.status_item and not self.status_item.isVisible():
            self.status_item.setVisible_(True)
        frame = int(time.monotonic() / 0.6) % 3 if icon_state == "loading" and not A.NSWorkspace.sharedWorkspace().accessibilityDisplayShouldReduceMotion() else -1
        if self.status_item and (self.last_status is None or self.last_status.get("icon_state") != icon_state or self.last_status.get("icon_frame") != frame):
            self.status_item.button().setImage_(self._status_image(icon_state, frame))
        if self.status_item:
            spoken = f"{APP_NAME}, {subtitle if paused else badge.lstrip('▲◌■●○ ')}"
            self.status_item.button().setAccessibilityLabel_(spoken)
            self.status_item.button().setToolTip_(f"{APP_NAME} — {subtitle}")
        status["icon_state"] = icon_state
        status["icon_frame"] = frame
        if hasattr(self, "sidebar_status"):
            self.sidebar_status.setStringValue_(f"{badge} · {model_name}" if enabled else badge)
        if self.overlay:
            self.overlay.configure(self.controller.config.show_accessory, self.controller.config.shortcuts.get("all", ""))
        for field in ("enabled", "global_enabled"):
            control = self.controls.get(field)
            if control:
                control.setState_(A.NSControlStateValueOn if getattr(self.controller.config, field) else A.NSControlStateValueOff)
        self.last_status = status
        if self.window and self.window.isVisible():
            self._refresh_live_pane(status)

    @objc.python_method
    def _refresh_live_pane(self, status):
        self._refresh_setup()
        if self.pane == "general":
            self._refresh_download(status)
        elif self.pane == "context":
            self._refresh_context(status)
        elif self.pane == "shortcuts":
            self._refresh_secure_input(status)
        elif self.pane == "battery":
            self._refresh_power(status)

    @objc.python_method
    def _model_name(self):
        config = self.controller.config
        return Path(config.model_path).name if config.backend == "gguf" and config.model_path else (config.model if config.backend != "gguf" else "No local model selected")

    @objc.python_method
    def _app_location(self, identifier):
        url = A.NSWorkspace.sharedWorkspace().URLForApplicationWithBundleIdentifier_(identifier) if identifier else None
        return str(url.path()) if url is not None else ""

    @objc.python_method
    def _app_name(self, identifier):
        if not identifier:
            return ""
        if identifier not in self.app_names:
            applications = A.NSRunningApplication.runningApplicationsWithBundleIdentifier_(identifier)
            name = str(applications[0].localizedName() or "") if applications else ""
            if not name:
                location = self._app_location(identifier)
                name = str(A.NSFileManager.defaultManager().displayNameAtPath_(location) or "") if location else ""
                name = name[:-4] if name.endswith(".app") else name
            self.app_names[identifier] = name or identifier
        return self.app_names[identifier]

    @objc.python_method
    def _rule_icon(self, key):
        if key not in self.app_icons:
            location = self._app_location(key[4:]) if key.startswith("app:") else ""
            if location:
                image = A.NSWorkspace.sharedWorkspace().iconForFile_(location)
            else:
                symbol = "globe" if key.startswith("domain:") else "app.dashed"
                image = A.NSImage.imageWithSystemSymbolName_accessibilityDescription_(symbol, None)
            image.setSize_(A.NSMakeSize(30, 30))
            self.app_icons[key] = image
        return self.app_icons[key]

    @objc.python_method
    def _status_image(self, state, phase):
        key = (state, phase)
        if key not in self.status_images:
            if state != "loading" or phase < 0:
                image = A.NSImage.alloc().initWithContentsOfFile_(str(asset_path(f"tray/{state}-template-18.png")))
                image.setSize_(A.NSMakeSize(18, 16))
            else:
                image = A.NSImage.alloc().initWithSize_(A.NSMakeSize(18, 16))
                image.lockFocus()
                A.NSColor.blackColor().setFill()
                A.NSBezierPath.bezierPathWithRoundedRect_xRadius_yRadius_(A.NSMakeRect(2, 2, 2.4, 12), 1.2, 1.2).fill()
                for index, x in enumerate((8, 11.5, 15)):
                    A.NSColor.blackColor().colorWithAlphaComponent_((1, 0.6, 0.3)[(index - phase) % 3]).setFill()
                    A.NSBezierPath.bezierPathWithOvalInRect_(A.NSMakeRect(x - 1.3, 6.7, 2.6, 2.6)).fill()
                image.unlockFocus()
            image.setTemplate_(True)
            self.status_images[key] = image
        return self.status_images[key]

    @objc.python_method
    def show_welcome(self):
        if self.shutdown_started:
            self.show_settings("about")
            return
        from .appkit_welcome import WelcomeWindow
        if self.welcome is None:
            self.welcome = WelcomeWindow.alloc().init().configure(self)
        self.welcome.show()

    @objc.python_method
    def _error(self, message):
        alert = A.NSAlert.alloc().init()
        alert.setMessageText_(f"{APP_NAME} couldn’t complete that change")
        alert.setInformativeText_(str(message))
        alert.addButtonWithTitle_("OK")
        alert.setAlertStyle_(A.NSAlertStyleWarning)
        A.NSApplication.sharedApplication().activateIgnoringOtherApps_(True)
        alert.runModal()

    @objc.python_method
    def _confirm(self, title, message):
        alert = A.NSAlert.alloc().init()
        alert.setMessageText_(title)
        alert.setInformativeText_(message)
        alert.addButtonWithTitle_("Continue")
        alert.addButtonWithTitle_("Cancel")
        A.NSApplication.sharedApplication().activateIgnoringOtherApps_(True)
        return alert.runModal() == A.NSAlertFirstButtonReturn

    @objc.python_method
    def _confirm_recording(self):
        """The one consent dialog for every way of turning on writing history."""
        return self._confirm("Enable encrypted local learning?", "Eligible writing will be stored encrypted on this Mac. New observed edits can teach local style and vocabulary; existing drafts, bulk pastes and BroTypist-generated suggestions do not establish authorship. Your OS secure storage must be available. Nothing is uploaded and there is no plaintext fallback.")

    @objc.python_method
    def _change(self, **changes):
        candidate = replace(self.controller.config, **changes)
        candidate.validate()
        self.controller.update_config(candidate)
        return True

    @objc.python_method
    def _switch(self, field, title, detail="", *, callback=None):
        control = A.NSSwitch.alloc().initWithFrame_(A.NSMakeRect(0, 0, 42, 25))
        control.setState_(A.NSControlStateValueOn if getattr(self.controller.config, field) else A.NSControlStateValueOff)
        _identify(control, f"inklow.{self.pane}.{field}", title)
        def changed(sender):
            value = bool(sender.state())
            try:
                if callback:
                    callback(value)
                else:
                    self._change(**{field: value})
            except Exception:
                actual = system.launch_at_login_state() if field == "launch_at_login" else getattr(self.controller.config, field)
                sender.setState_(A.NSControlStateValueOn if actual else A.NSControlStateValueOff)
                raise
        self._bind(control, changed)
        self.controls[field] = control
        self.form.row(title, detail, control, control_width=42)
        return control

    @objc.python_method
    def _popup(self, field, title, options, detail="", *, value=None, callback=None):
        control = A.NSPopUpButton.alloc().initWithFrame_pullsDown_(A.NSMakeRect(0, 0, 220, 26), False)
        for label, item_value in options:
            control.addItemWithTitle_(label)
        current = getattr(self.controller.config, field) if value is None else value
        values = [v for label, v in options]
        if current in values:
            control.selectItemAtIndex_(values.index(current))
        _identify(control, f"inklow.{self.pane}.{field}", title)
        def changed(sender):
            selected = values[int(sender.indexOfSelectedItem())]
            try:
                if callback:
                    callback(selected)
                else:
                    self._change(**{field: selected})
            except Exception:
                actual = getattr(self.controller.config, field, current)
                if actual in values:
                    sender.selectItemAtIndex_(values.index(actual))
                raise
        self._bind(control, changed)
        self.controls[field] = control
        self.form.row(title, detail, control)
        return control

    @objc.python_method
    def _number(self, field, title, detail, minimum, maximum):
        control = A.NSTextField.alloc().initWithFrame_(A.NSMakeRect(0, 0, 100, 25))
        control.setStringValue_(str(getattr(self.controller.config, field)))
        _identify(control, f"inklow.{self.pane}.{field}", title)
        def changed(sender):
            try:
                value = int(str(sender.stringValue()).strip())
                if not minimum <= value <= maximum:
                    raise ValueError(f"Enter a whole number from {minimum} to {maximum}.")
                self._change(**{field: value})
            except Exception:
                sender.setStringValue_(str(getattr(self.controller.config, field)))
                raise
        self._bind(control, changed)
        self.controls[field] = control
        self.form.row(title, detail, control, control_width=100)
        return control

    @objc.python_method
    def show_settings(self, pane="general"):
        if pane == "welcome":
            self.show_welcome()
            return
        if self.overlay:
            self.overlay.clear(hide_accessory=True)
        pane = _canonical_pane(pane) or "general"
        if self.window is None:
            self._build_window()
        self._show_pane(pane)
        self.window.makeKeyAndOrderFront_(None)
        A.NSApplication.sharedApplication().activateIgnoringOtherApps_(True)

    @objc.python_method
    def _sidebar_rows(self):
        titles = {key: (title, symbol) for key, title, symbol in PANES}
        titles["support"] = ("Support", "questionmark.bubble")
        rows = []
        for index, group in enumerate(SIDEBAR_GROUPS):
            if index:
                rows.append({"id": f"separator{index}", "kind": "separator", "label": "", "symbol": ""})
            for key in group:
                title, symbol = titles[key]
                rows.append({"id": key, "label": title, "symbol": symbol,
                             "kind": "link" if key == "support" else "pane"})
        return rows

    @objc.python_method
    def _build_window(self):
        style = A.NSWindowStyleMaskTitled | A.NSWindowStyleMaskClosable | A.NSWindowStyleMaskMiniaturizable | A.NSWindowStyleMaskFullSizeContentView
        self.window = A.NSWindow.alloc().initWithContentRect_styleMask_backing_defer_(A.NSMakeRect(0, 0, 900, 640), style, A.NSBackingStoreBuffered, False)
        self.window.setTitle_(f"{APP_NAME} Settings")
        self.window.setIdentifier_("inklow.settings")
        self.window.setAccessibilityIdentifier_("inklow.settings")
        self.window.setTitleVisibility_(A.NSWindowTitleHidden)
        self.window.setTitlebarAppearsTransparent_(True)
        self.window.setAutorecalculatesKeyViewLoop_(True)
        self.window.setReleasedWhenClosed_(False)
        self.window.setDelegate_(self)
        self.window.setMinSize_(A.NSMakeSize(900, 640))
        self.window.center()
        root = A.NSView.alloc().initWithFrame_(A.NSMakeRect(0, 0, 900, 640))
        self.window.setContentView_(root)
        sidebar = A.NSVisualEffectView.alloc().initWithFrame_(A.NSMakeRect(0, 0, 210, 640))
        sidebar.setMaterial_(A.NSVisualEffectMaterialSidebar)
        sidebar.setBlendingMode_(A.NSVisualEffectBlendingModeBehindWindow)
        root.addSubview_(sidebar)
        source = TableSource.alloc().init().configure(
            self._sidebar_rows(), ["label"], lambda row: self._show_pane(row["id"]), True,
            lambda row: self._open_url(SUPPORT_URL))
        table = A.NSTableView.alloc().initWithFrame_(A.NSMakeRect(0, 0, 194, 440))
        column = A.NSTableColumn.alloc().initWithIdentifier_("label")
        column.setWidth_(194)
        table.addTableColumn_(column)
        table.setHeaderView_(None)
        table.setRowHeight_(30)
        table.setIntercellSpacing_(A.NSMakeSize(0, 1))
        table.setSelectionHighlightStyle_(A.NSTableViewSelectionHighlightStyleSourceList)
        table.setBackgroundColor_(A.NSColor.clearColor())
        table.setDataSource_(source)
        table.setDelegate_(source)
        table.setAllowsEmptySelection_(False)
        _identify(table, "inklow.sidebar", "Settings categories")
        scroll = A.NSScrollView.alloc().initWithFrame_(A.NSMakeRect(8, 120, 194, 470))
        scroll.setDrawsBackground_(False)
        scroll.setHasVerticalScroller_(False)
        scroll.setDocumentView_(table)
        sidebar.addSubview_(scroll)
        self.sidebar_status = _label("", A.NSMakeRect(20, 18, 174, 48), small=True)
        sidebar.addSubview_(self.sidebar_status)
        self.pane_title = _label("", A.NSMakeRect(234, 590, 630, 24), bold=True)
        self.pane_title.setFont_(A.NSFont.systemFontOfSize_weight_(15, A.NSFontWeightSemibold))
        root.addSubview_(self.pane_title)
        self.sidebar_source, self.sidebar_table = source, table
        self.content_scroll = A.NSScrollView.alloc().initWithFrame_(A.NSMakeRect(210, 0, 690, 588))
        self.content_scroll.setHasVerticalScroller_(True)
        self.content_scroll.setAutohidesScrollers_(True)
        self.content_scroll.setDrawsBackground_(False)
        root.addSubview_(self.content_scroll)
        self.last_setup_refresh = 0.0

    @objc.python_method
    def _remember_drafts(self):
        for key, editor in self.editors.items():
            self.drafts[key] = str(editor.string())

    @objc.python_method
    def _show_pane(self, pane):
        pane = _canonical_pane(pane) or "general"
        if self.window is None or not hasattr(self, "content_scroll"):
            return
        if getattr(self, "rendering", False):
            return
        self.rendering = True
        try:
            self.window.makeFirstResponder_(None)
            self._remember_drafts()
            self.editors = {}
            self.controls = {}
            self.instruction_buttons = {}
            self.setup_rows = {}
            self.model_description = self.download_progress = self.download_label = None
            self.update_status_label = self.update_progress = None
            self.update_buttons = {}
            # Keep only menu bindings; released pane controls must not retain actions.
            self.bindings = {tag: action for tag, action in self.bindings.items() if tag <= self.menu_binding_limit}
            self.pane = pane
            index = next(i for i, row in enumerate(self.sidebar_source.rows) if row["id"] == pane)
            self.sidebar_table.selectRowIndexes_byExtendingSelection_(NSIndexSet.indexSetWithIndex_(index), False)
            self.pane_title.setStringValue_(next(title for key, title, symbol in PANES if key == pane))
            view = FlippedView.alloc().initWithFrame_(A.NSMakeRect(0, 0, 690, 588))
            self.content_scroll.setDocumentView_(view)
            self.form = Form(self, view, pane)
            getattr(self, f"_pane_{pane}")()
            self.form.finish()
            view.scrollPoint_(A.NSMakePoint(0, 0))
        finally:
            self.rendering = False
        self.last_setup_refresh = 0.0
        if self.last_status is not None:
            self._refresh_live_pane(self.last_status)

    def windowShouldClose_(self, sender):
        self._remember_drafts()
        if self.no_tray or self.status_item is None:
            A.NSApplication.sharedApplication().terminate_(None)
            return False
        return True

    @objc.python_method
    def _permission_state(self):
        import ApplicationServices as AX
        import Quartz
        return {
            "accessibility": bool(AX.AXIsProcessTrusted()),
            "input_monitoring": bool(Quartz.CGPreflightListenEventAccess()),
            "screen_recording": bool(system.screen_capture_granted()),
        }

    # ----- Setup -------------------------------------------------------------

    @objc.python_method
    def _terminal_permissions(self):
        reader = getattr(self.controller.bridge, "terminal_permissions", None)
        try:
            permissions = reader() if reader is not None else {}
        except Exception:
            permissions = {}
        result = {}
        for key, (app_id, _name) in _TERMINAL_HOSTS.items():
            entry = permissions.get(app_id, {}) if isinstance(permissions, dict) else {}
            status = entry.get("status") if isinstance(entry, dict) else None
            result[key] = status if status in _AUTOMATION_LABELS else "unavailable"
        return result

    @objc.python_method
    def _setup_state(self):
        """Live checklist: (importance, done, status text) from OS and loaded state."""
        permissions = self._permission_state()
        config = self.controller.config
        status = self.last_status or {}
        download = status.get("download") or {}
        inline = system.macos_inline_prediction_enabled()
        screenshots = config.screenshot_context or config.screenshot_appearance
        if self._model_ready():
            model = "Ready" if config.backend == "gguf" else "Local server"
        elif download.get("state") == "downloading" and download.get("total"):
            model = f"Downloading {int(float(download.get('received', 0)) * 100 / float(download['total']))}%"
        else:
            model = "Not installed"
        state = {
            "accessibility": ("required", permissions["accessibility"], "Granted" if permissions["accessibility"] else "Not granted"),
            "input_monitoring": ("required", permissions["input_monitoring"], "Granted" if permissions["input_monitoring"] else "Not granted"),
            "model": ("required", self._model_ready(), model),
            "inline_prediction": ("recommended", inline is not True,
                                  "Disabled" if inline is False else ("Enabled" if inline else "Couldn’t be detected")),
            "screen_context": ("optional", config.screen_context, "On" if config.screen_context else "Off"),
            "clipboard_context": ("optional", config.clipboard_context, "On" if config.clipboard_context else "Off"),
            "screen_recording": ("optional", permissions["screen_recording"] or not screenshots,
                                 "Granted" if permissions["screen_recording"] else ("Not granted" if screenshots else "Not needed")),
        }
        state["terminal_completion"] = ("optional", config.terminal_completion,
                                        "On" if config.terminal_completion else "Off")
        for key, permission in self._terminal_permissions().items():
            state[key] = ("optional", not config.terminal_completion or permission == "granted",
                          _AUTOMATION_LABELS[permission])
        return state

    @objc.python_method
    def _setup_summary(self, state):
        required = sum(1 for importance, done, _text in state.values() if importance == "required" and not done)
        optional = sum(1 for importance, done, _text in state.values() if importance != "required" and not done)
        if required:
            return f"{required} Required Step{'s' if required != 1 else ''} Left", required, optional
        if optional:
            return ("One Optional Step Left" if optional == 1 else f"{optional} Optional Steps Left"), required, optional
        return "You’re All Set", required, optional

    @objc.python_method
    def _setup_action(self, key):
        config = self.controller.config
        state = self._setup_state()
        if key in {"accessibility", "input_monitoring"}:
            system.open_permissions(key)
        elif key == "model":
            self.show_settings("general")
            return
        elif key == "inline_prediction":
            system.open_permissions("keyboard")
        elif key in {"screen_context", "clipboard_context"}:
            if getattr(config, key):
                self.show_settings("context")
                return
            self._context_consent(key, True)
        elif key == "screen_recording":
            if not state[key][1]:
                self._request_screen_recording()
            else:
                self.show_settings("context")
                return
        elif key == "terminal_completion":
            self._change(terminal_completion=not config.terminal_completion)
        elif key in _TERMINAL_HOSTS:
            if not config.terminal_completion:
                return
            if state[key][2] in {"Granted", "Denied"}:
                self._open_url("x-apple.systempreferences:com.apple.preference.security?Privacy_Automation")
            else:
                request = getattr(self.controller.bridge, "request_terminal_access", None)
                if request is not None:
                    request(_TERMINAL_HOSTS[key][0])
        self.last_setup_refresh = 0.0
        self._refresh_setup()

    @objc.python_method
    def _setup_row(self, key, title, detail, height=None):
        container = FlippedView.alloc().initWithFrame_(A.NSMakeRect(0, 0, 330, 27))
        status = _label("", A.NSMakeRect(0, 5, 150, 18), small=True)
        status.setAlignment_(A.NSTextAlignmentRight)
        _identify(status, f"inklow.setup.{key}.status")
        button = self._button("", f"inklow.setup.{key}", lambda k=key: self._setup_action(k))
        button.setFrame_(A.NSMakeRect(158, 0, 172, 27))
        container.addSubview_(status)
        container.addSubview_(button)
        self.form.row(title, detail, container, control_width=330, height=height)
        self.setup_rows[key] = (status, button)

    @objc.python_method
    def _pane_setup(self):
        f = self.form
        self.setup_header = _label("", A.NSMakeRect(0, 0, 552, 30), bold=True)
        self.setup_header.setFont_(A.NSFont.systemFontOfSize_weight_(20, A.NSFontWeightSemibold))
        _identify(self.setup_header, "inklow.setup.summary")
        f.custom(self.setup_header, 30)
        f.note("This checklist reads the actual macOS permission, model and preference state every second. Nothing here changes until you press a button.")
        f.section("Required")
        self._setup_row("accessibility", "Accessibility", "Reads the focused text field and inserts accepted text at its caret. System Settings → Privacy & Security → Accessibility.")
        self._setup_row("input_monitoring", "Input Monitoring", "Detects typing and your acceptance shortcuts. System Settings → Privacy & Security → Input Monitoring.")
        self._setup_row("model", "AI model", "A local model on this Mac (or a configured loopback server). Downloads start only when you ask.")
        f.section("Recommended")
        self._setup_row("inline_prediction", "macOS inline predictive text", "Detected from your Mac’s keyboard preference. Its grey text competes with BroTypist; turn off Keyboard → Text Input → Edit… → Show inline predictive text. BroTypist never changes it for you.", height=84)
        f.section("Optional")
        self._setup_row("screen_context", "Focused-window context", "Use readable Accessibility text from the focused window to improve suggestions. Asks for confirmation.")
        self._setup_row("clipboard_context", "Clipboard context", "Use clipboard text for eligible requests. Asks for confirmation.")
        self._setup_row("screen_recording", "Screen & System Audio Recording", "Needed only for the opt-in screenshot features in Context. Images stay in memory on this Mac and are discarded immediately.", height=78)
        self._setup_row("terminal_completion", "Terminal AI-prompt support (opt-in)",
                        "Allow supported AI-prompt fields in Terminal and iTerm2. Shell prompts stay excluded; custom app exclusions remain hard blocks.", height=78)
        for key, (_app_id, name) in _TERMINAL_HOSTS.items():
            self._setup_row(key, f"{name} Automation",
                            f"Live permission to read {name} through Apple Events. Open the host first; only Request Access asks macOS for permission.", height=78)
        f.note("Automation permission does not prove terminal input is supported. Suggestions stay silent unless a safe AI-prompt field and verified same-line caret are available. Google Docs accessibility is enabled separately from its document accessory menu; it is not a completion-support guarantee.")
        f.note("When launching from source, macOS may list your terminal or Python host instead of BroTypist. Permission changes can require restarting BroTypist.")
        self.setup_native = _label("", A.NSMakeRect(0, 0, 552, 54), small=True)
        _identify(self.setup_native, "inklow.setup.nativeStatus")
        f.custom(self.setup_native, 54)
        f.section()
        f.row("Welcome & tour", "Review the setup steps and try a real local suggestion.", self._button("Show Welcome…", "inklow.setup.welcome", self.show_welcome), control_width=150)

    @objc.python_method
    def _refresh_setup(self):
        now = time.monotonic()
        if now - self.last_setup_refresh < 1 or not hasattr(self, "sidebar_source"):
            return
        self.last_setup_refresh = now
        state = self._setup_state()
        summary, required, optional = self._setup_summary(state)
        pending = bool(required or optional)
        if bool(self.sidebar_source.badges.get("setup")) != pending:
            self.sidebar_source.badges["setup"] = pending
            self.sidebar_table.reloadData()
        if self.pane != "setup" or not getattr(self, "setup_rows", None):
            return
        self.setup_header.setStringValue_(summary)
        config = self.controller.config
        for key, (label, button) in self.setup_rows.items():
            importance, done, text = state[key]
            label.setStringValue_(text)
            permission_key = key == "screen_recording" or key in _TERMINAL_HOSTS
            label.setTextColor_(A.NSColor.systemGreenColor() if done and (not permission_key or text == "Granted")
                                else (A.NSColor.systemRedColor() if importance == "required" else A.NSColor.secondaryLabelColor()))
            if key in {"accessibility", "input_monitoring"}:
                title = "Open Settings…"
                button.setHidden_(done)
            elif key == "model":
                title = "Change…" if done else "Choose Model…"
            elif key == "inline_prediction":
                title = "Keyboard Settings…"
            elif key in {"screen_context", "clipboard_context"}:
                title = "Context…" if getattr(config, key) else "Enable…"
            elif key == "terminal_completion":
                title = "Turn Off" if config.terminal_completion else "Enable…"
            elif key in _TERMINAL_HOSTS:
                title = "Automation Settings…" if text in {"Granted", "Denied"} else "Request Access…"
                button.setEnabled_(config.terminal_completion and text not in {"Not running", "Unavailable"})
            else:
                title = "Request Access…" if not done else "Context…"
            button.setTitle_(title)
            button.setAccessibilityLabel_(f"{title} {key.replace('_', ' ')}")
        status = self.last_status or {}
        self.setup_native.setStringValue_(str(status.get("native_status", "")))

    # ----- General -----------------------------------------------------------

    @objc.python_method
    def _autostart(self, value):
        old = system.launch_at_login_state()
        outcome = system.set_launch_at_login(value)
        if outcome is False:
            raise RuntimeError("Login startup could not be changed. Your saved preference was not changed.")
        try:
            self._change(launch_at_login=value)
        except Exception:
            system.set_launch_at_login(old)
            raise

    @objc.python_method
    def _pane_general(self):
        f = self.form
        if self.welcome and self.welcome.window.isVisible() and self.welcome.step == 3:
            f.section("Welcome")
            f.row("Model setup", "Return to the welcome window when your model is selected.", self._button("Return to Welcome", "inklow.general.returnToWelcome", self._return_to_welcome), control_width=166)
        f.section("Startup")
        login = self._switch("launch_at_login", "Launch BroTypist at login", callback=self._autostart)
        login.setState_(A.NSControlStateValueOn if system.launch_at_login_state() else A.NSControlStateValueOff)
        self._switch("show_accessory", "Show accessory button", "A small Ghost Caret beside the text field with this app’s completion, recording and Tab controls. Option-click it to type a normal Tab.")
        f.row("Menu bar icon", "Always shown, so BroTypist can always be paused, reopened or quit.")
        f.section("Completions")
        self._switch("global_enabled", "Enable predictions", "Turn off to keep BroTypist running without suggesting anything.")
        self._switch("enabled", "Enable completions by default", "When off, only apps and websites enabled in App Settings get completions.")
        self._switch("complete_inside_text", "Complete inside existing text", "Also suggest when there is text after the caret.")
        self._popup("completion_length", "Maximum completion length",
                    [("Current word only", "word"), ("Short (~1–2 words)", "short"), ("Medium (~2–4 words)", "medium"), ("Long (~5–8 words)", "long")],
                    "Medium is recommended; longer completions are right less often. Current word only finishes the word you are typing and never suggests the next one.")
        f.section("Autocorrect")
        self._switch("typo_suppression", "Don’t show completions when a typo is suspected", "Checks only the word you are typing, on this Mac, with the macOS spelling checker for the detected language.")
        accept = self.controller.config.shortcuts.get("word") or "Tab"
        self._switch("typo_fixes", "Show suggested fixes", f"Strikes through a likely typo and shows the fix on the same line. Press {accept} to replace the word.")
        self._general_model()

    @objc.python_method
    def _general_model(self):
        f = self.form
        f.section("AI Model")
        selected = ""
        if self.controller.config.backend == "gguf":
            from .models import recommended_preset
            self.model_inventory = self.controller.models()
            recommended = recommended_preset()
            installed = next((m for m in self.model_inventory if str(m["id"]) == recommended.id and m.get("installed")), None)
            f.row(f"Recommended for this Mac: {recommended.label.split(' — ', 1)[-1]} ({_gb(recommended.size_bytes)})",
                  "Recommended default based on this Mac’s memory." + (" Installed." if installed else ""), height=70)
            options = [("Custom GGUF file", "")]
            for model in self.model_inventory:
                label = f"{str(model['label']).split(' — ', 1)[-1]} ({_gb(model.get('size_bytes'))})"
                options.append((("✓ " if model.get("installed") else "↓ ") + label, str(model["id"])))
            config = self.controller.config
            selected = self.model_selection if self.model_selection is not None else next(
                (str(m["id"]) for m in self.model_inventory if config.model_path and str(m.get("path", "")) == config.model_path),
                "" if config.model_path else config.model_id)
            self._popup("model_preset", "AI model", options, "✓ installed · ↓ needs download. Selecting a downloaded model uses it immediately.", value=selected, callback=self._select_model)
        else:
            f.row("AI model", str(self.controller.config.model))
        f.row(self._model_name(), str(self.controller.status().get("message") or "The cached model prepares in supported writing fields before prediction is requested."), height=80)
        f.row("Model files", control=self._button("Reveal in Finder", "inklow.general.revealModels", system.reveal_models), control_width=146)
        self._model_controls(selected)

    @objc.python_method
    def _return_to_welcome(self):
        self.window.orderOut_(None)
        self.welcome.window.makeKeyAndOrderFront_(None)
        A.NSApplication.sharedApplication().activateIgnoringOtherApps_(True)

    @objc.python_method
    def _model_controls(self, selected):
        f = self.form
        f.section("Model Management")
        self._popup("backend", "Inference backend", [("Local GGUF model", "gguf"), ("Local Ollama server", "ollama"), ("Local OpenAI-compatible server", "openai")],
                    "Local models or literal loopback servers only.", callback=self._backend)
        if self.controller.config.backend == "gguf":
            self.model_description = _label("", A.NSMakeRect(0, 0, 520, 42), small=True)
            f.row("Selected preset", control=self.model_description, height=65, control_width=360)
            actions = FlippedView.alloc().initWithFrame_(A.NSMakeRect(0, 0, 552, 36))
            for x, width, title, identifier, callback in (
                (0, 150, "Download", "download", self._download),
                (158, 120, "Cancel", "cancelDownload", self.controller.cancel_download),
                (286, 150, "Choose GGUF…", "chooseModel", self._choose_model),
            ):
                button = self._button(title, f"inklow.general.{identifier}", callback)
                button.setFrame_(A.NSMakeRect(x, 0, width, 28))
                actions.addSubview_(button)
                self.controls[identifier] = button
            f.custom(actions, 36)
            self.download_progress = A.NSProgressIndicator.alloc().initWithFrame_(A.NSMakeRect(0, 0, 552, 16))
            self.download_progress.setStyle_(A.NSProgressIndicatorStyleBar)
            self.download_progress.setIndeterminate_(False)
            self.download_progress.setMinValue_(0)
            self.download_progress.setMaxValue_(1)
            _identify(self.download_progress, "inklow.general.downloadProgress", "Model download progress")
            f.custom(self.download_progress, 16)
            self.download_label = _label("", A.NSMakeRect(0, 0, 552, 35), small=True)
            _identify(self.download_label, "inklow.general.downloadStatus")
            f.custom(self.download_label, 35)
            f.note(f"Current file: {self.controller.config.model_path or 'None selected'}")
            self._select_model(selected, apply=False)
        else:
            for field, title in (("endpoint", "Loopback endpoint"), ("model", "Model name")):
                control = A.NSTextField.alloc().initWithFrame_(A.NSMakeRect(0, 0, 280, 25))
                control.setStringValue_(getattr(self.controller.config, field))
                _identify(control, f"inklow.general.{field}", title)
                self._bind(control, lambda sender, key=field: self._change(**{key: str(sender.stringValue()).strip()}))
                f.row(title, "Press Return to save.", control, control_width=280)
        self._popup("acceleration", "Hardware acceleration", [("Automatic (Metal when available)", "auto"), ("CPU only", "cpu")])
        self._popup("prompt_style", "Model prompt style", [("Text continuation", "continuation"), ("Instruct writing model", "writing")],
                    "Preset models select their prompt format automatically.")

    @objc.python_method
    def _backend(self, value):
        self._change(backend=value)
        self._show_pane("general")

    @objc.python_method
    def _selected_model(self):
        popup = self.controls.get("model_preset")
        index = int(popup.indexOfSelectedItem()) - 1 if popup else -1
        return self.model_inventory[index] if 0 <= index < len(self.model_inventory) else None

    @objc.python_method
    def _use_preset(self, model):
        self.controller.set_model(str(model["path"]), str(model.get("prompt_style", "continuation")), model_id=str(model["id"]))
        self.model_selection = None

    @objc.python_method
    def _select_model(self, preset_id, apply=True):
        model = next((item for item in self.model_inventory if str(item["id"]) == preset_id), None)
        if self.model_description is not None:
            self.model_description.setStringValue_(str(model.get("description", "")) if model else "Use Choose GGUF to select your own local model.")
        if apply:
            self.model_selection = preset_id
            if model and model.get("installed"):
                self._use_preset(model)
            elif model and self.pane != "general":
                self.show_settings("general")
                return
            elif model is None:
                self._choose_model()
                return
        self._refresh_download(self.controller.status())

    @objc.python_method
    def _download(self):
        if self.shutdown_started:
            raise ValueError("Local cleanup has started. Quit and reopen BroTypist before starting another model download.")
        model = self._selected_model()
        if model is None:
            raise ValueError("Select a model preset before downloading.")
        current = next((item for item in self.controller.models() if item["id"] == model["id"]), model)
        if current.get("installed"):
            self._use_preset(current)
            self._show_pane("general")
        else:
            if model.get("availability"):
                raise RuntimeError(str(model["availability"]))
            accepted = False
            if model.get("requires_license_acceptance"):
                alert = A.NSAlert.alloc().init()
                alert.setMessageText_(f"Accept {model['license_name']}?")
                alert.setInformativeText_(
                    "This model is downloaded separately and is not covered by BroTypist’s software license. "
                    "Use is subject to the model’s terms and incorporated Prohibited Use Policy.\n\n"
                    + str(model["license_url"]) + "\nhttps://ai.google.dev/gemma/prohibited_use_policy")
                alert.addButtonWithTitle_("Agree and Download")
                alert.addButtonWithTitle_("Cancel")
                alert.addButtonWithTitle_("Read Terms")
                response = alert.runModal()
                if response == A.NSAlertThirdButtonReturn:
                    A.NSWorkspace.sharedWorkspace().openURL_(NSURL.URLWithString_(str(model["license_url"])))
                    return
                if response != A.NSAlertFirstButtonReturn:
                    return
                accepted = True
            self.controller.download_model(str(model["id"]), accept_license=accepted)

    @objc.python_method
    def _choose_model(self):
        panel = A.NSOpenPanel.openPanel()
        panel.setCanChooseFiles_(True)
        panel.setCanChooseDirectories_(False)
        panel.setAllowsMultipleSelection_(False)
        panel.setAllowedFileTypes_(["gguf"])
        panel.setTitle_("Choose a local GGUF model")
        if panel.runModal() == A.NSModalResponseOK:
            path = str(panel.URL().path())
            self.controller.set_model(path, self.controller.config.prompt_style, model_id="")
        self.model_selection = None
        self._show_pane("general")

    @objc.python_method
    def _refresh_download(self, status):
        if self.download_progress is None or "download" not in self.controls:
            return
        download = status.get("download") or {}
        active = download.get("state") == "downloading"
        if download.get("state") == "complete":
            completed = str(download.get("id", ""))
            for item in self.model_inventory:
                if str(item["id"]) == completed:
                    item["installed"] = True
        done = float(download.get("received", download.get("downloaded", download.get("bytes", 0))) or 0)
        total = float(download.get("total", download.get("size_bytes", 0)) or 0)
        self.download_progress.setMaxValue_(max(1, total))
        self.download_progress.setDoubleValue_(min(done, max(1, total)))
        self.download_progress.setIndeterminate_(active and total <= 0)
        if active and total <= 0:
            self.download_progress.startAnimation_(None)
        else:
            self.download_progress.stopAnimation_(None)
        model = self._selected_model()
        self.controls["download"].setTitle_("Use Model" if model and model.get("installed") else "Download")
        self.controls["download"].setEnabled_(bool(model) and not active)
        self.controls["cancelDownload"].setEnabled_(active)
        message = str(download.get("error") or download.get("message") or "")
        if active and total:
            message = f"Downloading: {done / 1_000_000:.1f} / {total / 1_000_000:.1f} MB"
        elif download.get("state") == "complete":
            message = "Download verified. Select this model and press Use Model to activate it."
        if not message:
            message = "No download in progress. Installed models are ready to use when selected."
        self.download_label.setStringValue_(message)

    # ----- Context -----------------------------------------------------------

    @objc.python_method
    def _context_consent(self, field, value):
        if value:
            title, message = _CONSENT[field]
            if not self._confirm(title, message):
                control = self.controls.get(field)
                if control:
                    control.setState_(A.NSControlStateValueOff)
                return False
        self._change(**{field: value})
        if value and field in _SCREENSHOT_FIELDS and not system.screen_capture_granted():
            self._request_screen_recording()
        if self.window and self.window.isVisible() and self.last_status is not None:
            self.last_setup_refresh = 0.0
            self._refresh_live_pane(self.last_status)
        return True

    @objc.python_method
    def _request_screen_recording(self):
        """Explicit user action only: the OS prompt appears at most once per install."""
        if system.request_screen_capture_permission():
            return True
        alert = A.NSAlert.alloc().init()
        alert.setMessageText_("Allow Screen & System Audio Recording")
        alert.setInformativeText_("Turn on BroTypist in System Settings → Privacy & Security → Screen & System Audio Recording, then quit and reopen BroTypist. Until then, screenshot features stay inactive and BroTypist keeps using Accessibility text.")
        alert.addButtonWithTitle_("Open System Settings")
        alert.addButtonWithTitle_("Not Now")
        A.NSApplication.sharedApplication().activateIgnoringOtherApps_(True)
        if alert.runModal() == A.NSAlertFirstButtonReturn:
            system.open_permissions("screen_recording")
        return False

    @objc.python_method
    def _pane_context(self):
        f = self.form
        f.section("Accessibility Context")
        self._switch("screen_context", "Use focused-window text", "Read relevant Accessibility text from the verified focused window. Inaccessible content is skipped. Uses Accessibility permission only.", callback=lambda value: self._context_consent("screen_context", value))
        self._switch("clipboard_context", "Use clipboard for context", "Clipboard text is read only for an eligible request and passed to your local model, not stored in history.", callback=lambda value: self._context_consent("clipboard_context", value))
        f.section("Screenshots (Opt-in)")
        self._switch("screenshot_context", "Use screenshots for context", "When Accessibility text is unavailable, recognize text around the field from an in-memory screenshot on this Mac. Images are never saved or uploaded.", callback=lambda value: self._context_consent("screenshot_context", value))
        self._switch("screenshot_appearance", "Use screenshots to improve suggestion appearance", "Measure the host text’s font and baseline from an in-memory screenshot so grey text matches it. Suggestions stay hidden when the measurement can’t be verified.", callback=lambda value: self._context_consent("screenshot_appearance", value))
        container = FlippedView.alloc().initWithFrame_(A.NSMakeRect(0, 0, 330, 27))
        self.screen_recording_label = _label("", A.NSMakeRect(0, 5, 150, 18), small=True)
        self.screen_recording_label.setAlignment_(A.NSTextAlignmentRight)
        _identify(self.screen_recording_label, "inklow.context.screenRecording.status")
        self.screen_recording_button = self._button("", "inklow.context.screenRecording", self._screen_recording_action)
        self.screen_recording_button.setFrame_(A.NSMakeRect(158, 0, 172, 27))
        container.addSubview_(self.screen_recording_label)
        container.addSubview_(self.screen_recording_button)
        f.row("Screen & System Audio Recording", "Required by macOS for either screenshot option. macOS may show its recording indicator while a screenshot is taken. Granting access usually needs a relaunch.", container, control_width=330, height=84)
        f.note("Restrictions are checked before any context is read: secure fields, excluded apps and blocked websites are skipped. Context is never saved to history or sent to a remote service.")
        status = self.last_status or {}
        self.context_status_label = _label("Context: " + str(status.get("context_message") or "Not requested"), A.NSMakeRect(0, 0, 552, 36), small=True)
        _identify(self.context_status_label, "inklow.context.status")
        self.form.custom(self.context_status_label, 36)

    @objc.python_method
    def _screen_recording_action(self):
        if system.screen_capture_granted():
            system.open_permissions("screen_recording")
        else:
            self._request_screen_recording()
        if self.last_status is not None:
            self._refresh_context(self.last_status)

    @objc.python_method
    def _refresh_context(self, status):
        label = getattr(self, "context_status_label", None)
        if label is None:
            return
        label.setStringValue_("Context: " + str(status.get("context_message") or "Not requested"))
        config = self.controller.config
        granted = bool(system.screen_capture_granted())
        wanted = config.screenshot_context or config.screenshot_appearance
        self.screen_recording_label.setStringValue_("Granted" if granted else ("Not granted — screenshots inactive" if wanted else "Not granted"))
        self.screen_recording_label.setTextColor_(A.NSColor.systemGreenColor() if granted else (A.NSColor.systemOrangeColor() if wanted else A.NSColor.secondaryLabelColor()))
        self.screen_recording_button.setTitle_("Open Settings…" if granted else "Request Access…")
        self.screen_recording_button.setEnabled_(granted or wanted)

    # ----- Personalization ---------------------------------------------------

    @objc.python_method
    def _history_toggle(self, value):
        if value and not self._confirm_recording():
            self.controls["record_history"].setState_(A.NSControlStateValueOff)
            return
        self.controller.set_history_enabled(value)
        self._update_history_info()

    @objc.python_method
    def _update_history_info(self):
        info = self.controller.history_info()
        if info.get("error"):
            text = f"Secure storage unavailable: {info['error']}"
        elif not info.get("available"):
            text = "Encrypted history is locked; learning is off." if info.get("count") else "No encrypted history yet. Enable recording to learn locally."
        else:
            text = f"{int(info.get('count', 0))} encrypted records · {int(info.get('bytes', 0)):,} bytes on disk"
        if hasattr(self, "history_label"):
            self.history_label.setStringValue_(text)

    @objc.python_method
    def _load_history_counts(self):
        """Explicit action: unlocks encrypted history once to count records."""
        self.history_counts = self.controller.history_counts(unlock=True)
        self._show_pane(self.pane)

    @objc.python_method
    def _scope_count(self, key):
        counts = self.history_counts or {}
        if key.startswith("app:"):
            return int((counts.get("apps") or {}).get(key[4:], 0))
        return int((counts.get("domains") or {}).get(key[7:], 0))

    @objc.python_method
    def _delete_history(self, app_id=None, domain=None):
        if app_id:
            target = f"encrypted history recorded in {self._app_name(app_id)}."
        elif domain:
            target = f"encrypted history recorded on {domain}."
        else:
            target = "all encrypted personalization history. Aggregate statistics are unchanged."
        if not self._confirm("Delete local writing history?", "This permanently deletes " + target):
            return
        self.controller.delete_history(app_id=app_id, domain=domain)
        if self.history_counts is not None:
            if app_id:
                self.history_counts.get("apps", {}).pop(app_id, None)
            elif domain:
                self.history_counts.get("domains", {}).pop(domain, None)
            else:
                self.history_counts = {**self.history_counts, "apps": {}, "domains": {}}
        self._update_history_info()
        if self.window and self.window.isVisible() and self.pane in {"personalization", "appSettings"}:
            self._show_pane(self.pane)

    @objc.python_method
    def _strength_slider(self):
        container = FlippedView.alloc().initWithFrame_(A.NSMakeRect(0, 0, 280, 27))
        container.addSubview_(_label("Off", A.NSMakeRect(0, 5, 30, 18), small=True))
        slider = A.NSSlider.sliderWithValue_minValue_maxValue_target_action_(
            float(self.controller.config.personalization_strength), 0.0, 100.0, None, None)
        slider.setFrame_(A.NSMakeRect(32, 2, 210, 24))
        slider.setNumberOfTickMarks_(6)
        slider.setContinuous_(False)
        _identify(slider, "inklow.personalization.personalization_strength", "Personalize writing style and word choice")
        self._bind(slider, lambda sender: self._change(personalization_strength=int(round(float(sender.doubleValue())))))
        self.controls["personalization_strength"] = slider
        container.addSubview_(slider)
        container.addSubview_(_label("Max", A.NSMakeRect(248, 5, 32, 18), small=True))
        return container

    @objc.python_method
    def _pane_personalization(self):
        f = self.form
        f.section("Local Writing Learning")
        self._switch("record_history", "Learn locally from recorded writing", "Opt-in encrypted history; the key stays in OS secure storage. Short texts and likely secrets are skipped. App and website rules take precedence.", callback=self._history_toggle)
        self._switch("record_unaccepted", "Also learn when you accept no suggestion", "Records eligible writing only while history is enabled for that app or website. Leave off to record only sessions with an accepted suggestion.")
        f.row("Personalize style and word choice", "Bounded evidence from your observed edits guides local suggestions. Off ignores learned evidence; explicit instructions and the draft's language win.", self._strength_slider(), control_width=280, height=85)
        f.note("Learning does not retrain model weights. It matches the current app or exact website and a confidently detected writing language: English, Portuguese, Spanish, French or German. Unknown or ambiguous language means no learned context.")
        f.note("Learning starts with new observed edits, not existing drafts, bulk pastes, older history or BroTypist-generated text. Deleting history forgets the derived style and cancels suggestions using it.")
        self.history_label = _label("", A.NSMakeRect(0, 0, 340, 43), small=True)
        f.row("Encrypted history", control=self.history_label, control_width=360, height=65)
        self._update_history_info()
        f.section("Recorded Texts by App and Website")
        if self.history_counts is None:
            f.row("Show counts", "Unlocks encrypted history once on this Mac to count records per app and website.", self._button("Show Counts…", "inklow.personalization.showCounts", self._load_history_counts), control_width=145)
        elif not self.history_counts.get("available"):
            f.row("Encrypted history is unavailable", "Enable history or check that OS secure storage is available.", height=70)
        else:
            scopes = [(f"app:{app}", count) for app, count in sorted((self.history_counts.get("apps") or {}).items())]
            scopes += [(f"domain:{host}", count) for host, count in sorted((self.history_counts.get("domains") or {}).items())]
            if not scopes:
                f.row("No recorded texts")
            for key, count in scopes:
                name = self._app_name(key[4:]) if key.startswith("app:") else key[7:]
                button = self._button("Delete…", f"inklow.personalization.delete.{key}",
                                      lambda k=key: self._delete_history(app_id=k[4:]) if k.startswith("app:") else self._delete_history(domain=k[7:]))
                f.row(f"{name} — {count} text{'s' if count != 1 else ''}", control=button, control_width=100)
        f.row("Forget all learned writing", "Deletes encrypted history and derived style. Aggregate statistics are unchanged.", self._button("Delete All…", "inklow.personalization.deleteHistory", self._delete_history), control_width=145)
        f.section("Custom AI Instructions")
        f.row("Your writing preferences", "Pre-filled from your Mac’s language and region. App and website instructions are added after these. Keep them to a few hundred words; edits apply after Save Changes.", height=85)
        self._instructions_editor("global_instructions", self.controller.config.custom_instructions)

    @objc.python_method
    def _instructions_editor(self, key, saved_value, *, app_rule=False):
        self.form.end_group()
        scroll = A.NSScrollView.alloc().initWithFrame_(A.NSMakeRect(0, 0, self.form.width, 152))
        scroll.setHasVerticalScroller_(True)
        scroll.setBorderType_(A.NSBezelBorder)
        editor = A.NSTextView.alloc().initWithFrame_(A.NSMakeRect(0, 0, self.form.width - 20, 150))
        editor.setFont_(A.NSFont.systemFontOfSize_(13))
        editor.setRichText_(False)
        editor.setAllowsUndo_(True)
        editor.setVerticallyResizable_(True)
        editor.setHorizontallyResizable_(False)
        editor.setAutoresizingMask_(A.NSViewWidthSizable)
        editor.setTextContainerInset_(A.NSMakeSize(9, 8))
        editor.textContainer().setWidthTracksTextView_(True)
        editor.setString_(self.drafts.get(key, saved_value))
        editor.setDelegate_(self)
        scope = "rule" if app_rule else "global"
        _identify(editor, f"inklow.{self.pane}.{scope}Instructions", f"{'App or website' if app_rule else 'Global'} custom instructions")
        scroll.setDocumentView_(editor)
        self.editors[key] = editor
        self.form.custom(scroll, 152)
        container = FlippedView.alloc().initWithFrame_(A.NSMakeRect(0, 0, 552, 32))
        save = self._button("Save Changes", f"inklow.{self.pane}.{scope}.saveInstructions", lambda: self._save_instructions(key, app_rule))
        revert = self._button("Revert", f"inklow.{self.pane}.{scope}.revertInstructions", lambda: self._revert_instructions(key, app_rule))
        save.setFrame_(A.NSMakeRect(self.form.width - 248, 0, 145, 28))
        revert.setFrame_(A.NSMakeRect(self.form.width - 100, 0, 100, 28))
        container.addSubview_(save)
        container.addSubview_(revert)
        if not app_rule:
            reset = self._button("Reset to Default", f"inklow.{self.pane}.resetInstructions", lambda: self._reset_instructions(key))
            reset.setFrame_(A.NSMakeRect(0, 0, 150, 28))
            container.addSubview_(reset)
        self.instruction_buttons[key] = (save, revert, app_rule)
        self.form.custom(container, 34)
        self._update_instruction_buttons()

    def textDidChange_(self, notification):
        self._update_instruction_buttons()

    @objc.python_method
    def _saved_instructions(self, app_rule):
        if app_rule:
            return self.controller.config.app_rules.get(self.selected_rule, {}).get("custom_instructions", "")
        return self.controller.config.custom_instructions

    @objc.python_method
    def _update_instruction_buttons(self):
        if not hasattr(self, "instruction_buttons"):
            return
        for key, (save, revert, app_rule) in self.instruction_buttons.items():
            editor = self.editors.get(key)
            if editor:
                changed = str(editor.string()) != self._saved_instructions(app_rule)
                save.setEnabled_(changed)
                revert.setEnabled_(changed)

    @objc.python_method
    def _save_instructions(self, key, app_rule):
        value = str(self.editors[key].string())
        if len(value) > 8000:
            raise ValueError("Custom instructions are limited to 8,000 characters.")
        if app_rule:
            self._rule_change(custom_instructions=value)
        else:
            self._change(custom_instructions=value)
        self.drafts.pop(key, None)
        self._update_instruction_buttons()

    @objc.python_method
    def _revert_instructions(self, key, app_rule):
        self.editors[key].setString_(self._saved_instructions(app_rule))
        self.drafts.pop(key, None)
        self._update_instruction_buttons()

    @objc.python_method
    def _reset_instructions(self, key):
        # Only the draft changes; Save Changes applies it like any other edit.
        from .config import default_instructions
        self.editors[key].setString_(default_instructions())
        self._update_instruction_buttons()

    # ----- Emoji -------------------------------------------------------------

    @objc.python_method
    def _pane_emoji(self):
        self.form.section("Emoji Suggestions")
        self._switch("emoji_enabled", "Suggest emoji after typing a colon", "At the start of a line or after a space, type a colon and a name such as :sweat_smile in an eligible field. Choose a numbered result to replace only the typed query. macOS’s own picker stays on Control-Command-Space.")
        self._switch("emoticons_enabled", "Suggest emoji from emoticons", "Offer a real emoji for emoticons such as :-) ;-) :( or <3. Accept explicitly; text is never silently rewritten.")
        self.form.section("Emoji Preferences")
        self._popup("emoji_skin_tone", "Preferred skin tone", [("Neutral 👋", 0), ("Light 👋🏻", 1), ("Medium-light 👋🏼", 2), ("Medium 👋🏽", 3), ("Medium-dark 👋🏾", 4), ("Dark 👋🏿", 5)])
        self._switch("emoji_include_neutral", "Include neutral variant", "Also list the unmodified emoji alongside your preferred skin tone.")
        self._popup("emoji_gender", "Preferred gender", [("Gender-Neutral", "neutral"), ("Female", "female"), ("Male", "male")])
        self.form.note("The numbered chooser uses keys 1–9 or a click without taking focus from your text field. Emoji lookup is local and does not require an inference worker.")

    # ----- Shortcuts ---------------------------------------------------------

    @objc.python_method
    def _record(self, action, title, detail):
        recorder = ShortcutRecorder.alloc().initWithFrame_(A.NSMakeRect(0, 0, 220, 27))
        recorder.setBezelStyle_(A.NSBezelStyleRounded)
        _identify(recorder, f"inklow.shortcuts.{action}", title)
        def save(value):
            shortcuts = dict(self.controller.config.shortcuts)
            if value and any(existing == value for name, existing in shortcuts.items() if name != action):
                self._error("This shortcut is already assigned to another action. Clear that assignment first.")
                return False
            shortcuts[action] = value
            try:
                self._change(shortcuts=shortcuts)
                return True
            except Exception as exc:
                self._error(str(exc))
                return False
        recorder.configure(self.controller.config.shortcuts.get(action, ""), save,
                           lambda recording: self.controller.bridge.set_shortcuts_enabled(not recording and not self.shutdown_started))
        self.form.row(title, detail, recorder, height=87)

    @objc.python_method
    def _pane_shortcuts(self):
        self.secure_banner = _label("", A.NSMakeRect(0, 0, 552, 46), small=True)
        _identify(self.secure_banner, "inklow.shortcuts.secureInput")
        self.form.custom(self.secure_banner, 46)
        self.form.section("Accepting Completions")
        self._record("word", "Complete only the next word", "Click the recorder, then press the actual shortcut. Delete clears it; Escape cancels recording. Press again to keep walking through a suggestion. Clear it to stop using Tab everywhere; per-app Tab is in App Settings.")
        self._switch("trailing_space", "Include trailing space after a single word", "Accepting one word also inserts its following space when available.")
        self._record("all", "Trigger full completion", "AboveTab is the physical key immediately above Tab, independent of keyboard layout. Default: that key without modifiers.")
        self._popup("punctuation_mode", "Include trailing punctuation",
                    [("Only with full completion", "full"), ("With either shortcut", "always"), ("Never", "never")],
                    "Punctuation left out stays in the suggestion, so pressing again inserts it.")
        self.form.note("Option-Tab, or Option-clicking the accessory button, types a normal Tab.")
        self.form.section("When You Press Escape")
        self._popup("escape_behavior", "Escape action", [("Pause completions for a few seconds", "pause"), ("Turn off in this app until you switch apps", "app"), ("Leave Escape to the app", "passthrough")], "Escape is consumed only when it dismisses a visible suggestion; a second Escape reaches your app.")
        self._number("escape_pause_seconds", "Brief pause (seconds)", "How long to pause suggestions after Escape; 1–3600.", 1, 3600)
        self.form.section("Activation & Toggles")
        self._record("force", "Force-activate completions", "Ask for a suggestion at the current eligible caret, even if automatic typing detection has not fired.")
        self._record("app_toggle", "Temporarily toggle the current app", "Turn completions off or on in the current app or website for a few minutes without changing its saved rule.")
        self._record("toggle", "Toggle completions globally", "Optional. Pause or resume everywhere without changing app-specific rules.")
        self.form.section("Word Tools")
        self._record("alternatives", "Show alternative suggestions", "Optional shortcut for the numbered alternatives chooser. Configure alternatives in Labs.")
        self._record("synonyms", "Look up synonyms", "Optional shortcut for synonyms of an explicitly selected word. Configure synonyms in Labs.")

    @objc.python_method
    def _refresh_secure_input(self, status):
        banner = getattr(self, "secure_banner", None)
        if banner is None:
            return
        if status.get("secure_input"):
            keys = ", ".join(value for value in (self.controller.config.shortcuts.get("word"), self.controller.config.shortcuts.get("all")) if value) or "your shortcuts"
            banner.setStringValue_(f"⚠︎ Secure Input is on. macOS hides typing from BroTypist while another app (often a password field or terminal) holds Secure Input, so {keys} can’t be detected until it ends.")
            banner.setTextColor_(A.NSColor.systemOrangeColor())
        else:
            banner.setStringValue_("Secure Input is off; shortcuts are available.")
            banner.setTextColor_(A.NSColor.secondaryLabelColor())

    # ----- Battery -----------------------------------------------------------

    @objc.python_method
    def _pane_battery(self):
        f = self.form
        self.power_banner = _label("", A.NSMakeRect(0, 0, 552, 36), small=True)
        _identify(self.power_banner, "inklow.battery.powerSource")
        f.custom(self.power_banner, 36)
        f.section("On Battery Power")
        self._switch("battery_on_demand", "Only show completions on demand", "On battery, generate after the force-activation shortcut instead of automatically after typing.")
        options = [("Same as usual", "same")]
        for model in self.controller.models():
            label = f"{str(model['label']).split(' — ', 1)[-1]} ({_gb(model.get('size_bytes'))})"
            if model.get("installed"):
                options.append((label, str(model["id"])))
            elif str(model["id"]) == self.controller.config.battery_model:
                options.append((label + " — not downloaded", str(model["id"])))
        self._popup("battery_model", "Use a smaller model", options, "Only downloaded catalogue models are offered; nothing downloads automatically. Applies to local GGUF models.")
        self._switch("battery_shorter", "Generate slightly shorter completions", "Reduce generated completion length on battery.")
        f.section("Inference Memory")
        self._number("idle_release_seconds", "Release model after idle (seconds)", "0 keeps the worker ready. 1–3600 releases the entire inference process after inactivity. Pause and Quit release it immediately.", 0, 3600)
        f.note("The cached model prepares when you focus a supported multiline writing field, so loading can overlap typing. It does not start blindly at launch or download automatically. Your idle timeout is preserved.")

    @objc.python_method
    def _refresh_power(self, status):
        banner = getattr(self, "power_banner", None)
        if banner is None:
            return
        if status.get("on_battery"):
            banner.setStringValue_("🔋 Running on battery power: the settings below are in effect now.")
            banner.setTextColor_(A.NSColor.systemGreenColor())
        else:
            banner.setStringValue_("🔌 This Mac is plugged in. These settings take effect when it runs on battery power.")
            banner.setTextColor_(A.NSColor.secondaryLabelColor())

    # ----- App Settings ------------------------------------------------------

    @objc.python_method
    def _rule_summary(self, rule):
        parts = []
        if "enabled" in rule:
            parts.append("Completions on" if rule["enabled"] else "Completions off")
        if "record_history" in rule:
            parts.append("Writing recorded" if rule["record_history"] else "Writing not recorded")
        if rule.get("tab_enabled") is False:
            parts.append("Tab off")
        if "improve_compatibility" in rule:
            parts.append("Compatibility " + ("on" if rule["improve_compatibility"] else "off"))
        if rule.get("complete_in_code_editors"):
            parts.append("Completes in code editor")
        if rule.get("custom_instructions"):
            parts.append("Instructions")
        return " · ".join(parts) or "Uses defaults"

    @objc.python_method
    def _rule_title(self, key):
        return self._app_name(key[4:]) if key.startswith("app:") else key[7:]

    @objc.python_method
    def _rule_rows(self):
        query = self.rule_query.casefold().strip()
        rows = []
        for key, rule in self.controller.config.app_rules.items():
            title = self._rule_title(key)
            if query and query not in title.casefold() and query not in key.casefold():
                continue
            rows.append({"key": key, "title": title, "summary": self._rule_summary(rule), "icon": self._rule_icon(key)})
        return sorted(rows, key=lambda row: (row["key"].startswith("domain:"), row["title"].casefold()))

    @objc.python_method
    def _filter_rules(self, query):
        self.rule_query = str(query)
        self.rule_source.rows = self._rule_rows()
        self.rule_table.reloadData()
        self._select_rule_row()

    @objc.python_method
    def _select_rule_row(self):
        keys = [row["key"] for row in self.rule_source.rows]
        if self.selected_rule in keys:
            self.rule_table.selectRowIndexes_byExtendingSelection_(NSIndexSet.indexSetWithIndex_(keys.index(self.selected_rule)), False)
        else:
            self.rule_table.deselectAll_(None)

    @objc.python_method
    def _pane_appSettings(self):
        f = self.form
        rules = self.controller.config.app_rules
        if self.selected_rule not in rules:
            self.selected_rule = min(rules, default="", key=lambda key: (key.startswith("domain:"), self._rule_title(key).casefold()))
        toolbar = FlippedView.alloc().initWithFrame_(A.NSMakeRect(0, 0, 552, 28))
        search = A.NSSearchField.alloc().initWithFrame_(A.NSMakeRect(0, 1, 220, 26))
        search.setPlaceholderString_("Search apps and websites")
        search.setStringValue_(self.rule_query)
        search.setSendsSearchStringImmediately_(True)
        _identify(search, "inklow.appSettings.search", "Search apps and websites")
        self._bind(search, lambda sender: self._filter_rules(sender.stringValue()))
        toolbar.addSubview_(search)
        for x, width, title, identifier, callback in (
            (232, 104, "Add App…", "addApp", self._choose_app),
            (342, 118, "Add Current App", "addCurrentApp", self._add_current_app),
            (466, 120, "Add Domain…", "addDomain", self._add_domain),
        ):
            button = self._button(title, f"inklow.appSettings.{identifier}", callback)
            button.setFrame_(A.NSMakeRect(x, 0, width, 28))
            toolbar.addSubview_(button)
        f.custom(toolbar, 28)
        self.rule_source = RuleListSource.alloc().init().configure(self)
        self.rule_source.rows = self._rule_rows()
        table = A.NSTableView.alloc().initWithFrame_(A.NSMakeRect(0, 0, f.width, 220))
        column = A.NSTableColumn.alloc().initWithIdentifier_("rule")
        column.setWidth_(f.width - 4)
        table.addTableColumn_(column)
        table.setHeaderView_(None)
        table.setRowHeight_(44)
        table.setDataSource_(self.rule_source)
        table.setDelegate_(self.rule_source)
        table.registerForDraggedTypes_([A.NSPasteboardTypeFileURL, A.NSPasteboardTypeURL])
        _identify(table, "inklow.appSettings.list", "Apps and websites with their own settings")
        scroll = A.NSScrollView.alloc().initWithFrame_(A.NSMakeRect(0, 0, f.width, 220))
        scroll.setHasVerticalScroller_(True)
        scroll.setBorderType_(A.NSBezelBorder)
        scroll.setDocumentView_(table)
        self.rule_table = table
        f.custom(scroll, 220)
        self._select_rule_row()
        from .native.macos_terminal import terminal_policy_exclusions
        excluded = ", ".join(terminal_policy_exclusions(self.controller.config)[:8])
        f.note(f"Drag an app from Finder, or a link from a browser, onto the list to add it. Website rules work in any browser that exposes its URL. Effective hard app exclusions: {excluded}… Custom exclusions are never removed by Terminal opt-in.")
        if not self.selected_rule:
            f.note("No app or website has its own settings yet. Everything follows General, Context and Personalization.")
            return
        self._rule_detail(self.selected_rule, rules[self.selected_rule])

    @objc.python_method
    def _rule_detail(self, key, rule):
        f = self.form
        app = key[4:] if key.startswith("app:") else ""
        f.section(self._rule_title(key))
        for field, title, detail in (
            ("enabled", "Completions", "Inherit follows General (or this browser’s app rule for a website)."),
            ("tab_enabled", "Accept with Tab", "Other acceptance shortcuts keep working here."),
            ("record_history", "Record writing", "Enabled asks for confirmation and needs secure storage, even when global history is off. Disabled never records here."),
        ):
            value = "inherit" if field not in rule else ("yes" if rule[field] else "no")
            self._popup(f"rule_{field}", title, [("Inherit", "inherit"), ("Enabled", "yes"), ("Disabled", "no")], detail, value=value, callback=lambda choice, name=field: self._rule_override(name, choice))
        if app:
            value = "auto" if "improve_compatibility" not in rule else ("on" if rule["improve_compatibility"] else "off")
            self._popup("rule_improve_compatibility", "Improve compatibility with this app", [("Automatic", "auto"), ("On", "on"), ("Off", "off")],
                        "On asks the app to expose its full accessibility tree (helps Electron and Chromium apps). Automatic does this only for detected Electron/Chromium apps; Off never does.",
                        value=value, callback=self._compatibility)
            from .native.base import is_code_editor
            if is_code_editor(app):
                control = A.NSSwitch.alloc().initWithFrame_(A.NSMakeRect(0, 0, 42, 25))
                control.setState_(A.NSControlStateValueOn if rule.get("complete_in_code_editors") else A.NSControlStateValueOff)
                _identify(control, "inklow.appSettings.complete_in_code_editors", "Complete in code editors")
                self._bind(control, lambda sender: self._code_editor(bool(sender.state())))
                f.row("Complete in code editors", "Code editing is off by default. Recognized stock VS Code chat fields may work automatically; this switch also permits other writable fields in this editor.", control, control_width=42)
        if self.history_counts is None:
            count_control = self._button("Show Count…", "inklow.appSettings.showCount", self._load_history_counts)
            f.row("Recorded texts", "Unlocks encrypted history once to count records.", count_control, control_width=145)
        else:
            count = self._scope_count(key)
            f.row(f"Recorded texts: {count}", "Records are attributed to the website when the browser exposes its URL, otherwise to the app.")
        delete = self._button("Delete History…", "inklow.appSettings.deleteHistory",
                              lambda: self._delete_history(app_id=app) if app else self._delete_history(domain=key[7:]))
        f.row("Delete recorded texts here", "Irreversible. Deletes only this app’s or exactly this website’s records.", delete, control_width=145)
        f.row("Remove these settings", "Restores inherited behavior. Recorded texts are not deleted.", self._button("Remove…", "inklow.appSettings.deleteRule", self._delete_rule), control_width=145)
        f.section("Custom Instructions")
        f.row("Additional instructions", "Added after your global instructions for this app or website. Edits apply after Save Changes; drafts survive switching panes.", height=85)
        self._instructions_editor(f"rule:{key}", str(rule.get("custom_instructions", "")), app_rule=True)

    @objc.python_method
    def _normalize_domain(self, value):
        from .config import normalize_domain
        text = value.strip()
        parsed = urlsplit(text if "://" in text else "https://" + text)
        if parsed.username or parsed.password or not parsed.hostname:
            raise ValueError("Enter a website hostname such as example.com.")
        domain = normalize_domain(parsed.hostname)
        if "." not in domain or parsed.port:
            raise ValueError("Enter a complete website hostname without a port.")
        return domain

    @objc.python_method
    def _add_rules(self, items):
        """Add (kind, value) entries; kind 0 = app identifier, 1 = website."""
        rules = deepcopy(self.controller.config.app_rules)
        selected = ""
        for kind, value in items:
            value = str(value).strip()
            if not value:
                raise ValueError("Enter an application identifier or website domain.")
            key = "domain:" + self._normalize_domain(value) if kind == 1 else "app:" + value
            rules.setdefault(key, {})
            selected = key
        if rules != self.controller.config.app_rules:
            self._change(app_rules=rules)
        self.selected_rule = selected
        self.rule_query = ""
        self._show_pane("appSettings")

    @objc.python_method
    def _add_current_app(self):
        app = self.last_external_app
        if not app:
            raise ValueError("Switch to the target application first, then open BroTypist’s App Settings from its menu.")
        self._add_rules([(0, app)])

    @objc.python_method
    def _add_domain(self):
        alert = A.NSAlert.alloc().init()
        alert.setMessageText_("Add a website")
        alert.setInformativeText_("Enter a hostname such as docs.example.com. Its settings apply in any browser that exposes the page URL.")
        field = A.NSTextField.alloc().initWithFrame_(A.NSMakeRect(0, 0, 280, 24))
        field.setPlaceholderString_("example.com")
        _identify(field, "inklow.appSettings.newDomain", "Website hostname")
        alert.setAccessoryView_(field)
        alert.addButtonWithTitle_("Add")
        alert.addButtonWithTitle_("Cancel")
        alert.window().setInitialFirstResponder_(field)
        if alert.runModal() == A.NSAlertFirstButtonReturn:
            self._add_rules([(1, str(field.stringValue()))])

    @objc.python_method
    def _choose_app(self):
        panel = A.NSOpenPanel.openPanel()
        panel.setCanChooseFiles_(True)
        panel.setCanChooseDirectories_(False)
        panel.setTreatsFilePackagesAsDirectories_(False)
        panel.setAllowedFileTypes_(["app"])
        panel.setAllowsMultipleSelection_(False)
        panel.setDirectoryURL_(NSURL.fileURLWithPath_("/Applications"))
        if panel.runModal() == A.NSModalResponseOK:
            bundle = NSBundle.bundleWithPath_(str(panel.URL().path()))
            app_id = str(bundle.bundleIdentifier() or "") if bundle else ""
            if not app_id:
                raise ValueError("The selected application has no bundle identifier.")
            self._add_rules([(0, app_id)])

    @objc.python_method
    def _select_rule(self, key):
        self.selected_rule = key
        self._show_pane("appSettings")

    @objc.python_method
    def _rule_change(self, **changes):
        rules = deepcopy(self.controller.config.app_rules)
        if self.selected_rule not in rules:
            raise ValueError("Select an existing app or website.")
        rules[self.selected_rule].update(changes)
        self._change(app_rules=rules)

    @objc.python_method
    def _rule_override(self, field, choice):
        rules = deepcopy(self.controller.config.app_rules)
        if choice == "inherit":
            rules[self.selected_rule].pop(field, None)
        else:
            if field == "record_history" and choice == "yes" and rules[self.selected_rule].get(field) is not True and not self._confirm_recording():
                self._defer(lambda: self._show_pane("appSettings"))
                return
            rules[self.selected_rule][field] = choice == "yes"
        self._change(app_rules=rules)
        self._defer(lambda: self._show_pane("appSettings"))

    @objc.python_method
    def _compatibility(self, choice):
        rules = deepcopy(self.controller.config.app_rules)
        if choice == "auto":
            rules[self.selected_rule].pop("improve_compatibility", None)
        else:
            rules[self.selected_rule]["improve_compatibility"] = choice == "on"
        self._change(app_rules=rules)
        self._defer(lambda: self._show_pane("appSettings"))

    @objc.python_method
    def _code_editor(self, value):
        rules = deepcopy(self.controller.config.app_rules)
        if value:
            rules[self.selected_rule]["complete_in_code_editors"] = True
        else:
            rules[self.selected_rule].pop("complete_in_code_editors", None)
        self._change(app_rules=rules)
        self._defer(lambda: self._show_pane("appSettings"))

    @objc.python_method
    def _delete_rule(self):
        name = self._rule_title(self.selected_rule)
        if not self._confirm(f"Remove settings for {name}?", "It will follow your general settings again. Recorded texts are not deleted."):
            return
        rules = deepcopy(self.controller.config.app_rules)
        rules.pop(self.selected_rule, None)
        self._change(app_rules=rules)
        self.drafts.pop(f"rule:{self.selected_rule}", None)
        self.selected_rule = ""
        self.editors = {}
        self._show_pane("appSettings")

    # ----- Labs --------------------------------------------------------------

    @objc.python_method
    def _pane_labs(self):
        self.form.section("Alternative Suggestions")
        self._switch("alternatives_enabled", "Show alternative suggestions", "A numbered list below the caret; press a number to insert. Results are generated locally and inserted only after you choose.")
        self._popup("alternatives_mode", "Show alternatives", [("With shortcut", "shortcut"), ("After a short delay", "pause")], "The alternatives shortcut also reveals the list early in delay mode.")
        self._number("alternatives_delay_ms", "Delay before showing (milliseconds)", "How long to wait before requesting alternatives in delay mode.", 100, 10000)
        self.form.section("Synonyms")
        self._switch("synonyms_enabled", "Suggest synonyms for selected words", "Select a word in a supported field. A numbered choice replaces only the exact revalidated selection.")
        self._popup("synonyms_mode", "Show synonyms", [("After selecting text", "selection"), ("With shortcut", "shortcut")])
        self._number("synonyms_delay_ms", "Delay before showing (milliseconds)", "Delay before requesting synonyms after selection.", 100, 10000)
        self.form.section("Typing Timing")
        self._number("debounce_ms", "Completion delay (milliseconds)", "Wait after typing before automatic inference; 100–5000.", 100, 5000)
        self._number("min_chars", "Minimum preceding characters", "Do not request an automatic completion before this much context; 1–1000.", 1, 1000)
        self._number("context_chars", "Maximum local context characters", "Bound the text passed to the local model; 64–8000.", 64, 8000)
        self.form.note("Shortcuts for word tools are in Shortcuts. Unsupported or private text fields never become eligible merely because a lab feature is enabled.")

    # ----- Statistics --------------------------------------------------------

    @objc.python_method
    def _pane_statistics(self):
        f = self.form
        f.section("Today’s Activity")
        self.today_label = _label("", A.NSMakeRect(0, 0, 390, 25))
        _identify(self.today_label, "inklow.statistics.today")
        f.row("Today", control=self.today_label, control_width=410)
        self._switch("statistics_enabled", "Collect aggregate statistics", "Counts only successful insertions. No writing content is stored in statistics.")
        f.section("History")
        self._popup("stat_period", "Time range", [("Today", "today"), ("Last 7 Days", "week"), ("Last 30 Days", "month"), ("Last Year", "year"), ("All Time", "all")], value=self.stat_period, callback=self._stat_period)
        self._popup("stat_group", "Group by", [("Day", "day"), ("Month", "month"), ("Application", "app")], value=self.stat_group, callback=self._stat_group)
        self._popup("stat_metric", "Metric", [("Completions", "completions"), ("Words", "words"), ("Characters", "characters"), ("Time saved (minutes)", "estimated_seconds")], value=self.stat_metric, callback=self._stat_metric)
        self.stats_chart = BarChartView.alloc().initWithFrame_(A.NSMakeRect(0, 0, f.width, 200))
        _identify(self.stats_chart, "inklow.statistics.chart")
        f.custom(self.stats_chart, 200)
        self.statistics_totals = _label("", A.NSMakeRect(0, 0, 552, 58), small=True)
        _identify(self.statistics_totals, "inklow.statistics.totals")
        f.custom(self.statistics_totals, 58)
        self.stats_source = TableSource.alloc().init().configure([], ["label", "completions", "words", "characters", "minutes"])
        table = A.NSTableView.alloc().initWithFrame_(A.NSMakeRect(0, 0, f.width - 4, 170))
        table.setRowHeight_(24)
        for key, title, width in (("label", "Day / Month / App", 210), ("completions", "Completions", 100), ("words", "Words", 90), ("characters", "Characters", 100), ("minutes", "Minutes", 80)):
            column = A.NSTableColumn.alloc().initWithIdentifier_(key)
            column.setTitle_(title)
            column.setWidth_(width)
            table.addTableColumn_(column)
        table.setDataSource_(self.stats_source)
        table.setDelegate_(self.stats_source)
        table.setUsesAlternatingRowBackgroundColors_(True)
        _identify(table, "inklow.statistics.table", "Accepted completions by day, month or application")
        scroll = A.NSScrollView.alloc().initWithFrame_(A.NSMakeRect(0, 0, f.width, 170))
        scroll.setHasVerticalScroller_(True)
        scroll.setBorderType_(A.NSBezelBorder)
        scroll.setDocumentView_(table)
        self.stats_table = table
        f.custom(scroll, 170)
        self._refresh_statistics()

    @objc.python_method
    def _stat_period(self, value):
        self.stat_period = value
        self._refresh_statistics()

    @objc.python_method
    def _stat_group(self, value):
        self.stat_group = value
        self._refresh_statistics()

    @objc.python_method
    def _stat_metric(self, value):
        self.stat_metric = value
        self._refresh_statistics()

    @objc.python_method
    def _stat_row_label(self, value):
        value = str(value)
        if self.stat_group == "app":
            return self._app_name(value)
        if self.stat_group == "day" and len(value) == 10:
            return value[5:]
        return value

    @objc.python_method
    def _refresh_statistics(self):
        stats = self.controller.stats(self.stat_period, self.stat_group)
        metric = self.stat_metric
        rows = stats.get("rows", [])
        self.stats_source.rows = [{**row, "label": self._stat_row_label(row.get("label", "")),
                                   "minutes": _minutes(row.get("estimated_seconds", 0))} for row in rows]
        self.stats_table.reloadData()
        scale = 60 if metric == "estimated_seconds" else 1
        formatter = (lambda value: f"{value:.1f} min") if scale == 60 else (lambda value: f"{int(value):,}")
        self.stats_chart.set_bars([(self._stat_row_label(row.get("label", "")), float(row.get(metric, 0) or 0) / scale) for row in rows], formatter)
        today = stats.get("today", {})
        self.today_label.setStringValue_(f"{int(today.get('completions', 0))} completions · {int(today.get('words', 0))} words · {int(today.get('characters', 0))} characters")
        totals = stats.get("totals", {})
        average = stats.get("daily_average", {})
        names = {"completions": "completions", "words": "words", "characters": "characters", "estimated_seconds": "minutes saved"}
        total_value = float(stats.get("estimated_seconds", 0) if metric == "estimated_seconds" else totals.get(metric, 0) or 0) / scale
        average_value = float(average.get(metric, 0) or 0) / scale
        active_days = int(stats.get("active_days", 0) or 0)
        if scale == 60:
            total_text, average_text = formatter(total_value), f"{average_value:.1f} min"
        else:
            total_text, average_text = f"{int(total_value):,} {names[metric]}", f"{average_value:,.1f} {names[metric]}"
        plural = "s" if active_days != 1 else ""
        self.statistics_totals.setStringValue_(
            f"Total: {total_text}  ·  Daily average ({active_days} active day{plural}): {average_text}\n"
            f"Estimated time saved: ≈{_minutes(stats.get('estimated_seconds', 0))} minutes (from accepted characters).")
        self.last_stats_refresh = time.monotonic()

    @objc.python_method
    def _check_updates(self):
        if self.termination_requested or self.update_committed:
            return
        self.show_settings("about")
        if self.updates.snapshot().get("state") not in {"checking", "downloading", "installing"}:
            self.updates.start_check()
        self._refresh_updates()

    @objc.python_method
    def _download_update(self):
        if not self.termination_requested:
            self.updates.start_download()
            self._refresh_updates()

    @objc.python_method
    def _cancel_update(self):
        if self.update_committed:
            return
        if self.update_handoff is not None:
            self._cancel_termination()
        else:
            self.updates.cancel()
        self._refresh_updates()

    @objc.python_method
    def _update_controls(self):
        self.form.section("Software Updates")
        self._switch("check_updates_automatically", "Check automatically",
                     "Check for signed releases at launch, at most once a day. Downloads and installation require your approval.")
        self.update_status_label = _label("", A.NSMakeRect(0, 0, self.form.width, 92), small=True)
        _identify(self.update_status_label, "inklow.about.updateStatus", "Software update status")
        self.form.custom(self.update_status_label, 92)
        self.update_progress = A.NSProgressIndicator.alloc().initWithFrame_(A.NSMakeRect(0, 0, self.form.width, 16))
        self.update_progress.setStyle_(A.NSProgressIndicatorStyleBar)
        self.update_progress.setIndeterminate_(False)
        self.update_progress.setMinValue_(0)
        self.update_progress.setMaxValue_(1)
        _identify(self.update_progress, "inklow.about.updateProgress", "Software update download progress")
        self.form.custom(self.update_progress, 16)
        self.form.section()
        actions = (
            ("check", "Check for Updates…", self._check_updates),
            ("download", "Download Update", self._download_update),
            ("cancel", "Cancel", self._cancel_update),
            ("install", "Install & Relaunch", self._install_update),
        )
        for key, title, callback in actions:
            button = self._button(title, f"inklow.about.update.{key}", callback)
            self.update_buttons[key] = button
            self.form.row(title, control=button, control_width=180)
        self.form.note("Free macOS signing: this app is not notarized. macOS may require per-app approval in System Settings → Privacy & Security → Open Anyway after an update. Approve only this app if you trust it; never disable Gatekeeper. Accessibility, Input Monitoring or Screen Recording access may need approval again.")
        self._refresh_updates()

    @objc.python_method
    def _refresh_updates(self):
        status = self.updates.snapshot()
        state = status.get("state", "idle")
        busy = state in {"checking", "downloading", "installing"}
        version = status.get("version") or ""
        if hasattr(self, "update_item"):
            self.update_item.setEnabled_(not busy and not self.termination_requested)
            self.update_item.setToolTip_(str(status.get("message") or "Check for a signed software update."))
        if self.update_status_label is None:
            return
        message = str(status.get("message") or "No update check has run yet.")
        heading = f"Installed: {status.get('current_version') or __version__}"
        if version:
            heading += f" · Release: {version}"
        reason = str(status.get("install_reason") or "")
        if self.termination_requested:
            message = "Finishing local work before quitting…" if self.update_handoff is None else "Finishing local work before installing…"
        elif self.shutdown_error:
            message = self.shutdown_error
        received = max(0, int(status.get("received") or 0))
        total = max(0, int(status.get("total") or 0))
        if state == "downloading":
            message += f" {received:,} of {total:,} bytes" if total else f" {received:,} bytes received"
        self.update_status_label.setStringValue_("\n".join(text for text in (heading, message, reason) if text))
        self.update_progress.setMaxValue_(max(1, total))
        self.update_progress.setDoubleValue_(min(received, max(1, total)))
        indeterminate = state == "checking" or (state == "downloading" and not total)
        self.update_progress.setIndeterminate_(indeterminate)
        if indeterminate:
            self.update_progress.startAnimation_(None)
        else:
            self.update_progress.stopAnimation_(None)
        self.update_progress.setHidden_(state not in {"checking", "downloading", "ready", "installing"})
        self.update_progress.setToolTip_(f"{received:,} of {total:,} bytes" if total else message)
        self.update_buttons["check"].setEnabled_(not busy and not self.termination_requested)
        self.update_buttons["download"].setEnabled_(state == "available" and bool(status.get("installable")) and not self.termination_requested)
        self.update_buttons["cancel"].setEnabled_((busy or self.update_handoff is not None) and not self.update_committed)
        self.update_buttons["install"].setEnabled_(state == "ready" and bool(status.get("installable")) and not self.termination_requested)
        self.update_buttons["install"].setHidden_(not bool(status.get("installable")))

    @objc.python_method
    def _allow_discard_instruction_drafts(self):
        self._remember_drafts()
        config = self.controller.config
        dirty = False
        for key, value in self.drafts.items():
            if key == "global_instructions":
                saved = config.custom_instructions
            elif key.startswith("rule:"):
                saved = config.app_rules.get(key[5:], {}).get("custom_instructions", "")
            else:
                continue
            if value != saved:
                dirty = True
                break
        return not dirty or self._confirm("Discard unsaved instructions?",
                                         "Custom instruction drafts have not been saved. Continue to discard them and quit, or cancel and use Save Changes in Settings.")

    @objc.python_method
    def _install_update(self):
        status = self.updates.snapshot()
        if self.termination_requested or status.get("state") != "ready" or not status.get("installable"):
            return
        if not self._confirm("Install update and relaunch?",
                             "BroTypist will stop local activity, quit, replace this app with the verified update and relaunch. macOS may require per-app approval; this free-signed release is not notarized. Your model, history and settings are not replaced."):
            return
        if not self._allow_discard_instruction_drafts():
            return
        if self.termination_requested or self.shutdown_complete:
            return
        self.update_handoff = self.updates.prepare_install()
        self.termination_drafts_approved = True
        A.NSApplication.sharedApplication().terminate_(None)

    # ----- About -------------------------------------------------------------

    @objc.python_method
    def _pane_about(self):
        icon = A.NSImageView.alloc().initWithFrame_(A.NSMakeRect(0, 0, 552, 88))
        icon.setImage_(A.NSImage.alloc().initWithContentsOfFile_(str(asset_path("app.icns"))))
        icon.setImageScaling_(A.NSImageScaleProportionallyUpOrDown)
        self.form.custom(icon, 88)
        from .appkit_welcome import wordmark
        self.form.custom(wordmark(552, 44, 32), 44)
        self.form.section()
        self.form.row(f"Version {__version__}", "Local-first predictive typing. A menu utility, not a document editor. No account, subscription or telemetry.", height=74)
        self._update_controls()
        self.form.section()
        self.form.row("Releases", "Release notes and downloadable packages on GitHub.", self._button("Open Releases…", "inklow.about.releases", lambda: self._open_url(RELEASES_URL)), control_width=145)
        self.form.row("Support", "Report a problem or ask a question on GitHub. Nothing is sent automatically.", self._button("Open Support…", "inklow.about.support", lambda: self._open_url(SUPPORT_URL)), control_width=145)
        self.form.row("Source repository", "Public source code and releases on GitHub. No account or access token is required.", self._button("View on GitHub", "inklow.about.source", lambda: self._open_url(REPOSITORY_URL)), control_width=145)
        self.form.row("Welcome & setup", "Review privacy, permissions, model setup and the real typing preview.", self._button("Show Welcome…", "inklow.about.welcome", self.show_welcome), control_width=145)
        self.form.row("Typing safety", "Secure fields, excluded apps and unsupported controls are skipped before capture. Insertion revalidates the original target.", height=92)
        self.form.row("Settings links", f"Other apps and scripts can open a pane with {URL_SCHEME}://settings/<pane>, for example {URL_SCHEME}://settings/shortcuts.", height=74)
        self.form.row(f"Quit {APP_NAME}", "Stops monitoring, cancels downloads and releases the inference worker.", self._button("Quit", "inklow.about.quit", lambda: A.NSApplication.sharedApplication().terminate_(None)), control_width=100)

    def applicationShouldTerminate_(self, app):
        if self.shutdown_complete:
            return A.NSTerminateNow
        # Always defer: a menu may still own tracking, and cleanup may need
        # several bounded cancellation attempts on the existing main-loop timer.
        self.termination_requested = True
        self.termination_waiting = True
        return A.NSTerminateLater

    @objc.python_method
    def _cancel_termination(self):
        if self.update_committed:
            return
        try:
            if self.update_handoff is not None:
                handoff, self.update_handoff = self.update_handoff, None
                try:
                    handoff.abort()
                finally:
                    self.updates.cancel()
        finally:
            self.termination_requested = False
            self.termination_drafts_approved = False
            self.shutdown_next_retry = 0.0
            if self.shutdown_started:
                self.shutdown_error = "Installation or quit was cancelled. Local activity is stopped; use Quit to finish cleanup, then reopen BroTypist."
            if self.termination_waiting:
                self.termination_waiting = False
                A.NSApplication.sharedApplication().replyToApplicationShouldTerminate_(False)

    @objc.python_method
    def _advance_shutdown(self):
        if getattr(self, "_shutdown_advancing", False) or time.monotonic() < self.shutdown_next_retry:
            return
        self._shutdown_advancing = True
        try:
            if not self.termination_drafts_approved:
                if not self._allow_discard_instruction_drafts():
                    self._cancel_termination()
                    return
                self.termination_drafts_approved = True
            self.shutdown_started = True
            cleanup = []
            if self.welcome:
                cleanup.append(("preview", self.welcome.stop_preview))
            cleanup.append(("controller", self.controller.stop))
            if self.overlay:
                cleanup.append(("overlay", self.overlay.close))
            if self.update_handoff is None:
                cleanup.append(("updates", self.updates.close))
            pending, failed = False, False
            for name, action in cleanup:
                try:
                    action()
                except ShutdownPending:
                    pending = True
                except Exception:
                    failed = True
                else:
                    if name == "overlay":
                        self.overlay = None
            if failed:
                raise RuntimeError("Local cleanup failed.")
            if pending:
                raise ShutdownPending("Finishing cancellation of local work.")
            if self.window:
                self.window.makeFirstResponder_(None)
            if self.welcome:
                self.welcome.close()
            if self.update_handoff is not None:
                # close() aborts a prepared handoff. Keep it alive until all
                # preview, model and native ownership has stopped successfully.
                self.update_handoff.commit()
                self.update_committed = True
        except ShutdownPending:
            self.shutdown_next_retry = time.monotonic() + 0.1
            self._refresh_updates()
        except Exception:
            self._cancel_termination()
            self.shutdown_error = "Quit was cancelled because local cleanup failed. No update was installed. Local activity is stopped; try Quit again to finish cleanup, then reopen BroTypist."
            self.last_error = self.shutdown_error
            self._refresh_updates()
            self.show_settings("about")
            self._error(self.shutdown_error)
        else:
            self.shutdown_complete = True
            self.termination_requested = False
            if self.termination_waiting:
                self.termination_waiting = False
                A.NSApplication.sharedApplication().replyToApplicationShouldTerminate_(True)
        finally:
            self._shutdown_advancing = False

    def applicationShouldTerminateAfterLastWindowClosed_(self, app):
        return False

    def applicationWillTerminate_(self, notification):
        self.shutdown()

    @objc.python_method
    def shutdown(self):
        if self.stopping:
            return
        # Unexpected event-loop exit must never approve a waiting installer.
        if not self.shutdown_complete:
            if self.update_handoff is not None and not self.update_committed:
                self.update_handoff.abort()
                self.update_handoff = None
                self.updates.cancel()
            if self.welcome:
                self.welcome.close()
            self.controller.stop()
            if self.overlay:
                self.overlay.close()
                self.overlay = None
        # UpdateService preserves the committed writer until actual process
        # exit; closing it must not release the installer's EOF fence early.
        self.updates.close()
        self.shutdown_complete = True
        self.stopping = True
        if self.window:
            self.window.makeFirstResponder_(None)
        if self.timer:
            self.timer.invalidate()
            self.timer = None
        if self.status_item:
            A.NSStatusBar.systemStatusBar().removeStatusItem_(self.status_item)
            self.status_item = None


def launch(config, *, background=False, no_tray=False, pane=None) -> int:
    """Run native AppKit from source or a frozen bundle, without a Dock icon."""
    app = A.NSApplication.sharedApplication()
    app.setActivationPolicy_(A.NSApplicationActivationPolicyAccessory)
    delegate = BroTypistApplication.alloc().init().configure(config, background, no_tray, pane)
    app.setDelegate_(delegate)
    try:
        app.run()
    except KeyboardInterrupt:
        pass
    finally:
        delegate.shutdown()
        app.setDelegate_(None)
    return 0
