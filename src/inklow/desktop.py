"""Select the native desktop shell while holding the process-wide instance lock.

Toolkit imports are deferred until launch; the typing engine remains independent
of the selected AppKit or Qt event loop.
"""
from __future__ import annotations

import sys

from .config import Config
from .instance import instance_lock


def launch(config: Config, *, background: bool = False, no_tray: bool = False,
           pane: str | None = None) -> int:
    """Run the platform shell and return its exit status.

    The instance lock spans the entire native event loop. A duplicate launch
    reports the resident instance and returns zero without opening another UI.
    Shell options are forwarded unchanged; the lock is released on exit.
    """
    with instance_lock() as acquired:
        if not acquired:
            if sys.stderr is not None:
                print("BroTypist is already running. Open Settings from its menu-bar or tray icon.", file=sys.stderr)
            return 0
        if sys.platform == "darwin":
            from .appkit_ui import launch as native_launch
        else:
            from .qt_ui import launch as native_launch
        return native_launch(config, background=background, no_tray=no_tray, pane=pane)
