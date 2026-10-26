"""Pure prediction state. A suggestion belongs to one exact field revision."""
from __future__ import annotations

from dataclasses import dataclass, replace
import re
import unicodedata
from threading import RLock


def _rgb_valid(value) -> bool:
    import math
    return (isinstance(value, (tuple, list)) and len(value) == 3
            and all(isinstance(part, (int, float)) and math.isfinite(float(part))
                    and 0 <= float(part) <= 1 for part in value))


@dataclass(frozen=True)
class InlineRender:
    """Native desktop layout and the exact font used to draw the ghost.

    AX styling is reproduced when available. A ``macos-ax-caret-row`` source
    instead fits a system font to observed glyph geometry when the host omits
    styling; it is end-of-line only and does not claim an exact host-font match.
    Private system-font aliases use ``system_font``/``system_weight``.
    A non-empty ``suffix`` is the host text from the caret to its line end
    that the overlay may repaint, displaced right of the ghost; it requires
    measured uniform ``background_rgb`` and host ``text_rgb`` colours.
    """
    font_name: str
    font_size: float
    baseline: float
    clip_rect: tuple[float, float, float, float]
    ascent: float
    descent: float
    direction: str = "ltr"
    source: str = ""
    font_weight: int = 400
    italic: bool = False
    system_font: bool = False
    system_weight: float = 0.0
    text_rgb: tuple[float, float, float] | None = None
    background_rgb: tuple[float, float, float] | None = None
    suffix: str = ""
    suffix_width: float = 0.0
    fixed_advance: float = 0.0

    def valid(self, caret_rect) -> bool:
        import math
        if (not isinstance(self.clip_rect, (tuple, list)) or len(self.clip_rect) != 4
                or not isinstance(caret_rect, (tuple, list)) or len(caret_rect) != 4):
            return False
        values = (*self.clip_rect, *caret_rect, self.font_size, self.baseline, self.ascent,
                  self.descent, self.system_weight, self.suffix_width, self.fixed_advance)
        if (not self.font_name or not self.source or self.direction != "ltr"
                or not all(isinstance(value, (int, float)) and math.isfinite(float(value)) for value in values)
                or not 1 <= self.font_size <= 300 or self.ascent <= 0 or self.descent < 0
                or not 0 <= self.fixed_advance <= 300
                or self.clip_rect[2] <= 0 or self.clip_rect[3] <= 0 or not caret_rect):
            return False
        for colour in (self.text_rgb, self.background_rgb):
            if colour is not None and not _rgb_valid(colour):
                return False
        x, y, width, height = self.clip_rect
        cx, cy, _, ch = caret_rect
        if self.suffix:
            # Displacement repaints visible host text; it needs exact colours
            # and a measured single-line suffix that is wholly inside the clip.
            # Trailing blanks have no ink, so they need no colour repaint.
            if (any(char in self.suffix for char in "\r\n\t") or self.suffix_width <= 0
                    or cx + self.suffix_width > x + width
                    or (self.suffix.strip() and (self.background_rgb is None or self.text_rgb is None))):
                return False
        elif self.suffix_width:
            return False
        return (ch > 0 and x <= cx and x + width - cx >= 1
                and y <= cy and cy + ch <= y + height + 1
                and cy < self.baseline <= cy + ch + 1
                and y <= self.baseline - self.ascent
                and self.baseline + self.descent <= y + height + 1)


@dataclass(frozen=True)
class Snapshot:
    app_id: str
    field_id: str
    text: str
    caret: int
    selection_length: int = 0
    caret_rect: tuple[float, float, float, float] | None = None
    protected: bool = False
    domain: str = ""
    multiline: bool = True
    field_rect: tuple[float, float, float, float] | None = None
    native_anchor: str = ""
    inline_render: InlineRender | None = None
    native_inline: bool = False
    window_id: str = ""
    window_revision: str = ""
    window_pid: int = 0
    window_bounds: tuple[float, float, float, float] | None = None
    on_demand_only: bool = False
    can_replace: bool = True

    @property
    def line_suffix(self) -> str:
        """Host text after the caret on its own line."""
        suffix = self.suffix
        ends = [index for index in (suffix.find("\n"), suffix.find("\r")) if index >= 0]
        return suffix[:min(ends)] if ends else suffix

    @property
    def inline_safe(self) -> bool:
        """A same-line suffix exists only at a line end or a verified displacement."""
        if (self.protected or self.selection_length != 0
                or not 0 <= self.caret <= len(self.text)):
            return False
        if self.native_inline and bool(self.native_anchor):
            return not self.line_suffix
        render = self.inline_render
        if render is None or not render.valid(self.caret_rect):
            return False
        return render.suffix == self.line_suffix

    @property
    def anchor_rect(self) -> tuple[float, float, float, float] | None:
        """An observed caret or field bounds can anchor explicit tools."""
        return self.caret_rect or self.field_rect

    @property
    def has_anchor(self) -> bool:
        """A compositor-owned input panel needs no global screen coordinates."""
        return bool(self.anchor_rect or self.native_anchor)

    @property
    def prefix(self) -> str:
        return self.text[:self.caret]

    @property
    def suffix(self) -> str:
        return self.text[self.caret:]

    @property
    def selected_text(self) -> str:
        return self.text[self.caret:self.caret + self.selection_length]

    @property
    def selection_suffix(self) -> str:
        return self.text[self.caret + self.selection_length:]

    @property
    def key(self) -> tuple:
        return (self.app_id, self.field_id, self.text, self.caret,
                self.selection_length, self.protected, self.domain, self.multiline,
                self.on_demand_only, self.can_replace)

    def eligible(self, min_chars: int = 8) -> bool:
        return (not self.protected and self.selection_length == 0
                and 0 <= self.caret <= len(self.text)
                and len(self.prefix.strip()) >= min_chars)


