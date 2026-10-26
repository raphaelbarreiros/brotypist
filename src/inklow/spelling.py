"""Local current-word typo assessment: latest-only, off the caller thread, never persisted.

The service answers one question for the word at the caret: is it a suspected
typo in a language this Mac can spell-check, and is there a confident fix?
All native work runs on one daemon thread; ``assess`` only tokenizes and reads
the latest finished result. Text is held in memory for the in-flight request
only; verdicts cache words, never field contents.
"""
from __future__ import annotations

import contextlib
import sys
import threading
import time
import unicodedata
from collections import OrderedDict
from dataclasses import dataclass
from typing import Callable, Protocol

from .core import Snapshot

JOINERS = frozenset("'\u2019\u02bc-\u2010")  # internal apostrophes and hyphens
_LINE_BREAKS = frozenset("\n\r\u2028\u2029\x0b\x0c")
_SEPARATORS = frozenset(" \t\u00a0\u202f")
_CODE_PREFIXES = frozenset("@#/\\.$:~=<>&%+*^|`_")
TRAILING_LIMIT = 4         # spaces/punctuation allowed between the word and the caret
MAX_WORD = 48
CONTEXT_CHARS = 400        # paragraph text before the word used for language recognition
VERDICT_CACHE = 512
LANGUAGE_REFRESH = 60.0
MAX_LANGUAGES = 4
HYPOTHESIS_FLOOR = 0.15    # weaker recognizer hypotheses are not spell-checked
UNSUPPORTED_CONFIDENCE = 0.6
UNSUPPORTED_MASS = 0.35
GUESS_DISTANCE = 2         # an unranked guess is a fix only when this close


class SpellingUnavailable(RuntimeError):
    """The local checker cannot answer now; never treated as a verdict."""


@dataclass(frozen=True)
class SpellingResult:
    """Verdict for ``key`` (the assessed ``Snapshot.key``).

    ``start``/``length`` are Python character indexes into the snapshot text and
    never split a combining-mark cluster; replace ``text[start:start + length]``
    with ``replacement`` to apply the fix. ``reason`` is one of ``typo``, ``ok``,
    ``partial`` (valid unfinished prefix), ``unsupported-language``, ``skipped``,
    ``no-word`` or ``unavailable``; only ``typo`` sets ``suspected``.
    """
    key: tuple
    start: int
    length: int
    word: str
    replacement: str
    suspected: bool
    language: str
    reason: str
    finished: bool = False


class SpellBackend(Protocol):
    """Native checker; every method runs on the service worker thread only."""

    def work_scope(self) -> contextlib.AbstractContextManager: ...

    def languages(self) -> tuple[tuple[str, ...], tuple[str, ...]]:
        """(installed spell-check languages, user-preferred subset in order)."""

    def identify(self, text: str) -> dict[str, float]:
        """Language-recognizer hypotheses (BCP-47-like tag -> probability)."""

    def misspelled(self, word: str, language: str) -> list[tuple[int, int]]:
        """Misspelled UTF-16 ranges of ``word`` in ``language``."""

    def has_completions(self, partial: str, language: str) -> bool:
        """Whether ``partial`` starts a dictionary word in ``language``."""

    def suggestions(self, text: str, start: int, length: int,
                    language: str) -> tuple[str, tuple[str, ...]]:
        """(confident correction or '', ranked guesses) for a UTF-16 range."""

    def close(self) -> None: ...


@dataclass(frozen=True)
class _Word:
    start: int
    end: int
    text: str
    finished: bool
    tail: str      # trailing joiners typed after an unfinished word ("isn'")
    context: str


@dataclass(frozen=True)
class _Job:
    key: tuple
    word: _Word


def _word_char(character: str) -> bool:
    category = unicodedata.category(character)
    return category[0] in "LM" or category == "Nd" or character == "_"


def _skippable(word: str) -> bool:
    letters = [c for c in word if unicodedata.category(c)[0] == "L"]
    if len(word) > MAX_WORD or len(letters) < 2:
        return True
    if any(c == "_" or unicodedata.category(c) == "Nd" for c in word):
        return True
    cased = [c for c in letters if c.isupper() or c.islower()]
    if len(cased) > 1 and all(c.isupper() for c in cased):
        return True  # acronym
    # camelCase / identifiers: an uppercase letter after a lowercase one.
    return any(a.islower() and b.isupper() for a, b in zip(letters, letters[1:]))


