"""Qt-only settings contracts; no native hooks, real credentials or model loads.

Run with QT_QPA_PLATFORM=offscreen and an isolated INKLOW_HOME. The integration
owner executes these after the shared Controller and system helpers land.
"""
from dataclasses import replace
import sys
import time

import pytest

if sys.platform == "darwin":
    pytest.skip("macOS uses the native AppKit shell, not the Qt UI", allow_module_level=True)
from PySide6.QtCore import QEvent, QPoint, QRect, Qt, QTimer
from PySide6.QtGui import QKeyEvent
from PySide6.QtTest import QTest
from PySide6.QtWidgets import QApplication, QMessageBox, QPlainTextEdit

from inklow.config import Config
from inklow import qt_ui
from inklow.core import Snapshot
from inklow.native.base import BridgeBase


class UIController:
    """A deliberately headless UI boundary, not a replacement shipped runtime."""

    def __init__(self, config):
        self.config = config
        self.events = []
        self.actions = []
        self.started = False
        self.stopped = False
        self.saved = []
        self.bridge = BridgeBase()
        self.ticks = 0

    def set_wake_callback(self, callback):
        self.wake_callback = callback

    def set_clipboard_reader(self, reader):
        self.clipboard_reader = reader

    def set_screen_reader(self, reader):
        self.screen_reader = reader

    def set_battery_reader(self, reader):
        self.battery_reader = reader

    def start(self):
        self.started = True
        self.bridge.configure(self.config)
        self.bridge.start_shortcuts(self.invoke)
        self.bridge.set_shortcuts_enabled(True)

    def tick(self):
        self.ticks += 1

    def drain_events(self):
        events, self.events = self.events, []
        return events

    def stop(self):
        self.stopped = True
        self.bridge.stop()

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

    def toggle_current_app(self, *, app=None, domain=""):
        self.actions.append(("app_toggle", app, domain))

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
def shell(monkeypatch, tmp_path, request):
    app = QApplication.instance() or QApplication([])
    app.setQuitOnLastWindowClosed(False)
    monkeypatch.setenv("INKLOW_HOME", str(tmp_path))
    monkeypatch.setattr(qt_ui, "Controller", UIController)
    monkeypatch.setattr(qt_ui.system, "launch_at_login_state", lambda: False)
    tray_available = getattr(request, "param", False)
    monkeypatch.setattr(qt_ui.QSystemTrayIcon, "isSystemTrayAvailable", lambda: tray_available)
    shell = qt_ui.MenuAgent(app, Config(global_enabled=False), no_tray=not tray_available)
    yield shell
    shell.stop()
    app.aboutToQuit.disconnect(shell.stop)
    if shell.welcome:
        shell.welcome.hide()
        shell.welcome.deleteLater()
    if shell.settings:
        shell.settings._closing = True
        shell.settings.hide()
        shell.settings.deleteLater()
    shell.overlay.deleteLater()
    shell.accessory.deleteLater()
    if shell.accessory.menu:
        shell.accessory.menu.deleteLater()
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


def test_first_run_opens_welcome_and_no_tray_recovery_even_when_background_requested(shell):
    shell.start(background=True)
    assert shell.controller.started
    assert shell.settings.isVisible()
    assert shell.welcome.isVisible()
    assert shell.welcome.pages.currentIndex() == 0
    assert not shell.tray_available


def test_subsequent_tray_launch_has_no_settings_window(shell):
    shell.controller.config.setup_completed = True
    shell.tray_available = True
    shell.start()
    assert shell.settings is None




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
    shell.show_settings("Languages")
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
    shell.show_settings("Privacy")
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






def test_unverified_completion_has_no_popup_and_clear_removes_privacy_accessory(shell):
    shell.controller.events = [{"type": "suggestion", "text": " tomorrow", "rect": [20, 20, 1, 20], "choices": [], "mode": "completion"}]
    shell.tick()
    assert not shell.overlay.isVisible()
    assert shell.accessory.isVisible()
    shell.controller.events = [{"type": "clear", "hide_accessory": True}]
    shell.tick()
    assert not shell.overlay.isVisible()
    assert not shell.accessory.isVisible()




