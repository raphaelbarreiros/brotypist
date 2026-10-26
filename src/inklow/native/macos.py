"""macOS AX text controls, an ordered Quartz input tap and verified inline metrics.

Threads: AX capture runs on the runtime thread under ``_lock``; the Quartz
taps run on ``inklow-quartz`` and never touch AX or ``_lock``; AX observer
notifications run on ``inklow-ax-observer``; workspace/input-source
notifications arrive on the main queue. Cross-thread state is published by
single reference assignments.
"""
from __future__ import annotations

import ctypes
import dataclasses
import hashlib
import math
import os
import threading
import time
import unicodedata
from collections import deque

from ..core import InlineRender, Snapshot
from .base import CHROMIUM_BROWSER_BUNDLES, BridgeBase, Shortcut, index_to_utf16, utf16_to_index
from .. import shortcuts

# kCGEventSourceUserData carried by every synthetic key event BroTypist posts,
# so its own taps pass them through without dispatching or invalidating.
SYNTHETIC_TAG = 0x42524F54
_TAB_KEYCODE = 48
_AX_MUTATION_TIMEOUT = 0.25
_REPLAY_SECONDS = 2.0
_VISION_SOURCES = frozenset({"macos-ax-geometry+vision", "macos-ax-attributed-run+vision"})
_RTL = frozenset({"R", "AL", "AN", "RLE", "RLO", "RLI"})
# AppKit NSFontWeight values for private system-font name suffixes. Longer
# words precede their substrings ("semibold" before "bold").
_SYSTEM_WEIGHTS = (("ultralight", -0.8), ("thin", -0.6), ("semibold", 0.3), ("light", -0.4),
                   ("medium", 0.23), ("heavy", 0.56), ("black", 0.62), ("emphasized", 0.4),
                   ("bold", 0.4), ("regular", 0.0))
# Page zoom factors offered by Chromium and Safari; calibration snaps only to these.
_ZOOM_LEVELS = (0.25, 1 / 3, 0.5, 2 / 3, 0.75, 0.8, 0.85, 0.9, 1.1, 1.15, 1.25,
                1.5, 1.75, 2.0, 2.5, 3.0, 4.0, 5.0)
_AX_NOTIFICATIONS = ("AXFocusedUIElementChanged", "AXFocusedWindowChanged", "AXMainWindowChanged",
                     "AXValueChanged", "AXSelectedTextChanged", "AXWindowMoved", "AXWindowResized")
_USER_NOTIFICATIONS = frozenset({"AXFocusedUIElementChanged", "AXFocusedWindowChanged", "AXMainWindowChanged"})
# Pinned upstream accessible names, not generic "chat"/"prompt" substring matches:
# microsoft/vscode 4d57ed62, chatInputPart.ts _getAriaLabel(). Localized names
# and custom fork composers remain unverified and require explicit editor opt-in.
_STOCK_CHAT_EDITORS = frozenset({"com.microsoft.vscode", "com.microsoft.vscodeinsiders"})
_STOCK_CHAT_NAMES = (
    "Chat Input (Agent), edit files in your workspace.",
    "Chat Input (Edit), edit files in your workspace.",
    "Chat Input (Ask), ask questions or type / for topics.",
)
SECURE_INPUT_STATUS = (
    "Secure Input is on: an app (often a password field or a terminal) is protecting keyboard input. "
    "BroTypist reads no text and captures no shortcuts until that app turns it off."
)

_carbon = None
_core_foundation = None


def _carbon_library():
    """HIToolbox via Carbon, or None off macOS. Loaded once."""
    global _carbon
    if _carbon is None:
        try:
            library = ctypes.cdll.LoadLibrary("/System/Library/Frameworks/Carbon.framework/Carbon")
            library.IsSecureEventInputEnabled.restype = ctypes.c_bool
            library.IsSecureEventInputEnabled.argtypes = []
            library.TISCopyCurrentKeyboardInputSource.restype = ctypes.c_void_p
            library.TISCopyCurrentKeyboardInputSource.argtypes = []
            library.TISGetInputSourceProperty.restype = ctypes.c_void_p
            library.TISGetInputSourceProperty.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
            _carbon = library
        except (OSError, AttributeError):
            _carbon = False
    return _carbon or None


def secure_event_input_enabled() -> bool:
    """Process-independent Secure Event Input state; it never names the owner."""
    library = _carbon_library()
    try:
        return bool(library is not None and library.IsSecureEventInputEnabled())
    except Exception:
        return False


def keyboard_input_source_kind() -> str:
    """'layout', 'ime' or 'unknown'. Text Input Sources require the main thread."""
    global _core_foundation
    library = _carbon_library()
    if library is None:
        return "unknown"
    try:
        import objc
        if _core_foundation is None:
            _core_foundation = ctypes.cdll.LoadLibrary(
                "/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation")
            _core_foundation.CFRelease.argtypes = [ctypes.c_void_p]

        def constant(name):
            pointer = ctypes.c_void_p.in_dll(library, name).value
            return str(objc.objc_object(c_void_p=pointer)) if pointer else None

        source = library.TISCopyCurrentKeyboardInputSource()
        if not source:
            return "unknown"
        try:
            key = ctypes.c_void_p.in_dll(library, "kTISPropertyInputSourceType").value
            value = library.TISGetInputSourceProperty(source, key) if key else None
            kind = str(objc.objc_object(c_void_p=value)) if value else ""
        finally:
            _core_foundation.CFRelease(source)
        if kind and kind == constant("kTISTypeKeyboardLayout"):
            return "layout"
        if kind and kind in {constant("kTISTypeKeyboardInputMode"),
                             constant("kTISTypeKeyboardInputMethodWithoutModes"),
                             constant("kTISTypeKeyboardInputMethodModeEnabled")}:
            return "ime"
    except Exception:
        pass
    return "unknown"


def _system_weight(name: str) -> float | None:
    """NSFontWeight of a private system-font alias, or None if not reproducible."""
    lowered = name.casefold()
    if any(word in lowered for word in ("italic", "oblique", "condensed", "compressed",
                                        "expanded", "mono", "rounded", "serif")):
        return None
    for word, weight in _SYSTEM_WEIGHTS:
        if word in lowered:
            return weight
    return 0.0


def resolve_font(render):
    """The exact NSFont an InlineRender (or its ``asdict``) describes, or None."""
    from AppKit import NSFont, NSFontFixedAdvanceAttribute
    value = render.get if isinstance(render, dict) else (lambda key, default=None: getattr(render, key, default))
    name = str(value("font_name") or "")
    try:
        size = float(value("font_size") or 0)
        weight = float(value("system_weight", 0.0) or 0.0)
        advance = float(value("fixed_advance", 0.0) or 0.0)
    except (TypeError, ValueError):
        return None
    if (not name or not math.isfinite(size) or not 1 <= size <= 300 or not math.isfinite(weight)
            or not math.isfinite(advance) or not 0 <= advance <= 300):
        return None
    if value("system_font", False):
        font = NSFont.systemFontOfSize_weight_(size, weight)
    elif name.startswith("."):
        return None  # Private names resolve to a substitute font through fontWithName.
    else:
        font = NSFont.fontWithName_size_(name, size)
    if font is not None and advance:
        descriptor = font.fontDescriptor().fontDescriptorByAddingAttributes_(
            {NSFontFixedAdvanceAttribute: advance})
        font = NSFont.fontWithDescriptor_size_(descriptor, size)
    return font if font is not None and str(font.fontName()) == name else None


def text_width(font, text: str) -> float:
    """CoreText typographic advance of one line in ``font`` (default kerning)."""
    import CoreText
    from Foundation import NSAttributedString
    attributed = NSAttributedString.alloc().initWithString_attributes_(text, {"NSFont": font})
    line = CoreText.CTLineCreateWithAttributedString(attributed)
    bounds = CoreText.CTLineGetTypographicBounds(line, None, None, None)
    return float(bounds[0] if isinstance(bounds, tuple) else bounds)