def current_word(snapshot: Snapshot) -> _Word | str:
    """The word being typed or just completed at the caret, else a skip reason."""
    text, caret = snapshot.text, snapshot.caret
    if snapshot.protected or snapshot.selection_length or not 0 <= caret <= len(text):
        return "skipped"  # secure, selected; macOS capture already rejects marked (IME) text
    if caret < len(text) and _word_char(text[caret]):
        return "no-word"  # editing inside or before a word
    end = caret
    while end > 0 and caret - end < TRAILING_LIMIT and not _word_char(text[end - 1]):
        character = text[end - 1]
        if character in _LINE_BREAKS:
            return "no-word"
        if character not in _SEPARATORS and unicodedata.category(character)[0] != "P":
            return "no-word"  # symbols and emoji end word context
        end -= 1
    if end == 0 or not _word_char(text[end - 1]):
        return "no-word"
    start = end
    while start > 0:
        character = text[start - 1]
        if _word_char(character):
            start -= 1
        elif character in JOINERS and start > 1 and _word_char(text[start - 2]):
            start -= 1
        else:
            break
    word = text[start:end]
    if (start > 0 and text[start - 1] in _CODE_PREFIXES) or _skippable(word):
        return "skipped"
    gap = text[end:caret]
    joiner_tail = bool(gap) and all(c in JOINERS for c in gap)
    paragraph = max(text.rfind(c, 0, start) for c in _LINE_BREAKS) + 1
    context = text[max(paragraph, end - CONTEXT_CHARS):end]
    return _Word(start, end, word, bool(gap) and not joiner_tail,
                 gap if joiner_tail else "", context)


def _clusters(word: str) -> tuple[str, list[tuple[int, int, int, int]]]:
    """NFC text checked natively plus (orig start, orig end, u16 start, u16 end) spans.

    A base character and its following marks form one unit, so a native range
    can never be mapped into the middle of an accented letter.
    """
    pieces, spans, units, index = [], [], 0, 0
    while index < len(word):
        stop = index + 1
        while stop < len(word) and unicodedata.category(word[stop])[0] == "M":
            stop += 1
        piece = unicodedata.normalize("NFC", word[index:stop])
        width = len(piece.encode("utf-16-le")) // 2
        pieces.append(piece)
        spans.append((index, stop, units, units + width))
        units += width
        index = stop
    return "".join(pieces), spans


def _u16_slice(text: str, start: int, end: int) -> str:
    return text.encode("utf-16-le")[2 * start:2 * end].decode("utf-16-le")


def _distance(left: str, right: str) -> int:
    """Optimal string alignment distance (adjacent transpositions cost one)."""
    left, right = left.casefold(), right.casefold()
    previous2: list[int] = []
    previous = list(range(len(right) + 1))
    for i, a in enumerate(left, 1):
        row = [i] + [0] * len(right)
        for j, b in enumerate(right, 1):
            row[j] = min(previous[j] + 1, row[j - 1] + 1, previous[j - 1] + (a != b))
            if i > 1 and j > 1 and a == right[j - 2] and left[i - 2] == b:
                row[j] = min(row[j], previous2[j - 2] + 1)
        previous2, previous = previous, row
    return previous[-1]


def _match_case(original: str, suggestion: str) -> str:
    if original[:1].isupper() and suggestion[:1].islower():
        return suggestion[:1].upper() + suggestion[1:]
    return suggestion


def _base(tag: str) -> str:
    return tag.replace("-", "_").split("_", 1)[0].casefold()


