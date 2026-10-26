"""Focused native contract scenarios; no real app, permissions or user text.

Requirements/risk map before generation:
R1 privacy before values -> denied app/domain never reaches AXValue (P0).
R2 exact replacement -> stale/protected/range-invalid refuses native edits (P0).
R3 Unicode -> UTF-16 surrogate interior refused; emoji suffix uses scalar range (P0).
R4 shortcut passthrough -> no-candidate false, modifiers exact, AltTab reserved (P0).
R5 acceptance options -> withheld punctuation remains insertible on next press (P1).
R6 selection semantics -> selected slice differs from post-selection suffix (P0).

Oracles: fake native boundary records attributes/edits; forbidden reads raise;
state keys and exact strings prove range/acceptance behavior. Parametrization
covers negative boundaries without invoking real hooks. Human review/execution
is reserved for the integration owner; these cases are not live compatibility proof.
"""
from dataclasses import replace
from types import SimpleNamespace

import pytest

from inklow.core import Snapshot, SuggestionState, take_word
from inklow.native.base import BridgeBase, index_to_utf16, utf16_to_index
from inklow.native.macos import MacBridge
from inklow.native import macos
from inklow.native.linux import LinuxBridge
from inklow import shortcuts
from inklow.shortcuts import normalize_shortcut, shortcut_from_event, mac_above_tab_keycode, mac_key_name


def test_selection_snapshot_has_unambiguous_rewrite_slices():
    snapshot = Snapshot("app", "field", "a😀word!", 2, 4, domain="example.com", multiline=False)
    assert snapshot.prefix == "a😀"
    assert snapshot.selected_text == "word"
    assert snapshot.suffix == "word!"
    assert snapshot.selection_suffix == "!"
    assert not snapshot.eligible(1)
    assert snapshot.key != replace(snapshot, domain="other.example").key
    assert snapshot.key != replace(snapshot, multiline=True).key


@pytest.mark.parametrize("full", [False, True])
def test_withheld_punctuation_can_be_accepted_on_second_press(full):
    state = SuggestionState()
    before = Snapshot("app", "field", "Hello", 5)
    assert state.offer(state.generation, before, " world, ")
    assert state.accept(before, full=full, trailing_space=False, trailing_punctuation=False) == " world"
    assert state.text == ", "
    assert state.accept(state.snapshot, full=full, trailing_space=False, trailing_punctuation=False) == ","
    assert state.text == ""


def test_default_word_acceptance_keeps_existing_spacing_and_punctuation():
    assert take_word(" world, next") == (" world, ", "next")
    assert take_word(" world, next", trailing_space=False) == (" world,", " next")


def test_suggestion_advance_refuses_domain_change():
    state = SuggestionState()
    before = Snapshot("app", "field", "Hello", 5, domain="example.com")
    assert state.offer(state.generation, before, " world")
    changed = replace(before, text="Hello w", caret=7, domain="denied.example")
    assert not state.advance(changed)


@pytest.mark.parametrize("chord, expected", [("control+option+super+abovetab", "Ctrl+Alt+Super+AboveTab"),
                                             ("shift+a", "Shift+A"), ("", ""), ("F12", "F12")])
def test_shortcut_normalization(chord, expected):
    assert normalize_shortcut(chord) == expected


@pytest.mark.parametrize("chord", ["Ctrl", "Ctrl++Tab", "Ctrl+Ctrl+A", "A+B", "Alt+Unknown", "F13"])
def test_shortcut_normalization_refuses_ambiguous_input(chord):
    with pytest.raises(ValueError):
        normalize_shortcut(chord)


@pytest.mark.parametrize("platform, code", [("macos", 10), ("windows", 41), ("linux", 49)])
def test_recorder_preserves_physical_above_tab_independent_of_glyph(platform, code, monkeypatch):
    monkeypatch.setattr(shortcuts, "mac_above_tab_keycode", lambda: 10)
    assert shortcut_from_event("§", ("Control",), platform=platform, keycode=code) == "Ctrl+AboveTab"


@pytest.mark.parametrize("geometry, code, other", [(b"ISO ", 10, 50), (b"ANSI", 50, 10)])
def test_macos_above_tab_does_not_hijack_the_other_geometry_key(geometry, code, other, monkeypatch):
    assert mac_above_tab_keycode(int.from_bytes(geometry, "big")) == code
    monkeypatch.setattr(shortcuts, "mac_above_tab_keycode", lambda: code)
    assert mac_key_name(code) == "AboveTab"
    assert mac_key_name(other) == ""


def test_chords_work_without_suggestion_and_false_callback_passes_through():
    bridge = BridgeBase()
    calls = []
    bridge.start_shortcuts(lambda action: calls.append(action) or False)
    bridge.set_shortcuts_enabled(True)
    assert bridge._key_action("AboveTab", ("Ctrl",)) == "force"
    assert not bridge._dispatch("force")
    assert calls == ["force"]
    assert bridge._key_action("Tab", ("Super",)) is None
    assert bridge._key_action("Escape", ("Ctrl",)) is None
    assert bridge._key_action("1") == "choice:1"
    assert bridge._key_action("9") == "choice:9"


@pytest.mark.parametrize("platform", ["windows", "linux"])
def test_os_alt_tab_is_never_intercepted_even_if_configured(platform):
    bridge = BridgeBase()
    bridge._shortcuts = {**bridge._shortcuts, "word": "Alt+Tab"}
    assert bridge._key_action("Tab", ("Alt",), platform=platform) is None
    assert bridge._key_action("Tab", ("Alt",), platform="macos") == "bypass"


def test_app_tab_policy_preserves_navigation():
    bridge = BridgeBase()
    bridge.foreground = lambda: {"app_id": "editor", "name": "Editor", "domain": "example.com"}
    bridge._app_rules = {"domain:example.com": {"tab_enabled": False}}
    assert bridge._key_action("Tab") is None
    assert bridge._key_action("AboveTab", ("Ctrl",)) == "force"


def test_domain_metadata_only_accepts_explicit_http_urls():
    assert BridgeBase._domain("https://BÜCHER.example/a") == "xn--bcher-kva.example"
    for value in ("Document - example.com", "example.com", "file:///private/file", None):
        assert BridgeBase._domain(value) == ""


def test_policy_exact_app_and_domain_denials_cannot_be_forced():
    bridge = BridgeBase()
    bridge._app_rules = {"app:editor": {"enabled": False}, "domain:secret.example": {"enabled": False}}
    assert not bridge._allowed("editor")
    assert not bridge._allowed("browser", "secret.example")
    assert bridge._allowed("browser", "")
    assert bridge._allowed("editor-other")


def test_mac_denied_domain_is_checked_before_focused_control_or_value_read():
    bridge = MacBridge.__new__(MacBridge)
    BridgeBase.__init__(bridge)
    bridge.ax = SimpleNamespace(AXIsProcessTrusted=lambda: True)
    bridge._foreground = lambda: (1234, "/Browser", {"app_id": "browser", "domain": "secret.example"})
    bridge._app_rules = {"domain:secret.example": {"enabled": False}}
    bridge._attribute = lambda *args: pytest.fail("AX field read after denied domain")
    assert bridge.capture() is None


@pytest.mark.parametrize("snapshot,start,length", [
    (Snapshot("app", "field", "hello", 5, protected=True), 0, 1),
    (Snapshot("app", "field", "hello", 5), -1, 1),
    (Snapshot("app", "field", "hello", 5), 4, 2),
    (Snapshot("app", "field", "hello", 5), 0, -1),
])
def test_invalid_replacements_fail_before_native_capture(snapshot, start, length):
    bridge = MacBridge.__new__(MacBridge)
    BridgeBase.__init__(bridge)
    bridge._capture = lambda: pytest.fail("Invalid replacement reached capture")
    assert not bridge.replace_range(snapshot, start, length, "x")


def test_mac_stale_replacement_never_sets_selected_text():
    bridge = MacBridge.__new__(MacBridge)
    BridgeBase.__init__(bridge)
    snapshot = Snapshot("app", "field", "say :-)", 7)
    bridge._capture = lambda: (replace(snapshot, text="changed"), object())
    bridge._settable = lambda *args: pytest.fail("Stale replacement reached setter")
    assert not bridge.replace_range(snapshot, 4, 3, "🙂")


