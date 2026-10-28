"""Ghost overlay presentation contract with real AppKit fonts and fake panels.

Risk map: a ghost drawn without verified metrics, or clipped text reported as
visible, would let Tab insert words the user never saw (P0). The oracle is
the exact visible text reported to the controller plus the panel frame.
"""
from dataclasses import asdict
from types import SimpleNamespace

import pytest

A = pytest.importorskip("AppKit")
from inklow import appkit_overlay
from inklow.core import InlineRender
from inklow.native.macos import text_width


@pytest.fixture
def screen(monkeypatch):
    screen = SimpleNamespace(frame=lambda: A.NSMakeRect(0, 0, 1000, 700),
                             visibleFrame=lambda: A.NSMakeRect(0, 0, 1000, 676),
                             backingScaleFactor=lambda: 2.0)
    monkeypatch.setattr(appkit_overlay.A, "NSScreen", SimpleNamespace(screens=lambda: [screen]))
    return screen


def overlay_with_recording_panels():
    observed, reports = {}, []
    panel = SimpleNamespace(
        orderOut_=lambda sender: observed.update(visible=False),
        setContentView_=lambda value: observed.update(surface=value),
        setFrame_display_=lambda frame, display: observed.update(frame=frame),
        orderFrontRegardless=lambda: observed.update(visible=True, on_active_space=True),
        isVisible=lambda: observed.get("visible", False),
        isOnActiveSpace=lambda: observed.get("on_active_space", True))
    overlay = appkit_overlay.CaretOverlay.__new__(appkit_overlay.CaretOverlay)
    overlay.ghost = panel
    overlay.chooser = SimpleNamespace(orderOut_=lambda sender: observed.update(chooser_hidden=True))
    overlay.accessory = SimpleNamespace(orderOut_=lambda sender: None)
    overlay.show_accessory = False
    overlay.rect = overlay.field_rect = None
    overlay._ghost_state = overlay._chooser_state = None
    overlay.on_present = lambda key, visible, mode: reports.append((key, visible, mode))
    return overlay, observed, reports


def ghost_event(text=" tomorrow", clip_width=900):
    font = A.NSFont.fontWithName_size_("Helvetica", 13)
    ascent, descent = float(font.ascender()), abs(float(font.descender()))
    render = InlineRender("Helvetica", 13, 200, (0, 0, clip_width, 600), ascent, descent,
                          source="macos-ax-attributed-run")
    return font, descent, {"type": "suggestion", "key": ("editor", "field"), "text": text,
                           "mode": "completion", "inline": True, "render": asdict(render),
                           "rect": (100, 200 - ascent, 1, ascent + descent)}


def test_native_ghost_uses_verified_host_baseline_and_reports_drawn_text(screen):
    _, descent, event = ghost_event()
    overlay, observed, reports = overlay_with_recording_panels()
    overlay.present(event)
    assert observed["visible"]
    assert isinstance(observed["surface"], appkit_overlay.GhostSurface)
    assert A.NSMinX(observed["frame"]) == 100
    assert A.NSMinY(observed["frame"]) <= 700 - 200 - descent < A.NSMinY(observed["frame"]) + 0.5
    assert reports == [(("editor", "field"), " tomorrow", "completion")]
    overlay.present({**event, "render": None})
    assert observed["visible"] is False
    assert reports[-1] == (("editor", "field"), "", "completion")


def test_clipped_ghost_reports_only_whole_visible_words(screen):
    font, _, event = ghost_event(" see you tomorrow")
    overlay, observed, reports = overlay_with_recording_panels()
    clip = 100 + text_width(font, " see you") + 1
    overlay.present({**event, "render": {**event["render"], "clip_rect": (0, 0, clip, 600)}})
    assert observed["visible"] and reports[-1][1] == " see you"
    narrow = 100 + text_width(font, " see") - 1
    overlay.present({**event, "render": {**event["render"], "clip_rect": (0, 0, narrow, 600)}})
    assert observed["visible"] is False and reports[-1][1] == ""


