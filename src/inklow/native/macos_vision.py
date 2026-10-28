"""Opt-in, on-device focused-window screenshots for macOS context and appearance.

Nothing here writes, uploads or logs pixels or recognized text. Every capture
targets one exact on-screen window of the focused process through
ScreenCaptureKit, is re-validated against the snapshot after capture, and the
CGImage is released before results are returned. Only derived style numbers
and one short-lived in-memory OCR string are retained, both bounded.

AX and window rectangles use top-left screen coordinates in points. Pixel
buffers carry their own scale; callers supply live authorization/focus checks
rather than treating a saved snapshot as continuing permission.
"""
from __future__ import annotations

from collections import OrderedDict
from dataclasses import dataclass, fields, replace
import math
import threading
import time
import unicodedata

from ..core import InlineRender

STYLE_SOURCE = "macos-ax-geometry+vision"
COLOUR_SOURCE = "macos-ax-attributed-run+vision"
VISION_SOURCES = frozenset({STYLE_SOURCE, COLOUR_SOURCE})

_CAPTURE_TIMEOUT = 2.0
_TEXT_REUSE_SECONDS = 4.0
_COLOUR_SECONDS = 15.0
_MIN_CAPTURE_INTERVAL = 0.75
_STYLE_CACHE_MAX = 32
_REJECT_CACHE_MAX = 64
_MAX_CANDIDATES = 8
_NCC_MIN = 0.82
_AMBIGUITY_MARGIN = 0.03
_OCR_CONFIDENCE = 0.4
_BACKOFF_NO_FONT = 30.0
_BACKOFF_PERMISSION = 10.0
_BACKOFF_MISMATCH = 3.0
_BACKOFF_FOCUS = 1.0
_RTL = {"R", "AL", "AN", "RLE", "RLO", "RLI"}
_MASK_CHARACTERS = set("•●*·∙◦")
_GENERIC_NAMES = (
    "Helvetica", "Helvetica-Bold", "HelveticaNeue", "HelveticaNeue-Medium", "HelveticaNeue-Bold",
    "ArialMT", "Arial-BoldMT", "TimesNewRomanPSMT", "TimesNewRomanPS-BoldMT", "Times-Roman",
    "Georgia", "Georgia-Bold", "Verdana", "Verdana-Bold", "Tahoma", "TrebuchetMS",
    "Menlo-Regular", "Monaco", "Courier", "CourierNewPSMT", "Avenir-Book", "Avenir-Roman",
    "GillSans", "Palatino-Roman",
)
_SYSTEM_FAMILIES = {"-apple-system", "system-ui", "blinkmacsystemfont", ".applesystemuifont",
                    "sf pro", "sf pro text", "sf pro display", ".sf ns"}
_GENERIC_FAMILIES = {"sans-serif": "Helvetica", "serif": "Times", "monospace": "Menlo",
                     "ui-monospace": "Menlo", "ui-serif": "New York"}
# CoreText weight trait → CSS weight, matching NSFontWeight constants.
_WEIGHTS = ((-0.8, 100), (-0.6, 200), (-0.4, 300), (0.0, 400), (0.23, 500),
            (0.3, 600), (0.4, 700), (0.56, 800), (0.62, 900))


# --- Framework access -------------------------------------------------------

_load_lock = threading.Lock()
_classes: dict | None = None
_secure_check = None


def _objc_classes() -> dict:
    """Load ScreenCaptureKit and Vision through PyObjC without extra wrappers."""
    global _classes
    with _load_lock:
        if _classes is None:
            import objc
            import Quartz  # noqa: F401  registers CGImageRef before block signatures use it
            for name in ("ScreenCaptureKit", "Vision"):
                objc.loadBundle(name, {}, bundle_path=f"/System/Library/Frameworks/{name}.framework",
                                scan_classes=False)

            def block(*types):
                arguments = {0: {"type": b"^v"}}
                arguments.update({index + 1: {"type": kind} for index, kind in enumerate(types)})
                return {"callable": {"retval": {"type": b"v"}, "arguments": arguments}}

            objc.registerMetaDataForSelector(
                b"SCShareableContent",
                b"getShareableContentExcludingDesktopWindows:onScreenWindowsOnly:completionHandler:",
                {"arguments": {2: {"type": b"Z"}, 3: {"type": b"Z"}, 4: block(b"@", b"@")}})
            objc.registerMetaDataForSelector(
                b"SCScreenshotManager", b"captureImageWithFilter:configuration:completionHandler:",
                {"arguments": {4: block(b"^{CGImage=}", b"@")}})
            objc.registerMetaDataForSelector(
                b"VNImageRequestHandler", b"performRequests:error:",
                {"retval": {"type": b"Z"}, "arguments": {3: {"type_modifier": b"o"}}})
            _classes = {name: objc.lookUpClass(name) for name in (
                "SCShareableContent", "SCContentFilter", "SCStreamConfiguration",
                "SCScreenshotManager", "VNImageRequestHandler", "VNRecognizeTextRequest")}
        return _classes


def capture_granted() -> bool:
    """Screen Recording preflight; never prompts."""
    import Quartz
    return bool(Quartz.CGPreflightScreenCaptureAccess())


def request_capture_permission() -> bool:
    """Ask macOS once; the OS may require relaunching before a grant applies."""
    import Quartz
    if Quartz.CGPreflightScreenCaptureAccess():
        return True
    return bool(Quartz.CGRequestScreenCaptureAccess())


def _secure_input() -> bool:
    """Read Secure Event Input conservatively; unavailable Carbon means blocked."""
    global _secure_check
    try:
        if _secure_check is None:
            import ctypes
            carbon = ctypes.CDLL("/System/Library/Frameworks/Carbon.framework/Carbon")
            check = carbon.IsSecureEventInputEnabled
            check.restype, check.argtypes = ctypes.c_bool, []
            _secure_check = check
        return bool(_secure_check())
    except (OSError, AttributeError):
        return True


def _frontmost_pid() -> int:
    from AppKit import NSWorkspace
    app = NSWorkspace.sharedWorkspace().frontmostApplication()
    return int(app.processIdentifier()) if app is not None else 0


# --- Geometry helpers -------------------------------------------------------

