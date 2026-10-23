"""Pure prediction state. A suggestion belongs to one exact field revision."""
from __future__ import annotations

from dataclasses import dataclass, replace
import re
from threading import RLock


@dataclass(frozen=True)
class Snapshot:
    app_id: str
    field_id: str
    text: str
    caret: int
    selection_length: int = 0
    caret_rect: tuple[float, float, float, float] | None = None
    protected: bool = False

    @property
    def prefix(self) -> str:
        return self.text[:self.caret]

    @property
    def suffix(self) -> str:
        return self.text[self.caret:]

    @property
    def key(self) -> tuple:
        return (self.app_id, self.field_id, self.text, self.caret,
                self.selection_length, self.protected)

    def eligible(self, min_chars: int = 8) -> bool:
        return (not self.protected and self.selection_length == 0
                and 0 <= self.caret <= len(self.text)
                and len(self.prefix.strip()) >= min_chars)


def take_word(suggestion: str) -> tuple[str, str]:
    """Preserve spacing and punctuation; accept one whitespace-delimited word."""
    match = re.match(r"\s*\S+\s*", suggestion)
    end = match.end() if match else len(suggestion)
    return suggestion[:end], suggestion[end:]


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

    def accept(self, current: Snapshot, full: bool = False) -> str:
        with self._lock:
            if (self.snapshot is None or current.key != self.snapshot.key
                    or not current.eligible(1)):
                self.invalidate()
                return ""
            accepted, remainder = (self.text, "") if full else take_word(self.text)
            if not accepted:
                return ""
            self.generation += 1
            self.text = remainder
            self.snapshot = replace(
                current, text=current.prefix + accepted + current.suffix,
                caret=current.caret + len(accepted))
            return accepted
