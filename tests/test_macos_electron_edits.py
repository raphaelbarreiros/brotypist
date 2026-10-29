"""Deterministic AX/Quartz host models, not live Electron compatibility proof."""
from dataclasses import replace
from types import SimpleNamespace

import pytest

from inklow.native.base import index_to_utf16, utf16_to_index
from test_native_contract import mac_capture_boundary


CHAT_BUNDLES = (
    "com.hnc.Discord",
    "com.hammerandchisel.discord",
    "com.tinyspeck.slackmacgap",
)


def edit_host(app_id="editor", *, chromium_node=None, setter_applies=False,
              events_apply=True, flags=0, event_access=True):
    """Apply only native selections/events, independently of requested Python edits."""
    bridge, attributes, reads, writes = mac_capture_boundary("hello 😀 world",
        AXSelectedTextRange=(index_to_utf16("hello 😀 world", 13), 0))
    bridge._foreground = lambda: (1234, "/SyntheticHost", {"app_id": app_id, "domain": ""})
    if chromium_node is not None:
        attributes[("field", "ChromeAXNodeId")] = chromium_node
    bridge.ax.AXUIElementIsAttributeSettable = lambda element, name, output: (
        0, element == "field" and name in {"AXValue", "AXSelectedText", "AXSelectedTextRange"})

    def apply_text(text):
        before = attributes[("field", "AXValue")]
        location, units = attributes[("field", "AXSelectedTextRange")]
        left = utf16_to_index(before, location)
        right = utf16_to_index(before, location + units)
        attributes[("field", "AXValue")] = before[:left] + text + before[right:]
        attributes[("field", "AXSelectedTextRange")] = (
            location + index_to_utf16(text, len(text)), 0)

    def set_attribute(element, name, value):
        writes.append((element, name, value))
        if name == "AXSelectedTextRange":
            attributes[(element, name)] = value
        elif name == "AXSelectedText" and setter_applies:
            apply_text(value)
        return 0

    bridge.ax.AXUIElementSetAttributeValue = set_attribute
    posted = []

    def set_unicode(event, units, text):
        assert units == index_to_utf16(text, len(text))
        event["unicode"] = text

    def post_event(tap, event):
        posted.append(dict(event))
        if event["down"] and events_apply:
            apply_text(event["unicode"])

    bridge.q = SimpleNamespace(
        CGPreflightPostEventAccess=lambda: event_access,
        CGEventSourceFlagsState=lambda state: flags,
        kCGEventSourceStateCombinedSessionState=1,
        kCGEventFlagMaskCommand=1,
        kCGEventFlagMaskControl=2,
        kCGEventFlagMaskAlternate=4,
        CGEventCreateKeyboardEvent=lambda source, code, down: {"code": code, "down": down},
        CGEventSetFlags=lambda event, value: event.update(flags=value),
        CGEventKeyboardSetUnicodeString=set_unicode,
        CGEventSetIntegerValueField=lambda event, key, value: event.update(tag=value),
        CGEventPost=post_event,
        kCGEventSourceUserData=1,
        kCGSessionEventTap=2,
    )
    return bridge, attributes, reads, writes, posted


@pytest.mark.parametrize("app_id", CHAT_BUNDLES)
def test_exact_chat_bundle_enables_advertised_accessibility_once(app_id):
    bridge, attributes, _, writes, _ = edit_host(app_id)
    attributes[("application", "AXEnhancedUserInterface")] = False
    is_settable = bridge.ax.AXUIElementIsAttributeSettable
    bridge.ax.AXUIElementIsAttributeSettable = lambda element, name, output: (
        (0, True) if (element, name) == ("application", "AXEnhancedUserInterface")
        else is_settable(element, name, output))
    assert bridge.capture() is not None
    assert bridge.capture() is not None
    assert writes == [("application", "AXEnhancedUserInterface", True)]


@pytest.mark.parametrize("app_id", ["org.example.discord-notes", "org.example.slack-editor"])
def test_chat_name_substring_does_not_enable_accessibility_or_unicode_route(app_id):
    bridge, attributes, _, writes, posted = edit_host(app_id, setter_applies=True)
    attributes[("application", "AXEnhancedUserInterface")] = False
    is_settable = bridge.ax.AXUIElementIsAttributeSettable
    bridge.ax.AXUIElementIsAttributeSettable = lambda element, name, output: (
        (0, True) if (element, name) == ("application", "AXEnhancedUserInterface")
        else is_settable(element, name, output))
    snapshot = bridge.capture()
    assert snapshot is not None and snapshot.can_replace
    assert bridge.insert(snapshot, "!")
    assert attributes[("field", "AXValue")] == "hello 😀 world!"
    assert writes == [("field", "AXSelectedText", "!")]
    assert posted == []


