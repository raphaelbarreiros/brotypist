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

APP_NAME = "BroTypist"
# Keep the established OS/security identity; a display rebrand is not a data migration.
APP_ID = "com.inklow.Inklow"
REPOSITORY_URL = "https://github.com/renan-brasilio/brotypist"


def asset_path(name: str) -> Path:
    """Return an existing package asset; never resolve outside the asset tree."""
    root = (Path(__file__).resolve().parent / "assets").resolve()
    path = (root / name).resolve()
    if not path.is_relative_to(root) or not path.is_file():
        raise FileNotFoundError(f"Unknown {APP_NAME} asset: {name}")
    return path


def app_icon(*, compact: bool = False) -> QIcon:
    """Select artwork by logical size, not ambiguous physical raster size."""
    from PySide6.QtCore import QSize, Qt
    from PySide6.QtGui import QIcon, QIconEngine

    small = QIcon()
    for size in (16, 24, 32):
        small.addFile(str(asset_path(f"icons/app-{size}.png")))
        small.addFile(str(asset_path(f"icons/app-{size}@2x.png")))
    if compact:
        return small
    large = QIcon()
    for size in (48, 64, 128, 256, 512, 1024):
        large.addFile(str(asset_path(f"icons/app-{size}.png")))

    class LogicalIconEngine(QIconEngine):
        # 32pt@2x and 64pt@1x both request 64 physical pixels but need
        # different drawings. Keep them in separate raster engines.
        def clone(self):
            return LogicalIconEngine()

        def isNull(self):
            return small.isNull() and large.isNull()

        def availableSizes(self, mode=QIcon.Normal, state=QIcon.Off):
            return [QSize(size, size) for size in (16, 24, 32, 48, 64, 128, 256, 512, 1024)]

        def _variant(self, size):
            return small if max(size.width(), size.height()) <= 32 else large

        def pixmap(self, size, mode, state):
            return self._variant(size).pixmap(size, 1.0, mode, state)

        def scaledPixmap(self, size, mode, state, scale):
            # Qt >=6.8 passes device-independent size here.
            return self._variant(size).pixmap(size, scale, mode, state)

        def paint(self, painter, rect, mode, state):
            self._variant(rect.size()).paint(painter, rect, Qt.AlignCenter, mode, state)

    return QIcon(LogicalIconEngine())



def tray_icon(state: str = "active") -> QIcon:
    """Return a Ghost Caret state glyph, template-colored by macOS."""
    from PySide6.QtGui import QIcon

    if state not in {"active", "paused", "loading", "off"}:
        raise ValueError(f"Unknown tray state: {state!r}")
    # macOS recolors the alpha mask. Portable trays get a white glyph with a
    # graphite keyline, retaining contrast against both light and dark panels.
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
    app.setOrganizationName(APP_NAME)
    # Retain the domain and native IDs; config, keyring and permissions stay put.
    app.setOrganizationDomain("inklow.com")
    app.setDesktopFileName(APP_ID)
    if sys.platform == "win32":
        # Match the executable identity so the taskbar groups windows together.
        ctypes.windll.shell32.SetCurrentProcessExplicitAppUserModelID(APP_ID)
    app.setWindowIcon(app_icon())
