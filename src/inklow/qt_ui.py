"""Native-theme Windows/Linux menu agent and preferences, never a text editor.

The controller owns all typing, storage, downloads and background processes. Qt
objects are created and updated exclusively on the application thread.
"""
from __future__ import annotations

from copy import deepcopy
from dataclasses import replace
from pathlib import Path
import sys
from urllib.parse import urlsplit

from PySide6.QtCore import QEvent, QPoint, QSize, Qt, QTimer, Signal
from PySide6.QtGui import QAction, QColor, QCursor, QKeySequence, QPalette
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
from .resources import configure_identity, tray_icon
from .runtime import Controller
from .shortcuts import normalize_shortcut, shortcut_from_event

PANES = (
    "Setup", "General", "Context", "Personalization", "Emoji", "Shortcuts",
    "Battery", "App Settings", "Labs", "Statistics", "About",
)


def _label(text: str) -> QLabel:
    label = QLabel(text)
    label.setTextFormat(Qt.TextFormat.PlainText)
    label.setWordWrap(True)
    return label


def _error(parent: QWidget, error: Exception | str) -> None:
    QMessageBox.warning(parent, "Inklow could not apply this change", str(error))


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
        self.setWindowTitle("Inklow models")
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

    def _call(self, function, *args) -> None:
        try:
            function(*args)
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
        self.description.setText(model["description"] if model else "No downloadable models are available.")
        self.current.setText(f"Selected model: {self.controller.config.model_path or 'None'}")
        self.download.setEnabled(bool(model) and not active)
        self.use.setEnabled(bool(model and model["installed"]) and not active)
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
        model = self._models[self.models.currentIndex()]
        if QMessageBox.question(self, "Download local model?", f"Download {model['label']} ({_size(model['size_bytes'])})?\nOnly model weights are downloaded.") == QMessageBox.StandardButton.Yes:
            self._call(self.controller.download_model, model["id"])
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
        self.result_rule = {name: control.currentData() for name, control in self.options.items() if control.currentData() is not None}
        if instructions:
            self.result_rule["custom_instructions"] = instructions
        self.accept()


