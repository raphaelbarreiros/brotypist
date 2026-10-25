"""Pure prediction state. A suggestion belongs to one exact field revision."""
from __future__ import annotations

from dataclasses import dataclass, replace
import re
import unicodedata
from threading import RLock


@dataclass(frozen=True)
class InlineRender:
    """Observed host run metrics, in native desktop coordinates (font in points)."""
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

    def valid(self, caret_rect) -> bool:
        import math
        if (not isinstance(self.clip_rect, (tuple, list)) or len(self.clip_rect) != 4
                or not isinstance(caret_rect, (tuple, list)) or len(caret_rect) != 4):
            return False
        values = (*self.clip_rect, *caret_rect, self.font_size, self.baseline, self.ascent, self.descent)
        if (not self.font_name or not self.source or self.direction != "ltr"
                or not all(math.isfinite(float(value)) for value in values)
                or not 1 <= self.font_size <= 300 or self.ascent <= 0 or self.descent < 0
                or self.clip_rect[2] <= 0 or self.clip_rect[3] <= 0 or not caret_rect):
            return False
        x, y, width, height = self.clip_rect
        cx, cy, _, ch = caret_rect
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

    @property
    def inline_safe(self) -> bool:
        """Only an unobstructed line end can carry a same-line suffix."""
        return (not self.protected and self.selection_length == 0
                and 0 <= self.caret <= len(self.text)
                and (not self.suffix or self.suffix.startswith(("\n", "\r")))
                and ((self.native_inline and bool(self.native_anchor)) or
                     (self.inline_render is not None and self.inline_render.valid(self.caret_rect))))

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
                self.selection_length, self.protected, self.domain, self.multiline)

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
               trailing_punctuation: bool = True) -> str:
        with self._lock:
            if (self.snapshot is None or current.key != self.snapshot.key
                    or not current.eligible(1)):
                self.invalidate()
                return ""
            if full:
                end = _accept_end(self.text, len(self.text), trailing_space, trailing_punctuation)
                accepted, remainder = self.text[:end], self.text[end:]
            else:
                accepted, remainder = take_word(self.text, trailing_space, trailing_punctuation)
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
