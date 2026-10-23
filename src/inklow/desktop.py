"""Native local-first editor, settings, and guarded system-wide predictions."""
from __future__ import annotations

from dataclasses import replace
import sys
from threading import RLock
import time

from PySide6.QtCore import QSignalBlocker, QTimer, Qt, Signal, Slot
from PySide6.QtGui import QAction, QColor, QKeySequence, QPalette
from PySide6.QtWidgets import (
    QApplication, QCheckBox, QComboBox, QDialog, QDialogButtonBox, QFileDialog,
    QFormLayout, QHBoxLayout, QLabel, QLineEdit, QMainWindow, QMessageBox,
    QPlainTextEdit, QPushButton, QSpinBox, QStatusBar, QVBoxLayout, QWidget,
)

from .config import Config, config_path, save_config
from .core import Snapshot, SuggestionState
from .ui_editor import INK, SuggestionOverlay, WritingEditor
from .ui_worker import InferenceWorker, PredictionRequest


def _plain_label(text: str = "", parent=None) -> QLabel:
    label = QLabel(text, parent)
    label.setTextFormat(Qt.TextFormat.PlainText)
    label.setWordWrap(True)
    return label


def _provider_key(config: Config) -> tuple:
    return (config.backend, config.model_path, config.endpoint, config.model,
            config.max_tokens, config.context_chars)