class SettingsWindow(QDialog):
    """An explicit draft/apply lifecycle which never rewrites unrelated fields."""

    def __init__(self, shell: MenuAgent):
        super().__init__()
        self.shell = shell
        self.controller = shell.controller
        self.setWindowTitle("Inklow Settings")
        self.resize(800, 632)
        self.setMinimumSize(710, 510)
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
        self.sidebar = QListWidget()
        self.sidebar.setAccessibleName("Settings sections")
        self.sidebar.addItems(PANES)
        self.sidebar.setFixedWidth(190)
        self.sidebar.setSpacing(3)
        self.sidebar.setFrameShape(QFrame.Shape.NoFrame)
        columns.addWidget(self.sidebar)
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
        close = QPushButton("Close")
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
        self.reload()
        self.sidebar.setCurrentRow(0 if not self.controller.config.setup_completed else 1)

    def _group(self, layout: QVBoxLayout, title: str) -> QFormLayout:
        group = QGroupBox(title)
        form = QFormLayout(group)
        form.setRowWrapPolicy(QFormLayout.RowWrapPolicy.WrapLongRows)
        form.setFieldGrowthPolicy(QFormLayout.FieldGrowthPolicy.ExpandingFieldsGrow)
        form.setVerticalSpacing(12)
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
        layout.addWidget(_label("Inklow offers completions in supported applications from the system tray. It does not contain a writing editor. Suggestions and optional context stay on this computer."))
        form = self._group(layout, "System access")
        self.native_diagnostic = _label("")
        form.addRow(self.native_diagnostic)
        permissions = QPushButton("Open accessibility / input permissions")
        permissions.clicked.connect(lambda: self._call(system.open_permissions, "accessibility"))
        form.addRow(permissions)
        form.addRow(_label("Windows uses accessible edit controls and the native keyboard hook. Linux/X11 uses AT-SPI and replayable key grabs. KDE Wayland uses an explicitly selected native input method, not raw keyboard-device access. Only eligible, unprotected fields are used for predictions."))
        if sys.platform.startswith("linux"):
            form.addRow(_label("KDE Wayland: selecting Inklow as the input method lets the OS send it keyboard events and surrounding text from supported text-input controls. Password/sensitive hints are discarded; excluded or mismatched AT-SPI fields are not used. KDE places the popup near the caret. Other input methods cannot occupy the same KDE input-method slot. No selection or permission changes happen automatically."))
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
        form.addRow(_label("Accept the next word with Tab; accept the full completion with the physical key above Tab. Native/system shortcuts are never replaced when no eligible suggestion is active."))

    def _install_wayland_method(self) -> None:
        try:
            path = system.install_wayland_input_method()
        except (OSError, RuntimeError, ValueError) as exc:
            _error(self, exc)
            return
        QMessageBox.information(
            self, "KDE input-method entry installed",
            f"Installed {path}\n\nQuit Inklow using its menu, then choose Inklow in KDE System Settings → Keyboard → Virtual Keyboard. KDE will start the native input-method instance. The selected input method was not changed.")

    def _general(self, layout) -> None:
        form = self._group(layout, "Application")
        self.login = QCheckBox("Launch automatically at login")
        self.login.setToolTip("Applied immediately, only when you explicitly change this switch.")
        self.login.toggled.connect(self._login_changed)
        form.addRow(self.login)
        self.login_status = _label("")
        form.addRow(self.login_status)
        form.addRow(_label("The system tray icon remains available while Inklow is running. Closing settings hides this window; Quit in the menu stops Inklow."))
        self._check(form, "show_accessory", "Show accessory button beside suggestions")
        form = self._group(layout, "AI settings")
        self.model_label = _label("")
        form.addRow("Selected model", self.model_label)
        models = QPushButton("Manage local models…")
        models.clicked.connect(self.open_models)
        form.addRow(models)
        reveal = QPushButton("Reveal model files")
        reveal.clicked.connect(lambda: self._call(system.reveal_models))
        form.addRow(reveal)
        self._combo(form, "prompt_style", "Model prompt style", (("Text continuation", "continuation"), ("Instruction-tuned writing", "writing")))
        form = self._group(layout, "Completion settings")
        self._check(form, "enabled", "Enable completions by default", "Application rules can override this default.")
        self._check(form, "global_enabled", "Enable global typing assistance", "Master switch: when off, no application rule can enable assistance. The tray menu toggles this switch.")
        self._check(form, "complete_inside_text", "Complete inside existing text")
        self._combo(form, "completion_length", "Maximum completion length", (("Short", "short"), ("Medium", "medium"), ("Long", "long")))

    def _context(self, layout) -> None:
        layout.addWidget(_label("Additional context is optional. It is read only for an eligible completion, never saved as an image or uploaded. Leave these switches off when working with sensitive content."))
        form = self._group(layout, "Optional local context")
        clipboard = self._check(form, "clipboard_context", "Include clipboard text")
        screen = self._check(form, "screen_context", "Include foreground screen text (macOS only)", "Foreground-window local OCR is available only in the native macOS application. Windows and Linux can use clipboard and surrounding typed context.")
        screen.setEnabled(False)
        clipboard.toggled.connect(lambda value: self._context_consent("clipboard_context", value))
        screen.toggled.connect(lambda value: self._context_consent("screen_context", value))
        permission = QPushButton("Open screen capture permissions")
        permission.clicked.connect(lambda: self._call(system.open_permissions, "screen_recording"))
        permission.setEnabled(False)
        permission.setToolTip("Screen OCR is available only in the native macOS application.")
        form.addRow(permission)
        self._spin(form, "context_chars", "Maximum typed context", 64, 8000, " characters")

    def _personalization(self, layout) -> None:
        form = self._group(layout, "Custom instructions")
        form.addRow(_label("Describe your preferred tone, language or vocabulary. These are instructions for local inference, not a place to draft documents."))
        self.instructions = QPlainTextEdit()
        self.instructions.setMaximumHeight(150)
        self.instructions.setPlaceholderText("For example: concise sentences; British English.")
        form.addRow(self.instructions)
        self._bind("custom_instructions", self.instructions, self.instructions.toPlainText, self.instructions.setPlainText, self.instructions.textChanged)
        form = self._group(layout, "Encrypted local personalization")
        self.history = QCheckBox("Record personalization history by default")
        self.history.setToolTip("Applied immediately after explicit consent and secure-storage validation.")
        self.history.toggled.connect(self._history_changed)
        form.addRow(self.history)
        form.addRow(_label("History is off by default and requires OS-backed encrypted storage; no plaintext fallback. App recording overrides take precedence: explicitly enabled apps continue recording when this default is off."))
        self._check(form, "record_unaccepted", "Also retain unaccepted typing", "Only effective while encrypted history recording is enabled. This increases sensitive data stored locally.")
        self._spin(form, "personalization_strength", "Personalization strength", 0, 100, "%")
        self.history_status = _label("")
        form.addRow(self.history_status)
        delete = QPushButton("Delete all personalization history…")
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
        form.addRow(_label("Click a shortcut, then press its real key combination. The physical key above Tab works across keyboard layouts. Escape cancels recording; Backspace clears an assignment. Operating-system and desktop-reserved shortcuts remain system-owned and may not be available to Inklow."))
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
        self._check(form, "battery_smaller_model", "Use a smaller installed local model", "An alternative model must already be installed; battery mode never downloads one automatically.")
        self._check(form, "battery_shorter", "Prefer shorter completions")
        self.battery_status = _label("")
        form.addRow(self.battery_status)
        form = self._group(layout, "Memory usage")
        timeout = self._spin(form, "idle_release_seconds", "Release model after inactivity", 0, 3600, " seconds")
        timeout.setSpecialValueText("Keep model ready")
        form.addRow(_label("The inference process starts only when needed. Pausing or disabling Inklow releases it immediately. Set a nonzero timeout to reclaim all model memory when idle."))

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
        form = self._group(layout, "Local inference configuration")
        self._combo(form, "backend", "Backend", (("Local GGUF process", "gguf"), ("Local Ollama server", "ollama"), ("Local OpenAI-compatible server", "openai")))
        self._text(form, "endpoint", "Loopback server URL")
        self._text(form, "model", "Local server model name")
        self._combo(form, "acceleration", "Acceleration", (("Automatic", "auto"), ("CPU only", "cpu")))
        self._spin(form, "debounce_ms", "Typing pause before completion", 100, 5000, " ms")
        self._spin(form, "min_chars", "Minimum typed characters", 1, 1000)
        self._spin(form, "max_tokens", "Maximum generated tokens", 1, 128)
        form.addRow(_label("Server backends accept literal loopback URLs only. Inklow does not upload writing to a remote API. Model residency for externally managed servers is controlled by that server."))

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
        form = self._group(layout, "Inklow")
        form.addRow("Version", _label(__version__))
        form.addRow(_label("An independent local typing assistant. No subscription, account, cloud sync, billing imitation, or writing editor. Model downloads require a network connection; local GGUF inference does not."))
        form.addRow(_label("Some applications do not expose editable text or caret geometry through native accessibility. In those applications Inklow remains inactive rather than reading or inserting unsafely."))
        settings = QPushButton("Open settings folder")
        settings.clicked.connect(self._reveal_settings)
        form.addRow(settings)
        quit_button = QPushButton("Quit Inklow")
        quit_button.clicked.connect(self.shell.quit)
        form.addRow(quit_button)

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
        if 0 <= index < len(PANES) and PANES[index] == "Statistics":
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
        if QMessageBox.question(self, "Allow optional local context?", f"Allow Inklow to read {noun} for eligible completion requests?\nThis may contain sensitive information. The preference takes effect only after Save.") != QMessageBox.StandardButton.Yes:
            self._bindings[field][0].setChecked(False)

    def _history_changed(self, enabled: bool) -> None:
        if self._loading:
            return
        old = self.controller.config.record_history
        consent = not enabled or QMessageBox.question(self, "Enable encrypted personalization history?", "Store typing history locally in encrypted form for personalization?\nThis requires secure OS secret storage. No plaintext fallback is permitted. Existing records are not deleted when recording is disabled.") == QMessageBox.StandardButton.Yes
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
        self.notice.setText(self.shell.tray_notice or message)
        self.native_diagnostic.setText(str(status.get("native_status", "Native bridge status unavailable.")))
        model = self.controller.config.model_path or "None selected — choose or download a model."
        self.model_label.setText(model)
        self.setup_model.setText(f"Selected model: {model}")
        self.current_app.setText(f"Current application: {status.get('current_app') or 'No eligible application'}\nSession completions: {'Enabled' if status.get('current_app_enabled', self.controller.config.enabled) else 'Disabled'}\nObservable website: {status.get('current_domain') or 'Unknown / not available'}")
        self.battery_status.setText("Currently on battery power." if status.get("on_battery") else "System is not reporting battery power.")
        try:
            info = self.controller.history_info()
            self.history_status.setText(f"{info['count']:,} encrypted records · {info['bytes']:,} bytes" if info["available"] else f"Secure history unavailable: {info.get('error') or 'OS secret storage is not available.'}")
        except Exception as exc:
            self.history_status.setText(f"Secure history unavailable: {exc}")
        try:
            state = system.launch_at_login_state()
            self.login_status.setText(f"System login registration: {state}")
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

    def closeEvent(self, event) -> None:
        if self._changes() and not self._closing:
            choice = QMessageBox.question(self, "Save settings changes?", "Save your settings changes before closing?", QMessageBox.StandardButton.Save | QMessageBox.StandardButton.Discard | QMessageBox.StandardButton.Cancel, QMessageBox.StandardButton.Save)
            if choice == QMessageBox.StandardButton.Cancel or (choice == QMessageBox.StandardButton.Save and not self.apply()):
                event.ignore()
                return
            if choice == QMessageBox.StandardButton.Discard:
                self.reload()
        if self.shell.tray_available and not self._closing:
            self.hide()
            event.ignore()
            return
        if not self._closing:
            # Without a tray, closing is explicitly a Quit rather than making
            # the resident agent inaccessible. Cancel always leaves recovery UI.
            if QMessageBox.question(self, "Quit Inklow?", "No system tray is available. Quit Inklow and stop typing assistance?", QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No, QMessageBox.StandardButton.No) != QMessageBox.StandardButton.Yes:
                event.ignore()
                return
            self.shell.quit()
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
        self.setAccessibleName("Inklow completion suggestion")
        layout = QVBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        self.text = QLabel()
        self.text.setTextFormat(Qt.TextFormat.PlainText)
        self.text.setFocusPolicy(Qt.FocusPolicy.NoFocus)
        self.text.setMaximumWidth(640)
        self.text.setWordWrap(True)
        palette = self.text.palette()
        palette.setColor(QPalette.ColorRole.WindowText, QColor("#757575"))
        self.text.setPalette(palette)
        layout.addWidget(self.text)

    def present(self, event: dict, point: QPoint) -> None:
        choices = event.get("choices") or []
        if choices and event.get("mode") != "completion":
            text = "\n".join(f"{index}. {choice}" for index, choice in enumerate(choices[:9], 1))
        else:
            text = str(event.get("text", ""))
        self.text.setText(text)
        self.adjustSize()
        screen = QApplication.screenAt(point)
        if screen:
            bounds = screen.availableGeometry()
            point.setX(max(bounds.left(), min(point.x(), bounds.right() - self.width() + 1)))
            point.setY(max(bounds.top(), min(point.y(), bounds.bottom() - self.height() + 1)))
        self.move(point)
        self.show()


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
        self.button.setIconSize(QSize(18, 18))
        self.button.setFixedSize(24, 24)
        self.button.setToolButtonStyle(Qt.ToolButtonStyle.ToolButtonIconOnly)
        self.button.setStyleSheet("QToolButton { border: none; padding: 3px; }")
        self.button.setToolTip("Inklow actions and choices")
        self.button.setAccessibleName("Inklow accessory menu")
        self.button.clicked.connect(self._menu)
        layout.addWidget(self.button)
        self.menu: QMenu | None = None

    def _menu(self) -> None:
        if self.menu:
            if self.menu.isVisible():
                return
            self.menu.close()
            self.menu.deleteLater()
        self.menu = self.shell.create_menu(choices=True)
        self.menu.setWindowFlag(Qt.WindowType.WindowDoesNotAcceptFocus, True)
        self.menu.setAttribute(Qt.WidgetAttribute.WA_ShowWithoutActivating)
        self.menu.setFocusPolicy(Qt.FocusPolicy.NoFocus)
        self.menu.popup(self.mapToGlobal(QPoint(0, self.height())))


