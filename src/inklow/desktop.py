"""Platform-native menu utilities; the typing engine is UI-toolkit independent."""
from __future__ import annotations

import sys

from .config import Config
from .instance import instance_lock


def launch(config: Config, *, background: bool = False, no_tray: bool = False,
           pane: str | None = None) -> int:
    with instance_lock() as acquired:
        if not acquired:
            if sys.stderr is not None:
                print("Inklow is already running. Open Settings from its menu-bar or tray icon.", file=sys.stderr)
            return 0
        if sys.platform == "darwin":
            from .appkit_ui import launch as native_launch
        else:
            from .qt_ui import launch as native_launch
        return native_launch(config, background=background, no_tray=no_tray, pane=pane)
