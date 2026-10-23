"""Qt-only settings contracts; no native hooks, real credentials or model loads.

Run with QT_QPA_PLATFORM=offscreen and an isolated INKLOW_HOME. The integration
owner executes these after the shared Controller and system helpers land.
"""
from dataclasses import replace
import sys

import pytest

if sys.platform == "darwin":
    pytest.skip("macOS uses the native AppKit shell, not the Qt UI", allow_module_level=True)
from PySide6.QtCore import QEvent, Qt
from PySide6.QtGui import QKeyEvent
from PySide6.QtWidgets import QApplication, QMessageBox, QPlainTextEdit

from inklow.config import Config
from inklow import qt_ui


class UIController:
    """A deliberately headless UI boundary, not a replacement shipped runtime."""

    def __init__(self, config):
        self.config = config
        self.events = []
        self.actions = []
        self.started = False
        self.stopped = False
        self.saved = []

    def set_clipboard_reader(self, reader):
        self.clipboard_reader = reader

    def set_screen_reader(self, reader):
        self.screen_reader = reader

    def set_battery_reader(self, reader):
        self.battery_reader = reader

    def start(self):
        self.started = True

    def tick(self):
        pass

    def drain_events(self):
        events, self.events = self.events, []
        return events

    def stop(self):
        self.stopped = True

    def status(self):
        return {"enabled": self.config.global_enabled, "state": "idle", "message": "Ready",
                "native_status": "Isolated UI test; no native access", "on_battery": False,
                "current_app": "test.app", "current_domain": "", "download": None}

    def update_config(self, config, persist=True):
        config.validate()
        self.config = config
        self.saved.append(config)

    def invoke(self, action):
        self.actions.append(action)
        return True

    def pause(self, minutes):
        self.actions.append(("pause", minutes))

    def models(self):
        return []

    def cancel_download(self):
        self.actions.append("cancel_download")

    def history_info(self):
        return {"count": 0, "bytes": 0, "available": True, "error": ""}

    def set_history_enabled(self, enabled):
        self.config = replace(self.config, record_history=enabled)

    def stats(self, period="week", group_by="day"):
        zero = {"completions": 0, "words": 0, "characters": 0}
        return {"totals": zero, "today": zero, "rows": [], "estimated_seconds": 0}


@pytest.fixture
def shell(monkeypatch, tmp_path):
    app = QApplication.instance() or QApplication([])
    app.setQuitOnLastWindowClosed(False)
    monkeypatch.setenv("INKLOW_HOME", str(tmp_path))
    monkeypatch.setattr(qt_ui, "Controller", UIController)
    monkeypatch.setattr(qt_ui.system, "launch_at_login_state", lambda: False)
    shell = qt_ui.MenuAgent(app, Config(global_enabled=False), no_tray=True)
    yield shell
    shell.stop()
    app.aboutToQuit.disconnect(shell.stop)
    if shell.settings:
        shell.settings._closing = True
        shell.settings.hide()
        shell.settings.deleteLater()
    shell.overlay.deleteLater()
    shell.accessory.deleteLater()
    shell.tray.deleteLater()
    shell.menu.deleteLater()
    app.processEvents()


def test_merge_updates_only_explicit_preference():
    current = Config(global_enabled=False, model_path="new-model.gguf")
    merged = qt_ui.merge_preferences(current, {"show_accessory": False})
    assert not merged.show_accessory
    assert merged.model_path == "new-model.gguf"
    assert not merged.global_enabled
    assert current.show_accessory


def test_three_way_rule_merge_preserves_unrelated_runtime_changes():
    baseline = {"app:writer": {"enabled": True, "custom_instructions": "original"}}
    draft = {"app:writer": {"enabled": True, "custom_instructions": "concise"}}
    current = {"app:writer": {"enabled": False, "custom_instructions": "original"},
               "app:other": {"enabled": False}}
    assert qt_ui.merge_mapping_edits(current, baseline, draft) == {
        "app:writer": {"enabled": False, "custom_instructions": "concise"},
        "app:other": {"enabled": False},
    }


def test_first_run_opens_setup_even_when_background_requested(shell):
    shell.start(background=True)
    assert shell.controller.started
    assert shell.settings.isVisible()
    assert shell.settings.sidebar.currentItem().text() == "Setup"
    assert not shell.tray_available


def test_subsequent_tray_launch_has_no_settings_window(shell):
    shell.controller.config.setup_completed = True
    shell.tray_available = True
    shell.start()
    assert shell.settings is None


def test_complete_settings_hierarchy_has_no_document_editor(shell):
    shell.show_settings("General")
    settings = shell.settings
    assert tuple(settings.sidebar.item(i).text() for i in range(settings.sidebar.count())) == qt_ui.PANES
    assert {control.objectName() for control in settings.findChildren(QPlainTextEdit)} == {
        "custom_instructions", "excluded_apps",
    }
    assert not settings.findChild(QPlainTextEdit, "editor")
    assert not settings._bindings["screen_context"][0].isEnabled()


def test_save_merges_draft_with_latest_controller_config(shell):
    shell.show_settings("General")
    settings = shell.settings
    settings._bindings["show_accessory"][0].setChecked(False)
    assert shell.controller.config.show_accessory
    shell.controller.config = replace(shell.controller.config, model_path="selected-later.gguf", global_enabled=True)
    assert settings.apply()
    assert not shell.controller.config.show_accessory
    assert shell.controller.config.global_enabled
    assert shell.controller.config.model_path == "selected-later.gguf"
    assert not settings._changes()


