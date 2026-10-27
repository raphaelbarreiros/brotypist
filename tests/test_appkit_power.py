"""Synthetic power readings exercise actual AppKit controls and CA animations."""
import pytest
from types import SimpleNamespace

A = pytest.importorskip("AppKit")
from inklow.appkit_power import NativePowerUI, PowerHelpPanel


def reading(*, available=True, has_battery=True, on_battery=False, charging=False, percent=65):
    return dict(available=available, has_battery=has_battery, on_battery=on_battery, charging=charging, percent=percent)


@pytest.fixture
def power_ui():
    A.NSApplication.sharedApplication()
    ui = NativePowerUI.alloc().init().configure()
    try:
        yield ui
    finally:
        ui.close()


@pytest.mark.parametrize("sample,state,level", [
    (reading(on_battery=True, percent=19), "low", "19%"),
    (reading(on_battery=True, percent=0), "low", "0%"),
    (reading(on_battery=True, percent=54), "battery", "54%"),
    (reading(charging=True, percent=54), "charging", "54%"),
    (reading(percent=100), "full", "100%"),
    (reading(percent=84), "external", "84%"),
    (reading(charging=None, percent=84), "external_unknown", "84%"),
    (reading(has_battery=False, charging=None, percent=None), "absent", None),
    (reading(available=False, has_battery=False, charging=None, percent=None), "unknown", None),
    (reading(on_battery=True, percent=None), "battery", None),
    (reading(on_battery=True, percent=True), "battery", None),
    (reading(on_battery=True, percent=101), "battery", None),
])
def test_power_card_renders_truthful_native_title_and_accessibility(power_ui, sample, state, level):
    box = power_ui.make_card()
    result = power_ui.present(sample)
    _, icon, title, detail = power_ui.cards["pane"]
    assert result.state == state
    assert title.stringValue() == result.title
    assert detail.stringValue() == result.detail
    assert box.accessibilityLabel() == result.title + ". " + result.detail
    assert icon.image() is not None
    if level is not None:
        assert level in title.stringValue()
        assert level in result.compact
    else:
        assert "%" not in title.stringValue()
        assert "%" not in result.compact


def test_native_charging_cue_is_bounded_and_stops_on_hide(power_ui):
    power_ui.apply_display_preferences(reduce_motion=False, increase_contrast=False, reduce_transparency=False)
    power_ui.make_card()
    power_ui.set_visibility(pane=True)
    power_ui.present(reading(charging=True))
    icon = power_ui.cards["pane"][1]
    animation = icon.layer().animationForKey_("inklow.charge")
    assert animation is not None
    assert animation.keyPath() == "opacity"
    assert animation.duration() * animation.repeatCount() * 2 <= 5
    assert animation.autoreverses()
    power_ui.set_visibility(pane=False)
    assert icon.layer().animationForKey_("inklow.charge") is None
    power_ui.set_visibility(pane=True)
    assert icon.layer().animationForKey_("inklow.charge") is not None
    power_ui.apply_display_preferences(reduce_motion=True, increase_contrast=True, reduce_transparency=True)
    assert icon.layer().animationForKey_("inklow.charge") is None
    assert power_ui.cards["pane"][0].borderWidth() == 2


def test_reduced_motion_has_static_charging_and_short_fade(power_ui):
    power_ui.apply_display_preferences(reduce_motion=True, increase_contrast=False, reduce_transparency=True)
    power_ui.make_card()
    power_ui.set_visibility(pane=True)
    power_ui.present(reading(charging=True, percent=48))
    box, icon, title, _ = power_ui.cards["pane"]
    assert icon.layer().animationForKey_("inklow.charge") is None
    assert "Charging" in title.stringValue()
    power_ui.reveal(box)
    fade = box.layer().animationForKey_("inklow.arrival")
    assert fade.keyPath() == "opacity"
    assert fade.duration() <= .2
    power_ui.set_visibility(pane=False)
    assert box.layer().animationForKey_("inklow.arrival") is None