class SettingsDialog(QDialog):
    def __init__(self, config: Config, parent=None):
        super().__init__(parent)
        self.setWindowTitle("Inklow settings")
        self.setMinimumWidth(580)
        self._original = config
        self.saved_config: Config | None = None
        layout = QVBoxLayout(self)
        layout.setSpacing(14)
        intro = _plain_label("Choose where predictions run. Writing is never saved or sent to an external server.")
        layout.addWidget(intro)
        form = QFormLayout()
        form.setFieldGrowthPolicy(QFormLayout.FieldGrowthPolicy.ExpandingFieldsGrow)
        self.backend = QComboBox()
        self.backend.addItem("GGUF file on this computer", "gguf")
        self.backend.addItem("Ollama on this computer", "ollama")
        self.backend.addItem("OpenAI-compatible local server", "openai")
        self.backend.setCurrentIndex(max(0, self.backend.findData(config.backend)))
        form.addRow("Prediction engine", self.backend)
        self.model_path = QLineEdit(config.model_path)
        self.model_path.setPlaceholderText("Select a base-model .gguf file")
        self.model_path.setAccessibleName("GGUF model file")
        browse = QPushButton("Browse…")
        browse.clicked.connect(self._browse)
        model_row = QWidget()
        model_layout = QHBoxLayout(model_row)
        model_layout.setContentsMargins(0, 0, 0, 0)
        model_layout.addWidget(self.model_path, 1)
        model_layout.addWidget(browse)
        self._model_row = model_row
        form.addRow("GGUF model", model_row)
        self.endpoint = QLineEdit(config.endpoint)
        self.endpoint.setPlaceholderText("http://127.0.0.1:11434")
        form.addRow("Local server URL", self.endpoint)
        self.model = QLineEdit(config.model)
        self.model.setPlaceholderText("Model name loaded by your local server")
        form.addRow("Server model", self.model)
        self.debounce = QSpinBox()
        self.debounce.setRange(100, 5000)
        self.debounce.setSingleStep(50)
        self.debounce.setSuffix(" ms")
        self.debounce.setValue(config.debounce_ms)
        form.addRow("Pause before suggesting", self.debounce)
        self.minimum = QSpinBox()
        self.minimum.setRange(1, 1000)
        self.minimum.setValue(config.min_chars)
        form.addRow("Minimum typed characters", self.minimum)
        self.tokens = QSpinBox()
        self.tokens.setRange(1, 128)
        self.tokens.setValue(config.max_tokens)
        form.addRow("Maximum prediction tokens", self.tokens)
        self.enabled = QCheckBox("Enable predictions")
        self.enabled.setChecked(config.enabled)
        form.addRow(self.enabled)
        self.global_enabled = QCheckBox("Also suggest in supported applications")
        self.global_enabled.setChecked(config.global_enabled)
        form.addRow(self.global_enabled)
        self.exclusions = QPlainTextEdit()
        self.exclusions.setAccessibleName("Excluded application identifiers, one per line")
        self.exclusions.setPlainText("\n".join(config.excluded_apps))
        self.exclusions.setMaximumHeight(115)
        form.addRow("Excluded applications\n(one identifier per line)", self.exclusions)
        layout.addLayout(form)
        self.backend_hint = _plain_label()
        layout.addWidget(self.backend_hint)
        layout.addWidget(_plain_label(
            "System-wide mode requires Accessibility permission on macOS, supported UI Automation controls on Windows, "
            "or AT-SPI on Linux/X11. Wayland supports this editor only. Password fields and unknown controls are skipped. "
            "Keep terminals and password managers excluded."))
        path_label = _plain_label(f"Only these settings are saved: {config_path()}")
        path_label.setTextInteractionFlags(Qt.TextInteractionFlag.TextSelectableByMouse)
        layout.addWidget(path_label)
        self.error = _plain_label()
        self.error.setStyleSheet("color: #A32929;")
        self.error.hide()
        layout.addWidget(self.error)
        buttons = QDialogButtonBox(QDialogButtonBox.StandardButton.Save | QDialogButtonBox.StandardButton.Cancel)
        buttons.accepted.connect(self._save)
        buttons.rejected.connect(self.reject)
        layout.addWidget(buttons)
        self.backend.currentIndexChanged.connect(self._backend_changed)
        self._backend_changed()

    def _browse(self) -> None:
        path, _ = QFileDialog.getOpenFileName(self, "Choose a local GGUF model", self.model_path.text(), "GGUF models (*.gguf);;All files (*)")
        if path:
            self.model_path.setText(path)

    def _backend_changed(self, *args) -> None:
        local_file = self.backend.currentData() == "gguf"
        self._model_row.setEnabled(local_file)
        self.endpoint.setEnabled(not local_file)
        self.model.setEnabled(not local_file)
        self.backend_hint.setText(
            "Use a base text-completion model, not a chat-only model. The first prediction loads it and may take several seconds."
            if local_file else
            "Start your local server and load its model first. Use a literal loopback address (127.0.0.1 or ::1), not localhost or a remote host."
        )

    def _save(self) -> None:
        candidate = replace(
            self._original,
            backend=self.backend.currentData(), model_path=self.model_path.text().strip(),
            endpoint=self.endpoint.text().strip(), model=self.model.text().strip(),
            debounce_ms=self.debounce.value(), min_chars=self.minimum.value(),
            max_tokens=self.tokens.value(), enabled=self.enabled.isChecked(),
            global_enabled=self.global_enabled.isChecked(),
            excluded_apps=tuple(dict.fromkeys(line.strip() for line in self.exclusions.toPlainText().splitlines() if line.strip())),
        )
        try:
            candidate.validate()
            save_config(candidate)
        except (ValueError, OSError) as exc:
            self.error.setText(f"Settings were not applied: {exc}")
            self.error.show()
            return
        self.saved_config = candidate
        self.accept()


