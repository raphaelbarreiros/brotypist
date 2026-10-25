"""Native menu tracking must not destroy its anchor or capture menu navigation."""
from collections import deque
from types import SimpleNamespace

import pytest

A = pytest.importorskip("AppKit")
from inklow import appkit_overlay, appkit_ui, appkit_welcome


@pytest.fixture
def shell(monkeypatch):
    shell = appkit_ui.BroTypistApplication.alloc().init()
    bridge = SimpleNamespace(_enabled=True)
    bridge.set_shortcuts_enabled = lambda enabled: setattr(bridge, "_enabled", enabled)
    calls = []
    shell.controller = SimpleNamespace(bridge=bridge, tick=lambda: calls.append("capture"), drain_events=lambda: [])
    shell.stopping = False
    shell.tracking_menus = set()
    shell.pending_menu_actions = deque()
    shell.menu_shortcuts_were_enabled = False
    shell.resume_menu_shortcuts = False
    shell.bindings = {}
    shell.window = None
    shell.welcome = None
    monkeypatch.setattr(shell, "_refresh_status", lambda: None)
    monkeypatch.setattr(shell, "_refresh_menu_stats", lambda: None)
    return shell, bridge, calls


def test_native_menu_preserves_anchor_until_closed_and_restores_shortcuts(shell):
    shell, bridge, calls = shell
    menu = A.NSMenu.alloc().initWithTitle_("Fixture menu")
    shell.menuWillOpen_(menu)
    shell.tick_(None)
    shell.tick_(None)
    assert calls == []
    assert not bridge._enabled
    shell.menuDidClose_(menu)
    shell.tick_(None)
    assert calls == ["capture"]
    assert bridge._enabled


def test_native_menu_selection_runs_after_tracking_not_during_it(shell):
    shell, bridge, calls = shell
    menu = A.NSMenu.alloc().initWithTitle_("Fixture menu")
    item = A.NSMenuItem.alloc().initWithTitle_action_keyEquivalent_("Pause", None, "")
    item.setTag_(7)
    shell.bindings[7] = lambda sender: calls.append("pause-selected")
    shell.menuWillOpen_(menu)
    shell.perform_(item)
    shell.tick_(None)
    assert calls == []
    shell.menuDidClose_(menu)
    shell.tick_(None)
    assert calls == ["pause-selected", "capture"]


def test_menu_dismissal_does_not_enable_an_existing_shortcut_recorder(shell):
    shell, bridge, _ = shell
    bridge._enabled = False
    menu = A.NSMenu.alloc().initWithTitle_("Fixture menu")
    shell.menuWillOpen_(menu)
    shell.menuDidClose_(menu)
    shell.tick_(None)
    assert not bridge._enabled


@pytest.fixture
def field_menu(shell, monkeypatch):
    from inklow.config import Config
    shell, bridge, calls = shell
    status = {"current_app": "fixture.writer", "current_domain": "writing.example",
              "current_app_enabled": True, "paused_until": 0}
    shell.next_tag = 0
    shell.controller.config = Config()
    shell.controller.status = lambda: dict(status)
    shell.controller.toggle_current_app = lambda **context: calls.append(("app_toggle", context))
    shell.controller.pause = lambda minutes: calls.append(("pause", minutes))
    monkeypatch.setattr(shell, "_app_name", lambda app: "Fixture Writer" if app else "")
    monkeypatch.setattr(shell, "show_settings", lambda pane: calls.append(("settings", pane)))
    shell._build_field_menu()
    return shell, bridge, calls, status




@pytest.mark.parametrize("app, allowed", [("fixture.writer", False), ("", True)])
def test_field_app_toggle_shows_disabled_state_and_requires_a_target(field_menu, app, allowed):
    shell, _, _, status = field_menu
    status["current_app"] = app
    status["current_app_enabled"] = allowed
    menu = shell._accessory_menu()
    item = menu.itemAtIndex_(0)
    assert item.state() == A.NSControlStateValueOff
    assert item.isEnabled() == bool(app)

def test_unsupported_field_badge_does_not_claim_active_or_disable_predictions(field_menu, monkeypatch):
    shell, _, _, status = field_menu
    status.update(enabled=True, state="idle", inline_available=False,
                  inline_message="Unsupported text control")
    shell.controller.config.backend = "ollama"
    shell.controls = {}
    shell.status_item = None
    shell.last_status = None
    shell.overlay = SimpleNamespace(configure=lambda *args: None)
    monkeypatch.setattr(shell, "_permission_state",
                        lambda: {"accessibility": True, "input_monitoring": True})

    class Control:
        text = ""

        def setStringValue_(self, value):
            self.text = value

        def __getattr__(self, name):
            if name.startswith("set"):
                return lambda value: None
            raise AttributeError(name)

    for name in ("status_line", "pause_item", "resume_item", "hour_item", "tomorrow_item",
                 "attention_item", "menu_badge", "menu_model", "menu_backend", "menu_subtitle", "current_item"):
        setattr(shell, name, Control())
    appkit_ui.BroTypistApplication._refresh_status(shell)
    assert "Active" not in shell.menu_badge.text
    assert shell.menu_subtitle.text
    assert shell.controller.config.global_enabled



def test_field_toggle_keeps_original_app_and_defers_until_tracking_ends(field_menu):
    shell, bridge, calls, status = field_menu
    menu = shell._accessory_menu()
    item = menu.itemAtIndex_(0)
    shell.menuWillOpen_(menu)
    status["current_app"] = "another.writer"
    status["current_domain"] = "another.example"
    shell.perform_(item)
    shell.tick_(None)
    assert calls == []
    assert not bridge._enabled
    shell.menuDidClose_(menu)
    shell.tick_(None)
    assert calls == [("app_toggle", {"app": "fixture.writer", "domain": "writing.example"}), "capture"]
    assert bridge._enabled