def _rect(value):
    """Accept a finite (x, y, width, height), including a zero-width caret."""
    if not isinstance(value, (tuple, list)) or len(value) != 4:
        return None
    try:
        rect = tuple(float(item) for item in value)
    except (TypeError, ValueError):
        return None
    if not all(math.isfinite(item) for item in rect) or rect[2] < 0 or rect[3] <= 0:
        return None
    return rect


def _close(left, right, tolerance) -> bool:
    return len(left) == len(right) and all(abs(float(a) - float(b)) <= tolerance for a, b in zip(left, right))


def _intersects(a, b) -> bool:
    return (min(a[0] + a[2], b[0] + b[2]) > max(a[0], b[0])
            and min(a[1] + a[3], b[1] + b[3]) > max(a[1], b[1]))


def _gap(a, b) -> float:
    dx = max(0.0, b[0] - (a[0] + a[2]), a[0] - (b[0] + b[2]))
    dy = max(0.0, b[1] - (a[1] + a[3]), a[1] - (b[1] + b[3]))
    return math.hypot(dx, dy)


# --- Single-window capture --------------------------------------------------

def _window_number(pid: int, bounds) -> int | None:
    """Return a unique shareable window ID whose frame matches within one point."""
    import Quartz as Q
    options = Q.kCGWindowListOptionOnScreenOnly | Q.kCGWindowListExcludeDesktopElements
    matches = []
    for item in Q.CGWindowListCopyWindowInfo(options, Q.kCGNullWindowID) or ():
        if int(item.get(Q.kCGWindowOwnerPID, -1)) != pid or int(item.get(Q.kCGWindowLayer, -1)) != 0:
            continue
        box = item.get(Q.kCGWindowBounds) or {}
        frame = tuple(box.get(key) for key in ("X", "Y", "Width", "Height"))
        if None in frame or not _close(frame, bounds, 1.0):
            continue
        if int(item.get(Q.kCGWindowSharingState, Q.kCGWindowSharingReadOnly)) == Q.kCGWindowSharingNone:
            return None  # The app marked this window as not shareable.
        if float(item.get(Q.kCGWindowAlpha, 1.0)) <= 0:
            continue
        matches.append(int(item.get(Q.kCGWindowNumber, 0)))
    return matches[0] if len(matches) == 1 and matches[0] > 0 else None


def _await(start):
    """Wait up to _CAPTURE_TIMEOUT seconds; discard late callback results.

    The native operation is not cancelled on timeout. Closing acceptance under
    the same lock as the callback prevents a late image from being retained.
    """
    done, lock, box = threading.Event(), threading.Lock(), []
    accepting = True

    def finish(value):
        with lock:
            if accepting:
                box.append(value)
                done.set()
    start(finish)
    done.wait(_CAPTURE_TIMEOUT)
    with lock:
        accepting = False
        return box.pop() if box else None


@dataclass(frozen=True)
class _Pixels:
    """Opaque sRGB RGBA crop, top row first, in window pixels."""
    width: int
    height: int
    rgba: bytes
    origin: tuple[float, float]
    scale: float


def _context(width: int, height: int, gray: bool):
    """Allocate an 8-bit grayscale or sRGB RGBA bitmap context in pixel units."""
    import Quartz as Q
    if gray:
        return Q.CGBitmapContextCreate(None, width, height, 8, 0, Q.CGColorSpaceCreateDeviceGray(),
                                       Q.kCGImageAlphaNone)
    return Q.CGBitmapContextCreate(None, width, height, 8, 0,
                                   Q.CGColorSpaceCreateWithName(Q.kCGColorSpaceSRGB),
                                   Q.kCGImageAlphaPremultipliedLast)


def _image_bytes(image, channels: int) -> bytes:
    """Copy image bytes into packed rows, omitting native row-stride padding."""
    import Quartz as Q
    width, height = int(Q.CGImageGetWidth(image)), int(Q.CGImageGetHeight(image))
    stride, row = int(Q.CGImageGetBytesPerRow(image)), width * channels
    data = bytes(Q.CGDataProviderCopyData(Q.CGImageGetDataProvider(image)))
    if stride == row:
        return data[:row * height]
    return b"".join(data[y * stride:y * stride + row] for y in range(height))


class _Shot:
    """One window image; close() releases it. Never stored beyond one request."""
    __slots__ = ("image", "frame", "scale", "number")

    def __init__(self, image, frame, scale, number):
        self.image, self.frame, self.scale, self.number = image, frame, scale, number

    def crop(self, rect) -> _Pixels | None:
        """Copy a screen-point rectangle into RGBA pixels clipped to this image.

        Return None for a tiny crop or failed native allocation. The copied
        buffer remains usable after close() drops this shot's image reference.
        """
        import Quartz as Q
        fx, fy, _, _ = self.frame
        scale = self.scale
        width, height = int(Q.CGImageGetWidth(self.image)), int(Q.CGImageGetHeight(self.image))
        x0, y0 = max(0, math.floor((rect[0] - fx) * scale)), max(0, math.floor((rect[1] - fy) * scale))
        x1 = min(width, math.ceil((rect[0] + rect[2] - fx) * scale))
        y1 = min(height, math.ceil((rect[1] + rect[3] - fy) * scale))
        if x1 - x0 < 4 or y1 - y0 < 4:
            return None
        part = Q.CGImageCreateWithImageInRect(self.image, Q.CGRectMake(x0, y0, x1 - x0, y1 - y0))
        context = _context(x1 - x0, y1 - y0, gray=False)
        if part is None or context is None:
            return None
        Q.CGContextDrawImage(context, Q.CGRectMake(0, 0, x1 - x0, y1 - y0), part)
        data = _image_bytes(Q.CGBitmapContextCreateImage(context), 4)
        return _Pixels(x1 - x0, y1 - y0, data, (fx + x0 / scale, fy + y0 / scale), scale)

    def close(self) -> None:
        """Drop the owned PyObjC image reference; safe to call repeatedly."""
        self.image = None