def test_mac_exact_selected_text_replacement_does_not_rewrite_document():
    bridge = MacBridge.__new__(MacBridge)
    BridgeBase.__init__(bridge)
    snapshot = Snapshot("app", "field", "a😀word!", 2, 4)
    writes = []
    field = object()
    bridge._capture = lambda: (snapshot, field)
    bridge._settable = lambda element, attribute: attribute == "AXSelectedText"
    bridge.ax = SimpleNamespace(AXUIElementSetMessagingTimeout=lambda field, seconds: 0,
                               AXUIElementSetAttributeValue=lambda element, attribute, text:
                                writes.append((element, attribute, text)) or 0)
    assert bridge.replace_selection(snapshot, "term")
    assert writes == [(field, "AXSelectedText", "term")]
    assert not bridge.insert(snapshot, "term")


def test_utf16_offsets_are_scalar_safe_for_suffix_ranges():
    text = "a😀:-)"
    assert index_to_utf16(text, 2) == 3
    assert utf16_to_index(text, 3) == 2
    with pytest.raises(ValueError):
        utf16_to_index(text, 2)


def test_linux_stale_range_replacement_never_deletes():
    bridge = LinuxBridge.__new__(LinuxBridge)
    BridgeBase.__init__(bridge)
    before = Snapshot("app", "field", "a😀:-)", 5)
    bridge._capture = lambda: (replace(before, caret=4), None, None)
    assert not bridge.replace_range(before, 2, 3, "🙂")


def test_linux_nonatomic_replacement_refuses_before_deleting_original_text():
    bridge = LinuxBridge.__new__(LinuxBridge)
    BridgeBase.__init__(bridge)
    snapshot = Snapshot("app", "field", "a😀:-)", 5)
    forbidden = SimpleNamespace(
        deleteText=lambda *args: pytest.fail("Original text was deleted"),
        insertText=lambda *args: pytest.fail("Replacement attempted without an atomic operation"))
    bridge._capture = lambda: (snapshot, forbidden, None)
    assert not bridge.replace_range(snapshot, 2, 3, "🙂")


def quartz_bridge(monkeypatch, callback):
    """MacBridge with a recording Quartz double; the tap thread body runs inline."""
    import sys
    from contextlib import nullcontext
    monkeypatch.setitem(sys.modules, "objc", SimpleNamespace(autorelease_pool=nullcontext))
    monkeypatch.setattr(shortcuts, "mac_above_tab_keycode", lambda: 10)
    monkeypatch.setattr(macos.threading, "Timer", lambda interval, callback:
                        SimpleNamespace(start=lambda: None, cancel=lambda: None))
    bridge = MacBridge.__new__(MacBridge)
    BridgeBase.__init__(bridge)
    bridge._refresh_status = lambda: None
    bridge.ax = SimpleNamespace(AXIsProcessTrusted=lambda: True,
                                AXUIElementSetMessagingTimeout=lambda field, seconds: 0)
    bridge.secure_input_active = lambda: False
    bridge._event_type = SimpleNamespace(eventWithCGEvent_=lambda event:
                                         SimpleNamespace(charactersIgnoringModifiers=lambda: "a"))
    posted, enabled = [], []
    fields = {10: "code", 12: "tag", 13: "repeat"}
    bridge.q = SimpleNamespace(
        kCGEventTapDisabledByTimeout=-1, kCGEventTapDisabledByUserInput=-2,
        kCGEventKeyDown=1, kCGEventLeftMouseDown=2, kCGEventRightMouseDown=3,
        kCGEventOtherMouseDown=4, kCGEventScrollWheel=5,
        kCGEventKeyUp=6, kCGEventFlagsChanged=7, kCGEventLeftMouseUp=8,
        kCGEventRightMouseUp=9, kCGEventOtherMouseUp=10, kCGEventMouseMoved=11,
        kCGEventLeftMouseDragged=12, kCGEventRightMouseDragged=13, kCGEventOtherMouseDragged=14,
        kCGEventFlagMaskControl=1, kCGEventFlagMaskAlternate=2,
        kCGEventFlagMaskShift=4, kCGEventFlagMaskCommand=8, kCGEventFlagMaskSecondaryFn=16,
        kCGKeyboardEventKeycode=10, kCGEventSourceUnixProcessID=11, kCGEventSourceUserData=12,
        kCGKeyboardEventAutorepeat=13, kCGSessionEventTap=0, kCGHeadInsertEventTap=0,
        kCGTailAppendEventTap=1, kCGEventTapOptionDefault=0, kCGEventTapOptionListenOnly=1,
        CGEventGetFlags=lambda event: event["flags"],
        CGEventGetIntegerValueField=lambda event, field: event.get(fields.get(field), 0),
        CGEventSetFlags=lambda event, flags: event.update(flags=flags),
        CGEventKeyboardGetUnicodeString=lambda event, size, length, chars:
            (len(event.get("chars", "x")), event.get("chars", "x")),
        CGEventTapEnable=lambda tap, value: enabled.append((tap, value)),
        CGEventTapCreate=lambda location, place, option, mask, callback, refcon: f"tap{option}",
        CGPreflightPostEventAccess=lambda: True,
        CGEventCreateCopy=lambda event: dict(event),
        CGEventCreateKeyboardEvent=lambda source, code, down: {"code": code, "down": down},
        CGEventSetIntegerValueField=lambda event, field, value: event.update({fields[field]: value}),
        CGEventPost=lambda location, event: posted.append(event))
    bridge.cf = SimpleNamespace(
        CFMachPortCreateRunLoopSource=lambda *args: object(),
        CFRunLoopGetCurrent=lambda: object(), kCFRunLoopCommonModes=0,
        CFRunLoopAddSource=lambda *args: None, CFRunLoopRun=lambda: None,
        CFRunLoopRemoveSource=lambda *args: None, CFMachPortInvalidate=lambda *args: None)
    BridgeBase.start_shortcuts(bridge, callback)
    bridge.set_shortcuts_enabled(True)
    bridge._event_loop()
    return bridge, posted, enabled


def test_quartz_keyboard_path_dispatches_configured_force_and_safe_option_tab(monkeypatch):
    calls = []
    bridge, _, _ = quartz_bridge(monkeypatch, lambda action: calls.append(action) or action in ("force", "bypass"))
    callback = bridge._event_callback
    tab = {"code": 48, "flags": 0}
    assert callback(None, 1, tab, None) is tab
    force = {"code": 10, "flags": 1}
    assert callback(None, 1, force, None) is None
    bridge.finish_input_transaction(bridge.input_transaction(), True)
    bridge.drain_input_transaction()
    bypass = {"code": 48, "flags": 2}
    assert callback(None, 1, bypass, None) is bypass
    assert bypass["flags"] == 0
    modified_tab = {"code": 48, "flags": 8}
    assert callback(None, 1, modified_tab, None) is modified_tab
    assert calls == ["word", "force", "bypass"]


# Arming/replay map (P0): a consumed shortcut must not invalidate the arming
# it used, but any passed-through key must; BroTypist's own tagged events are
# invisible to dispatch and revisions; a deferred failure replays exactly once.
def test_quartz_revisions_tagged_events_and_single_replay(monkeypatch):
    calls = []
    bridge, posted, _ = quartz_bridge(monkeypatch, lambda action: calls.append(action) or action == "word")
    callback = bridge._event_callback
    revision = bridge.user_input_revision()
    assert callback(None, 1, {"code": 48, "flags": 0}, None) is None
    assert bridge.user_input_revision() == revision
    synthetic = {"code": 48, "flags": 0, "tag": macos.SYNTHETIC_TAG}
    assert callback(None, 1, synthetic, None) is synthetic
    assert calls == ["word"] and bridge.user_input_revision() == revision
    gate = bridge.input_transaction()
    up = {"code": 48, "flags": 0}
    ordinary = {"code": 0, "flags": 0}
    assert callback(None, bridge.q.kCGEventKeyUp, up, None) is None
    assert callback(None, 1, ordinary, None) is None
    bridge.finish_input_transaction(gate, False)
    bridge.drain_input_transaction()
    assert [(event["code"], event["tag"]) for event in posted] == [
        (48, macos.SYNTHETIC_TAG), (48, macos.SYNTHETIC_TAG), (0, macos.SYNTHETIC_TAG)]
    assert bridge.user_input_revision() > revision
    assert bridge.input_transaction() is None


