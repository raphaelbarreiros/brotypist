"""Native-theme Windows/Linux menu agent and preferences, never a text editor.

The controller owns all typing, storage, downloads and background processes. Qt
objects are created and updated exclusively on the application thread.
"""
from __future__ import annotations

from collections.abc import Callable
from copy import deepcopy
from dataclasses import dataclass, field, replace
from pathlib import Path
import os
import sys
import threading
import time
from datetime import datetime, timedelta
from urllib.parse import urlsplit

from PySide6.QtCore import QEvent, QObject, QPoint, QPointF, QRectF, QSize, Qt, QThread, QTimer, Signal, Slot
from PySide6.QtGui import QAction, QColor, QCursor, QFont, QFontInfo, QFontMetricsF, QFontDatabase, QIcon, QKeySequence, QPainter, QPainterPath, QPalette, QPen, QPixmap, QShortcut
from PySide6.QtWidgets import (
    QAbstractItemView, QApplication, QCheckBox, QComboBox, QDialog,
    QDialogButtonBox, QFileDialog, QFormLayout, QFrame, QGroupBox,
    QHBoxLayout, QHeaderView, QLabel, QLineEdit, QListWidget, QMenu,
    QMessageBox, QPlainTextEdit, QProgressBar, QPushButton, QScrollArea,
    QSpinBox, QStackedWidget, QSystemTrayIcon, QTableWidget,
    QTableWidgetItem, QToolButton, QVBoxLayout, QWidget,
)

from . import __version__, system
from .config import Config
from .core import InlineRender
from .models import PRESETS
from .resources import REPOSITORY_URL, app_icon, configure_identity, tray_icon
from .runtime import Controller, ShutdownPending
from .shortcuts import normalize_shortcut, shortcut_from_event
from .updates import UpdateService
from .update_install import HEALTH_NONCE, HEALTH_PATH, acknowledge_startup

PANES = ("General", "AI Models", "Autocomplete", "Languages", "Privacy", "About")

_CLIPBOARD_TIMEOUT = 0.25


@dataclass
class _ClipboardRead:
    authorized: Callable[[], bool]
    deadline: float
    done: threading.Event = field(default_factory=threading.Event)
    cancelled: bool = False
    text: str = ""
    failed: bool = False


class ClipboardReader(QObject):
    """One queued GUI read; only the optional-context worker may wait."""

    _requested = Signal(object)

    def __init__(self, app: QApplication):
        super().__init__(app)
        self._lock = threading.Lock()
        self._pending = None
        self._requested.connect(self._read, Qt.ConnectionType.QueuedConnection)

    def read(self, *, authorized: Callable[[], bool]) -> str:
        if not authorized():
            return ""
        if QThread.currentThread() == self.thread():
            return system.clipboard_text(authorized=authorized)
        request = _ClipboardRead(authorized, time.monotonic() + _CLIPBOARD_TIMEOUT)
        with self._lock:
            if self._pending is not None:
                return ""
            self._pending = request
        self._requested.emit(request)
        request.done.wait(max(0, request.deadline - time.monotonic()))
        with self._lock:
            completed = request.done.is_set() and time.monotonic() < request.deadline
            text, failed = request.text, request.failed
            request.cancelled = True
            request.text = ""
        if not completed or not authorized():
            return ""
        if failed:
            raise RuntimeError("Clipboard context could not be read.")
        return text

    @Slot(object)
    def _read(self, request):
        def authorized():
            with self._lock:
                active = not request.cancelled and time.monotonic() < request.deadline
            return active and request.authorized()

        text, failed = "", False
        try:
            text = system.clipboard_text(authorized=authorized)
        except Exception:
            # Native exception bodies may contain clipboard entries.
            failed = True
        finally:
            with self._lock:
                if not request.cancelled and time.monotonic() < request.deadline:
                    request.text, request.failed = text, failed
                if self._pending is request:
                    self._pending = None
                request.done.set()


class UiWake(QObject):
    """Queue worker readiness on the GUI thread without a polling interval."""
    requested = Signal()

    def __init__(self, app, callback):
        super().__init__(app)
        self.callback = callback
        self.requested.connect(self.dispatch, Qt.ConnectionType.QueuedConnection)

    @Slot()
    def dispatch(self):
        self.callback()


def _settings_icon(index: int) -> QIcon:
    """Small native-vector symbols, never font-dependent emoji or letter tiles."""
    colors = ("#6E717A", "#1FA97A", "#3979C6", "#605BC2", "#30343D", "#6E717A")
    image = QPixmap(40, 40)
    image.setDevicePixelRatio(2)
    image.fill(Qt.GlobalColor.transparent)
    painter = QPainter(image)
    painter.setRenderHint(QPainter.RenderHint.Antialiasing)
    painter.setPen(Qt.PenStyle.NoPen)
    painter.setBrush(QColor(colors[index]))
    painter.drawRoundedRect(QRectF(0, 0, 20, 20), 5, 5)
    pen = QPen(Qt.GlobalColor.white, 1.35)
    pen.setCapStyle(Qt.PenCapStyle.RoundCap)
    pen.setJoinStyle(Qt.PenJoinStyle.RoundJoin)
    painter.setPen(pen)
    painter.setBrush(Qt.BrushStyle.NoBrush)
    if index == 0:
        painter.setPen(Qt.PenStyle.NoPen)
        painter.setBrush(Qt.GlobalColor.white)
        painter.translate(10, 10)
        for _ in range(8):
            painter.drawRoundedRect(QRectF(-1.8, -8, 3.6, 4), 0.6, 0.6)
            painter.rotate(45)
        painter.drawEllipse(QRectF(-5.5, -5.5, 11, 11))
        painter.setBrush(QColor(colors[index]))
        painter.drawEllipse(QRectF(-2.5, -2.5, 5, 5))
    elif index == 1:
        painter.drawRoundedRect(QRectF(5, 5, 10, 10), 1.5, 1.5)
        painter.drawRect(QRectF(8, 8, 4, 4))
        for position in (7, 10, 13):
            painter.drawLine(position, 3, position, 5)
            painter.drawLine(position, 15, position, 17)
            painter.drawLine(3, position, 5, position)
            painter.drawLine(15, position, 17, position)
    elif index == 2:
        painter.drawLine(11, 4, 11, 16)
        painter.drawLine(8, 4, 14, 4)
        painter.drawLine(8, 16, 14, 16)
        painter.drawLine(4, 8, 7, 8)
        painter.drawLine(4, 12, 7, 12)
    elif index == 3:
        painter.drawEllipse(QRectF(3, 3, 14, 14))
        painter.drawEllipse(QRectF(6, 3, 8, 14))
        painter.drawLine(3, 10, 17, 10)
    elif index == 4:
        path = QPainterPath()
        path.moveTo(10, 2.5)
        path.lineTo(16, 5)
        path.lineTo(15, 12)
        path.quadTo(14, 15, 10, 17.5)
        path.quadTo(6, 15, 5, 12)
        path.lineTo(4, 5)
        path.closeSubpath()
        painter.drawPath(path)
        painter.drawLine(7, 10, 9, 12)
        painter.drawLine(9, 12, 13, 7)
    else:
        painter.drawEllipse(QRectF(3, 3, 14, 14))
        painter.drawPoint(10, 6)
        painter.drawLine(10, 9, 10, 14)
    painter.end()
    return QIcon(image)



def _model_name(config: Config) -> str:
    return Path(config.model_path).name if config.backend == "gguf" and config.model_path else (
        config.model if config.backend != "gguf" else "No model selected")


def _tray_state(status: dict, config: Config) -> str:
    if not config.global_enabled:
        return "off"
    if status.get("state") == "paused":
        return "paused"
    if status.get("state") in {"starting", "loading"}:
        return "loading"
    if not status.get("inline_available", True):
        return "unavailable"
    return "active"


def _external_link(parent: QWidget, url: str) -> None:
    from PySide6.QtCore import QUrl
    from PySide6.QtGui import QDesktopServices
    if not QDesktopServices.openUrl(QUrl(url)):
        _error(parent, f"Could not open {url}")


def _label(text: str) -> QLabel:
    label = QLabel(text)
    label.setTextFormat(Qt.TextFormat.PlainText)
    label.setWordWrap(True)
    return label


def _error(parent: QWidget, error: Exception | str) -> None:
    QMessageBox.warning(parent, "BroTypist could not apply this change", str(error))


def _size(value: int) -> str:
    return f"{value / 1024 ** 3:.2f} GiB" if value >= 1024 ** 3 else f"{value / 1024 ** 2:.0f} MiB"


def merge_preferences(config: Config, changes: dict) -> Config:
    """Apply only explicitly edited preferences to the *current* config."""
    candidate = replace(config, **deepcopy(changes))
    candidate.validate()
    return candidate


def merge_mapping_edits(current: dict, baseline: dict, draft: dict) -> dict:
    """Three-way merge changed keys, including fields inside per-app rules."""
    result = deepcopy(current)
    missing = object()
    for key in baseline.keys() | draft.keys():
        before, after = baseline.get(key, missing), draft.get(key, missing)
        if before == after:
            continue
        if after is missing:
            result.pop(key, None)
        elif isinstance(before, dict) and isinstance(after, dict):
            result[key] = merge_mapping_edits(result.get(key, {}), before, after)
        else:
            result[key] = deepcopy(after)
    return result


class ShortcutRecorder(QPushButton):
    """One real key chord, including the physical key above Tab on any layout."""

    changed = Signal()
    recording_changed = Signal(bool)

    def __init__(self, parent=None):
        super().__init__(parent)
        self._value = ""
        self._recording = False
        self.setAccessibleName("Record keyboard shortcut")
        self.setToolTip("Click, then press the shortcut. Escape cancels recording; Backspace clears it.")
        self.clicked.connect(self._begin)

    def value(self) -> str:
        return self._value

    def set_value(self, value: str) -> None:
        normalized = normalize_shortcut(value)
        was_recording = self._recording
        self._value = normalized
        self._recording = False
        self.setText(self._value or "Not assigned")
        if was_recording:
            self.recording_changed.emit(False)

    def _begin(self) -> None:
        self._recording = True
        self.setText("Press a key combination…")
        self.setFocus(Qt.FocusReason.OtherFocusReason)
        self.recording_changed.emit(True)

    def event(self, event) -> bool:
        # Capture Tab as an actual chord rather than moving focus.
        if self._recording and event.type() == QEvent.Type.ShortcutOverride:
            event.accept()
            return True
        if self._recording and event.type() == QEvent.Type.KeyPress:
            self.keyPressEvent(event)
            return True
        return super().event(event)

    def keyPressEvent(self, event) -> None:
        if not self._recording:
            super().keyPressEvent(event)
            return
        key = event.key()
        if key == Qt.Key.Key_Escape:
            self.set_value(self._value)
            return
        if key in (Qt.Key.Key_Control, Qt.Key.Key_Alt, Qt.Key.Key_Shift, Qt.Key.Key_Meta):
            return
        if key in (Qt.Key.Key_Backspace, Qt.Key.Key_Delete) and not event.modifiers():
            self.set_value("")
            self.changed.emit()
            return
        names = {Qt.Key.Key_Tab: "Tab", Qt.Key.Key_Backtab: "Tab", Qt.Key.Key_Space: "Space",
                 Qt.Key.Key_Return: "Return", Qt.Key.Key_Enter: "Return"}
        key_name = names.get(key, QKeySequence(key).toString(QKeySequence.SequenceFormat.PortableText))
        modifiers = []
        for flag, name in ((Qt.KeyboardModifier.ControlModifier, "Ctrl"),
                           (Qt.KeyboardModifier.AltModifier, "Alt"),
                           (Qt.KeyboardModifier.ShiftModifier, "Shift"),
                           (Qt.KeyboardModifier.MetaModifier, "Super")):
            if event.modifiers() & flag:
                modifiers.append(name)
        if key == Qt.Key.Key_Backtab and "Shift" not in modifiers:
            modifiers.append("Shift")
        platform = "windows" if sys.platform == "win32" else "linux"
        try:
            chord = shortcut_from_event(key_name, modifiers, platform=platform,
                                        keycode=int(event.nativeScanCode()))
            if not chord:
                raise ValueError("This key is not supported. Choose Tab, the key above Tab, a letter, digit, or function key.")
            self.set_value(chord)
        except ValueError as exc:
            self.setToolTip(str(exc))
            self.setText("Unsupported key — try again")
            return
        self.changed.emit()

    def focusOutEvent(self, event) -> None:
        if self._recording:
            self.set_value(self._value)
        super().focusOutEvent(event)