def _capture(pid: int, bounds, current) -> _Shot | None:
    """Capture one matched window, excluding occluding windows and the cursor.

    Recheck current() around asynchronous native calls. Return None for stale
    focus, ambiguous geometry, timeout or unusable capture; native exceptions
    propagate. The caller must close a returned shot after extracting evidence.
    """
    if not current():
        return None
    number = _window_number(pid, bounds)
    if number is None or not current():
        return None
    classes = _objc_classes()
    if not current():
        return None
    content = _await(lambda finish: classes["SCShareableContent"]
                     .getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler_(
                         True, True, lambda value, error: finish(value)))
    if content is None or not current():
        return None
    window = next((item for item in (content.windows() or ()) if int(item.windowID()) == number), None)
    content = None
    if window is None:
        return None
    owner, frame = window.owningApplication(), window.frame()
    frame = (float(frame.origin.x), float(frame.origin.y), float(frame.size.width), float(frame.size.height))
    if owner is None or int(owner.processID()) != pid or not window.isOnScreen() or not _close(frame, bounds, 1.0):
        return None
    content_filter = classes["SCContentFilter"].alloc().initWithDesktopIndependentWindow_(window)
    scale = float(content_filter.pointPixelScale())
    if not 0.5 <= scale <= 4:
        return None
    width, height = round(frame[2] * scale), round(frame[3] * scale)
    configuration = classes["SCStreamConfiguration"].alloc().init()
    if not configuration.respondsToSelector_("setIgnoreShadowsSingleWindow:"):
        return None  # Shadow margins would break the frame-to-pixel mapping.
    configuration.setIgnoreShadowsSingleWindow_(True)
    configuration.setWidth_(width)
    configuration.setHeight_(height)
    configuration.setShowsCursor_(False)
    if not current():
        return None
    image = _await(lambda finish: classes["SCScreenshotManager"]
                   .captureImageWithFilter_configuration_completionHandler_(
                       content_filter, configuration, lambda value, error: finish(value)))
    if image is None or not current():
        return None
    import Quartz as Q
    if abs(int(Q.CGImageGetWidth(image)) - width) > 1 or abs(int(Q.CGImageGetHeight(image)) - height) > 1:
        return None
    return _Shot(image, frame, scale, number)


# --- Screenshot context (OCR) -----------------------------------------------

_text_lock = threading.Lock()
_text_cache = None  # (key, monotonic, text); one entry, in memory only.
_text_timer = None
_text_epoch = 0


def clear_text_cache() -> None:
    """Forget retained OCR plaintext now and prevent in-flight work restoring it."""
    global _text_cache, _text_timer, _text_epoch
    with _text_lock:
        _text_epoch += 1
        _text_cache = None
        if _text_timer is not None:
            _text_timer.cancel()
            _text_timer = None


def _expire_text_cache(epoch: int) -> None:
    """Expire only the generation scheduled by this timer, not a newer result."""
    global _text_cache, _text_timer, _text_epoch
    with _text_lock:
        if epoch == _text_epoch:
            _text_cache = None
            _text_timer = None
            _text_epoch += 1


def _retain_text(key, text: str, epoch: int) -> bool:
    """Retain one OCR result if its generation survives; arm plaintext expiry."""
    global _text_cache, _text_timer, _text_epoch
    with _text_lock:
        if epoch != _text_epoch:
            return False
        if _text_timer is not None:
            _text_timer.cancel()
        _text_epoch += 1
        _text_cache = (key, time.monotonic(), text)
        # The timer retains only a generation, never a reference to plaintext.
        _text_timer = threading.Timer(_TEXT_REUSE_SECONDS, _expire_text_cache, (_text_epoch,))
        _text_timer.daemon = True
        _text_timer.start()
        return True


def _recognize(image, frame):
    """Return (text, confidence, screen-point rect) observations, or None on failure."""
    classes = _objc_classes()
    handler = classes["VNImageRequestHandler"].alloc().initWithCGImage_options_(image, {})
    request = classes["VNRecognizeTextRequest"].alloc().init()
    request.setRecognitionLevel_(0)  # VNRequestTextRecognitionLevelAccurate
    request.setUsesLanguageCorrection_(True)
    if request.respondsToSelector_("setAutomaticallyDetectsLanguage:"):
        request.setAutomaticallyDetectsLanguage_(True)
    ok, _error = handler.performRequests_error_([request], None)
    if not ok:
        return None
    observations = []
    for item in request.results() or ():
        candidates = item.topCandidates_(1)
        if not candidates:
            continue
        text = str(candidates[0].string() or "").strip()
        box = item.boundingBox()  # Normalized, bottom-left origin.
        x, y = float(box.origin.x), float(box.origin.y)
        w, h = float(box.size.width), float(box.size.height)
        observations.append((text, float(candidates[0].confidence()),
                             (frame[0] + x * frame[2], frame[1] + (1 - y - h) * frame[3],
                              w * frame[2], h * frame[3])))
    return observations


def _compose(observations, field, limit: int) -> str:
    """Nearest text first within the budget, then emitted in reading order."""
    kept = []
    for text, confidence, rect in observations:
        compact = text.replace(" ", "")
        if (confidence < _OCR_CONFIDENCE or not compact or set(compact) <= _MASK_CHARACTERS
                or (field is not None and _intersects(rect, field))):
            continue
        kept.append((text, rect))
    if not kept or limit <= 0:
        return ""
    if field is not None:
        kept.sort(key=lambda item: _gap(item[1], field))
    chosen, used = [], 0
    for item in kept:
        if used + len(item[0]) + 1 > limit:
            continue
        chosen.append(item)
        used += len(item[0]) + 1
    chosen.sort(key=lambda item: item[1][1] + item[1][3] / 2)
    lines, line, centre, height = [], [], None, 0.0
    for item in chosen:
        middle = item[1][1] + item[1][3] / 2
        if line and abs(middle - centre) > max(height, item[1][3]) / 2:
            lines.append(line)
            line = []
        if not line:
            centre, height = middle, item[1][3]
        line.append(item)
    if line:
        lines.append(line)
    return "\n".join(" ".join(text for text, _ in sorted(row, key=lambda item: item[1][0]))
                     for row in lines)[:limit]