def plan_languages(hypotheses: dict[str, float], available: tuple[str, ...],
                   preferred: tuple[str, ...]) -> tuple[str, tuple[str, ...], str]:
    """(status, spell languages to try in priority order, recognized tag).

    A word is a typo only when every planned language rejects it. Text the
    recognizer confidently places in a language without an installed spelling
    dictionary is ``unsupported``, never checked against an English fallback.
    """
    installed = set(available)
    variants: dict[str, str] = {}
    for tag in tuple(t for t in preferred if t in installed) + available:
        variants.setdefault(_base(tag), tag)
    ranked = sorted(((float(p), str(tag)) for tag, p in hypotheses.items()
                     if tag and tag != "und" and float(p) > 0),
                    key=lambda item: (-item[0], item[1]))
    detected = ranked[0][1] if ranked else ""
    supported = sum(p for p, tag in ranked if _base(tag) in variants)
    unsupported = sum(p for p, tag in ranked if _base(tag) not in variants)
    if ranked and _base(detected) not in variants and (
            ranked[0][0] >= UNSUPPORTED_CONFIDENCE
            or (unsupported >= UNSUPPORTED_MASS and unsupported > supported)):
        return "unsupported", (), detected
    languages: list[str] = []
    for probability, tag in ranked:
        variant = variants.get(_base(tag))
        if probability >= HYPOTHESIS_FLOOR and variant and variant not in languages:
            languages.append(variant)
    for tag in preferred:
        if tag in installed and tag not in languages:
            languages.append(tag)
    if not languages:
        return "unavailable", (), detected
    return "ok", tuple(languages[:MAX_LANGUAGES]), detected


def _default_backend() -> SpellBackend:
    if sys.platform != "darwin":
        raise SpellingUnavailable("Typo detection uses the macOS spelling service.")
    from .native.macos_spelling import MacSpellBackend
    return MacSpellBackend()