def test_linux_disables_only_unsafe_rewrites_not_alternatives(shell):
    if not sys.platform.startswith("linux"):
        pytest.skip("Linux AT-SPI atomic-replacement capability boundary")
    shell.show_settings("Autocomplete")
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
    shell.show_settings("Autocomplete")
    recorder = shell.settings.shortcut_controls["toggle"]
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


@pytest.mark.parametrize("caret", [True, False])
def test_accessory_is_compact_and_above_native_line_or_field(shell, caret):
    bounds = shell.app.primaryScreen().availableGeometry()
    ratio = shell.app.primaryScreen().devicePixelRatio()
    field_x, field_y = bounds.left() + 40, bounds.top() + 80
    line_y = field_y + 60 if caret else field_y
    field = [field_x * ratio, field_y * ratio, 240 * ratio, 160 * ratio]
    rect = [(field_x + 120) * ratio, line_y * ratio, ratio, 20 * ratio] if caret else None
    shell._caret({"type": "caret", "rect": rect, "field_rect": field})
    geometry = shell.accessory.geometry()
    assert shell.accessory.isVisible()
    assert geometry.size().width() == geometry.size().height() == 24
    assert shell.accessory.button.toolButtonStyle() == Qt.ToolButtonStyle.ToolButtonIconOnly
    assert geometry.left() == field_x
    assert geometry.bottom() < line_y
    assert 3 <= line_y - geometry.bottom() <= 8
    assert not geometry.intersects(QRect(field_x, line_y, 240, 20))


def test_accessory_clamps_at_screen_edge_without_covering_text_line(shell):
    screen = shell.app.primaryScreen()
    bounds, ratio = screen.availableGeometry(), screen.devicePixelRatio()
    line_y = bounds.top() + 2
    shell._caret({"type": "caret", "rect": [bounds.right() * ratio, line_y * ratio, ratio, 20 * ratio]})
    geometry = shell.accessory.geometry()
    assert bounds.contains(geometry)
    assert geometry.top() > line_y + 20
    assert not geometry.intersects(QRect(bounds.left(), line_y, bounds.width(), 20))


@pytest.mark.parametrize("surface", ["tray", "accessory"])
def test_owned_menu_stays_selectable_and_freezes_capture_across_timer_ticks(shell, surface):
    shell.controller.start()
    shell.controller.events = [{"type": "caret", "rect": [120, 160, 1, 20], "field_rect": [40, 100, 240, 160]}]
    shell.tick()
    anchor = shell.accessory.geometry()
    if surface == "accessory":
        shell.accessory.button.click()
        menu = shell.accessory.menu
    else:
        menu = shell.menu
        menu.popup(QPoint(40, 40))
    name = "field_app_toggle" if surface == "accessory" else "enabled"
    enabled = next(action for action in menu.actions() if action.objectName() == name)
    menu.setActiveAction(enabled)
    ticks = shell.controller.ticks
    shell.controller.events = [{"type": "clear", "hide_accessory": True}]
    for _ in range(12):
        shell.tick()
    assert menu.isVisible()
    assert menu.activeAction() is enabled
    assert shell.accessory.geometry() == anchor
    assert shell.accessory.isVisible()
    assert shell.controller.ticks == ticks
    assert shell.controller.events == [{"type": "clear", "hide_accessory": True}]
    assert not shell.controller.bridge._dispatch("toggle")
    QTest.keyClick(menu, Qt.Key.Key_Return)
    assert not menu.isVisible()
    assert shell.controller.actions == []
    shell.app.processEvents()
    if surface == "accessory":
        assert not shell.controller.config.global_enabled
        assert shell.controller.actions == [("app_toggle", "test.app", "")]
    else:
        assert shell.controller.config.global_enabled
        assert shell.controller.actions == [("pause", 0)]
    assert shell.controller.ticks > ticks
    assert not shell.accessory.isVisible()
    assert shell.controller.bridge._dispatch("toggle")


def test_submenu_dismissal_keeps_parent_menu_capture_suspended(shell):
    shell.controller.start()
    shell.menu.popup(QPoint(40, 40))
    submenu = next(action.menu() for action in shell.menu.actions() if action.menu())
    submenu.popup(QPoint(80, 80))
    submenu.close()
    shell.app.processEvents()
    shell.tick()
    assert shell.menu.isVisible()
    assert shell.controller.ticks == 0
    assert not shell.controller.bridge._dispatch("toggle")
    shell.menu.close()
    shell.app.processEvents()
    assert shell.controller.ticks > 0
    assert shell.controller.bridge._dispatch("toggle")