def recognize_window_text(snapshot, *, authorized, current, max_chars: int = 2400) -> dict:
    """Synchronously recognize context in the snapshot's focused window.

    authorized() must recheck consent/policy; current() must recheck snapshot
    identity. Both can be called repeatedly during capture and before delivery.
    Return a status/text mapping; only ready contains nonempty text, bounded to
    max_chars clamped to 0..4000. OCR plaintext may be reused for four seconds
    for the same context key. Native exceptions are left to the caller.
    Observations overlapping the focused field are omitted when its rectangle
    is available; this exclusion cannot be applied without field geometry.
    """
    with _text_lock:
        epoch = _text_epoch
    limit = max(0, min(int(max_chars), 4000))
    if snapshot.protected or not authorized():
        clear_text_cache()
        return {"status": "blocked", "text": ""}
    pid, window = int(snapshot.window_pid or 0), _rect(snapshot.window_bounds)
    if pid <= 0 or window is None or window[2] <= 0:
        return {"status": "unavailable", "text": ""}
    if not capture_granted():
        return {"status": "screen_permission", "text": ""}
    if _secure_input():
        return {"status": "blocked", "text": ""}
    field = _rect(snapshot.field_rect)
    key = (snapshot.app_id, snapshot.domain, pid, snapshot.window_id, snapshot.window_revision,
           snapshot.field_id, window, field)
    with _text_lock:
        cached = _text_cache
        if cached is not None and time.monotonic() - cached[1] >= _TEXT_REUSE_SECONDS:
            cached = None
    if cached is not None and cached[0] == key:
        if not authorized():
            return {"status": "blocked", "text": ""}
        if not current():
            return {"status": "stale", "text": ""}
        text = cached[2][:limit]
        return {"status": "ready" if text else "empty", "text": text}
    cached = None  # Do not carry another site's plaintext through capture/OCR.
    if not current() or _frontmost_pid() != pid:
        return {"status": "stale", "text": ""}
    import objc
    with objc.autorelease_pool():
        # Cache clearing invalidates capture as well as publication. Check the
        # epoch again after callbacks, which may race a consent/focus reset.
        def capture_current():
            with _text_lock:
                if epoch != _text_epoch:
                    return False
            if not (authorized() and current() and _frontmost_pid() == pid and not _secure_input()):
                return False
            with _text_lock:
                return epoch == _text_epoch

        shot = _capture(pid, window, capture_current)
        if shot is None:
            return {"status": "unavailable" if authorized() else "blocked", "text": ""}
        try:
            if not authorized():
                return {"status": "blocked", "text": ""}
            if not current() or _window_number(pid, window) != shot.number:
                return {"status": "stale", "text": ""}
            observations = _recognize(shot.image, shot.frame)
        finally:
            shot.close()
            shot = None
    if observations is None:
        return {"status": "error", "text": ""}
    if not authorized():
        return {"status": "blocked", "text": ""}
    if not current():
        return {"status": "stale", "text": ""}
    text = _compose(observations, field, 4000)
    if not _retain_text(key, text, epoch):
        return {"status": "stale", "text": ""}
    text = text[:limit]
    return {"status": "ready" if text else "empty", "text": text}


# --- Appearance evidence ----------------------------------------------------

@dataclass(frozen=True)
class _Span:
    """Short left-to-right AX text sample with a screen-point rectangle."""
    text: str
    rect: tuple


@dataclass(frozen=True)
class _Line:
    """Validated same-line geometry and transient text for one style request."""
    samples: tuple
    suffix: _Span | None
    caret: tuple
    clip: tuple
    top: float
    height: float
    font_name: str
    font_family: str
    font_size: float
    ax: InlineRender | None
    pid: int
    window: tuple
    style_key: tuple
    line_key: tuple
    fixed_advance: float = 0.0

    @property
    def spans(self) -> tuple:
        return self.samples + ((self.suffix,) if self.suffix is not None else ())


@dataclass(frozen=True)
class _Style:
    """Reusable font metrics and expiring colours; stores no sampled line text."""
    spec: tuple
    font_name: str
    font_size: float
    offset: float
    line_height: float
    ascent: float
    descent: float
    weight: int
    italic: bool
    line_top: float
    verified_at: float
    text_rgb: tuple | None
    background_rgb: tuple | None


def _span(value) -> _Span | None:
    """Reject unbounded, multiline or RTL samples unsuitable for this matcher."""
    if not isinstance(value, dict):
        return None
    text, rect = value.get("text"), _rect(value.get("rect"))
    if (not isinstance(text, str) or not text.strip() or len(text) > 400 or rect is None or rect[2] <= 0
            or any(character in "\r\n\t\x00" for character in text)
            or any(unicodedata.bidirectional(character) in _RTL for character in text)):
        return None
    return _Span(text, rect)


def _ax_render(value) -> InlineRender | None:
    """Normalize AX render evidence without taking over suffix displacement."""
    if isinstance(value, dict):
        names = {item.name for item in fields(InlineRender)}
        data = {key: item for key, item in value.items() if key in names}
        data["clip_rect"] = _rect(value.get("clip_rect"))
        if data["clip_rect"] is None:
            return None
        try:
            value = InlineRender(**data)
        except TypeError:
            return None
    if not isinstance(value, InlineRender):
        return None
    # The bridge owns displacement; this module contributes colours only.
    return replace(value, text_rgb=None, background_rgb=None, suffix="", suffix_width=0.0)