class ModelManager(QDialog):
    """Inventory and explicit download controls; no model is loaded by this UI."""

    def __init__(self, controller: Controller, parent=None):
        super().__init__(parent)
        self.controller = controller
        self._models: list[dict] = []
        self.setWindowTitle("BroTypist models")
        self.resize(580, 340)
        layout = QVBoxLayout(self)
        layout.addWidget(_label("Download verified local model weights, or select a GGUF file. Your writing is not uploaded."))
        self.models = QComboBox()
        self.models.setAccessibleName("Available local models")
        layout.addWidget(self.models)
        self.description = _label("")
        self.current = _label("")
        self.status = _label("")
        layout.addWidget(self.description)
        layout.addWidget(self.current)
        self.progress = QProgressBar()
        self.progress.setAccessibleName("Model download progress")
        self.progress.setRange(0, 1000)
        layout.addWidget(self.progress)
        layout.addWidget(self.status)
        actions = QHBoxLayout()
        self.download = QPushButton("Download")
        self.use = QPushButton("Use installed model")
        self.cancel = QPushButton("Cancel download")
        actions.addWidget(self.download)
        actions.addWidget(self.use)
        actions.addWidget(self.cancel)
        layout.addLayout(actions)
        files = QHBoxLayout()
        self.custom = QPushButton("Choose GGUF…")
        reveal = QPushButton("Reveal models folder")
        files.addWidget(self.custom)
        files.addWidget(reveal)
        layout.addLayout(files)
        buttons = QDialogButtonBox(QDialogButtonBox.StandardButton.Close)
        buttons.rejected.connect(self.reject)
        layout.addWidget(buttons)
        self.models.currentIndexChanged.connect(self.refresh)
        self.download.clicked.connect(self._download)
        self.use.clicked.connect(self._use)
        self.cancel.clicked.connect(self.controller.cancel_download)
        self.custom.clicked.connect(self._custom)
        reveal.clicked.connect(lambda: self._call(system.reveal_models))
        self._refresh_timer = QTimer(self)
        self._refresh_timer.setInterval(1000)
        self._refresh_timer.timeout.connect(self.refresh_inventory)
        self.refresh_inventory()

    def showEvent(self, event) -> None:
        super().showEvent(event)
        self.refresh_inventory()
        self._refresh_timer.start()

    def hideEvent(self, event) -> None:
        self._refresh_timer.stop()
        super().hideEvent(event)

    def _call(self, function, *args, **kwargs) -> None:
        try:
            function(*args, **kwargs)
        except Exception as exc:
            _error(self, exc)

    def refresh_inventory(self) -> None:
        previous = self._models
        selected = self.models.currentData()
        try:
            self._models = self.controller.models()
            self._inventory_error = ""
        except Exception as exc:
            self._models = []
            self._inventory_error = f"Model inventory unavailable: {exc}. Check the model-folder permissions and reopen Models."
        if self._models == previous and self.models.count() == len(self._models):
            self.refresh()
            return
        self.models.blockSignals(True)
        self.models.clear()
        for model in self._models:
            self.models.addItem(f"{model['label']} — {_size(model['size_bytes'])}", model["id"])
        index = self.models.findData(selected)
        if index >= 0:
            self.models.setCurrentIndex(index)
        self.models.blockSignals(False)
        self.refresh()

    def refresh(self, *_args) -> None:
        index = self.models.currentIndex()
        model = self._models[index] if 0 <= index < len(self._models) else None
        download = self.controller.status().get("download") or {}
        active = bool(download.get("active", download.get("state") in {"downloading", "verifying", "cancelling"}))
        unavailable = str(model.get("availability") or "") if model else ""
        details = [model["description"]] if model else ["No downloadable models are available."]
        if model and model.get("license_name"):
            details.append(f"License: {model['license_name']}\n{model.get('license_url', '')}")
        if unavailable:
            details.append(unavailable)
        self.description.setText("\n\n".join(details))
        self.current.setText(f"Selected model: {self.controller.config.model_path or 'None'}")
        self.download.setEnabled(bool(model) and not active and not unavailable)
        self.use.setEnabled(bool(model and model["installed"]) and not active and not unavailable)
        self.models.setEnabled(not active)
        self.custom.setEnabled(not active)
        self.cancel.setEnabled(active)
        received = int(download.get("received", download.get("bytes", 0)))
        total = int(download.get("total", 0))
        self.progress.setRange(0, 1000 if total or not active else 0)
        if total:
            self.progress.setValue(min(1000, int(1000 * received / total)))
        message = self._inventory_error or download.get("error") or download.get("message")
        if not message:
            message = f"Downloaded {_size(received)} of {_size(total)}" if active else (
                "Installed; choose Use installed model to select it." if model and model["installed"] else "No download is running.")
        self.status.setText(str(message))

    def _download(self) -> None:
        index = self.models.currentIndex()
        if not self.download.isEnabled() or not 0 <= index < len(self._models):
            return
        model = self._models[index]
        requires_acceptance = bool(model.get("requires_license_acceptance"))
        consent = (f"\n\nRead and accept {model.get('license_name', 'the model license')} before downloading:\n"
                   f"{model.get('license_url', '')}\n\nChoosing Yes accepts these terms for this download."
                   if requires_acceptance else "")
        if QMessageBox.question(self, "Download local model?", f"Download {model['label']} ({_size(model['size_bytes'])})?\nOnly model weights are downloaded.{consent}",
                                QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
                                QMessageBox.StandardButton.No) == QMessageBox.StandardButton.Yes:
            self._call(self.controller.download_model, model["id"], accept_license=requires_acceptance)
            self.refresh()

    def _use(self) -> None:
        model = self._models[self.models.currentIndex()]
        self._call(self.controller.set_model, str(model["path"]), model["prompt_style"])
        self.refresh()

    def _custom(self) -> None:
        filename, _ = QFileDialog.getOpenFileName(self, "Choose local GGUF model", "", "GGUF models (*.gguf)")
        if not filename:
            return
        try:
            with Path(filename).open("rb") as stream:
                if stream.read(4) != b"GGUF":
                    raise ValueError("This file is not a GGUF model. Choose a valid .gguf file.")
            self.controller.set_model(filename, "continuation")
        except Exception as exc:
            _error(self, exc)
        self.refresh()


class RuleEditor(QDialog):
    def __init__(self, key: str = "", rule: dict | None = None, parent=None):
        super().__init__(parent)
        self.setWindowTitle("Application or website settings")
        self.resize(530, 400)
        layout = QVBoxLayout(self)
        form = QFormLayout()
        self.kind = QComboBox()
        self.kind.addItem("Application identifier", "app")
        self.kind.addItem("Website hostname", "domain")
        # The portable bridges do not currently expose trustworthy browser URL
        # metadata. Preserve/edit existing domain rules, but never suggest a new
        # website restriction can be enforced on Windows or Linux.
        if not key.startswith("domain:"):
            self.kind.model().item(1).setEnabled(False)
        self.kind.setToolTip("Website matching is available only through trusted macOS browser accessibility metadata.")
        self.identifier = QLineEdit()
        if key:
            kind, value = key.split(":", 1)
            self.kind.setCurrentIndex(self.kind.findData(kind))
            self.identifier.setText(value)
        form.addRow("Rule type", self.kind)
        form.addRow("Identifier", self.identifier)
        self.options = {}
        rule = rule or {}
        for name, title in (("enabled", "Completions"), ("tab_enabled", "Accept with Tab"), ("record_history", "Personalization history")):
            control = QComboBox()
            for text, value in (("Use default", None), ("Enabled", True), ("Disabled", False)):
                control.addItem(text, value)
            control.setCurrentIndex(control.findData(rule.get(name)))
            self.options[name] = control
            form.addRow(title, control)
        layout.addLayout(form)
        layout.addWidget(_label("Website URLs are unavailable on Windows and Linux. Disabled website rules block recognized browsers without an observable URL; website recording exclusions also suppress recording there. Other rules use exact app identifiers. History uses the global default unless overridden here. An Enabled app override records even when the global default is off and requires working OS-backed encrypted storage."))
        layout.addWidget(_label("Custom instructions for this application or website"))
        self.instructions = QPlainTextEdit()
        self.instructions.setAccessibleName("Application-specific instructions")
        self.instructions.setPlainText(rule.get("custom_instructions", ""))
        layout.addWidget(self.instructions)
        buttons = QDialogButtonBox(QDialogButtonBox.StandardButton.Save | QDialogButtonBox.StandardButton.Cancel)
        buttons.accepted.connect(self._accept)
        buttons.rejected.connect(self.reject)
        layout.addWidget(buttons)
        self.result_key = ""
        self.result_rule: dict = {}
        # Rule fields this dialog does not edit are kept unchanged.
        self._preserved = {name: value for name, value in rule.items()
                           if name not in self.options and name != "custom_instructions"}

    def _accept(self) -> None:
        value = self.identifier.text().strip()
        kind = self.kind.currentData()
        if not value:
            _error(self, "Enter an exact application identifier or website hostname.")
            return
        if kind == "domain":
            parsed = urlsplit(value if "://" in value else "//" + value)
            try:
                if parsed.username or parsed.password or parsed.path not in ("", "/") or parsed.query or parsed.fragment or parsed.port:
                    raise ValueError("Use only a hostname, such as example.com; do not include a path or port.")
                value = (parsed.hostname or "").rstrip(".").encode("idna").decode("ascii").lower()
                if not value or any(not part or part.startswith("-") or part.endswith("-") or not all(c.isalnum() or c == "-" for c in part) for part in value.split(".")):
                    raise ValueError("Enter a valid website hostname.")
            except (ValueError, UnicodeError) as exc:
                _error(self, exc)
                return
        instructions = self.instructions.toPlainText()
        if len(instructions) > 8000:
            _error(self, "Custom instructions must be no longer than 8,000 characters.")
            return
        self.result_key = f"{kind}:{value}"
        self.result_rule = dict(self._preserved)
        self.result_rule.update({name: control.currentData() for name, control in self.options.items() if control.currentData() is not None})
        if instructions:
            self.result_rule["custom_instructions"] = instructions
        self.accept()


