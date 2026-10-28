"""Synthetic power readings exercise actual AppKit controls and CA animations."""
import pytest
from types import SimpleNamespace

A = pytest.importorskip("AppKit")
from inklow.appkit_power import NativePowerUI, PowerHelpPanel, charging_help_text


def reading(*, available=True, has_battery=True, on_battery=False, charging=False, percent=65):
    return dict(available=available, has_battery=has_battery, on_battery=on_battery, charging=charging, percent=percent)


@pytest.fixture
def power_ui():
    A.NSApplication.sharedApplication()
    ui = NativePowerUI.alloc().init().configure()
    try:
        yield ui
    finally:
        try:
            ui.close()
        finally:
            if ui.help_panel is not None:
                ui.help_panel.close()


@pytest.fixture
def owned_help_anchor(power_ui, monkeypatch):
    """Keep real help panels off every display without faking session consent."""
    import Quartz

    def session_is_eligible():
        session = Quartz.CGSessionCopyCurrentDictionary()
        return bool(
            session
            and not session.get("CGSSessionScreenIsLocked")
            and session.get(Quartz.kCGSessionOnConsoleKey, False)
            and session.get(Quartz.kCGSessionLoginDoneKey, False)
        )

    if not session_is_eligible():
        pytest.skip("Native help requires the real unlocked, logged-in console")
    screens = list(A.NSScreen.screens())
    if not screens:
        pytest.skip("Native help requires an available display")
    # Only screen/anchor geometry is synthetic. Native content, ordering and
    # target/action dispatch remain real, entirely outside the display union.
    x = max(A.NSMaxX(screen.frame()) for screen in screens) + 4096
    frame = A.NSMakeRect(x, 4096, 640, 480)
    screen = SimpleNamespace(visibleFrame=lambda: frame)
    window = A.NSWindow.alloc().initWithContentRect_styleMask_backing_defer_(
        A.NSMakeRect(x + 200, 4400, 100, 50),
        A.NSWindowStyleMaskBorderless, A.NSBackingStoreBuffered, False)
    window.setReleasedWhenClosed_(False)
    try:
        button = A.NSButton.buttonWithTitle_target_action_("", None, None)
        button.setFrame_(A.NSMakeRect(10, 10, 24, 24))
        window.contentView().addSubview_(button)
        power_ui.status_item = SimpleNamespace(button=lambda: button, isVisible=lambda: True)

        def anchor():
            # Recheck the real session at each lookup, including the presenter's
            # second lookup, rather than bypassing a lock that occurs mid-test.
            if not session_is_eligible() or not power_ui.session_active:
                return None
            bounds = button.convertRect_toView_(button.bounds(), None)
            return window.convertRectToScreen_(bounds), screen

        monkeypatch.setattr(power_ui, "_help_anchor", anchor)
        yield window
    finally:
        power_ui.dismiss_help()
        power_ui.status_item = None
        window.close()

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


@pytest.mark.parametrize("percent,state,symbol", [
    (19, "low", "battery.25percent"),
    (20, "low", "battery.25percent"),
    (21, "battery", "battery.75percent"),
])
def test_battery_card_includes_twenty_percent_in_low_state(power_ui, percent, state, symbol):
    power_ui.make_card()
    result = power_ui.present(reading(on_battery=True, percent=percent), allow_help=False)
    _, icon, title, _ = power_ui.cards["pane"]
    assert result.state == state
    assert result.symbol == symbol
    assert f"{percent}%" in title.stringValue()
    expected_icon = A.NSImage.imageWithSystemSymbolName_accessibilityDescription_(symbol, result.title)
    actual_pixels = icon.image().TIFFRepresentation()
    assert actual_pixels is not None
    assert actual_pixels == expected_icon.TIFFRepresentation()
    expected_tint = A.NSColor.systemOrangeColor() if state == "low" else A.NSColor.labelColor()
    assert icon.contentTintColor().isEqual_(expected_tint)
    assert power_ui.help_panel is None
    assert not power_ui.help_showing


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


def test_actual_help_presenter_is_nonactivating_and_native_dismissal_hides_it(
        power_ui, owned_help_anchor):
    app = A.NSApplication.sharedApplication()
    active, key, main = app.isActive(), app.keyWindow(), app.mainWindow()
    power_ui.present(reading(on_battery=True, percent=19), allow_help=True)

    panel = power_ui.help_panel
    assert panel is not None
    assert panel.isVisible()
    assert power_ui.help_showing
    assert power_ui.help_deadline > 0
    assert not panel.canBecomeKeyWindow()
    assert not panel.canBecomeMainWindow()
    assert not panel.isKeyWindow()
    assert not panel.isMainWindow()
    assert app.isActive() == active
    assert app.keyWindow() == key
    assert app.mainWindow() == main
    assert all(not A.NSIntersectsRect(panel.frame(), screen.frame()) for screen in A.NSScreen.screens())

    content = panel.contentView().contentView()
    dismiss = next(view for view in content.subviews() if isinstance(view, A.NSButton))
    assert dismiss.isEnabled() and not dismiss.isHidden()
    dismiss.performClick_(None)

    assert not panel.isVisible()
    assert not power_ui.help_showing
    assert power_ui.help_deadline == 0
    assert app.isActive() == active
    assert app.keyWindow() == key
    assert app.mainWindow() == main


def test_actual_help_presenter_without_anchor_creates_no_surface(power_ui):
    # Call the real presenter even without a resident status item. A fatal fault
    # before anchor lookup must fail here, not hide behind present()'s gating.
    power_ui._show_help()
    assert power_ui.help_panel is None
    assert not power_ui.help_showing
    assert power_ui.help_deadline == 0


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