def _parse(snapshot, evidence) -> _Line | None:
    """Validate AX evidence cheaply; any inconsistency means no style request."""
    pid = int(snapshot.window_pid or 0)
    if (not isinstance(evidence, dict) or snapshot.protected or snapshot.selection_length
            or not snapshot.window_id or not snapshot.field_id or pid <= 0):
        return None
    window = _rect(snapshot.window_bounds)
    caret = _rect(evidence.get("caret_rect") or snapshot.caret_rect)
    clip = _rect(evidence.get("clip_rect"))
    if window is None or caret is None or clip is None or clip[2] <= 0 or window[2] <= 0:
        return None
    raw = evidence.get("samples") or ()
    if not isinstance(raw, (list, tuple)) or len(raw) > 8:
        return None
    samples = tuple(_span(item) for item in raw)
    if any(item is None for item in samples):
        return None
    suffix = None
    if evidence.get("suffix") is not None:
        suffix = _span(evidence["suffix"])
        if suffix is None:
            return None
    ax = None
    if evidence.get("ax_render") is not None:
        ax = _ax_render(evidence["ax_render"])
        if ax is None or not ax.valid(caret):
            return None
    elif len(samples) < 2 or len({item.text for item in samples}) < 2:
        return None  # One span cannot separate font choice from size.
    spans = samples + ((suffix,) if suffix is not None else ())
    if not spans:
        return None
    # Every sample must describe the same visible line before its widths can
    # constrain a font; caret adjacency ties that line to the current insertion.
    top, height = spans[0].rect[1], spans[0].rect[3]
    for item in spans:
        x, y, w, h = item.rect
        if (abs(y - top) > 1 or abs(h - height) > 1
                or x < clip[0] - 1 or x + w > clip[0] + clip[2] + 1
                or y < clip[1] - 1 or y + h > clip[1] + clip[3] + 1
                or x < window[0] - 1 or x + w > window[0] + window[2] + 1
                or y < window[1] - 1 or y + h > window[1] + window[3] + 1):
            return None
    if samples:
        if any(right.rect[0] < left.rect[0] - 0.5 for left, right in zip(samples, samples[1:])):
            return None
        if abs(samples[-1].rect[0] + samples[-1].rect[2] - caret[0]) > 1.5:
            return None
    if suffix is not None and abs(suffix.rect[0] - caret[0]) > 1.5:
        return None
    if not top - 1 <= caret[1] + caret[3] / 2 <= top + height + 1:
        return None
    try:
        font_size = float(evidence.get("font_size") or 0)
        fixed_advance = float(evidence.get("fixed_advance") or 0)
    except (TypeError, ValueError):
        return None
    if not math.isfinite(font_size) or (font_size and not 1 <= font_size <= 300):
        return None
    if not math.isfinite(fixed_advance) or not 0 <= fixed_advance <= 300:
        return None
    font_name, family = str(evidence.get("font_name") or ""), str(evidence.get("font_family") or "")
    if fixed_advance and (not font_name or not font_size):
        return None  # Fixed cells remove the size information from advances.
    signature = (("ax", ax.font_name, round(ax.font_size, 2), ax.fixed_advance) if ax is not None
                 else ("evidence", font_name, family, round(font_size, 2), fixed_advance))
    style_key = (snapshot.app_id, snapshot.domain, snapshot.window_id, snapshot.field_id, signature)
    return _Line(samples, suffix, caret, clip, top, height, font_name, family, font_size, ax, pid, window,
                 style_key, style_key + (round(top), round(height), suffix is not None),
                 fixed_advance=fixed_advance)


# --- Fonts ------------------------------------------------------------------
# Font specs are ("named", PostScript name), ("system", NSFontWeight),
# ("fixed", PostScript name, cell advance in points), or ("ax",).

def _font(spec: tuple, size: float, line: _Line | None = None):
    """Only a font the overlay can recreate exactly (see native.macos.resolve_font)."""
    from AppKit import NSFont
    if spec[0] == "ax":
        from .macos import resolve_font
        return resolve_font(line.ax) if line is not None and line.ax is not None else None
    if spec[0] == "fixed":
        from .macos import resolve_font
        return resolve_font({"font_name": spec[1], "font_size": size, "fixed_advance": spec[2]})
    if spec[0] == "system":
        return NSFont.systemFontOfSize_weight_(size, spec[1])
    font = NSFont.fontWithName_size_(spec[1], size)
    return font if font is not None and str(font.fontName()) == spec[1] else None


def _width(font, text: str) -> float:
    from .macos import text_width
    return float(text_width(font, text))


def _width_ok(predicted: float, observed: float) -> bool:
    return abs(predicted - observed) <= max(1.0, 0.012 * observed)


def _widths_fit(font, samples) -> bool:
    return font is not None and all(_width_ok(_width(font, item.text), item.rect[2]) for item in samples)


def _fit(spec: tuple, line: _Line):
    """Least-squares size from measured CoreText advances; every span must agree.

    Advances are re-measured at the estimated size because optical sizing
    (notably the system font) makes them non-linear in point size.
    """
    size = line.font_size or 0.0
    if not size:
        size = 100.0
        for _ in range(3):
            font = _font(spec, size)
            if font is None:
                return None
            units = [(_width(font, item.text) / size, item.rect[2]) for item in line.samples]
            if not units or any(unit <= 0 for unit, _ in units):
                return None
            size = sum(unit * observed for unit, observed in units) / sum(unit * unit for unit, _ in units)
            if not 4 <= size <= 200:
                return None
    sized = _font(spec, size)
    if sized is None:
        return None
    widths = [(_width(sized, item.text), item.rect[2]) for item in line.samples]
    if not widths or not all(_width_ok(predicted, observed) for predicted, observed in widths):
        return None
    box = float(sized.ascender()) + abs(float(sized.descender()))
    if not 0.75 * box <= line.height <= 3.0 * box:
        return None
    error = sum(abs(predicted - observed) for predicted, observed in widths) / sum(observed for _, observed in widths)
    return size, error


def _system_specs() -> list[tuple]:
    from AppKit import NSFontWeightBold, NSFontWeightMedium, NSFontWeightRegular, NSFontWeightSemibold
    return [("system", float(weight)) for weight in
            (NSFontWeightRegular, NSFontWeightMedium, NSFontWeightSemibold, NSFontWeightBold)]


def _family_specs(family: str) -> list[tuple]:
    import CoreText as CT
    descriptor = CT.CTFontDescriptorCreateWithAttributes({CT.kCTFontFamilyNameAttribute: family})
    matches = CT.CTFontDescriptorCreateMatchingFontDescriptors(descriptor, None) or ()
    return [("named", str(CT.CTFontDescriptorCopyAttribute(item, CT.kCTFontNameAttribute)))
            for item in list(matches)[:24]]


def _candidate_specs(line: _Line) -> list[tuple]:
    """Prefer explicit AX font evidence, then CSS families, then bounded defaults."""
    if line.font_name:
        if line.fixed_advance:
            return [("fixed", line.font_name, line.fixed_advance)]
        # Private ".SF…" aliases cannot be recreated by name, only by weight.
        return _system_specs() if line.font_name.startswith(".") else [("named", line.font_name)]
    specs = []
    for raw in line.font_family.split(","):
        family = raw.strip().strip("'\"")
        lowered = family.lower()
        if not family:
            continue
        if lowered in _SYSTEM_FAMILIES:
            specs += _system_specs()
        else:
            specs += _family_specs(_GENERIC_FAMILIES.get(lowered, family))
    if not specs:
        specs = _system_specs() + [("named", name) for name in _GENERIC_NAMES]
    return list(dict.fromkeys(specs))[:40]


