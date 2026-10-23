"""The macOS menu-first shell; this module never imports a Qt component.

Accessibility identifiers use ``inklow.<pane>.<setting>``. The AppKit timer is
sole owner of controller events and overlays; keyboard callbacks never own UI.
"""
from __future__ import annotations

from copy import deepcopy
from dataclasses import replace
from pathlib import Path
import time
from urllib.parse import urlsplit

import AppKit as A
import objc
from Foundation import NSBundle, NSIndexSet, NSObject, NSRunLoop, NSRunLoopCommonModes, NSTimer

from . import __version__, system
from .appkit_overlay import CaretOverlay
from .resources import APP_NAME, asset_path
from .runtime import Controller
from .shortcuts import mac_key_name, normalize_shortcut, shortcut_from_event

PANES = (
    ("setup", "Setup", "checkmark.shield"),
    ("general", "General", "gearshape"),
    ("context", "Context", "rectangle.on.rectangle"),
    ("personalization", "Personalization", "wand.and.stars"),
    ("emoji", "Emoji", "face.smiling"),
    ("shortcuts", "Shortcuts", "command"),
    ("battery", "Battery", "battery.100"),
    ("appSettings", "App Settings", "slider.horizontal.3"),
    ("labs", "Labs", "flask"),
    ("statistics", "Statistics", "chart.bar"),
    ("about", "About", "info.circle"),
)


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


