"""Constrain consent, stale-window rejection, OCR retention and style evidence.

Capture/recognition doubles isolate privacy and cache behavior without reading
the desktop. Darwin-only raster tests draw synthetic text with native fonts;
they do not establish compatibility with every application's rendering.
"""

import sys
import threading
import time
from contextlib import nullcontext
from types import SimpleNamespace

import pytest

from inklow.core import Snapshot
from inklow.native import macos_vision as vision

darwin = pytest.mark.skipif(sys.platform != "darwin", reason="macOS frameworks")


def _snapshot(**changes):
    """Build a synthetic field/window identity with overridable capture geometry."""
    values = dict(caret_rect=(300, 210, 1, 20), field_rect=(100, 200, 400, 40),
                  window_id="writer:window", window_revision="revision-1",
                  window_pid=4242, window_bounds=(0, 0, 1000, 800))
    values.update(changes)
    return Snapshot("writer", "body", "Hello brave new world", 21, **values)


def _ax_reader(text, calls):
    """Return a journaled AX context response for testing acquisition precedence."""
    def read(snapshot, *, authorized, max_chars):
        calls.append("accessibility")
        return {"text": text, "status": "ready" if text else "empty",
                "window_id": snapshot.window_id, "revision": snapshot.window_revision}
    return read


def test_screenshot_context_is_used_only_after_accessibility_and_only_when_enabled(monkeypatch):
    from inklow import system
    calls = []
    monkeypatch.setattr(sys, "platform", "darwin")
    monkeypatch.setattr(vision, "recognize_window_text",
                        lambda *args, **kwargs: calls.append("screenshot") or {"status": "ready", "text": "Invoice 42"})
    bridge = SimpleNamespace(focused_context=_ax_reader("Agenda", calls), context_matches=lambda snapshot: True)
    result = system.screen_text(_snapshot(), bridge=bridge, authorized=lambda: True, screenshot_context=True)
    assert (result["provenance"], result["text"], calls) == ("accessibility", "Agenda", ["accessibility"])

    calls.clear()
    bridge.focused_context = _ax_reader("", calls)
    assert system.screen_text(_snapshot(), bridge=bridge, authorized=lambda: True)["status"] == "empty"
    assert calls == ["accessibility"]

    calls.clear()
    result = system.screen_text(_snapshot(), bridge=bridge, authorized=lambda: True,
                                accessibility=False, screenshot_context=True)
    assert (result["provenance"], result["text"], calls) == ("screenshot", "Invoice 42", ["screenshot"])


def test_screenshot_context_never_runs_for_protected_or_unconsented_fields(monkeypatch):
    from inklow import system
    calls = []
    monkeypatch.setattr(sys, "platform", "darwin")
    monkeypatch.setattr(vision, "recognize_window_text", lambda *args, **kwargs: calls.append("screenshot"))
    bridge = SimpleNamespace(focused_context=_ax_reader("", calls), context_matches=lambda snapshot: True)
    protected = system.screen_text(_snapshot(protected=True), bridge=bridge, authorized=lambda: True,
                                   accessibility=False, screenshot_context=True)
    revoked = system.screen_text(_snapshot(), bridge=bridge, authorized=lambda: False,
                                 accessibility=False, screenshot_context=True)
    neither = system.screen_text(_snapshot(), bridge=bridge, authorized=lambda: True,
                                 accessibility=False, screenshot_context=False)
    assert [protected["status"], revoked["status"], neither["status"]] == ["blocked"] * 3
    assert calls == []


@darwin
def test_window_ocr_discards_capture_when_focus_changes_and_releases_image(monkeypatch):
    vision.clear_text_cache()
    focus, closed, captures = [True], [], []

    class Shot:
        number, image, frame = 7, object(), (0.0, 0.0, 1000.0, 800.0)

        def close(self):
            closed.append(True)

    def capture(pid, bounds, current):
        assert current()
        captures.append(pid)
        focus[0] = False  # The user switches windows while the capture completes.
        return Shot()

    monkeypatch.setattr(vision, "capture_granted", lambda: True)
    monkeypatch.setattr(vision, "_secure_input", lambda: False)
    monkeypatch.setattr(vision, "_frontmost_pid", lambda: 4242)
    monkeypatch.setattr(vision, "_window_number", lambda pid, bounds: 7)
    monkeypatch.setattr(vision, "_capture", capture)
    monkeypatch.setattr(vision, "_recognize", lambda image, frame: [("Quarterly plan", 0.9, (10, 10, 90, 12))])
    result = vision.recognize_window_text(_snapshot(), authorized=lambda: True, current=lambda: focus[0])
    assert result == {"status": "stale", "text": ""}
    assert closed == [True]
    focus[0] = True
    monkeypatch.setattr(vision, "_capture", lambda pid, bounds, current: captures.append(pid) or Shot())
    result = vision.recognize_window_text(_snapshot(), authorized=lambda: True, current=lambda: focus[0])
    assert result == {"status": "ready", "text": "Quarterly plan"}
    assert len(captures) == 2  # The stale capture left nothing to reuse.