def test_alternatives_chooser_never_replaces_the_ghost(screen):
    _, _, event = ghost_event()
    overlay, observed, _ = overlay_with_recording_panels()
    overlay.present(event)
    overlay.clear(panel="chooser")
    assert observed["visible"] and observed.get("chooser_hidden")


def test_docs_help_places_only_metadata_accessory_and_never_acknowledges_text(screen):
    overlay, observed, reports = overlay_with_recording_panels()
    overlay._docs_anchor = None
    overlay.show_accessory = True
    overlay.accessory_button = SimpleNamespace(
        setAccessibilityLabel_=lambda value: observed.update(accessory_label=value),
        setToolTip_=lambda value: observed.update(accessory_tip=value))
    overlay.accessory = SimpleNamespace(
        orderOut_=lambda sender: observed.update(accessory_visible=False),
        setFrame_display_=lambda frame, display: observed.update(accessory_frame=frame),
        orderFrontRegardless=lambda: observed.update(accessory_visible=True))
    overlay.docs_help({"token": "opaque-capability", "anchor": (100, 200, 600, 400)})
    assert observed["accessory_visible"]
    assert not observed["visible"] and observed["chooser_hidden"]
    assert overlay.rect is None and overlay.field_rect is None
    assert reports == []
    overlay.docs_help(None)
    assert not observed["accessory_visible"] and reports == []


def test_return_from_docs_help_preserves_real_ghost_acknowledgment(screen):
    overlay, observed, reports = overlay_with_recording_panels()
    overlay._docs_anchor = None
    overlay.accessory_button = SimpleNamespace(
        setAccessibilityLabel_=lambda value: None, setToolTip_=lambda value: None)
    overlay.docs_help({"token": "opaque-capability", "anchor": (100, 200, 600, 400)})
    _, _, event = ghost_event()
    overlay.present(event)
    overlay.docs_help(None)  # A delayed metadata revocation must not clear the real ghost.
    assert observed["visible"]
    assert reports == [(("editor", "field"), " tomorrow", "completion")]


def test_identical_completion_reappears_after_native_window_is_hidden(screen):
    overlay, observed, reports = overlay_with_recording_panels()
    _, _, event = ghost_event()
    overlay.present(event)
    overlay.ghost.orderOut_(None)  # Native hiding does not change the suggestion.
    overlay.present(event)
    assert observed["visible"]
    assert reports[-1] == (event["key"], " tomorrow", "completion")


def test_identical_completion_returns_to_the_active_space(screen):
    overlay, observed, reports = overlay_with_recording_panels()
    _, _, event = ghost_event()
    overlay.present(event)
    observed["on_active_space"] = False
    overlay.present(event)
    assert observed["on_active_space"]
    assert reports[-1] == (event["key"], " tomorrow", "completion")


def test_unchanged_completion_recovers_after_font_resolution_becomes_available(screen, monkeypatch):
    overlay, observed, reports = overlay_with_recording_panels()
    font, _, event = ghost_event()
    fonts = iter((None, font))
    monkeypatch.setattr(appkit_overlay, "resolve_font", lambda render: next(fonts))
    overlay.present(event)
    assert not observed["visible"] and reports[-1][1] == ""
    overlay.present(event)
    assert observed["visible"]
    assert reports[-1] == (event["key"], " tomorrow", "completion")


def test_hidden_completion_is_not_acknowledged_when_native_ordering_cannot_show_it(screen, monkeypatch):
    overlay, observed, reports = overlay_with_recording_panels()
    _, _, event = ghost_event()
    overlay.present(event)
    overlay.ghost.orderOut_(None)
    monkeypatch.setattr(overlay.ghost, "orderFrontRegardless", lambda: None)
    overlay.present(event)
    assert not observed["visible"]
    assert reports[-1] == (event["key"], "", "completion")
