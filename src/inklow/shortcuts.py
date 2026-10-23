"""Canonical, toolkit-independent shortcut names and physical keyboard mappings."""
from __future__ import annotations

from collections.abc import Iterable
from functools import lru_cache
import sys

MODIFIERS = ("Ctrl", "Alt", "Shift", "Super")
DEFAULT_SHORTCUTS = {"word": "Tab", "all": "AboveTab", "force": "Ctrl+AboveTab",
                     "app_toggle": "Ctrl+Alt+Super+AboveTab", "toggle": "",
                     "alternatives": "", "synonyms": ""}
_ALIASES = {"control": "Ctrl", "ctrl": "Ctrl", "option": "Alt", "alt": "Alt",
            "shift": "Shift", "command": "Super", "cmd": "Super", "meta": "Super",
            "super": "Super", "win": "Super", "windows": "Super",
            "esc": "Escape", "escape": "Escape", "tab": "Tab", "abovetab": "AboveTab",
            "space": "Space", " ": "Space", "enter": "Return", "return": "Return"}
_KEYS = {"Tab", "AboveTab", "Escape", "Space", "Return"}
_KEYS.update(chr(code) for code in range(ord("A"), ord("Z") + 1))
_KEYS.update(str(number) for number in range(10))
_KEYS.update(f"F{number}" for number in range(1, 13))


def normalize_shortcut(value: str) -> str:
    """Validate a single chord; empty means disabled. Never ignore modifiers."""
    if not isinstance(value, str):
        raise ValueError("Shortcut must be a string")
    if not value.strip():
        return ""
    parts = [_ALIASES.get(part.strip().casefold(), part.strip().upper()) for part in value.split("+")]
    if any(not part for part in parts):
        raise ValueError("Shortcut contains an empty key")
    keys = [part for part in parts if part not in MODIFIERS]
    modifiers = [part for part in parts if part in MODIFIERS]
    if len(keys) != 1 or keys[0] not in _KEYS or len(set(modifiers)) != len(modifiers):
        raise ValueError("Use one key with optional Ctrl, Alt, Shift, Super modifiers")
    return "+".join([modifier for modifier in MODIFIERS if modifier in modifiers] + keys)


def shortcut_from_event(key: str, modifiers: Iterable[str] = (), *, platform: str = "",
                        keycode: int | None = None) -> str:
    """Recorder helper; keycode is native Mac keycode/Windows scan/X11 keycode."""
    physical = (mac_above_tab_keycode() if platform in {"macos", "darwin"} else
                {"windows": 41, "win32": 41, "linux": 49, "x11": 49}.get(platform))
    if keycode is not None and keycode == physical:
        key = "AboveTab"
    return normalize_shortcut("+".join([*modifiers, key]))


# ANSI letters are physical only on macOS (CGEvent keycodes); the recorder and
# event tap share the same mapping, independently of the character above Tab.
MAC_KEYS = {0: "A", 1: "S", 2: "D", 3: "F", 4: "H", 5: "G", 6: "Z", 7: "X",
            8: "C", 9: "V", 11: "B", 12: "Q", 13: "W", 14: "E", 15: "R", 16: "Y",
            17: "T", 31: "O", 32: "U", 34: "I", 35: "P", 37: "L", 38: "J", 40: "K",
            45: "N", 46: "M"}
MAC_KEYS.update({18: "1", 19: "2", 20: "3", 21: "4", 23: "5", 22: "6", 26: "7",
                 28: "8", 25: "9", 29: "0", 48: "Tab", 53: "Escape",
                 49: "Space", 36: "Return", 122: "F1", 120: "F2", 99: "F3", 118: "F4",
                 96: "F5", 97: "F6", 98: "F7", 100: "F8", 101: "F9", 109: "F10",
                 103: "F11", 111: "F12"})


@lru_cache(maxsize=1)
def _mac_keyboard_library():
    import ctypes
    library = ctypes.CDLL("/System/Library/Frameworks/Carbon.framework/Frameworks/HIToolbox.framework/HIToolbox")
    library.LMGetKbdType.restype = ctypes.c_uint8
    library.KBGetLayoutType.argtypes = [ctypes.c_int16]
    library.KBGetLayoutType.restype = ctypes.c_uint32
    return library


def mac_above_tab_keycode(layout_type: int | None = None) -> int | None:
    """ISO's § key is 10, not ANSI's backtick 50; never hijack both."""
    if layout_type is None:
        if sys.platform != "darwin":
            return None
        try:
            library = _mac_keyboard_library()
            layout_type = int(library.KBGetLayoutType(library.LMGetKbdType()))
        except (OSError, AttributeError):
            return None  # Unassign when geometry cannot be established.
    if layout_type == int.from_bytes(b"ISO ", "big"):
        return 10
    if layout_type == int.from_bytes(b"ANSI", "big"):
        return 50
    return None


def mac_key_name(code: int) -> str:
    return "AboveTab" if code == mac_above_tab_keycode() else MAC_KEYS.get(code, "")


def windows_key(vk: int, scan: int) -> str:
    if scan == 41:
        return "AboveTab"
    if 0x30 <= vk <= 0x39 or 0x41 <= vk <= 0x5A:
        return chr(vk)
    if 0x70 <= vk <= 0x7B:
        return f"F{vk - 0x6F}"
    return {9: "Tab", 27: "Escape", 32: "Space", 13: "Return"}.get(vk, "")
