"""macOS AX text controls and a conditional Quartz keyboard event tap."""
from __future__ import annotations

import os
import threading

from ..core import Snapshot
from .base import BridgeBase, Shortcut, utf16_to_index


class MacBridge(BridgeBase):
    def __init__(self) -> None:
        super().__init__()
        import ApplicationServices as AX
        import Quartz
        import CoreFoundation as CF
        from AppKit import NSWorkspace, NSAccessibilityRoleDescription
        self.ax, self.q, self.cf = AX, Quartz, CF
        self._workspace = NSWorkspace.sharedWorkspace()
        self._secure_description = NSAccessibilityRoleDescription("AXTextField", "AXSecureTextField")
        self._tap = None
        self._runloop = None
        self._thread: threading.Thread | None = None
        self._refresh_status()

    def _refresh_status(self) -> None:
        if not self.ax.AXIsProcessTrusted():
            self._set_status(
                "Accessibility permission required: System Settings → Privacy & Security → Accessibility. "
                "Enable Inklow (or the terminal/Python host launching it), then restart."
            )
        elif self._tap is None:
            self._set_status(
                "Accessibility available. Global shortcuts require Input Monitoring permission in "
                "System Settings → Privacy & Security for Inklow or its terminal/Python host. "
                "Supported targets: writable AXTextField/AXTextArea with an observable caret."
            )
        else:
            self._set_status("macOS AX and Quartz ready. Secure, read-only, and unsupported controls are excluded.")

    def status(self) -> str:
        if not self.ax.AXIsProcessTrusted():
            self._refresh_status()
        return super().status()

    def _attribute(self, element, name):
        error, value = self.ax.AXUIElementCopyAttributeValue(element, name, None)
        if error:
            return None
        return value

    def _settable(self, element, name: str) -> bool:
        error, result = self.ax.AXUIElementIsAttributeSettable(element, name, None)
        return not error and bool(result)

    def _focused(self):
        # Resolve the foreground process through NSWorkspace. System-wide AX
        # AXFocusedApplication can return kAXErrorCannotComplete even while the
        # foreground app's own AXFocusedUIElement is accessible.
        app_info = self._workspace.frontmostApplication()
        if app_info is None:
            return None
        pid = int(app_info.processIdentifier())
        if pid == os.getpid():
            return None
        executable_url = app_info.executableURL()
        executable = str(executable_url.path()) if executable_url else ""
        app_id = str(app_info.bundleIdentifier() or executable)
        if not app_id or self._excluded(app_id, executable):
            return None
        app = self.ax.AXUIElementCreateApplication(pid)
        field = self._attribute(app, "AXFocusedUIElement")
        if field is None:
            return None
        # Security/capability checks precede every read of AXValue.
        role = self._attribute(field, "AXRole")
        subrole = self._attribute(field, "AXSubrole")
        if role not in ("AXTextField", "AXTextArea") or subrole == "AXSecureTextField":
            return None
        # Qt exposes password state as a localized role description while
        # omitting AXSubrole. Ask AppKit for that description; do not guess
        # from the field value or depend on English UI strings.
        if self._attribute(field, "AXRoleDescription") == self._secure_description:
            return None
        if self._attribute(field, "AXProtectedContent"):
            return None
        # NSTextView omits AXEnabled; its writable setters establish editability.
        enabled = self._attribute(field, "AXEnabled")
        if (enabled is not None and not enabled) or not self._attribute(field, "AXFocused"):
            return None
        if not (self._settable(field, "AXSelectedText") or self._settable(field, "AXValue")):
            return None
        return field, pid, app_id

    def _capture(self):
        if self._stopped.is_set() or not self.ax.AXIsProcessTrusted():
            return None
        focused = self._focused()
        if focused is None:
            return None
        field, pid, app_id = focused
        marked_range = self._attribute(field, "AXMarkedTextRange")
        if marked_range is not None:
            valid, marked = self.ax.AXValueGetValue(marked_range, self.ax.kAXValueCFRangeType, None)
            if not valid or marked[1] > 0:
                return None
        native_range = self._attribute(field, "AXSelectedTextRange")
        if native_range is None:
            return None
        valid, native_range = self.ax.AXValueGetValue(native_range, self.ax.kAXValueCFRangeType, None)
        if not valid:
            return None
        location, length = int(native_range[0]), int(native_range[1])
        text = self._attribute(field, "AXValue")
        if not isinstance(text, str) or len(text) > 1_000_000:
            return None
        caret = utf16_to_index(text, location)
        end = utf16_to_index(text, location + length)
        rect = None
        bounds_range = self.ax.AXValueCreate(self.ax.kAXValueCFRangeType, (location, 0))
        error, bounds = self.ax.AXUIElementCopyParameterizedAttributeValue(
            field, "AXBoundsForRange", bounds_range, None
        )
        if not error and bounds is not None:
            valid, bounds = self.ax.AXValueGetValue(bounds, self.ax.kAXValueCGRectType, None)
            if valid and bounds[1][1] > 0:
                rect = (float(bounds[0][0]), float(bounds[0][1]), max(1.0, float(bounds[1][0])), float(bounds[1][1]))
        # CFEqual/hash on AXUIElement retains identity across fresh AX references.
        field_id = f"{pid}:{hash(field)}"
        return Snapshot(app_id, field_id, text, caret, end - caret, rect), field

    def capture(self) -> Snapshot | None:
        with self._lock:
            try:
                result = self._capture()
                return result[0] if result else None
            except Exception:
                return None

    def insert(self, snapshot: Snapshot, text: str) -> bool:
        if not text or "\x00" in text or snapshot.protected or snapshot.selection_length:
            return False
        with self._lock:
            try:
                current = self._capture()
                if current is None or current[0].key != snapshot.key:
                    return False
                # AXSelectedText replaces exactly the revalidated collapsed range;
                # unlike AXValue, it does not overwrite the rest of the document.
                field = current[1]
                if self._settable(field, "AXSelectedText"):
                    return self.ax.AXUIElementSetAttributeValue(field, "AXSelectedText", text) == 0
                # Some writable AX controls (including Qt) expose AXValue but
                # not an AXSelectedText setter. Type at the validated caret;
                # never use AXValue to replace the document or the clipboard.
                q = self.q
                if not self._settable(field, "AXValue") or not q.CGPreflightPostEventAccess():
                    return False
                flags = q.CGEventSourceFlagsState(q.kCGEventSourceStateCombinedSessionState)
                if flags & (q.kCGEventFlagMaskCommand | q.kCGEventFlagMaskControl | q.kCGEventFlagMaskAlternate):
                    return False
                chunk, units = "", 0
                chunks = []
                for character in text:
                    width = 2 if ord(character) > 0xFFFF else 1
                    if units + width > 20:
                        chunks.append((chunk, units))
                        chunk, units = "", 0
                    chunk += character
                    units += width
                if chunk:
                    chunks.append((chunk, units))
                for chunk, units in chunks:
                    for down in (True, False):
                        event = q.CGEventCreateKeyboardEvent(None, 0, down)
                        q.CGEventSetFlags(event, 0)
                        q.CGEventKeyboardSetUnicodeString(event, units, chunk)
                        q.CGEventPost(q.kCGSessionEventTap, event)
                return True
            except Exception:
                return False

    def start_shortcuts(self, callback: Shortcut) -> None:
        super().start_shortcuts(callback)
        with self._lock:
            if self._thread is not None or self._stopped.is_set():
                return
            self._thread = threading.Thread(target=self._event_loop, name="inklow-quartz", daemon=True)
            self._thread.start()

    def _event_loop(self) -> None:
        q, cf = self.q, self.cf

        def event_callback(proxy, event_type, event, refcon):
            if event_type == q.kCGEventTapDisabledByTimeout:
                self.set_shortcuts_enabled(False)
                self._set_status("macOS timed out the keyboard tap. Pause/resume global typing to re-enable it.")
                return event
            if event_type == q.kCGEventTapDisabledByUserInput:
                # CGEventTapEnable(False) also queues this notification. It may
                # arrive after a new suggestion enabled the tap; preserve the
                # current requested state instead of disabling that suggestion.
                with self._lock:
                    if self._tap is not None:
                        if self._enabled:
                            q.CGEventTapEnable(self._tap, True)
                return event
            if event_type != q.kCGEventKeyDown:
                return event
            flags = q.CGEventGetFlags(event)
            forbidden = q.kCGEventFlagMaskCommand | q.kCGEventFlagMaskControl | q.kCGEventFlagMaskAlternate | q.kCGEventFlagMaskSecondaryFn
            if flags & forbidden:
                return event
            code = q.CGEventGetIntegerValueField(event, q.kCGKeyboardEventKeycode)
            action = None
            if code == 48:
                action = "all" if flags & q.kCGEventFlagMaskShift else "word"
            elif code == 53 and not flags & q.kCGEventFlagMaskShift:
                action = "dismiss"
            return None if action and self._dispatch(action) else event

        self._event_callback = event_callback  # Retain the C callback bridge.
        try:
            tap = q.CGEventTapCreate(q.kCGSessionEventTap, q.kCGHeadInsertEventTap, q.kCGEventTapOptionDefault,
                                     1 << q.kCGEventKeyDown, event_callback, None)
            if tap is None:
                self._set_status(
                    "Keyboard capture denied. Enable Accessibility and Input Monitoring for Inklow "
                    "(or its terminal/Python host) in System Settings → Privacy & Security, then restart."
                )
                return
            source = cf.CFMachPortCreateRunLoopSource(None, tap, 0)
            runloop = cf.CFRunLoopGetCurrent()
            with self._lock:
                self._tap, self._runloop = tap, runloop
                q.CGEventTapEnable(tap, self._enabled and not self._stopped.is_set())
            cf.CFRunLoopAddSource(runloop, source, cf.kCFRunLoopCommonModes)
            self._refresh_status()
            if not self._stopped.is_set():
                cf.CFRunLoopRun()
            cf.CFRunLoopRemoveSource(runloop, source, cf.kCFRunLoopCommonModes)
            cf.CFMachPortInvalidate(tap)
        except Exception as exc:
            self._set_status(f"Quartz event tap unavailable ({type(exc).__name__}). Check Input Monitoring permissions and restart.")
        finally:
            with self._lock:
                self._tap, self._runloop = None, None

    def set_shortcuts_enabled(self, enabled: bool) -> None:
        with self._lock:
            previous = self._enabled
            super().set_shortcuts_enabled(enabled)
            if self._tap is not None and previous != self._enabled:
                self.q.CGEventTapEnable(self._tap, self._enabled)
                if self._enabled:
                    self._refresh_status()

    def stop(self) -> None:
        super().stop()
        with self._lock:
            runloop = self._runloop
        if runloop is not None:
            self.cf.CFRunLoopStop(runloop)
        # No join: the GUI may own a suggestion lock needed by an in-flight callback.