def test_quartz_key_callback_never_waits_for_accessibility_capture(monkeypatch):
    import threading
    bridge, _, _ = quartz_bridge(monkeypatch, lambda action: True)
    bridge.foreground = lambda: pytest.fail("Key tap performed foreground accessibility work")
    bridge._front = {"app_id": "editor", "domain": ""}
    finished = threading.Event()
    with bridge._lock:  # An in-flight AX capture holds the platform lock.
        worker = threading.Thread(target=lambda: bridge._event_callback(
            None, 1, {"code": 48, "flags": 0}, None) is None and finished.set(), daemon=True)
        worker.start()
        worker.join(1)
    assert finished.is_set()


def test_quartz_timeout_recovers_automatically_then_stops_when_repeated(monkeypatch):
    bridge, _, enabled = quartz_bridge(monkeypatch, lambda action: False)
    bridge._tap = "tap0"
    enabled.clear()
    for _ in range(3):
        bridge._event_callback(None, -1, {}, None)
    assert enabled == [("tap0", True)] * 3 and bridge._enabled
    bridge._event_callback(None, -1, {}, None)
    assert not bridge._enabled and enabled[-1] == ("tap0", False)
    assert "timed out" in bridge.status()


@pytest.mark.parametrize("guard", ["secure", "protected", "readonly", "disabled", "hidden", "composition", "unknown",
                                   "secure_input", "dead_key", "unobservable_ime"])
def test_mac_security_guards_precede_editable_value_access(guard):
    bridge = MacBridge.__new__(MacBridge)
    BridgeBase.__init__(bridge)
    bridge._secure_description = "Localized secure role"
    bridge._foreground = lambda: (1234, "/Editor", {"app_id": "editor", "domain": ""})
    bridge._settable = lambda *args: guard != "readonly"
    bridge.secure_input_active = lambda: guard == "secure_input"
    bridge._dead_key = guard == "dead_key"
    bridge._ime_kind = "unknown" if guard == "unobservable_ime" else "layout"
    attrs = {"AXFocusedUIElement": "field", "AXRole": "AXTextField",
             "AXSubrole": "", "AXRoleDescription": "Text field",
             "AXProtectedContent": False, "AXEnabled": True, "AXFocused": True,
             "AXSelectedTextRange": (0, 0)}
    if guard == "secure":
        attrs["AXSubrole"] = "AXSecureTextField"
    elif guard == "protected":
        attrs["AXProtectedContent"] = True
    elif guard == "disabled":
        attrs["AXEnabled"] = False
    elif guard == "hidden":
        attrs["AXHidden"] = True
    elif guard == "unknown":
        attrs["AXRole"] = "AXUnknown"
    elif guard == "composition":
        attrs["AXMarkedTextRange"] = "marked"
    def attribute(element, name):
        if name == "AXValue":
            pytest.fail("Protected/unsupported field value was read")
        return attrs.get(name)
    bridge._attribute = attribute
    bridge.ax = SimpleNamespace(AXIsProcessTrusted=lambda: True,
                                AXUIElementCreateApplication=lambda pid: "application",
                                kAXValueCFRangeType=1,
                                AXValueGetValue=lambda *args: (True, (0, 1)))
    assert bridge.capture() is None


def test_mac_dead_key_blocks_capture_until_a_composed_character_arrives(monkeypatch):
    bridge, _, _ = quartz_bridge(monkeypatch, lambda action: False)
    bridge._event_callback(None, 1, {"code": 14, "flags": 2, "chars": ""}, None)
    assert bridge._dead_key
    bridge._event_callback(None, 1, {"code": 14, "flags": 0, "chars": "é"}, None)
    assert not bridge._dead_key


@pytest.mark.parametrize("app_id, browser", [
    ("company.thebrowser.Browser", True), ("company.thebrowser.dia", True),
    ("app.zen-browser.zen", True), ("com.kagi.kagimacOS", True),
    ("/usr/bin/arc", False), ("/usr/bin/dia", False), ("/Applications/Orion.app/x/zen", False),
])
def test_browser_identity_is_exact_without_short_name_false_positives(app_id, browser):
    assert BridgeBase._is_browser(app_id) is browser


def test_code_editors_stay_silent_unless_the_app_rule_opts_in():
    bridge = BridgeBase()
    assert not bridge._allowed("com.microsoft.VSCode")
    assert not bridge._allowed("com.jetbrains.pycharm")
    bridge._app_rules = {"app:com.microsoft.VSCode": {"complete_in_code_editors": True}}
    assert bridge._allowed("com.microsoft.VSCode")
    assert bridge._allowed("com.apple.TextEdit")


def test_policy_default_app_domain_precedence_never_overrides_global_or_exclusions():
    bridge = BridgeBase()
    bridge._default_enabled = False
    bridge._app_rules = {"app:editor": {"enabled": True}, "domain:secret.example": {"enabled": False}}
    assert bridge._allowed("editor")
    assert not bridge._allowed("other")
    assert not bridge._allowed("editor", "secret.example")
    bridge._app_rules = {"app:editor": {"enabled": False}, "domain:secret.example": {"enabled": True}}
    assert bridge._allowed("editor", "secret.example")
    bridge._global_enabled = False
    assert not bridge._allowed("editor", "secret.example")
    bridge._global_enabled = True
    bridge.set_excluded_apps(("editor",))
    assert not bridge._allowed("editor", "secret.example")


def test_windows_native_range_replacement_uses_utf16_bounded_edit_messages():
    from contextlib import nullcontext
    from inklow.native.windows import WindowsBridge
    bridge = WindowsBridge.__new__(WindowsBridge)
    BridgeBase.__init__(bridge)
    snapshot = Snapshot("app", "field", "a😀:-)", 5)
    selected = replace(snapshot, caret=2, selection_length=3)
    captures = iter(((snapshot, 123), (selected, 123)))
    bridge._capture = lambda automation: next(captures)
    bridge._automation = lambda: nullcontext(object())
    messages = []
    bridge._message = lambda *args: messages.append(args) or 1
    assert bridge.replace_range(snapshot, 2, 3, "🙂")
    assert messages[0] == (123, 0x00B1, 3, 6)
    assert [message[1] for message in messages] == [0x00B1, 0x00C2]


def test_windows_uia_unknown_units_refuse_arbitrary_suffix_movement():
    from contextlib import nullcontext
    from inklow.native.windows import WindowsBridge
    bridge = WindowsBridge.__new__(WindowsBridge)
    BridgeBase.__init__(bridge)
    snapshot = Snapshot("app", "field", "a😀:-)", 5)
    bridge._capture = lambda automation: (snapshot, None)
    bridge._automation = lambda: nullcontext(object())
    bridge._send_unicode = lambda text: pytest.fail("Unknown provider range caused input")
    assert not bridge.replace_range(snapshot, 2, 3, "🙂")
    assert "cannot safely select" in bridge.status()


@pytest.mark.parametrize("browser", ["com.apple.Safari", "C:\\Program Files\\Browser\\chrome.exe",
                                   "/usr/lib/firefox/firefox", "/opt/google/chrome/chrome"])
def test_unknown_browser_url_fails_closed_for_domain_exclusion_but_not_other_apps(browser):
    bridge = BridgeBase()
    bridge._app_rules = {"domain:private.example": {"enabled": False}}
    assert not bridge._allowed(browser, "")
    assert "no editable text was read" in bridge.status()
    assert bridge._allowed("ordinary-editor", "")
    assert bridge._allowed(browser, "public.example")
    assert not bridge._allowed(browser, "private.example")


def test_unknown_mac_browser_url_is_denied_before_focused_control_access():
    bridge = MacBridge.__new__(MacBridge)
    BridgeBase.__init__(bridge)
    bridge._foreground = lambda: (1234, "/Safari", {"app_id": "com.apple.Safari", "domain": ""})
    bridge._app_rules = {"domain:private.example": {"enabled": False}}
    bridge.ax = SimpleNamespace(AXIsProcessTrusted=lambda: True)
    bridge._attribute = lambda *args: pytest.fail("Unknown browser exclusion read a field")
    assert bridge.capture() is None


