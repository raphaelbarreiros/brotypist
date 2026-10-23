"""Small shared contracts; no platform imports or text persistence."""
from __future__ import annotations

import threading
from typing import Callable
from urllib.parse import urlsplit

from ..shortcuts import DEFAULT_SHORTCUTS, normalize_shortcut

from ..core import Snapshot

Shortcut = Callable[[str], bool]


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
    def __init__(self) -> None:
        self._lock = threading.RLock()
        self._callback: Shortcut | None = None
        self._enabled = False
        self._stopped = threading.Event()
        self._diagnostic = "Native integration has not started."
        from ..config import Config
        self._excluded_apps = tuple(item.casefold() for item in Config().excluded_apps if item)
        self._app_rules: dict[str, dict] = {}
        self._shortcuts = dict(DEFAULT_SHORTCUTS)
        self._default_enabled = True
        self._global_enabled = True

    def configure(self, config) -> None:
        with self._lock:
            self.set_excluded_apps(config.excluded_apps)
            self._default_enabled = config.enabled
            self._global_enabled = config.global_enabled
            self._app_rules = {key: dict(value) for key, value in config.app_rules.items()}
            self._shortcuts = {action: normalize_shortcut(value) for action, value in
                               {**DEFAULT_SHORTCUTS, **config.shortcuts}.items()}

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
        bundles = {"com.apple.safari", "com.apple.safaritechnologypreview", "com.google.chrome",
                   "org.mozilla.firefox", "com.microsoft.edgemac", "com.brave.browser",
                   "com.operasoftware.opera", "com.vivaldi.vivaldi", "com.google.chrome.canary",
                   "org.chromium.chromium"}
        executables = {"safari", "safari technology preview", "chrome", "chromium", "chromium-browser",
                       "google-chrome", "google-chrome-stable", "firefox", "firefox-esr",
                       "msedge", "microsoft-edge", "microsoft-edge-stable", "brave", "brave-browser",
                       "opera", "vivaldi", "vivaldi-bin", "iexplore"}
        for identifier in identifiers:
            identity = identifier.casefold()
            name = identity.replace("\\", "/").rsplit("/", 1)[-1]
            if name.endswith(".exe"):
                name = name[:-4]
            if identity in bundles or name in executables:
                return True
        return False

    def _allowed(self, app_id: str, domain: str = "", *names: str) -> bool:
        if not self._global_enabled or self._excluded(app_id, *names):
            return False
        if (not domain and self._is_browser(app_id, *names)
                and any(key.startswith("domain:") and rule.get("enabled") is False
                        for key, rule in self._app_rules.items())):
            self._set_status(
                "Browser typing is paused: website exclusions require an observable native window URL. "
                "This browser does not expose one; no editable text was read."
            )
            return False
        enabled = self._default_enabled
        for key in (f"app:{app_id}", f"domain:{domain}" if domain else ""):
            enabled = self._app_rules.get(key, {}).get("enabled", enabled)
        return enabled

    def foreground(self) -> dict:
        return {"app_id": "", "name": "", "domain": "", "browser": False, "domain_observable": False}

    def _key_action(self, key: str, modifiers: tuple[str, ...] = (), *, platform: str = "") -> str | None:
        if not key:
            return None
        chord = normalize_shortcut("+".join((*modifiers, key)))
        # Alt+Tab is OS navigation on Windows/X11, even if misconfigured.
        if platform != "macos" and key == "Tab" and "Alt" in modifiers:
            return None
        if platform == "macos" and chord == "Alt+Tab":
            return "bypass"
        with self._lock:
            if key == "Tab":
                metadata = self.foreground()
                tab_enabled = True
                for rule in (f"app:{metadata['app_id']}", f"domain:{metadata['domain']}" if metadata["domain"] else ""):
                    tab_enabled = self._app_rules.get(rule, {}).get("tab_enabled", tab_enabled)
                if not tab_enabled:
                    return None
            for action, binding in self._shortcuts.items():
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

    def set_excluded_apps(self, excluded_apps: tuple[str, ...]) -> None:
        with self._lock:
            self._excluded_apps = tuple(item.casefold() for item in excluded_apps if item)

    def _excluded(self, *identifiers: str) -> bool:
        return any(exclusion in identifier.casefold()
                   for exclusion in self._excluded_apps for identifier in identifiers)

    def status(self) -> str:
        with self._lock:
            return self._diagnostic

    def _set_status(self, message: str) -> None:
        with self._lock:
            self._diagnostic = message

    def _dispatch(self, action: str) -> bool:
        # Never hold the adapter lock during a callback: the GUI can disable
        # shortcuts while a keyboard thread is waiting for its suggestion lock.
        with self._lock:
            callback = self._callback if self._enabled and not self._stopped.is_set() else None
        if callback is None:
            return False
        try:
            return bool(callback(action))
        except Exception:
            self.set_shortcuts_enabled(False)
            self._set_status("Shortcut callback failed; interception disabled. Pause and resume global typing.")
            return False

    def set_shortcuts_enabled(self, enabled: bool) -> None:
        with self._lock:
            self._enabled = bool(enabled) and not self._stopped.is_set()

    def start_shortcuts(self, callback: Shortcut) -> None:
        with self._lock:
            self._callback = callback

    def stop(self) -> None:
        self._stopped.set()
        self.set_shortcuts_enabled(False)
        with self._lock:
            self._callback = None


class UnavailableBridge(BridgeBase):
    """Explicitly unavailable integration, never a simulated native adapter."""
    def __init__(self, reason: str) -> None:
        super().__init__()
        self._diagnostic = reason

    def capture(self) -> Snapshot | None:
        return None

    def insert(self, snapshot: Snapshot, text: str) -> bool:
        return False