def test_clear_does_not_destroy_open_accessory_menu_and_dismissal_applies_hide(shell):
    shell._caret({"rect": [120, 160, 1, 20], "field_rect": [40, 100, 240, 160]})
    shell.accessory.button.click()
    menu = shell.accessory.menu
    shell.clear_overlay(hide_accessory=True)
    assert shell.accessory.isVisible()
    assert menu.isVisible()
    menu.close()
    shell.app.processEvents()
    assert not menu.isVisible()
    assert not shell.accessory.isVisible()


@pytest.mark.parametrize("changed_app", ["fixture.writer", "another.writer"])
def test_field_app_toggle_retains_original_app_after_menu_closes(shell, changed_app):
    status = {**shell.controller.status(), "current_app": "fixture.writer",
              "current_domain": "writing.example", "current_app_enabled": True}
    shell.controller.status = lambda: status
    shell.accessory.button.click()
    menu = shell.accessory.menu
    action = next(action for action in menu.actions() if action.objectName() == "field_app_toggle")
    menu.setActiveAction(action)
    ticks = shell.controller.ticks
    status["current_app"] = changed_app
    status["current_domain"] = "another.example"
    QTest.keyClick(menu, Qt.Key.Key_Return)
    assert not menu.isVisible()
    assert shell.controller.actions == []
    assert shell.controller.ticks == ticks
    shell.app.processEvents()
    assert shell.controller.actions == [("app_toggle", "fixture.writer", "writing.example")]
    assert shell.controller.ticks > ticks




def test_field_pause_and_resume_do_not_change_global_or_app_preferences(shell):
    shell.controller.config = replace(shell.controller.config, global_enabled=True)
    status = {**shell.controller.status(), "paused_until": 0}
    shell.controller.status = lambda: status
    menu = shell.create_field_menu()
    pause = next(action for action in menu.actions() if action.objectName() == "field_pause")
    pause.trigger()
    assert shell.controller.actions == [("pause", 60)]
    menu.deleteLater()
    status["paused_until"] = time.monotonic() + 3600
    menu = shell.create_field_menu()
    resume = next(action for action in menu.actions() if action.objectName() == "field_pause")
    assert resume.text() == "Resume predictions"
    resume.trigger()
    assert shell.controller.actions == [("pause", 60), ("pause", 0)]
    assert shell.controller.config.global_enabled
    assert shell.controller.saved == []
    menu.deleteLater()


def test_unsupported_field_is_not_labeled_active_or_globally_disabled(shell):
    shell.controller.config = replace(shell.controller.config, global_enabled=True)
    shell.controller.status = lambda: {"enabled": True, "state": "idle",
                                       "inline_available": False, "inline_message": "Unsupported text control",
                                       "current_app": "fixture.writer", "current_app_enabled": True}
    shell.refresh_menu()
    status = next(action for action in shell.menu.actions() if action.objectName() == "status")
    assert "Active" not in status.text()
    assert shell.controller.config.global_enabled
    menu = shell.create_field_menu()
    toggle = next(action for action in menu.actions() if action.objectName() == "field_app_toggle")
    assert toggle.isChecked()
    menu.deleteLater()


def test_native_field_menu_uses_same_compact_actions_and_deferred_target(shell, monkeypatch):
    shell.controller.start()
    status = {**shell.controller.status(), "current_app": "fixture.writer",
              "current_domain": "writing.example", "current_app_enabled": True}
    shell.controller.status = lambda: status
    rows = []
    monkeypatch.setattr(shell.controller.bridge, "show_native_menu",
                        lambda items: rows.extend(items) or True, raising=False)
    monkeypatch.setattr(shell.controller.bridge, "hide_native", lambda: None, raising=False)
    shell._native_action("menu")
    assert len(rows) == 3
    assert rows[0][1] == "✓ Predictions in fixture.writer: Enabled"
    assert rows[1][1] == "Pause for 1 hour"
    assert rows[2][1] == "Settings…"
    assert shell._menu_session
    assert not shell.controller.bridge._enabled
    ticks = shell.controller.ticks
    status["current_app"] = "another.writer"
    shell._native_action(rows[0][0])
    assert shell.controller.actions == []
    assert shell.controller.ticks == ticks
    assert not shell.controller.bridge._enabled
    shell.app.processEvents()
    assert shell.controller.actions == [("app_toggle", "fixture.writer", "writing.example")]
    assert shell.controller.ticks > ticks
    assert shell.controller.bridge._enabled