def test_windows_hook_accepts_exact_control_chord_but_preserves_altgr_and_false_callbacks():
    import ctypes
    from inklow.native.windows import WindowsBridge, KBDLLHOOKSTRUCT
    bridge = WindowsBridge.__new__(WindowsBridge)
    BridgeBase.__init__(bridge)
    bridge._hook = None
    bridge._hook_thread_id = 0
    bridge._hook_type = lambda callback: callback
    bridge.kernel = SimpleNamespace(GetCurrentThreadId=lambda: 123)
    keys = set()
    bridge.user = SimpleNamespace(
        GetAsyncKeyState=lambda key: 0x8000 if key in keys else 0,
        PeekMessageW=lambda *args: None, GetMessageW=lambda *args: 0,
        CallNextHookEx=lambda *args: 42)
    calls = []
    BridgeBase.start_shortcuts(bridge, lambda action: calls.append(action) or action == "force")
    bridge._event_loop()
    BridgeBase.set_shortcuts_enabled(bridge, True)
    bridge._shortcuts = {**bridge._shortcuts, "force": "Ctrl+Alt+A"}
    event = KBDLLHOOKSTRUCT(vkCode=0x41, scanCode=30)
    keys.update((0x11, 0x12, 0xA5))
    assert bridge._hook_callback(0, 0x0100, ctypes.addressof(event)) == 42
    assert calls == []
    keys.remove(0xA5)
    assert bridge._hook_callback(0, 0x0100, ctypes.addressof(event)) == 1
    assert calls == ["force"]
    keys.clear()
    event.vkCode, event.scanCode = 9, 15
    assert bridge._hook_callback(0, 0x0100, ctypes.addressof(event)) == 42
    assert calls == ["force", "word"]


@pytest.mark.parametrize("lost_focus", ["password", "unfocused", "another_field", "another_process", "missing"])
@pytest.mark.parametrize("selected", [False, True])
def test_windows_uia_refuses_input_when_focus_changes_during_capture(lost_focus, selected):
    from contextlib import nullcontext
    from inklow.native.windows import WindowsBridge
    bridge = WindowsBridge.__new__(WindowsBridge)
    BridgeBase.__init__(bridge)
    snapshot = Snapshot("app", "42:uia:7:8", "useful", 0 if selected else 6,
                        6 if selected else 0)
    focused = SimpleNamespace(CurrentIsPassword=lost_focus == "password",
                              CurrentHasKeyboardFocus=lost_focus != "unfocused",
                              CurrentProcessId=43 if lost_focus == "another_process" else 42,
                              GetRuntimeId=lambda: (9, 8) if lost_focus == "another_field" else (7, 8))
    automation = SimpleNamespace(GetFocusedElement=lambda: None if lost_focus == "missing" else focused)
    bridge._capture = lambda automation: (snapshot, None)
    bridge._automation = lambda: nullcontext(automation)
    bridge._send_unicode = lambda text: pytest.fail("Input reached a changed or protected UIA focus")
    if selected:
        assert not bridge.replace_selection(snapshot, "helpful")
    else:
        assert not bridge.insert(snapshot, " next")


@pytest.mark.parametrize("text,caret,location,glyph_range", [
    ("abc", 3, 3, (2, 1)), ("a😀", 2, 3, (1, 2)), ("a😀b", 2, 3, (3, 1)),
])
def test_mac_caret_uses_real_glyph_row_without_moving_its_horizontal_position(text, caret, location, glyph_range):
    bridge = MacBridge.__new__(MacBridge)
    rectangles = {(location, 0): ((100, 40), (0, 20)),
                  glyph_range: ((80, 60), (20, 20))}
    bridge.ax = SimpleNamespace(
        kAXValueCFRangeType=1, kAXValueCGRectType=2,
        AXValueCreate=lambda kind, value: value,
        AXUIElementCopyParameterizedAttributeValue=lambda field, name, value, output: (0, rectangles[value]),
        AXValueGetValue=lambda value, kind, output: (True, value))
    assert bridge._caret_bounds(object(), text, caret, location) == (100, 60, 1, 20)


# Field discovery regression map: geometry survives empty text (P1); missing
# optional focus flags still require authoritative app focus (P0); explicit
# denial, changed focus, and unsupported geometry fail closed (P0). AX doubles
# provide only synthetic text and assert returned snapshots / forbidden reads.
def mac_capture_boundary(text="", **overrides):
    bridge = MacBridge.__new__(MacBridge)
    BridgeBase.__init__(bridge)
    bridge._secure_description = "Localized secure role"
    bridge._foreground = lambda: (1234, "/Editor", {"app_id": "editor", "domain": ""})
    bridge.secure_input_active = lambda: False
    bridge._ime_kind = "layout"
    attributes = {
        ("application", "AXFocusedUIElement"): "field",
        ("field", "AXRole"): "AXTextArea",
        ("field", "AXFocused"): True,
        ("field", "AXEnabled"): True,
        ("field", "AXSelectedTextRange"): (len(text), 0),
        ("field", "AXValue"): text,
        ("field", "AXPosition"): (120, 240),
        ("field", "AXSize"): (400, 180),
    }
    attributes.update({("field", name): value for name, value in overrides.items()})
    reads, writes = [], []
    def copy_attribute(element, name, output):
        reads.append((element, name))
        value = attributes.get((element, name))
        return (0, value) if value is not None else (-25205, None)
    bridge.ax = SimpleNamespace(
        AXIsProcessTrusted=lambda: True,
        AXUIElementSetMessagingTimeout=lambda field, seconds: 0,
        AXUIElementCreateApplication=lambda pid: "application",
        AXUIElementCopyAttributeValue=copy_attribute,
        AXUIElementIsAttributeSettable=lambda element, name, output:
            (0, element == "field" and name == "AXValue"),
        AXUIElementSetAttributeValue=lambda element, name, value:
            writes.append((element, name, value)) or 0,
        kAXValueCFRangeType=1, kAXValueCGRectType=2,
        kAXValueCGPointType=3, kAXValueCGSizeType=4,
        AXValueCreate=lambda kind, value: value,
        AXValueGetValue=lambda value, kind, output: (True, value),
        AXUIElementCopyParameterizedAttributeValue=lambda *args: (-25205, None),
    )
    return bridge, attributes, reads, writes


@pytest.mark.parametrize("description", [
    "", "Chat", "Ask AI", "Chat Input.py, editor content", "Search Chat Input",
    "Chat Input (Agent), edit files in your workspace.py",
])
def test_editor_chat_probe_never_reads_code_or_generic_prompt_fields(description):
    bridge, _, reads, writes = mac_capture_boundary("private source code", AXDescription=description)
    bridge._foreground = lambda: (1234, "/Code", {"app_id": "com.microsoft.VSCode", "domain": ""})
    assert bridge.capture() is None
    assert ("field", "AXValue") not in reads and writes == []


@pytest.mark.parametrize("mode", [
    "Chat Input (Agent), edit files in your workspace..",
    "Chat Input (Edit), edit files in your workspace. Press Enter to send out the request.",
    "Chat Input (Ask), ask questions or type / for topics., Local model. ",
])
def test_chat_to_code_switch_rejects_stale_acceptance_without_reading_code(mode):
    bridge, attributes, reads, writes = mac_capture_boundary("Please explain this issue", AXDescription=mode)
    bridge._foreground = lambda: (1234, "/Code", {"app_id": "com.microsoft.VSCode", "domain": ""})
    snapshot = bridge.capture()
    assert snapshot is not None
    attributes[("field", "AXDescription")] = "main.py, editor content"
    reads.clear()
    assert not bridge.insert(snapshot, " in detail")
    assert ("field", "AXValue") not in reads and writes == []


@pytest.mark.parametrize("rule", [{"enabled": False}, {}])
def test_chat_probe_cannot_bypass_app_disable_or_global_pause(rule):
    bridge, _, reads, writes = mac_capture_boundary(
        "private chat", AXDescription="Chat Input (Agent), edit files in your workspace..")
    bridge._foreground = lambda: (1234, "/Code", {"app_id": "com.microsoft.VSCode", "domain": ""})
    bridge._app_rules = {"app:com.microsoft.VSCode": rule}
    bridge._global_enabled = bool(rule)
    assert bridge.capture() is None
    assert ("application", "AXFocusedUIElement") not in reads and writes == []


def test_mac_empty_field_keeps_native_field_geometry_without_inventing_a_caret():
    bridge, _, _, _ = mac_capture_boundary()
    snapshot = bridge.capture()
    assert snapshot is not None
    assert snapshot.text == ""
    assert snapshot.field_rect == (120, 240, 400, 180)
    assert snapshot.caret_rect is None


@pytest.mark.parametrize("attribute,value", [
    ("AXPosition", None), ("AXSize", None), ("AXSize", (0, 180)),
    ("AXSize", (-1, 180)), ("AXPosition", (float("nan"), 240)),
    ("AXSize", (400, float("inf"))),
])
def test_mac_invalid_field_geometry_does_not_discard_a_supported_field(attribute, value):
    bridge, _, _, _ = mac_capture_boundary(**{attribute: value})
    snapshot = bridge.capture()
    assert snapshot is not None
    assert snapshot.field_rect is None