def test_field_pause_is_temporary_and_resume_does_not_toggle_global_setting(field_menu):
    import time
    shell, _, calls, status = field_menu
    menu = shell._accessory_menu()
    item = menu.itemAtIndex_(1)
    shell.perform_(item)
    shell.tick_(None)
    assert calls == [("pause", 60), "capture"]
    status["paused_until"] = time.monotonic() + 3600
    shell._accessory_menu()
    assert item.title() == "Resume predictions"
    shell.perform_(item)
    shell.tick_(None)
    assert calls[-2:] == [("pause", 0), "capture"]
    assert shell.controller.config.global_enabled
    shell.controller.config.global_enabled = False
    status["paused_until"] = 0
    shell._accessory_menu()
    assert not item.isEnabled()


@pytest.fixture
def screen(monkeypatch):
    screen = SimpleNamespace(frame=lambda: A.NSMakeRect(0, 0, 1000, 700),
                             visibleFrame=lambda: A.NSMakeRect(0, 0, 1000, 676))
    monkeypatch.setattr(appkit_overlay.A, "NSScreen", SimpleNamespace(screens=lambda: [screen]))
    return screen


def test_accessory_is_above_the_text_line_instead_of_covering_it(screen):
    frame = appkit_overlay._position((100, 200, 1, 18), 24, 24, accessory=True)
    assert A.NSMinY(frame) >= 700 - 200 + 6
    assert A.NSWidth(frame) == 24


def test_top_edge_hides_accessory_instead_of_moving_onto_the_writing_line(screen):
    field = (10, 20, 900, 32)
    assert appkit_overlay._position(
        (10, 20, 900, 18), 24, 24, accessory=True, field_rect=field) is None


@pytest.mark.parametrize("left", [0, 10])
def test_left_edge_accessory_cannot_cover_preceding_lines(screen, left):
    assert appkit_overlay._position(
        (left, 200, 1, 18), 24, 24, accessory=True,
        field_rect=(left, 100, 900, 400)) is None


def test_accessory_is_hidden_when_no_nonoverlapping_position_exists(screen):
    field = (0, 20, 1000, 32)
    assert appkit_overlay._position((0, 20, 1000, 18), 24, 24, accessory=True, field_rect=field) is None


@pytest.mark.parametrize("text, expected", [
    ("a short suggestion", "a short suggestion"),
    ("x" * 44, "x" * 44),
    ("a thoughtful suggestion with another useful ending", "a thoughtful suggestion with another useful…"),
    ("x" * 70, "x" * 43 + "…"),
])
def test_chip_limits_display_only_at_a_word_boundary(text, expected):
    """DESIGN §8: chip display is bounded; the full insertion source is unchanged."""
    source = {"text": text}
    display = appkit_overlay._chip_text(source["text"])
    assert display == expected
    assert len(display) <= 44
    assert source["text"] == text


@pytest.mark.parametrize("text, location, length, expected", [
    ("A😀B café", 3, 0, ("A😀B café", 2, 0)),
    ("A😀B café", 1, 2, ("A😀B café", 1, 1)),
    ("A😀B café", 3, 1, ("A😀B café", 2, 1)),
    ("plain text", 5, 0, ("plain text", 5, 0)),
])
def test_native_preview_converts_caret_and_selection_from_utf16(text, location, length, expected):
    """PreviewSession owns Python character indices, including selected emoji."""
    editor = SimpleNamespace(string=lambda: text, selectedRange=lambda: SimpleNamespace(location=location, length=length))
    welcome = SimpleNamespace(editor=editor)
    assert appkit_welcome.WelcomeWindow.preview_values(welcome) == expected


def test_chip_flips_above_caret_near_visible_screen_bottom(screen):
    frame = appkit_overlay._position((990, 650, 1, 18), 240, 28)
    assert A.NSMinY(frame) >= 700 - 650 + 6
    assert A.NSMaxX(frame) <= 1000 - 8
    assert A.NSMinX(frame) >= 8


def test_native_ghost_uses_verified_host_baseline_without_card_or_mouse_target(screen):
    from dataclasses import asdict
    from inklow.core import InlineRender
    font = A.NSFont.fontWithName_size_("Helvetica", 13)
    ascent, descent = float(font.ascender()), abs(float(font.descender()))
    render = InlineRender("Helvetica", 13, 200, (0, 0, 900, 600), ascent, descent,
                          source="macos-ax-attributed-run")
    observed = {}
    panel = SimpleNamespace(
        orderOut_=lambda sender: observed.update(visible=False),
        setHasShadow_=lambda value: observed.update(shadow=value),
        setIgnoresMouseEvents_=lambda value: observed.update(mouse_through=value),
        setContentView_=lambda value: observed.update(surface=value),
        setFrame_display_=lambda frame, display: observed.update(frame=frame),
        orderFrontRegardless=lambda: observed.update(visible=True))
    overlay = appkit_overlay.CaretOverlay.__new__(appkit_overlay.CaretOverlay)
    overlay.suggestion = panel
    event = {"text": " tomorrow", "mode": "completion", "inline": True,
             "render": asdict(render), "rect": (100, 200 - ascent, 1, ascent + descent)}
    overlay._present_inline(event)
    assert observed["visible"]
    assert observed["shadow"] is False
    assert observed["mouse_through"] is True
    assert isinstance(observed["surface"], appkit_overlay.GhostSurface)
    assert A.NSMinX(observed["frame"]) == 100
    assert A.NSMinY(observed["frame"]) == pytest.approx(700 - 200 - descent)
    overlay._present_inline({**event, "render": None})
    assert observed["visible"] is False
