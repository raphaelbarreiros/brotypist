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
def screen(monkeypatch):
    screen = SimpleNamespace(frame=lambda: A.NSMakeRect(0, 0, 1000, 700),
                             visibleFrame=lambda: A.NSMakeRect(0, 0, 1000, 676))
    monkeypatch.setattr(appkit_overlay.A, "NSScreen", SimpleNamespace(screens=lambda: [screen]))
    return screen


def test_accessory_is_above_the_text_line_instead_of_covering_it(screen):
    frame = appkit_overlay._position((100, 200, 1, 18), 24, 24, accessory=True)
    assert A.NSMinY(frame) >= 700 - 200 + 6
    assert A.NSWidth(frame) == 24


def test_top_edge_uses_space_beside_field_not_over_its_words(screen):
    field = (10, 20, 900, 32)
    frame = appkit_overlay._position((10, 20, 900, 18), 24, 24, accessory=True, field_rect=field)
    assert frame is not None
    assert A.NSMinX(frame) >= field[0] + field[2] + 6


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