@pytest.mark.parametrize("shell", [True], indirect=True)
def test_tray_stays_visible_after_settings_close_and_quit_exits_event_loop(shell):
    shell.controller.config.setup_completed = True
    shell.start(background=True)
    shell.show_settings("General")
    shell.settings.close()
    assert shell.tray.isVisible()
    assert not shell.settings.isVisible()
    assert shell.timer.isActive()
    shell.menu.popup(QPoint(40, 40))
    quit_action = next(action for action in shell.menu.actions() if action.objectName() == "quit")
    shell.menu.setActiveAction(quit_action)
    exited = []
    on_quit = lambda: exited.append(True)
    shell.app.aboutToQuit.connect(on_quit)
    timeout = QTimer()
    timeout.setSingleShot(True)
    timeout.timeout.connect(lambda: shell.app.exit(87))
    timeout.start(2000)
    QTimer.singleShot(0, lambda: QTest.keyClick(shell.menu, Qt.Key.Key_Return))
    try:
        assert shell.app.exec() == 0
    finally:
        timeout.stop()
        shell.app.aboutToQuit.disconnect(on_quit)
    assert exited == [True]
    assert shell.controller.stopped
    assert not shell.timer.isActive()
    assert not shell.tray.isVisible()
    assert not shell.accessory.isVisible()


@pytest.mark.parametrize("shell", [True], indirect=True)
def test_tray_recovers_after_host_disappears_without_restarting_agent(shell, monkeypatch):
    def wait_for_tray(available):
        deadline = time.monotonic() + 3
        while shell.tray_available != available and time.monotonic() < deadline:
            QTest.qWait(25)
        assert shell.tray_available is available

    shell.controller.config.setup_completed = True
    shell.start(background=True)
    monkeypatch.setattr(qt_ui.QSystemTrayIcon, "isSystemTrayAvailable", lambda: False)
    wait_for_tray(False)
    assert not shell.tray_available
    assert shell.settings.isVisible()
    assert not shell.controller.stopped
    monkeypatch.setattr(qt_ui.QSystemTrayIcon, "isSystemTrayAvailable", lambda: True)
    wait_for_tray(True)
    assert shell.tray_available
    assert shell.tray.isVisible()
    shell.settings.close()
    assert not shell.settings.isVisible()
    assert shell.tray.isVisible()
    assert not shell.controller.stopped


def test_verified_host_suffix_is_transparent_same_line_and_does_not_take_focus(shell):
    from dataclasses import asdict
    from inklow.core import InlineRender
    from PySide6.QtGui import QFontInfo, QFontMetricsF
    font = QApplication.font()
    metrics, info = QFontMetricsF(font), QFontInfo(font)
    screen = shell.app.primaryScreen()
    ratio = screen.devicePixelRatio()
    render = InlineRender(info.family(), font.pointSizeF(), 100 * ratio,
                          (0, 0, 700 * ratio, 500 * ratio),
                          metrics.ascent() * ratio, metrics.descent() * ratio, source="verified-host-run")
    event = {"type": "suggestion", "text": " tomorrow", "choices": [], "mode": "completion",
             "rect": [100 * ratio, (100 - metrics.ascent()) * ratio, ratio, metrics.height() * ratio],
             "render": asdict(render), "inline": True}
    focused = QApplication.focusWidget()
    shell._suggestion(event)
    assert shell.overlay.isVisible()
    assert not shell.overlay.surface.isVisible()
    assert shell.overlay._ghost[0] == " tomorrow"
    assert shell.overlay.y() == round(100 - metrics.ascent())
    assert QApplication.focusWidget() is focused
    assert shell.overlay.testAttribute(Qt.WidgetAttribute.WA_TransparentForMouseEvents)