class TableSource(NSObject, protocols=[objc.protocolNamed("NSTableViewDataSource"), objc.protocolNamed("NSTableViewDelegate")]):
    @objc.python_method
    def configure(self, rows, columns, callback=None, sidebar=False):
        self.rows, self.columns = rows, columns
        self.callback, self.sidebar = callback, sidebar
        return self

    def numberOfRowsInTableView_(self, table):
        return len(self.rows)

    def tableView_objectValueForTableColumn_row_(self, table, column, row):
        return str(self.rows[row].get(str(column.identifier()), ""))

    def tableView_viewForTableColumn_row_(self, table, column, row):
        identifier = str(column.identifier())
        cell = table.makeViewWithIdentifier_owner_(identifier, self)
        if cell is None:
            cell = A.NSTableCellView.alloc().initWithFrame_(A.NSMakeRect(0, 0, column.width(), 28))
            cell.setIdentifier_(identifier)
            offset = 30 if self.sidebar else 6
            label = A.NSTextField.labelWithString_("")
            label.setFrame_(A.NSMakeRect(offset, 5, column.width() - offset - 5, 20))
            label.setFont_(A.NSFont.systemFontOfSize_(13))
            label.setAutoresizingMask_(A.NSViewWidthSizable)
            cell.setTextField_(label)
            cell.addSubview_(label)
            if self.sidebar:
                image = A.NSImageView.alloc().initWithFrame_(A.NSMakeRect(5, 6, 19, 19))
                cell.setImageView_(image)
                cell.addSubview_(image)
        cell.textField().setStringValue_(str(self.rows[row].get(identifier, "")))
        if self.sidebar:
            symbol = A.NSImage.imageWithSystemSymbolName_accessibilityDescription_(self.rows[row]["symbol"], self.rows[row]["label"])
            cell.imageView().setImage_(symbol)
            cell.setAccessibilityIdentifier_(f"inklow.sidebar.{self.rows[row]['id']}")
            cell.textField().setAccessibilityIdentifier_(f"inklow.sidebar.{self.rows[row]['id']}")
        return cell

    def tableViewSelectionDidChange_(self, notification):
        row = int(notification.object().selectedRow())
        if self.callback and 0 <= row < len(self.rows):
            self.callback(self.rows[row])


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
        self.y = 24
        self.group_view = None
        self.group_y = 0
        self.row_y = 0

    def section(self, title=""):
        self.end_group()
        if title:
            self.view.addSubview_(_label(title, A.NSMakeRect(28, self.y, 544, 20), small=True, bold=True))
            self.y += 27
        box = A.NSBox.alloc().initWithFrame_(A.NSMakeRect(24, self.y, 552, 10))
        box.setBoxType_(A.NSBoxCustom)
        box.setBorderType_(A.NSNoBorder)
        box.setFillColor_(A.NSColor.controlBackgroundColor())
        box.setCornerRadius_(8)
        box.setContentViewMargins_(A.NSMakeSize(0, 0))
        self.group_view = FlippedView.alloc().initWithFrame_(A.NSMakeRect(0, 0, 552, 10))
        box.setContentView_(self.group_view)
        self.view.addSubview_(box)
        self.box, self.group_y, self.row_y = box, self.y, 0
        return self

    def end_group(self):
        if self.group_view is not None:
            self.box.setFrameSize_(A.NSMakeSize(552, self.row_y))
            self.group_view.setFrameSize_(A.NSMakeSize(552, self.row_y))
            self.y = self.group_y + self.row_y + 22
            self.group_view = None

    def row(self, title, detail="", control=None, *, height=None, control_width=220):
        if self.group_view is None:
            self.section()
        if height is None:
            height = 46 if not detail else 70 + max(0, len(detail) - 135) // 74 * 14
        if self.row_y:
            divider = A.NSBox.alloc().initWithFrame_(A.NSMakeRect(14, self.row_y, 524, 1))
            divider.setBoxType_(A.NSBoxSeparator)
            self.group_view.addSubview_(divider)
        title_width = 516 - control_width - 10 if control is not None else 520
        self.group_view.addSubview_(_label(title, A.NSMakeRect(14, self.row_y + 11, title_width, 20)))
        if detail:
            self.group_view.addSubview_(_label(detail, A.NSMakeRect(14, self.row_y + 34, 521, height - 39), small=True))
        if control is not None:
            control_height = min(46, height - 16) if isinstance(control, A.NSTextField) and not control.isEditable() else 27
            control.setFrame_(A.NSMakeRect(538 - control_width, self.row_y + 8, control_width, control_height))
            self.group_view.addSubview_(control)
        self.row_y += height
        return control

    def note(self, text):
        self.end_group()
        height = 18 + max(1, (len(text) + 76) // 77) * 15
        self.view.addSubview_(_label(text, A.NSMakeRect(28, self.y, 540, height), small=True))
        self.y += height + 12

    def custom(self, view, height):
        self.end_group()
        view.setFrame_(A.NSMakeRect(24, self.y, 552, height))
        self.view.addSubview_(view)
        self.y += height + 16

    def finish(self):
        self.end_group()
        self.view.setFrameSize_(A.NSMakeSize(600, max(632, self.y)))


class InklowApplication(NSObject):
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
        self.model_settings_open = False
        self.model_selection = None
        self.requested_pane = pane
        self.pane = "setup" if not config.setup_completed else (pane or "general")
        self.last_external_app = ""
        self.stat_period, self.stat_group = "week", "day"
        self.selected_rule = ""
        self.last_status = None
        self.last_stats_refresh = 0.0
        self.stopping = False
        self.window = None
        self.timer = None
        self.status_item = None
        self.menu = None
        self.overlay = None
        self.last_error = ""
        return self

    def applicationDidFinishLaunching_(self, notification):
        app = A.NSApplication.sharedApplication()
        app.setActivationPolicy_(A.NSApplicationActivationPolicyAccessory)
        app.setApplicationIconImage_(A.NSImage.alloc().initWithContentsOfFile_(str(asset_path("app.png"))))
        self._build_main_menu()
        self._build_status_menu()
        self.overlay = CaretOverlay(self.controller.invoke, self.menu, self.controller.config.show_accessory)
        if not self.no_tray:
            try:
                self.status_item = A.NSStatusBar.systemStatusBar().statusItemWithLength_(A.NSVariableStatusItemLength)
                if self.status_item is None or self.status_item.button() is None:
                    raise RuntimeError("The macOS menu bar is unavailable. Settings will stay accessible instead.")
                self.status_item.setMenu_(self.menu)
                _identify(self.status_item.button(), "inklow.statusItem", "Inklow menu")
            except Exception as exc:
                self.no_tray = True
                self.last_error = str(exc)
        try:
            self.controller.start()
        except Exception as exc:
            self.last_error = str(exc)
        self.timer = NSTimer.timerWithTimeInterval_target_selector_userInfo_repeats_(0.1, self, "tick:", None, True)
        NSRunLoop.mainRunLoop().addTimer_forMode_(self.timer, NSRunLoopCommonModes)
        self._refresh_status()
        if not self.controller.config.setup_completed or self.requested_pane or self.no_tray or self.status_item is None:
            self.show_settings(self.pane)
            if self.last_error:
                self._error(self.last_error)

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
            try:
                callback(sender)
            except Exception as exc:
                self._error(str(exc))

    @objc.python_method
    def _button(self, title, identifier, callback):
        button = A.NSButton.buttonWithTitle_target_action_(title, None, None)
        _identify(button, identifier, title)
        return self._bind(button, lambda sender: callback())

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
        self._menu_item(application, "Quit Inklow", "inklow.main.quit", lambda: A.NSApplication.sharedApplication().terminate_(None), "q")
        root.setSubmenu_(application)
        edit = self._menu_item(main, "Edit", "inklow.editMenu")
        edits = A.NSMenu.alloc().initWithTitle_("Edit")
        for title, selector, key in (("Undo", "undo:", "z"), ("Cut", "cut:", "x"), ("Copy", "copy:", "c"), ("Paste", "paste:", "v"), ("Select All", "selectAll:", "a")):
            item = A.NSMenuItem.alloc().initWithTitle_action_keyEquivalent_(title, selector, key)
            edits.addItem_(item)
        edit.setSubmenu_(edits)
        A.NSApplication.sharedApplication().setMainMenu_(main)
        self.main_menu = main

    @objc.python_method
    def _build_status_menu(self):
        self.menu = A.NSMenu.alloc().initWithTitle_(APP_NAME)
        self.menu.setAutoenablesItems_(False)
        self.menu.setDelegate_(self)
        self.status_line = self._menu_item(self.menu, "Inklow", "inklow.menu.status")
        self.status_line.setEnabled_(False)
        self.pause_item = self._menu_item(self.menu, "Pause Inklow", "inklow.menu.toggle", self.controller.toggle_enabled)
        self.current_item = self._menu_item(self.menu, "Enable in Current App", "inklow.menu.currentApp", self.controller.toggle_current_app)
        pause = self._menu_item(self.menu, "Pause for", "inklow.menu.pause")
        submenu = A.NSMenu.alloc().initWithTitle_("Pause for")
        for minutes in (5, 15, 60):
            self._menu_item(submenu, f"{minutes} minutes", f"inklow.menu.pause.{minutes}", lambda m=minutes: self.controller.pause(m))
        pause.setSubmenu_(submenu)
        self._menu_item(self.menu, "Suggest Now", "inklow.menu.force", self.controller.force)
        self.menu.addItem_(A.NSMenuItem.separatorItem())
        self.stats_line = self._menu_item(self.menu, "Today", "inklow.menu.stats", lambda: self.show_settings("statistics"))
        self.menu.addItem_(A.NSMenuItem.separatorItem())
        self._menu_item(self.menu, "Settings…", "inklow.menu.settings", lambda: self.show_settings("general"), ",")
        self._menu_item(self.menu, "App Settings…", "inklow.menu.appSettings", lambda: self.show_settings("appSettings"))
        self._menu_item(self.menu, "Setup & Permissions…", "inklow.menu.setup", lambda: self.show_settings("setup"))
        self.menu.addItem_(A.NSMenuItem.separatorItem())
        self._menu_item(self.menu, "Quit Inklow", "inklow.menu.quit", lambda: A.NSApplication.sharedApplication().terminate_(None), "q")
        self.menu_binding_limit = self.next_tag

    def menuWillOpen_(self, menu):
        try:
            self._refresh_status()
            self._refresh_menu_stats()
        except Exception as exc:
            self.status_line.setTitle_(f"Inklow — {str(exc)[:100]}")

    def tick_(self, timer):
        if self.stopping:
            return
        try:
            self.controller.tick()
            for event in self.controller.drain_events():
                kind = event.get("type")
                if kind == "suggestion":
                    self.overlay.present(event)
                elif kind == "caret":
                    self.overlay.caret(event.get("rect"))
                elif kind == "clear":
                    self.overlay.clear(hide_accessory=bool(event.get("hide_accessory")))
                elif kind == "settings":
                    self.show_settings(event.get("pane", "general"))
            self._refresh_status()
            if self.window and self.window.isVisible() and self.pane == "statistics" and time.monotonic() - self.last_stats_refresh > 5:
                self._refresh_statistics()
        except Exception as exc:
            message = str(exc)
            if message != self.last_error:
                self.last_error = message
                self.status_line.setTitle_(f"Inklow — {message[:110]}")
                self.status_line.setToolTip_(message)

    @objc.python_method
    def _refresh_menu_stats(self):
        stats = self.controller.stats("week", "day")
        today = stats.get("today", {})
        self.stats_line.setTitle_(f"Today: {int(today.get('completions', 0))} completions · {int(today.get('words', 0))} words")

    @objc.python_method
    def _refresh_status(self):
        status = self.controller.status()
        enabled = bool(status.get("enabled"))
        state = str(status.get("state", "idle"))
        message = str(status.get("message") or state.capitalize())
        self.status_line.setTitle_(f"Inklow — {message[:100]}")
        self.status_line.setToolTip_(message)
        self.pause_item.setTitle_("Pause Inklow" if enabled else "Resume Inklow")
        app = str(status.get("current_app", ""))
        domain = str(status.get("current_domain", ""))
        if app:
            self.last_external_app = app
        rules = self.controller.config.app_rules
        rule = rules.get(f"app:{app}", {})
        active = rule.get("enabled", self.controller.config.enabled)
        if domain:
            active = rules.get(f"domain:{domain}", {}).get("enabled", active)
        active = bool(status.get("current_app_enabled", active))
        self.current_item.setTitle_(("Temporarily Disable" if active else "Temporarily Enable") + f" in {app or 'Current App'}")
        self.current_item.setEnabled_(bool(app))
        self.current_item.setState_(A.NSControlStateValueOn if active and app else A.NSControlStateValueOff)
        icon_state = "active" if enabled else "paused"
        if self.status_item and (self.last_status is None or self.last_status.get("enabled") != enabled):
            image = A.NSImage.alloc().initWithContentsOfFile_(str(asset_path(f"tray/{icon_state}-template-18.png")))
            image.setTemplate_(True)
            self.status_item.button().setImage_(image)
            self.status_item.button().setToolTip_(f"Inklow — {message}")
        self.overlay.configure(self.controller.config.show_accessory)
        for field in ("enabled", "global_enabled"):
            control = self.controls.get(field)
            if control:
                control.setState_(A.NSControlStateValueOn if getattr(self.controller.config, field) else A.NSControlStateValueOff)
        if self.window and self.window.isVisible():
            if self.pane == "setup":
                self._refresh_setup()
            if self.pane == "general":
                self._refresh_download(status)
        self.last_status = status

    @objc.python_method
    def _error(self, message):
        alert = A.NSAlert.alloc().init()
        alert.setMessageText_("Inklow couldn’t complete that change")
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
        return alert.runModal() == A.NSAlertFirstButtonReturn

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
        pane = {"app-settings": "appSettings", "app_settings": "appSettings"}.get(pane, pane)
        if self.overlay:
            self.overlay.clear(hide_accessory=True)
        if pane not in [item[0] for item in PANES]:
            pane = "general"
        if self.window is None:
            self._build_window()
        self._show_pane(pane)
        self.window.makeKeyAndOrderFront_(None)
        A.NSApplication.sharedApplication().activateIgnoringOtherApps_(True)

    @objc.python_method
    def _build_window(self):
        style = A.NSWindowStyleMaskTitled | A.NSWindowStyleMaskClosable | A.NSWindowStyleMaskMiniaturizable | A.NSWindowStyleMaskFullSizeContentView
        self.window = A.NSWindow.alloc().initWithContentRect_styleMask_backing_defer_(A.NSMakeRect(0, 0, 800, 632), style, A.NSBackingStoreBuffered, False)
        self.window.setTitle_("Inklow Settings")
        self.window.setIdentifier_("inklow.settings")
        self.window.setAccessibilityIdentifier_("inklow.settings")
        self.window.setTitleVisibility_(A.NSWindowTitleHidden)
        self.window.setTitlebarAppearsTransparent_(True)
        self.window.setReleasedWhenClosed_(False)
        self.window.setDelegate_(self)
        self.window.center()
        root = A.NSView.alloc().initWithFrame_(A.NSMakeRect(0, 0, 800, 632))
        self.window.setContentView_(root)
        sidebar = A.NSVisualEffectView.alloc().initWithFrame_(A.NSMakeRect(0, 0, 200, 632))
        sidebar.setMaterial_(A.NSVisualEffectMaterialSidebar)
        sidebar.setBlendingMode_(A.NSVisualEffectBlendingModeBehindWindow)
        root.addSubview_(sidebar)
        source = TableSource.alloc().init().configure(
            [{"id": key, "label": title, "symbol": symbol} for key, title, symbol in PANES],
            ["label"], lambda row: self._show_pane(row["id"]), True)
        table = A.NSTableView.alloc().initWithFrame_(A.NSMakeRect(0, 0, 184, 540))
        column = A.NSTableColumn.alloc().initWithIdentifier_("label")
        column.setWidth_(184)
        table.addTableColumn_(column)
        table.setHeaderView_(None)
        table.setRowHeight_(32)
        table.setIntercellSpacing_(A.NSMakeSize(0, 1))
        table.setSelectionHighlightStyle_(A.NSTableViewSelectionHighlightStyleSourceList)
        table.setBackgroundColor_(A.NSColor.clearColor())
        table.setDataSource_(source)
        table.setDelegate_(source)
        table.setAllowsEmptySelection_(False)
        _identify(table, "inklow.sidebar", "Settings categories")
        scroll = A.NSScrollView.alloc().initWithFrame_(A.NSMakeRect(8, 35, 184, 552))
        scroll.setDrawsBackground_(False)
        scroll.setHasVerticalScroller_(False)
        scroll.setDocumentView_(table)
        sidebar.addSubview_(scroll)
        self.sidebar_source, self.sidebar_table = source, table
        self.content_scroll = A.NSScrollView.alloc().initWithFrame_(A.NSMakeRect(200, 0, 600, 632))
        self.content_scroll.setHasVerticalScroller_(True)
        self.content_scroll.setAutohidesScrollers_(True)
        self.content_scroll.setDrawsBackground_(False)
        root.addSubview_(self.content_scroll)

    @objc.python_method
    def _remember_drafts(self):
        for key, editor in self.editors.items():
            self.drafts[key] = str(editor.string())

    @objc.python_method
    def _show_pane(self, pane):
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
            # Keep only menu bindings; released pane controls must not retain actions.
            self.bindings = {tag: action for tag, action in self.bindings.items() if tag <= self.menu_binding_limit}
            self.pane = pane
            self.sidebar_table.selectRowIndexes_byExtendingSelection_(NSIndexSet.indexSetWithIndex_([item[0] for item in PANES].index(pane)), False)
            view = FlippedView.alloc().initWithFrame_(A.NSMakeRect(0, 0, 600, 632))
            self.content_scroll.setDocumentView_(view)
            self.form = Form(self, view, pane)
            getattr(self, f"_pane_{pane}")()
            self.form.finish()
            view.scrollPoint_(A.NSMakePoint(0, 0))
        finally:
            self.rendering = False

    def windowShouldClose_(self, sender):
        self._remember_drafts()
        if self.no_tray or self.status_item is None:
            A.NSApplication.sharedApplication().terminate_(None)
        return True

    @objc.python_method
    def _permission_state(self):
        import ApplicationServices as AX
        import Quartz
        return {
            "accessibility": bool(AX.AXIsProcessTrusted()),
            "input_monitoring": bool(Quartz.CGPreflightListenEventAccess()),
            "screen_recording": bool(Quartz.CGPreflightScreenCaptureAccess()),
        }

    @objc.python_method
    def _refresh_setup(self):
        values = self._permission_state()
        for key, control in getattr(self, "permission_labels", {}).items():
            control.setStringValue_("Granted" if values[key] else "Not granted")
        if hasattr(self, "setup_status"):
            status = self.controller.status()
            self.setup_status.setStringValue_(str(status.get("native_status", "")))
        if hasattr(self, "setup_model"):
            config = self.controller.config
            installed = bool(config.model_path and Path(config.model_path).is_file()) if config.backend == "gguf" else False
            self.setup_model.setStringValue_("Local model selected" if installed else ("Local server selected" if config.backend != "gguf" else "Choose or download a model"))

    @objc.python_method
    def _pane_setup(self):
        f = self.form
        f.section("Set up Inklow")
        f.row("Private, local predictive typing", "Inklow lives in the menu bar. It never opens a document editor. Secure, excluded and unsupported text fields are not read.", height=86)
        f.note("Grant only the permissions you want. When launching from source, macOS may list your terminal or Python host instead of Inklow. Permissions can require restarting Inklow.")
        f.section("macOS Permissions")
        self.permission_labels = {}
        for key, title, detail in (
            ("accessibility", "Accessibility", "Required to read a supported text field and insert at its caret. System Settings → Privacy & Security → Accessibility."),
            ("input_monitoring", "Input Monitoring", "Required for global acceptance shortcuts. System Settings → Privacy & Security → Input Monitoring."),
            ("screen_recording", "Screen & System Audio Recording", "Optional. Only used after you explicitly enable foreground screen context in Context. No screenshots are saved."),
        ):
            button = self._button("Open Settings…", f"inklow.setup.{key}", lambda k=key: system.open_permissions(k))
            f.row(title, detail, button, height=92, control_width=140)
            label = _label("", A.NSMakeRect(14, f.row_y - 29, 200, 18), small=True)
            _identify(label, f"inklow.setup.{key}.status")
            f.group_view.addSubview_(label)
            self.permission_labels[key] = label
        f.section("Completion model")
        self.setup_model = _label("", A.NSMakeRect(0, 0, 220, 22), small=True)
        f.row("Local inference", "Download a verified model, choose an existing GGUF file, or configure an already-running local server in General.", self._button("Choose Model…", "inklow.setup.model", lambda: self.show_settings("general")), control_width=140, height=80)
        f.row("Model status", control=self.setup_model)
        f.section("Keyboard")
        f.row("macOS inline predictive text", "If macOS predictions conflict, turn off Show inline predictive text in Keyboard → Text Input → Edit. Inklow does not silently change this setting.", self._button("Open Keyboard…", "inklow.setup.keyboard", lambda: system.open_permissions("keyboard")), control_width=145, height=96)
        self.setup_status = _label("", A.NSMakeRect(0, 0, 552, 54), small=True)
        f.custom(self.setup_status, 54)
        f.section()
        f.row("Setup can be revisited at any time", "You can continue without a model or permission; suggestions stay unavailable until prerequisites are ready.", self._button("Done", "inklow.setup.done", self._complete_setup), control_width=95, height=80)
        self._refresh_setup()

    @objc.python_method
    def _complete_setup(self):
        self._change(setup_completed=True)
        if not self.no_tray:
            self.window.orderOut_(None)
        else:
            self.show_settings("general")

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
        self.model_description = self.download_progress = self.download_label = None
        f.section()
        login = self._switch("launch_at_login", "Launch automatically at login", callback=self._autostart)
        login.setState_(A.NSControlStateValueOn if system.launch_at_login_state() else A.NSControlStateValueOff)
        self._switch("show_accessory", "Show accessory button", "Quick access to Inklow’s menu beside the text field.")
        f.section("AI Settings")
        selected = ""
        if self.controller.config.backend == "gguf":
            self.model_inventory = self.controller.models()
            options = [("Custom model", "")]
            for model in self.model_inventory:
                label = str(model["label"]).split(" — ", 1)[-1]
                options.append((("✓ " if model.get("installed") else "↓ ") + label, str(model["id"])))
            fallback = str(self.model_inventory[0]["id"]) if self.model_inventory and not self.controller.config.model_path else ""
            selected = self.model_selection if self.model_selection is not None else next(
                (str(m["id"]) for m in self.model_inventory if str(m.get("path", "")) == self.controller.config.model_path), fallback)
            self._popup("model_preset", "AI model", options, value=selected, callback=self._select_model)
        else:
            f.row("AI model", str(self.controller.config.model))
        actions = FlippedView.alloc().initWithFrame_(A.NSMakeRect(0, 0, 292, 28))
        for x, width, title, identifier, callback in (
            (0, 138, "Manage Models…", "manageModels", self._toggle_model_settings),
            (146, 146, "Reveal in Finder", "revealModels", system.reveal_models),
        ):
            button = self._button(title, f"inklow.general.{identifier}", callback)
            button.setFrame_(A.NSMakeRect(x, 0, width, 28))
            actions.addSubview_(button)
        f.row("Model files", control=actions, control_width=292)
        f.section("Completion Settings")
        self._switch("enabled", "Enable completions by default", "Turn this off to enable Inklow only in selected apps.")
        self._switch("complete_inside_text", "Complete inside existing text", "Also suggest when there is text after the caret.")
        self._popup("completion_length", "Maximum completion length",
                    [("Short (~1–2 words)", "short"), ("Medium (~2–4 words)", "medium"), ("Long (~5–8 words)", "long")])
        self._switch("global_enabled", "Enable Inklow")
        if self.model_settings_open:
            f.end_group()
            self.model_settings_top = f.y
            self._model_controls(selected)

    @objc.python_method
    def _toggle_model_settings(self):
        self.model_settings_open = not self.model_settings_open
        self._show_pane("general")
        if self.model_settings_open:
            self.content_scroll.documentView().scrollPoint_(A.NSMakePoint(0, self.model_settings_top))

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
    def _select_model(self, preset_id, apply=True):
        model = next((item for item in self.model_inventory if str(item["id"]) == preset_id), None)
        if self.model_description is not None:
            self.model_description.setStringValue_(str(model.get("description", "")) if model else "Use Choose GGUF to select your own local model.")
        if apply:
            self.model_selection = preset_id
            if model and model.get("installed"):
                self.controller.set_model(str(model["path"]), str(model.get("prompt_style", "continuation")))
                self.model_selection = None
            elif model and not self.model_settings_open:
                self._toggle_model_settings()
                return
            elif model is None:
                self._choose_model()
                return
        self._refresh_download(self.controller.status())

    @objc.python_method
    def _download(self):
        model = self._selected_model()
        if model is None:
            raise ValueError("Select a model preset before downloading.")
        current = next((item for item in self.controller.models() if item["id"] == model["id"]), model)
        if current.get("installed"):
            self.controller.set_model(str(current["path"]), str(current.get("prompt_style", "continuation")))
            self.model_selection = None
            self._show_pane("general")
        else:
            self.controller.download_model(str(model["id"]))

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
            self.controller.set_model(path, self.controller.config.prompt_style)
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
            message = "Download verified. Select this preset and press Use Model to activate it."
        if not message:
            message = "No download in progress. Installed presets are ready to use when selected."
        self.download_label.setStringValue_(message)

    @objc.python_method
    def _context_consent(self, field, value):
        if value and not self._confirm("Use local context?", "Inklow will read " + ("the clipboard" if field == "clipboard_context" else "the supported foreground app’s screen using local OCR") + " only for eligible suggestion requests. It is not saved or sent to a remote service. You can turn this off at any time."):
            control = self.controls.get(field)
            if control:
                control.setState_(A.NSControlStateValueOff)
            return
        self._change(**{field: value})

    @objc.python_method
    def _pane_context(self):
        self.form.section("Screen Context")
        self._switch("screen_context", "Use foreground screen context", "Optional, on-device OCR of the eligible foreground app. Permission is required; no screenshot artifacts or unrelated windows are retained.", callback=lambda value: self._context_consent("screen_context", value))
        self.form.row("Screen recording permission", "Enabling the switch does not grant macOS permission.", self._button("Open Settings…", "inklow.context.permission", lambda: system.open_permissions("screen_recording")), control_width=140)
        self.form.section("Clipboard Context")
        self._switch("clipboard_context", "Use clipboard for context", "Optional. Clipboard text is read only for an eligible request and passed to your local model, not stored in history.", callback=lambda value: self._context_consent("clipboard_context", value))
        self.form.note("Restrictions are checked before context is read. Website rules require an observable native URL. If a browser hides its URL while a website is blocked, Inklow pauses that browser rather than guessing.")

    @objc.python_method
    def _history_toggle(self, value):
        if value and not self._confirm("Enable encrypted local history?", "Accepted writing will be stored encrypted on this Mac to personalize future suggestions. Your OS secure storage must be available; Inklow never falls back to plaintext."):
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
            text = "Encrypted storage is not initialized. Enable history to initialize secure storage."
        else:
            text = f"{int(info.get('count', 0))} encrypted records · {int(info.get('bytes', 0)):,} bytes on disk"
        if hasattr(self, "history_label"):
            self.history_label.setStringValue_(text)

    @objc.python_method
    def _delete_history(self, app_id=None):
        if self._confirm("Delete local writing history?", "This permanently deletes " + (f"encrypted history for {app_id}." if app_id else "all encrypted personalization history. Aggregate statistics are unchanged.")):
            self.controller.delete_history(app_id=app_id)
            self._update_history_info()

    @objc.python_method
    def _instructions_editor(self, key, saved_value, *, app_rule=False):
        self.form.end_group()
        scroll = A.NSScrollView.alloc().initWithFrame_(A.NSMakeRect(0, 0, 552, 152))
        scroll.setHasVerticalScroller_(True)
        scroll.setBorderType_(A.NSBezelBorder)
        editor = A.NSTextView.alloc().initWithFrame_(A.NSMakeRect(0, 0, 532, 150))
        editor.setFont_(A.NSFont.systemFontOfSize_(13))
        editor.setRichText_(False)
        editor.setAllowsUndo_(True)
        editor.setVerticallyResizable_(True)
        editor.setHorizontallyResizable_(False)
        editor.setTextContainerInset_(A.NSMakeSize(9, 8))
        editor.textContainer().setWidthTracksTextView_(True)
        editor.setString_(self.drafts.get(key, saved_value))
        editor.setDelegate_(self)
        _identify(editor, f"inklow.{self.pane}.instructions", "Custom instructions")
        scroll.setDocumentView_(editor)
        self.editors[key] = editor
        self.form.custom(scroll, 152)
        container = FlippedView.alloc().initWithFrame_(A.NSMakeRect(0, 0, 552, 32))
        save = self._button("Save Changes", f"inklow.{self.pane}.saveInstructions", lambda: self._save_instructions(key, app_rule))
        revert = self._button("Revert", f"inklow.{self.pane}.revertInstructions", lambda: self._revert_instructions(key, app_rule))
        save.setFrame_(A.NSMakeRect(304, 0, 145, 28))
        revert.setFrame_(A.NSMakeRect(452, 0, 100, 28))
        container.addSubview_(save)
        container.addSubview_(revert)
        self.instruction_buttons = (save, revert, key, app_rule)
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
        save, revert, key, app_rule = self.instruction_buttons
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
    def _pane_personalization(self):
        self.form.section("Encrypted Local History")
        self._switch("record_history", "Remember accepted writing by default", "Opt-in encrypted history; the key stays in OS secure storage. App recording overrides take precedence, including when this default is off.", callback=self._history_toggle)
        self._switch("record_unaccepted", "Also remember unaccepted writing", "Only recorded when encrypted history is enabled. This includes captured text that you did not accept as a completion.")
        self._number("personalization_strength", "Personalization strength", "0–100. Higher values allow more relevant encrypted history to inform a local request.", 0, 100)
        self.history_label = _label("", A.NSMakeRect(0, 0, 340, 43), small=True)
        self.form.row("Encrypted history", control=self.history_label, control_width=360, height=65)
        self.form.row("Delete writing history", "Irreversible. Deleting writing history does not delete aggregate statistics.", self._button("Delete History…", "inklow.personalization.deleteHistory", self._delete_history), control_width=145)
        self._update_history_info()
        self.form.section("Custom Instructions")
        self.form.row("Your writing preferences", "Instructions apply to eligible local requests. App and domain instructions can override them. Edits are not applied until Save Changes.", height=85)
        self._instructions_editor("global_instructions", self.controller.config.custom_instructions)

    @objc.python_method
    def _pane_emoji(self):
        self.form.section("Emoji Suggestions")
        self._switch("emoji_enabled", "Search emoji with a colon", "Type a name such as :smile in an eligible field. Choose a numbered result to replace only the typed emoji query.")
        self._switch("emoticons_enabled", "Replace typed emoticons", "Offer a real emoji replacement for supported emoticons such as :-). Accept explicitly; text is not silently rewritten.")
        self.form.section("Emoji Preferences")
        self._popup("emoji_skin_tone", "Skin tone", [("Default", 0), ("Light", 1), ("Medium-light", 2), ("Medium", 3), ("Medium-dark", 4), ("Dark", 5)])
        self._popup("emoji_gender", "Preferred gender", [("Neutral", "neutral"), ("Female", "female"), ("Male", "male")])
        self._switch("emoji_include_neutral", "Include default skin-tone variants", "Include unmodified emoji alongside the selected skin tone. Gender is chosen separately above.")
        self.form.note("The numbered chooser uses keys 1–9 or a click without taking focus from your text field. Emoji lookup is local and does not require an inference worker.")

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
                           lambda recording: self.controller.bridge.set_shortcuts_enabled(not recording))
        self.form.row(title, detail, recorder, height=87)

    @objc.python_method
    def _pane_shortcuts(self):
        self.form.section("Accepting Completions")
        self._record("word", "Complete only the next word", "Click the recorder, then press the actual shortcut. Delete clears it; Escape cancels recording. Repeated acceptance continues a suggestion.")
        self._switch("trailing_space", "Include trailing space", "Accepting one word also inserts its following space when available.")
        self._record("all", "Trigger full completion", "AboveTab is the physical key immediately above Tab, independent of keyboard layout. Default: that key without modifiers.")
        self._popup("punctuation_mode", "Include trailing punctuation",
                    [("Only with full completion", "full"), ("With either shortcut", "always"), ("Never", "never")],
                    "Punctuation left out stays in the suggestion, so pressing again inserts it.")
        self.form.section("When You Press Escape")
        self._popup("escape_behavior", "Escape action", [("Pause briefly", "pause"), ("Disable in current app", "app"), ("Pass through", "passthrough")], "The shortcut handler consumes Escape only when it performs an active suggestion action; otherwise your app receives it normally.")
        self._number("escape_pause_seconds", "Brief pause (seconds)", "How long to pause suggestions after Escape; 1–3600.", 1, 3600)
        self.form.section("Activation & Toggles")
        self._record("force", "Force-activate completions", "Ask for a suggestion at the current eligible caret, even if automatic typing detection has not fired.")
        self._record("app_toggle", "Toggle current app", "Temporarily switch completions in the current application without changing its saved rule.")
        self._record("toggle", "Toggle Inklow globally", "Optional. Pause or resume everywhere. Pause releases the inference worker.")
        self.form.section("Word Tools")
        self._record("alternatives", "Alternative words", "Optional shortcut for the numbered alternatives chooser. Enable alternatives in Labs.")
        self._record("synonyms", "Selected-text synonyms", "Optional shortcut for alternatives to an explicitly selected word. Enable synonyms in Labs.")

    @objc.python_method
    def _pane_battery(self):
        self.form.section("On Battery Power")
        self._switch("battery_on_demand", "Generate only on demand", "On battery, generate after the force-completion shortcut instead of automatically after typing.")
        self._switch("battery_smaller_model", "Prefer a smaller installed model", "Use the smaller verified installed preset on battery when available; no automatic downloads.")
        self._switch("battery_shorter", "Prefer shorter completions", "Reduce generated completion length on battery.")
        self.form.section("Inference Memory")
        self._number("idle_release_seconds", "Release model after idle (seconds)", "0 keeps the worker ready. 1–3600 releases the entire inference process after inactivity. Pause and Quit release it immediately.", 0, 3600)
        self.form.row("Current power source", control=_label("Battery" if system.on_battery() else "External power / desktop", A.NSMakeRect(0, 0, 220, 24), small=True))
        self.form.note("Inklow does not load a model at launch. A worker is started only when an eligible request needs inference.")

    @objc.python_method
    def _pane_appSettings(self):
        f = self.form
        f.section("Apps & Websites")
        f.row("Privacy rules are checked before reading text", "Website rules require the browser’s native URL. A blocked website rule pauses recognized browsers when their URL is hidden. Application identifiers match exactly.", height=100)
        container = FlippedView.alloc().initWithFrame_(A.NSMakeRect(0, 0, 552, 38))
        kind = A.NSPopUpButton.alloc().initWithFrame_pullsDown_(A.NSMakeRect(0, 0, 112, 27), False)
        kind.addItemsWithTitles_(["Application", "Website"])
        _identify(kind, "inklow.appSettings.ruleKind", "New rule type")
        entry = A.NSTextField.alloc().initWithFrame_(A.NSMakeRect(118, 0, 324, 25))
        entry.setPlaceholderString_("com.example.App or example.com")
        _identify(entry, "inklow.appSettings.newRule", "Application identifier or website domain")
        add = self._button("Add", "inklow.appSettings.addRule", lambda: self._add_rule(int(kind.indexOfSelectedItem()), str(entry.stringValue())))
        add.setFrame_(A.NSMakeRect(448, 0, 104, 28))
        for control in (kind, entry, add):
            container.addSubview_(control)
        f.custom(container, 38)
        f.section()
        f.row("Use the current app", "Adds its exact native identifier; no document text is read.", self._button("Add Current App", "inklow.appSettings.addCurrentApp", self._add_current_app), control_width=160)
        f.row("Choose an installed app", "Select an .app bundle to read its bundle identifier.", self._button("Choose App…", "inklow.appSettings.chooseApp", self._choose_app), control_width=160)
        keys = sorted(self.controller.config.app_rules)
        if not keys:
            self.selected_rule = ""
            f.note("No app or website overrides. All eligible apps inherit your General and Personalization settings.")
            return
        if self.selected_rule not in keys:
            self.selected_rule = keys[0]
        f.section("Rule Overrides")
        self._popup("selected_rule", "Application or website", [(key, key) for key in keys], value=self.selected_rule, callback=self._select_rule)
        f.row("Delete this rule", "Removing a rule restores inherited behavior; it does not delete history.", self._button("Delete Rule", "inklow.appSettings.deleteRule", self._delete_rule), control_width=140)
        rule = self.controller.config.app_rules[self.selected_rule]
        for field, title, detail in (
            ("enabled", "Completions", "Inherit uses the app override for a website, or General otherwise."),
            ("tab_enabled", "Tab acceptance", "Control acceptance with Tab in this app or website; other shortcuts remain available."),
            ("record_history", "Encrypted writing history", "Enable requires secure storage, even when global history is disabled. Off prohibits recording here."),
        ):
            value = "inherit" if field not in rule else ("yes" if rule[field] else "no")
            self._popup(f"rule_{field}", title, [("Inherit", "inherit"), ("Enabled", "yes"), ("Disabled", "no")], detail, value=value, callback=lambda choice, key=field: self._rule_override(key, choice))
        delete_history = self._button("Delete History…", "inklow.appSettings.deleteHistory",
                                      lambda: self._delete_history(self.selected_rule[4:]))
        delete_history.setEnabled_(self.selected_rule.startswith("app:"))
        f.row("Delete this app’s encrypted history", "Domain history is attributed to its native application. Select an app rule for scoped deletion.", delete_history, control_width=145)
        f.section("Custom Instructions")
        f.row("Override inherited instructions", "Leave empty to inherit. Edits are applied only after Save Changes. Your unsaved draft is preserved while switching panes.", height=85)
        self._instructions_editor(f"rule:{self.selected_rule}", str(rule.get("custom_instructions", "")), app_rule=True)

    @objc.python_method
    def _normalize_domain(self, value):
        text = value.strip()
        parsed = urlsplit(text if "://" in text else "https://" + text)
        if parsed.username or parsed.password or not parsed.hostname:
            raise ValueError("Enter a website hostname such as example.com.")
        domain = parsed.hostname.rstrip(".").lower().encode("idna").decode("ascii")
        if any(character.isspace() for character in domain) or "." not in domain or parsed.port:
            raise ValueError("Enter a complete website hostname without a port.")
        return domain

    @objc.python_method
    def _add_rule(self, kind, value):
        value = value.strip()
        if not value:
            raise ValueError("Enter an application identifier or website domain.")
        key = "domain:" + self._normalize_domain(value) if kind == 1 else "app:" + value
        rules = deepcopy(self.controller.config.app_rules)
        if key not in rules:
            rules[key] = {}
            self._change(app_rules=rules)
        self.selected_rule = key
        self._show_pane("appSettings")

    @objc.python_method
    def _add_current_app(self):
        app = self.last_external_app
        if not app:
            raise ValueError("Switch to the target application first, then open Inklow’s App Settings from its menu.")
        self._add_rule(0, app)

    @objc.python_method
    def _choose_app(self):
        panel = A.NSOpenPanel.openPanel()
        panel.setCanChooseFiles_(True)
        panel.setCanChooseDirectories_(False)
        panel.setTreatsFilePackagesAsDirectories_(False)
        panel.setAllowedFileTypes_(["app"])
        panel.setAllowsMultipleSelection_(False)
        if panel.runModal() == A.NSModalResponseOK:
            bundle = NSBundle.bundleWithPath_(str(panel.URL().path()))
            app_id = str(bundle.bundleIdentifier() or "") if bundle else ""
            if not app_id:
                raise ValueError("The selected application has no bundle identifier; enter its exact executable identifier instead.")
            self._add_rule(0, app_id)

    @objc.python_method
    def _select_rule(self, key):
        self.selected_rule = key
        self._show_pane("appSettings")

    @objc.python_method
    def _rule_change(self, **changes):
        rules = deepcopy(self.controller.config.app_rules)
        if self.selected_rule not in rules:
            raise ValueError("Select an existing app or website rule.")
        rules[self.selected_rule].update(changes)
        self._change(app_rules=rules)

    @objc.python_method
    def _rule_override(self, field, choice):
        rules = deepcopy(self.controller.config.app_rules)
        if choice == "inherit":
            rules[self.selected_rule].pop(field, None)
        else:
            rules[self.selected_rule][field] = choice == "yes"
        self._change(app_rules=rules)

    @objc.python_method
    def _delete_rule(self):
        rules = deepcopy(self.controller.config.app_rules)
        rules.pop(self.selected_rule, None)
        self._change(app_rules=rules)
        self.drafts.pop(f"rule:{self.selected_rule}", None)
        self.selected_rule = ""
        self.editors = {}
        self._show_pane("appSettings")

    @objc.python_method
    def _pane_labs(self):
        self.form.section("Alternative Words")
        self._switch("alternatives_enabled", "Suggest alternative words", "Offer a numbered list at an eligible caret. Results are generated locally and inserted only after you choose.")
        self._popup("alternatives_mode", "Show alternatives", [("With shortcut", "shortcut"), ("After typing pauses", "pause")])
        self._number("alternatives_delay_ms", "Pause delay (milliseconds)", "How long to wait before requesting alternatives in pause mode.", 100, 10000)
        self.form.section("Selected-Text Synonyms")
        self._switch("synonyms_enabled", "Suggest contextual synonyms", "Select a word in a supported field. A numbered choice replaces only the exact revalidated selection.")
        self._popup("synonyms_mode", "Show synonyms", [("After selection", "selection"), ("With shortcut", "shortcut")])
        self._number("synonyms_delay_ms", "Selection delay (milliseconds)", "Delay before requesting synonyms after selection.", 100, 10000)
        self.form.section("Typing Timing")
        self._number("debounce_ms", "Completion delay (milliseconds)", "Wait after typing before automatic inference; 100–5000.", 100, 5000)
        self._number("min_chars", "Minimum preceding characters", "Do not request an automatic completion before this much context; 1–1000.", 1, 1000)
        self._number("context_chars", "Maximum local context characters", "Bound the text passed to the local model; 64–8000.", 64, 8000)
        self.form.note("Configure optional word-tool keyboard shortcuts in Shortcuts. Unsupported or private text fields never become eligible merely because a Labs tool is enabled.")

    @objc.python_method
    def _pane_statistics(self):
        self.form.section("Local Activity")
        self._switch("statistics_enabled", "Collect aggregate statistics", "Counts only successful insertions. No writing content is stored in aggregate statistics.")
        self.today_label = _label("", A.NSMakeRect(0, 0, 390, 25))
        self.form.row("Today", control=self.today_label, control_width=410)
        self.form.section("Completion Statistics")
        self._popup("stat_period", "Time range", [("Today", "today"), ("Last 7 Days", "week"), ("Last 30 Days", "month"), ("Last Year", "year"), ("All Time", "all")], value=self.stat_period, callback=self._stat_period)
        self._popup("stat_group", "Group by", [("Day", "day"), ("Application", "app")], value=self.stat_group, callback=self._stat_group)
        self.stats_source = TableSource.alloc().init().configure([], ["label", "completions", "words", "characters"])
        table = A.NSTableView.alloc().initWithFrame_(A.NSMakeRect(0, 0, 550, 230))
        table.setRowHeight_(27)
        for key, title, width in (("label", "Day / Application", 220), ("completions", "Completions", 107), ("words", "Words", 90), ("characters", "Characters", 110)):
            column = A.NSTableColumn.alloc().initWithIdentifier_(key)
            column.setTitle_(title)
            column.setWidth_(width)
            table.addTableColumn_(column)
        table.setDataSource_(self.stats_source)
        table.setDelegate_(self.stats_source)
        table.setUsesAlternatingRowBackgroundColors_(True)
        _identify(table, "inklow.statistics.table", "Actual accepted completions by day or application")
        scroll = A.NSScrollView.alloc().initWithFrame_(A.NSMakeRect(0, 0, 552, 250))
        scroll.setHasVerticalScroller_(True)
        scroll.setHasHorizontalScroller_(True)
        scroll.setBorderType_(A.NSBezelBorder)
        scroll.setDocumentView_(table)
        self.stats_table = table
        self.form.custom(scroll, 250)
        self.statistics_totals = _label("", A.NSMakeRect(0, 0, 552, 58), small=True)
        _identify(self.statistics_totals, "inklow.statistics.totals")
        self.form.custom(self.statistics_totals, 58)
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
    def _refresh_statistics(self):
        stats = self.controller.stats(self.stat_period, self.stat_group)
        self.stats_source.rows = stats.get("rows", [])
        self.stats_table.reloadData()
        today = stats.get("today", {})
        self.today_label.setStringValue_(f"{int(today.get('completions', 0))} completions · {int(today.get('words', 0))} words · {int(today.get('characters', 0))} characters")
        totals = stats.get("totals", {})
        self.statistics_totals.setStringValue_(f"Total: {int(totals.get('completions', 0))} completions · {int(totals.get('words', 0))} words · {int(totals.get('characters', 0))} characters\nEstimated typing time saved: {int(stats.get('estimated_seconds', 0))} seconds (estimate based on accepted characters).")
        self.last_stats_refresh = time.monotonic()

    @objc.python_method
    def _pane_about(self):
        self.form.section("Inklow")
        icon = A.NSImageView.alloc().initWithFrame_(A.NSMakeRect(0, 0, 552, 72))
        icon.setImage_(A.NSImage.alloc().initWithContentsOfFile_(str(asset_path("icons/app-128.png"))))
        icon.setImageScaling_(A.NSImageScaleProportionallyUpOrDown)
        self.form.custom(icon, 72)
        self.form.section()
        self.form.row(f"Inklow {__version__}", "Independent, local-first predictive typing. A menu utility, not an editor. This is an independent application.", height=80)
        self.form.row("Local by design", "No subscriptions, cloud history sync, telemetry or remote inference services. Local server integrations accept loopback endpoints only.", height=80)
        self.form.row("Typing safety", "Secure fields, excluded apps, unsupported controls and private app/domain rules are checked before text capture. Insertion revalidates the exact original target.", height=92)
        self.form.row("Permissions & setup", control=self._button("Open Setup…", "inklow.about.setup", lambda: self.show_settings("setup")), control_width=145)
        self.form.row("Quit Inklow", "Stops keyboard monitoring, cancels downloads and releases the inference worker.", self._button("Quit", "inklow.about.quit", lambda: A.NSApplication.sharedApplication().terminate_(None)), control_width=100)

    def applicationShouldTerminateAfterLastWindowClosed_(self, app):
        return False

    def applicationWillTerminate_(self, notification):
        self.shutdown()

    @objc.python_method
    def shutdown(self):
        if self.stopping:
            return
        self.stopping = True
        if self.window:
            self.window.makeFirstResponder_(None)
        if self.timer:
            self.timer.invalidate()
            self.timer = None
        if self.overlay:
            self.overlay.close()
        try:
            self.controller.stop()
        finally:
            if self.status_item:
                A.NSStatusBar.systemStatusBar().removeStatusItem_(self.status_item)
                self.status_item = None


def launch(config, *, background=False, no_tray=False, pane=None) -> int:
    """Run native AppKit from source or a frozen bundle, without a Dock icon."""
    app = A.NSApplication.sharedApplication()
    app.setActivationPolicy_(A.NSApplicationActivationPolicyAccessory)
    delegate = InklowApplication.alloc().init().configure(config, background, no_tray, pane)
    app.setDelegate_(delegate)
    try:
        app.run()
    except KeyboardInterrupt:
        pass
    finally:
        delegate.shutdown()
        app.setDelegate_(None)
    return 0
