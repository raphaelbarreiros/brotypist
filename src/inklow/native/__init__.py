"""Opt-in, fail-closed native desktop integration.

Importing this package never loads an OS accessibility stack or captures keys.
"""
from __future__ import annotations

import sys

from .base import UnavailableBridge


def create_bridge():
    """Create a native adapter or retain initialization errors as unavailable.

    Linux prefers the compositor input-method bridge when its probe succeeds;
    otherwise the X11 adapter reports its own session/dependency limitations.
    Construction does not grant permission to intercept shortcuts.
    """
    try:
        if sys.platform == "darwin":
            from .macos import MacBridge
            return MacBridge()
        if sys.platform == "win32":
            from .windows import WindowsBridge
            return WindowsBridge()
        if sys.platform.startswith("linux"):
            from .wayland import WaylandBridge, available_input_method
            if available_input_method():
                return WaylandBridge()
            from .linux import LinuxBridge
            return LinuxBridge()
        return UnavailableBridge("System-wide typing is unsupported on this OS. Settings remain available.")
    except Exception as exc:
        return UnavailableBridge(
            f"Native integration could not initialize ({type(exc).__name__}: {exc}). "
            "Install the platform extras and restart BroTypist. Settings remain available."
        )


__all__ = ["create_bridge"]