def _candidates(line: _Line) -> list[tuple[tuple, float]]:
    """Rank width-fitting fonts and retain viable rounded point-size variants."""
    if line.ax is not None:
        return [(("ax",), line.ax.font_size)]
    fits = []
    for spec in _candidate_specs(line):
        fit = _fit(spec, line)
        if fit is not None:
            fits.append((fit[1], spec, fit[0]))
    fits.sort(key=lambda item: item[0])
    result = []
    for _, spec, size in fits[:_MAX_CANDIDATES]:
        exact = round(size, 2)
        result.append((spec, exact))
        half = round(size * 2) / 2
        if half != exact and _widths_fit(_font(spec, half), line.samples):
            result.append((spec, half))
    return result


def _traits(font) -> tuple[int, bool]:
    import CoreText as CT
    traits = CT.CTFontCopyTraits(font) or {}
    weight = float(traits.get(CT.kCTFontWeightTrait, 0.0) or 0.0)
    symbolic = int(traits.get(CT.kCTFontSymbolicTrait, 0) or 0)
    return min(_WEIGHTS, key=lambda item: abs(item[0] - weight))[1], bool(symbolic & CT.kCTFontItalicTrait)


# --- Image verification -----------------------------------------------------

def _luminance(rgba: bytes) -> bytes:
    return bytes((54 * r + 183 * g + 19 * b) >> 8 for r, g, b in zip(rgba[0::4], rgba[1::4], rgba[2::4]))


def _median(values: bytes) -> int:
    half, seen = (len(values) + 1) // 2, 0
    for level in range(256):
        seen += values.count(level)
        if seen >= half:
            return level
    return 255


def _gather(buffer, width: int, rows, cols, dx: int = 0) -> bytearray:
    """Gather half-open row/column intervals with an optional pixel x offset."""
    out = bytearray()
    for y in range(*rows):
        base = y * width - dx
        for start, end in cols:
            out += buffer[base + start:base + end]
    return out


def _row_profile(buffer, width: int, height: int, cols) -> list[int]:
    return [sum(sum(buffer[y * width + start:y * width + end]) for start, end in cols) for y in range(height)]


def _ncc(a, b) -> float:
    """Return normalized cross-correlation, or zero for empty/constant inputs."""
    count = len(a)
    if not count or count != len(b):
        return 0.0
    sum_a, sum_b = sum(a), sum(b)
    var_a = sum(value * value for value in a) - sum_a * sum_a / count
    var_b = sum(value * value for value in b) - sum_b * sum_b / count
    if var_a <= 0 or var_b <= 0:
        return 0.0
    return (sum(x * y for x, y in zip(a, b)) - sum_a * sum_b / count) / math.sqrt(var_a * var_b)


def _best_shift(captured: list[int], rendered: list[int], limit: int) -> int:
    """Find the best bounded vertical pixel shift using zero-padded row profiles."""
    count, best, best_score = len(captured), 0, -2.0
    for shift in range(-limit, limit + 1):
        score = _ncc(captured, [rendered[y - shift] if 0 <= y - shift < count else 0 for y in range(count)])
        if score > best_score:
            best, best_score = shift, score
    return best


def _render(font, spans, pixels: _Pixels, baseline: float) -> bytes:
    """Coverage (0-255) of spans drawn at AX x positions; baseline is crop-relative points."""
    import CoreText as CT
    import Quartz as Q
    from Foundation import NSAttributedString
    width, height, scale = pixels.width, pixels.height, pixels.scale
    context = _context(width, height, gray=True)
    Q.CGContextSetGrayFillColor(context, 0.0, 1.0)
    Q.CGContextFillRect(context, Q.CGRectMake(0, 0, width, height))
    Q.CGContextScaleCTM(context, scale, scale)
    Q.CGContextSetShouldAntialias(context, True)
    Q.CGContextSetShouldSmoothFonts(context, False)
    Q.CGContextSetAllowsFontSubpixelPositioning(context, True)
    Q.CGContextSetShouldSubpixelPositionFonts(context, True)
    Q.CGContextSetTextMatrix(context, Q.CGAffineTransformIdentity)
    Q.CGContextSetGrayFillColor(context, 1.0, 1.0)
    attributes = {CT.kCTFontAttributeName: font, CT.kCTForegroundColorFromContextAttributeName: True}
    for item in spans:
        line = CT.CTLineCreateWithAttributedString(
            NSAttributedString.alloc().initWithString_attributes_(item.text, attributes))
        Q.CGContextSetTextPosition(context, item.rect[0] - pixels.origin[0], height / scale - baseline)
        CT.CTLineDraw(line, context)
    return _image_bytes(Q.CGBitmapContextCreateImage(context), 1)


def _align(pixels, captured, captured_rows, spans, rows, cols, font, guess, vertical, shift):
    """Best (score, crop baseline, dx, coverage) after row-profile then 2D NCC search."""
    width, height, scale = pixels.width, pixels.height, pixels.scale
    first = _render(font, spans, pixels, guess)
    offset = _best_shift(captured_rows, _row_profile(first, width, height, cols), vertical)
    baseline = guess + offset / scale
    image = first if offset == 0 else _render(font, spans, pixels, baseline)
    best = (-1.0, baseline, 0, image)
    for dx in range(-shift, shift + 1):
        score = _ncc(captured, _gather(image, width, rows, cols, dx))
        if score > best[0]:
            best = (score, baseline, dx, image)
    for step in (-0.5, 0.5):
        candidate = baseline + step / scale
        image = _render(font, spans, pixels, candidate)
        score = _ncc(captured, _gather(image, width, rows, cols, best[2]))
        if score > best[0]:
            best = (score, candidate, best[2], image)
    return best


def _uniform(rgba: bytes, indexes, minimum: int, max_deviation: float):
    """Return mean RGB in 0..1 only for enough opaque, low-variance samples."""
    if len(indexes) < minimum or any(rgba[index * 4 + 3] < 250 for index in indexes):
        return None
    means = []
    for channel in range(3):
        values = [rgba[index * 4 + channel] for index in indexes]
        mean = sum(values) / len(values)
        if math.sqrt(sum((value - mean) ** 2 for value in values) / len(values)) > max_deviation:
            return None
        means.append(round(mean / 255, 4))
    return tuple(means)


