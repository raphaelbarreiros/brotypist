"""Application identity and bundled, original artwork.

Assets sit beside this module in wheels and PyInstaller's internal package tree.
No resource lookup depends on the working directory or user configuration.
"""
from __future__ import annotations

import ctypes
from pathlib import Path
import sys
from typing import TYPE_CHECKING

from . import __version__

if TYPE_CHECKING:
    from PySide6.QtGui import QGuiApplication, QIcon

APP_NAME = "Inklow"
APP_ID = "com.inklow.Inklow"


def asset_path(name: str) -> Path:
    """Return an existing package asset; never resolve outside the asset tree."""
    root = Path(__file__).resolve().parent / "assets"
    path = (root / name).resolve()
    if not path.is_relative_to(root) or not path.is_file():
        raise FileNotFoundError(f"Unknown Inklow asset: {name}")
    return path


def app_icon() -> QIcon:
    """Supply raster sizes explicitly for crisp docks, title bars and dialogs."""
    from PySide6.QtGui import QIcon

    icon = QIcon()
    for size in (16, 24, 32, 48, 64, 128, 256, 512, 1024):
        icon.addFile(str(asset_path(f"icons/app-{size}.png")))
    return icon


def tray_icon(paused: bool = False) -> QIcon:
    """Return a distinct active/paused glyph; macOS supplies template coloring."""
    from PySide6.QtGui import QIcon

    state = "paused" if paused else "active"
    # macOS recolors an alpha-mask image for light/dark menu bars. Other desktop
    # trays use the light glyph with its thin dark keyline for either background.
    variant = "template" if sys.platform == "darwin" else "contrast"
    icon = QIcon()
    for size in (18, 22, 24, 36, 44, 48):
        icon.addFile(str(asset_path(f"tray/{state}-{variant}-{size}.png")))
    if sys.platform == "darwin":
        icon.setIsMask(True)
    return icon


def configure_identity(app: QGuiApplication) -> None:
    """Set native application identity before constructing windows or the tray."""
    app.setApplicationName(APP_NAME)
    app.setApplicationDisplayName(APP_NAME)
    app.setApplicationVersion(__version__)
    app.setOrganizationName("Inklow")
    app.setOrganizationDomain("inklow.com")
    app.setDesktopFileName(APP_ID)
    if sys.platform == "win32":
        # Match the executable identity so the taskbar groups windows together.
        ctypes.windll.shell32.SetCurrentProcessExplicitAppUserModelID(APP_ID)
    app.setWindowIcon(app_icon())
