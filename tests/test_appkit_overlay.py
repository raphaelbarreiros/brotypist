"""Ghost overlay presentation contract with real AppKit fonts and fake panels.

Risk map: a ghost drawn without verified metrics, or clipped text reported as
visible, would let Tab insert words the user never saw (P0). Oracles combine
native offscreen glyph ink/geometry with exact visible acknowledgements.
"""
from dataclasses import asdict
import math
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
    chooser = {"visible": False, "on_active_space": True, "orderings": 0}

    def order_chooser():
        chooser.update(visible=True, on_active_space=True, orderings=chooser["orderings"] + 1)

    def hide_chooser(sender):
        observed["chooser_hidden"] = True
        chooser["visible"] = False

    observed["chooser"] = chooser
    overlay.chooser = SimpleNamespace(
        orderOut_=hide_chooser,
        setContentView_=lambda value: chooser.update(surface=value),
        setFrame_display_=lambda frame, display: chooser.update(frame=frame),
        orderFrontRegardless=order_chooser,
        isVisible=lambda: chooser["visible"],
        isOnActiveSpace=lambda: chooser["on_active_space"])
    overlay.target = appkit_overlay.OverlayActions.alloc().init().setup(
        lambda action: True, lambda sender: None)
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


def chooser_event(rows=None, mode="synonyms"):
    return {"type": "suggestion", "key": ("editor", "field", 17), "mode": mode,
            "choices": rows if rows is not None else ["tomorrow", "next week"],
            "rect": (100, 200, 1, 16)}


@pytest.mark.parametrize(
    ("rows", "visible"),
    [
        (["tomorrow", "next week"], "tomorrow\nnext week"),
        (["tomorrow", "a deliberately long choice that exceeds the display limit", "later"], "tomorrow"),
        (["W" * 43, "later"], ""),
    ],
    ids=["complete-rows", "length-truncated-prefix", "glyph-width-truncated-prefix"],
)
def test_chooser_acknowledges_only_contiguous_fully_displayed_rows(screen, rows, visible):
    overlay, observed, reports = overlay_with_recording_panels()
    event = chooser_event(rows)
    overlay.present(event)
    chooser = observed["chooser"]
    assert chooser["visible"] and chooser["on_active_space"]
    assert reports == [(event["key"], visible, event["mode"])]
    eligible = len(visible.splitlines())
    buttons = list(chooser["surface"].subviews())
    assert [bool(button.isEnabled()) for button in buttons] == [
        index < eligible for index in range(len(rows))]
    assert overlay.target.actions == {
        100: "menu", **{index: f"choice:{index}" for index in range(1, eligible + 1)}}


@pytest.mark.parametrize("lost_state", ["visible", "on_active_space"], ids=["hidden", "other-space"])
def test_identical_chooser_recovers_native_visibility_and_preserves_revision(screen, lost_state):
    overlay, observed, reports = overlay_with_recording_panels()
    event = chooser_event()
    overlay.present(event)
    chooser = observed["chooser"]
    chooser[lost_state] = False
    overlay.present(event)
    assert chooser["visible"] and chooser["on_active_space"]
    assert chooser["orderings"] == 2
    assert reports == [(event["key"], "tomorrow\nnext week", event["mode"])] * 2
    revised = {**event, "key": ("editor", "field", 18)}
    overlay.present(revised)
    assert chooser["orderings"] == 2
    assert reports[-1] == (revised["key"], "tomorrow\nnext week", revised["mode"])


