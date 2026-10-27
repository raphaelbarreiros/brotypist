"""Native menu tracking must not destroy its anchor or capture menu navigation."""
from collections import deque
from types import SimpleNamespace
import threading

import pytest

A = pytest.importorskip("AppKit")
from inklow import appkit_overlay, appkit_ui, appkit_welcome
from inklow.config import Config


@pytest.fixture
def shell(monkeypatch):
    shell = appkit_ui.BroTypistApplication.alloc().init()
    bridge = SimpleNamespace(_enabled=True)
    bridge.set_shortcuts_enabled = lambda enabled: setattr(bridge, "_enabled", enabled)
    calls = []
    shell.controller = SimpleNamespace(config=Config(check_updates_automatically=False), bridge=bridge,
                                       tick=lambda: calls.append("capture"), drain_events=lambda: [])
    shell.stopping = False
    shell.shutdown_started = False
    shell.shutdown_error = ""
    shell.termination_requested = False
    shell.startup_initialized = True
    shell.startup_acknowledged = True
    shell.startup_failed = False
    shell._wake_lock = threading.Lock()
    shell._wake_pending = False
    shell.tracking_menus = set()
    shell.pending_menu_actions = deque()
    shell.menu_shortcuts_were_enabled = False
    shell.resume_menu_shortcuts = False
    shell.bindings = {}
    shell.window = None
    shell.welcome = None
    monkeypatch.setattr(shell, "_refresh_status", lambda: None)
    monkeypatch.setattr(shell, "_refresh_updates", lambda: None)
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


def test_native_wake_burst_queues_one_tick_and_timer_does_not_reopen_queue(shell, monkeypatch):
    shell, _, calls = shell
    queued = []
    monkeypatch.setattr(shell, "performSelectorOnMainThread_withObject_waitUntilDone_",
                        lambda selector, obj, wait: queued.append((selector, obj, wait)))
    workers = [threading.Thread(target=lambda: [shell._wake_controller() for _ in range(100)])
               for _ in range(4)]
    for worker in workers:
        worker.start()
    for worker in workers:
        worker.join()
    assert queued == [("wakeTick:", None, False)]
    shell.tick_(None)  # Timer may run before the queued selector.
    shell._wake_controller()
    assert len(queued) == 1
    shell.wakeTick_(None)
    shell._wake_controller()
    assert len(queued) == 2
    assert calls == ["capture", "capture"]


@pytest.mark.parametrize("blocked", ["menu", "shutdown"])
def test_queued_wake_is_released_even_when_tick_cannot_capture(shell, monkeypatch, blocked):
    shell, _, calls = shell
    queued = []
    monkeypatch.setattr(shell, "performSelectorOnMainThread_withObject_waitUntilDone_",
                        lambda *args: queued.append(args))
    shell._wake_controller()
    if blocked == "menu":
        shell.tracking_menus.add("tracking")
    else:
        shell.stopping = True
    shell.wakeTick_(None)
    assert not shell._wake_pending and calls == []
    shell.tracking_menus.clear()
    shell.stopping = False
    shell._wake_controller()
    assert len(queued) == 2


@pytest.fixture
def field_menu(shell, monkeypatch):
    from inklow.config import Config
    shell, bridge, calls = shell
    status = {"current_app": "fixture.writer", "current_domain": "writing.example",
              "current_app_enabled": True, "paused_until": 0}
    shell.next_tag = 0
    shell.controller.config = Config(check_updates_automatically=False)
    shell.controller.status = lambda: dict(status)
    shell.controller.toggle_current_app = lambda **context: calls.append(("app_toggle", context))
    shell.controller.pause = lambda minutes: calls.append(("pause", minutes))
    shell.controller.update_config = lambda config: setattr(shell.controller, "config", config)
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

        def tag(self):
            return 0

        def __getattr__(self, name):
            if name.startswith("set"):
                return lambda value: None
            raise AttributeError(name)

    for name in ("status_line", "pause_item", "resume_item", "hour_item", "tomorrow_item",
                 "attention_item", "secure_item", "download_item", "menu_badge", "menu_model",
                 "menu_backend", "menu_subtitle", "current_item", "record_item", "tab_item"):
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