def test_ocr_text_skips_the_typed_field_masked_and_uncertain_text():
    field = (100, 200, 400, 40)
    observations = [
        ("My own draft", 0.99, (110, 210, 100, 14)),
        ("••••••", 0.95, (100, 300, 60, 14)),
        ("blurry", 0.1, (100, 320, 60, 14)),
        ("Subject: Launch", 0.9, (100, 150, 120, 14)),
        ("To: Ana", 0.9, (300, 151, 60, 14)),
        ("Sent Monday", 0.9, (100, 260, 90, 14)),
    ]
    assert vision._compose(observations, field, 2400) == "Subject: Launch To: Ana\nSent Monday"


def _evidence(top, words=("Hello brave", "world"), widths=(80.0, 40.0), left=200.0):
    """Build two adjacent sample boxes in screen points without claiming a font."""
    first = {"text": words[0], "rect": (left, top, widths[0], 20)}
    second_left = left + widths[0] + 4
    second = {"text": words[1], "rect": (second_left, top, widths[1], 20)}
    caret = second_left + widths[1]
    return {"font_name": "", "font_family": "", "font_size": 0, "samples": [first, second],
            "caret_rect": (caret, top, 1, 20), "clip_rect": (100, top - 10, 400, 40)}


def test_style_requests_are_latest_only_and_disabling_forgets_backoff(monkeypatch):
    engine = vision._StyleEngine()
    monkeypatch.setattr(vision, "_MIN_CAPTURE_INTERVAL", 0.0)
    started, release, seen = threading.Event(), threading.Event(), []
    # Hold the first verification so both newer requests reach the pending slot
    # before the worker resumes; only the last pending line should survive.

    def verify(line, authorized):
        seen.append(line.top)
        started.set()
        release.wait(5)
        return None, 60.0
    monkeypatch.setattr(vision, "_verify", verify)
    snapshot = _snapshot()
    assert engine.request(snapshot, _evidence(100), True, lambda: True) is None
    assert started.wait(5)
    engine.request(snapshot, _evidence(140), True, lambda: True)
    engine.request(snapshot, _evidence(180), True, lambda: True)
    release.set()
    deadline = time.monotonic() + 5
    while len(seen) < 2 and time.monotonic() < deadline:
        time.sleep(0.01)
    time.sleep(0.05)
    assert seen == [100, 180]
    engine.request(snapshot, _evidence(180), True, lambda: True)  # Rejected lines back off instead of recapturing.
    time.sleep(0.1)
    assert seen == [100, 180]
    engine.request(snapshot, _evidence(180), False)
    engine.request(snapshot, _evidence(180), True, lambda: True)
    deadline = time.monotonic() + 5
    while len(seen) < 3 and time.monotonic() < deadline:
        time.sleep(0.01)
    assert seen == [100, 180, 180]


def _pixels(font_name, size, runs, baseline, origin, *, scale=2.0, size_pt=(300, 40), draw=True, font=None):
    """Rasterize synthetic host text into scaled pixels, not a desktop capture.

    Run positions, baseline and origin use screen points; ``scale`` converts
    them to bitmap pixels. An explicit font supplies fixed-advance test glyphs.
    """
    import CoreText as CT
    import Quartz as Q
    from AppKit import NSFont
    from Foundation import NSAttributedString
    width, height = int(size_pt[0] * scale), int(size_pt[1] * scale)
    context = Q.CGBitmapContextCreate(None, width, height, 8, 0, Q.CGColorSpaceCreateWithName(Q.kCGColorSpaceSRGB),
                                      Q.kCGImageAlphaPremultipliedLast)
    Q.CGContextSetRGBFillColor(context, 0.97, 0.96, 0.93, 1.0)
    Q.CGContextFillRect(context, Q.CGRectMake(0, 0, width, height))
    Q.CGContextScaleCTM(context, scale, scale)
    if draw:
        attributes = {"NSFont": font or NSFont.fontWithName_size_(font_name, size),
                      CT.kCTForegroundColorAttributeName: Q.CGColorCreateSRGB(0.1, 0.15, 0.3, 1.0)}
        for x, text in runs:
            # Screen-space y grows downward; the CoreText bitmap baseline grows
            # upward, so subtract the crop-relative baseline from its height.
            line = CT.CTLineCreateWithAttributedString(
                NSAttributedString.alloc().initWithString_attributes_(text, attributes))
            Q.CGContextSetTextPosition(context, x - origin[0], size_pt[1] - (baseline - origin[1]))
            CT.CTLineDraw(line, context)
    data = vision._image_bytes(Q.CGBitmapContextCreateImage(context), 4)
    return vision._Pixels(width, height, data, origin, scale)


