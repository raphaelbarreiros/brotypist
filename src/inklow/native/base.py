"""Small shared contracts; no platform imports or text persistence."""
from __future__ import annotations

import itertools
import threading
import time
from collections.abc import Mapping
from types import MappingProxyType
from typing import Callable
from urllib.parse import urlsplit

from ..shortcuts import DEFAULT_SHORTCUTS, normalize_shortcut

from ..core import Snapshot

Shortcut = Callable[[str], bool]

# Exact macOS bundle identifiers (casefolded). Short executable names such as
# "arc", "dia", "zen" or "orion" are deliberately absent: they collide with
# unrelated programs, so those browsers are recognized by bundle identity only.
CHROMIUM_BROWSER_BUNDLES = frozenset({
    "com.google.chrome", "com.google.chrome.beta", "com.google.chrome.dev",
    "com.google.chrome.canary", "org.chromium.chromium", "com.microsoft.edgemac",
    "com.microsoft.edgemac.beta", "com.microsoft.edgemac.dev", "com.microsoft.edgemac.canary",
    "com.brave.browser", "com.brave.browser.beta", "com.brave.browser.nightly",
    "com.operasoftware.opera", "com.operasoftware.operagx", "com.vivaldi.vivaldi",
    "company.thebrowser.browser", "company.thebrowser.dia",
})
BROWSER_BUNDLES = CHROMIUM_BROWSER_BUNDLES | frozenset({
    "com.apple.safari", "com.apple.safaritechnologypreview", "org.mozilla.firefox",
    "org.mozilla.firefoxdeveloperedition", "org.mozilla.nightly", "app.zen-browser.zen",
    "com.kagi.kagimacos", "com.kagi.kagimacos.rc",
    "at.studio.asidebrowser",
})
_CODE_EDITORS = frozenset({
    "com.microsoft.vscode", "com.microsoft.vscodeinsiders", "com.vscodium",
    "com.todesktop.230313mzl4w4u92", "com.exafunction.windsurf", "com.apple.dt.xcode",
    "dev.zed.zed", "dev.zed.zed-preview", "com.sublimetext.3", "com.sublimetext.4",
    "com.panic.nova", "com.barebones.bbedit", "com.macromates.textmate", "org.vim.macvim",
    "com.google.android.studio",
})


def is_code_editor(app_id: str) -> bool:
    """Known code-editor bundle (VS Code family, Cursor, Windsurf, Xcode, JetBrains, ...)."""
    if not isinstance(app_id, str):
        return False
    identity = app_id.casefold()
    return identity in _CODE_EDITORS or identity.startswith("com.jetbrains.")


def utf16_to_index(text: str, offset: int) -> int:
    """Convert a UTF-16 offset, rejecting a position inside a surrogate pair."""
    if offset < 0:
        raise ValueError("Negative UTF-16 offset")
    units = 0
    for index, character in enumerate(text):
        if units == offset:
            return index
        units += 2 if ord(character) > 0xFFFF else 1
        if units > offset:
            raise ValueError("UTF-16 offset splits a surrogate pair")
    if units != offset:
        raise ValueError("UTF-16 offset exceeds text")
    return len(text)


def index_to_utf16(text: str, index: int) -> int:
    if not 0 <= index <= len(text):
        raise ValueError("Invalid codepoint index")
    return len(text[:index].encode("utf-16-le", errors="strict")) // 2


