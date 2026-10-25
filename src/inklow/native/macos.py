"""macOS AX text controls and a conditional Quartz keyboard event tap."""
from __future__ import annotations

import os
import math
import threading
import dataclasses

from ..core import InlineRender, Snapshot
from .base import BridgeBase, Shortcut, utf16_to_index, index_to_utf16
from ..shortcuts import mac_key_name


class MacBridge(BridgeBase):
    def __init__(self) -> None:
        super().__init__()
        import ApplicationServices as AX
        import Quartz
        import CoreFoundation as CF
        from AppKit import NSWorkspace, NSAccessibilityRoleDescription, NSEvent
        self.ax, self.q, self.cf = AX, Quartz, CF
        self._workspace = NSWorkspace.sharedWorkspace()
        self._event_type = NSEvent
        self._secure_description = NSAccessibilityRoleDescription("AXTextField", "AXSecureTextField")
        self._tap = None
        self._runloop = None
        self._thread: threading.Thread | None = None
        self._refresh_status()

    def _refresh_status(self) -> None:
        if not self.ax.AXIsProcessTrusted():
            self._set_status(
                "Accessibility permission required: System Settings → Privacy & Security → Accessibility. "
                "Enable BroTypist (or the terminal/Python host launching it), then restart."
            )
        elif self._tap is None:
            self._set_status(
                "Accessibility available. Global shortcuts require Input Monitoring permission in "
                "System Settings → Privacy & Security for BroTypist or its terminal/Python host. "
                "Inline completion requires a writable AXTextField/AXTextArea with a verified attributed font run and baseline."
            )
        else:
            self._set_status("macOS AX and Quartz ready. Inline requires verified host font, baseline and visible line-end geometry; unsupported controls stay silent. Secure and read-only controls are excluded.")

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

    def _foreground(self):
        app_info = self._workspace.frontmostApplication()
        if app_info is None:
            return None
        pid = int(app_info.processIdentifier())
        if pid == os.getpid():
            return None
        executable_url = app_info.executableURL()
        executable = str(executable_url.path()) if executable_url else ""
        app_id = str(app_info.bundleIdentifier() or executable)
        domain = ""
        # AXDocument is native window metadata, not an editable address-bar
        # value, AppleScript, page DOM, or a title guessed to be a URL.
        browser = self._is_browser(app_id, executable)
        if browser and self.ax.AXIsProcessTrusted():
            app = self.ax.AXUIElementCreateApplication(pid)
            window = self._attribute(app, "AXFocusedWindow")
            document = self._attribute(window, "AXDocument") if window is not None else None
            if document is not None and hasattr(document, "absoluteString"):
                document = str(document.absoluteString())
            domain = self._domain(document)
        return pid, executable, {"app_id": app_id, "name": str(app_info.localizedName() or app_id),
                                 "domain": domain, "browser": browser, "domain_observable": bool(domain)}

    def foreground(self) -> dict:
        with self._lock:
            try:
                result = self._foreground()
                return result[2] if result else super().foreground()
            except Exception:
                return super().foreground()

    def _prepare_accessibility(self, app, pid, app_id):
        # Electron documents AXManualAccessibility for third-party assistive
        # clients. Chromium browsers advertise AXEnhancedUserInterface instead.
        # Only touch an advertised, writable per-application flag; never TCC.
        target = (pid, app_id)
        if getattr(self, "_accessibility_target", None) == target:
            return
        self._accessibility_target = target
        manual = self._attribute(app, "AXManualAccessibility")
        if manual is not None:
            attribute, enabled = "AXManualAccessibility", manual
        elif app_id.casefold() in {
            "com.google.chrome", "com.google.chrome.canary", "org.chromium.chromium",
            "com.microsoft.edgemac", "com.brave.browser",
            "com.operasoftware.opera", "com.vivaldi.vivaldi",
        }:
            attribute = "AXEnhancedUserInterface"
            enabled = self._attribute(app, attribute)
        else:
            return
        if enabled is not None and not enabled and self._settable(app, attribute):
            self.ax.AXUIElementSetAttributeValue(app, attribute, True)

    def _focused(self):
        # Resolve the foreground process through NSWorkspace. System-wide AX
        # AXFocusedApplication can return kAXErrorCannotComplete even while the
        # foreground app's own AXFocusedUIElement is accessible.
        result = self._foreground()
        if result is None:
            return None
        pid, executable, metadata = result
        app_id = metadata["app_id"]
        if pid == os.getpid() or not app_id or not self._allowed(app_id, metadata["domain"], executable):
            return None
        app = self.ax.AXUIElementCreateApplication(pid)
        self._prepare_accessibility(app, pid, app_id)
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
        focused = self._attribute(field, "AXFocused")
        # AXFocusedUIElement on the foreground application's AX root is the
        # authoritative focus source. Some providers omit the redundant flag;
        # an explicit false still rejects the control.
        if (enabled is not None and not enabled) or (focused is not None and not focused):
            return None
        if not (self._settable(field, "AXSelectedText") or self._settable(field, "AXValue")):
            return None
        return field, pid, app_id, metadata["domain"]

    def _capture(self):
        if self._stopped.is_set() or not self.ax.AXIsProcessTrusted():
            return None
        focused = self._focused()
        if focused is None:
            return None
        field, pid, app_id, domain = focused
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
        if self._focused() != focused:
            return None
        text = self._attribute(field, "AXValue")
        if not isinstance(text, str) or len(text) > 1_000_000:
            return None
        caret = utf16_to_index(text, location)
        end = utf16_to_index(text, location + length)
        rect = self._caret_bounds(field, text, caret, location)
        field_rect = self._field_bounds(field)
        render = self._inline_metrics(field, text, caret, location, rect, field_rect) if length == 0 else None
        window = self._attribute(field, "AXWindow")
        window_id = f"{pid}:{hash(window)}" if window is not None else ""
        window_revision = repr((window_id, field_rect))
        multiline = self._attribute(field, "AXRole") == "AXTextArea"
        # Re-resolve foreground policy, security, writability and field identity
        # after asynchronous AX calls. Never return a snapshot of stale focus.
        if self._focused() != focused:
            return None
        # CFEqual/hash on AXUIElement retains identity across fresh AX references.
        field_id = f"{pid}:{hash(field)}"
        return Snapshot(app_id, field_id, text, caret, end - caret, rect,
                        domain=domain, multiline=multiline,
                        field_rect=field_rect, inline_render=render,
                        window_id=window_id, window_revision=window_revision), field

    def _field_bounds(self, field):
        position = self._attribute(field, "AXPosition")
        size = self._attribute(field, "AXSize")
        if position is None or size is None:
            return None
        valid_position, position = self.ax.AXValueGetValue(position, self.ax.kAXValueCGPointType, None)
        valid_size, size = self.ax.AXValueGetValue(size, self.ax.kAXValueCGSizeType, None)
        if not valid_position or not valid_size:
            return None
        bounds = (float(position[0]), float(position[1]), float(size[0]), float(size[1]))
        if not all(math.isfinite(value) for value in bounds) or bounds[2] <= 0 or bounds[3] <= 0:
            return None
        # AX screen coordinates already use the top-left origin, like caret
        # ranges; do not apply AppKit's bottom-left screen conversion here.
        return bounds

    def _range_bounds(self, field, location, length):
        value = self.ax.AXValueCreate(self.ax.kAXValueCFRangeType, (location, length))
        error, bounds = self.ax.AXUIElementCopyParameterizedAttributeValue(field, "AXBoundsForRange", value, None)
        if error or bounds is None:
            return None
        valid, bounds = self.ax.AXValueGetValue(bounds, self.ax.kAXValueCGRectType, None)
        if not valid or bounds[1][1] <= 0:
            return None
        bounds = (float(bounds[0][0]), float(bounds[0][1]), float(bounds[1][0]), float(bounds[1][1]))
        if not all(math.isfinite(value) for value in bounds) or bounds[2] < 0:
            return None
        return bounds

    def _caret_bounds(self, field, text, caret, location):
        bounds = self._range_bounds(field, location, 0)
        if bounds is None:
            return None
        # NSTextView on macOS 27 reports a zero-length range one line above
        # its actual insertion point. Keep that range's exact x, but use an
        # adjacent real glyph's observed row when the metrics agree.
        if caret < len(text) and text[caret] not in "\r\n":
            glyph_length = 2 if ord(text[caret]) > 0xFFFF else 1
            glyph = self._range_bounds(field, location, glyph_length)
        elif caret and text[caret - 1] not in "\r\n":
            glyph_length = 2 if ord(text[caret - 1]) > 0xFFFF else 1
            glyph = self._range_bounds(field, location - glyph_length, glyph_length)
        else:
            return None  # No reliable glyph row on an empty line.
        if (glyph is not None and abs(glyph[3] - bounds[3]) <= 1
                and 0 <= glyph[1] - bounds[1] <= bounds[3] + 1
                and bounds[0] <= glyph[0] + glyph[2] + 1):
            bounds = (bounds[0], glyph[1], bounds[2], bounds[3])
        return bounds[0], bounds[1], max(1.0, bounds[2]), bounds[3]

    def _inline_metrics(self, field, text, caret, location, rect, field_rect):
        """Use the attributed host run and native layout baseline, never caret size."""
        if (not rect or not field_rect or not caret or text[caret - 1] in "\r\n"
                or (text[caret:] and not text[caret:].startswith(("\r", "\n")))):
            return None
        import unicodedata
        if any(unicodedata.bidirectional(ch) in {"R", "AL", "AN", "RLE", "RLO", "RLI"}
               for ch in text[text.rfind("\n", 0, caret) + 1:caret]):
            return None
        try:
            from AppKit import NSFont, NSLayoutManager
            visible = self._attribute(field, "AXVisibleCharacterRange")
            if visible is not None:
                valid, span = self.ax.AXValueGetValue(visible, self.ax.kAXValueCFRangeType, None)
                if not valid or not int(span[0]) < location <= int(span[0]) + int(span[1]):
                    return None
            clip = field_rect
            parent = self._attribute(field, "AXParent")
            for _ in range(16):
                if parent is None:
                    break
                bounds = self._field_bounds(parent)
                if bounds:
                    left, top = max(clip[0], bounds[0]), max(clip[1], bounds[1])
                    right = min(clip[0] + clip[2], bounds[0] + bounds[2])
                    bottom = min(clip[1] + clip[3], bounds[1] + bounds[3])
                    if right <= left or bottom <= top:
                        return None
                    clip = (left, top, right - left, bottom - top)
                parent = self._attribute(parent, "AXParent")
            glyph_length = 2 if ord(text[caret - 1]) > 0xFFFF else 1
            value = self.ax.AXValueCreate(self.ax.kAXValueCFRangeType, (location - glyph_length, glyph_length))
            error, attributed = self.ax.AXUIElementCopyParameterizedAttributeValue(
                field, "AXAttributedStringForRange", value, None)
            if error or attributed is None or not attributed.length():
                return None
            attributes, _ = attributed.attributesAtIndex_effectiveRange_(0, None)
            descriptor = attributes.get("AXFont")
            if not descriptor:
                return None
            name, size = str(descriptor.get("AXFontName") or ""), float(descriptor.get("AXFontSize") or 0)
            font = NSFont.fontWithName_size_(name, size)
            if font is None or str(font.fontName()) != name:
                return None
            # Superscripts, baseline shifts and host IME runs are not plain text.
            if (any(attributes.get(key) for key in ("AXSuperscript", "NSBaselineOffset", "AXMarkedText"))
                    or any(key in attributes for key in ("NSMarkedClauseSegment",
                                                         "NSMarkedClauseSegmentAttributeName"))):
                return None
            glyph = self._range_bounds(field, location - glyph_length, glyph_length)
            layout = getattr(self, "_layout_manager", None)
            if layout is None:
                layout = self._layout_manager = NSLayoutManager.alloc().init()
            line_height = float(layout.defaultLineHeightForFont_(font))
            baseline_offset = float(layout.defaultBaselineOffsetForFont_(font))
            if (not glyph or abs(glyph[3] - line_height) > 0.75
                    or abs(glyph[1] - rect[1]) > 0.75
                    or abs(glyph[0] + glyph[2] - rect[0]) > 1.5
                    or abs(rect[3] - line_height) > 0.75):
                return None
            # NSLayoutManager reports the baseline's distance from the line top.
            baseline = glyph[1] + baseline_offset
            render = InlineRender(name, size, baseline, clip,
                                  float(font.ascender()), abs(float(font.descender())),
                                  source="macos-ax-attributed-run")
            return render if render.valid(rect) else None
        except Exception:
            return None

    def context_matches(self, snapshot):
        try:
            focused = self._focused()
            if focused is None:
                return False
            field, pid, app_id, domain = focused
            window = self._attribute(field, "AXWindow")
            return (app_id == snapshot.app_id and domain == snapshot.domain
                    and f"{pid}:{hash(field)}" == snapshot.field_id
                    and window is not None and f"{pid}:{hash(window)}" == snapshot.window_id
                    and not self._attribute(field, "AXHidden")
                    and self._field_bounds(field) is not None)
        except Exception:
            return False

    def focused_context(self, snapshot, *, authorized, max_chars=2400):
        result = {"text": "", "provenance": "accessibility",
                  "window_id": snapshot.window_id, "revision": snapshot.window_revision,
                  "status": "unavailable"}
        if not authorized():
            return {**result, "status": "blocked"}
        try:
            focused = self._focused()
            if focused is None:
                return result
            field, pid, app_id, domain = focused
            window = self._attribute(field, "AXWindow")
            if (window is None or f"{pid}:{hash(window)}" != snapshot.window_id
                    or f"{pid}:{hash(field)}" != snapshot.field_id or app_id != snapshot.app_id):
                return result
            # Browser chrome can nest the document beyond the traversal budget.
            # Prefer the focused field's nearest document over unrelated toolbar UI.
            content_root = window
            parent = self._attribute(field, "AXParent")
            for _ in range(16):
                if parent is None or hash(parent) == hash(window):
                    break
                if self._attribute(parent, "AXRole") == "AXWebArea":
                    content_root = parent
                    break
                parent = self._attribute(parent, "AXParent")
            import time
            deadline = time.monotonic() + 0.15
            stack, chunks, remaining, visited = [(content_root, 0)], [], min(12000, max(0, max_chars)), set()
            for _ in range(128):
                if not stack or remaining <= 0 or time.monotonic() >= deadline:
                    break
                element, depth = stack.pop()
                if not authorized() or self._focused() != focused:
                    return {**result, "status": "blocked"}
                identity = hash(element)
                if identity in visited:
                    continue
                visited.add(identity)
                role = self._attribute(element, "AXRole")
                if (self._attribute(element, "AXHidden") or self._attribute(element, "AXProtectedContent")
                        or self._attribute(element, "AXSubrole") == "AXSecureTextField"
                        or self._attribute(element, "AXRoleDescription") == self._secure_description):
                    continue
                bounds = self._field_bounds(element)
                window_bounds = self._field_bounds(window)
                if bounds and window_bounds:
                    x, y, w, h = bounds
                    wx, wy, ww, wh = window_bounds
                    if x + w <= wx or y + h <= wy or x >= wx + ww or y >= wy + wh:
                        continue
                if role == "AXStaticText" and bounds:
                    if not authorized():
                        return {**result, "status": "blocked"}
                    value = self._attribute(element, "AXValue")
                    if isinstance(value, str) and value.strip() and value != snapshot.text:
                        value = value[:remaining]
                        chunks.append(value)
                        remaining -= len(value) + 1
                # Do not traverse other editable documents or secure subtrees.
                if depth < 8 and role not in ("AXTextField", "AXTextArea"):
                    children = self._attribute(element, "AXChildren") or []
                    stack.extend((child, depth + 1) for child in list(children)[:64])
            if not authorized() or self._focused() != focused:
                return {**result, "status": "blocked"}
            text = "\n".join(chunks)[:max_chars]
            return {**result, "text": text, "status": "ready" if text else "empty"}
        except Exception:
            return {**result, "status": "error"}

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

    def replace_range(self, snapshot: Snapshot, start: int, length: int, text: str) -> bool:
        if not self._valid_replacement(snapshot, start, length, text):
            return False
        with self._lock:
            try:
                current = self._capture()
                if current is None or current[0].key != snapshot.key:
                    return False
                field = current[1]
                if not self._settable(field, "AXSelectedText"):
                    self._set_status("This control does not expose safe AXSelectedText replacement. Ordinary completion insertion may still work.")
                    return False
                if (start, length) != (snapshot.caret, snapshot.selection_length):
                    if not self._settable(field, "AXSelectedTextRange"):
                        self._set_status("This control does not expose a writable AXSelectedTextRange; emoji suffix replacement is unavailable.")
                        return False
                    location = index_to_utf16(snapshot.text, start)
                    units = index_to_utf16(snapshot.text, start + length) - location
                    native_range = self.ax.AXValueCreate(self.ax.kAXValueCFRangeType, (location, units))
                    if self.ax.AXUIElementSetAttributeValue(field, "AXSelectedTextRange", native_range):
                        return False
                    selected = self._capture()
                    if selected is None:
                        return False
                    expected = dataclasses.replace(snapshot, caret=start, selection_length=length)
                    if selected[0].key != expected.key:
                        return False
                return self.ax.AXUIElementSetAttributeValue(field, "AXSelectedText", text) == 0
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
        import objc
        q, cf = self.q, self.cf

        def dispatch_event(proxy, event_type, event, refcon):
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
            if q.CGEventGetIntegerValueField(event, q.kCGEventSourceUnixProcessID) == os.getpid():
                return event
            self._note_input()
            code = q.CGEventGetIntegerValueField(event, q.kCGKeyboardEventKeycode)
            key = mac_key_name(code)
            if flags & q.kCGEventFlagMaskSecondaryFn and not (len(key) > 1 and key.startswith("F")):
                return event
            if len(key) == 1 and key.isalpha():
                native_event = self._event_type.eventWithCGEvent_(event)
                character = str(native_event.charactersIgnoringModifiers() or "")
                if len(character) == 1 and character.isascii() and character.isalpha():
                    key = character.upper()
            modifiers = tuple(name for name, mask in (
                ("Ctrl", q.kCGEventFlagMaskControl), ("Alt", q.kCGEventFlagMaskAlternate),
                ("Shift", q.kCGEventFlagMaskShift), ("Super", q.kCGEventFlagMaskCommand))
                if flags & mask)
            action = self._key_action(key, modifiers, platform="macos")
            if action == "bypass" and self._dispatch(action):
                q.CGEventSetFlags(event, flags & ~q.kCGEventFlagMaskAlternate)
                return event
            return None if action and action != "bypass" and self._dispatch(action) else event

        def event_callback(proxy, event_type, event, refcon):
            # Quartz runs outside AppKit's main-loop autorelease pools.
            with objc.autorelease_pool():
                return dispatch_event(proxy, event_type, event, refcon)

        self._event_callback = event_callback  # Retain the C callback bridge.
        try:
            tap = q.CGEventTapCreate(q.kCGSessionEventTap, q.kCGHeadInsertEventTap, q.kCGEventTapOptionDefault,
                                     1 << q.kCGEventKeyDown, event_callback, None)
            if tap is None:
                self._set_status(
                    "Keyboard capture denied. Enable Accessibility and Input Monitoring for BroTypist "
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