@pytest.mark.parametrize("replay", [False, True], ids=["initial", "cached-replay"])
@pytest.mark.parametrize("refused_state", ["visible", "on_active_space"], ids=["hidden", "other-space"])
def test_refused_chooser_ordering_revokes_ack_and_unchanged_retry_recovers(
    screen, monkeypatch, replay, refused_state,
):
    overlay, observed, reports = overlay_with_recording_panels()
    event = chooser_event(
        ["tomorrow", "a deliberately long choice that exceeds the display limit", "later"],
        mode="emoji",
    )
    chooser = observed["chooser"]
    if replay:
        overlay.present(event)
        assert reports[-1] == (event["key"], "tomorrow", event["mode"])
    chooser[refused_state] = False
    successful_order = overlay.chooser.orderFrontRegardless

    def refuse_order():
        chooser.update(visible=refused_state != "visible",
                       on_active_space=refused_state != "on_active_space",
                       orderings=chooser["orderings"] + 1)

    monkeypatch.setattr(overlay.chooser, "orderFrontRegardless", refuse_order)
    before = chooser["orderings"]
    overlay.present(event)
    overlay.present(event)
    assert chooser["orderings"] == before + 2
    assert reports[-2:] == [(event["key"], "", event["mode"])] * 2
    monkeypatch.setattr(overlay.chooser, "orderFrontRegardless", successful_order)
    overlay.present(event)
    assert chooser["visible"] and chooser["on_active_space"]
    assert reports[-1] == (event["key"], "tomorrow", event["mode"])


def test_completion_has_no_implicit_popup_and_ghost_chooser_clear_independently(screen):
    overlay, observed, reports = overlay_with_recording_panels()
    _, _, event = ghost_event()
    overlay.present(event)
    assert observed["visible"] and not observed["chooser"]["visible"]
    assert observed["chooser"]["orderings"] == 0
    assert reports == [(event["key"], event["text"], "completion")]
    combined = {**event, "choices": ["tomorrow", "next week"], "choices_mode": "alternatives"}
    overlay.present(combined)
    assert observed["visible"] and observed["chooser"]["visible"]
    assert reports[-2:] == [
        (event["key"], event["text"], "completion"),
        (event["key"], "tomorrow\nnext week", "alternatives"),
    ]
    overlay.clear(panel="chooser")
    assert observed["visible"] and not observed["chooser"]["visible"]
    overlay.present(combined)
    overlay.clear(panel="ghost")
    assert not observed["visible"] and observed["chooser"]["visible"]


def offscreen_ink(surface):
    """Return painted pixel centres in view points, without a window or theme."""
    import Quartz

    padding = 8
    width = math.ceil(surface.bounds().size.width) + 2 * padding
    height = math.ceil(surface.bounds().size.height) + 2 * padding
    bitmap = A.NSBitmapImageRep.alloc().initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel_(
        None, width, height, 8, 4, True, False, A.NSDeviceRGBColorSpace, 0, 0)
    context = A.NSGraphicsContext.graphicsContextWithBitmapImageRep_(bitmap)
    A.NSGraphicsContext.saveGraphicsState()
    try:
        A.NSGraphicsContext.setCurrentContext_(context)
        cg = context.CGContext()
        Quartz.CGContextClearRect(cg, Quartz.CGRectMake(0, 0, width, height))
        # Calibrate raw bitmap row orientation independently of text drawing.
        # The marker is outside the view, so it cannot count as glyph ink.
        Quartz.CGContextSetRGBFillColor(cg, 1, 0, 0, 1)
        Quartz.CGContextFillRect(cg, Quartz.CGRectMake(0, 0, 1, 1))
        Quartz.CGContextSaveGState(cg)
        try:
            Quartz.CGContextTranslateCTM(cg, padding, padding)
            Quartz.CGContextClipToRect(cg, surface.bounds())
            surface.drawRect_(surface.bounds())
        finally:
            Quartz.CGContextRestoreGState(cg)
        raw = bytes(bitmap.bitmapData())
    finally:
        A.NSGraphicsContext.restoreGraphicsState()
    stride = int(bitmap.bytesPerRow())
    marker_rows = [row for row in range(height) if raw[row * stride + 3]]
    assert len(marker_rows) == 1 and marker_rows[0] in (0, height - 1)
    top_down = marker_rows[0] == height - 1
    return [
        (column + 0.5 - padding,
         (height - row - 0.5 if top_down else row + 0.5) - padding)
        for row in range(height)
        for column in range(1, width)
        if raw[row * stride + column * 4 + 3]
    ]