def test_failed_validation_keeps_draft_and_reports_recovery(shell, monkeypatch):
    shell.show_settings("Personalization")
    settings = shell.settings
    settings.instructions.setPlainText("x" * 8001)
    errors = []
    monkeypatch.setattr(qt_ui, "_error", lambda parent, error: errors.append(str(error)))
    assert not settings.apply()
    assert shell.controller.config.custom_instructions == ""
    assert len(settings.instructions.toPlainText()) == 8001
    assert "8000" in errors[0]


def test_login_failure_does_not_persist_or_install_at_startup(shell, monkeypatch):
    calls = []
    def fail(enabled):
        calls.append(enabled)
        raise RuntimeError("Register the installed executable before enabling login startup.")
    monkeypatch.setattr(qt_ui.system, "set_launch_at_login", fail)
    monkeypatch.setattr(qt_ui, "_error", lambda *_: None)
    shell.show_settings("General")
    assert not calls
    shell.settings.login.setChecked(True)
    assert calls == [True]
    assert not shell.settings.login.isChecked()
    assert not shell.controller.config.launch_at_login


def test_history_opt_in_failure_stays_disabled(shell, monkeypatch):
    shell.show_settings("Personalization")
    monkeypatch.setattr(QMessageBox, "question", lambda *_: QMessageBox.StandardButton.Yes)
    def unavailable(enabled):
        raise RuntimeError("Unlock OS secret storage; plaintext fallback is not allowed.")
    monkeypatch.setattr(shell.controller, "set_history_enabled", unavailable)
    errors = []
    monkeypatch.setattr(qt_ui, "_error", lambda parent, error: errors.append(str(error)))
    shell.settings.history.setChecked(True)
    assert not shell.settings.history.isChecked()
    assert not shell.controller.config.record_history
    assert "plaintext" in errors[0]


def test_no_tray_close_cancel_keeps_recovery_settings_visible(shell, monkeypatch):
    shell.show_settings("General")
    monkeypatch.setattr(QMessageBox, "question", lambda *_: QMessageBox.StandardButton.No)
    shell.settings.close()
    assert shell.settings.isVisible()
    assert not shell.controller.stopped


def test_tray_close_hides_settings_without_stopping(shell):
    shell.tray_available = True
    shell.show_settings("General")
    shell.settings.close()
    assert not shell.settings.isVisible()
    assert not shell.controller.stopped


def test_shortcut_recorder_captures_tab_instead_of_focus_navigation(shell):
    recorder = qt_ui.ShortcutRecorder()
    recorder.set_value("AboveTab")
    recorder._begin()
    recorder.event(QKeyEvent(QEvent.Type.KeyPress, Qt.Key.Key_Tab, Qt.KeyboardModifier.NoModifier))
    assert recorder.value() == "Tab"
    recorder.deleteLater()






def test_main_thread_drain_clears_suggestions_and_privacy_accessory(shell):
    shell.controller.events = [{"type": "suggestion", "text": " tomorrow", "rect": [20, 20, 1, 20], "choices": [], "mode": "completion"}]
    shell.tick()
    assert shell.overlay.isVisible()
    assert shell.accessory.isVisible()
    shell.controller.events = [{"type": "clear", "hide_accessory": True}]
    shell.tick()
    assert not shell.overlay.isVisible()
    assert not shell.accessory.isVisible()




def test_linux_disables_only_unsafe_rewrites_not_alternatives(shell):
    if not sys.platform.startswith("linux"):
        pytest.skip("Linux AT-SPI atomic-replacement capability boundary")
    shell.show_settings("Labs")
    controls = shell.settings._bindings
    assert not controls["emoji_enabled"][0].isEnabled()
    assert not controls["synonyms_enabled"][0].isEnabled()
    assert controls["alternatives_enabled"][0].isEnabled()


@pytest.mark.parametrize("finish", ["accept", "cancel", "focus_loss"])
def test_recording_existing_global_chord_cannot_toggle_typing(shell, finish):
    from PySide6.QtGui import QFocusEvent
    from inklow.native.base import BridgeBase
    from inklow.runtime import Controller
    config = replace(Config(), shortcuts={**Config().shortcuts, "toggle": "Ctrl+Alt+F8"})
    bridge = BridgeBase()
    shell.controller = Controller(config, bridge)
    shell.controller.start()
    shell.show_settings("Shortcuts")
    recorder = shell.settings._shortcuts["toggle"]
    recorder._begin()
    action = bridge._key_action("F8", ("Ctrl", "Alt"))
    assert action == "toggle"
    assert not bridge._dispatch(action)
    if finish == "focus_loss":
        recorder.focusOutEvent(QFocusEvent(QEvent.Type.FocusOut))
    else:
        event = QKeyEvent(QEvent.Type.KeyPress,
                          Qt.Key.Key_F8 if finish == "accept" else Qt.Key.Key_Escape,
                          Qt.KeyboardModifier.ControlModifier | Qt.KeyboardModifier.AltModifier
                          if finish == "accept" else Qt.KeyboardModifier.NoModifier)
        recorder.event(event)
    assert shell.controller.config.global_enabled
    assert bridge._dispatch(action)
    shell.controller.tick()
    assert not shell.controller.config.global_enabled