def _host_line(font_name="Helvetica", size=15.0, draw=True, evidence_font=None, family=""):
    """Return parsed native-width samples, their raster and the known baseline."""
    from AppKit import NSFont
    from inklow.native.macos import text_width
    font = NSFont.fontWithName_size_(font_name, size)
    origin, left, top, baseline = (100.0, 200.0), 110.0, 210.0, 224.25
    first, second = "Hello brave new", "world"
    second_left = left + text_width(font, first + " ")
    samples = [{"text": first, "rect": (left, top, text_width(font, first), 20.0)},
               {"text": second, "rect": (second_left, top, text_width(font, second), 20.0)}]
    caret = second_left + samples[1]["rect"][2]
    evidence = {"font_name": evidence_font or "", "font_family": family, "font_size": 0, "samples": samples,
                "caret_rect": (caret, top, 1.0, 20.0), "clip_rect": (100.0, 200.0, 300.0, 40.0)}
    snapshot = _snapshot(caret_rect=(caret, top, 1.0, 20.0))
    line = vision._parse(snapshot, evidence)
    pixels = _pixels(font_name, size, [(left, first + " " + second)], baseline, origin, draw=draw)
    return line, pixels, baseline


@darwin
def test_appearance_verifies_font_size_and_baseline_against_window_pixels():
    line, pixels, baseline = _host_line(evidence_font="Helvetica")
    style, _ = vision._match(pixels, line, vision._candidates(line))
    assert style is not None
    render = vision._render_for(style, line)
    assert render.source == vision.STYLE_SOURCE and render.font_name == "Helvetica"
    assert abs(render.font_size - 15.0) <= 0.1 and abs(render.baseline - baseline) <= 0.5
    assert render.background_rgb is not None
    assert all(abs(a - b) <= 0.02 for a, b in zip(render.background_rgb, (0.97, 0.96, 0.93)))


@darwin
def test_appearance_never_guesses_without_matching_geometry_or_glyphs():
    wrong_family, pixels, _ = _host_line(family="Georgia")
    assert vision._candidates(wrong_family) == []
    blank_line, blank, _ = _host_line(evidence_font="Helvetica", draw=False)
    assert vision._match(blank, blank_line, vision._candidates(blank_line))[0] is None



@darwin
def test_fixed_cell_appearance_matches_native_raster_and_preserves_overlay_advance():
    from AppKit import NSFont, NSFontFixedAdvanceAttribute
    from inklow.native.macos import resolve_font, text_width
    # Independent host font: its glyphs, not our renderer, provide the pixel oracle.
    font = NSFont.fontWithName_size_("Menlo-Regular", 12)
    descriptor = font.fontDescriptor().fontDescriptorByAddingAttributes_({NSFontFixedAdvanceAttribute: 7.0})
    native = NSFont.fontWithDescriptor_size_(descriptor, 12)
    caret = (187.0, 210.0, 1.0, 20.0)
    snapshot = _snapshot(caret_rect=caret)
    evidence = {
        "font_name": "Menlo-Regular", "font_size": 12, "fixed_advance": 7,
        "samples": [{"text": "Hello", "rect": (110, 210, 35, 20)},
                    {"text": "world", "rect": (152, 210, 35, 20)}],
        "caret_rect": caret, "clip_rect": (100, 200, 300, 40),
    }
    pixels = _pixels("Menlo-Regular", 12, [(110, "Hello world")], 224.25, (100, 200), font=native)
    line = vision._parse(snapshot, evidence)
    style, _ = vision._match(pixels, line, vision._candidates(line))
    assert style is not None
    render = vision._render_for(style, line)
    assert abs(render.baseline - 224.25) <= 0.5
    assert abs(text_width(resolve_font(render), "world") - 35.0) < 0.01
    wrong = vision._parse(snapshot, {**evidence, "fixed_advance": 8})
    assert vision._match(pixels, wrong, vision._candidates(wrong))[0] is None
    assert vision._parse(snapshot, {**evidence, "font_size": 0}) is None