@pytest.mark.parametrize("consent", [False, True])
def test_field_recording_needs_consent_and_targets_the_site_seen_when_opened(field_menu, monkeypatch, consent):
    shell, _, calls, status = field_menu
    prompts = []
    monkeypatch.setattr(shell, "_confirm", lambda title, message: prompts.append(title) or consent)
    menu = shell._accessory_menu()
    item = menu.itemAtIndex_(1)
    shell.menuWillOpen_(menu)
    status["current_domain"] = "another.example"
    shell.perform_(item)
    assert prompts == []
    shell.menuDidClose_(menu)
    shell.tick_(None)
    assert len(prompts) == 1
    expected = {"domain:writing.example": {"record_history": True}} if consent else {}
    assert shell.controller.config.app_rules == expected


def test_field_recording_can_be_stopped_without_a_prompt(field_menu, monkeypatch):
    shell, _, _, status = field_menu
    status["current_recording"] = True
    monkeypatch.setattr(shell, "_confirm", lambda title, message: pytest.fail("stopping needs no consent"))
    menu = shell._accessory_menu()
    assert menu.itemAtIndex_(1).state() == A.NSControlStateValueOn
    shell.perform_(menu.itemAtIndex_(1))
    shell.tick_(None)
    assert shell.controller.config.app_rules == {"domain:writing.example": {"record_history": False}}


def test_docs_action_is_explicit_and_keeps_original_capability_while_menu_tracks(field_menu, monkeypatch):
    shell, _, calls, status = field_menu
    shell.docs_help_hint = None
    menu = shell._accessory_menu()
    item = shell.field_docs_item
    assert item.isHidden()
    binding_count = shell.next_tag
    shell.docs_help_hint = {"token": "original-document"}
    monkeypatch.setattr(shell, "_enable_docs_accessibility",
                        lambda token: calls.append(("enable_docs", token)))
    shell._accessory_menu()
    assert not item.isHidden() and item.isEnabled()
    shell.menuWillOpen_(menu)
    shell.docs_help_hint = None
    status["current_app"] = "another.writer"
    assert shell._accessory_menu() == menu
    assert not item.isHidden()  # No reshuffling beneath the pointer.
    shell.perform_(item)
    shell.tick_(None)
    assert calls == []
    shell.menuDidClose_(menu)
    shell.tick_(None)
    assert calls == [("enable_docs", "original-document"), "capture"]
    shell._accessory_menu()
    assert item.isHidden() and shell.next_tag == binding_count


def test_docs_accessory_option_click_remains_plain_tab_not_enable_or_menu(monkeypatch):
    actions = []
    target = appkit_overlay.OverlayActions.alloc().init().setup(
        lambda action: actions.append(action), lambda sender: actions.append("menu"))
    target.actions = {100: "menu"}
    sender = SimpleNamespace(tag=lambda: 100)
    monkeypatch.setattr(appkit_overlay.A, "NSEvent", SimpleNamespace(
        modifierFlags=lambda: A.NSEventModifierFlagOption))
    target.pressed_(sender)
    assert actions == ["plain_tab"]


@pytest.mark.parametrize("url, pane", [
    ("brotypist://settings/shortcuts", "shortcuts"),
    ("brotypist://settings/appSettings", "appSettings"),
    ("BroTypist://settings/statistics", "statistics"),
    ("brotypist://welcome", "welcome"),
    ("brotypist://settings/unknown", "general"),
    ("https://example.com/settings/shortcuts", None),
])
def test_deep_links_only_navigate_to_known_panes(url, pane):
    assert appkit_ui._url_target(url) == pane


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
