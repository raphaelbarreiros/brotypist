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
    bridge._shortcuts["word"] = "Alt+Tab"
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
    bridge.ax = SimpleNamespace(AXUIElementSetAttributeValue=lambda element, attribute, text:
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


def test_quartz_keyboard_path_dispatches_configured_force_and_safe_option_tab(monkeypatch):
    import sys
    from contextlib import nullcontext
    monkeypatch.setitem(sys.modules, "objc", SimpleNamespace(autorelease_pool=nullcontext))
    monkeypatch.setattr(shortcuts, "mac_above_tab_keycode", lambda: 10)
    bridge = MacBridge.__new__(MacBridge)
    BridgeBase.__init__(bridge)
    bridge._tap = bridge._runloop = None
    bridge._refresh_status = lambda: None
    bridge._event_type = SimpleNamespace(eventWithCGEvent_=lambda event:
                                         SimpleNamespace(charactersIgnoringModifiers=lambda: "a"))
    q = SimpleNamespace(
        kCGEventTapDisabledByTimeout=-1, kCGEventTapDisabledByUserInput=-2,
        kCGEventKeyDown=1, kCGEventFlagMaskControl=1, kCGEventFlagMaskAlternate=2,
        kCGEventFlagMaskShift=4, kCGEventFlagMaskCommand=8, kCGEventFlagMaskSecondaryFn=16,
        kCGKeyboardEventKeycode=10, kCGEventSourceUnixProcessID=11,
        kCGSessionEventTap=0, kCGHeadInsertEventTap=0, kCGEventTapOptionDefault=0,
        CGEventGetFlags=lambda event: event["flags"],
        CGEventGetIntegerValueField=lambda event, field: event["code"] if field == 10 else 0,
        CGEventSetFlags=lambda event, flags: event.update(flags=flags),
        CGEventTapEnable=lambda *args: None, CGEventTapCreate=lambda *args: object())
    bridge.q = q
    bridge.cf = SimpleNamespace(
        CFMachPortCreateRunLoopSource=lambda *args: object(),
        CFRunLoopGetCurrent=lambda: object(), kCFRunLoopCommonModes=0,
        CFRunLoopAddSource=lambda *args: None, CFRunLoopRun=lambda: None,
        CFRunLoopRemoveSource=lambda *args: None, CFMachPortInvalidate=lambda *args: None)
    calls = []
    bridge.start_shortcuts = BridgeBase.start_shortcuts.__get__(bridge)
    bridge.start_shortcuts(lambda action: calls.append(action) or action in ("force", "bypass"))
    bridge.set_shortcuts_enabled(True)
    bridge._event_loop()
    callback = bridge._event_callback
    tab = {"code": 48, "flags": 0}
    assert callback(None, 1, tab, None) is tab
    force = {"code": 10, "flags": 1}
    assert callback(None, 1, force, None) is None
    bypass = {"code": 48, "flags": 2}
    assert callback(None, 1, bypass, None) is bypass
    assert bypass["flags"] == 0
    modified_tab = {"code": 48, "flags": 8}
    assert callback(None, 1, modified_tab, None) is modified_tab
    assert calls == ["word", "force", "bypass"]


@pytest.mark.parametrize("guard", ["secure", "protected", "readonly", "disabled", "composition", "unknown"])
def test_mac_security_guards_precede_editable_value_access(guard):
    bridge = MacBridge.__new__(MacBridge)
    BridgeBase.__init__(bridge)
    bridge._secure_description = "Localized secure role"
    bridge._foreground = lambda: (1234, "/Editor", {"app_id": "editor", "domain": ""})
    bridge._settable = lambda *args: guard != "readonly"
    attrs = {"AXFocusedUIElement": "field", "AXRole": "AXTextField",
             "AXSubrole": "", "AXRoleDescription": "Text field",
             "AXProtectedContent": False, "AXEnabled": True, "AXFocused": True}
    if guard == "secure":
        attrs["AXSubrole"] = "AXSecureTextField"
    elif guard == "protected":
        attrs["AXProtectedContent"] = True
    elif guard == "disabled":
        attrs["AXEnabled"] = False
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


def test_policy_default_app_domain_precedence_never_overrides_global_or_exclusions():
    bridge = BridgeBase()
    bridge._default_enabled = False
    bridge._app_rules = {"app:editor": {"enabled": True}, "domain:secret.example": {"enabled": False}}
    assert bridge._allowed("editor")
    assert not bridge._allowed("other")
    assert not bridge._allowed("editor", "secret.example")
    bridge._app_rules["domain:secret.example"]["enabled"] = True
    bridge._app_rules["app:editor"]["enabled"] = False
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
    bridge._shortcuts["force"] = "Ctrl+Alt+A"
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


def test_mac_attributed_run_uses_font_baseline_not_caret_height(monkeypatch):
    import sys
    bridge = MacBridge.__new__(MacBridge)
    font = SimpleNamespace(fontName=lambda: "Host-Regular", ascender=lambda: 16,
                           descender=lambda: -4)
    layout = SimpleNamespace(defaultLineHeightForFont_=lambda value: 20,
                             defaultBaselineOffsetForFont_=lambda value: 16)
    bridge._attribute = lambda *args: None
    monkeypatch.setitem(sys.modules, "AppKit", SimpleNamespace(
        NSFont=SimpleNamespace(fontWithName_size_=lambda name, size:
                               font if (name, size) == ("Host-Regular", 13) else None),
        NSLayoutManager=SimpleNamespace(alloc=lambda: SimpleNamespace(init=lambda: layout))))
    attributed = SimpleNamespace(length=lambda: 1, attributesAtIndex_effectiveRange_=lambda index, out:
                                 ({"AXFont": {"AXFontName": "Host-Regular", "AXFontSize": 13}}, None))
    bridge.ax = SimpleNamespace(kAXValueCFRangeType=4, AXValueCreate=lambda kind, value: value,
                                AXUIElementCopyParameterizedAttributeValue=lambda *args: (0, attributed))
    bridge._range_bounds = lambda *args: (90, 40, 10, 20)
    render = bridge._inline_metrics("field", "Hello", 5, 5, (100, 40, 1, 20), (0, 0, 400, 200))
    assert render.font_name == "Host-Regular"
    assert render.font_size == 13
    assert render.baseline == 56
    # Enlarging the caret cannot silently select a larger guessed font/baseline.
    assert bridge._inline_metrics("field", "Hello", 5, 5, (100, 40, 1, 30), (0, 0, 400, 200)) is None


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