class MenuAgent:
    def __init__(self, app: QApplication, config: Config, *, no_tray: bool = False):
        self.app = app
        self.controller = Controller(config)
        self.controller.show_settings = self.show_settings
        self.controller.set_clipboard_reader(system.clipboard_text)
        self.controller.set_screen_reader(system.screen_text)
        self.controller.set_battery_reader(system.on_battery)
        self._tray_requested = not no_tray
        self.tray_available = not no_tray and QSystemTrayIcon.isSystemTrayAvailable()
        self.tray_notice = "" if self.tray_available else (
            "System tray disabled. Keep settings open to control Inklow; closing offers Quit." if no_tray else
            "No system tray is available. Keep settings open, or enable your desktop's tray extension; Inklow reconnects automatically. Closing offers Quit.")
        self.settings: SettingsWindow | None = None
        self.overlay = CaretOverlay()
        self.accessory = Accessory(self)
        self.choices: list[str] = []
        self._stopped = False
        self._ticks = 0
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
        self.tray.setToolTip("Inklow")
        self.menu = self.create_menu()
        self.tray.setContextMenu(self.menu)
        self.menu.aboutToShow.connect(self.refresh_menu)
        self.tray.activated.connect(self._activated)
        if self._tray_requested:
            self.tray.show()
        self.timer = QTimer()
        self.timer.setInterval(100)
        self.timer.timeout.connect(self.tick)
        self.app.aboutToQuit.connect(self.stop)
        if self._native_surface:
            self.controller.bridge.bind_ui(self._native_action)

    def start(self, *, background: bool = False) -> None:
        try:
            self.controller.start()
        except Exception as exc:
            # Native backend failure must leave a recoverable settings surface.
            self.tray_notice = f"Global typing could not start: {exc}. Review Setup and system permissions."
            self.show_settings("Setup")
        self.timer.start()
        # background means no unnecessary foreground activation, never bypassing
        # first-run setup or hiding all controls when the tray is unavailable.
        if not self.controller.config.setup_completed or not self.tray_available:
            self.show_settings("Setup" if not self.controller.config.setup_completed else "General")

    def create_menu(self, *, choices: bool = False) -> QMenu:
        menu = QMenu()
        self._watch_menu(menu)
        status = menu.addAction("Inklow")
        status.setEnabled(False)
        status.setObjectName("status")
        enabled = menu.addAction("Enable Inklow")
        enabled.setCheckable(True)
        enabled.setChecked(bool(self.controller.config.global_enabled))
        enabled.setObjectName("enabled")
        enabled.triggered.connect(lambda _checked: self._invoke("toggle"))
        pause = menu.addMenu("Pause")
        self._watch_menu(pause)
        for minutes in (5, 15, 60):
            pause.addAction(f"{minutes} minutes", lambda _checked=False, minutes=minutes: self._menu_action(self.controller.pause, minutes))
        app_toggle = menu.addAction("Enable in current application (this session)")
        app_toggle.setObjectName("app_toggle")
        app_toggle.setCheckable(True)
        current = self.controller.status()
        app_toggle.setChecked(bool(current.get("current_app_enabled", self.controller.config.enabled)))
        app_toggle.setEnabled(bool(current.get("current_app")))
        app_toggle.triggered.connect(lambda _checked: self._invoke("app_toggle"))
        menu.addSeparator()
        if choices:
            for index, choice in enumerate(self.choices[:9], 1):
                action = QAction(f"{index}. {choice}".replace("&", "&&"), menu)
                action.setObjectName(f"choice:{index}")
                action.triggered.connect(lambda _checked=False, index=index: self._invoke(f"choice:{index}"))
                menu.addAction(action)
            menu.addAction("Accept next word", lambda: self._invoke("word")).setObjectName("word")
            menu.addAction("Accept full completion", lambda: self._invoke("all")).setObjectName("all")
            menu.addAction("Dismiss suggestion", lambda: self._invoke("dismiss")).setObjectName("dismiss")
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
        menu.addAction("Statistics…", lambda: self._menu_action(self.show_settings, "Statistics"))
        menu.addAction("Models…", lambda: self._menu_action(self.open_models))
        menu.addSeparator()
        menu.addAction("Quit Inklow", lambda: self._menu_action(self.quit)).setObjectName("quit")
        return menu

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
            menu = self.create_menu(choices=True)
            self.refresh_menu(menu)
            self._native_menu = menu
            self._native_actions = {}
            rows = []

            def collect(current: QMenu, prefix: str = "") -> None:
                for action in current.actions():
                    if action.isSeparator():
                        continue
                    title = action.text().replace("&&", "\0").replace("&", "").replace("\0", "&")
                    if action.isCheckable():
                        title = ("✓ " if action.isChecked() else "") + title
                    if action.menu():
                        collect(action.menu(), prefix + title + " → ")
                        continue
                    key = f"action:{len(self._native_actions)}"
                    self._native_actions[key] = action
                    rows.append((key, prefix + title, action.isEnabled()))

            collect(menu)
            self._native_menu_active = True
            self._sync_shortcuts()
            if not self.controller.bridge.show_native_menu(rows):
                self._native_action("menu_closed")
            return
        action = self._native_actions.get(identifier)
        menu, self._native_menu = self._native_menu, None
        self._native_actions = {}
        self._native_menu_active = False
        self._sync_shortcuts()
        if identifier != "menu_closed":
            self.controller.bridge.hide_native()
        if action is not None and action.isEnabled():
            self._call(action.trigger)
        if menu is not None:
            menu.deleteLater()
        self.tick()
        if self._last_overlay:
            self._suggestion(self._last_overlay)


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
        recording = self.settings and any(
            recorder._recording for recorder in self.settings.shortcut_controls.values())
        self.controller.bridge.set_shortcuts_enabled(
            not self._stopped and not self._menu_session and not self._native_menu_active and not recording)
        if recording and self._native_surface:
            self.controller.bridge.hide_native()

    def _menu_action(self, function, *args) -> None:
        if self._menu_session:
            self._deferred_actions.append((function, args))
        else:
            self._call(function, *args)

    def _finish_menu_session(self) -> None:
        if self._tracking_menus or not self._menu_session or self._stopped:
            return
        actions, self._deferred_actions = self._deferred_actions, []
        for function, args in actions:
            # Controller.invoke performs the normal fresh foreground/snapshot
            # checks after the popup releases focus; never insert from cached UI.
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
            self.show_settings("Setup")
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
        state = self.controller.status()
        for action in (menu or self.menu).actions():
            if action.objectName() == "status":
                action.setText(f"Inklow — {state.get('state', 'idle')}")
            elif action.objectName() == "enabled":
                action.setChecked(bool(state.get("enabled")))
                action.setText("Resume Inklow" if state.get("state") == "paused" else "Enable Inklow")
            elif action.objectName() == "app_toggle":
                app_name = str(state.get("current_app") or "current application")
                action.setText(f"Enable in {app_name[-70:]} (this session)".replace("&", "&&"))
                action.setChecked(bool(state.get("current_app_enabled", self.controller.config.enabled)))
                action.setEnabled(bool(state.get("current_app")))

    def show_settings(self, pane: str = "General") -> None:
        if self.settings is None:
            self.settings = SettingsWindow(self)
        self.settings.refresh_status()
        self.settings.show_pane(pane if isinstance(pane, str) else "General")

    def open_models(self) -> None:
        self.show_settings("General")
        self.settings.open_models()

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
        if self.settings and (self.settings.isActiveWindow() or (self.settings.model_manager and self.settings.model_manager.isActiveWindow())):
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
        point, height = self._position(rect)
        if event.get("mode") != "completion" or not event.get("inline", True):
            point.setY(point.y() + height + 4)
        self.overlay.present(event, point)
        self._caret(event)

    def _caret(self, event: dict) -> None:
        if self._menu_session:
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

    def tick(self) -> None:
        if self._stopped or self._menu_session or self._native_menu_active:
            return
        try:
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
            if self._ticks % 10 == 0:
                status = self.controller.status()
                self.tray.setIcon(tray_icon(paused=not bool(status.get("enabled")) or status.get("state") == "paused"))
                self.tray.setToolTip(f"Inklow — {status.get('message') or status.get('state', 'idle')}")
                if self.settings and self.settings.isVisible():
                    self.settings.refresh_status()
                    if self.settings.sidebar.currentItem() and self.settings.sidebar.currentItem().text() == "Statistics":
                        self.settings.refresh_stats()
                if not self.controller.config.show_accessory:
                    self.accessory.hide()
                if self._tray_requested:
                    available = QSystemTrayIcon.isSystemTrayAvailable()
                    if self.tray_available and not available:
                        self.tray_notice = "The system tray became unavailable. Settings remain open so Inklow stays controllable."
                        self.show_settings("General")
                    elif available and not self.tray_available:
                        self.tray_notice = ""
                    self.tray_available = available
                    if not self.tray.isVisible():
                        self.tray.show()
        except Exception as exc:
            self.clear_overlay()
            self.tray_notice = f"Typing assistance error: {exc}. Review Setup; you can still change settings or Quit."
            self.show_settings("Setup")

    def stop(self) -> None:
        if self._stopped:
            return
        self._stopped = True
        self.timer.stop()
        self._deferred_actions.clear()
        self._native_menu_active = False
        self._native_actions.clear()
        if self._native_menu:
            self._native_menu.deleteLater()
            self._native_menu = None
        self._menu_session = False
        for menu in tuple(self._tracking_menus):
            menu.close()
        self._tracking_menus.clear()
        self.menu.close()
        if self.accessory.menu:
            self.accessory.menu.close()
        self.clear_overlay()
        self.tray.hide()
        self.controller.stop()
        self._hide_accessory_after_menu = False

    def quit(self) -> None:
        if self.settings and self.settings._changes():
            choice = QMessageBox.question(self.settings, "Save before quitting?", "Save your settings changes before quitting Inklow?", QMessageBox.StandardButton.Save | QMessageBox.StandardButton.Discard | QMessageBox.StandardButton.Cancel, QMessageBox.StandardButton.Save)
            if choice == QMessageBox.StandardButton.Cancel or (choice == QMessageBox.StandardButton.Save and not self.settings.apply()):
                return
        try:
            self.stop()
        except Exception as exc:
            self.show_settings("Setup")
            _error(self.settings, f"Could not stop cleanly: {exc}")
            return
        if self.settings:
            self.settings._closing = True
        self.app.quit()


def launch(config: Config, *, background: bool = False, no_tray: bool = False, pane: str | None = None) -> int:
    """Run the portable resident utility using the operating system's Qt theme."""
    app = QApplication.instance() or QApplication(sys.argv[:1])
    app.setQuitOnLastWindowClosed(False)
    configure_identity(app)
    shell = MenuAgent(app, config, no_tray=no_tray)
    # Retain ownership independently of Python's local frame for embedded smoke
    # entrypoints and application shutdown callbacks.
    app._inklow_menu_agent = shell
    shell.start(background=background)
    if pane is not None:
        shell.show_settings(pane)
    return app.exec()