@pytest.mark.parametrize("app_id,chromium_node", [
    *((app_id, None) for app_id in CHAT_BUNDLES),
    ("editor", "12345"),
    ("editor", 0),
])
def test_chat_or_chromium_capability_inserts_host_text_without_noop_ax_setter(app_id, chromium_node):
    bridge, attributes, _, writes, posted = edit_host(app_id, chromium_node=chromium_node)
    snapshot = bridge.capture()
    assert snapshot is not None and not snapshot.can_replace
    inserted = " 😀" * 12  # Cross the event's UTF-16 chunk boundary without splitting a scalar.
    assert bridge.insert(snapshot, inserted)
    assert attributes[("field", "AXValue")] == snapshot.text + inserted
    assert attributes[("field", "AXSelectedTextRange")] == (
        index_to_utf16(snapshot.text + inserted, len(snapshot.text + inserted)), 0)
    assert writes == []
    assert "".join(event["unicode"] for event in posted if event["down"]) == inserted
    assert len(posted) >= 4


def test_unicode_dispatch_does_not_claim_host_confirmation_or_retry_dropped_events():
    bridge, attributes, _, writes, posted = edit_host(CHAT_BUNDLES[0], events_apply=False)
    snapshot = bridge.capture()
    assert bridge.insert(snapshot, "!")  # Adapter contract is dispatch, not host readback.
    assert attributes[("field", "AXValue")] == snapshot.text
    assert writes == []
    assert len(posted) == 2  # No uncertain write retry through AX or a second event pair.


@pytest.mark.parametrize("flags,event_access", [(1, True), (2, True), (4, True), (0, False)])
def test_electron_unicode_denial_never_dispatches_or_falls_back(flags, event_access):
    bridge, attributes, _, writes, posted = edit_host(
        CHAT_BUNDLES[0], flags=flags, event_access=event_access)
    snapshot = bridge.capture()
    assert not bridge.insert(snapshot, "!")
    assert attributes[("field", "AXValue")] == snapshot.text
    assert writes == posted == []


@pytest.mark.parametrize("change", ["text", "selection", "focus", "capability"])
def test_stale_electron_snapshot_never_dispatches(change):
    bridge, attributes, _, writes, posted = edit_host("editor", chromium_node="node")
    snapshot = bridge.capture()
    if change == "text":
        attributes[("field", "AXValue")] = "changed draft"
    elif change == "selection":
        attributes[("field", "AXSelectedTextRange")] = (0, 0)
    elif change == "focus":
        attributes[("application", "AXFocusedUIElement")] = "other-field"
    else:
        attributes.pop(("field", "ChromeAXNodeId"))
    assert not bridge.insert(snapshot, "!")
    assert writes == posted == []


@pytest.mark.parametrize("app_id,chromium_node", [
    *((app_id, None) for app_id in CHAT_BUNDLES),
    ("editor", "12345"),
])
def test_known_noop_controls_reject_replacement_without_selecting_or_writing(app_id, chromium_node):
    bridge, attributes, _, writes, posted = edit_host(app_id, chromium_node=chromium_node)
    snapshot = bridge.capture()
    assert snapshot is not None and not snapshot.can_replace
    assert not bridge.replace_range(snapshot, 0, 5, "goodbye")
    # A caller cannot opt into an unsupported native mutation by changing metadata.
    assert not bridge.replace_range(replace(snapshot, can_replace=True), 0, 5, "goodbye")
    assert attributes[("field", "AXValue")] == snapshot.text
    assert writes == posted == []


def test_replacement_rechecks_capability_before_native_selection_mutation():
    bridge, attributes, _, writes, posted = edit_host(setter_applies=True)
    snapshot = bridge.capture()
    assert snapshot.can_replace
    attributes[("field", "ChromeAXNodeId")] = "node-appeared"
    assert not bridge.replace_range(snapshot, 0, 5, "goodbye")
    assert attributes[("field", "AXValue")] == snapshot.text
    assert writes == posted == []


def test_safe_native_replacement_preserves_utf16_selection_and_applies_host_edit():
    bridge, attributes, _, writes, posted = edit_host(setter_applies=True)
    snapshot = bridge.capture()
    assert snapshot is not None and snapshot.can_replace
    assert bridge.replace_range(snapshot, 6, 1, "sun")
    assert attributes[("field", "AXValue")] == "hello sun world"
    assert writes == [
        ("field", "AXSelectedTextRange", (6, 2)),
        ("field", "AXSelectedText", "sun"),
    ]
    assert posted == []


def test_axvalue_only_native_control_advertises_no_range_replacement():
    bridge, _, _, writes = mac_capture_boundary("hello")
    snapshot = bridge.capture()
    assert snapshot is not None and not snapshot.can_replace
    assert not bridge.replace_range(snapshot, 0, 5, "goodbye")
    assert writes == []


def test_rejected_native_setter_returns_false_without_unicode_retry():
    bridge, attributes, _, writes, posted = edit_host(setter_applies=True)
    snapshot = bridge.capture()

    def reject_attribute(element, name, value):
        writes.append((element, name, value))
        return -25202

    bridge.ax.AXUIElementSetAttributeValue = reject_attribute
    assert not bridge.replace_range(snapshot, snapshot.caret, 0, "!")
    assert attributes[("field", "AXValue")] == snapshot.text
    assert writes == [("field", "AXSelectedText", "!")]
    assert posted == []
