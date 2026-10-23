"""Small shared contracts; no platform imports or text persistence."""
from __future__ import annotations

import threading
from typing import Callable

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