class BridgeBase:
    """Platform-neutral policy and key-callback state.

    ``_lock`` serializes platform accessibility work, which can block for a
    long time. Keyboard callbacks never take it: configuration objects are
    immutable and published by one reference assignment, and the enable state
    uses the tiny ``_state_lock`` that is never held during platform calls.
    """

    def __init__(self) -> None:
        self._lock = threading.RLock()
        self._state_lock = threading.Lock()
        self._callback: Shortcut | None = None
        self._input_callback: Callable[[], None] | None = None
        self._enabled = False
        self._stopped = threading.Event()
        self._diagnostic = "Native integration has not started."
        self._revisions = itertools.count(1)
        self._input_revision = 0
        self._user_input_revision = 0
        self._input_time = 0.0
        from ..config import Config
        self._excluded_apps = tuple(item.casefold() for item in Config().excluded_apps if item)
        self._app_rules: Mapping[str, Mapping] = MappingProxyType({})
        self._shortcuts: Mapping[str, str] = MappingProxyType(dict(DEFAULT_SHORTCUTS))
        self._default_enabled = True
        self._global_enabled = True

    def configure(self, config) -> None:
        # Build complete replacements first, then publish each by reference.
        rules = MappingProxyType({key: MappingProxyType(dict(value))
                                  for key, value in config.app_rules.items()})
        bindings = MappingProxyType({action: normalize_shortcut(value) for action, value in
                                     {**DEFAULT_SHORTCUTS, **config.shortcuts}.items()})
        with self._lock:
            self.set_excluded_apps(config.excluded_apps)
            self._default_enabled = config.enabled
            self._global_enabled = config.global_enabled
            self._app_rules = rules
            self._shortcuts = bindings

    @staticmethod
    def _domain(url) -> str:
        """Only a native URL property is accepted; titles are never URL sources."""
        if not isinstance(url, str):
            return ""
        try:
            parsed = urlsplit(url)
            if parsed.scheme.lower() not in ("http", "https") or not parsed.hostname:
                return ""
            return parsed.hostname.encode("idna").decode("ascii").lower().rstrip(".")
        except (ValueError, UnicodeError):
            return ""

    @staticmethod
    def _is_browser(*identifiers: str) -> bool:
        executables = {"safari", "safari technology preview", "chrome", "chromium", "chromium-browser",
                       "google-chrome", "google-chrome-stable", "firefox", "firefox-esr",
                       "msedge", "microsoft-edge", "microsoft-edge-stable", "brave", "brave-browser",
                       "opera", "vivaldi", "vivaldi-bin", "iexplore", "zen-browser"}
        for identifier in identifiers:
            identity = identifier.casefold()
            name = identity.replace("\\", "/").rsplit("/", 1)[-1]
            if name.endswith(".exe"):
                name = name[:-4]
            if identity in BROWSER_BUNDLES or name in executables:
                return True
        return False

    def _allowed(self, app_id: str, domain: str = "", *names: str, editor_probe: bool = False) -> bool:
        rules = self._app_rules
        if not self._global_enabled or self._excluded(app_id, *names):
            return False
        if (not domain and self._is_browser(app_id, *names)
                and any(key.startswith("domain:") and rule.get("enabled") is False
                        for key, rule in rules.items())):
            self._set_status(
                "Browser typing is paused: website exclusions require an observable native window URL. "
                "This browser does not expose one; no editable text was read."
            )
            return False
        if (is_code_editor(app_id) and not editor_probe
                and rules.get(f"app:{app_id}", {}).get("complete_in_code_editors") is not True):
            self._set_status(
                "Completions are off in code editors. Turn on “Complete in Code Editors” for this app "
                "in App Settings; no editable text was read."
            )
            return False
        enabled = self._default_enabled
        for key in (f"app:{app_id}", f"domain:{domain}" if domain else ""):
            enabled = rules.get(key, {}).get("enabled", enabled)
        return enabled

    def input_revision(self) -> int:
        """A timestamp-free change counter; never retain a key or written text."""
        return self._input_revision

    def user_input_revision(self) -> int:
        """Changes only for user input/focus, not host notifications of our own edits."""
        return self._user_input_revision

    def input_timestamp(self) -> float:
        """Last native input arrival time only; never retain its key or text."""
        return self._input_time

    def set_input_callback(self, callback: Callable[[], None] | None) -> None:
        """Install a nonblocking wake, called after each input revision."""
        self._input_callback = callback

    def _note_input(self, user: bool = True) -> None:
        """User input/focus change (``user``) or a host text/geometry notification.

        Safe from any native thread: ``itertools.count`` advances atomically
        and the callback contract is nonblocking.
        """
        revision = next(self._revisions)
        if user:
            self._input_time = time.monotonic()
            self._user_input_revision = revision
        self._input_revision = revision
        callback = self._input_callback
        if callback is not None and not self._stopped.is_set():
            try:
                callback()
            except Exception:
                pass

    def foreground(self) -> dict:
        return {"app_id": "", "name": "", "domain": "", "browser": False, "domain_observable": False}

    def _tab_metadata(self) -> dict:
        """Foreground identity for per-app Tab rules; adapters may serve a cache."""
        return self.foreground()

    def context_matches(self, snapshot: Snapshot) -> bool:
        return False

    def focused_context(self, snapshot: Snapshot, *, authorized: Callable[[], bool],
                        max_chars: int = 2400) -> dict:
        """No generic desktop scraping: platform providers must verify the window."""
        return {"text": "", "provenance": "accessibility", "window_id": snapshot.window_id,
                "revision": snapshot.window_revision,
                "status": "unavailable" if authorized() else "blocked"}

    def _key_action(self, key: str, modifiers: tuple[str, ...] = (), *, platform: str = "") -> str | None:
        """Lock-free: bindings and rules are immutable published snapshots."""
        if not key:
            return None
        chord = normalize_shortcut("+".join((*modifiers, key)))
        # Alt+Tab is OS navigation on Windows/X11, even if misconfigured.
        if platform != "macos" and key == "Tab" and "Alt" in modifiers:
            return None
        if platform == "macos" and chord == "Alt+Tab":
            return "bypass"
        bindings, rules = self._shortcuts, self._app_rules
        if key == "Tab":
            metadata = self._tab_metadata()
            tab_enabled = True
            for rule in (f"app:{metadata['app_id']}", f"domain:{metadata['domain']}" if metadata["domain"] else ""):
                tab_enabled = rules.get(rule, {}).get("tab_enabled", tab_enabled)
            if not tab_enabled:
                return None
        for action, binding in bindings.items():
            if binding and binding == chord:
                return action
        if chord == "Escape":
            return "dismiss"
        if not modifiers and key in "123456789":
            return f"choice:{key}"
        return None

    @staticmethod
    def _valid_replacement(snapshot: Snapshot, start: int, length: int, text: str) -> bool:
        return (isinstance(text, str) and bool(text) and "\x00" not in text
                and not snapshot.protected and isinstance(start, int) and isinstance(length, int)
                and 0 <= snapshot.caret <= len(snapshot.text)
                and 0 <= snapshot.selection_length <= len(snapshot.text) - snapshot.caret
                and 0 <= start <= len(snapshot.text) and 0 <= length <= len(snapshot.text) - start)

    def replace_selection(self, snapshot: Snapshot, text: str) -> bool:
        if snapshot.selection_length <= 0:
            return False
        return self.replace_range(snapshot, snapshot.caret, snapshot.selection_length, text)

    def replace_range(self, snapshot: Snapshot, start: int, length: int, text: str) -> bool:
        return False

    def range_rect(self, snapshot: Snapshot, start: int, length: int):
        """Verified same-line host bounds of a Python-index range, or None."""
        return None

    def secure_input_active(self) -> bool:
        return False

    def set_excluded_apps(self, excluded_apps: tuple[str, ...]) -> None:
        self._excluded_apps = tuple(item.casefold() for item in excluded_apps if item)

    def _excluded(self, *identifiers: str) -> bool:
        excluded = self._excluded_apps
        return any(exclusion in identifier.casefold()
                   for exclusion in excluded for identifier in identifiers)

    def status(self) -> str:
        return self._diagnostic

    def _set_status(self, message: str) -> None:
        # One reference assignment: callable from key callbacks without locks.
        self._diagnostic = message

    def _dispatch(self, action: str) -> bool:
        # Never take a lock here: platform capture can hold ``_lock`` for a
        # long accessibility round trip, which would stall the keyboard.
        callback = self._callback
        if callback is None or not self._enabled or self._stopped.is_set():
            return False
        try:
            return bool(callback(action))
        except Exception:
            self.set_shortcuts_enabled(False)
            self._set_status("Shortcut callback failed; interception disabled. Pause and resume global typing.")
            return False

    def set_shortcuts_enabled(self, enabled: bool) -> None:
        with self._state_lock:
            self._enabled = bool(enabled) and not self._stopped.is_set()

    def start_shortcuts(self, callback: Shortcut) -> None:
        self._callback = callback

    def stop(self) -> None:
        self._stopped.set()
        self.set_shortcuts_enabled(False)
        self._callback = None
        self._input_callback = None


class UnavailableBridge(BridgeBase):
    """Explicitly unavailable integration, never a simulated native adapter."""
    def __init__(self, reason: str) -> None:
        super().__init__()
        self._diagnostic = reason

    def capture(self) -> Snapshot | None:
        return None

    def insert(self, snapshot: Snapshot, text: str) -> bool:
        return False
