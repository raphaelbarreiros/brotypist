"""Native shared actions, resident tray, and explicit document persistence."""
from __future__ import annotations

from dataclasses import replace
from pathlib import Path
import sys
import time

from PySide6.QtCore import QIODevice, QSaveFile, QSignalBlocker, Qt
from PySide6.QtGui import QAction, QKeySequence
from PySide6.QtWidgets import QApplication, QFileDialog, QMenu, QMessageBox, QSystemTrayIcon

from . import __version__
from .config import save_config
from .models import PRESETS
from .resources import app_icon, tray_icon
from .ui_models import ModelManager


class DesktopShell:
    """Behavior mixed into the main window; every menu uses the same actions."""

    def _build_shell(self, no_tray: bool) -> None:
        self._document_path: Path | None = None
        self._models_dialog: ModelManager | None = None
        self._tray = None
        self._quit_requested = False
        self.setWindowIcon(app_icon())
        self.actions: dict[str, QAction] = {}

        def action(key, label, callback, shortcut=None, role=None, checkable=False):
            item = QAction(label, self)
            item.setCheckable(checkable)
            if shortcut is not None:
                item.setShortcut(shortcut)
            if role is not None:
                item.setMenuRole(role)
            item.triggered.connect(callback)
            self.actions[key] = item
            return item

        action("new", "&New", self.new_document, QKeySequence.StandardKey.New)
        action("open", "&Open…", self.open_document, QKeySequence.StandardKey.Open)
        action("save", "&Save", self.save_document, QKeySequence.StandardKey.Save)
        action("save_as", "Save &As…", self.save_document_as, QKeySequence.StandardKey.SaveAs)
        action("close", "&Close Window", self.close, QKeySequence.StandardKey.Close)
        action("show", "Show Editor", self._toggle_editor)
        action("pause", "Pause Predictions", self._toggle_pause)
        action("global", "Suggest in Other Applications", self._toggle_global, checkable=True)
        action("models", "&Models…", self.open_models)
        action("settings", "&Settings and Exclusions…", self.open_settings,
               QKeySequence.StandardKey.Preferences, QAction.MenuRole.PreferencesRole)
        action("diagnostics", "Diagnostics and Permissions…", self.show_diagnostics)
        action("about", "About Inklow", self.show_about, role=QAction.MenuRole.AboutRole)
        action("quit", "&Quit Inklow", self.request_quit,
               QKeySequence.StandardKey.Quit, QAction.MenuRole.QuitRole)
        for minutes in (5, 15, 60):
            action(f"pause_{minutes}", f"Pause for {minutes} Minutes",
                   lambda checked=False, duration=minutes: self.pause_for(duration))
        file_menu = self.menuBar().addMenu("&File")
        for key in ("new", "open", "save", "save_as", "close"):
            file_menu.addAction(self.actions[key])
        file_menu.addSeparator()
        file_menu.addAction(self.actions["quit"])
        edit_menu = self.menuBar().addMenu("&Edit")
        for key, label, shortcut, callback in (
            ("undo", "&Undo", QKeySequence.StandardKey.Undo, self.editor.undo),
            ("redo", "&Redo", QKeySequence.StandardKey.Redo, self.editor.redo),
            ("cut", "Cu&t", QKeySequence.StandardKey.Cut, self.editor.cut),
            ("copy", "&Copy", QKeySequence.StandardKey.Copy, self.editor.copy),
            ("paste", "&Paste", QKeySequence.StandardKey.Paste, self.editor.paste),
            ("select_all", "Select &All", QKeySequence.StandardKey.SelectAll, self.editor.selectAll),
        ):
            edit_menu.addAction(action(key, label, callback, shortcut))
        self.actions["undo"].setEnabled(False)
        self.actions["redo"].setEnabled(False)
        self.actions["cut"].setEnabled(False)
        self.actions["copy"].setEnabled(False)
        self.editor.undoAvailable.connect(self.actions["undo"].setEnabled)
        self.editor.redoAvailable.connect(self.actions["redo"].setEnabled)
        self.editor.copyAvailable.connect(self.actions["cut"].setEnabled)
        self.editor.copyAvailable.connect(self.actions["copy"].setEnabled)
        utility_menu = self.menuBar().addMenu("&Inklow")
        self._populate_utility_menu(utility_menu)
        if not no_tray and QSystemTrayIcon.isSystemTrayAvailable():
            self._tray_menu = QMenu(self)
            self._populate_utility_menu(self._tray_menu)
            self._tray = QSystemTrayIcon(tray_icon(), self)
            self._tray.setToolTip("Inklow — local predictive writing")
            self._tray.setContextMenu(self._tray_menu)
            self._tray.activated.connect(self._tray_activated)
            self._tray.show()
        QApplication.instance().setQuitOnLastWindowClosed(False)
        # Font/margin setup can mark a fresh QTextDocument modified even
        # though the user has not entered a document yet.
        self.editor.document().setModified(False)
        self.editor.document().modificationChanged.connect(self._document_modified)
        self._sync_shell()

    def _populate_utility_menu(self, menu: QMenu) -> None:
        menu.addAction(self.actions["show"])
        menu.addAction(self.actions["pause"])
        pause = menu.addMenu("Pause Temporarily")
        for minutes in (5, 15, 60):
            pause.addAction(self.actions[f"pause_{minutes}"])
        menu.addAction(self.actions["global"])
        menu.addSeparator()
        for key in ("models", "settings", "diagnostics", "about"):
            menu.addAction(self.actions[key])
        menu.addSeparator()
        menu.addAction(self.actions["quit"])

    def _tray_usable(self) -> bool:
        return self._tray is not None and self._tray.isVisible() and QSystemTrayIcon.isSystemTrayAvailable()

    def _sync_shell(self) -> None:
        paused = not self.config.enabled or time.monotonic() < self._paused_until
        self.actions["pause"].setText("Resume Predictions" if paused else "Pause Predictions")
        self.actions["show"].setText("Hide Editor" if self.isVisible() and self._tray_usable() else "Show Editor")
        blocker = QSignalBlocker(self.actions["global"])
        self.actions["global"].setChecked(self.config.global_enabled)
        del blocker
        if self._tray is not None:
            self._tray.setIcon(tray_icon(paused))
            self._tray.setToolTip("Inklow — predictions paused" if paused else "Inklow — local predictive writing")

    def _toggle_editor(self) -> None:
        if self.isVisible() and self._tray_usable():
            self.hide()
        else:
            self.show_editor()
        self._sync_shell()

    def show_editor(self) -> None:
        self.showNormal()
        self.raise_()
        self.activateWindow()
        self.editor.setFocus()
        self._sync_shell()

    def _tray_activated(self, reason) -> None:
        if sys.platform != "darwin" and reason in (
                QSystemTrayIcon.ActivationReason.Trigger, QSystemTrayIcon.ActivationReason.DoubleClick):
            self.show_editor()

    def _toggle_pause(self) -> None:
        if not self.config.enabled:
            self._paused_until = 0.0
            self._pause_timer.stop()
            self._toggle_predictions(True)
        elif time.monotonic() < self._paused_until:
            self._paused_until = 0.0
            self._pause_timer.stop()
            self._resume_after_pause()
        else:
            self._toggle_predictions(False)

    def pause_for(self, minutes: int) -> None:
        self._paused_until = time.monotonic() + minutes * 60
        self._invalidate()
        self._pause_timer.start(minutes * 60 * 1000 + 25)
        self._refresh_status()

    def _toggle_global(self, enabled: bool) -> None:
        candidate = replace(self.config, global_enabled=enabled)
        try:
            save_config(candidate)
        except (OSError, ValueError) as exc:
            QMessageBox.warning(self, "Settings not saved", str(exc))
            self._sync_shell()
            return
        self._apply_config(candidate)

    def _document_modified(self, modified: bool) -> None:
        name = self._document_path.name if self._document_path else "Untitled"
        self.setWindowTitle(f"Inklow — {name}[*]")
        self.setWindowModified(modified)

    def _confirm_unsaved(self) -> bool:
        if not self.editor.document().isModified():
            return True
        answer = QMessageBox.warning(
            self, "Save document?", "Your document has unsaved changes.",
            QMessageBox.StandardButton.Save | QMessageBox.StandardButton.Discard | QMessageBox.StandardButton.Cancel,
            QMessageBox.StandardButton.Save)
        if answer == QMessageBox.StandardButton.Save:
            return self.save_document()
        return answer == QMessageBox.StandardButton.Discard

    def new_document(self) -> None:
        if not self._confirm_unsaved():
            return
        self._invalidate()
        self._document_path = None
        self.editor.clear()
        self.editor.document().setModified(False)
        self._document_modified(False)
        self.show_editor()

    def open_document(self) -> None:
        if not self._confirm_unsaved():
            return
        filename, _ = QFileDialog.getOpenFileName(self, "Open document", "", "Text documents (*.txt *.md);;All files (*)")
        if not filename:
            return
        try:
            text = Path(filename).read_text(encoding="utf-8-sig")
        except (OSError, UnicodeError) as exc:
            QMessageBox.warning(self, "Document not opened", str(exc))
            return
        self._invalidate()
        self._document_path = Path(filename)
        self.editor.setPlainText(text)
        self.editor.document().setModified(False)
        self._document_modified(False)
        self.show_editor()

    def save_document(self) -> bool:
        return self._save_to(self._document_path) if self._document_path else self.save_document_as()

    def save_document_as(self) -> bool:
        filename, _ = QFileDialog.getSaveFileName(self, "Save document", str(self._document_path or "Untitled.txt"), "Text documents (*.txt);;All files (*)")
        return self._save_to(Path(filename)) if filename else False

    def _save_to(self, path: Path) -> bool:
        stream = QSaveFile(str(path))
        if not stream.open(QIODevice.OpenModeFlag.WriteOnly):
            QMessageBox.warning(self, "Document not saved", stream.errorString())
            return False
        data = self.editor.toPlainText().encode("utf-8")
        if stream.write(data) != len(data):
            stream.cancelWriting()
            QMessageBox.warning(self, "Document not saved", stream.errorString())
            return False
        if not stream.commit():
            QMessageBox.warning(self, "Document not saved", stream.errorString())
            return False
        self._document_path = path
        self.editor.document().setModified(False)
        self._document_modified(False)
        return True

    def open_models(self) -> None:
        if self._closing:
            return
        if self._models_dialog is None:
            self._models_dialog = ModelManager(self.config.model_path, self)
            self._models_dialog.model_selected.connect(self._select_model)
            self._models_dialog.download_finished.connect(self._finish_shutdown)
        self._models_dialog.set_current_path(self.config.model_path)
        self._models_dialog.show()
        self._models_dialog.raise_()
        self._models_dialog.activateWindow()

    def _select_model(self, path: str) -> None:
        if self._closing:
            return
        preset = next((item for item in PRESETS if Path(path).name == item.filename), None)
        candidate = replace(self.config, backend="gguf", model_path=path,
                            prompt_style=preset.prompt_style if preset else "continuation")
        try:
            save_config(candidate)
        except (ValueError, OSError) as exc:
            QMessageBox.warning(self, "Model not selected", str(exc))
            return
        self._apply_config(candidate)
        self._models_dialog.accept()

    def show_diagnostics(self) -> None:
        if sys.platform == "darwin":
            permissions = "System Settings → Privacy & Security → Accessibility: enable Inklow. Global keyboard shortcuts may also require Input Monitoring. Restart Inklow after granting permission."
        elif sys.platform == "win32":
            permissions = "System-wide mode supports accessible UI Automation text controls. Apps running as administrator may not be accessible from a non-elevated Inklow."
        else:
            permissions = "System-wide mode needs AT-SPI and an X11 session. Wayland supports the built-in editor only."
        details = (
            f"Inklow {__version__}\nEngine: {self.config.backend}\n"
            f"Acceleration: {self.config.acceleration}\n"
            f"Engine status: {self._provider_error or ('Ready' if self._ready else 'Preparing')}\n"
            f"System-wide: {'Enabled' if self.config.global_enabled else 'Disabled'}\n"
            f"Accessibility: {self._bridge_error or ('Connected' if self._bridge else 'Not connected')}\n"
            f"Tray: {'Available' if self._tray_usable() else 'Unavailable; closing the window quits'}\n\n"
            f"{permissions}\n\nPassword/protected fields, unknown controls, and excluded applications are skipped. No typed text is included in these diagnostics."
        )
        QMessageBox.information(self, "Diagnostics and permissions", details)

    def show_about(self) -> None:
        QMessageBox.about(self, "About Inklow", f"Inklow {__version__}\nIndependent, local-first predictive writing.\nModels and inference stay on your computer.\nDocuments are saved only when you choose Save.")

    def request_quit(self) -> None:
        if self._quit_requested:
            return
        if not self._confirm_unsaved():
            return
        self._quit_requested = True
        self._begin_shutdown()
        self._finish_shutdown()

    def _shutdown_complete(self) -> bool:
        return (self._closing and not self._worker.isRunning()
                and (self._models_dialog is None or not self._models_dialog.downloading))

    def _finish_shutdown(self) -> None:
        if self._shutdown_complete():
            QApplication.instance().quit()

    def showEvent(self, event) -> None:
        super().showEvent(event)
        self._sync_shell()

    def hideEvent(self, event) -> None:
        super().hideEvent(event)
        self._sync_shell()