class InklowWindow(QMainWindow):
    # Keyboard hooks may emit these, but never inspect or change a Qt widget.
    native_action = Signal()

    def __init__(self, config: Config):
        super().__init__()
        # Use one explicit light palette; mixing a light writing surface with
        # native dark-mode button text makes the controls unreadable.
        app = QApplication.instance()
        app.setStyle("Fusion")
        palette = QPalette()
        for role, color in (
            (QPalette.ColorRole.Window, "#F5F6F8"),
            (QPalette.ColorRole.WindowText, "#202A35"),
            (QPalette.ColorRole.Base, "#FFFFFF"),
            (QPalette.ColorRole.AlternateBase, "#F5F6F8"),
            (QPalette.ColorRole.Text, "#202A35"),
            (QPalette.ColorRole.Button, "#F0F2F5"),
            (QPalette.ColorRole.ButtonText, "#202A35"),
            (QPalette.ColorRole.Highlight, INK),
            (QPalette.ColorRole.HighlightedText, "#FFFFFF"),
            (QPalette.ColorRole.PlaceholderText, "#657184"),
        ):
            palette.setColor(role, QColor(color))
        app.setPalette(palette)
        self.setWindowTitle("Inklow — Untitled")
        self.resize(1040, 780)
        self.setMinimumSize(640, 440)
        self.config = replace(config)
        self._lock = RLock()
        self._state = SuggestionState()
        self._candidate: Snapshot | None = None
        self._channel = ""
        self._revision = 1
        self._ready = False
        self._provider_error = ""
        self._bridge_error = ""
        self._bridge = None
        self._hook_active = False
        self._paused_until = 0.0
        self._settings_open = False
        self._accepting = False
        self._closing = False
        self._busy_generation: int | None = None
        self._overlay = SuggestionOverlay()
        self._build_ui()
        self._debounce = QTimer(self)
        self._debounce.setSingleShot(True)
        self._debounce.timeout.connect(self._request_prediction)
        self._poll = QTimer(self)
        self._poll.setInterval(150)
        self._poll.timeout.connect(self._poll_global)
        self._pause_timer = QTimer(self)
        self._pause_timer.setSingleShot(True)
        self._pause_timer.timeout.connect(self._resume_after_pause)
        self._worker = InferenceWorker(self)
        self._worker.provider_ready.connect(self._on_provider_ready, Qt.ConnectionType.QueuedConnection)
        self._worker.result_ready.connect(self._on_result, Qt.ConnectionType.QueuedConnection)
        self._worker.finished.connect(self._on_worker_finished)
        self.native_action.connect(self._on_native_action, Qt.ConnectionType.QueuedConnection)
        QApplication.instance().applicationStateChanged.connect(self._application_state_changed)
        self.editor.activity.connect(self._editor_activity)
        self.editor.focus_changed.connect(self._editor_focus)
        self.editor.composition_changed.connect(self._editor_composition)
        self.editor.accept_suggestion = self._accept_editor
        self.editor.dismiss_suggestion = self._dismiss_editor
        self._worker.configure(replace(self.config), self._revision)
        self._worker.start()
        self._configure_bridge()
        self._poll.start()
        self._refresh_status()

    def _build_ui(self) -> None:
        self.setStyleSheet(f"""
            QMainWindow {{ background: #F5F6F8; color: #202A35; }}
            QPlainTextEdit#writingEditor {{ background: white; color: #202A35; border: 1px solid #D6DBE3; selection-background-color: #D7E6FC; selection-color: #182D4B; }}
            QLabel#appName {{ color: {INK}; font-size: 24px; font-weight: 600; }}
            QLabel#shortcutHint {{ color: #526379; }}
            QPushButton {{ padding: 5px 12px; }}
            QPushButton:focus {{ border: 2px solid {INK}; }}
        """)
        central = QWidget()
        layout = QVBoxLayout(central)
        layout.setContentsMargins(24, 16, 24, 10)
        layout.setSpacing(12)
        header = QHBoxLayout()
        title = QLabel("Inklow")
        title.setObjectName("appName")
        header.addWidget(title)
        subtitle = _plain_label("Local predictive writing")
        subtitle.setStyleSheet("color: #526379;")
        header.addWidget(subtitle)
        header.addStretch(1)
        self.enabled_toggle = QCheckBox("Predictions")
        self.enabled_toggle.setChecked(self.config.enabled)
        self.enabled_toggle.setToolTip("Pause or resume predictions without changing your document")
        self.enabled_toggle.toggled.connect(self._toggle_predictions)
        header.addWidget(self.enabled_toggle)
        self.settings_button = QPushButton("Settings…")
        self.settings_button.clicked.connect(self.open_settings)
        header.addWidget(self.settings_button)
        layout.addLayout(header)
        self.notice = _plain_label()
        self.notice.setTextInteractionFlags(Qt.TextInteractionFlag.TextSelectableByMouse)
        self.notice.setStyleSheet("color: #31577F; padding: 2px 0;")
        layout.addWidget(self.notice)
        document_row = QHBoxLayout()
        document_row.addStretch(1)
        self.editor = WritingEditor()
        self.editor.setMaximumWidth(900)
        document_row.addWidget(self.editor, 12)
        document_row.addStretch(1)
        layout.addLayout(document_row, 1)
        hint = _plain_label("Tab  Accept next word     Shift+Tab  Accept all     Esc  Dismiss for 3 seconds")
        hint.setObjectName("shortcutHint")
        layout.addWidget(hint)
        self.setCentralWidget(central)
        self.status = QStatusBar()
        self.setStatusBar(self.status)
        self.status_text = _plain_label()
        self.word_count = QLabel("0 words")
        self.status.addWidget(self.status_text, 1)
        self.status.addPermanentWidget(self.word_count)
        file_menu = self.menuBar().addMenu("&File")
        close_action = QAction("&Close", self)
        close_action.setShortcut(QKeySequence.StandardKey.Close)
        close_action.triggered.connect(self.close)
        file_menu.addAction(close_action)
        edit_menu = self.menuBar().addMenu("&Edit")
        for label, shortcut, callback in (
            ("&Undo", QKeySequence.StandardKey.Undo, self.editor.undo),
            ("&Redo", QKeySequence.StandardKey.Redo, self.editor.redo),
            ("Cu&t", QKeySequence.StandardKey.Cut, self.editor.cut),
            ("&Copy", QKeySequence.StandardKey.Copy, self.editor.copy),
            ("&Paste", QKeySequence.StandardKey.Paste, self.editor.paste),
            ("Select &all", QKeySequence.StandardKey.SelectAll, self.editor.selectAll),
        ):
            action = QAction(label, self)
            action.setShortcut(shortcut)
            action.triggered.connect(callback)
            edit_menu.addAction(action)
        settings_action = QAction("&Settings…", self)
        settings_action.setShortcut(QKeySequence.StandardKey.Preferences)
        settings_action.triggered.connect(self.open_settings)
        edit_menu.addSeparator()
        edit_menu.addAction(settings_action)

    def _editor_is_active(self) -> bool:
        return self.isActiveWindow() and self.editor.hasFocus() and not self.editor.composing

    @Slot(Qt.ApplicationState)
    def _application_state_changed(self, state) -> None:
        if self._closing:
            return
        self._invalidate()
        if state == Qt.ApplicationState.ApplicationActive:
            self._editor_activity()
        else:
            QTimer.singleShot(0, self._poll_global)
        self._refresh_status()

    def _can_predict(self) -> bool:
        return (not self._closing and not self._settings_open and self.config.enabled
                and self._ready and not self._provider_error
                and not (self.config.backend == "gguf" and not self.config.model_path.strip())
                and time.monotonic() >= self._paused_until)

    def _invalidate(self, clear_candidate: bool = True) -> None:
        self._debounce.stop()
        self._worker.clear_pending()
        with self._lock:
            self._state.invalidate()
            self._hook_active = False
            self._busy_generation = None
            if clear_candidate:
                self._candidate = None
                self._channel = ""
        if self._bridge is not None:
            self._bridge.set_shortcuts_enabled(False)
        self.editor.set_ghost("")
        self._overlay.hide()

    def _set_candidate(self, snapshot: Snapshot | None, channel: str) -> None:
        with self._lock:
            old = self._candidate
            unchanged = old is not None and snapshot is not None and old.key == snapshot.key and self._channel == channel
        if unchanged:
            with self._lock:
                self._candidate = snapshot
            self._render_suggestion()
            return
        self._invalidate()
        with self._lock:
            self._candidate = snapshot
            self._channel = channel if snapshot is not None else ""
        if (snapshot is not None and snapshot.eligible(self.config.min_chars)
                and self._can_predict()):
            self._debounce.start(self.config.debounce_ms)
        self._refresh_status()

    @Slot()
    def _editor_activity(self) -> None:
        self.word_count.setText(f"{len(self.editor.toPlainText().split()):,} words")
        if self._accepting or self._closing:
            return
        if self._editor_is_active():
            self._set_candidate(self.editor.snapshot(), "editor")
        elif self._channel == "editor":
            self._invalidate()
            self._refresh_status()

    @Slot(bool)
    def _editor_focus(self, focused: bool) -> None:
        if focused:
            self._editor_activity()
        else:
            self._invalidate()
            self._refresh_status()

    @Slot(bool)
    def _editor_composition(self, composing: bool) -> None:
        self._invalidate()
        if not composing:
            QTimer.singleShot(0, self._editor_activity)

    @Slot()
    def _request_prediction(self) -> None:
        if not self._can_predict():
            return
        with self._lock:
            snapshot = self._candidate
            channel = self._channel
            generation = self._state.generation
        current = self._capture_current(channel)
        if (snapshot is None or current is None or snapshot.key != current.key
                or not current.eligible(self.config.min_chars)):
            self._invalidate()
            return
        with self._lock:
            self._busy_generation = generation
        self._worker.submit(PredictionRequest(generation, self._revision, channel, current))
        self._refresh_status()

    def _capture_current(self, channel: str) -> Snapshot | None:
        if channel == "editor":
            if self._editor_is_active():
                return self.editor.snapshot()
            return None
        if channel == "global" and self._bridge is not None and self.config.global_enabled:
            try:
                snapshot = self._bridge.capture()
            except Exception as exc:
                self._bridge_error = str(exc) or type(exc).__name__
                return None
            if snapshot is None or snapshot.protected or snapshot.caret_rect is None:
                return None
            app_id = snapshot.app_id.casefold()
            if any(excluded.casefold() in app_id for excluded in self.config.excluded_apps):
                return None
            return snapshot
        return None

    @Slot(int, str)
    def _on_provider_ready(self, revision: int, error: str) -> None:
        if self._closing or revision != self._revision:
            return
        self._ready = not error
        self._provider_error = error
        if self._candidate is not None and self._can_predict():
            self._debounce.start(self.config.debounce_ms)
        self._refresh_status()

    @Slot(object, str, str)
    def _on_result(self, request: PredictionRequest, text: str, error: str) -> None:
        with self._lock:
            fresh = (not self._closing and request.revision == self._revision
                     and request.generation == self._state.generation
                     and request.channel == self._channel)
        if not fresh or not self._can_predict():
            return
        current = self._capture_current(request.channel)
        if current is None or current.key != request.snapshot.key:
            self._invalidate()
            self._refresh_status()
            return
        with self._lock:
            self._busy_generation = None
            if error:
                self._provider_error = error
                self._state.invalidate()
            else:
                self._state.offer(request.generation, current, text)
        self._render_suggestion()
        self._refresh_status()

    def _render_suggestion(self) -> None:
        with self._lock:
            text = self._state.text
            snapshot = self._state.snapshot
            channel = self._channel
            candidate = self._candidate
        if (not self._can_predict() or snapshot is None or candidate is None
                or candidate.key != snapshot.key):
            text = ""
        self.editor.set_ghost(text if channel == "editor" else "")
        visible = False
        if channel == "global" and text and candidate is not None and candidate.caret_rect is not None:
            visible = self._overlay.show_suggestion(text, candidate.caret_rect)
        else:
            self._overlay.hide()
        with self._lock:
            self._hook_active = bool(visible and self._state.text and self._state.snapshot is not None
                                     and snapshot is not None and self._state.snapshot.key == snapshot.key)
            hook_active = self._hook_active
        if self._bridge is not None:
            self._bridge.set_shortcuts_enabled(hook_active)

    def _accept_editor(self, full: bool) -> bool:
        if not self._can_predict() or not self._editor_is_active():
            return False
        current = self.editor.snapshot()
        with self._lock:
            if self._channel != "editor":
                return False
            accepted = self._state.accept(current, full=full)
        if not accepted:
            self._invalidate()
            return False
        self._accepting = True
        try:
            cursor = self.editor.textCursor()
            cursor.beginEditBlock()
            cursor.insertText(accepted)
            cursor.endEditBlock()
            self.editor.setTextCursor(cursor)
        finally:
            self._accepting = False
        with self._lock:
            self._candidate = self.editor.snapshot()
            if self._state.snapshot is None or self._candidate.key != self._state.snapshot.key:
                self._state.invalidate()
        self._render_suggestion()
        self._refresh_status()
        return True

    def _dismiss_editor(self) -> bool:
        with self._lock:
            has_suggestion = self._channel == "editor" and bool(self._state.text)
        if not has_suggestion:
            return False
        self._paused_until = time.monotonic() + 3.0
        self._invalidate()
        self._pause_timer.start(3050)
        self._refresh_status()
        return True

    def _native_shortcut(self, action: str) -> bool:
        # This is called by the native keyboard hook. No QWidget, QTimer, or
        # GUI-bound method is touched here; only native APIs and locked state.
        bridge = self._bridge
        if bridge is None or action not in {"word", "all", "dismiss"}:
            return False
        with self._lock:
            if (self._closing or not self._hook_active or self._channel != "global"
                    or not self.config.enabled or not self.config.global_enabled
                    or not self._state.text or time.monotonic() < self._paused_until):
                return False
            try:
                current = bridge.capture()
                expected = self._state.snapshot
                if current is None or expected is None or current.key != expected.key:
                    self._state.invalidate()
                    self._hook_active = False
                    consumed = False
                elif action == "dismiss":
                    self._paused_until = time.monotonic() + 3.0
                    self._state.invalidate()
                    self._hook_active = False
                    consumed = True
                else:
                    accepted = self._state.accept(current, full=action == "all")
                    consumed = bool(accepted and bridge.insert(current, accepted))
                    if not consumed:
                        self._state.invalidate()
                    self._hook_active = False
            except Exception:
                self._state.invalidate()
                self._hook_active = False
                consumed = False
        bridge.set_shortcuts_enabled(False)
        self.native_action.emit()
        return consumed

    @Slot()
    def _on_native_action(self) -> None:
        if self._closing:
            return
        self._overlay.hide()
        if time.monotonic() < self._paused_until:
            self._invalidate()
            self._pause_timer.start(max(1, int((self._paused_until - time.monotonic()) * 1000) + 25))
        else:
            # Insertion may be asynchronous in the target app. Retain remainder
            # only if the next capture exactly matches the expected new text.
            with self._lock:
                expected = self._state.snapshot
            current = self._capture_current("global")
            with self._lock:
                if expected is not None and current is not None and current.key == expected.key:
                    self._candidate = current
                else:
                    self._state.invalidate()
                    self._candidate = None
            self._render_suggestion()
        self._refresh_status()

    @Slot()
    def _resume_after_pause(self) -> None:
        if self._closing:
            return
        if time.monotonic() < self._paused_until:
            self._pause_timer.start(100)
            return
        if self._editor_is_active():
            self._editor_activity()
        else:
            self._poll_global()
        self._refresh_status()

    def _configure_bridge(self) -> None:
        if not self.config.global_enabled:
            self._stop_bridge()
            return
        try:
            if self._bridge is not None:
                self._bridge.set_excluded_apps(tuple(self.config.excluded_apps))
                return
            from .native import create_bridge
            bridge = create_bridge()
            self._bridge = bridge
            bridge.set_excluded_apps(tuple(self.config.excluded_apps))
            bridge.set_shortcuts_enabled(False)
            bridge.start_shortcuts(self._native_shortcut)
            self._bridge_error = ""
        except Exception as exc:
            self._bridge_error = str(exc) or type(exc).__name__
            self._stop_bridge()

    def _stop_bridge(self) -> None:
        bridge = self._bridge
        with self._lock:
            self._hook_active = False
        if bridge is not None:
            try:
                bridge.set_shortcuts_enabled(False)
                bridge.stop()
            except Exception as exc:
                self._bridge_error = str(exc) or type(exc).__name__
            self._bridge = None
        self._overlay.hide()

    @Slot()
    def _poll_global(self) -> None:
        if self._closing or self._settings_open or self.isActiveWindow():
            return
        if not self.config.enabled or not self.config.global_enabled or self._bridge is None:
            if self._channel == "global":
                self._invalidate()
            return
        if time.monotonic() < self._paused_until:
            return
        self._set_candidate(self._capture_current("global"), "global")

    @Slot(bool)
    def _toggle_predictions(self, enabled: bool) -> None:
        candidate = replace(self.config, enabled=enabled)
        try:
            save_config(candidate)
        except (OSError, ValueError) as exc:
            blocker = QSignalBlocker(self.enabled_toggle)
            self.enabled_toggle.setChecked(self.config.enabled)
            del blocker
            QMessageBox.warning(self, "Settings not saved", f"Predictions were not changed.\n\n{exc}")
            return
        self._apply_config(candidate)

    @Slot()
    def open_settings(self) -> None:
        if self._closing or self._settings_open:
            return
        self._settings_open = True
        self._invalidate()
        dialog = SettingsDialog(self.config, self)
        dialog.exec()
        self._settings_open = False
        if dialog.saved_config is not None:
            self._apply_config(dialog.saved_config)
        else:
            self._refresh_status()
        if self.isActiveWindow():
            self.editor.setFocus()
            self._editor_activity()
        dialog.deleteLater()

    def _apply_config(self, config: Config) -> None:
        provider_changed = _provider_key(self.config) != _provider_key(config)
        retry_failed_provider = bool(self._provider_error)
        self._invalidate()
        with self._lock:
            self.config = replace(config)
        blocker = QSignalBlocker(self.enabled_toggle)
        self.enabled_toggle.setChecked(self.config.enabled)
        del blocker
        if provider_changed or retry_failed_provider:
            self._revision += 1
            self._ready = False
            self._provider_error = ""
            self._worker.configure(replace(self.config), self._revision)
        self._configure_bridge()
        if self._editor_is_active():
            self._editor_activity()
        self._refresh_status()

    def _refresh_status(self) -> None:
        if self._closing:
            self.status_text.setText("Finishing the current local request and releasing the model…")
            return
        if not self.config.enabled:
            self.notice.setText("Predictions are paused. Your document remains editable.")
            self.status_text.setText("Paused")
            return
        if self.config.backend == "gguf" and not self.config.model_path.strip():
            self.notice.setText("Choose a base-model GGUF file in Settings, or connect to a local Ollama / completion server. No model is bundled.")
            self.status_text.setText("Model setup needed")
            return
        if self._provider_error:
            self.notice.setText(f"Prediction unavailable: {self._provider_error} Open Settings to correct the setup and save to retry.")
            self.status_text.setText("Model needs attention")
            return
        if self.config.global_enabled:
            if self._bridge_error:
                self.notice.setText(f"System-wide mode unavailable: {self._bridge_error} The built-in editor still works.")
            elif self._bridge is not None:
                try:
                    self.notice.setText(f"System-wide: {self._bridge.status()}")
                except Exception as exc:
                    self.notice.setText(f"System-wide status unavailable: {exc}. The built-in editor still works.")
            else:
                self.notice.setText("System-wide mode is unavailable. The built-in editor still works.")
        else:
            self.notice.setText("Your document is held in memory only. Copy anything you want to keep before closing.")
        with self._lock:
            busy = self._busy_generation == self._state.generation
            has_text = bool(self._state.text)
        if time.monotonic() < self._paused_until:
            message = "Suggestions dismissed for 3 seconds"
        elif self.editor.composing:
            message = "Composing text — suggestions wait for the input method"
        elif not self._ready:
            message = "Preparing the local prediction engine…"
        elif busy:
            message = "Thinking locally… First use may take several seconds."
        elif has_text:
            message = "Suggestion ready"
        else:
            message = f"Ready — type at least {self.config.min_chars} characters, then pause"
        self.status_text.setText(message)

    def _begin_shutdown(self) -> None:
        if self._closing:
            return
        with self._lock:
            self._closing = True
        self._invalidate()
        self._poll.stop()
        self._pause_timer.stop()
        self._stop_bridge()
        self._overlay.close()
        self._worker.request_stop()
        self.editor.setReadOnly(True)
        self.enabled_toggle.setEnabled(False)
        self.settings_button.setEnabled(False)
        self._refresh_status()

    def closeEvent(self, event) -> None:
        self._begin_shutdown()
        if self._worker.isRunning():
            # Keep processing Qt events while an uncancellable local inference
            # finishes. Never terminate a thread inside llama.cpp/native code.
            event.ignore()
        else:
            event.accept()

    @Slot()
    def _on_worker_finished(self) -> None:
        if self._closing:
            self.close()

    @Slot()
    def shutdown_on_quit(self) -> None:
        self._begin_shutdown()
        # aboutToQuit is the final OS/session-exit path, after event processing
        # ends. Join before Qt destroys the worker and its signal receivers.
        self._worker.wait()


def launch(config: Config) -> int:
    app = QApplication.instance() or QApplication(sys.argv)
    app.setApplicationName("Inklow")
    app.setOrganizationName("Inklow")
    window = InklowWindow(config)
    app.aboutToQuit.connect(window.shutdown_on_quit)
    window.show()
    window.editor.setFocus()
    return app.exec()