def _accept_end(suggestion: str, end: int, trailing_space: bool,
                trailing_punctuation: bool) -> int:
    if not trailing_space:
        while end > 0 and suggestion[end - 1].isspace():
            end -= 1
    if not trailing_punctuation:
        # Withheld punctuation stays visible. A punctuation-only remainder must
        # be acceptible on the next press, otherwise the user could never finish.
        token_end = end
        while token_end > 0 and suggestion[token_end - 1].isspace():
            token_end -= 1
        punctuation_start = token_end
        while punctuation_start > 0 and unicodedata.category(suggestion[punctuation_start - 1]).startswith("P"):
            punctuation_start -= 1
        if punctuation_start < token_end and punctuation_start and suggestion[:punctuation_start].strip():
            end = punctuation_start
    return end


def take_word(suggestion: str, trailing_space: bool = True,
              trailing_punctuation: bool = True) -> tuple[str, str]:
    """Accept a word; optional trailing material remains in the candidate."""
    match = re.match(r"\s*\S+\s*", suggestion)
    end = match.end() if match else len(suggestion)
    end = _accept_end(suggestion, end, trailing_space, trailing_punctuation)
    remainder = suggestion[end:]
    if not trailing_space and not remainder.strip():
        remainder = ""
    return suggestion[:end], remainder


class SuggestionState:
    def __init__(self) -> None:
        self.snapshot: Snapshot | None = None
        self.text = ""
        self.generation = 0
        self._lock = RLock()

    def invalidate(self) -> int:
        with self._lock:
            self.generation += 1
            self.snapshot = None
            self.text = ""
            return self.generation

    def offer(self, generation: int, snapshot: Snapshot, text: str) -> bool:
        with self._lock:
            if generation != self.generation or not snapshot.eligible(1):
                return False
            self.snapshot = snapshot
            self.text = text
            return True

    def advance(self, current: Snapshot) -> bool:
        """Consume only an exact, safe insertion matching the shown suggestion."""
        with self._lock:
            previous = self.snapshot
            if (previous is None or not self.text or not current.eligible(1)
                    or current.app_id != previous.app_id
                    or current.field_id != previous.field_id
                    or current.domain != previous.domain
                    or current.multiline != previous.multiline
                    or current.on_demand_only != previous.on_demand_only
                    or current.can_replace != previous.can_replace
                    or current.caret <= previous.caret):
                return False
            inserted = current.text[previous.caret:current.caret]
            if (not self.text.startswith(inserted)
                    or current.text != previous.prefix + inserted + previous.suffix):
                return False
            self.text = self.text[len(inserted):]
            self.snapshot = current
            self.generation += 1
            return True

    def accept(self, current: Snapshot, full: bool = False, trailing_space: bool = True,
               trailing_punctuation: bool = True, visible: str | None = None) -> str:
        """Accept from the shown suggestion; ``visible`` bounds it to drawn text.

        Only whitespace may extend past ``visible``: hidden words never insert.
        """
        with self._lock:
            if (self.snapshot is None or current.key != self.snapshot.key
                    or not current.eligible(1)):
                self.invalidate()
                return ""
            if visible is not None and (not visible or not self.text.startswith(visible)):
                return ""
            limit = len(self.text) if visible is None else len(visible)
            if full:
                end = _accept_end(self.text, limit, trailing_space, trailing_punctuation)
                if limit < len(self.text) and trailing_space:
                    while end < len(self.text) and self.text[end].isspace() and self.text[end] not in "\r\n":
                        end += 1
                accepted, remainder = self.text[:end], self.text[end:]
            else:
                accepted, remainder = take_word(self.text, trailing_space, trailing_punctuation)
                if accepted[limit:].strip():
                    return ""
            if not trailing_space and not remainder.strip():
                remainder = ""
            if not accepted:
                return ""
            if (trailing_space and not full and not remainder and not current.suffix
                    and not accepted[-1].isspace() and any(char.isalnum() for char in accepted)):
                accepted += " "
            self.generation += 1
            self.text = remainder
            self.snapshot = replace(
                current, text=current.prefix + accepted + current.suffix,
                caret=current.caret + len(accepted), caret_rect=None, inline_render=None,
                native_inline=False, native_anchor="")
            return accepted