def test_mac_authoritative_app_focus_allows_missing_optional_focused_flag():
    bridge, _, _, _ = mac_capture_boundary(AXFocused=None)
    assert bridge.capture() is not None


def test_mac_explicitly_unfocused_control_never_reads_its_value():
    bridge, _, reads, _ = mac_capture_boundary(AXFocused=False)
    assert bridge.capture() is None
    assert ("field", "AXValue") not in reads


def test_mac_changed_focus_during_capture_discards_the_snapshot():
    bridge, attributes, _, _ = mac_capture_boundary()
    copy_attribute = bridge.ax.AXUIElementCopyAttributeValue
    def change_focus_after_value(element, name, output):
        result = copy_attribute(element, name, output)
        if name == "AXValue":
            attributes[("application", "AXFocusedUIElement")] = "another_field"
        return result
    bridge.ax.AXUIElementCopyAttributeValue = change_focus_after_value
    assert bridge.capture() is None


def test_mac_electron_accessibility_activation_is_advertised_and_not_repeated():
    bridge, attributes, _, writes = mac_capture_boundary()
    attributes[("application", "AXManualAccessibility")] = False
    is_settable = bridge.ax.AXUIElementIsAttributeSettable
    bridge.ax.AXUIElementIsAttributeSettable = lambda element, name, output: (
        (0, True) if (element, name) == ("application", "AXManualAccessibility")
        else is_settable(element, name, output))
    assert bridge.capture() is not None
    assert bridge.capture() is not None
    assert writes == [("application", "AXManualAccessibility", True)]


def test_mac_ordinary_native_application_never_receives_accessibility_setters():
    bridge, _, _, writes = mac_capture_boundary()
    assert bridge.capture() is not None
    assert writes == []


def test_mac_denied_app_never_activates_accessibility_or_reads_a_field():
    bridge, _, reads, writes = mac_capture_boundary()
    bridge._app_rules = {"app:editor": {"enabled": False}}
    assert bridge.capture() is None
    assert reads == []
    assert writes == []


def test_mac_caret_on_next_visual_row_is_not_moved_back_to_the_previous_glyph():
    bridge = MacBridge.__new__(MacBridge)
    rectangles = {(3, 0): ((100, 60), (0, 20)),
                  (2, 1): ((380, 40), (20, 20))}
    bridge.ax = SimpleNamespace(
        kAXValueCFRangeType=1, kAXValueCGRectType=2,
        AXValueCreate=lambda kind, value: value,
        AXUIElementCopyParameterizedAttributeValue=lambda field, name, value, output: (0, rectangles[value]),
        AXValueGetValue=lambda value, kind, output: (True, value))
    assert bridge._caret_bounds(object(), "abc", 3, 3) == (100, 60, 1, 20)


@pytest.mark.parametrize("app_id,expected", [
    ("com.google.chrome", [("application", "AXEnhancedUserInterface", True)]),
    ("ordinary-editor", []),
])
def test_mac_enhanced_accessibility_is_limited_to_known_chromium_browsers(app_id, expected):
    bridge, attributes, _, writes = mac_capture_boundary()
    bridge._foreground = lambda: (1234, "/Editor", {"app_id": app_id, "domain": ""})
    attributes[("application", "AXEnhancedUserInterface")] = False
    is_settable = bridge.ax.AXUIElementIsAttributeSettable
    bridge.ax.AXUIElementIsAttributeSettable = lambda element, name, output: (
        (0, True) if (element, name) == ("application", "AXEnhancedUserInterface")
        else is_settable(element, name, output))
    assert bridge.capture() is not None
    assert bridge.capture() is not None
    assert writes == expected


def test_mac_already_enabled_accessibility_is_not_toggled():
    bridge, attributes, _, writes = mac_capture_boundary()
    attributes[("application", "AXManualAccessibility")] = True
    assert bridge.capture() is not None
    assert writes == []


def test_mac_caret_at_wrap_boundary_is_not_moved_to_the_following_glyph_row():
    bridge = MacBridge.__new__(MacBridge)
    rectangles = {(2, 0): ((400, 40), (0, 20)),
                  (2, 1): ((100, 60), (20, 20))}
    bridge.ax = SimpleNamespace(
        kAXValueCFRangeType=1, kAXValueCGRectType=2,
        AXValueCreate=lambda kind, value: value,
        AXUIElementCopyParameterizedAttributeValue=lambda field, name, value, output: (0, rectangles[value]),
        AXValueGetValue=lambda value, kind, output: (True, value))
    assert bridge._caret_bounds(object(), "abc", 2, 2) == (400, 40, 1, 20)


def test_mac_changed_focus_during_range_metadata_never_reads_the_old_value():
    bridge, attributes, reads, _ = mac_capture_boundary(AXFocused=None)
    copy_attribute = bridge.ax.AXUIElementCopyAttributeValue
    def change_focus_after_range(element, name, output):
        result = copy_attribute(element, name, output)
        if name == "AXSelectedTextRange":
            attributes[("application", "AXFocusedUIElement")] = "another_field"
        return result
    bridge.ax.AXUIElementCopyAttributeValue = change_focus_after_range
    assert bridge.capture() is None
    assert ("field", "AXValue") not in reads


# Inline contract matrix: P0 non-EOL/selection/protected/invalid-baseline must
# remain silent; P1 native attributed run must preserve exact font and baseline.
# Oracles are observed eligibility and run geometry, not timer/default wiring.
@pytest.mark.parametrize("changes", [
    {"text": "Hello world ", "caret": 5},
    {"selection_length": 1},
    {"protected": True},
    {"inline_render": None},
])
def test_inline_suffix_refuses_obstructed_or_unverified_host_line(changes):
    from inklow.core import InlineRender
    render = InlineRender("HostFont", 13, 56, (0, 0, 400, 200), 16, 4, source="native-run")
    snapshot = Snapshot("editor", "field", "Hello", 5, caret_rect=(100, 40, 1, 20),
                        inline_render=render)
    assert snapshot.inline_safe
    assert not replace(snapshot, **changes).inline_safe


@pytest.mark.parametrize("changes", [
    {"baseline": 2}, {"clip_rect": (0, 0, 100, 200)}, {"direction": "rtl"},
    {"clip_rect": (0, 0, 100.5, 200)},
    {"font_size": float("nan")}, {"ascent": 0}, {"source": ""},
])
def test_inline_run_rejects_clipped_or_unreliable_native_metrics(changes):
    from inklow.core import InlineRender
    render = InlineRender("HostFont", 13, 56, (0, 0, 400, 200), 16, 4, source="native-run")
    assert not replace(render, **changes).valid((100, 40, 1, 20))


def test_native_preedit_is_eligible_only_at_a_collapsed_safe_line_end():
    snapshot = Snapshot("editor", "field", "Hello\nnext", 5, native_inline=True,
                        native_anchor="1:2")
    assert snapshot.inline_safe
    assert not replace(snapshot, caret=4).inline_safe
    assert not replace(snapshot, selection_length=1).inline_safe
    assert not replace(snapshot, protected=True).inline_safe


# Inline metric map (P0): a font is accepted only when CoreText advances of
# two host spans match AX geometry; private system aliases reproduce the
# system font; unverifiable runs return vision evidence instead of a guess;
# mid-line displacement needs measured colours. Widths are synthetic doubles.
def mac_metrics(monkeypatch, text="Hello world", *, ax_name="Host-Regular", widths=None):
    import sys
    bridge = MacBridge.__new__(MacBridge)
    BridgeBase.__init__(bridge)
    fonts = {name: SimpleNamespace(fontName=lambda name=name: name, ascender=lambda: 16, descender=lambda: -4)
             for name in ("Host-Regular", ".SFNS-Regular")}
    layout = SimpleNamespace(defaultLineHeightForFont_=lambda font: 20,
                             defaultBaselineOffsetForFont_=lambda font: 16)
    monkeypatch.setitem(sys.modules, "AppKit", SimpleNamespace(
        NSFont=SimpleNamespace(
            fontWithName_size_=lambda name, size: fonts.get(name) if size == 13 and not name.startswith(".") else None,
            systemFontOfSize_weight_=lambda size, weight: fonts[".SFNS-Regular"] if (size, weight) == (13, 0.0) else None),
        NSLayoutManager=SimpleNamespace(alloc=lambda: SimpleNamespace(init=lambda: layout))))
    advances = {"Hello world": 60.0, " world": 30.0, " tail": 25.0, **(widths or {})}
    monkeypatch.setattr(macos, "text_width", lambda font, value: advances[value])
    attributed = SimpleNamespace(length=lambda: len(text), attributesAtIndex_effectiveRange_=lambda index, out:
                                 ({"AXFont": {"AXFontName": ax_name, "AXFontSize": 13}}, (0, len(text))))
    bridge._attribute = lambda *args: None
    bridge.ax = SimpleNamespace(kAXValueCFRangeType=4, AXValueCreate=lambda kind, value: value,
                                AXUIElementCopyParameterizedAttributeValue=lambda *args: (0, attributed))
    rows = {(0, 11): (40, 40, 60, 20), (5, 6): (70, 40, 30, 20), (11, 5): (100, 40, 25, 20)}
    bridge._range_bounds = lambda field, location, length: rows.get((location, length))
    return bridge


