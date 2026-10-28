"""macOS spelling backend for spelling.py: AppKit NSSpellChecker + NaturalLanguage.

Everything stays on this Mac. Only installed spell-check languages are used, so
no dictionary is ever requested or downloaded, and no shared checker setting
(language, automatic identification, learned/ignored words) is changed: each
call names its language explicitly and ignored-word state lives in a private
spell-document tag closed on shutdown. Apart from first creation of the shared
checker (main thread, as AppKit requires) every method runs on the spelling
service's single worker thread, so calls never overlap.
"""
from __future__ import annotations

import threading

from ..spelling import SpellingUnavailable
from .macos_language import LanguageUnavailable, identify, language_recognizer
ACQUIRE_TIMEOUT = 2.0
MAX_RANGES = 8


def _utf16_length(text: str) -> int:
    """Count Cocoa string-range units, not Python Unicode code points."""
    return len(text.encode("utf-16-le")) // 2


def _nsrange(value) -> tuple[int, int]:
    """Normalize PyObjC's NSRange or (NSRange, wordCount) result to integers."""
    # With the wordCount out-parameter PyObjC returns (NSRange, count).
    if isinstance(value, tuple) and len(value) == 2 and not isinstance(value[0], int):
        value = value[0]
    if hasattr(value, "location"):
        return int(value.location), int(value.length)
    return int(value[0]), int(value[1])


class MacSpellBackend:
    """Worker-confined spelling session with a private native document tag."""

    def __init__(self) -> None:
        """Acquire the checker, scheduling first creation on the main thread."""
        import objc
        from AppKit import NSSpellChecker
        from Foundation import NSOperationQueue, NSThread
        self._objc = objc
        self._spell = NSSpellChecker
        self._checker = None
        self._failure = ""
        self._ready = threading.Event()
        self._tag: int | None = None
        self._recognizer = None
        if NSThread.isMainThread() or NSSpellChecker.sharedSpellCheckerExists():
            self._adopt()
        else:
            # Creating the shared checker off the main thread can deadlock AppKit.
            NSOperationQueue.mainQueue().addOperationWithBlock_(self._adopt)

    def _adopt(self) -> None:
        """Publish checker acquisition or failure and always release waiters."""
        try:
            self._checker = self._spell.sharedSpellChecker()
            if self._checker is None:
                self._failure = "The macOS spelling service is unavailable."
        except Exception as exc:  # AppKit raises when the Services setup fails
            self._failure = f"The macOS spelling service is unavailable: {exc}"
        finally:
            self._ready.set()

    def _require(self):
        """Wait at most ACQUIRE_TIMEOUT seconds and lazily allocate our tag.

        Raise SpellingUnavailable on timeout or failed checker acquisition.
        A timeout does not cancel the queued main-thread acquisition.
        """
        if not self._ready.wait(ACQUIRE_TIMEOUT):
            raise SpellingUnavailable("Waiting for the main thread to start the macOS spelling service.")
        if self._checker is None:
            raise SpellingUnavailable(self._failure or "The macOS spelling service is unavailable.")
        if self._tag is None:
            self._tag = int(self._spell.uniqueSpellDocumentTag())
        return self._checker

    def work_scope(self):
        """Return the autorelease scope the worker must enter around native work."""
        return self._objc.autorelease_pool()

    def languages(self) -> tuple[tuple[str, ...], tuple[str, ...]]:
        """Return available dictionary tags and their deduplicated preference order.

        Locale preferences are matched only against available dictionaries;
        this lookup neither selects a checker language nor installs a dictionary.
        """
        from Foundation import NSLocale
        checker = self._require()
        available = tuple(str(tag) for tag in (checker.availableLanguages() or ()))
        installed = set(available)
        # Automatic spelling can report every dictionary as "user preferred".
        # Language & Region is the actual user preference, not that service list.
        preferred = []
        for locale in NSLocale.preferredLanguages():
            tag = str(locale).replace("-", "_")
            match = next((item for item in available if item == tag), None)
            if match is None:
                base = tag.split("_", 1)[0].casefold()
                match = next((item for item in available
                              if item.split("_", 1)[0].casefold() == base), None)
            if match:
                preferred.append(match)
        current = checker.language()
        if current:
            preferred.append(str(current))
        ordered: list[str] = []
        for tag in preferred:
            if tag in installed and tag not in ordered:
                ordered.append(tag)
        return available, tuple(ordered)

    def _language_recognizer(self):
        """Lazily own one recognizer, translating load failures for the service."""
        if self._recognizer is None:
            try:
                self._recognizer = language_recognizer()
            except LanguageUnavailable as exc:
                raise SpellingUnavailable(str(exc)) from exc
        return self._recognizer

    def identify(self, text: str) -> dict[str, float]:
        """Return language scores without leaving text in the reusable recognizer."""
        if not text.strip():
            return {}
        recognizer = self._language_recognizer()
        return identify(recognizer, text)

    def misspelled(self, word: str, language: str) -> list[tuple[int, int]]:
        """Return at most MAX_RANGES misspellings as UTF-16 (start, length) pairs."""
        checker = self._require()
        units = _utf16_length(word)
        ranges: list[tuple[int, int]] = []
        start = 0
        # Advance in Cocoa units and reject non-progressing/wrapped responses;
        # Python slicing here would misplace ranges after non-BMP characters.
        while start < units and len(ranges) < MAX_RANGES:
            location, length = _nsrange(
                checker.checkSpellingOfString_startingAt_language_wrap_inSpellDocumentWithTag_wordCount_(
                    word, start, language, False, self._tag, None))
            if length <= 0 or location < start or location >= units:
                break  # NSNotFound, or the scan wrapped
            ranges.append((location, min(length, units - location)))
            start = location + length
        return ranges

    def has_completions(self, partial: str, language: str) -> bool:
        """Report whether the dictionary offers a completion beyond the input."""
        checker = self._require()
        completions = checker.completionsForPartialWordRange_inString_language_inSpellDocumentWithTag_(
            (0, _utf16_length(partial)), partial, language, self._tag) or ()
        folded = partial.casefold()
        return any(str(item).casefold() != folded for item in completions)

    def suggestions(self, text: str, start: int, length: int,
                    language: str) -> tuple[str, tuple[str, ...]]:
        """Return the native correction and at most five guesses for a UTF-16 range.

        An absent correction becomes an empty string. Native ordering is kept;
        range validation and the decision to apply a suggestion belong to callers.
        """
        checker = self._require()
        correction = checker.correctionForWordRange_inString_language_inSpellDocumentWithTag_(
            (start, length), text, language, self._tag)
        guesses = checker.guessesForWordRange_inString_language_inSpellDocumentWithTag_(
            (start, length), text, language, self._tag) or ()
        return (str(correction) if correction else "", tuple(str(guess) for guess in list(guesses)[:5]))

    def close(self) -> None:
        """Release our recognizer and document tag, leaving the shared checker alive."""
        self._recognizer = None
        if self._tag is not None and self._checker is not None:
            self._checker.closeSpellDocumentWithTag_(self._tag)
        self._tag = None