def _colours(pixels, ink, coverage, dx, rows, band):
    """Glyph-interior ink colour and a uniform line-band background, else None."""
    width = pixels.width
    text, background, total = [], [], 0
    for y in range(*rows):
        base = y * width
        for x in range(*band):
            index = base + x
            total += 1
            covered = coverage[index - dx]
            if covered >= 230:
                text.append(index)
            elif covered <= 8 and ink[index] <= 10:
                background.append(index)
    text_rgb = _uniform(pixels.rgba, text, 12, 28)
    background_rgb = _uniform(pixels.rgba, background, max(12, total // 2), 6)
    if text_rgb and background_rgb and max(abs(a - b) for a, b in zip(text_rgb, background_rgb)) < 0.08:
        return None, None
    return text_rgb, background_rgb


def _layout(line: _Line, pixels: _Pixels):
    """Non-overlapping spans to draw and their verified pixel columns."""
    kept = []
    for item in sorted(line.spans, key=lambda value: value.rect[2], reverse=True):
        if not any(_intersects(item.rect, other.rect) for other in kept):
            kept.append(item)
    kept.sort(key=lambda value: value.rect[0])
    scale, origin = pixels.scale, pixels.origin[0]
    shift, caret = math.ceil(1.5 * scale), math.ceil(scale)
    cols = []
    for item in kept:
        start = round((item.rect[0] - origin) * scale)
        end = round((item.rect[0] + item.rect[2] - origin) * scale)
        if abs(item.rect[0] + item.rect[2] - line.caret[0]) <= 1.5:
            end -= caret  # The host caret may be drawn here.
        if abs(item.rect[0] - line.caret[0]) <= 1.5:
            start += caret
        start, end = max(start, shift), min(end, pixels.width - shift)
        if end - start >= 2:
            cols.append((start, end))
    return tuple(kept), cols, shift


def _match(pixels: _Pixels, line: _Line, candidates):
    """Verify candidates against the window pixels; return (_Style | None, backoff seconds)."""
    width, height, scale = pixels.width, pixels.height, pixels.scale
    spans, cols, shift = _layout(line, pixels)
    rows = (max(0, math.floor((line.top - pixels.origin[1]) * scale)),
            min(height, math.ceil((line.top + line.height - pixels.origin[1]) * scale)))
    if not cols or rows[1] - rows[0] < 4:
        return None, _BACKOFF_MISMATCH
    luminance = _luminance(pixels.rgba)
    background = _median(luminance)
    ink = bytes(abs(value - background) for value in luminance)
    captured = _gather(ink, width, rows, cols)
    if not captured or max(captured) < 40 or sum(captured) < 4 * len(captured):
        return None, _BACKOFF_MISMATCH  # No visible glyphs where AX says text is.
    # Neighbouring lines may be in the crop; only this AX line's rows are evidence.
    captured_rows = [value if rows[0] <= y < rows[1] else 0
                     for y, value in enumerate(_row_profile(ink, width, height, cols))]
    vertical = math.ceil((1.0 if line.ax is not None else 0.5 * line.height) * scale)
    results = []
    for spec, size in candidates:
        font = _font(spec, size, line)
        if font is None:
            continue
        ascent, descent = float(font.ascender()), abs(float(font.descender()))
        if line.ax is not None:
            guess = line.ax.baseline - pixels.origin[1]
        else:
            guess = line.top - pixels.origin[1] + (line.height - ascent - descent) / 2 + ascent
        score, baseline, dx, coverage = _align(pixels, captured, captured_rows, spans, rows, cols,
                                               font, guess, vertical, shift)
        results.append((score, spec, size, baseline + pixels.origin[1], dx, coverage, ascent, descent, font))
    if not results:
        return None, _BACKOFF_NO_FONT
    results.sort(key=lambda item: item[0], reverse=True)
    # A high score alone is insufficient when another font explains the same
    # pixels nearly as well; abstain rather than choosing an arbitrary family.
    best = results[0]
    rival = next((item for item in results[1:] if item[1] != best[1]), None)
    if best[0] < _NCC_MIN or (rival is not None and rival[0] > best[0] - _AMBIGUITY_MARGIN):
        return None, _BACKOFF_NO_FONT if best[0] >= _NCC_MIN else _BACKOFF_MISMATCH
    _, spec, size, baseline, dx, coverage, ascent, descent, font = best
    band = (min(start for start, _ in cols), max(end for _, end in cols))
    text_rgb, background_rgb = _colours(pixels, ink, coverage, dx, rows, band)
    if (line.suffix is not None or line.ax is not None) and (text_rgb is None or background_rgb is None):
        return None, _BACKOFF_MISMATCH  # Displacement needs verified colours.
    if line.ax is not None:
        baseline = line.ax.baseline
        ascent, descent = line.ax.ascent, line.ax.descent
        weight, italic = line.ax.font_weight, line.ax.italic
        name = line.ax.font_name
    else:
        weight, italic = _traits(font)
        name = str(font.fontName())
    style = _Style(spec, name, size, baseline - line.top, line.height, ascent, descent,
                   weight, italic, line.top, time.monotonic(), text_rgb, background_rgb)
    return (style, 0.0) if _render_for(style, line) is not None else (None, _BACKOFF_MISMATCH)


def _render_for(style: _Style, line: _Line) -> InlineRender | None:
    """Re-derive the render for the current AX line; reuse only quantitatively consistent styles."""
    if abs(line.height - style.line_height) > 0.5:
        return None
    fresh = (abs(line.top - style.line_top) <= 1
             and time.monotonic() - style.verified_at <= _COLOUR_SECONDS)
    text_rgb, background_rgb = (style.text_rgb, style.background_rgb) if fresh else (None, None)
    if (line.suffix is not None or line.ax is not None) and (text_rgb is None or background_rgb is None):
        return None
    if line.ax is not None:
        if (style.spec != ("ax",) or line.ax.font_name != style.font_name
                or abs(line.ax.font_size - style.font_size) > 0.01):
            return None
        render = replace(line.ax, source=COLOUR_SOURCE, text_rgb=text_rgb, background_rgb=background_rgb)
    else:
        font = _font(style.spec, style.font_size)
        if font is None or str(font.fontName()) != style.font_name or not _widths_fit(font, line.samples):
            return None
        system = style.spec[0] == "system"
        render = InlineRender(style.font_name, style.font_size, line.top + style.offset, line.clip,
                              style.ascent, style.descent, source=STYLE_SOURCE, font_weight=style.weight,
                              italic=style.italic, system_font=system,
                              system_weight=float(style.spec[1]) if system else 0.0,
                              text_rgb=text_rgb, background_rgb=background_rgb,
                              fixed_advance=float(style.spec[2]) if style.spec[0] == "fixed" else 0.0)
    return render if render.valid(line.caret) else None


def _crop_rect(line: _Line):
    """Pad the sample bounds in points to leave room for baseline alignment."""
    spans = line.spans
    left = min(item.rect[0] for item in spans) - 4
    right = max(item.rect[0] + item.rect[2] for item in spans) + 4
    return (left, line.top - 0.5 * line.height, right - left, 2 * line.height)


def _verify(line: _Line, authorized):
    """One background verification: authorize before acquiring focused-window pixels."""
    if not authorized():
        return None, _BACKOFF_FOCUS
    import objc
    with objc.autorelease_pool():
        candidates = _candidates(line)
        if not candidates:
            return None, _BACKOFF_NO_FONT
        if not capture_granted():
            return None, _BACKOFF_PERMISSION
        def current():
            return authorized() and _frontmost_pid() == line.pid and not _secure_input()

        if not current():
            return None, _BACKOFF_FOCUS
        shot = _capture(line.pid, line.window, current)
        if shot is None:
            return None, _BACKOFF_MISMATCH
        try:
            pixels, number = shot.crop(_crop_rect(line)), shot.number
        finally:
            shot.close()
            shot = None
        if pixels is None:
            return None, _BACKOFF_MISMATCH
        if not current() or _window_number(line.pid, line.window) != number:
            return None, _BACKOFF_FOCUS
        return _match(pixels, line, candidates)


class _StyleEngine:
    """Latest-only background verifier with bounded style and rejection caches."""

    def __init__(self) -> None:
        self._cond = threading.Condition()
        self._pending = None
        self._active = None
        self._thread = None
        self._epoch = 0
        self._styles: OrderedDict = OrderedDict()
        self._rejected: OrderedDict = OrderedDict()
        self._last_capture = 0.0

    def request(self, snapshot, evidence, enabled: bool, authorized=None) -> InlineRender | None:
        """Return a reusable render now, or replace the pending verification.

        Never wait for capture. Missing/false authorization clears cached state;
        rejected or already-active lines are not queued again until eligible.
        """
        if not enabled or authorized is None or not self._authorized(authorized):
            self.reset()
            return None
        line = _parse(snapshot, evidence)
        if line is None:
            return None
        with self._cond:
            style = self._styles.get(line.style_key)
            if style is not None:
                self._styles.move_to_end(line.style_key)
        if style is not None:
            try:
                render = _render_for(style, line)
            except Exception:
                render = None
            if render is not None and self._authorized(authorized):
                return render
        with self._cond:
            expiry = self._rejected.get(line.line_key)
            if (expiry is not None and expiry > time.monotonic()) or self._active == line.line_key:
                return None
            self._pending = (self._epoch, line, authorized)
            if self._thread is None:
                self._thread = threading.Thread(target=self._run, daemon=True, name="inklow-vision-style")
                self._thread.start()
            self._cond.notify()
        return None

    @staticmethod
    def _authorized(authorized) -> bool:
        """Treat callback failures as revoked authorization, without logging text."""
        try:
            return bool(authorized())
        except Exception:
            return False

    def _current(self, epoch: int, authorized) -> bool:
        """Check authorization outside the lock, then detect concurrent resets."""
        with self._cond:
            if epoch != self._epoch:
                return False
        if not self._authorized(authorized):
            return False
        with self._cond:
            return epoch == self._epoch

    def reset(self) -> None:
        """Invalidate pending/cached work without stopping the daemon or native call."""
        with self._cond:
            if self._pending is None and not self._styles and not self._rejected and self._active is None:
                return
            self._epoch += 1
            self._pending = None
            self._styles.clear()
            self._rejected.clear()

    def _run(self) -> None:
        """Process the newest request serially and publish only a current epoch."""
        while True:
            with self._cond:
                while self._pending is None:
                    self._cond.wait()
                delay = self._last_capture + _MIN_CAPTURE_INTERVAL - time.monotonic()
                if delay > 0:
                    self._cond.wait(delay)
                    continue  # Pick whichever request is newest after waiting.
                epoch, line, authorized = self._pending
                self._pending = None
                self._active = line.line_key
                self._last_capture = time.monotonic()
            try:
                style, backoff = _verify(line, lambda: self._current(epoch, authorized))
            except Exception:
                # Content-free: native errors can quote window or font data.
                style, backoff = None, _BACKOFF_MISMATCH
            valid = self._current(epoch, authorized)
            # A reset can occur during native verification or authorization;
            # publication checks the epoch again while holding the cache lock.
            with self._cond:
                self._active = None
                if valid and epoch == self._epoch:
                    if style is not None:
                        self._styles[line.style_key] = style
                        self._styles.move_to_end(line.style_key)
                        self._rejected.pop(line.line_key, None)
                        while len(self._styles) > _STYLE_CACHE_MAX:
                            self._styles.popitem(last=False)
                    else:
                        self._rejected[line.line_key] = time.monotonic() + backoff
                        self._rejected.move_to_end(line.line_key)
                        while len(self._rejected) > _REJECT_CACHE_MAX:
                            self._rejected.popitem(last=False)
            # Do not retain line text or a snapshot-bearing callback while idle.
            del line, authorized


_engine = _StyleEngine()


def request_inline_style(snapshot, evidence, *, enabled: bool = False, authorized=None) -> InlineRender | None:
    """Nonblocking cached lookup; schedules at most one latest-only verification.

    Returns a render only when its font, size and baseline were verified against
    the focused window's pixels (or AX, for COLOUR_SOURCE) and still fit the
    current AX geometry. Authorization must recheck consent, policy and exact
    focused field/window/domain; a missing callback fails closed.
    Calling with enabled=False drops all style state.
    """
    return _engine.request(snapshot, evidence, enabled, authorized)


def reset() -> None:
    """Cancel pending verification and forget every cached style and OCR string."""
    _engine.reset()
    clear_text_cache()