def test_native_session_resign_and_power_transition_stop_motion(power_ui):
    power_ui.apply_display_preferences(reduce_motion=False, increase_contrast=False, reduce_transparency=False)
    power_ui.make_card()
    power_ui.set_visibility(pane=True)
    power_ui.present(reading(charging=True))
    icon = power_ui.cards["pane"][1]
    power_ui.sessionResigned_(None)
    assert icon.layer().animationForKey_("inklow.charge") is None
    assert not power_ui.help_showing
    power_ui.sessionBecameActive_(None)
    power_ui.present(reading(on_battery=True, percent=42))
    assert icon.layer().animationForKey_("inklow.charge") is None
    assert power_ui.presentation.state == "battery"


def test_power_help_never_consumes_low_cycle_without_resident_anchor(power_ui):
    sample = reading(on_battery=True, percent=19)
    power_ui.present(sample, allow_help=True)
    assert not power_ui.help_showing
    # No resident anchor means no presented ask; the policy remains armed.
    assert power_ui.policy.should_ask(sample)
    assert not power_ui.policy.should_ask(sample)

@pytest.mark.parametrize("on_console,login_done,locked,eligible", [
    (True, True, False, True),
    (False, True, False, False),
    (True, False, False, False),
    (True, True, True, False),
])
def test_help_anchor_requires_unlocked_logged_in_console(power_ui, monkeypatch, on_console, login_done, locked, eligible):
    import Quartz
    session = {
        Quartz.kCGSessionOnConsoleKey: on_console,
        Quartz.kCGSessionLoginDoneKey: login_done,
        "CGSSessionScreenIsLocked": locked,
    }
    monkeypatch.setattr(Quartz, "CGSessionCopyCurrentDictionary", lambda: session)
    window = A.NSWindow.alloc().initWithContentRect_styleMask_backing_defer_(
        A.NSMakeRect(100, 100, 100, 50), A.NSWindowStyleMaskBorderless,
        A.NSBackingStoreBuffered, False)
    window.setReleasedWhenClosed_(False)
    try:
        button = A.NSButton.buttonWithTitle_target_action_("", None, None)
        button.setFrame_(A.NSMakeRect(10, 10, 24, 24))
        window.contentView().addSubview_(button)
        power_ui.status_item = SimpleNamespace(button=lambda: button, isVisible=lambda: True)
        anchor = power_ui._help_anchor()
        assert (anchor is not None) is eligible
        if eligible:
            assert anchor == (window.convertRectToScreen_(button.convertRect_toView_(button.bounds(), None)),
                              window.screen())
    finally:
        window.close()



def test_ui_help_deduplicates_dismissal_and_rearms_only_after_real_power_transition(power_ui, monkeypatch):
    button = A.NSButton.buttonWithTitle_target_action_("", None, None)
    power_ui.status_item = SimpleNamespace(button=lambda: button, isVisible=lambda: True)
    monkeypatch.setattr(power_ui, "_help_anchor", lambda: object())
    asks = []

    def show_help():
        asks.append(power_ui.presentation.state)
        power_ui.help_showing = True
        power_ui.help_deadline = 20

    monkeypatch.setattr(power_ui, "_show_help", show_help)
    low = reading(on_battery=True, percent=19)
    power_ui.present(low, allow_help=True)
    assert power_ui.help_showing
    power_ui.tick(21)
    assert not power_ui.help_showing
    for sample in (low, reading(available=False), low, reading(on_battery=True, percent=23), low):
        power_ui.present(sample, allow_help=True)
    assert asks == ["low"]
    power_ui.present(reading(charging=True), allow_help=True)
    power_ui.present(low, allow_help=True)
    assert asks == ["low", "low"]
    power_ui.close()
    power_ui.present(low, allow_help=True)
    assert asks == ["low", "low"]


def test_power_help_panel_cannot_become_key_or_main():
    panel = PowerHelpPanel.alloc().initWithContentRect_styleMask_backing_defer_(
        A.NSMakeRect(0, 0, 306, 112),
        A.NSWindowStyleMaskBorderless | A.NSWindowStyleMaskNonactivatingPanel,
        A.NSBackingStoreBuffered, False)
    try:
        assert not panel.canBecomeKeyWindow()
        assert not panel.canBecomeMainWindow()
    finally:
        panel.close()