@pytest.mark.parametrize("text,caret,expected", [
    ("abc", 3, (100, 60, 1, 20)),
    ("abc", 2, None),
    ("abc\n", 4, None),
    ("אבג", 3, None),
])
def test_mac_missing_zero_length_caret_uses_only_an_observed_ltr_document_end(
        text, caret, expected):
    bridge = MacBridge.__new__(MacBridge)
    BridgeBase.__init__(bridge)
    bridge._range_bounds = lambda field, location, length: (
        (90, 60, 10, 20) if length == 1 else None)
    assert bridge._caret_bounds("field", text, caret, caret) == expected


@pytest.mark.parametrize("suffix,available", [("", True), (" tail", False)])
def test_mac_missing_host_font_still_draws_at_a_safe_line_end_without_repainting(
        monkeypatch, suffix, available):
    text = "Hello world" + suffix
    bridge = mac_metrics(monkeypatch, text)
    attributed = SimpleNamespace(
        length=lambda: len(text),
        attributesAtIndex_effectiveRange_=lambda index, out: ({}, (0, len(text))))
    bridge.ax.AXUIElementCopyParameterizedAttributeValue = lambda *args: (0, attributed)
    render, _ = bridge._inline_metrics(
        "field", text, 11, 11, (100, 40, 1, 20), (0, 0, 400, 200))
    assert (render is not None) is available
    if render is not None:
        assert render.baseline == 56
        assert render.system_font
        assert render.suffix == ""
        assert Snapshot("editor", "field", text, 11, caret_rect=(100, 40, 1, 20),
                        inline_render=render).inline_safe


@pytest.mark.parametrize("visible,available", [((0, 0), True), ((0, 5), False), ((0, 11), True)])
def test_mac_empty_visible_range_uses_observed_glyphs_not_an_empty_viewport(
        monkeypatch, visible, available):
    bridge = mac_metrics(monkeypatch)
    bridge._attribute = lambda field, name: visible if name == "AXVisibleCharacterRange" else None
    bridge.ax.AXValueGetValue = lambda value, kind, output: (True, value)
    render, _ = bridge._inline_metrics(
        "field", "Hello world", 11, 11, (100, 40, 1, 20), (0, 0, 400, 200))
    assert (render is not None) is available
    if render is not None:
        assert render.baseline == 56


def test_mac_attributed_run_uses_font_baseline_not_caret_height(monkeypatch):
    bridge = mac_metrics(monkeypatch)
    render, evidence = bridge._inline_metrics("field", "Hello world", 11, 11, (100, 40, 1, 20), (0, 0, 400, 200))
    assert (render.font_name, render.font_size, render.baseline, evidence) == ("Host-Regular", 13, 56, None)
    # Enlarging the caret cannot move the verified glyph-row baseline.
    taller, _ = bridge._inline_metrics("field", "Hello world", 11, 11, (100, 40, 1, 30), (0, 0, 400, 200))
    assert taller.baseline == 56


def test_mac_private_system_font_alias_is_reproduced_not_rejected(monkeypatch):
    bridge = mac_metrics(monkeypatch, ax_name=".AppleSystemUIFont")
    render, _ = bridge._inline_metrics("field", "Hello world", 11, 11, (100, 40, 1, 20), (0, 0, 400, 200))
    assert render.system_font and render.font_name == ".SFNS-Regular" and render.system_weight == 0.0


def test_mac_completion_measures_the_caret_row_after_a_soft_wrap(monkeypatch):
    text = "previous row. Hello world"
    bridge = mac_metrics(monkeypatch, text, widths={"rld": 15.0})
    rows = {
        (0, len(text)): (0, 40, 100, 40),
        (13, 12): (0, 40, 100, 40),
        (19, 6): (70, 60, 30, 20),
        (22, 3): (85, 60, 15, 20),
    }
    bridge._range_bounds = lambda field, location, length: rows.get((location, length))
    render, evidence = bridge._inline_metrics(
        "field", text, len(text), len(text), (100, 60, 1, 20), (0, 0, 400, 200))
    assert render is not None
    assert render.baseline == 76
    assert evidence is None


@pytest.mark.parametrize("row_height,available", [(21, True), (22, False)])
def test_mac_integral_accessibility_row_rounding_preserves_inline_completion(
        monkeypatch, row_height, available):
    bridge = mac_metrics(monkeypatch)
    font = bridge._font_candidates("Host-Regular", "", 13)[0][0]
    font.ascender = lambda: 15
    rows = {(0, 11): (40, 40, 60, row_height),
            (5, 6): (70, 40, 30, row_height)}
    bridge._range_bounds = lambda field, location, length: rows.get((location, length))
    render, _ = bridge._inline_metrics(
        "field", "Hello world", 11, 11, (100, 40, 1, row_height), (0, 0, 400, 200))
    assert (render is not None) is available
    if render is not None:
        assert render.baseline == 56


@pytest.mark.parametrize("ax_name, widths", [("Host-Regular", {" world": 34.0}), ("Downloaded-WebFont", {})])
def test_mac_unverified_glyph_advances_never_guess_and_offer_vision_evidence(monkeypatch, ax_name, widths):
    bridge = mac_metrics(monkeypatch, ax_name=ax_name, widths=widths)
    render, evidence = bridge._inline_metrics("field", "Hello world", 11, 11, (100, 40, 1, 20), (0, 0, 400, 200))
    assert render is None
    assert [sample["text"] for sample in evidence["samples"]] == ["Hello world", " world"]
    assert evidence["caret_rect"] == (100, 40, 1, 20)


def test_mac_mid_line_suffix_displaces_only_with_measured_colours(monkeypatch):
    import sys
    import inklow.native as native_package
    from inklow.core import InlineRender
    text = "Hello world tail"
    bridge = mac_metrics(monkeypatch, text)
    render, evidence = bridge._inline_metrics("field", text, 11, 11, (100, 40, 1, 20), (0, 0, 400, 200))
    assert render is None and evidence["suffix"]["text"] == " tail"
    snapshot = Snapshot("editor", "field", text, 11, caret_rect=(100, 40, 1, 20))
    assert bridge._styled_render(snapshot, None, evidence) is None  # Screenshot appearance is off.
    vision = SimpleNamespace(request_inline_style=lambda snapshot, evidence, enabled=False, authorized=None: InlineRender(
        **{**evidence["ax_render"], "source": "macos-ax-attributed-run+vision",
           "text_rgb": (0.1, 0.1, 0.1), "background_rgb": (1.0, 1.0, 1.0)}))
    monkeypatch.setattr(native_package, "macos_vision", vision, raising=False)
    monkeypatch.setitem(sys.modules, "inklow.native.macos_vision", vision)
    bridge._screenshot_appearance = True
    styled = bridge._styled_render(snapshot, None, evidence)
    assert styled.suffix == " tail" and styled.suffix_width == 25
    assert replace(snapshot, inline_render=styled).inline_safe
    assert not replace(snapshot, inline_render=replace(styled, background_rgb=None)).inline_safe


def test_mac_correction_range_must_share_the_caret_row():
    bridge = MacBridge.__new__(MacBridge)
    BridgeBase.__init__(bridge)
    snapshot = Snapshot("editor", "field", "see teh", 7, caret_rect=(100, 40, 1, 20),
                        field_rect=(0, 0, 400, 200))
    bridge._capture = lambda: (snapshot, "field")
    rows = {(4, 3): (70, 40, 30, 20), (6, 1): (90, 40, 10, 20)}
    bridge._range_bounds = lambda field, location, length: rows.get((location, length))
    assert bridge.range_rect(snapshot, 4, 3) == (70, 40, 30, 20)
    rows[(4, 3)] = (70, 60, 30, 20)  # A wrapped/other visual row is never struck through.
    assert bridge.range_rect(snapshot, 4, 3) is None


