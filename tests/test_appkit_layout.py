"""Consumer-visible native settings behavior; no capture or model execution."""
import pytest

A = pytest.importorskip("AppKit")
from inklow import appkit_ui
from inklow.config import Config


@pytest.fixture
def settings_shell(monkeypatch, tmp_path):
    monkeypatch.setenv("INKLOW_HOME", str(tmp_path))
    app = A.NSApplication.sharedApplication()
    shell = appkit_ui.BroTypistApplication.alloc().init().configure(
        Config(global_enabled=False, statistics_enabled=False, record_history=False,
               screen_context=False, clipboard_context=False, screenshot_context=False,
               screenshot_appearance=False, show_accessory=False,
               check_updates_automatically=False), True, True)
    shell._build_status_menu()
    shell._build_window()
    shell._show_pane("general")
    try:
        yield shell
    finally:
        shell.window.setDelegate_(None)
        shell.window.close()
        shell.updates.close()
        shell.controller.stop()


def test_settings_detail_tracks_window_growth_without_stranded_content(settings_shell):
    shell = settings_shell
    root = shell.window.contentView()
    root.layoutSubtreeIfNeeded()
    before_root = root.bounds()
    before_detail = shell.content_scroll.frame()
    right_inset = A.NSWidth(before_root) - A.NSMaxX(before_detail)
    top_inset = A.NSHeight(before_root) - A.NSMaxY(before_detail)
    shell.window.setContentSize_(A.NSMakeSize(A.NSWidth(before_root) + 300,
                                            A.NSHeight(before_root) + 160))
    root.layoutSubtreeIfNeeded()
    after_root = root.bounds()
    after_detail = shell.content_scroll.frame()
    assert A.NSWidth(after_detail) == pytest.approx(A.NSWidth(before_detail) + 300, abs=1)
    assert A.NSHeight(after_detail) == pytest.approx(A.NSHeight(before_detail) + 160, abs=1)
    assert A.NSWidth(after_root) - A.NSMaxX(after_detail) == pytest.approx(right_inset, abs=1)
    assert A.NSHeight(after_root) - A.NSMaxY(after_detail) == pytest.approx(top_inset, abs=1)


def test_resize_preserves_unsaved_instruction_editor_and_selection(settings_shell):
    shell = settings_shell
    shell._show_pane("personalization")
    editor = shell.editors["global_instructions"]
    draft = "Unsaved instructions must survive resizing."
    editor.setString_(draft)
    editor.setSelectedRange_((8, 12))
    shell.window.makeFirstResponder_(editor)
    shell.window.setContentSize_(A.NSMakeSize(1200, 800))
    shell.window.setContentSize_(A.NSMakeSize(900, 640))
    shell.window.contentView().layoutSubtreeIfNeeded()
    assert shell.editors["global_instructions"] is editor
    assert shell.window.firstResponder() == editor
    assert str(editor.string()) == draft
    assert tuple(editor.selectedRange()) == (8, 12)


def test_visible_cancel_cannot_turn_into_install_before_ui_refresh(settings_shell, monkeypatch):
    shell = settings_shell
    status = {"state": "downloading", "installable": True, "version": "99.0.0"}
    monkeypatch.setattr(shell.updates, "snapshot", lambda: dict(status))
    shell._show_pane("about")
    button = shell.update_buttons["primary"]
    # The download finishes after drawing Cancel, before the user's click.
    status["state"] = "ready"
    actions = []
    monkeypatch.setattr(shell, "_cancel_update", lambda: actions.append("cancel"))
    monkeypatch.setattr(shell, "_install_update", lambda: actions.append("install"))
    button.performClick_(None)
    assert actions == ["cancel"]


def test_first_setup_badge_refresh_does_not_leave_requested_models_pane(settings_shell):
    shell = settings_shell
    shell._refresh_status()
    shell._show_pane("models")
    shell.window.contentView().layoutSubtreeIfNeeded()
    assert shell.pane == "models"
    row = int(shell.sidebar_table.selectedRow())
    assert shell.sidebar_source.rows[row]["id"] == "models"