@pytest.mark.parametrize("change", ["unchanged", "window", "field", "domain", "consent", "reset"])
def test_appearance_reauthorizes_after_shareable_window_enumeration(monkeypatch, change):
    """The same foreground PID does not authorize another window, field or tab."""
    engine = vision._StyleEngine()
    snapshot = _snapshot(domain="allowed.example")
    live = {"window": snapshot.window_id, "field": snapshot.field_id,
            "domain": snapshot.domain, "consent": True}
    enumeration, completed = threading.Event(), threading.Event()
    callbacks, captures = [], []
    window = SimpleNamespace(
        windowID=lambda: 7, owningApplication=lambda: SimpleNamespace(processID=lambda: 4242),
        isOnScreen=lambda: True,
        frame=lambda: SimpleNamespace(origin=SimpleNamespace(x=0, y=0),
                                      size=SimpleNamespace(width=1000, height=800)))

    # Retain the completion callback to change authorization while enumeration
    # is in flight, before the real verification path can request any pixels.
    def enumerate_windows(desktop, on_screen, callback):
        callbacks.append(callback)
        enumeration.set()

    def capture_image(content_filter, configuration, callback):
        captures.append("pixels")
        callback(None, None)

    configuration = SimpleNamespace(
        respondsToSelector_=lambda selector: True,
        setIgnoreShadowsSingleWindow_=lambda value: None,
        setWidth_=lambda value: None, setHeight_=lambda value: None,
        setShowsCursor_=lambda value: None)
    native_classes = {
        "SCShareableContent": SimpleNamespace(
            getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler_=enumerate_windows),
        "SCContentFilter": SimpleNamespace(alloc=lambda: SimpleNamespace(
            initWithDesktopIndependentWindow_=lambda window: SimpleNamespace(pointPixelScale=lambda: 1))),
        "SCStreamConfiguration": SimpleNamespace(alloc=lambda: SimpleNamespace(init=lambda: configuration)),
        "SCScreenshotManager": SimpleNamespace(
            captureImageWithFilter_configuration_completionHandler_=capture_image),
    }
    monkeypatch.setitem(sys.modules, "objc", SimpleNamespace(autorelease_pool=nullcontext))
    monkeypatch.setattr(vision, "_objc_classes", lambda: native_classes)
    monkeypatch.setattr(vision, "_window_number", lambda pid, bounds: 7)
    monkeypatch.setattr(vision, "_frontmost_pid", lambda: 4242)
    monkeypatch.setattr(vision, "_secure_input", lambda: False)
    monkeypatch.setattr(vision, "capture_granted", lambda: True)
    monkeypatch.setattr(vision, "_candidates", lambda line: [(("named", "Helvetica"), 15)])
    verify = vision._verify

    def observe_verification(line, authorized):
        try:
            return verify(line, authorized)
        finally:
            completed.set()

    monkeypatch.setattr(vision, "_verify", observe_verification)

    def authorized():
        return (live["consent"] and live["window"] == snapshot.window_id
                and live["field"] == snapshot.field_id and live["domain"] == snapshot.domain)

    engine.request(snapshot, _evidence(100), True, authorized)
    assert enumeration.wait(2)
    if change == "reset":
        engine.reset()
    elif change != "unchanged":
        live[change] = False if change == "consent" else "other"
    callbacks[0](SimpleNamespace(windows=lambda: [window]), None)
    assert completed.wait(2)
    assert captures == (["pixels"] if change == "unchanged" else [])
    engine.reset()


def test_appearance_without_live_authorization_never_schedules_capture():
    engine = vision._StyleEngine()
    assert engine.request(_snapshot(), _evidence(100), True) is None
    assert engine._pending is None and engine._thread is None