class SettingsWindow(QDialog):
    """An explicit draft/apply lifecycle which never rewrites unrelated fields."""

    def __init__(self, shell: MenuAgent):
        super().__init__()
        self.shell = shell
        self.controller = shell.controller
        self.setWindowTitle("BroTypist Settings")
        self.resize(900, 620)
        self.setMinimumSize(900, 620)
        self._bindings: dict[str, tuple] = {}
        self._baseline: dict = {}
        self._rules: dict[str, dict] = {}
        self.shortcut_controls: dict[str, ShortcutRecorder] = {}
        self._loading = False
        self._closing = False
        self.model_manager: ModelManager | None = None
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 12)
        columns = QHBoxLayout()
        columns.setSpacing(0)
        sidebar_column = QVBoxLayout()
        sidebar_column.setContentsMargins(12, 16, 12, 12)
        self.sidebar = QListWidget()
        self.sidebar.setAccessibleName("Settings sections")
        self.sidebar.addItems(PANES)
        for index in range(len(PANES)):
            self.sidebar.item(index).setIcon(_settings_icon(index))
        self.sidebar.setSpacing(3)
        self.sidebar.setFrameShape(QFrame.Shape.NoFrame)
        self.sidebar.setStyleSheet(
            "QListWidget { background: transparent; }"
            "QListWidget::item { min-height: 30px; padding: 0 8px; border-radius: 5px; }"
            "QListWidget::item:selected { background: palette(midlight); color: palette(text);"
            " border-left: 2px solid palette(highlight); padding-left: 6px; }")
        sidebar_column.addWidget(self.sidebar, 1)
        self.sidebar_status = _label("")
        self.sidebar_status.setAccessibleName("Prediction and model status")
        sidebar_column.addWidget(self.sidebar_status)
        sidebar_widget = QWidget()
        sidebar_widget.setFixedWidth(210)
        sidebar_widget.setAutoFillBackground(True)
        sidebar_palette = sidebar_widget.palette()
        sidebar_palette.setColor(QPalette.ColorRole.Window, sidebar_palette.color(QPalette.ColorRole.AlternateBase))
        sidebar_widget.setPalette(sidebar_palette)
        sidebar_widget.setLayout(sidebar_column)
        columns.addWidget(sidebar_widget)
        self.pages = QStackedWidget()
        columns.addWidget(self.pages, 1)
        root.addLayout(columns, 1)
        self.notice = _label("")
        self.notice.setContentsMargins(12, 0, 12, 0)
        root.addWidget(self.notice)
        bottom = QHBoxLayout()
        bottom.setContentsMargins(12, 0, 12, 0)
        self.dirty_label = _label("Changes apply only after Save.")
        bottom.addWidget(self.dirty_label, 1)
        self.revert = QPushButton("Revert")
        self.save = QPushButton("Save")
        close = QPushButton("Close window")
        self.quit_button = QPushButton("Quit BroTypist")
        self.quit_button.setObjectName("quit")
        self.quit_button.setAutoDefault(False)
        self.quit_button.setToolTip("Stop typing assistance and exit, including when no system tray is available.")
        self.quit_button.clicked.connect(self.shell.quit)
        bottom.addWidget(self.quit_button)
        bottom.addWidget(self.revert)
        bottom.addWidget(self.save)
        bottom.addWidget(close)
        root.addLayout(bottom)
        for name in PANES:
            area = QScrollArea()
            area.setWidgetResizable(True)
            area.setFrameShape(QFrame.Shape.NoFrame)
            page = QWidget()
            layout = QVBoxLayout(page)
            layout.setContentsMargins(24, 22, 24, 20)
            heading = _label(name)
            font = heading.font()
            font.setPointSizeF(font.pointSizeF() + 3)
            font.setBold(True)
            heading.setFont(font)
            layout.addWidget(heading)
            getattr(self, "_" + name.lower().replace(" ", "_"))(layout)
            layout.addStretch()
            area.setWidget(page)
            self.pages.addWidget(area)
        self.sidebar.currentRowChanged.connect(self._pane_changed)
        self.revert.clicked.connect(self.reload)
        self.save.clicked.connect(self.apply)
        close.clicked.connect(self.close)
        quit_shortcut = QShortcut(QKeySequence(QKeySequence.StandardKey.Quit), self)
        quit_shortcut.activated.connect(self.shell.quit)
        self.reload()
        self.sidebar.setCurrentRow(0)
        for index in range(len(PANES)):
            shortcut = QShortcut(QKeySequence(f"Ctrl+{index + 1}"), self)
            shortcut.activated.connect(lambda index=index: self.sidebar.setCurrentRow(index))
        self.setStyleSheet(
            "QGroupBox { border: 1px solid palette(mid); border-radius: 10px; margin-top: 20px; padding: 14px; }"
            "QGroupBox::title { subcontrol-origin: margin; left: 14px; padding: 0 4px; }"
            "QCheckBox, QComboBox, QSpinBox, QLineEdit { min-height: 26px; }"
            "QPushButton { min-height: 26px; }")

    def _group(self, layout: QVBoxLayout, title: str) -> QFormLayout:
        group = QGroupBox(title)
        form = QFormLayout(group)
        form.setRowWrapPolicy(QFormLayout.RowWrapPolicy.WrapLongRows)
        form.setFieldGrowthPolicy(QFormLayout.FieldGrowthPolicy.ExpandingFieldsGrow)
        form.setVerticalSpacing(12)
        form.setContentsMargins(14, 20, 14, 14)
        layout.addWidget(group)
        return form

    def _bind(self, field: str, control, read, write, signal) -> None:
        control.setObjectName(field)
        control.setAccessibleName(field.replace("_", " "))
        self._bindings[field] = (control, read, write)
        signal.connect(self._changed)

    def _check(self, form: QFormLayout, field: str, title: str, help_text: str = "") -> QCheckBox:
        control = QCheckBox(title)
        if help_text:
            control.setToolTip(help_text)
        form.addRow(control)
        if help_text:
            form.addRow(_label(help_text))
        self._bind(field, control, control.isChecked, control.setChecked, control.toggled)
        return control

    def _combo(self, form: QFormLayout, field: str, title: str, choices: tuple) -> QComboBox:
        control = QComboBox()
        for label, value in choices:
            control.addItem(label, value)
        form.addRow(title, control)
        self._bind(field, control, control.currentData, lambda value: control.setCurrentIndex(control.findData(value)), control.currentIndexChanged)
        return control

    def _spin(self, form: QFormLayout, field: str, title: str, low: int, high: int, suffix: str = "") -> QSpinBox:
        control = QSpinBox()
        control.setRange(low, high)
        control.setSuffix(suffix)
        form.addRow(title, control)
        self._bind(field, control, control.value, control.setValue, control.valueChanged)
        return control

    def _text(self, form: QFormLayout, field: str, title: str) -> QLineEdit:
        control = QLineEdit()
        form.addRow(title, control)
        self._bind(field, control, control.text, control.setText, control.textChanged)
        return control

    def _setup(self, layout) -> None:
        form = self._group(layout, "System access")
        self.native_diagnostic = _label("")
        form.addRow(self.native_diagnostic)
        permissions = QPushButton("Open accessibility / input permissions")
        permissions.clicked.connect(lambda: self._call(system.open_permissions, "accessibility"))
        form.addRow(permissions)
        form.addRow(_label("Windows uses accessible edit controls and the native keyboard hook. Linux/X11 uses AT-SPI and replayable key grabs. KDE Wayland uses an explicitly selected native input method, not raw keyboard-device access. Only eligible, unprotected fields are used for predictions."))
        if sys.platform.startswith("linux"):
            form.addRow(_label("KDE Wayland: BroTypist must be explicitly selected as the input method. Only one input method can occupy that slot. Password/sensitive hints, selections, excluded apps and mismatched AT-SPI fields are rejected. Ordinary inline completion is unavailable: there is no proven selection-safe, host-rendered inactive-text path. No ordinary preedit or suggestion popup is sent. KDE can place the separate manual control near an eligible caret. Quit exits BroTypist without changing KDE's selection or restarting the compositor."))
            install_method = QPushButton("Install KDE input-method entry…")
            install_method.clicked.connect(self._install_wayland_method)
            form.addRow(install_method)
            open_method = QPushButton("Open KDE Virtual Keyboard settings")
            open_method.clicked.connect(lambda: self._call(system.open_wayland_settings))
            form.addRow(open_method)
        models = QPushButton("Choose or download a local model…")
        models.clicked.connect(self.open_models)
        form.addRow(models)
        self.setup_model = _label("")
        form.addRow(self.setup_model)
        self._check(form, "setup_completed", "Setup completed", "Check after reviewing system access and selecting a model. No permission is granted automatically.")
        form.addRow(_label("Acceptance shortcuts are shown under Autocomplete and in Welcome. Native/system shortcuts are never replaced when no eligible suggestion is active."))

    def _install_wayland_method(self) -> None:
        try:
            path = system.install_wayland_input_method()
        except (OSError, RuntimeError, ValueError) as exc:
            _error(self, exc)
            return
        QMessageBox.information(
            self, "KDE input-method entry installed",
            f"Installed {path}\n\nUse Quit BroTypist, then choose BroTypist in KDE System Settings → Keyboard → Virtual Keyboard. KDE will start the native input-method instance. The selected input method was not changed. Ordinary inline completion remains unavailable on this path.")

    def _general(self, layout) -> None:
        form = self._group(layout, "Application")
        self.login = QCheckBox("Launch BroTypist at login")
        self.login.setToolTip("Applied immediately, only when you explicitly change this switch.")
        self.login.toggled.connect(self._login_changed)
        form.addRow(self.login)
        self.login_status = _label("")
        form.addRow(self.login_status)
        form.addRow(_label("Close window keeps BroTypist running when a system tray is available. Without a tray, closing offers Quit. Quit BroTypist below always stops typing assistance and exits; it does not change your desktop's selected input method."))
        self._check(form, "global_enabled", "Enable predictions", "Turn off to keep BroTypist running without suggesting anything.")
        self._check(form, "enabled", "Enable in eligible apps by default", "App and website rules in Privacy can override this default.")
        self._check(form, "show_accessory", "Show accessory beside the text field", "Current-app predictions, temporary pause and Settings without leaving your text field.")
        self._statistics(layout)

    def _ai_models(self, layout) -> None:
        form = self._group(layout, "Current local model")
        self.model_label = _label("")
        self.model_label.setFont(QFontDatabase.systemFont(QFontDatabase.SystemFont.FixedFont))
        form.addRow("Selected model", self.model_label)
        models = QPushButton("Manage local models…")
        models.clicked.connect(self.open_models)
        form.addRow(models)
        reveal = QPushButton("Reveal model files")
        reveal.clicked.connect(lambda: self._call(system.reveal_models))
        form.addRow(reveal)
        self._combo(form, "prompt_style", "Model prompt style", (("Text continuation", "continuation"), ("Instruction-tuned writing", "writing")))
        self._inference(layout)
        self._battery(layout)

    def _autocomplete(self, layout) -> None:
        form = self._group(layout, "Suggestions")
        self._check(form, "complete_inside_text", "Complete inside existing text")
        self._combo(form, "completion_length", "Maximum completion length", (("Current word only", "word"), ("Short", "short"), ("Medium", "medium"), ("Long", "long")))
        self._spin(form, "debounce_ms", "Typing pause before completion", 100, 5000, " ms")
        self._spin(form, "min_chars", "Minimum typed characters", 1, 1000)
        self._shortcuts(layout)
        self._emoji(layout)
        self._labs(layout)

    def _languages(self, layout) -> None:
        layout.addWidget(_label("Language support comes from the selected local model. BroTypist does not expose a separate language detector or language-quality selector. Use instructions to request your preferred language; results depend on the model."))
        self.language_model = _label("")
        layout.addWidget(self.language_model)
        self._personalization(layout)

    def _privacy(self, layout) -> None:
        layout.addWidget(_label("Predictions run on this computer. Local GGUF inference does not upload writing. Optional local servers must use loopback URLs; their own logging and model residency are managed by that server. Downloads fetch model weights."))
        self._setup(layout)
        self._context(layout)
        self._history_controls(layout)
        self._app_settings(layout)

    def _context(self, layout) -> None:
        layout.addWidget(_label("Additional context is optional. It is read only for an eligible completion, never saved as an image or uploaded. Leave these switches off when working with sensitive content."))
        form = self._group(layout, "Optional local context")
        clipboard = self._check(form, "clipboard_context", "Include clipboard text")
        screen = self._check(form, "screen_context", "Include focused-window Accessibility text",
                             "Optional visible text from the active native window only. Protected and hidden controls are excluded; unsupported providers return no context.")
        clipboard.toggled.connect(lambda value: self._context_consent("clipboard_context", value))
        screen.toggled.connect(lambda value: self._context_consent("screen_context", value))
        # Accessibility access is shared with native typing; no screen-recording
        # permission or desktop-wide screenshot is needed for context.
        self._spin(form, "context_chars", "Maximum typed context", 64, 8000, " characters")
        self.window_context_status = _label("Context not requested")
        form.addRow("Availability", self.window_context_status)

    def _personalization(self, layout) -> None:
        form = self._group(layout, "Custom instructions")
        form.addRow(_label("Describe your preferred tone, language or vocabulary. These are instructions for local inference, not a place to draft documents."))
        self.instructions = QPlainTextEdit()
        self.instructions.setMaximumHeight(150)
        self.instructions.setPlaceholderText("For example: concise sentences; British English.")
        form.addRow(self.instructions)
        self._bind("custom_instructions", self.instructions, self.instructions.toPlainText, self.instructions.setPlainText, self.instructions.textChanged)

    def _history_controls(self, layout) -> None:
        form = self._group(layout, "Encrypted local writing learning")
        self.history = QCheckBox("Learn locally from recorded writing by default")
        self.history.setToolTip("Requires explicit consent and OS-backed encrypted storage. No uploads or model-weight training.")
        self.history.toggled.connect(self._history_changed)
        form.addRow(self.history)
        form.addRow(_label("History is off by default and requires OS-backed encrypted storage; no plaintext fallback. App recording overrides take precedence: explicitly enabled apps continue recording when this default is off."))
        self._check(form, "record_unaccepted", "Also learn when no suggestion is accepted", "Only while history is enabled for that app/site. Leave off to record only sessions with an accepted suggestion.")
        self._spin(form, "personalization_strength", "Style and vocabulary strength", 0, 100, "%")
        form.addRow(_label("New observed edits supply bounded style and vocabulary evidence; existing drafts, bulk pastes, older records and BroTypist-generated suggestions do not establish authorship. Current language and explicit instructions take precedence."))
        form.addRow(_label("Learning matches the same app or exact website in confidently detected English, Portuguese, Spanish, French or German. Unknown or ambiguous language means no learned context. Deletion forgets the derived profile and cancels suggestions using it."))
        self.history_status = _label("")
        form.addRow(self.history_status)
        delete = QPushButton("Forget all learned writing…")
        delete.clicked.connect(self._delete_history)
        form.addRow(delete)

    def _emoji(self, layout) -> None:
        form = self._group(layout, "Emoji suggestions")
        self._check(form, "emoji_enabled", "Find emoji by typing :name")
        self._check(form, "emoticons_enabled", "Replace emoticons such as :-)")
        self._combo(form, "emoji_skin_tone", "Preferred skin tone", tuple((name, index) for index, name in enumerate(("Default", "Light", "Medium-light", "Medium", "Medium-dark", "Dark"))))
        self._check(form, "emoji_include_neutral", "Include neutral-tone alternatives")
        self._combo(form, "emoji_gender", "Preferred gender", (("Neutral", "neutral"), ("Female", "female"), ("Male", "male")))
        form.addRow(_label("Choose a numbered suggestion with its number key while the chooser is visible. Escape dismisses it according to your shortcut settings."))
        if sys.platform.startswith("linux"):
            form.parentWidget().setEnabled(False)
            layout.addWidget(_label("Emoji and emoticon conversion are unavailable on Linux: AT-SPI does not provide atomic typed-suffix replacement. Ordinary text completions and alternative continuations remain available."))
        elif sys.platform == "win32":
            layout.addWidget(_label("Typed-suffix conversion requires a native Windows Edit control. Other UI Automation controls are left unchanged when exact atomic replacement is unavailable."))

    def _shortcuts(self, layout) -> None:
        form = self._group(layout, "Keyboard shortcuts")
        for action, title in (("word", "Accept next word"), ("all", "Accept full completion"), ("force", "Request completion now"), ("app_toggle", "Toggle current app for this session"), ("toggle", "Toggle globally"), ("alternatives", "Alternative words"), ("synonyms", "Selected-text synonyms")):
            recorder = ShortcutRecorder()
            recorder.setAccessibleName(title + " shortcut")
            if action == "synonyms" and sys.platform.startswith("linux"):
                recorder.setEnabled(False)
                recorder.setToolTip("Selected-text rewriting is unavailable on Linux because AT-SPI has no atomic replacement operation.")
            recorder.changed.connect(self._changed)
            recorder.recording_changed.connect(lambda _recording: self.shell._sync_shortcuts())
            self.shortcut_controls[action] = recorder
            form.addRow(title, recorder)
        form.addRow(_label("Click a shortcut, then press its real key combination. The physical key above Tab works across keyboard layouts. Escape cancels recording; Backspace clears an assignment. Operating-system and desktop-reserved shortcuts remain system-owned and may not be available to BroTypist."))
        form = self._group(layout, "Acceptance and Escape")
        self._combo(form, "escape_behavior", "Escape action", (("Pause briefly", "pause"), ("Disable for current application", "app"), ("Pass through to application", "passthrough")))
        self._spin(form, "escape_pause_seconds", "Brief pause duration", 1, 3600, " seconds")
        self._check(form, "trailing_space", "Include trailing space when accepting")
        self._combo(form, "punctuation_mode", "Include trailing punctuation", (
            ("Only with full completion", "full"), ("With either shortcut", "always"), ("Never", "never")))

    def _battery(self, layout) -> None:
        layout.addWidget(_label("These limits apply while the system reports battery power. They do not change your normal model or completion preferences."))
        form = self._group(layout, "While on battery")
        self._check(form, "battery_on_demand", "Generate only when requested")
        self._combo(form, "battery_model", "Model on battery", (("Same as usual", "same"),) + tuple((preset.label, preset.id) for preset in PRESETS))
        form.addRow(_label("A different model must already be installed; battery mode never downloads one automatically."))
        self._check(form, "battery_shorter", "Prefer shorter completions")
        self.battery_status = _label("")
        form.addRow(self.battery_status)
        form = self._group(layout, "Memory usage")
        timeout = self._spin(form, "idle_release_seconds", "Release model after inactivity", 0, 3600, " seconds")
        timeout.setSpecialValueText("Keep model ready")
        form.addRow(_label("The inference process starts only when needed. Pausing or disabling BroTypist releases it immediately. Set a nonzero timeout to reclaim all model memory when idle."))

    def _app_settings(self, layout) -> None:
        self.current_app = _label("")
        layout.addWidget(self.current_app)
        layout.addWidget(_label("Use exact application IDs. These native bridges cannot observe browser URLs. Stored website exclusions fail closed: a disabled website rule blocks all recognized browsers with unknown URLs, and a website recording exclusion prevents recording in those browsers. Per-website matching is unavailable. Changes remain a draft until Save."))
        self.rule_table = QTableWidget(0, 4)
        self.rule_table.setHorizontalHeaderLabels(("Application / website", "Completions", "Tab", "History"))
        self.rule_table.setSelectionBehavior(QAbstractItemView.SelectionBehavior.SelectRows)
        self.rule_table.setSelectionMode(QAbstractItemView.SelectionMode.SingleSelection)
        self.rule_table.setEditTriggers(QAbstractItemView.EditTrigger.NoEditTriggers)
        self.rule_table.horizontalHeader().setSectionResizeMode(0, QHeaderView.ResizeMode.Stretch)
        self.rule_table.setMinimumHeight(180)
        self.rule_table.doubleClicked.connect(lambda *_: self._edit_rule())
        layout.addWidget(self.rule_table)
        row = QHBoxLayout()
        for title, action in (("Add…", self._add_rule), ("Edit…", self._edit_rule), ("Remove", self._remove_rule), ("Delete app history…", self._delete_app_history)):
            button = QPushButton(title)
            button.clicked.connect(action)
            row.addWidget(button)
        layout.addLayout(row)
        form = self._group(layout, "Always excluded applications")
        excluded = QPlainTextEdit()
        excluded.setMaximumHeight(95)
        excluded.setToolTip("One executable or application identifier per line. Password managers and terminals are excluded by default.")
        form.addRow(excluded)
        self._bind("excluded_apps", excluded, lambda: tuple(line.strip() for line in excluded.toPlainText().splitlines() if line.strip()), lambda values: excluded.setPlainText("\n".join(values)), excluded.textChanged)

    def _labs(self, layout) -> None:
        layout.addWidget(_label("Alternative continuations are inserted at the caret. Synonyms replace only the exact, revalidated selected text. These tools never rewrite an entire document."))
        form = self._group(layout, "Alternative words")
        self._check(form, "alternatives_enabled", "Offer alternative words")
        self._combo(form, "alternatives_mode", "Show alternatives", (("With shortcut", "shortcut"), ("After a pause", "pause")))
        self._spin(form, "alternatives_delay_ms", "Pause delay", 100, 10000, " ms")
        form = self._group(layout, "Contextual synonyms")
        self._check(form, "synonyms_enabled", "Offer synonyms for selected text")
        self._combo(form, "synonyms_mode", "Show synonyms", (("When text is selected", "selection"), ("With shortcut", "shortcut")))
        self._spin(form, "synonyms_delay_ms", "Selection delay", 100, 10000, " ms")
        if sys.platform.startswith("linux"):
            form.parentWidget().setEnabled(False)
            layout.addWidget(_label("Selected-text synonyms are unavailable on Linux because AT-SPI has no atomic selection-replacement API. Alternative continuations remain available."))
        elif sys.platform == "win32":
            layout.addWidget(_label("Selected-text synonyms require a native Edit control or a UI Automation control with an exact, replaceable selection. Unsupported controls remain unchanged."))

    def _inference(self, layout) -> None:
        form = self._group(layout, "Local inference configuration")
        self._combo(form, "backend", "Backend", (("Local GGUF process", "gguf"), ("Local Ollama server", "ollama"), ("Local OpenAI-compatible server", "openai")))
        self._text(form, "endpoint", "Loopback server URL")
        self._text(form, "model", "Local server model name")
        self._combo(form, "acceleration", "Acceleration", (("Automatic", "auto"), ("CPU only", "cpu")))
        self._spin(form, "max_tokens", "Maximum generated tokens", 1, 128)
        form.addRow(_label("Server backends accept literal loopback URLs only. BroTypist does not upload writing to a remote API. Model residency for externally managed servers is controlled by that server."))

    def _statistics(self, layout) -> None:
        form = self._group(layout, "Local aggregate statistics")
        self._check(form, "statistics_enabled", "Count successfully accepted completions", "Aggregate counts only; no draft text. Turning this off stops new counting.")
        self.period = QComboBox()
        for title, value in (("Today", "day"), ("Past week", "week"), ("Past month", "month"), ("All time", "all")):
            self.period.addItem(title, value)
        self.period.setCurrentIndex(1)
        self.group_by = QComboBox()
        self.group_by.addItem("By day", "day")
        self.group_by.addItem("By application", "app")
        form.addRow("Period", self.period)
        form.addRow("Group", self.group_by)
        self.stats_totals = _label("")
        form.addRow(self.stats_totals)
        self.stats_table = QTableWidget(0, 4)
        self.stats_table.setHorizontalHeaderLabels(("Day / application", "Completions", "Words", "Characters"))
        self.stats_table.horizontalHeader().setSectionResizeMode(0, QHeaderView.ResizeMode.Stretch)
        self.stats_table.setEditTriggers(QAbstractItemView.EditTrigger.NoEditTriggers)
        self.stats_table.setMinimumHeight(200)
        layout.addWidget(self.stats_table)
        self.period.currentIndexChanged.connect(self.refresh_stats)
        self.group_by.currentIndexChanged.connect(self.refresh_stats)

    def _about(self, layout) -> None:
        icon = QLabel()
        icon.setPixmap(app_icon().pixmap(88, 88))
        icon.setAlignment(Qt.AlignmentFlag.AlignCenter)
        icon.setAccessibleName("BroTypist app icon")
        layout.addWidget(icon)
        wordmark = QLabel('<span style="font-weight:700">Bro</span><span style="color:#A7A9AF;font-weight:600">Typist</span>')
        wordmark.setAlignment(Qt.AlignmentFlag.AlignCenter)
        font = wordmark.font()
        font.setPointSize(26)
        wordmark.setFont(font)
        layout.addWidget(wordmark)
        version = _label(f"Version {__version__} · {'Windows' if sys.platform == 'win32' else 'Linux'}")
        version.setAlignment(Qt.AlignmentFlag.AlignCenter)
        layout.addWidget(version)
        form = self._group(layout, "BroTypist")
        form.addRow(_label("An independent local typing assistant. No account, cloud sync, subscription or main writing editor. Models retain their respective licenses."))
        form.addRow(_label("Some applications do not expose editable text or trustworthy caret geometry. BroTypist stays inactive rather than reading or inserting unsafely."))
        source = QPushButton("Source repository…")
        source.clicked.connect(lambda: _external_link(self, REPOSITORY_URL))
        form.addRow(source)
        welcome = QPushButton("Show Welcome…")
        welcome.clicked.connect(self.shell.show_welcome)
        form.addRow(welcome)
        settings = QPushButton("Open settings folder")
        settings.clicked.connect(self._reveal_settings)
        form.addRow(settings)
        form.addRow(_label("Use Quit BroTypist at the bottom of this window to stop the app, or Close window to return to the tray."))
        updates = self._group(layout, "Updates")
        self._check(updates, "check_updates_automatically", "Check for updates automatically",
                    "Checks once a day when the resident app starts. Downloads and installation always require your approval.")
        self.update_status = _label("")
        self.update_status.setAccessibleName("Software update status")
        updates.addRow(self.update_status)
        self.update_installation = _label("")
        updates.addRow(self.update_installation)
        self.update_progress = QProgressBar()
        self.update_progress.setAccessibleName("Software update download progress")
        updates.addRow(self.update_progress)
        buttons = QHBoxLayout()
        self.update_check = QPushButton("Check for Updates…")
        self.update_check.clicked.connect(self.shell.check_updates)
        self.update_download = QPushButton("Download Update")
        self.update_download.clicked.connect(lambda: self.shell._update_action(self.shell.updates.start_download))
        self.update_cancel = QPushButton("Cancel")
        self.update_cancel.clicked.connect(lambda: self.shell._update_action(self.shell.updates.cancel))
        self.update_install = QPushButton("Install & Relaunch")
        self.update_install.clicked.connect(self.shell.install_update)
        for name, button in (("update_check", self.update_check), ("update_download", self.update_download),
                             ("update_cancel", self.update_cancel), ("update_install", self.update_install)):
            button.setObjectName(name)
            button.setAutoDefault(False)
            buttons.addWidget(button)
        updates.addRow(buttons)
        self.refresh_updates()

    def refresh_updates(self, snapshot: dict | None = None) -> None:
        status = self.shell.updates.snapshot() if snapshot is None else snapshot
        state = status.get("state", "idle")
        kind = status.get("install_kind", "source")
        version = status.get("version")
        current = status.get("current_version") or __version__
        message = str(status.get("message") or "Check for a newer signed release.")
        self.update_status.setText(f"Installed version: {current}" + (f" · Update: {version}" if version else "") + f"\n{message}")
        if kind == "linux_debian":
            explanation = ("Managed Debian installation. The verified .deb opens in your system's package installer, "
                           "which may request administrator approval. BroTypist does not replace /opt itself. "
                           "After installation finishes, quit and reopen BroTypist to use the new version.")
        else:
            explanation = str(status.get("install_reason") or "")
            if kind == "macos_bundle":
                explanation += ("\nMac releases use free per-app approval, not paid Developer ID notarization. "
                                "macOS may require approval of the replacement app and review of system access.")
        self.update_installation.setText(explanation)
        shutting_down = self.shell._shutdown_pending or self.shell._stopping or self.shell._stopped
        busy = state in {"checking", "downloading", "installing"}
        self.update_check.setEnabled(not busy and not shutting_down)
        can_install = bool(status.get("installable")) or kind == "linux_debian"
        self.update_download.setEnabled(state == "available" and can_install and not shutting_down)
        self.update_cancel.setVisible(state in {"checking", "downloading"})
        self.update_cancel.setEnabled(not shutting_down)
        self.update_install.setText("Open System Installer" if kind == "linux_debian" else "Install & Relaunch")
        self.update_install.setVisible(can_install)
        self.update_install.setEnabled(state == "ready" and can_install and not shutting_down)
        self.update_progress.setVisible(state == "downloading")
        received, total = int(status.get("received") or 0), int(status.get("total") or 0)
        if total > 0:
            self.update_progress.setRange(0, 1000)
            self.update_progress.setValue(min(1000, received * 1000 // total))
            self.update_progress.setFormat(f"{_size(received)} / {_size(total)} · %p%")
        else:
            self.update_progress.setRange(0, 0)

    def _reveal_settings(self) -> None:
        from PySide6.QtCore import QUrl
        from PySide6.QtGui import QDesktopServices
        from .config import config_path
        directory = config_path().parent
        directory.mkdir(parents=True, exist_ok=True, mode=0o700)
        if not QDesktopServices.openUrl(QUrl.fromLocalFile(str(directory))):
            _error(self, f"Could not open the settings folder: {directory}")

    def _call(self, function, *args) -> None:
        try:
            function(*args)
        except Exception as exc:
            _error(self, exc)

    def _pane_changed(self, index: int) -> None:
        self.pages.setCurrentIndex(index)
        if 0 <= index < len(PANES) and PANES[index] == "General":
            self.refresh_stats()

    def show_pane(self, pane: str = "General") -> None:
        for index, name in enumerate(PANES):
            if name.casefold() == pane.casefold():
                self.sidebar.setCurrentRow(index)
                break
        self.show()
        self.raise_()
        self.activateWindow()

    def _changes(self) -> dict:
        changes = {field: read() for field, (_control, read, _write) in self._bindings.items() if read() != self._baseline.get(field)}
        shortcuts = {name: recorder.value() for name, recorder in self.shortcut_controls.items()}
        if shortcuts != self._baseline.get("shortcuts"):
            changes["shortcuts"] = merge_mapping_edits(self.controller.config.shortcuts, self._baseline["shortcuts"], shortcuts)
        if self._rules != self._baseline.get("app_rules"):
            changes["app_rules"] = merge_mapping_edits(self.controller.config.app_rules, self._baseline["app_rules"], self._rules)
        return changes

    def _changed(self, *_args) -> None:
        if self._loading:
            return
        count = len(self._changes())
        self.save.setEnabled(bool(count))
        self.revert.setEnabled(bool(count))
        self.dirty_label.setText("Unsaved changes" if count else "All changes saved")

    def reload(self) -> None:
        self._loading = True
        config = self.controller.config
        for field, (_control, _read, write) in self._bindings.items():
            value = deepcopy(getattr(config, field))
            self._baseline[field] = value
            write(value)
        self._baseline["shortcuts"] = deepcopy(config.shortcuts)
        for name, recorder in self.shortcut_controls.items():
            recorder.set_value(config.shortcuts.get(name, ""))
        self._rules = deepcopy(config.app_rules)
        self._baseline["app_rules"] = deepcopy(self._rules)
        self.login.setChecked(config.launch_at_login)
        self.history.setChecked(config.record_history)
        self._refresh_rules()
        self._loading = False
        self._changed()
        self.refresh_status()

    def apply(self) -> bool:
        try:
            changes = self._changes()
            if changes:
                self.controller.update_config(merge_preferences(self.controller.config, changes))
        except Exception as exc:
            self.notice.setText(f"Not saved: {exc} Correct the setting and choose Save again.")
            _error(self, exc)
            return False
        self.reload()
        return True

    def _login_changed(self, enabled: bool) -> None:
        if self._loading:
            return
        old = self.controller.config.launch_at_login
        try:
            system.set_launch_at_login(enabled)
            try:
                self.controller.update_config(merge_preferences(self.controller.config, {"launch_at_login": enabled}))
            except Exception:
                system.set_launch_at_login(old)
                raise
        except Exception as exc:
            self.login.blockSignals(True)
            self.login.setChecked(old)
            self.login.blockSignals(False)
            _error(self, exc)
        self.refresh_status()

    def _context_consent(self, field: str, enabled: bool) -> None:
        if self._loading or not enabled:
            return
        noun = "clipboard text" if field == "clipboard_context" else "text visible in the foreground application"
        if QMessageBox.question(self, "Allow optional local context?", f"Allow BroTypist to read {noun} for eligible completion requests?\nThis may contain sensitive information. The preference takes effect only after Save.") != QMessageBox.StandardButton.Yes:
            self._bindings[field][0].setChecked(False)

    def _history_changed(self, enabled: bool) -> None:
        if self._loading:
            return
        old = self.controller.config.record_history
        consent = not enabled or QMessageBox.question(self, "Enable encrypted local learning?", "Store eligible writing locally in encrypted form? New observed edits can teach style and vocabulary; existing drafts, bulk pastes and BroTypist-generated suggestions do not establish authorship.\nNothing is uploaded or used to retrain weights. OS secure storage is required; there is no plaintext fallback. Turning recording off does not delete existing history.") == QMessageBox.StandardButton.Yes
        if consent:
            try:
                self.controller.set_history_enabled(enabled)
            except Exception as exc:
                _error(self, exc)
        actual = self.controller.config.record_history if consent else old
        self.history.blockSignals(True)
        self.history.setChecked(actual)
        self.history.blockSignals(False)
        self.refresh_status()

    def _delete_history(self, *_args, app_id=None) -> None:
        target = f" for {app_id}" if app_id else ""
        if QMessageBox.question(self, "Delete encrypted history?", f"Permanently delete personalization history{target}?\nThis cannot be undone; aggregate statistics are separate.") == QMessageBox.StandardButton.Yes:
            self._call(self.controller.delete_history, app_id)
            self.refresh_status()

    def _selected_rule(self) -> str | None:
        row = self.rule_table.currentRow()
        item = self.rule_table.item(row, 0)
        return item.text() if item else None

    def _add_rule(self) -> None:
        self._rule_dialog()

    def _edit_rule(self) -> None:
        key = self._selected_rule()
        if key:
            self._rule_dialog(key)

    def _rule_dialog(self, key: str = "") -> None:
        dialog = RuleEditor(key, self._rules.get(key), self)
        if dialog.exec() != QDialog.DialogCode.Accepted:
            return
        new_key = dialog.result_key
        if new_key != key and new_key in self._rules:
            _error(self, "A rule already exists for that identifier. Edit the existing rule instead.")
            return
        if key:
            del self._rules[key]
        self._rules[new_key] = dialog.result_rule
        self._refresh_rules()
        self._changed()

    def _remove_rule(self) -> None:
        key = self._selected_rule()
        if key:
            del self._rules[key]
            self._refresh_rules()
            self._changed()

    def _delete_app_history(self) -> None:
        key = self._selected_rule()
        if not key or not key.startswith("app:"):
            _error(self, "Select an application rule. History deletion is keyed by application ID, not hostname.")
            return
        self._delete_history(app_id=key[4:])

    def _refresh_rules(self) -> None:
        self.rule_table.setRowCount(len(self._rules))
        for row, (key, rule) in enumerate(sorted(self._rules.items())):
            values = [key] + ["Default" if name not in rule else "Enabled" if rule[name] else "Disabled" for name in ("enabled", "tab_enabled", "record_history")]
            for column, value in enumerate(values):
                self.rule_table.setItem(row, column, QTableWidgetItem(value))

    def open_models(self) -> None:
        if self.model_manager is None:
            self.model_manager = ModelManager(self.controller, self)
        self.model_manager.refresh_inventory()
        self.model_manager.show()
        self.model_manager.raise_()
        self.model_manager.activateWindow()

    def refresh_stats(self, *_args) -> None:
        try:
            data = self.controller.stats(self.period.currentData(), self.group_by.currentData())
            totals = data["totals"]
            today = data["today"]
            self.stats_totals.setText(f"{totals['completions']:,} accepted completions · {totals['words']:,} words · {totals['characters']:,} characters\nToday: {today['completions']:,} completions. Estimated typing time saved: {data['estimated_seconds']:,} seconds.")
            rows = data["rows"]
            self.stats_table.setRowCount(len(rows))
            for row, record in enumerate(rows):
                for column, name in enumerate(("label", "completions", "words", "characters")):
                    self.stats_table.setItem(row, column, QTableWidgetItem(str(record[name])))
        except Exception as exc:
            self.stats_totals.setText(f"Statistics unavailable: {exc}")
            self.stats_table.setRowCount(0)

    def refresh_status(self) -> None:
        status = self.controller.status()
        message = str(status.get("message", ""))
        if status.get("enabled") and status.get("state") in {"idle", "ready"} and not status.get("inline_available", True):
            message = str(status.get("inline_message") or message)
        self.window_context_status.setText(str(status.get("context_message") or "Context not requested"))
        self.notice.setText(self.shell.tray_notice or message)
        self.native_diagnostic.setText(str(status.get("native_status", "Native bridge status unavailable.")))
        model = self.controller.config.model_path or "None selected — choose or download a model."
        self.model_label.setText(model)
        self.setup_model.setText(f"Selected model: {model}")
        self.language_model.setText(f"Current model: {_model_name(self.controller.config)}\nNo language list is imposed by BroTypist. Consult the model's documentation for supported languages.")
        state = _tray_state(status, self.controller.config)
        self.sidebar_status.setText(f"{state.capitalize()} · {_model_name(self.controller.config)}")
        self.current_app.setText(f"Current application: {status.get('current_app') or 'No eligible application'}\nSession completions: {'Enabled' if status.get('current_app_enabled', self.controller.config.enabled) else 'Disabled'}\nObservable website: {status.get('current_domain') or 'Unknown / not available'}")
        self.battery_status.setText("Currently on battery power." if status.get("on_battery") else "System is not reporting battery power.")
        try:
            info = self.controller.history_info()
            self.history_status.setText(f"{info['count']:,} encrypted records · {info['bytes']:,} bytes" if info["available"] else f"Secure history unavailable: {info.get('error') or 'OS secret storage is not available.'}")
        except Exception as exc:
            self.history_status.setText(f"Secure history unavailable: {exc}")
        try:
            state = system.launch_at_login_state()
            self.login_status.setText("Launch at login is on." if state else "Launch at login is off.")
        except Exception as exc:
            self.login_status.setText(f"Login registration unavailable: {exc}")
        # Synchronize untouched controls with menu/runtime changes. Dirty draft
        # fields remain owned by the user until Save or Revert.
        self._loading = True
        for field, (_control, read, write) in self._bindings.items():
            current = deepcopy(getattr(self.controller.config, field))
            if read() == self._baseline.get(field) and current != self._baseline.get(field):
                write(current)
                self._baseline[field] = current
        latest_shortcuts = self.controller.config.shortcuts
        if latest_shortcuts != self._baseline["shortcuts"]:
            draft_shortcuts = {name: recorder.value() for name, recorder in self.shortcut_controls.items()}
            rebased = merge_mapping_edits(latest_shortcuts, self._baseline["shortcuts"], draft_shortcuts)
            for name, recorder in self.shortcut_controls.items():
                recorder.set_value(rebased.get(name, ""))
            self._baseline["shortcuts"] = deepcopy(latest_shortcuts)
        latest_rules = self.controller.config.app_rules
        if latest_rules != self._baseline["app_rules"]:
            self._rules = merge_mapping_edits(latest_rules, self._baseline["app_rules"], self._rules)
            self._baseline["app_rules"] = deepcopy(latest_rules)
            self._refresh_rules()
        self.login.setChecked(self.controller.config.launch_at_login)
        self.history.setChecked(self.controller.config.record_history)
        self._loading = False
        self._changed()
        self.refresh_updates()

    def closeEvent(self, event) -> None:
        if self._closing or self.shell._stopped:
            event.accept()
            return
        if not self.shell.tray_available:
            if QMessageBox.question(self, "Quit BroTypist?", "No system tray is available. Quit BroTypist and stop typing assistance?", QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No, QMessageBox.StandardButton.No) == QMessageBox.StandardButton.Yes:
                if self.shell.quit():
                    event.accept()
                    return
            event.ignore()
            return
        if self._changes():
            choice = QMessageBox.question(self, "Save settings changes?", "Save your settings changes before closing?", QMessageBox.StandardButton.Save | QMessageBox.StandardButton.Discard | QMessageBox.StandardButton.Cancel, QMessageBox.StandardButton.Save)
            if choice == QMessageBox.StandardButton.Cancel or (choice == QMessageBox.StandardButton.Save and not self.apply()):
                event.ignore()
                return
            if choice == QMessageBox.StandardButton.Discard:
                self.reload()
        self.hide()
        event.ignore()

    def reject(self) -> None:
        self.close()


class WelcomeWindow(QDialog):
    """Six real setup steps, with inference isolated to this try-it document."""

    TITLES = ("Welcome to BroTypist", "Your writing stays local", "System access", "Choose a model", "Try it", "You're all set")

    def __init__(self, shell: MenuAgent):
        super().__init__()
        self.shell = shell
        self.controller = shell.controller
        self.preview = None
        self.model_manager = None
        self._finishing = False
        self.setWindowTitle("Welcome to BroTypist")
        self.resize(760, 580)
        self.setMinimumSize(760, 580)
        self.setObjectName("welcome")
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        top = QHBoxLayout()
        top.setContentsMargins(24, 12, 24, 8)
        top.addStretch()
        self.segments = []
        for index in range(6):
            segment = QFrame()
            segment.setFixedSize(28, 4)
            segment.setAccessibleName(f"Step {index + 1} of 6")
            top.addWidget(segment)
            self.segments.append(segment)
        root.addLayout(top)
        self.pages = QStackedWidget()
        root.addWidget(self.pages, 1)
        for index, title in enumerate(self.TITLES):
            page = QWidget()
            layout = QVBoxLayout(page)
            layout.setContentsMargins(56, 20, 56, 20)
            heading = _label(title)
            if index == 0:
                heading.setTextFormat(Qt.TextFormat.RichText)
                heading.setText('Welcome to <b>Bro</b><span style="color:#A7A9AF">Typist</span>')
            font = heading.font()
            font.setPointSize(26)
            font.setBold(True)
            heading.setFont(font)
            layout.addWidget(heading)
            getattr(self, f"_step_{index}")(layout)
            layout.addStretch()
            area = QScrollArea()
            area.setWidgetResizable(True)
            area.setFrameShape(QFrame.Shape.NoFrame)
            area.setWidget(page)
            self.pages.addWidget(area)
        bottom = QHBoxLayout()
        bottom.setContentsMargins(24, 16, 24, 16)
        self.quit_button = QPushButton("Quit BroTypist")
        self.quit_button.setObjectName("quit")
        self.quit_button.setAutoDefault(False)
        self.quit_button.clicked.connect(self.shell.quit)
        bottom.addWidget(self.quit_button)
        quit_shortcut = QShortcut(QKeySequence(QKeySequence.StandardKey.Quit), self)
        quit_shortcut.activated.connect(self.shell.quit)
        self.back = QPushButton("Back")
        self.back.clicked.connect(lambda: self._select(self.pages.currentIndex() - 1))
        bottom.addWidget(self.back)
        self.hint = _label("")
        bottom.addWidget(self.hint, 1)
        self.next = QPushButton()
        self.next.setDefault(True)
        self.next.clicked.connect(self._next)
        bottom.addWidget(self.next)
        root.addLayout(bottom)
        self.timer = QTimer(self)
        self.timer.setInterval(150)
        self.timer.timeout.connect(self.refresh)
        self._select(0)

    def _step_0(self, layout) -> None:
        icon = QLabel()
        icon.setPixmap(app_icon().pixmap(112, 112))
        icon.setAccessibleName("BroTypist app icon")
        layout.addWidget(icon)
        layout.addWidget(_label("A cursor, then the words that come next."))
        for title, body in (
            ("Runs on this computer", "A local model. No account or cloud service required."),
            ("Stays out of the way", "Faint suggestions in supported apps. Accept what helps; keep typing to ignore the rest."),
            ("Yours to control", "Choose a model, review system access, and decide whether optional context or encrypted history is enabled."),
        ):
            group = QGroupBox(title)
            row = QVBoxLayout(group)
            row.addWidget(_label(body))
            layout.addWidget(group)

    def _step_1(self, layout) -> None:
        layout.addWidget(_label("Text before cursor → local model → suggestion"))
        layout.addWidget(_label("BroTypist reads eligible focused fields through the native bridge. Protected fields and excluded applications are skipped. Some applications cannot provide safe text access or geometry and remain unsupported."))
        layout.addWidget(_label("Local GGUF predictions do not upload your writing. Alternative server backends are restricted to loopback addresses. Model downloads use the network; a local server's own logging and network behavior are controlled by that server."))
        layout.addWidget(_label("Clipboard and focused-window Accessibility context are optional. Protected or hidden controls are excluded and unavailable providers return no context. Optional personalization history stores typing in OS-backed encrypted local storage; per-app recording overrides can enable it even when the global default is off. Review and delete it in Privacy."))
        privacy = QPushButton("Review Privacy settings…")
        privacy.clicked.connect(lambda: self.shell.show_settings("Privacy"))
        layout.addWidget(privacy)

    def _step_2(self, layout) -> None:
        self.native_status = _label("")
        layout.addWidget(self.native_status)
        layout.addWidget(_label("Windows uses accessible editable controls and a native keyboard hook. Linux/X11 uses AT-SPI and key grabs. Permission requirements differ by desktop; a settings page opening is not proof of access."))
        permissions = QPushButton("Open system access settings…")
        permissions.clicked.connect(lambda: self.shell._call(system.open_permissions, "accessibility"))
        layout.addWidget(permissions)
        if sys.platform.startswith("linux"):
            layout.addWidget(_label("KDE Wayland requires explicitly selecting BroTypist as the native input method in Virtual Keyboard settings. Only one method can occupy that slot. Manual controls are supported for eligible text inputs; ordinary inline completion is unavailable, with no preedit or suggestion popup substituted. Quit leaves KDE's selection unchanged and does not restart the compositor."))
            install = QPushButton("Install KDE input-method entry…")
            install.clicked.connect(self._install_wayland)
            layout.addWidget(install)
            settings = QPushButton("Open KDE Virtual Keyboard settings…")
            settings.clicked.connect(lambda: self.shell._call(system.open_wayland_settings))
            layout.addWidget(settings)
        self.access_reviewed = QCheckBox("I have reviewed the access requirements for this desktop")
        self.access_reviewed.toggled.connect(self.refresh)
        layout.addWidget(self.access_reviewed)

    def _install_wayland(self) -> None:
        self.shell.show_settings("General")
        self.shell.settings._install_wayland_method()

    def _step_3(self, layout) -> None:
        layout.addWidget(_label("Choose from the existing local model catalog or a GGUF file. Downloads and model changes happen only when you explicitly request them. Your current model and cache stay unchanged until then."))
        self.model_status = _label("")
        layout.addWidget(self.model_status)
        models = QPushButton("Choose or download a local model…")
        models.clicked.connect(self._models)
        layout.addWidget(models)
        layout.addWidget(_label("Local server configuration is available in Settings → AI Models. A configured server must already be running; Welcome does not start or download one."))

    def _models(self) -> None:
        self._close_preview()
        if self.model_manager is None:
            self.model_manager = ModelManager(self.controller, self)
        self.model_manager.show()
        self.model_manager.raise_()
        self.model_manager.activateWindow()

    def _step_4(self, layout) -> None:
        layout.addWidget(_label("Type below to ask your actual configured local model. Suggestions use only this field; nothing is inserted into another app or saved as personalization history."))
        self.try_field = QPlainTextEdit()
        self.try_field.setObjectName("welcome_try_it")
        self.try_field.setAccessibleName("Try local predictions")
        self.try_field.setPlaceholderText("Try: I was thinking we could")
        self.try_field.setMaximumHeight(130)
        font = self.try_field.font()
        font.setPointSize(17)
        self.try_field.setFont(font)
        self.try_field.installEventFilter(self)
        self.try_field.textChanged.connect(self._preview_update)
        self.try_field.cursorPositionChanged.connect(self._preview_update)
        layout.addWidget(self.try_field)
        self.suggestion = QLabel("", self.try_field.viewport())
        self.suggestion.setTextFormat(Qt.TextFormat.PlainText)
        self.suggestion.setFont(font)
        self.suggestion.setAccessibleName("Local model suggestion")
        self.suggestion.setFocusPolicy(Qt.FocusPolicy.NoFocus)
        self.suggestion.setAttribute(Qt.WidgetAttribute.WA_TransparentForMouseEvents)
        palette = self.suggestion.palette()
        palette.setColor(QPalette.ColorRole.WindowText, palette.color(QPalette.ColorRole.PlaceholderText))
        self.suggestion.setPalette(palette)
        self.suggestion.hide()
        self.try_status = _label("Focus the field and start typing.")
        layout.addWidget(self.try_status)
        self.key_hints = _label("")
        layout.addWidget(self.key_hints)

    def _step_5(self, layout) -> None:
        layout.addWidget(_label("When a system tray is available, use its menu to pause, resume, manage models or open Settings. Close window hides Settings; Quit BroTypist stops typing assistance and exits."))
        self.summary = _label("")
        layout.addWidget(self.summary)
        self.login = QCheckBox("Launch BroTypist at login")
        self.login.setChecked(self.controller.config.launch_at_login)
        self.login.toggled.connect(self._login_changed)
        layout.addWidget(self.login)
        layout.addWidget(_label("If your desktop has no tray host, Settings stays available after setup. Closing Settings or Welcome offers a real Quit action; Quit BroTypist is also always visible below."))

    def _login_changed(self, enabled: bool) -> None:
        old = self.controller.config.launch_at_login
        try:
            system.set_launch_at_login(enabled)
            try:
                self.controller.update_config(merge_preferences(self.controller.config, {"launch_at_login": enabled}))
            except Exception:
                system.set_launch_at_login(old)
                raise
        except Exception as exc:
            self.login.blockSignals(True)
            self.login.setChecked(old)
            self.login.blockSignals(False)
            _error(self, exc)

    def _model_ready(self) -> bool:
        config = self.controller.config
        return bool(config.model) if config.backend != "gguf" else bool(config.model_path and Path(config.model_path).is_file())

    def _select(self, index: int) -> None:
        self._close_preview()
        self.pages.setCurrentIndex(index)
        self.back.setVisible(index > 0)
        self.next.setText(("Get started", "Continue", "Continue", "Continue", "Finish", "Start typing")[index])
        palette = self.palette()
        for position, segment in enumerate(self.segments):
            color = palette.color(QPalette.ColorRole.WindowText if position <= index else QPalette.ColorRole.Mid)
            segment.setStyleSheet(f"background: {color.name()}; border-radius: 2px;")
        self.refresh()
        if index == 4:
            self.try_field.setFocus(Qt.FocusReason.OtherFocusReason)

    def _next(self) -> None:
        index = self.pages.currentIndex()
        if not self.next.isEnabled():
            return
        if index == 4:
            try:
                self.controller.update_config(merge_preferences(self.controller.config, {"setup_completed": True}))
            except Exception as exc:
                _error(self, exc)
                return
        if index == 5:
            self._finishing = True
            try:
                self.close()
            finally:
                self._finishing = False
        else:
            self._select(index + 1)

    def _preview_update(self) -> None:
        if self.pages.currentIndex() != 4 or not self.try_field.hasFocus():
            return
        self.suggestion.hide()
        try:
            if self.preview is None:
                from .preview import PreviewSession
                self.preview = PreviewSession(self.controller.config, self.controller)
            cursor = self.try_field.textCursor()
            # QTextCursor positions are UTF-16; PreviewSession consumes Python
            # character offsets. The selected prefix gives a lossless mapping.
            document = self.try_field.toPlainText()
            encoded = document.encode("utf-16-le")
            offset = len(encoded[:cursor.position() * 2].decode("utf-16-le", errors="ignore"))
            start = len(encoded[:cursor.selectionStart() * 2].decode("utf-16-le", errors="ignore"))
            end = len(encoded[:cursor.selectionEnd() * 2].decode("utf-16-le", errors="ignore"))
            self.preview.update(document, start if cursor.hasSelection() else offset, selection_length=end - start)
            self.shell._sync_shortcuts()
        except Exception as exc:
            self.try_status.setText(f"Preview unavailable: {exc}")
            self._close_preview()

    def _close_preview(self) -> None:
        if self.preview is not None:
            self.preview.close()
            self.preview = None
        self.suggestion.setText("")
        self.suggestion.hide()
        QTimer.singleShot(0, self.shell._sync_shortcuts)

    def eventFilter(self, watched, event) -> bool:
        if watched is self.try_field:
            if event.type() == QEvent.Type.FocusOut:
                self._close_preview()
            elif event.type() == QEvent.Type.FocusIn:
                QTimer.singleShot(0, self._preview_update)
            elif event.type() in (QEvent.Type.ShortcutOverride, QEvent.Type.KeyPress):
                modifiers = [name for flag, name in (
                    (Qt.KeyboardModifier.ControlModifier, "Ctrl"), (Qt.KeyboardModifier.AltModifier, "Alt"),
                    (Qt.KeyboardModifier.ShiftModifier, "Shift"), (Qt.KeyboardModifier.MetaModifier, "Super")
                ) if event.modifiers() & flag]
                if event.key() == Qt.Key.Key_Backtab and "Shift" not in modifiers:
                    modifiers.append("Shift")
                names = {Qt.Key.Key_Tab: "Tab", Qt.Key.Key_Backtab: "Tab", Qt.Key.Key_Space: "Space",
                         Qt.Key.Key_Return: "Return", Qt.Key.Key_Enter: "Return"}
                name = names.get(event.key(), QKeySequence(event.key()).toString(QKeySequence.SequenceFormat.PortableText))
                try:
                    chord = shortcut_from_event(name, modifiers, platform="windows" if sys.platform == "win32" else "linux",
                                                keycode=int(event.nativeScanCode()))
                except ValueError:
                    chord = ""
                action = next((key for key in ("word", "all") if chord and chord == self.controller.config.shortcuts.get(key)), None)
                dismiss = event.key() == Qt.Key.Key_Escape
                ready = self.preview is not None and bool(self.preview.poll().get("suggestion"))
                if self.preview is not None and (dismiss or (action and ready)):
                    if event.type() == QEvent.Type.KeyPress:
                        if dismiss:
                            self.preview.dismiss()
                        else:
                            text = self.preview.accept_word() if action == "word" else self.preview.accept_all()
                            if text:
                                cursor = self.try_field.textCursor()
                                self.try_field.blockSignals(True)
                                cursor.insertText(text)
                                self.try_field.setTextCursor(cursor)
                                self.try_field.blockSignals(False)
                                self._preview_update()
                        self.refresh()
                    event.accept()
                    return True
        return super().eventFilter(watched, event)

    def refresh(self, *_args) -> None:
        index = self.pages.currentIndex()
        config = self.controller.config
        self.next.setEnabled((index != 2 or self.access_reviewed.isChecked()) and (index != 3 or self._model_ready()))
        self.native_status.setText(str(self.controller.status().get("native_status", "Native access status unavailable.")))
        self.model_status.setText(f"Selected model: {_model_name(config)}\n" + (
            "Local model file is available." if config.backend == "gguf" and self._model_ready() else
            "No local model file selected. Choose one to continue." if config.backend == "gguf" else
            f"Configured local {config.backend} server: {config.endpoint}. Availability is checked by Try it."))
        keys = config.shortcuts
        self.key_hints.setText(f"{keys.get('word') or 'Unassigned'}: next word · {keys.get('all') or 'Unassigned'}: full suggestion · Esc: dismiss")
        self.summary.setText(f"Model: {_model_name(config)}\nNext word: {keys.get('word') or 'Unassigned'}\nFull suggestion: {keys.get('all') or 'Unassigned'}\nDismiss: Esc")
        self.hint.setText(("Use your current model and shortcuts", "Optional storage is your choice", "Review access; no permission is granted automatically",
                           "Choose a local model to continue", "Try it, or continue without accepting", "Use the tray menu any time")[index])
        if self.preview is not None:
            result = self.preview.poll()
            text = str(result.get("suggestion") or "")
            cursor = self.try_field.textCursor()
            rect = self.try_field.cursorRect()
            viewport = self.try_field.viewport()
            available = max(0, viewport.width() - rect.right() - 8)
            # Geometry is local to this owned widget, never guessed for a host.
            self.suggestion.setText(self.suggestion.fontMetrics().elidedText(text, Qt.TextElideMode.ElideRight, available))
            self.suggestion.adjustSize()
            self.suggestion.move(rect.right() + 1, rect.top())
            self.suggestion.setVisible(bool(text) and available > 20 and not cursor.hasSelection() and cursor.atBlockEnd() and self.try_field.hasFocus())
            self.suggestion.raise_()
            accepted = int(result.get("accepted", 0))
            self.try_status.setText(f"Accepted {accepted} suggestion{'s' if accepted != 1 else ''} — that's the whole trick." if accepted else
                                    str(result.get("message") or result.get("state") or "Waiting for you to type…"))

    def showEvent(self, event) -> None:
        super().showEvent(event)
        self.timer.start()
        self.refresh()

    def hideEvent(self, event) -> None:
        self.timer.stop()
        self._close_preview()
        super().hideEvent(event)

    def closeEvent(self, event) -> None:
        if not self.shell._stopped and not self.shell.tray_available:
            if self._finishing:
                self.shell.show_settings("General")
            elif QMessageBox.question(self, "Quit BroTypist?", "No system tray is available. Quit BroTypist and stop typing assistance?", QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No, QMessageBox.StandardButton.No) != QMessageBox.StandardButton.Yes or not self.shell.quit():
                event.ignore()
                return
        self._close_preview()
        event.accept()

    def reject(self) -> None:
        self.close()


class CaretOverlay(QWidget):
    """Transparent, mouse-through ghost text; never an editable/focused window."""

    def __init__(self):
        super().__init__(None, Qt.WindowType.ToolTip | Qt.WindowType.FramelessWindowHint | Qt.WindowType.WindowStaysOnTopHint | Qt.WindowType.WindowDoesNotAcceptFocus)
        self.setAttribute(Qt.WidgetAttribute.WA_TranslucentBackground)
        self.setAttribute(Qt.WidgetAttribute.WA_TransparentForMouseEvents)
        self.setAttribute(Qt.WidgetAttribute.WA_ShowWithoutActivating)
        self.setFocusPolicy(Qt.FocusPolicy.NoFocus)
        self.setAccessibleName("BroTypist completion suggestion")
        self._ghost = None
        layout = QHBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        self.surface = QFrame()
        self.surface.setObjectName("suggestion_chip")
        content = QHBoxLayout(self.surface)
        content.setContentsMargins(10, 4, 10, 4)
        content.setSpacing(8)
        self.text = QLabel()
        self.text.setTextFormat(Qt.TextFormat.PlainText)
        self.text.setFocusPolicy(Qt.FocusPolicy.NoFocus)
        self.text.setMaximumWidth(540)
        self.text.setWordWrap(True)
        content.addWidget(self.text)
        layout.addWidget(self.surface)
        self.setStyleSheet(
            "QFrame#suggestion_chip { background: palette(toolTipBase); color: palette(toolTipText); "
            "border: 1px solid palette(mid); border-radius: 8px; }"
            "QFrame#suggestion_chip QLabel { color: palette(toolTipText); background: transparent; border: none; }")

    def present(self, event: dict, point: QPoint) -> None:
        if event.get("mode", "completion") == "completion" or not event.get("choices"):
            self.hide()
            return
        self._ghost = None
        self.surface.show()
        self.setMinimumSize(0, 0)
        self.setMaximumSize(16777215, 16777215)
        choices = event.get("choices") or []
        text = "\n".join(f"{index}. {choice}" for index, choice in enumerate(choices[:9], 1))
        self.text.setText(text)
        self.adjustSize()
        screen = QApplication.screenAt(point)
        if screen:
            bounds = screen.availableGeometry()
            point.setX(max(bounds.left(), min(point.x(), bounds.right() - self.width() + 1)))
            if point.y() + self.height() > bounds.bottom() - 40:
                point.setY(point.y() - self.height() - int(event.get("line_height", 20)) - 12)
            point.setY(max(bounds.top(), min(point.y(), bounds.bottom() - self.height() + 1)))
        self.move(point)
        self.show()


    def present_inline(self, event: dict, point: QPoint, *, ratio: float) -> None:
        self.hide()
        try:
            render = InlineRender(**event["render"])
            if not event.get("inline") or not render.valid(event.get("rect")):
                return
            font = QFont(render.font_name)
            font.setPointSizeF(render.font_size)
            font.setWeight(QFont.Weight(render.font_weight))
            font.setItalic(render.italic)
            info, metrics = QFontInfo(font), QFontMetricsF(font)
            if (info.family().casefold() != render.font_name.casefold()
                    or abs(metrics.ascent() * ratio - render.ascent) > 1.5
                    or abs(metrics.descent() * ratio - render.descent) > 1.5):
                return
            text = str(event.get("text") or "").split("\n", 1)[0].split("\r", 1)[0]
            x, y, width, height = render.clip_rect
            available = (x + width - float(event["rect"][0])) / ratio
            width = min(metrics.horizontalAdvance(text), available)
            if not text or width < 1:
                return
            import math
            self.surface.hide()
            self._ghost = (text, font, render.ascent / ratio)
            self.setFixedSize(math.ceil(width), math.ceil((render.ascent + render.descent) / ratio))
            self.move(point)
            screen = QApplication.screenAt(point)
            if screen is None or not screen.geometry().contains(self.geometry()):
                return
            self.show()
            self.update()
        except (KeyError, TypeError, ValueError):
            self._ghost = None

    def paintEvent(self, event):
        if self._ghost is None:
            return super().paintEvent(event)
        text, font, baseline = self._ghost
        painter = QPainter(self)
        painter.setFont(font)
        painter.setPen(QColor("#909299"))
        painter.setClipRect(self.rect())
        painter.drawText(QPointF(0, baseline), text)
        painter.end()


class Accessory(QWidget):
    """A nonactivating mouse target; actions retain controller revalidation."""

    def __init__(self, shell: MenuAgent):
        super().__init__(None, Qt.WindowType.Tool | Qt.WindowType.FramelessWindowHint | Qt.WindowType.WindowStaysOnTopHint | Qt.WindowType.WindowDoesNotAcceptFocus)
        self.shell = shell
        self.setAttribute(Qt.WidgetAttribute.WA_ShowWithoutActivating)
        self.setFocusPolicy(Qt.FocusPolicy.NoFocus)
        self.setFixedSize(24, 24)
        layout = QHBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        self.button = QToolButton()
        self.button.setFocusPolicy(Qt.FocusPolicy.NoFocus)
        self.button.setIcon(tray_icon())
        self.button.setIconSize(QSize(18, 16))
        self.button.setFixedSize(24, 24)
        self.button.setToolButtonStyle(Qt.ToolButtonStyle.ToolButtonIconOnly)
        self.button.setStyleSheet("QToolButton { border: none; padding: 3px; }")
        self.button.setToolTip("BroTypist — app predictions, pause and Settings")
        self.button.setAccessibleName("BroTypist accessory menu")
        self.button.clicked.connect(self._menu)
        layout.addWidget(self.button)
        self.menu: QMenu | None = None

    def _menu(self) -> None:
        if self.menu:
            if self.menu.isVisible():
                return
            self.menu.close()
            self.menu.deleteLater()
        self.menu = self.shell.create_field_menu()
        self.menu.setWindowFlag(Qt.WindowType.WindowDoesNotAcceptFocus, True)
        self.menu.setAttribute(Qt.WidgetAttribute.WA_ShowWithoutActivating)
        self.menu.setFocusPolicy(Qt.FocusPolicy.NoFocus)
        self.menu.popup(self.mapToGlobal(QPoint(0, self.height())))


class MenuAgent:
    def __init__(self, app: QApplication, config: Config, *, no_tray: bool = False):
        self.app = app
        self.controller = Controller(config)
        self.updates = UpdateService()
        self._update_snapshot: dict | None = None
        self._update_handoff = None
        self._install_requested = False
        self._shutdown_pending = False
        self.controller.show_settings = self.show_settings
        self._clipboard_reader = ClipboardReader(app)
        self.controller.set_clipboard_reader(self._clipboard_reader.read)
        self.controller.set_screen_reader(system.screen_text)
        self.controller.set_battery_reader(system.on_battery)
        self._tray_requested = not no_tray
        self.tray_available = not no_tray and QSystemTrayIcon.isSystemTrayAvailable()
        self.tray_notice = "" if self.tray_available else (
            "System tray disabled. Keep settings open to control BroTypist; closing offers Quit." if no_tray else
            "No system tray is available. Keep settings open, or enable your desktop's tray extension; BroTypist reconnects automatically. Closing offers Quit.")
        self.settings: SettingsWindow | None = None
        self.welcome: WelcomeWindow | None = None
        self.overlay = CaretOverlay()
        self.accessory = Accessory(self)
        self.choices: list[str] = []
        self._stopped = False
        self._stopping = False
        self._quitting = False
        self._ticks = 0
        self._tray_ticks = 0
        self._last_overlay: dict | None = None
        self._tracking_menus: set[QMenu] = set()
        self._menu_session = False
        self._native_surface = bool(getattr(self.controller.bridge, "supports_native_overlay", False))
        self._native_refresh_pending = False
        self._native_anchor = ""
        self._native_menu_active = False
        self._native_menu: QMenu | None = None
        self._native_actions: dict[str, QAction] = {}
        self._deferred_actions: list[tuple] = []
        self._hide_accessory_after_menu = False
        self.tray = QSystemTrayIcon(tray_icon(), app)
        self.tray.setToolTip("BroTypist")
        self.menu = self.create_menu()
        self.tray.setContextMenu(self.menu)
        self.menu.aboutToShow.connect(self.refresh_menu)
        self.tray.activated.connect(self._activated)
        if self._tray_requested:
            self.tray.show()
        self.timer = QTimer()
        self.timer.setInterval(25)
        self.timer.timeout.connect(self.tick)
        self._ui_wake = UiWake(app, self.tick)
        self.controller.set_wake_callback(self._ui_wake.requested.emit)
        self.app.aboutToQuit.connect(self.stop)
        if self._native_surface:
            self.controller.bridge.bind_ui(self._native_action)

    def start(self, *, background: bool = False) -> None:
        try:
            self.controller.start()
        except Exception as exc:
            # Native backend failure must leave a recoverable settings surface.
            self.tray_notice = f"Global typing could not start: {exc}. Review General and system access."
            self.show_settings("General")
        self.timer.start()
        # background means no unnecessary foreground activation, never bypassing
        # first-run setup or hiding all controls when the tray is unavailable.
        if not self.tray_available:
            self.show_settings("General")
        config = self.controller.config
        missing_model = config.backend == "gguf" and not (config.model_path and Path(config.model_path).is_file())
        if not config.setup_completed or missing_model:
            self.show_welcome()
        if config.check_updates_automatically:
            self.updates.start_check(automatic=True)

    def create_menu(self) -> QMenu:
        menu = QMenu()
        self._watch_menu(menu)
        # Plain QActions are intentionally the complete menu: KDE's SNI/DBus
        # exporter cannot display essential controls hidden in QWidgetActions.
        status = menu.addAction("BroTypist")
        status.setEnabled(False)
        status.setObjectName("status")
        for name in ("context", "model", "detail"):
            action = menu.addAction("")
            action.setObjectName(name)
            action.setEnabled(False)
        menu.addSeparator()
        enabled = menu.addAction("Predictions")
        enabled.setCheckable(True)
        enabled.setChecked(bool(self.controller.config.global_enabled))
        enabled.setObjectName("enabled")
        enabled.triggered.connect(lambda checked: self._menu_action(self._set_predictions, checked))
        resume = menu.addAction("Resume now", lambda: self._invoke("toggle"))
        resume.setObjectName("resume")
        pause = menu.addMenu("Pause")
        pause.menuAction().setObjectName("pause")
        self._watch_menu(pause)
        for minutes in (5, 15, 60):
            title = "Pause for 1 hour" if minutes == 60 else f"Pause for {minutes} minutes"
            pause.addAction(title, lambda _checked=False, minutes=minutes: self._menu_action(self.controller.pause, minutes))
        pause.addAction("Pause until tomorrow", lambda: self._menu_action(self._pause_until_tomorrow))
        app_toggle = menu.addAction("Turn off in current application (this session)")
        app_toggle.setObjectName("app_toggle")
        app_toggle.setCheckable(True)
        current = self.controller.status()
        app_toggle.setChecked(bool(current.get("current_app_enabled", self.controller.config.enabled)))
        app_toggle.setEnabled(bool(current.get("current_app")))
        app_toggle.triggered.connect(lambda _checked: self._invoke("app_toggle"))
        menu.addSeparator()
        menu.addAction("Request completion", lambda: self._invoke("force")).setObjectName("force")
        menu.addAction("Alternative words", lambda: self._invoke("alternatives")).setObjectName("alternatives")
        synonyms = menu.addAction("Selected-text synonyms", lambda: self._invoke("synonyms"))
        synonyms.setObjectName("synonyms")
        if sys.platform.startswith("linux"):
            synonyms.setEnabled(False)
            synonyms.setToolTip("Unavailable: AT-SPI cannot atomically replace selected text.")
        menu.addSeparator()
        menu.addAction("Settings…", lambda: self._menu_action(self.show_settings, "General"))
        menu.addAction("Welcome…", lambda: self._menu_action(self.show_welcome))
        menu.addAction("Models…", lambda: self._menu_action(self.open_models))
        menu.addAction("Check for Updates…", lambda: self._menu_action(self.check_updates)).setObjectName("check_updates")
        menu.addSeparator()
        # DBus tray exporters need not emit QMenu.aboutToHide when an exported
        # action is invoked. Quit must not depend on that menu-tracking fence.
        menu.addAction("Quit BroTypist", self.quit).setObjectName("quit")
        self.refresh_menu(menu)
        return menu

    def create_field_menu(self) -> QMenu:
        """The field accessory is deliberately not a second tray dashboard."""
        menu = QMenu()
        self._watch_menu(menu)
        status = self.controller.status()
        app = str(status.get("current_app") or "")
        domain = str(status.get("current_domain") or "")
        allowed = bool(status.get("current_app_enabled", self.controller.config.enabled))
        name = app[:29] + "…" if len(app) > 30 else app or "Current App"
        toggle = menu.addAction(f"Predictions in {name}: {'Enabled' if allowed else 'Disabled'}".replace("&", "&&"))
        toggle.setObjectName("field_app_toggle")
        toggle.setCheckable(True)
        toggle.setChecked(allowed and bool(app))
        toggle.setEnabled(bool(app))
        toggle.setToolTip("Changes predictions for this app for the current session only.")
        toggle.triggered.connect(
            lambda _checked: self._menu_action(self._toggle_field_app, app, domain))
        paused = float(status.get("paused_until", 0)) > time.monotonic()
        pause = menu.addAction("Resume predictions" if paused else "Pause for 1 hour")
        pause.setObjectName("field_pause")
        pause.setEnabled(paused or self.controller.config.global_enabled)
        pause.triggered.connect(
            lambda _checked=False: self._menu_action(self.controller.pause, 0 if paused else 60))
        menu.addSeparator()
        menu.addAction("Settings…", lambda: self._menu_action(self.show_settings, "General")).setObjectName("field_settings")
        return menu

    def _toggle_field_app(self, app: str, domain: str) -> None:
        self.controller.toggle_current_app(app=app, domain=domain)

    def _set_predictions(self, checked: bool) -> None:
        self.controller.update_config(merge_preferences(self.controller.config, {"global_enabled": checked}))
        if checked:
            self.controller.pause(0)
        self.refresh_menu()

    def _pause_until_tomorrow(self) -> None:
        now = datetime.now()
        tomorrow = (now + timedelta(days=1)).replace(hour=0, minute=0, second=0, microsecond=0)
        self.controller.pause(max(1, int((tomorrow.timestamp() - now.timestamp() + 59) // 60)))

    def _native_refresh(self) -> None:
        self._native_refresh_pending = False
        self.tick()

    def _native_action(self, identifier: str) -> None:
        if self._stopped:
            return
        if identifier == "context_changed":
            if not self._native_refresh_pending:
                self._native_refresh_pending = True
                QTimer.singleShot(0, self._native_refresh)
            return
        if identifier == "menu":
            if self._native_menu_active:
                return
            menu = self.create_field_menu()
            self._native_menu = menu
            self._native_actions = {}
            rows = []

            for action in menu.actions():
                if action.isSeparator():
                    continue
                title = action.text().replace("&&", "\0").replace("&", "").replace("\0", "&")
                if action.isCheckable():
                    title = ("✓ " if action.isChecked() else "") + title
                key = f"action:{len(self._native_actions)}"
                self._native_actions[key] = action
                rows.append((key, title, action.isEnabled()))
            self._menu_session = True
            self._native_menu_active = True
            self._sync_shortcuts()
            if not self.controller.bridge.show_native_menu(rows):
                self._native_action("menu_closed")
            return
        action = self._native_actions.get(identifier)
        menu, self._native_menu = self._native_menu, None
        self._native_actions = {}
        self._native_menu_active = False
        if identifier != "menu_closed":
            self.controller.bridge.hide_native()
        if action is not None and action.isEnabled():
            self._call(action.trigger)
        if menu is not None:
            menu.deleteLater()
        # Native menu callbacks need the same deferred selection boundary as
        # QMenu: keep capture suspended until the native event has unwound.
        QTimer.singleShot(0, self._finish_menu_session)


    def _watch_menu(self, menu: QMenu) -> None:
        menu.aboutToShow.connect(lambda: self._menu_opened(menu))
        menu.aboutToHide.connect(lambda: self._menu_closed(menu))

    def _menu_opened(self, menu: QMenu) -> None:
        self._tracking_menus.add(menu)
        self._menu_session = True
        self._sync_shortcuts()

    def _menu_closed(self, menu: QMenu) -> None:
        self._tracking_menus.discard(menu)
        if not self._tracking_menus:
            # QAction.triggered can follow aboutToHide. Keep capture and native
            # interception suspended until the whole selection event unwinds.
            QTimer.singleShot(0, self._finish_menu_session)

    def _sync_shortcuts(self) -> None:
        if self._stopped:
            return
        recording = self.settings and any(
            recorder._recording for recorder in self.settings.shortcut_controls.values())
        preview_focused = bool(self.welcome and self.welcome.isVisible() and self.welcome.try_field.hasFocus())
        self.controller.bridge.set_shortcuts_enabled(
            not self._stopped and not self._menu_session and not self._native_menu_active and not recording and not preview_focused)
        if (recording or preview_focused) and self._native_surface:
            self.controller.bridge.hide_native()

    def _menu_action(self, function, *args) -> None:
        if self._menu_session:
            self._deferred_actions.append((function, args))
        else:
            self._call(function, *args)

    def _finish_menu_session(self) -> None:
        if self._tracking_menus or self._native_menu_active or not self._menu_session or self._stopped:
            return
        actions, self._deferred_actions = self._deferred_actions, []
        for function, args in actions:
            # Run only after tracking releases focus. Field preference actions
            # keep their original app; insertion actions revalidate foreground.
            self._call(function, *args)
            if self._stopped:
                return
        self._menu_session = False
        if self._hide_accessory_after_menu:
            self.accessory.hide()
            self._hide_accessory_after_menu = False
        # Revalidate the focused field and drain clears before interception
        # resumes, including when the menu was dismissed without an action.
        self.tick()
        self._sync_shortcuts()

    def _call(self, function, *args) -> None:
        try:
            function(*args)
        except Exception as exc:
            self.show_settings("General")
            _error(self.settings, exc)

    def _invoke(self, action: str) -> None:
        self._menu_action(self.controller.invoke, action)

    def _activated(self, reason) -> None:
        if reason == QSystemTrayIcon.ActivationReason.DoubleClick:
            self.show_settings("General")
        elif reason == QSystemTrayIcon.ActivationReason.Trigger:
            self.refresh_menu()
            geometry = self.tray.geometry()
            self.menu.popup(geometry.bottomLeft() if geometry.isValid() else QCursor.pos())

    def refresh_menu(self, menu: QMenu | None = None) -> None:
        status = self.controller.status()
        config = self.controller.config
        state = _tray_state(status, config)
        paused = state == "paused"
        remaining = max(0, float(status.get("paused_until", 0)) - time.monotonic())
        resume_at = datetime.fromtimestamp(time.time() + remaining).strftime("%H:%M")
        app_name = str(status.get("current_app") or "No eligible application")
        backend = {"gguf": "GGUF process", "ollama": "local Ollama", "openai": "local compatible server"}.get(config.backend, config.backend)
        for action in (menu or self.menu).actions():
            name = action.objectName()
            if name == "status":
                label = "Needs attention" if status.get("state") == "error" else {
                    "active": "Active", "paused": "Paused", "loading": "Loading model",
                    "unavailable": "Unavailable here", "off": "Predictions off"}[state]
                action.setText(f"BroTypist — {label}")
                action.setIcon(tray_icon("active" if state == "unavailable" else state))
            elif name == "context":
                action.setText((f"Resumes at {resume_at}" if paused else
                                "Warming up model…" if state == "loading" else
                                str(status.get("inline_message") or "Inline unavailable here") if not status.get("inline_available", True) else
                                f"Predicting in {app_name}" if status.get("current_app") else app_name).replace("&", "&&"))
            elif name == "model":
                action.setText(f"Model: {_model_name(config)} · {backend}".replace("&", "&&"))
            elif name == "detail":
                download = status.get("download") or {}
                action.setText(str(download.get("error") or status.get("message") or "Local predictions").replace("&", "&&"))
            elif name == "enabled":
                action.setChecked(bool(config.global_enabled))
            elif name == "resume":
                action.setVisible(paused)
                action.setEnabled(paused)
            elif name == "pause":
                action.setEnabled(bool(config.global_enabled) and not paused)
            elif name == "app_toggle":
                allowed = bool(status.get("current_app_enabled", config.enabled))
                action.setText(f"Turn {'off' if allowed else 'on'} in {app_name[-70:]} (this session)".replace("&", "&&"))
                action.setChecked(allowed)
                action.setEnabled(bool(status.get("current_app")))

    def show_settings(self, pane: str = "General") -> None:
        if isinstance(pane, str) and pane.casefold() == "welcome":
            self.show_welcome()
            return
        if isinstance(pane, str) and pane.casefold() == "models":
            pane = "AI Models"
        if self.settings is None:
            self.settings = SettingsWindow(self)
        self.settings.refresh_status()
        self.settings.show_pane(pane if isinstance(pane, str) else "General")

    def show_welcome(self) -> None:
        if self.welcome is None:
            self.welcome = WelcomeWindow(self)
        self.clear_overlay()
        self.welcome.show()
        self.welcome.raise_()
        self.welcome.activateWindow()

    def open_models(self) -> None:
        self.show_settings("AI Models")
        self.settings.open_models()

    def check_updates(self) -> None:
        if self._shutdown_pending or self._stopped:
            return
        self.show_settings("About")
        self._update_action(self.updates.start_check)

    def _update_action(self, function) -> None:
        if self._shutdown_pending or self._stopped:
            return
        try:
            function()
        except Exception:
            self.show_settings("About")
            _error(self.settings, "The update action could not be completed. Check the update status and try again.")
        self._poll_updates()

    def _poll_updates(self) -> None:
        snapshot = self.updates.snapshot()
        if snapshot != self._update_snapshot:
            self._update_snapshot = snapshot
            if self.settings:
                self.settings.refresh_updates(snapshot)
        for action in self.menu.actions():
            if action.objectName() == "check_updates":
                action.setEnabled(not self._shutdown_pending and snapshot.get("state") not in {"checking", "downloading", "installing"})

    def install_update(self) -> None:
        if self._shutdown_pending or self._stopped:
            return
        status = self.updates.snapshot()
        if status.get("state") != "ready":
            return
        self.show_settings("About")
        if status.get("install_kind") == "linux_debian":
            choice = QMessageBox.question(
                self.settings, "Open system package installer?",
                "Open the verified Debian package in the system installer? It may ask for administrator approval. "
                "BroTypist cannot report whether installation succeeds; quit and reopen it after the installer finishes.",
                QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.Cancel, QMessageBox.StandardButton.Cancel)
            if choice == QMessageBox.StandardButton.Yes:
                self._update_action(self.updates.open_package_installer)
            return
        if not status.get("installable"):
            return
        choice = QMessageBox.question(
            self.settings, "Install update and relaunch?",
            "Stop typing assistance, install the verified update and reopen BroTypist? Your settings, history and models are kept.",
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.Cancel, QMessageBox.StandardButton.Cancel)
        if choice != QMessageBox.StandardButton.Yes:
            return
        self._install_requested = True
        self.quit()

    def _position(self, rect, *, clamp: bool = True) -> tuple[QPoint, int]:
        x, y, width, height = (float(value) for value in rect)
        # Native Windows and X11 caret rectangles use device pixels. Qt widgets
        # use logical desktop coordinates. Convert relative to the owning screen
        # rather than dividing a mixed-DPI desktop's global origin.
        screens = self.app.screens()
        screen = None
        for candidate in screens:
            geometry = candidate.geometry()
            ratio = candidate.devicePixelRatio()
            # Windows keeps physical screen origins while scaling screen size;
            # X11's uniform Qt scale also scales the virtual desktop origin.
            native_left = geometry.x() if sys.platform == "win32" else geometry.x() * ratio
            native_top = geometry.y() if sys.platform == "win32" else geometry.y() * ratio
            if native_left <= x <= native_left + geometry.width() * ratio and native_top <= y <= native_top + geometry.height() * ratio:
                screen = candidate
                x = geometry.x() + (x - native_left) / ratio
                y = geometry.y() + (y - native_top) / ratio
                height /= ratio
                width /= ratio
                break
        if screen is None:
            screen = self.app.screenAt(QPoint(round(x), round(y))) or self.app.primaryScreen()
        point = QPoint(round(x + width + 2), round(y))
        if clamp and screen:
            available = screen.availableGeometry()
            point.setX(max(available.left(), min(point.x(), available.right() - 100)))
            point.setY(max(available.top(), min(point.y(), available.bottom() - 50)))
        return point, max(18, round(height))

    def _suggestion(self, event: dict) -> None:
        if (self.welcome and (self.welcome.isActiveWindow() or (self.welcome.model_manager and self.welcome.model_manager.isActiveWindow()))) or (
                self.settings and (self.settings.isActiveWindow() or (self.settings.model_manager and self.settings.model_manager.isActiveWindow()))):
            self.clear_overlay()
            return
        if self._native_surface:
            self._last_overlay = event
            self.choices = list(event.get("choices") or [])
            self._native_anchor = str(event.get("native_anchor") or "")
            self.controller.bridge.present_native(self._native_anchor, str(event.get("text") or ""))
            return
        rect = event.get("rect")
        if not isinstance(rect, (tuple, list)) or len(rect) != 4:
            self.clear_overlay()
            return
        self._last_overlay = event
        self.choices = list(event.get("choices") or [])
        rect = (rect[0], rect[1], 0, rect[3]) if event.get("field_anchor") else rect
        point, height = self._position(rect, clamp=False)
        presentation = {**event, "accept_key": self.controller.config.shortcuts.get("word", ""), "line_height": height}
        if event.get("mode", "completion") == "completion":
            payload = event.get("render")
            if not isinstance(payload, dict):
                self.overlay.hide()
                self._caret(event)
                return
            screen = self.app.screenAt(point)
            ratio = screen.devicePixelRatio() if screen else 1.0
            baseline = float(payload.get("baseline", 0))
            ascent = float(payload.get("ascent", 0))
            point, _ = self._position((rect[0], baseline - ascent, 0, 1), clamp=False)
            point.setX(point.x() - 2)
            self.overlay.present_inline(presentation, point, ratio=ratio)
            self._caret(event)
            return
        point.setX(point.x() - 2)
        point.setY(point.y() + height + 6)
        self.overlay.present(presentation, point)
        self._caret(event)

    def _caret(self, event: dict) -> None:
        if self._menu_session:
            return
        if self.welcome and self.welcome.isActiveWindow():
            self.clear_overlay()
            return
        if self._native_surface:
            self._native_anchor = str(event.get("native_anchor") or "")
            text = str((self._last_overlay or {}).get("text") or "")
            if self.controller.config.show_accessory or text:
                status = self.controller.status()
                self.controller.bridge.present_native(self._native_anchor, text, paused=status.get("state") == "paused")
            else:
                self.controller.bridge.hide_native()
            return
        caret, field = event.get("rect"), event.get("field_rect")
        valid_caret = isinstance(caret, (tuple, list)) and len(caret) == 4
        valid_field = isinstance(field, (tuple, list)) and len(field) == 4
        rect = caret if valid_caret else field
        if not self.controller.config.show_accessory or not (valid_caret or valid_field):
            self.accessory.hide()
            return
        point, height = self._position((rect[0], rect[1], 0, rect[3]), clamp=False)
        left = point.x() - 2
        if valid_field:
            field_point, _ = self._position((field[0], field[1], 0, field[3]), clamp=False)
            left = field_point.x() - 2
        screen = self.app.screenAt(QPoint(point.x() - 2, point.y())) or self.app.primaryScreen()
        top = point.y() - self.accessory.height() - 4
        if screen:
            bounds = screen.availableGeometry()
            left = max(bounds.left(), min(left, bounds.right() - self.accessory.width() + 1))
            if top < bounds.top():
                # At the top edge, use the space below the line rather than
                # clamping the icon over the text it is meant to leave visible.
                top = point.y() + height + 4
            if top < bounds.top() or top + self.accessory.height() > bounds.bottom() + 1:
                self.accessory.hide()
                return
        self.accessory.move(left, top)
        self.accessory.show()

    def clear_overlay(self, *, hide_accessory: bool = True) -> None:
        self._last_overlay = None
        self.choices = []
        self.overlay.hide()
        if self._native_surface:
            if hide_accessory or not self.controller.config.show_accessory:
                self._native_anchor = ""
                self.controller.bridge.hide_native()
            else:
                self.controller.bridge.present_native(self._native_anchor, "")
        if hide_accessory:
            if self._menu_session:
                self._hide_accessory_after_menu = True
            else:
                self.accessory.hide()

    def _refresh_tray_availability(self) -> None:
        available = QSystemTrayIcon.isSystemTrayAvailable()
        previous, self.tray_available = self.tray_available, available
        if previous and not available:
            self.tray_notice = "The system tray became unavailable. Settings remain open so BroTypist stays controllable."
            # An exported menu can disappear with its host without aboutToHide.
            # Release only that tray's fence, not an unrelated field menu.
            for menu in (self.menu, *self.menu.findChildren(QMenu)):
                menu.close()
                self._tracking_menus.discard(menu)
            QTimer.singleShot(0, self._finish_menu_session)
            self.show_settings("General")
        elif available and not previous:
            self.tray_notice = ""
        if not self.tray.isVisible():
            self.tray.show()

    def tick(self) -> None:
        if self._stopped or self._stopping:
            return
        try:
            # Recovery UI cannot depend on capture/menu tracking making progress.
            self._tray_ticks += 1
            if self._tray_requested and self._tray_ticks % 40 == 0:
                self._refresh_tray_availability()
            if not self._shutdown_pending and self.controller.config.check_updates_automatically:
                self.updates.start_check(automatic=True)
            self._poll_updates()
            if self._menu_session or self._native_menu_active:
                return
            self.controller.tick()
            for event in self.controller.drain_events():
                kind = event.get("type")
                if kind == "suggestion":
                    self._suggestion(event)
                elif kind == "clear":
                    self.clear_overlay(hide_accessory=event.get("hide_accessory", False))
                elif kind == "caret":
                    self._caret(event)
                elif kind == "settings":
                    self.show_settings(event.get("pane", "General"))
            self._ticks += 1
            if self._ticks % 40 == 0:
                status = self.controller.status()
                state = _tray_state(status, self.controller.config)
                icon_state = "active" if state == "unavailable" else state
                self.tray.setIcon(tray_icon(icon_state))
                self.accessory.button.setIcon(tray_icon(icon_state))
                description = status.get("message") or _model_name(self.controller.config)
                if state == "paused":
                    remaining = max(0, float(status.get("paused_until", 0)) - time.monotonic())
                    description = "Resumes at " + datetime.fromtimestamp(time.time() + remaining).strftime("%H:%M")
                self.tray.setToolTip(f"BroTypist, {state} — {description}")
                self.refresh_menu()
                if self.settings and self.settings.isVisible():
                    self.settings.refresh_status()
                    if self.settings.sidebar.currentItem() and self.settings.sidebar.currentItem().text() == "General":
                        self.settings.refresh_stats()
                if not self.controller.config.show_accessory:
                    self.accessory.hide()
        except Exception as exc:
            self.clear_overlay()
            self.tray_notice = f"Typing assistance error: {exc}. Review General; you can still change settings or Quit."
            self.show_settings("General")

    def stop(self) -> None:
        if self._stopped or self._stopping:
            return
        self._stopping = True
        self.timer.stop()
        self._deferred_actions.clear()
        self._native_menu_active = False
        self._native_actions.clear()
        self._menu_session = False
        for menu in tuple(self._tracking_menus):
            menu.close()
        self._tracking_menus.clear()
        self.menu.close()
        if self.accessory.menu:
            self.accessory.menu.close()
        self.overlay.hide()
        self.accessory.hide()
        try:
            try:
                if self.welcome:
                    self.welcome._close_preview()
            finally:
                self.controller.stop()
            for window in (self.welcome, self.settings):
                if window and window.model_manager:
                    window.model_manager._refresh_timer.stop()
                    window.model_manager.hide()
            if self._update_handoff is not None:
                # prepare_install settles the update worker before it creates
                # a helper. Approval is only sent after all controller-owned
                # work has stopped; its pipe stays open until actual exit.
                self._update_handoff.commit()
            self.updates.close()
        except ShutdownPending:
            self.timer.start()
            raise
        except Exception:
            self._abort_update_install()
            # Keep a visible, working Quit button so incomplete cleanup can be
            # retried; never strand an invisible process marked as stopped.
            self.timer.start()
            raise
        else:
            self._stopped = True
            self._shutdown_pending = False
            retry = getattr(self, "_quit_retry", None)
            if retry is not None:
                retry.stop()
            self._last_overlay = None
            self._native_anchor = ""
            self.choices = []
            if self._native_menu:
                self._native_menu.deleteLater()
                self._native_menu = None
            for window in (self.welcome, self.settings):
                if window:
                    if window.model_manager:
                        window.model_manager._refresh_timer.stop()
                        window.model_manager.hide()
                    if window is self.welcome:
                        window.timer.stop()
                    window.hide()
            self.tray.hide()
            self._hide_accessory_after_menu = False
        finally:
            self._stopping = False

    def _abort_update_install(self) -> None:
        handoff = self._update_handoff
        if handoff is not None and not handoff.committed:
            handoff.abort()
            self.updates.cancel()
            self._update_handoff = None
        self._install_requested = False

    def quit(self) -> bool:
        if self._quitting:
            return False
        self._quitting = True
        try:
            if self.settings and self.settings._changes():
                choice = QMessageBox.question(self.settings, "Save before quitting?", "Save your settings changes before quitting BroTypist?", QMessageBox.StandardButton.Save | QMessageBox.StandardButton.Discard | QMessageBox.StandardButton.Cancel, QMessageBox.StandardButton.Save)
                if choice == QMessageBox.StandardButton.Cancel or (choice == QMessageBox.StandardButton.Save and not self.settings.apply()):
                    self._abort_update_install()
                    return False
                if choice == QMessageBox.StandardButton.Discard:
                    self.settings.reload()
            updating = self._install_requested or self._update_handoff is not None
            try:
                if self._install_requested and self._update_handoff is None:
                    self._update_handoff = self.updates.prepare_install()
                elif self._update_handoff is None:
                    self.updates.cancel()
                self.stop()
            except ShutdownPending:
                self._shutdown_pending = True
                retry = getattr(self, "_quit_retry", None)
                if retry is None:
                    retry = self._quit_retry = QTimer(self.app)
                    retry.setSingleShot(True)
                    retry.timeout.connect(self.quit)
                self.tray_notice = "Closing BroTypist — cancelling local background work…"
                self.show_settings("About" if self._install_requested else "General")
                for window in (self.settings, self.welcome):
                    if window:
                        window.setEnabled(False)
                retry.start(100)
                return False
            except Exception:
                self._abort_update_install()
                self._shutdown_pending = False
                retry = getattr(self, "_quit_retry", None)
                if retry is not None:
                    retry.stop()
                for window in (self.settings, self.welcome):
                    if window:
                        window.setEnabled(True)
                self.tray_notice = ("Update installation was cancelled because shutdown could not finish. "
                                    "No update was approved; choose Quit BroTypist again to release local resources."
                                    if updating and self._update_handoff is None else
                                    "Shutdown is incomplete. Choose Quit BroTypist again to finish releasing local resources.")
                self.show_settings("About" if updating else "General")
                _error(self.settings, self.tray_notice)
                return False
            if self.settings:
                self.settings._closing = True
            self.app.quit()
            return True
        finally:
            self._quitting = False


def launch(config: Config, *, background: bool = False, no_tray: bool = False, pane: str | None = None) -> int:
    """Run the portable resident utility using the operating system's Qt theme."""
    app = QApplication.instance() or QApplication(sys.argv[:1])
    app.setQuitOnLastWindowClosed(False)
    configure_identity(app)
    shell = MenuAgent(app, config, no_tray=no_tray)
    # Retain ownership independently of Python's local frame for embedded smoke
    # entrypoints and application shutdown callbacks.
    app._inklow_menu_agent = shell
    def start_resident():
        # Prove that the replacement reached its real GUI event loop, then
        # remove the private receipt environment before model workers can start.
        expected_ack = bool(os.environ.get(HEALTH_PATH) or os.environ.get(HEALTH_NONCE))
        try:
            acknowledged = acknowledge_startup()
        except Exception:
            acknowledged = False
        if expected_ack and not acknowledged:
            app.exit(1)
            return
        shell.start(background=background)
        if pane is not None:
            shell.show_settings(pane)

    QTimer.singleShot(0, start_resident)
    try:
        return app.exec()
    finally:
        shell.stop()