def _tolerance(width: float) -> float:
    return max(0.75, abs(width) * 0.012)


class MacBridge(BridgeBase):
    # Class defaults keep partially constructed test doubles coherent.
    _tap = None
    _pointer_tap = None
    _runloop = None
    _thread: threading.Thread | None = None
    _observer_thread: threading.Thread | None = None
    _observer_runloop = None
    _front = {"app_id": "", "domain": ""}
    _observe_pid = 0
    _dead_key = False
    _ime_kind = "unknown"
    _key_names: dict[int, str] = {}
    _screenshot_appearance = False
    _suppressed_keyups = frozenset()
    _tap_timeouts: tuple[float, ...] = ()
    _notification_tokens: tuple = ()
    _accessibility_target = None
    _layout_manager = None
    _input_gate = None
    _insertion_gate = None
    _event_dispatch = None
    _focused_field_hash = None
    _observer_value_at = 0.0
    _terminal_adapter = None
    _terminal_completion = False
    _docs_helper = None
    _document_salt = None
    _input_mutex = threading.RLock()  # Only queue/state/posting; never AX.

    def __init__(self) -> None:
        super().__init__()
        import ApplicationServices as AX
        import Quartz
        import CoreFoundation as CF
        from AppKit import NSWorkspace, NSAccessibilityRoleDescription, NSEvent
        self.ax, self.q, self.cf = AX, Quartz, CF
        self._workspace = NSWorkspace.sharedWorkspace()
        self._document_salt = os.urandom(32)
        self._event_type = NSEvent
        self._secure_description = NSAccessibilityRoleDescription("AXTextField", "AXSecureTextField")
        self._observe_notifications()
        self._refresh_status()


    def _terminal(self):
        if self._terminal_adapter is None:
            from .macos_terminal import TerminalAdapter
            self._terminal_adapter = TerminalAdapter()
        return self._terminal_adapter

    @staticmethod
    def _native_window_number(pid, bounds):
        from .macos_vision import _window_number
        return _window_number(pid, bounds)

    def terminal_permissions(self):
        from .macos_terminal import TERMINAL, ITERM
        adapter = self._terminal()
        return {app_id: {"name": name, "status": adapter.permission_state(app_id)}
                for app_id, name in ((TERMINAL, "Terminal"), (ITERM, "iTerm"))}

    def request_terminal_access(self, app_id):
        from .macos_terminal import SUPPORTED_APPS
        return self._terminal().request_access(app_id) if app_id in SUPPORTED_APPS else "unavailable"

    def docs_hint(self):
        if self._docs_helper is None:
            from .macos_docs import DocsHelper
            self._docs_helper = DocsHelper(self)
        return self._docs_helper.hint()

    def enable_docs_accessibility(self, token):
        if self._docs_helper is None:
            from .macos_docs import HELP_URL
            return {"status": "unknown", "diagnostic": "docs_token_invalid", "help_url": HELP_URL}
        return self._docs_helper.enable(token)

    def _observe_notifications(self) -> None:
        """App activation and input-source changes, delivered on the main queue."""
        tokens = []
        try:
            from AppKit import NSWorkspaceDidActivateApplicationNotification
            from Foundation import NSDistributedNotificationCenter, NSOperationQueue, NSThread
            main = NSOperationQueue.mainQueue()
            center = self._workspace.notificationCenter()
            tokens.append((center, center.addObserverForName_object_queue_usingBlock_(
                NSWorkspaceDidActivateApplicationNotification, None, main, self._application_activated)))
            distributed = NSDistributedNotificationCenter.defaultCenter()
            tokens.append((distributed, distributed.addObserverForName_object_queue_usingBlock_(
                "com.apple.Carbon.TISNotifySelectedKeyboardInputSourceChanged", None, main,
                self._input_source_changed)))
            if NSThread.isMainThread():
                self._input_source_changed(None)
            else:
                main.addOperationWithBlock_(lambda: self._input_source_changed(None))
        except Exception:
            pass
        self._notification_tokens = tuple(tokens)

    def _application_activated(self, notification) -> None:
        app_id = ""
        try:
            app = notification.userInfo().objectForKey_("NSWorkspaceApplicationKey")
            if app is not None:
                executable_url = app.executableURL()
                app_id = str(app.bundleIdentifier() or (executable_url.path() if executable_url else ""))
        except Exception:
            pass
        # The domain is unknown until the next capture reads AXDocument.
        self._front = {"app_id": app_id, "domain": ""}
        self._dead_key = False
        self._note_input()

    def _input_source_changed(self, notification) -> None:
        # This notification runs on the main queue. AppKit/HIToolbox keyboard
        # translation traps when called from the background Quartz event tap.
        keys = dict(shortcuts.MAC_KEYS)
        above_tab = shortcuts.mac_above_tab_keycode()
        if above_tab is not None:
            keys[above_tab] = "AboveTab"
        for code, name in shortcuts.MAC_KEYS.items():
            if len(name) != 1 or not name.isalpha():
                continue
            try:
                event = self.q.CGEventCreateKeyboardEvent(None, code, True)
                self.q.CGEventSetFlags(event, 0)
                native_event = self._event_type.eventWithCGEvent_(event)
                character = str(native_event.charactersIgnoringModifiers() or "")
            except Exception:
                continue
            if len(character) == 1 and character.isascii() and character.isalpha():
                keys[code] = character.upper()
        # Publish a replacement; the tap never mutates or translates this map.
        self._key_names = keys
        self._ime_kind = keyboard_input_source_kind()
        self._dead_key = False
        self._note_input()

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
                "Inline completion requires a writable text control with observable caret-row geometry."
            )
        else:
            self._set_status("macOS AX and Quartz ready. Inline uses visible caret-row geometry and "
                             "host typography when exposed, or a fitted system font at a safe line end. "
                             "Secure, read-only and unobservable controls are excluded.")

    def status(self) -> str:
        if not self.ax.AXIsProcessTrusted():
            self._refresh_status()
        if self.secure_input_active():
            return SECURE_INPUT_STATUS
        if self._ime_kind == "unknown":
            return super().status() + " Keyboard input source is not yet observable; composition safety cannot be verified."
        if getattr(self, "_capture_error", ""):
            return self._capture_error
        return super().status()

    def secure_input_active(self) -> bool:
        return secure_event_input_enabled()

    def configure(self, config) -> None:
        from .macos_terminal import terminal_policy_exclusions
        effective = dataclasses.replace(config, excluded_apps=terminal_policy_exclusions(config))
        with self._lock:
            super().configure(effective)
            self._terminal_completion = config.terminal_completion
        enabled = bool(getattr(config, "screenshot_appearance", False))
        previous, self._screenshot_appearance = self._screenshot_appearance, enabled
        # Compatibility rules may have changed; re-evaluate on the next capture.
        self._accessibility_target = None
        if previous and not enabled:
            try:
                from . import macos_vision
                reset = getattr(macos_vision, "reset", None)
                if reset is not None:
                    reset()
            except Exception:
                pass

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
        window_id = document_id = ""
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
            if window is not None:
                window_id = f"{pid}:{hash(window)}"
            if isinstance(document, str) and self._document_salt:
                document_id = hashlib.blake2s(document.encode("utf-8"),
                                              key=self._document_salt, digest_size=16).hexdigest()
        # Published for the key tap's Tab rule and the AX observer thread.
        self._front = {"app_id": app_id, "domain": domain}
        self._observe_pid = pid
        return pid, executable, {"app_id": app_id, "name": str(app_info.localizedName() or app_id),
                                 "domain": domain, "browser": browser, "domain_observable": bool(domain),
                                 "pid": pid, "window_id": window_id, "document_id": document_id}

    def foreground(self) -> dict:
        with self._lock:
            try:
                result = self._foreground()
                return result[2] if result else super().foreground()
            except Exception:
                return super().foreground()

    def _tab_metadata(self) -> dict:
        # Never AX from the key tap: the last observed foreground identity.
        return self._front

    def _prepare_accessibility(self, app, pid, app_id):
        # Electron documents AXManualAccessibility for third-party assistive
        # clients. Chromium browsers advertise AXEnhancedUserInterface instead.
        # The app rule "improve_compatibility" forces (True) or forbids (False)
        # these per-application flags; absent keeps the advertised defaults.
        # Never TCC, never another process's preferences.
        rule = self._app_rules.get(f"app:{app_id}", {}).get("improve_compatibility")
        target = (pid, app_id, rule)
        if self._accessibility_target == target:
            return
        self._accessibility_target = target
        if rule is False:
            return
        manual = self._attribute(app, "AXManualAccessibility")
        if manual is not None:
            attribute, enabled = "AXManualAccessibility", manual
        elif app_id.casefold() in CHROMIUM_BROWSER_BUNDLES:
            attribute = "AXEnhancedUserInterface"
            enabled = self._attribute(app, attribute)
        elif rule is True:
            attribute = next((name for name in ("AXManualAccessibility", "AXEnhancedUserInterface")
                              if self._settable(app, name)), "")
            if not attribute:
                return
            enabled = self._attribute(app, attribute) or False
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
        # Terminal AXValue is scrollback, not an editable document. Dedicated
        # session/input-range validation must precede any terminal text access.
        if app_id in ("com.apple.Terminal", "com.googlecode.iterm2"):
            return None
        probe_editor = app_id.casefold() in _STOCK_CHAT_EDITORS
        if (pid == os.getpid() or not app_id
                or not self._allowed(app_id, metadata["domain"], executable, editor_probe=probe_editor)):
            return None
        # Secure Event Input means some control is protecting keystrokes;
        # no element or value is read while it is on.
        if self.secure_input_active():
            return None
        app = self.ax.AXUIElementCreateApplication(pid)
        self._prepare_accessibility(app, pid, app_id)
        field = self._attribute(app, "AXFocusedUIElement")
        if field is None:
            return None
        if self._is_browser(app_id):
            window = self._attribute(field, "AXWindow")
            if window is None or window != self._attribute(app, "AXFocusedWindow"):
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
        if ((enabled is not None and not enabled) or (focused is not None and not focused)
                or self._attribute(field, "AXHidden")):
            return None
        if self._field_ancestry(field) is None:
            return None
        if not (self._settable(field, "AXSelectedText") or self._settable(field, "AXValue")):
            return None
        if probe_editor and self._app_rules.get(f"app:{app_id}", {}).get("complete_in_code_editors") is not True:
            description = self._attribute(field, "AXDescription")
            if not (isinstance(description, str) and any(
                    description == name or (description.startswith(name)
                                            and description[len(name):].startswith((".", ", ", " ")))
                    for name in _STOCK_CHAT_NAMES)):
                self._set_status("Code editing stays off; this field is not a recognized stock VS Code chat input.")
                return None
        return field, pid, app_id, metadata["domain"]

    def _field_identity(self, field, pid, app_id):
        identity = f"{pid}:{hash(field)}"
        if not self._is_browser(app_id):
            return identity
        # A browser can reuse its focused AX object across same-origin
        # navigations. Bind capture/acceptance to native document metadata, not
        # just the field hash, domain, page title or editable address bar.
        window = self._attribute(field, "AXWindow")
        document = self._attribute(window, "AXDocument") if window is not None else None
        if document is not None and hasattr(document, "absoluteString"):
            document = str(document.absoluteString())
        if not isinstance(document, str) or not self._domain(document) or not self._document_salt:
            return None
        digest = hashlib.blake2s(document.encode("utf-8"),
                                 key=self._document_salt, digest_size=16).hexdigest()
        return f"{identity}:{hash(window)}:{digest}"

    def _capture(self):
        if self._stopped.is_set() or not self.ax.AXIsProcessTrusted():
            return None
        if self._dead_key:
            return None  # A dead key is composing; the host shows provisional text.
        if getattr(self, "_terminal_completion", False):
            foreground = self._foreground()
            if foreground is not None and foreground[2]["app_id"] in ("com.apple.Terminal", "com.googlecode.iterm2"):
                return self._terminal().capture(self, foreground)
        focused = self._focused()
        if focused is None:
            return None
        field, pid, app_id, domain = focused
        field_id = self._field_identity(field, pid, app_id)
        if field_id is None:
            return None
        marked_range = self._attribute(field, "AXMarkedTextRange")
        if marked_range is None:
            # Without the attribute, composition is unobservable: only a plain
            # keyboard layout (no input method) can be trusted to have none.
            if self._ime_kind != "layout":
                return None
        else:
            valid, marked = self.ax.AXValueGetValue(marked_range, self.ax.kAXValueCFRangeType, None)
            if not valid or marked[1] > 0:
                return None
        native_range = self._attribute(field, "AXSelectedTextRange")
        if native_range is None:
            # Marker offsets are not interchangeable with this field's CFRange:
            # Chromium AXTextMarkerForIndex creates a root-document position.
            # Never reinterpret marker-only Docs metadata as a writable draft.
            return None
        valid, native_range = self.ax.AXValueGetValue(native_range, self.ax.kAXValueCFRangeType, None)
        if not valid:
            return None
        location, length = int(native_range[0]), int(native_range[1])
        if (location < 0 or length < 0 or self._focused() != focused
                or self._field_identity(field, pid, app_id) != field_id):
            return None
        text = self._attribute(field, "AXValue")
        if not isinstance(text, str) or len(text) > 1_000_000:
            return None
        caret = utf16_to_index(text, location)
        end = utf16_to_index(text, location + length)
        rect = self._caret_bounds(field, text, caret, location)
        field_rect = self._field_bounds(field)
        render, evidence = (self._inline_metrics(field, text, caret, location, rect, field_rect,
                                                 browser=self._is_browser(app_id))
                            if length == 0 else (None, None))
        window = self._attribute(field, "AXWindow")
        window_id = f"{pid}:{hash(window)}" if window is not None else ""
        window_bounds = self._field_bounds(window) if window is not None else None
        window_revision = repr((window_id, field_rect))
        multiline = self._attribute(field, "AXRole") == "AXTextArea"
        # CFEqual/hash on AXUIElement retains identity across fresh AX references.
        # Browser identities also bind the native document, without exposing URLs.
        self._focused_field_hash = hash(field)
        snapshot = Snapshot(app_id, field_id, text, caret, end - caret, rect,
                            domain=domain, multiline=multiline, field_rect=field_rect,
                            window_id=window_id, window_revision=window_revision,
                            window_pid=pid, window_bounds=window_bounds)
        render = self._styled_render(snapshot, render, evidence)
        if render is not None:
            snapshot = dataclasses.replace(snapshot, inline_render=render)
        # Re-resolve foreground policy, security, writability and field identity
        # after asynchronous AX calls. Never return a snapshot of stale focus.
        if (self._focused() != focused
                or self._field_identity(field, pid, app_id) != field_id):
            return None
        return snapshot, field

    def _styled_render(self, snapshot, render, evidence):
        """Optional screenshot appearance for metrics AX alone could not verify."""
        if render is not None or not evidence or not self._screenshot_appearance:
            return render
        try:
            from . import macos_vision
            styled = macos_vision.request_inline_style(
                snapshot, evidence, enabled=True,
                authorized=lambda: (not self._stopped.is_set() and self._screenshot_appearance
                                    and self.context_matches(snapshot)))
        except Exception:
            return None
        if not isinstance(styled, InlineRender) or styled.source not in _VISION_SOURCES:
            return None
        verified = evidence.get("ax_render")
        if verified:
            # Mid-line: keep AX-verified geometry; accept only measured colours.
            base = InlineRender(**verified)
            suffix = evidence.get("suffix") or {}
            if (styled.background_rgb is None or styled.text_rgb is None
                    or abs(styled.baseline - base.baseline) > 0.75
                    or abs(styled.font_size - base.font_size) > 0.01):
                return None
            candidate = dataclasses.replace(
                base, source="macos-ax-attributed-run+vision", suffix=str(suffix.get("text") or ""),
                suffix_width=float(suffix.get("rect", (0, 0, 0, 0))[2]),
                text_rgb=tuple(styled.text_rgb), background_rgb=tuple(styled.background_rgb))
        else:
            if styled.suffix or styled.suffix_width:
                return None
            candidate = styled
        return (candidate if candidate.valid(snapshot.caret_rect)
                and candidate.suffix == snapshot.line_suffix else None)

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
            # Qt omits zero-length bounds at the document end, while exposing
            # the final glyph's real advance and row. Only that collapsed LTR
            # endpoint is derivable without guessing or displacing host text.
            line_start = max(text.rfind("\n", 0, caret), text.rfind("\r", 0, caret)) + 1
            if (not caret or caret != len(text) or text[caret - 1] in "\r\n"
                    or unicodedata.combining(text[caret - 1])
                    or any(unicodedata.bidirectional(char) in _RTL for char in text[line_start:caret])):
                return None
            units = 2 if ord(text[caret - 1]) > 0xFFFF else 1
            glyph = self._range_bounds(field, location - units, units)
            if glyph is None or glyph[2] <= 0:
                return None
            return glyph[0] + glyph[2], glyph[1], 1.0, glyph[3]
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

    def _field_ancestry(self, field):
        """Bounded, non-protected native ancestry; no text or names are read."""
        chain = []
        secure_description = getattr(self, "_secure_description", None)
        for _ in range(32):
            if field is None:
                return chain
            if field in chain:
                return None
            if (self._attribute(field, "AXHidden")
                    or self._attribute(field, "AXProtectedContent")
                    or self._attribute(field, "AXSubrole") == "AXSecureTextField"
                    or (secure_description is not None
                        and self._attribute(field, "AXRoleDescription") == secure_description)):
                return None
            chain.append(field)
            field = self._attribute(field, "AXParent")
        return None  # A truncated chain is not proof of visibility or protection.

    def _visible_clip(self, field, field_rect):
        """Safe ancestry; visual clipping ends at the owning native window."""
        chain = self._field_ancestry(field)
        if chain is None:
            return None
        window = self._attribute(field, "AXWindow")
        if window is None:
            window = next((node for node in chain[1:]
                           if self._attribute(node, "AXRole") == "AXWindow"), None)
        clip = field_rect
        for parent in chain[1:]:
            if self._attribute(parent, "AXRole") == "AXApplication":
                break
            bounds = self._field_bounds(parent)
            if bounds:
                left, top = max(clip[0], bounds[0]), max(clip[1], bounds[1])
                right = min(clip[0] + clip[2], bounds[0] + bounds[2])
                bottom = min(clip[1] + clip[3], bounds[1] + bounds[3])
                if right <= left or bottom <= top:
                    return None
                clip = (left, top, right - left, bottom - top)
            if parent == window:
                break
        return clip

    def _font_candidates(self, name: str, family: str, size: float):
        """(font, system, weight) reproductions of an AX font; never a guessed family."""
        from AppKit import NSFont
        if name.startswith(".") or family.startswith("."):
            weight = _system_weight(name or family)
            font = NSFont.systemFontOfSize_weight_(size, weight) if weight is not None else None
            return [(font, True, weight)] if font is not None else []
        font = NSFont.fontWithName_size_(name, size) if name else None
        return [(font, False, 0.0)] if font is not None and str(font.fontName()) == name else []

    @staticmethod
    def _scaled(font, system: bool, weight: float, size: float):
        from AppKit import NSFont
        if system:
            scaled = NSFont.systemFontOfSize_weight_(size, weight)
        else:
            scaled = NSFont.fontWithName_size_(str(font.fontName()), size)
        return scaled if scaled is not None and str(scaled.fontName()) == str(font.fontName()) else None

    @staticmethod
    def _fitted_system_render(span, whole, rect, clip):
        """End-of-line display when AX supplies geometry but omits styling.

        This is an explicitly fitted system font, not a claim to reproduce the
        host font. The native glyph edge anchors the ghost; no host text is
        erased or repainted, and screenshots are not required.
        """
        from AppKit import NSFont
        if whole is None or not span.strip():
            return None
        seed = NSFont.systemFontOfSize_weight_(13.0, 0.0)
        width = text_width(seed, span)
        if not math.isfinite(width) or width <= 0:
            return None
        size = 13.0 * whole[2] / width
        if not math.isfinite(size) or not 1 <= size <= 300:
            return None
        font = NSFont.systemFontOfSize_weight_(size, 0.0)
        ascent, descent = float(font.ascender()), abs(float(font.descender()))
        if ascent + descent > rect[3]:
            size *= rect[3] / (ascent + descent)
            font = NSFont.systemFontOfSize_weight_(size, 0.0)
            ascent, descent = float(font.ascender()), abs(float(font.descender()))
        baseline = rect[1] + (rect[3] - ascent - descent) / 2 + ascent
        render = InlineRender(str(font.fontName()), size, baseline, clip, ascent, descent,
                              source="macos-ax-caret-row", system_font=True)
        return render if render.valid(rect) else None

    @staticmethod
    def _calibrate(font, text: str, part_text: str, width: float, part_width: float,
                   browser: bool, coordinate_tolerance: float):
        """Scale verified from two host spans, or None when glyph advances disagree.

        The whole span sets the ratio; the sub-span ending at the caret must
        then agree independently. Only browsers may differ from 1.0, and only
        by an offered page-zoom factor.
        """
        measured = text_width(font, text)
        part = text_width(font, part_text) if part_text else None
        if measured <= 0 or (part is not None and part <= 0):
            return None
        if (abs(width - measured) <= max(coordinate_tolerance, _tolerance(measured))
                and (part is None or abs(part_width - part) <= max(coordinate_tolerance, _tolerance(part)))):
            return 1.0
        if part is None or not browser or len(text) < 8 or width < 30:
            return None
        ratio = width / measured
        zoom = min(_ZOOM_LEVELS, key=lambda level: abs(level - ratio))
        if (abs(ratio - zoom) > 0.01 * zoom
                or abs(part_width - part * zoom) > max(coordinate_tolerance, _tolerance(part * zoom))):
            return None
        return zoom

    def _layout(self):
        from AppKit import NSLayoutManager
        layout = self._layout_manager
        if layout is None:
            layout = self._layout_manager = NSLayoutManager.alloc().init()
        return layout

    def _inline_metrics(self, field, text, caret, location, rect, field_rect, browser=False):
        """Verify host font, advances and baseline; return (render, vision evidence).

        Evidence is returned only when the caret row geometry itself is sound
        but AX alone could not prove the font/baseline (or a mid-line suffix's
        colours), so optional screenshot appearance may verify it.
        """
        if not rect or not field_rect or not caret:
            return None, None
        line_start = max(text.rfind("\n", 0, caret), text.rfind("\r", 0, caret)) + 1
        ends = [index for index in (text.find("\n", caret), text.find("\r", caret)) if index >= 0]
        line_end = min(ends) if ends else len(text)
        before, after = text[line_start:caret], text[caret:line_end]
        if (not before or "\t" in after
                or any(unicodedata.bidirectional(char) in _RTL for char in before + after)):
            return None, None
        span = before[before.rfind("\t") + 1:][-32:]
        if not span.strip():
            return None, None
        try:
            suffix_units = index_to_utf16(text, line_end) - location
            visible = self._attribute(field, "AXVisibleCharacterRange")
            if visible is not None:
                valid, shown = self.ax.AXValueGetValue(visible, self.ax.kAXValueCFRangeType, None)
                if not valid:
                    return None, None
                shown = int(shown[0]), int(shown[1])
                # UIKit advertises this attribute but returns (0, 0) even for
                # visible non-empty composers. Treat that sentinel as unknown,
                # not an empty viewport: glyph bounds plus ancestor clipping
                # must still independently prove the visible caret row below.
                if shown != (0, 0) and (not shown[0] < location
                        or location + suffix_units > shown[0] + shown[1]):
                    return None, None
            clip = self._visible_clip(field, field_rect)
            if (clip is None or rect[0] < clip[0] or rect[0] + 1 > clip[0] + clip[2]
                    or rect[1] < clip[1] or rect[1] + rect[3] > clip[1] + clip[3] + 1):
                return None, None
            # A logical paragraph can wrap. AX bounds spanning two visual rows
            # cannot calibrate the caret row; shrink only the leading sample,
            # retaining independently observed glyph bounds and font checks.
            while span:
                base = index_to_utf16(text, caret - len(span))
                whole = self._range_bounds(field, base, location - base)
                if (whole is not None and whole[2] > 0
                        and abs(whole[1] - rect[1]) <= 1
                        and whole[3] <= rect[3] + 1
                        and abs(whole[0] + whole[2] - rect[0]) <= 1.5):
                    break
                span = span[(len(span) + 1) // 2:]
            if not span.strip():
                return None, None
            base = index_to_utf16(text, caret - len(span))
            caret_offset = location - base
            value = self.ax.AXValueCreate(self.ax.kAXValueCFRangeType, (base, caret_offset + suffix_units))
            error, attributed = self.ax.AXUIElementCopyParameterizedAttributeValue(
                field, "AXAttributedStringForRange", value, None)
            expected_units = caret_offset + suffix_units
            expected_text = span + after
            if (error or attributed is None or attributed.length() != expected_units
                    or str(attributed.string()) != expected_text):
                return None, None
            attributes, run = attributed.attributesAtIndex_effectiveRange_(caret_offset - 1, None)
            if run is None:
                return None, None
            run_start, run_end = int(run[0]), int(run[0]) + int(run[1])
            if not 0 <= run_start <= caret_offset - 1 < run_end <= expected_units:
                return None, None
            # Calibrate only within the uniform run that ends at the caret.
            if run_start > 0:
                span = span[utf16_to_index(span, run_start):]
            if not span.strip():
                return None, None
            span_location = base + max(0, run_start)
            whole = (self._range_bounds(field, span_location, location - span_location)
                     if run_start > 0 else whole)
            descriptor = attributes.get("AXFont")
            # Superscripts, baseline shifts and host IME runs are not plain text.
            if (any(attributes.get(key) for key in ("AXSuperscript", "NSBaselineOffset", "AXMarkedText"))
                    or any(key in attributes for key in ("NSMarkedClauseSegment",
                                                         "NSMarkedClauseSegmentAttributeName"))):
                return None, None
            if not descriptor:
                return ((self._fitted_system_render(span, whole, rect, clip), None)
                        if not after else (None, None))
            name = str(descriptor.get("AXFontName") or "")
            family = str(descriptor.get("AXFontFamily") or "")
            size = float(descriptor.get("AXFontSize") or 0)
            if not math.isfinite(size) or not 1 <= size <= 300:
                return None, None
            half = len(span) // 2
            part_text = span[half:] if half else ""
            if half and not part_text.strip():
                return None, None
            part = (self._range_bounds(field, span_location + index_to_utf16(span, half),
                                       location - span_location - index_to_utf16(span, half))
                    if half else whole)
            # Both spans lie on one visual row and end exactly at the caret.
            if (whole is None or part is None or whole[2] <= 0 or part[2] <= 0
                    or abs(whole[1] - part[1]) > 1 or abs(whole[3] - part[3]) > 1
                    or abs(whole[0] + whole[2] - rect[0]) > 1.5 or abs(part[0] + part[2] - rect[0]) > 1.5
                    or not whole[1] - 1 <= rect[1] <= whole[1] + whole[3]):
                return None, None
            samples = [{"text": span, "rect": whole}]
            if half:
                samples.append({"text": part_text, "rect": part})
            evidence = {"font_name": name, "font_family": family, "font_size": size,
                        "samples": samples, "caret_rect": rect, "clip_rect": clip}
            # AX range rectangles can include a rounded ink-envelope pixel,
            # particularly in one-character styling runs. The observed caret
            # supplies the actual advance; the endpoint checks above still
            # reject unrelated rows or materially inconsistent geometry.
            whole_advance = rect[0] - whole[0]
            part_advance = rect[0] - part[0]
            # Two independently rounded integer edges can differ by one point.
            # Fractional AX coordinates retain the stricter sub-point bound.
            coordinate_tolerance = (1.0 if all(float(edge).is_integer()
                                               for edge in (rect[0], whole[0], part[0])) else 0.75)
            verified = None
            for font, system, weight in self._font_candidates(name, family, size):
                scale = self._calibrate(font, span, part_text, whole_advance, part_advance,
                                        browser, coordinate_tolerance)
                if scale is None:
                    continue
                if scale != 1.0:
                    font = self._scaled(font, system, weight, size * scale)
                    if font is None or abs(text_width(font, span) - whole_advance) > _tolerance(whole_advance):
                        continue
                if verified is not None:
                    return None, evidence  # Two reproductions fit: ambiguous.
                verified = (font, system, weight, size * scale)
            if verified is None:
                return None, evidence
            font, system, weight, effective = verified
            ascent, descent = float(font.ascender()), abs(float(font.descender()))
            top, height = whole[1], whole[3]
            # Line-box models, each quantitatively matched to the observed row:
            # a glyph content box (ascent+descent), a TextKit line fragment,
            # or (browsers only) a CSS line box with symmetric half-leading.
            candidates = []
            if abs(height - (ascent + descent)) <= 1.0:
                candidates.append(top + ascent)
            layout = self._layout()
            # Integral AX row edges can round the native line box up by one
            # point (UIKit/Catalyst). Larger or fractional mismatches still
            # require the optional pixel measurement.
            line_tolerance = 1.0 if float(height).is_integer() else 0.75
            if abs(height - float(layout.defaultLineHeightForFont_(font))) <= line_tolerance:
                candidates.append(top + float(layout.defaultBaselineOffsetForFont_(font)))
            if browser and not candidates and ascent + descent < height <= 3 * (ascent + descent):
                candidates.append(top + (height - ascent - descent) / 2 + ascent)
            if not candidates or max(candidates) - min(candidates) > 0.75:
                return None, evidence
            baseline = candidates[0] if browser else candidates[-1]
            render = InlineRender(str(font.fontName()), effective, baseline, clip, ascent, descent,
                                  source="macos-ax-attributed-run", system_font=system,
                                  system_weight=weight if system else 0.0)
            if not render.valid(rect):
                return None, evidence
            if not after:
                return render, None
            # Mid-line: the same run must continue to the line end, and the
            # suffix must be measured on the caret row so it can be displaced.
            if run_end < caret_offset + suffix_units:
                return None, None
            suffix_rect = self._range_bounds(field, location, suffix_units)
            expected = text_width(font, after)
            if (suffix_rect is None or abs(suffix_rect[1] - top) > 1 or abs(suffix_rect[3] - height) > 1
                    or abs(suffix_rect[0] - rect[0]) > 1.5 or suffix_rect[2] <= 0
                    or abs(suffix_rect[2] - expected) > _tolerance(expected)):
                return None, None
            if not after.strip():
                blank = dataclasses.replace(render, suffix=after, suffix_width=suffix_rect[2])
                return (blank if blank.valid(rect) else None), None
            return None, {**evidence, "suffix": {"text": after, "rect": suffix_rect},
                          "ax_render": dataclasses.asdict(render)}
        except Exception as error:
            self._capture_error = f"Last native inline-layout failure: {type(error).__name__}. No writing is logged."
            return None, None

    def range_rect(self, snapshot: Snapshot, start: int, length: int):
        """Host bounds of a same-row range before/around the caret, or None."""
        if (snapshot.protected or snapshot.caret_rect is None or not isinstance(start, int)
                or not isinstance(length, int) or length <= 0
                or not 0 <= start <= start + length <= len(snapshot.text)
                or any(char in "\r\n" for char in snapshot.text[min(start, snapshot.caret):
                                                                  max(start + length, snapshot.caret)])):
            return None
        with self._lock:
            try:
                current = self._capture()
                if current is None or current[0].key != snapshot.key or current[0].caret_rect is None:
                    return None
                live, field = current
                location = index_to_utf16(live.text, start)
                units = index_to_utf16(live.text, start + length) - location
                bounds = self._range_bounds(field, location, units)
                caret_location = index_to_utf16(live.text, live.caret)
                if live.caret:
                    width = 2 if ord(live.text[live.caret - 1]) > 0xFFFF else 1
                    row = self._range_bounds(field, caret_location - width, width)
                else:
                    row = None
                if (bounds is None or row is None or bounds[2] <= 0
                        or abs(bounds[1] - row[1]) > 1 or abs(bounds[3] - row[3]) > 1):
                    return None
                field_rect = live.field_rect
                if field_rect and (bounds[0] < field_rect[0] - 1
                                   or bounds[0] + bounds[2] > field_rect[0] + field_rect[2] + 1):
                    return None
                return bounds
            except Exception:
                return None

    def context_matches(self, snapshot):
        if self._stopped.is_set() or self._dead_key:
            return False
        try:
            if snapshot.app_id in ("com.apple.Terminal", "com.googlecode.iterm2"):
                return bool(snapshot.field_id.startswith("terminal:")
                            and self._terminal().context_matches(self, snapshot))
            focused = self._focused()
            if focused is None:
                return False
            field, pid, app_id, domain = focused
            marked_range = self._attribute(field, "AXMarkedTextRange")
            if marked_range is None:
                if self._ime_kind != "layout":
                    return False
            else:
                valid, marked = self.ax.AXValueGetValue(
                    marked_range, self.ax.kAXValueCFRangeType, None)
                if not valid or marked[1] > 0:
                    return False
            window = self._attribute(field, "AXWindow")
            return (app_id == snapshot.app_id and domain == snapshot.domain
                    and self._field_identity(field, pid, app_id) == snapshot.field_id
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
                    or self._field_identity(field, pid, app_id) != snapshot.field_id
                    or app_id != snapshot.app_id):
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
            deadline = time.monotonic() + 0.15
            stack, chunks, remaining, visited = [(content_root, 0)], [], min(12000, max(0, max_chars)), set()
            for _ in range(128):
                if not stack or remaining <= 0 or time.monotonic() >= deadline:
                    break
                element, depth = stack.pop()
                if (not authorized() or self._focused() != focused
                        or self._field_identity(field, pid, app_id) != snapshot.field_id):
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
            if (not authorized() or self._focused() != focused
                    or self._field_identity(field, pid, app_id) != snapshot.field_id):
                return {**result, "status": "blocked"}
            text = "\n".join(chunks)[:max_chars]
            return {**result, "text": text, "status": "ready" if text else "empty"}
        except Exception:
            return {**result, "status": "error"}

    def capture(self) -> Snapshot | None:
        with self._lock:
            try:
                result = self._capture()
                if result is not None and result[0].inline_safe:
                    self._capture_error = ""
                return result[0] if result else None
            except Exception as error:
                self._capture_error = f"Last native capture failure: {type(error).__name__}. No writing is logged."
                return None

    def _post_key(self, code: int, flags: int, unicode: tuple[str, int] | None = None) -> None:
        """Post a tagged key down/up pair into the login session."""
        q = self.q
        for down in (True, False):
            event = q.CGEventCreateKeyboardEvent(None, code, down)
            q.CGEventSetFlags(event, flags)
            if unicode is not None:
                q.CGEventKeyboardSetUnicodeString(event, unicode[1], unicode[0])
            q.CGEventSetIntegerValueField(event, q.kCGEventSourceUserData, SYNTHETIC_TAG)
            q.CGEventPost(q.kCGSessionEventTap, event)

    def insert(self, snapshot: Snapshot, text: str) -> bool:
        if not text or "\x00" in text or snapshot.protected or snapshot.selection_length:
            return False
        with self._lock:
            try:
                if snapshot.app_id in ("com.apple.Terminal", "com.googlecode.iterm2"):
                    return bool(snapshot.field_id.startswith("terminal:")
                                and self._terminal().append(self, snapshot, text))
                current = self._capture()
                if current is None or current[0].key != snapshot.key:
                    return False
                field = current[1]
                if self.ax.AXUIElementSetMessagingTimeout(field, _AX_MUTATION_TIMEOUT):
                    self._set_status("macOS could not configure a bounded Accessibility edit; no text was inserted.")
                    return False
                # AXSelectedText replaces only the revalidated collapsed range.
                if self._settable(field, "AXSelectedText"):
                    if not self._commit_input():
                        return False
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
                if not self._commit_input():
                    return False
                for chunk, units in chunks:
                    self._post_key(0, 0, (chunk, units))
                return True
            except Exception:
                return False

    def replace_range(self, snapshot: Snapshot, start: int, length: int, text: str) -> bool:
        if not snapshot.can_replace or not self._valid_replacement(snapshot, start, length, text):
            return False
        with self._lock:
            try:
                current = self._capture()
                if current is None or current[0].key != snapshot.key:
                    return False
                if not self._input_valid():
                    return False
                field = current[1]
                if self.ax.AXUIElementSetMessagingTimeout(field, _AX_MUTATION_TIMEOUT):
                    self._set_status("macOS could not configure a bounded Accessibility edit; no text was replaced.")
                    return False
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
                    if not self._commit_input():
                        return False
                    if self.ax.AXUIElementSetAttributeValue(field, "AXSelectedTextRange", native_range):
                        return False
                    selected = self._capture()
                    if selected is None:
                        return False
                    expected = dataclasses.replace(snapshot, caret=start, selection_length=length)
                    if selected[0].key != expected.key:
                        return False
                if not self._commit_input():
                    return False
                return self.ax.AXUIElementSetAttributeValue(field, "AXSelectedText", text) == 0
            except Exception:
                return False

    def input_transaction(self):
        """Published before dispatch, including the original event."""
        return self._input_gate

    def _input_valid(self):
        gate = self._insertion_gate
        return gate is None or (not gate["aborted"] and time.monotonic() < gate["deadline"])

    def _commit_input(self):
        # Linearize the irreversible native operation after authoritative AX
        # validation. The input mutex is never held during the native call.
        with self._input_mutex:
            gate = self._insertion_gate
            if not self._input_valid():
                return False
            if gate is not None:
                gate["committing"] = True
            return True

    def begin_input_transaction(self, gate):
        if gate is not None and (gate is not self._input_gate or gate["aborted"]
                                 or time.monotonic() >= gate["deadline"]):
            return False
        self._insertion_gate = gate
        return True

    def finish_input_transaction(self, gate, success, *, presentation_key=None):
        with self._input_mutex:
            self._insertion_gate = None
            if gate is None:
                return
            gate["success"] = bool(success)
            gate["ready"] = True
            gate["presentation_key"] = presentation_key
            gate["committing"] = False
            if gate["aborted"]:
                self._abort_input(gate)

    def acknowledge_input_presentation(self, key):
        with self._input_mutex:
            gate = self._input_gate
            if gate is not None and gate.get("presentation_key") == key:
                gate["presentation_key"] = None
                self._drain_input_locked()

    def _post_original(self, event):
        self.q.CGEventSetIntegerValueField(event, self.q.kCGEventSourceUserData, SYNTHETIC_TAG)
        self.q.CGEventPost(self.q.kCGSessionEventTap, event)

    def _abort_input(self, expected=None):
        with self._input_mutex:
            self._abort_input_locked(expected)

    def _abort_input_locked(self, expected=None):
        gate = self._input_gate
        if gate is None or (expected is not None and expected is not gate):
            return
        gate["aborted"] = True
        # A native mutation already issued cannot be revoked. Its completion
        # owns release; never deliver following events ahead of that mutation.
        if gate["committing"]:
            # The pre-commit limits cannot revoke an issued OS request.
            # Preserve input until it returns, then disable interception rather
            # than silently repeating an overrun. A non-returning OS setter is
            # deliberately not claimed to have a strict FIFO memory bound.
            gate["commit_overrun"] = True
            return
        self._input_gate = None
        gate["timer"].cancel()
        self._note_input()
        if not gate["success"]:
            self._suppressed_keyups = self._suppressed_keyups - {gate["code"]}
            self._post_original(gate["original"])
        else:
            self._suppressed_keyups = self._suppressed_keyups | {gate["code"]}
        while gate["events"]:
            event_type, event = gate["events"].popleft()
            if (gate["success"] and event_type == self.q.kCGEventKeyUp
                    and self.q.CGEventGetIntegerValueField(event, self.q.kCGKeyboardEventKeycode) == gate["code"]):
                self._suppressed_keyups = self._suppressed_keyups - {gate["code"]}
                continue
            self._post_original(event)
        if gate.get("commit_overrun"):
            self.set_shortcuts_enabled(False)
            self._set_status(
                "macOS exceeded the deferred-input limit during an Accessibility edit. "
                "Queued input was preserved and released in order; shortcut interception is disabled. "
                "Pause and resume global typing to retry.")

    def drain_input_transaction(self):
        with self._input_mutex:
            self._drain_input_locked()

    def _drain_input_locked(self):
        """UI-thread release only after overlay acknowledgments have run."""
        gate = self._input_gate
        if gate is None:
            return
        if gate["aborted"] or time.monotonic() >= gate["deadline"]:
            self._abort_input()
            return
        if not gate["ready"] or gate.get("presentation_key") is not None:
            return
        self._input_gate = None
        gate["timer"].cancel()
        if not gate["success"]:
            self._note_input()
            self._suppressed_keyups = self._suppressed_keyups - {gate["code"]}
            self._post_original(gate["original"])
        else:
            self._suppressed_keyups = self._suppressed_keyups | {gate["code"]}
        while gate["events"]:
            event_type, event = gate["events"].popleft()
            # The initiating down was consumed, so its matching up is too.
            if (gate["success"] and event_type == self.q.kCGEventKeyUp
                    and self.q.CGEventGetIntegerValueField(event, self.q.kCGKeyboardEventKeycode) == gate["code"]):
                self._suppressed_keyups = self._suppressed_keyups - {gate["code"]}
                continue
            result = self._event_dispatch(event_type, event)
            if result is not None:
                self._post_original(result)
            if self._input_gate is not None:
                self._input_gate["events"].extend(gate["events"])
                gate["events"].clear()
                break

    def _open_input(self, action, code, event):
        gate = {"action": action, "code": code, "original": self.q.CGEventCreateCopy(event),
                "events": deque(), "deadline": time.monotonic() + _REPLAY_SECONDS,
                "aborted": False, "ready": False, "success": False, "committing": False}
        gate["timer"] = threading.Timer(_REPLAY_SECONDS, lambda: self._abort_input(gate))
        gate["timer"].daemon = True
        self._input_gate = gate
        return gate

    def replay_action(self, action: str) -> bool:
        """Deliver a consumed shortcut to the host once, tagged to bypass the tap.

        ``plain_tab`` posts an unmodified Tab (accessory Option-click).
        """
        if action != "plain_tab":
            with self._input_mutex:
                gate = self._input_gate
                if (gate is None or gate["action"] != action or gate["success"] or gate["committing"]):
                    return False
                self._abort_input(gate)
                return True
        code, flags = _TAB_KEYCODE, 0
        try:
            if self._stopped.is_set() or not self.q.CGPreflightPostEventAccess():
                return False
            self._post_key(code, flags)
            return True
        except Exception:
            return False

    def start_shortcuts(self, callback: Shortcut) -> None:
        super().start_shortcuts(callback)
        with self._lock:
            if self._stopped.is_set():
                return
            if self._thread is None:
                self._thread = threading.Thread(target=self._event_loop, name="inklow-quartz", daemon=True)
                self._thread.start()
            if self._observer_thread is None:
                self._observer_thread = threading.Thread(target=self._observer_loop,
                                                         name="inklow-ax-observer", daemon=True)
                self._observer_thread.start()

    def _note_composition(self, event, flags) -> None:
        """A key that yields no characters without Command/Control is a dead key."""
        q = self.q
        if flags & (q.kCGEventFlagMaskCommand | q.kCGEventFlagMaskControl):
            self._dead_key = False
            return
        try:
            length, _ = q.CGEventKeyboardGetUnicodeString(event, 4, None, None)
        except Exception:
            return
        self._dead_key = not length

    def _event_loop(self) -> None:
        import objc
        q, cf = self.q, self.cf

        def recover_timeout():
            # macOS disables a tap that answered slowly; a key may have been
            # delivered without us, so every armed suggestion is stale.
            now = time.monotonic()
            recent = tuple(moment for moment in self._tap_timeouts if now - moment < 60) + (now,)
            self._tap_timeouts = recent
            self._abort_input()
            self._note_input()
            tap = self._tap
            if len(recent) <= 3:
                if tap is not None and self._enabled:
                    q.CGEventTapEnable(tap, True)
            else:
                self.set_shortcuts_enabled(False)
                self._set_status("macOS repeatedly timed out the keyboard tap, so BroTypist stopped intercepting "
                                 "keys. Pause and resume global typing to re-enable it.")

        def handle_event(event_type, event):
            if event_type == q.kCGEventTapDisabledByTimeout:
                recover_timeout()
                return event
            if event_type == q.kCGEventTapDisabledByUserInput:
                # CGEventTapEnable(False) also queues this notification. It may
                # arrive after a new suggestion enabled the tap; preserve the
                # current requested state instead of disabling that suggestion.
                tap = self._tap
                if tap is not None and self._enabled:
                    q.CGEventTapEnable(tap, True)
                return event
            if (q.CGEventGetIntegerValueField(event, q.kCGEventSourceUserData) == SYNTHETIC_TAG
                    or q.CGEventGetIntegerValueField(event, q.kCGEventSourceUnixProcessID) == os.getpid()):
                return event
            gate = self._input_gate
            if gate is not None:
                if time.monotonic() >= gate["deadline"] or len(gate["events"]) >= 256:
                    self._abort_input()
                    if self._input_gate is None:
                        self._post_original(event)
                        return None
                gate["events"].append((event_type, q.CGEventCreateCopy(event)))
                return None
            if event_type == q.kCGEventKeyUp:
                code = q.CGEventGetIntegerValueField(event, q.kCGKeyboardEventKeycode)
                if code in self._suppressed_keyups:
                    self._suppressed_keyups = self._suppressed_keyups - {code}
                    return None
            if event_type != q.kCGEventKeyDown:
                if event_type != q.kCGEventKeyUp:
                    self._note_input()
                return event
            flags = q.CGEventGetFlags(event)
            code = q.CGEventGetIntegerValueField(event, q.kCGKeyboardEventKeycode)
            key = self._key_names.get(code, "")
            if flags & q.kCGEventFlagMaskSecondaryFn and not (len(key) > 1 and key.startswith("F")):
                self._note_input()
                return event
            modifiers = tuple(name for name, mask in (
                ("Ctrl", q.kCGEventFlagMaskControl), ("Alt", q.kCGEventFlagMaskAlternate),
                ("Shift", q.kCGEventFlagMaskShift), ("Super", q.kCGEventFlagMaskCommand))
                if flags & mask)
            action = self._key_action(key, modifiers, platform="macos")
            if action == "bypass":
                if self._dispatch(action):
                    q.CGEventSetFlags(event, flags & ~q.kCGEventFlagMaskAlternate)
                self._dead_key = False
                self._note_input()
                self._suppressed_keyups = self._suppressed_keyups - {code}
                return event
            if action:
                gate = self._open_input(action, code, event)
                if self._dispatch(action):
                    gate["timer"].start()
                    return None
                if self._input_gate is gate:
                    self._input_gate = None
            self._suppressed_keyups = self._suppressed_keyups - {code}
            self._note_composition(event, flags)
            self._note_input()
            return event
        def dispatch_event(event_type, event):
            with self._input_mutex:
                return handle_event(event_type, event)


        def event_callback(proxy, event_type, event, refcon):
            # Quartz runs outside AppKit's main-loop autorelease pools.
            with objc.autorelease_pool():
                return dispatch_event(event_type, event)

        def pointer_callback(proxy, event_type, event, refcon):
            with objc.autorelease_pool():
                if event_type in (q.kCGEventTapDisabledByTimeout, q.kCGEventTapDisabledByUserInput):
                    pointer = self._pointer_tap
                    if pointer is not None and not self._stopped.is_set():
                        q.CGEventTapEnable(pointer, True)
                    self._note_input()
                    return event
                # The ordered head tap owns pointer invalidation. This
                # listen-only tap also observes input while interception is off.
                if not self._enabled:
                    self._dead_key = False
                    self._note_input()
                return event

        self._event_dispatch = dispatch_event
        self._event_callback = event_callback  # Retain the C callback bridges.
        self._pointer_callback = pointer_callback
        try:
            tap = q.CGEventTapCreate(q.kCGSessionEventTap, q.kCGHeadInsertEventTap, q.kCGEventTapOptionDefault,
                                     sum(1 << kind for kind in (
                                         q.kCGEventKeyDown, q.kCGEventKeyUp, q.kCGEventFlagsChanged,
                                         q.kCGEventLeftMouseDown, q.kCGEventLeftMouseUp,
                                         q.kCGEventRightMouseDown, q.kCGEventRightMouseUp,
                                         q.kCGEventOtherMouseDown, q.kCGEventOtherMouseUp,
                                         q.kCGEventScrollWheel)), event_callback, None)
            if tap is None:
                self._set_status(
                    "Keyboard capture denied. Enable Accessibility and Input Monitoring for BroTypist "
                    "(or its terminal/Python host) in System Settings → Privacy & Security, then restart."
                )
                return
            # Listen-only: pointer events are observed, never delayed or altered.
            pointer = q.CGEventTapCreate(
                q.kCGSessionEventTap, q.kCGTailAppendEventTap, q.kCGEventTapOptionListenOnly,
                (1 << q.kCGEventLeftMouseDown) | (1 << q.kCGEventRightMouseDown)
                | (1 << q.kCGEventOtherMouseDown) | (1 << q.kCGEventScrollWheel),
                pointer_callback, None)
            runloop = cf.CFRunLoopGetCurrent()
            sources = [cf.CFMachPortCreateRunLoopSource(None, port, 0)
                       for port in (tap, pointer) if port is not None]
            with self._state_lock:
                self._tap, self._pointer_tap, self._runloop = tap, pointer, runloop
                q.CGEventTapEnable(tap, self._enabled and not self._stopped.is_set())
            if pointer is not None:
                q.CGEventTapEnable(pointer, True)
            for source in sources:
                cf.CFRunLoopAddSource(runloop, source, cf.kCFRunLoopCommonModes)
            self._refresh_status()
            if not self._stopped.is_set():
                cf.CFRunLoopRun()
            for source in sources:
                cf.CFRunLoopRemoveSource(runloop, source, cf.kCFRunLoopCommonModes)
            for port in (tap, pointer):
                if port is not None:
                    cf.CFMachPortInvalidate(port)
        except Exception as exc:
            self._set_status(f"Quartz event tap unavailable ({type(exc).__name__}). Check Input Monitoring permissions and restart.")
        finally:
            with self._state_lock:
                self._tap, self._pointer_tap, self._runloop = None, None, None

    def _observer_loop(self) -> None:
        """AX notifications for the foreground app: prompt invalidation/reposition."""
        import objc
        ax, cf = self.ax, self.cf

        def notified(observer, element, notification, refcon):
            with objc.autorelease_pool():
                name = str(notification)
                user = name in _USER_NOTIFICATIONS
                if name in {"AXValueChanged", "AXSelectedTextChanged"}:
                    if hash(element) != self._focused_field_hash:
                        return
                    now = time.monotonic()
                    if now - self._observer_value_at < 0.05:
                        return
                    self._observer_value_at = now
                if user:
                    self._dead_key = False
                self._note_input(user=user)

        self._observer_callback = notified  # Retain the C callback bridge.
        runloop = cf.CFRunLoopGetCurrent()
        self._observer_runloop = runloop
        observed, source = 0, None
        try:
            while not self._stopped.is_set():
                pid = self._observe_pid
                if pid != observed:
                    if source is not None:
                        cf.CFRunLoopRemoveSource(runloop, source, cf.kCFRunLoopDefaultMode)
                    source, observed = None, pid
                    if pid and pid != os.getpid():
                        error, observer = ax.AXObserverCreate(pid, notified, None)
                        if not error and observer is not None:
                            app = ax.AXUIElementCreateApplication(pid)
                            for name in _AX_NOTIFICATIONS:
                                ax.AXObserverAddNotification(observer, app, name, None)
                            source = ax.AXObserverGetRunLoopSource(observer)
                            cf.CFRunLoopAddSource(runloop, source, cf.kCFRunLoopDefaultMode)
                if source is None:
                    self._stopped.wait(0.25)  # An empty run loop would return immediately.
                else:
                    cf.CFRunLoopRunInMode(cf.kCFRunLoopDefaultMode, 0.25, False)
        except Exception:
            pass
        finally:
            if source is not None:
                try:
                    cf.CFRunLoopRemoveSource(runloop, source, cf.kCFRunLoopDefaultMode)
                except Exception:
                    pass
            self._observer_runloop = None

    def set_shortcuts_enabled(self, enabled: bool) -> None:
        if not enabled:
            self._abort_input()
        with self._state_lock:
            previous = self._enabled
            self._enabled = bool(enabled) and not self._stopped.is_set()
            changed = previous != self._enabled
            if self._tap is not None and changed:
                self.q.CGEventTapEnable(self._tap, self._enabled)
            enabled_now = self._enabled
        if changed and enabled_now and self._tap is not None:
            self._refresh_status()

    def stop(self) -> None:
        self._abort_input()
        from . import macos_vision
        macos_vision.reset()
        super().stop()
        for center, token in self._notification_tokens:
            try:
                center.removeObserver_(token)
            except Exception:
                pass
        self._notification_tokens = ()
        for runloop in (self._runloop, self._observer_runloop):
            if runloop is not None:
                self.cf.CFRunLoopStop(runloop)
        # No join: the GUI may own a suggestion lock needed by an in-flight callback.