@pytest.fixture
def ocr_boundary(monkeypatch):
    """Replace OS acquisition/recognition, preserving the real OCR/cache consumer.

    Clear shared plaintext on both sides of the fixture. ``recognized`` injects
    a state change during OCR so stale work cannot silently restore cleared data.
    """
    vision.clear_text_cache()
    state = SimpleNamespace(text="First site context", captures=0, recognized=None)

    class Shot:
        number, image, frame = 7, object(), (0.0, 0.0, 1000.0, 800.0)

        def close(self):
            self.image = None

    def capture(pid, bounds, current):
        if not current():
            return None
        state.captures += 1
        return Shot()

    def recognize(image, frame):
        if state.recognized is not None:
            state.recognized()
        return [(state.text, 0.9, (10, 10, 90, 12))]

    monkeypatch.setitem(sys.modules, "objc", SimpleNamespace(autorelease_pool=nullcontext))
    monkeypatch.setattr(vision, "capture_granted", lambda: True)
    monkeypatch.setattr(vision, "_secure_input", lambda: False)
    monkeypatch.setattr(vision, "_frontmost_pid", lambda: 4242)
    monkeypatch.setattr(vision, "_window_number", lambda pid, bounds: 7)
    monkeypatch.setattr(vision, "_capture", capture)
    monkeypatch.setattr(vision, "_recognize", recognize)
    yield state
    vision.clear_text_cache()


def test_ocr_cache_does_not_reuse_prior_site_in_the_same_window(ocr_boundary):
    def read(domain):
        return vision.recognize_window_text(_snapshot(domain=domain),
                                            authorized=lambda: True, current=lambda: True)

    assert read("first.example")["text"] == "First site context"
    ocr_boundary.text = "Second site context"
    assert read("first.example")["text"] == "First site context"
    assert ocr_boundary.captures == 1
    assert read("second.example")["text"] == "Second site context"
    assert ocr_boundary.captures == 2
    assert vision._text_cache[2] == "Second site context"  # One bounded entry, not a site history.


def test_context_opt_out_clears_plaintext_without_another_ocr_request(ocr_boundary):
    from inklow import system
    vision.recognize_window_text(_snapshot(), authorized=lambda: True, current=lambda: True)
    assert vision._text_cache is not None
    result = system.screen_text(_snapshot(), bridge=SimpleNamespace(), authorized=lambda: True,
                                accessibility=False, screenshot_context=False)
    assert result["status"] == "blocked"
    assert vision._text_cache is None
    assert vision._text_timer is None


def test_clear_during_ocr_prevents_inflight_plaintext_being_restored(ocr_boundary):
    ocr_boundary.recognized = vision.clear_text_cache
    result = vision.recognize_window_text(_snapshot(), authorized=lambda: True, current=lambda: True)
    assert result == {"status": "stale", "text": ""}
    assert vision._text_cache is None


def test_ocr_plaintext_expires_without_a_followup_request(monkeypatch, ocr_boundary):
    timers = []

    class Timer:
        """Capture the expiry callback for explicit invocation, without a thread."""

        def __init__(self, interval, function, args):
            self.interval, self.function, self.args = interval, function, args
            self.cancelled = False
            timers.append(self)

        def start(self):
            pass

        def cancel(self):
            self.cancelled = True

    monkeypatch.setattr(vision.threading, "Timer", Timer)
    vision.recognize_window_text(_snapshot(), authorized=lambda: True, current=lambda: True)
    expiry = timers[-1]
    assert expiry.interval == vision._TEXT_REUSE_SECONDS
    assert vision._text_cache[2] == "First site context"
    expiry.function(*expiry.args)  # Advance the scheduled deadline, without reading again.
    assert vision._text_cache is None
    assert vision._text_timer is None


def test_terminal_context_consent_never_exposes_scrollback_and_clears_old_ocr(ocr_boundary):
    from inklow import system
    vision.recognize_window_text(_snapshot(), authorized=lambda: True, current=lambda: True)
    assert vision._text_cache is not None

    def forbidden(*args, **kwargs):
        pytest.fail("Terminal scrollback must not be acquired as model context")

    from dataclasses import replace
    snapshot = replace(_snapshot(), app_id="com.apple.Terminal", field_id="terminal:owned-input")
    result = system.screen_text(
        snapshot, bridge=SimpleNamespace(focused_context=forbidden, context_matches=forbidden),
        authorized=lambda: True, accessibility=True, screenshot_context=True)
    assert result["status"] == "blocked" and result["text"] == ""
    assert ocr_boundary.captures == 1
    assert vision._text_cache is None and vision._text_timer is None