def assert_ink_at_host_baseline(observed, font, event, visible):
    """Compare actual ink to font glyph bounds at the host's AX baseline."""
    import CoreText
    from Foundation import NSAttributedString

    ink = offscreen_ink(observed["surface"])
    assert ink, "The acknowledged ghost must paint real glyphs."
    line = CoreText.CTLineCreateWithAttributedString(
        NSAttributedString.alloc().initWithString_attributes_(visible, {"NSFont": font}))
    glyphs = CoreText.CTLineGetBoundsWithOptions(line, CoreText.kCTLineBoundsUseGlyphPathBounds)
    frame = observed["frame"]
    actual = (
        A.NSMinX(frame) + min(x for x, _ in ink),
        A.NSMinY(frame) + min(y for _, y in ink),
        A.NSMinX(frame) + max(x for x, _ in ink),
        A.NSMinY(frame) + max(y for _, y in ink),
    )
    # This comes from the host coordinates, not the surface's drawing baseline.
    host_baseline = 700 - event["render"]["baseline"]
    host_x = event["rect"][0]
    expected = (
        host_x + glyphs.origin.x,
        host_baseline + glyphs.origin.y,
        host_x + glyphs.origin.x + glyphs.size.width,
        host_baseline + glyphs.origin.y + glyphs.size.height,
    )
    # Raster pixel centres/antialiasing may straddle a path edge; no golden pixels.
    assert actual == pytest.approx(expected, abs=1.5)
    return actual


@pytest.mark.parametrize("text", [" tomorrow", " amanhã"], ids=["english", "portuguese"])
def test_native_ghost_paints_real_glyphs_at_the_measured_host_baseline(screen, text):
    font, _, event = ghost_event(text)
    event["render"].update(text_rgb=(0, 0, 0), background_rgb=(1, 1, 1))
    overlay, observed, reports = overlay_with_recording_panels()
    overlay.present(event)
    assert reports == [(event["key"], text, "completion")]
    assert_ink_at_host_baseline(observed, font, event, text)


def test_native_clipped_ghost_paints_only_whole_fitting_words_inside_host_clip(screen):
    font, _, event = ghost_event(" see you tomorrow")
    render = event["render"]
    clip_width = text_width(font, " see you") + 1
    clip_top = render["baseline"] - render["ascent"]
    clip_height = render["ascent"] + render["descent"]
    render.update(clip_rect=(100, clip_top, clip_width, clip_height),
                  text_rgb=(0, 0, 0), background_rgb=(1, 1, 1))
    overlay, observed, reports = overlay_with_recording_panels()
    overlay.present(event)
    assert reports == [(event["key"], " see you", "completion")]
    left, bottom, right, top = assert_ink_at_host_baseline(observed, font, event, " see you")
    assert 100 <= left < right <= 100 + clip_width
    assert 700 - clip_top - clip_height <= bottom < top <= 700 - clip_top


@pytest.mark.parametrize("offset", [-80, 80], ids=["above-caret-row", "below-caret-row"])
def test_ghost_rejects_a_baseline_outside_the_caret_row_even_inside_the_clip(screen, offset):
    _, _, event = ghost_event()
    overlay, observed, reports = overlay_with_recording_panels()
    overlay.present(event)
    assert observed["visible"] and reports[-1][1] == event["text"]
    event = {**event, "render": {**event["render"], "baseline": 200 + offset}}
    overlay.present(event)
    assert not observed["visible"]
    assert reports[-1] == (event["key"], "", "completion")


@pytest.mark.parametrize("edge", ["top", "bottom"])
def test_ghost_rejects_host_clipping_that_cuts_the_verified_font_band(screen, edge):
    _, _, event = ghost_event()
    render = event["render"]
    top = render["baseline"] - render["ascent"]
    height = render["ascent"] + render["descent"]
    overlay, observed, reports = overlay_with_recording_panels()
    overlay.present(event)
    assert observed["visible"]
    clipped_top = top + 2 if edge == "top" else top
    event = {**event, "render": {**render, "clip_rect": (0, clipped_top, 900, height - 2)}}
    overlay.present(event)
    assert not observed["visible"]
    assert reports[-1] == (event["key"], "", "completion")