def test_mac_context_prunes_hidden_and_secure_subtrees_before_values():
    bridge = MacBridge.__new__(MacBridge)
    BridgeBase.__init__(bridge)
    window, field, visible, hidden, secure = (object() for _ in range(5))
    focused = (field, 1234, "editor", "")
    bridge._focused = lambda: focused
    bridge._secure_description = "secure"
    reads = []
    attributes = {
        window: {"AXRole": "AXWindow", "AXChildren": [visible, hidden, secure]},
        field: {"AXWindow": window},
        visible: {"AXRole": "AXStaticText", "AXValue": "Relevant reference"},
        hidden: {"AXRole": "AXGroup", "AXHidden": True},
        secure: {"AXRole": "AXTextField", "AXSubrole": "AXSecureTextField"},
    }
    def attribute(element, name):
        if name in ("AXValue", "AXChildren") and element in (hidden, secure):
            pytest.fail("Protected or hidden context subtree was read")
        if name == "AXValue":
            reads.append(element)
        return attributes.get(element, {}).get(name)
    bridge._attribute = attribute
    bridge._field_bounds = lambda element: (0, 0, 500, 300)
    snapshot = Snapshot("editor", f"1234:{hash(field)}", "Writing", 7,
                        window_id=f"1234:{hash(window)}", window_revision="revision")
    result = bridge.focused_context(snapshot, authorized=lambda: True)
    assert result["status"] == "ready"
    assert result["text"] == "Relevant reference"
    assert reads == [visible]
    assert bridge.focused_context(snapshot, authorized=lambda: False)["status"] == "blocked"
    assert reads == [visible]


def test_atspi_without_explicit_ime_state_never_invents_inline_capability():
    bridge = LinuxBridge.__new__(LinuxBridge)
    field = SimpleNamespace(queryComponent=lambda: pytest.fail("Unverified IME state queried geometry"))
    assert bridge._inline_metrics(field, None, "Hello", 5, (100, 40, 1, 20), {}) is None


@pytest.mark.parametrize("attribute,value", [(40005, None), (40006, object()),
                                             (40028, object()), (40028, 1), (40013, object())])
def test_uia_mixed_or_missing_run_attributes_never_guess_a_host_font(attribute, value, monkeypatch):
    from inklow.native import windows
    bridge = windows.WindowsBridge.__new__(windows.WindowsBridge)
    values = {40005: "Host", 40006: 13, 40007: 400, 40014: False, 40028: 0, 40013: False}
    values[attribute] = value
    run = SimpleNamespace(MoveEndpointByRange=lambda *args: None,
                          MoveEndpointByUnit=lambda *args: -1, GetText=lambda count: "o",
                          GetAttributeValue=lambda key: values[key])
    selected = SimpleNamespace(Clone=lambda: run)
    monkeypatch.setattr(windows.C, "WinDLL", lambda *args: pytest.fail("Missing UIA font metadata guessed"), raising=False)
    assert bridge._inline_metrics(None, selected, "Hello", 5, (100, 40, 1, 20)) is None


def test_partial_acceptance_waits_for_observed_new_caret_before_drawing_remainder():
    from inklow.core import InlineRender
    render = InlineRender("HostFont", 13, 56, (0, 0, 400, 200), 16, 4, source="native-run")
    before = Snapshot("editor", "field", "Hello", 5, caret_rect=(100, 40, 1, 20),
                      inline_render=render)
    state = SuggestionState()
    assert state.offer(state.generation, before, " world tomorrow")
    assert state.accept(before) == " world "
    assert state.text == "tomorrow"
    assert not state.snapshot.inline_safe
    observed = replace(state.snapshot, caret_rect=(145, 40, 1, 20), inline_render=render)
    assert state.offer(state.generation, observed, state.text)
    assert state.snapshot.inline_safe


def test_mac_browser_context_reads_focused_document_not_deep_toolbar_tree():
    bridge = MacBridge.__new__(MacBridge)
    BridgeBase.__init__(bridge)
    window, field, document, reference, toolbar = (object() for _ in range(5))
    groups = [object() for _ in range(10)]
    bridge._focused = lambda: (field, 1234, "browser", "")
    bridge._secure_description = "secure"
    attributes = {
        window: {"AXRole": "AXWindow", "AXChildren": [groups[0], toolbar]},
        field: {"AXRole": "AXTextArea", "AXWindow": window, "AXParent": document},
        document: {"AXRole": "AXWebArea", "AXChildren": [reference, field],
                   "AXParent": groups[-1]},
        reference: {"AXRole": "AXStaticText", "AXValue": "Review Tuesday in Cedar"},
        toolbar: {"AXRole": "AXStaticText", "AXValue": "Unrelated browser toolbar"},
    }
    for index, group in enumerate(groups):
        attributes[group] = {
            "AXRole": "AXGroup",
            "AXChildren": [groups[index + 1] if index + 1 < len(groups) else document],
            "AXParent": groups[index - 1] if index else window,
        }
    reads = []
    def attribute(element, name):
        if name == "AXValue":
            reads.append(element)
        return attributes.get(element, {}).get(name)
    bridge._attribute = attribute
    bridge._field_bounds = lambda element: (0, 0, 500, 300)
    snapshot = Snapshot("browser", f"1234:{hash(field)}", "Our review", 10,
                        window_id=f"1234:{hash(window)}", window_revision="revision")
    result = bridge.focused_context(snapshot, authorized=lambda: True)
    assert result["status"] == "ready"
    assert result["text"] == "Review Tuesday in Cedar"
    assert reads == [reference]


def test_quartz_gate_is_published_before_consumed_dispatch_and_orders_following_input(monkeypatch):
    seen = []
    bridge, posted, _ = quartz_bridge(monkeypatch, lambda action: True)

    def consume(action):
        gate = bridge.input_transaction()
        seen.append((action, gate["original"]["code"]))
        return True

    bridge._callback = consume
    callback = bridge._event_callback
    assert callback(None, 1, {"code": 48, "flags": 0}, None) is None
    gate = bridge.input_transaction()
    for kind, event in ((6, {"code": 48, "flags": 0}),
                        (7, {"code": 56, "flags": 4}),
                        (1, {"code": 0, "flags": 4}),
                        (2, {"code": 0, "flags": 0})):
        assert callback(None, kind, event, None) is None
    assert posted == [] and seen == [("word", 48)]
    posted.append({"insertion": "word one"})
    bridge.finish_input_transaction(gate, True)
    bridge.drain_input_transaction()
    assert posted[0] == {"insertion": "word one"}
    assert [(event["code"], event["flags"]) for event in posted[1:]] == [(56, 4), (0, 4), (0, 0)]


def test_quartz_rapid_second_tab_waits_until_next_presentation_is_armed(monkeypatch):
    visible = [True]
    claimed = []
    bridge, posted, _ = quartz_bridge(monkeypatch, lambda action: False)

    def consume(action):
        if not visible[0]:
            return False
        visible[0] = False
        claimed.append(bridge.input_transaction())
        return True

    bridge._callback = consume
    callback = bridge._event_callback
    assert callback(None, 1, {"code": 48, "flags": 0}, None) is None
    assert callback(None, 6, {"code": 48, "flags": 0}, None) is None
    assert callback(None, 1, {"code": 48, "flags": 0}, None) is None
    bridge.finish_input_transaction(claimed[0], True, presentation_key="remainder")
    bridge.drain_input_transaction()
    assert len(claimed) == 1 and not posted
    bridge.acknowledge_input_presentation("old-presentation")
    assert len(claimed) == 1 and not posted
    visible[0] = True  # The UI acknowledged the displayed remainder.
    bridge.acknowledge_input_presentation("remainder")
    assert len(claimed) == 2 and not posted
    bridge.finish_input_transaction(claimed[1], True)
    bridge.drain_input_transaction()
    assert bridge.input_transaction() is None and not posted