class SpellingService:
    """Latest-only asynchronous current-word typo checks.

    ``assess`` never blocks or raises. ``None`` means the latest snapshot is
    still being checked; ``wake`` is then called from the worker thread, with
    no service lock held, once its result is ready.
    """

    def __init__(self, wake: Callable[[], None] | None = None, *,
                 backend: SpellBackend | None = None) -> None:
        self._wake = wake
        self._lock = threading.Condition()
        self._pending: _Job | None = None
        self._inflight: tuple | None = None
        self._latest: SpellingResult | None = None
        self._closed = False
        self._thread: threading.Thread | None = None
        self._verdicts: OrderedDict[tuple, tuple] = OrderedDict()
        self._languages: tuple[tuple[str, ...], tuple[str, ...]] | None = None
        self._languages_at = 0.0
        self._unavailable = ""
        try:
            self._backend = backend if backend is not None else _default_backend()
        except Exception as exc:  # missing framework or PyObjC: report, never fake
            self._backend = None
            self._unavailable = str(exc) or type(exc).__name__

    @property
    def diagnostic(self) -> str:
        return self._unavailable

    def assess(self, snapshot: Snapshot) -> SpellingResult | None:
        key = snapshot.key
        if self._backend is None or self._closed:
            return SpellingResult(key, snapshot.caret, 0, "", "", False, "", "unavailable")
        try:
            word = current_word(snapshot)
        except Exception:
            return SpellingResult(key, snapshot.caret, 0, "", "", False, "", "skipped")
        if isinstance(word, str):
            return SpellingResult(key, snapshot.caret, 0, "", "", False, "", word)
        with self._lock:
            if self._closed:
                return SpellingResult(key, snapshot.caret, 0, "", "", False, "", "unavailable")
            if self._latest is not None and self._latest.key == key:
                return self._latest
            if self._inflight == key:
                self._pending = None  # the in-flight check is the latest request again
            elif self._pending is None or self._pending.key != key:
                self._pending = _Job(key, word)  # replaces any older queued request
                if self._thread is None:
                    self._thread = threading.Thread(target=self._run, name="brotypist-spelling",
                                                    daemon=True)
                    self._thread.start()
                self._lock.notify()
        return None

    def close(self) -> None:
        with self._lock:
            if self._closed:
                return
            self._closed = True
            self._pending = None
            self._latest = None
            thread = self._thread
            self._lock.notify_all()
        if thread is None and self._backend is not None:
            with contextlib.suppress(Exception):
                self._backend.close()
        elif thread is not None and thread is not threading.current_thread():
            thread.join(timeout=0.5)

    def _run(self) -> None:
        try:
            while True:
                with self._lock:
                    while self._pending is None and not self._closed:
                        self._lock.wait()
                    if self._closed:
                        return
                    job, self._pending = self._pending, None
                    self._inflight = job.key
                result = self._evaluate(job)
                with self._lock:
                    self._inflight = None
                    current = (not self._closed and self._pending is None)
                    if current:
                        self._latest = result
                wake = self._wake
                if current and wake is not None:
                    with contextlib.suppress(Exception):
                        wake()
        finally:
            with contextlib.suppress(Exception):
                self._backend.close()

    def _result(self, job: _Job, reason: str, *, start: int | None = None,
                end: int | None = None, replacement: str = "", language: str = "") -> SpellingResult:
        word = job.word
        start = word.start if start is None else start
        end = word.end if end is None else end
        return SpellingResult(job.key, start, end - start, word.text[start - word.start:end - word.start]
                              if word.start <= start <= end <= word.end else "",
                              replacement, reason == "typo", language, reason, word.finished)

    def _evaluate(self, job: _Job) -> SpellingResult:
        try:
            with self._backend.work_scope():
                result = self._check(job)
            self._unavailable = ""
            return result
        except Exception as exc:
            self._unavailable = str(exc) or type(exc).__name__
            return self._result(job, "unavailable")

    def _installed(self) -> tuple[tuple[str, ...], tuple[str, ...]]:
        now = time.monotonic()
        if self._languages is None or now - self._languages_at > LANGUAGE_REFRESH:
            self._languages = self._backend.languages()
            self._languages_at = now
            self._verdicts.clear()  # pick up newly learned words and dictionary changes
        return self._languages

    def _check(self, job: _Job) -> SpellingResult:
        word = job.word
        available, preferred = self._installed()
        hypotheses = self._backend.identify(word.context)
        status, languages, detected = plan_languages(hypotheses, available, preferred)
        if status == "unsupported":
            return self._result(job, "unsupported-language", language=detected)
        if status != "ok":
            return self._result(job, "unavailable", language=detected)
        checked, spans = _clusters(word.text)
        tail = unicodedata.normalize("NFC", word.tail)
        cache_key = (word.text, checked, word.finished, tail, languages)
        verdict = self._verdicts.get(cache_key)
        if verdict is None:
            verdict = self._verdict(checked, spans, word.text, word.finished, tail, languages)
            self._verdicts[cache_key] = verdict
            while len(self._verdicts) > VERDICT_CACHE:
                self._verdicts.popitem(last=False)
        else:
            self._verdicts.move_to_end(cache_key)
        reason, start, end, replacement, language = verdict
        return self._result(job, reason, start=word.start + start, end=word.start + end,
                            replacement=replacement, language=language)

    def _verdict(self, checked: str, spans: list[tuple[int, int, int, int]], original: str,
                 finished: bool, tail: str, languages: tuple[str, ...]) -> tuple:
        whole = (0, len(original))
        flagged: dict[str, list[tuple[int, int]]] = {}
        for language in languages:
            ranges = [(a, n) for a, n in self._backend.misspelled(checked, language) if n > 0]
            if not ranges:
                return ("ok", *whole, "", language)
            flagged[language] = ranges
        if not finished:
            for language in languages:
                if self._backend.has_completions(checked + tail, language):
                    return ("partial", *whole, "", language)
        primary = languages[0]
        units = spans[-1][3]
        location, length = flagged[primary][-1]  # the flagged part nearest the caret
        location, stop = max(0, min(location, units)), max(0, min(location + length, units))
        if stop <= location:
            return ("typo", *whole, "", primary)
        first = next(s for s in spans if s[3] > location)
        last = next(s for s in reversed(spans) if s[2] < stop)
        correction, guesses = self._backend.suggestions(checked, location, stop - location, primary)
        flagged_text = _u16_slice(checked, location, stop)
        suggestion = correction
        if not suggestion:
            suggestion = next((g for g in guesses[:1]
                               if len(flagged_text) >= 3
                               and _distance(flagged_text, g) <= GUESS_DISTANCE), "")
        replacement = ""
        if suggestion and suggestion != flagged_text:
            replacement = (_u16_slice(checked, first[2], location) + _match_case(flagged_text, suggestion)
                           + _u16_slice(checked, stop, last[3]))
            if replacement == original[first[0]:last[1]]:
                replacement = ""
        return ("typo", first[0], last[1], replacement, primary)