@pytest.mark.parametrize("reason", ["timeout", "overflow", "stop"])
def test_quartz_aborted_acceptance_never_commits_and_replays_original_before_tail(monkeypatch, reason):
    bridge, posted, _ = quartz_bridge(monkeypatch, lambda action: True)
    callback = bridge._event_callback
    assert callback(None, 1, {"code": 48, "flags": 0}, None) is None
    gate = bridge.input_transaction()
    assert bridge.begin_input_transaction(gate)
    count = 257 if reason == "overflow" else 1
    for index in range(count):
        assert callback(None, 1, {"code": 0, "flags": 0, "index": index}, None) is None
    if reason == "timeout":
        gate["deadline"] = 0
        bridge.drain_input_transaction()
    elif reason == "stop":
        # stop's native gate release, without starting real platform teardown.
        bridge._abort_input()
    assert not bridge._commit_input()
    assert not bridge.begin_input_transaction(gate)
    assert [event["code"] for event in posted] == [48] + [0] * count
    assert bridge.input_transaction() is None


def test_quartz_old_deadline_cannot_abort_a_new_consumed_event(monkeypatch):
    bridge, posted, _ = quartz_bridge(monkeypatch, lambda action: True)
    callback = bridge._event_callback
    callback(None, 1, {"code": 48, "flags": 0}, None)
    old = bridge.input_transaction()
    bridge.finish_input_transaction(old, True)
    bridge.drain_input_transaction()
    callback(None, 1, {"code": 48, "flags": 0}, None)
    current = bridge.input_transaction()
    bridge._abort_input(old)
    assert bridge.input_transaction() is current and not posted
    bridge.finish_input_transaction(current, True)
    bridge.drain_input_transaction()


@pytest.mark.parametrize("abort_during_capture", [False, True])
def test_quartz_native_insertion_checks_transaction_after_one_authoritative_capture(monkeypatch, abort_during_capture):
    bridge, posted, _ = quartz_bridge(monkeypatch, lambda action: True)
    bridge._event_callback(None, 1, {"code": 48, "flags": 0}, None)
    gate = bridge.input_transaction()
    assert bridge.begin_input_transaction(gate)
    snapshot = Snapshot("editor", "field", "Hello", 5)
    writes, captures = [], []

    def capture():
        captures.append(snapshot.key)
        if abort_during_capture:
            bridge._abort_input()
        return snapshot, "native-field"

    bridge._capture = capture
    bridge._settable = lambda field, attribute: True
    bridge.ax.AXUIElementSetAttributeValue = lambda field, attribute, value: writes.append(value) or 0
    success = bridge.insert(snapshot, " world")
    bridge.finish_input_transaction(gate, success)
    bridge.drain_input_transaction()
    assert success is not abort_during_capture
    assert captures == [snapshot.key]
    assert writes == ([] if abort_during_capture else [" world"])
    assert [event["code"] for event in posted] == ([48] if abort_during_capture else [])


def test_quartz_queued_tab_without_displayed_remainder_passes_before_following_letter(monkeypatch):
    visible = [True]
    bridge, posted, _ = quartz_bridge(monkeypatch, lambda action: False)

    def consume(action):
        if not visible[0]:
            return False
        visible[0] = False
        return True

    bridge._callback = consume
    bridge._event_callback(None, 1, {"code": 48, "flags": 0}, None)
    gate = bridge.input_transaction()
    bridge._event_callback(None, 6, {"code": 48, "flags": 0}, None)
    bridge._event_callback(None, 1, {"code": 48, "flags": 0}, None)
    bridge._event_callback(None, 1, {"code": 0, "flags": 0}, None)
    bridge.finish_input_transaction(gate, True)
    bridge.drain_input_transaction()
    assert [event["code"] for event in posted] == [48, 0]
    assert bridge.input_transaction() is None


def test_mac_value_notifications_ignore_unfocused_elements_and_coalesce_bursts(monkeypatch):
    import sys
    from contextlib import nullcontext
    monkeypatch.setitem(sys.modules, "objc", SimpleNamespace(autorelease_pool=nullcontext))
    monkeypatch.setattr(macos.time, "monotonic", lambda: 100.0)
    bridge = MacBridge.__new__(MacBridge)
    BridgeBase.__init__(bridge)
    bridge._observe_pid = 1234
    bridge._focused_field_hash = hash("focused")
    wakes = []
    bridge.set_input_callback(lambda: wakes.append(bridge.input_revision()))
    observer = []
    bridge.ax = SimpleNamespace(
        AXObserverCreate=lambda pid, callback, refcon: observer.append(callback) or (0, "observer"),
        AXUIElementCreateApplication=lambda pid: "app",
        AXObserverAddNotification=lambda *args: 0,
        AXObserverGetRunLoopSource=lambda observer: "source")

    def run_notifications(*args):
        for element, name in (("animated-slider", "AXValueChanged"),
                              ("focused", "AXValueChanged"),
                              ("focused", "AXSelectedTextChanged")):
            observer[0]("observer", element, name, None)
        bridge._stopped.set()

    bridge.cf = SimpleNamespace(
        CFRunLoopGetCurrent=lambda: "runloop", kCFRunLoopDefaultMode=0,
        CFRunLoopAddSource=lambda *args: None, CFRunLoopRemoveSource=lambda *args: None,
        CFRunLoopRunInMode=run_notifications)
    bridge._observer_loop()
    assert len(wakes) == 1
    assert bridge.user_input_revision() == 0


def test_quartz_consumed_keyup_is_suppressed_even_when_it_arrives_after_release(monkeypatch):
    bridge, posted, _ = quartz_bridge(monkeypatch, lambda action: True)
    callback = bridge._event_callback
    callback(None, 1, {"code": 48, "flags": 0}, None)
    gate = bridge.input_transaction()
    bridge.finish_input_transaction(gate, True)
    bridge.drain_input_transaction()
    assert callback(None, 6, {"code": 48, "flags": 0}, None) is None
    bridge._callback = lambda action: False
    down, up = {"code": 48, "flags": 0}, {"code": 48, "flags": 0}
    assert callback(None, 1, down, None) is down
    assert callback(None, 6, up, None) is up
    assert posted == []


@pytest.mark.parametrize("overrun", ["overflow", "deadline"])
def test_quartz_inflight_native_edit_preserves_overrun_input_then_disables_interception(monkeypatch, overrun):
    bridge, posted, _ = quartz_bridge(monkeypatch, lambda action: True)
    callback = bridge._event_callback
    callback(None, 1, {"code": 48, "flags": 0}, None)
    gate = bridge.input_transaction()
    assert bridge.begin_input_transaction(gate)
    snapshot = Snapshot("editor", "field", "Hello", 5)
    bridge._capture = lambda: (snapshot, "native-field")
    bridge._settable = lambda *args: True
    timeouts, writes = [], []
    bridge.ax.AXUIElementSetMessagingTimeout = lambda field, seconds: timeouts.append((field, seconds)) or 0
    count = 257 if overrun == "overflow" else 1

    def native_write(field, attribute, value):
        for index in range(count):
            assert callback(None, 1, {"code": 0, "flags": 0, "index": index}, None) is None
        if overrun == "deadline":
            bridge._abort_input(gate)
        assert posted == []  # No typed input overtakes the issued native edit.
        writes.append(value)
        return 0

    bridge.ax.AXUIElementSetAttributeValue = native_write
    assert bridge.insert(snapshot, " world")
    bridge.finish_input_transaction(gate, True)
    assert writes == [" world"]
    assert timeouts == [("native-field", macos._AX_MUTATION_TIMEOUT)]
    assert [event["index"] for event in posted] == list(range(count))
    assert bridge.input_transaction() is None and not bridge._enabled
    assert "preserved" in bridge.status() and "interception is disabled" in bridge.status()


def test_quartz_native_edit_refuses_an_unconfigurable_os_timeout(monkeypatch):
    bridge, posted, _ = quartz_bridge(monkeypatch, lambda action: True)
    bridge._event_callback(None, 1, {"code": 48, "flags": 0}, None)
    gate = bridge.input_transaction()
    assert bridge.begin_input_transaction(gate)
    snapshot = Snapshot("editor", "field", "Hello", 5)
    bridge._capture = lambda: (snapshot, "native-field")
    bridge._settable = lambda *args: True
    bridge.ax.AXUIElementSetMessagingTimeout = lambda *args: -25202
    bridge.ax.AXUIElementSetAttributeValue = lambda *args: pytest.fail("Unbounded native mutation was issued")
    assert not bridge.insert(snapshot, " world")
    bridge.finish_input_transaction(gate, False)
    bridge.drain_input_transaction()
    assert [event["code"] for event in posted] == [48]
