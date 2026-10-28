"""Recognize draft languages locally with Apple's optional NaturalLanguage.

Recognizers are caller-owned mutable objects; do not share one across concurrent
calls. Recognition clears their accumulated text and does not change settings.
"""
from __future__ import annotations

import math
import re
import sys

NATURAL_LANGUAGE = "/System/Library/Frameworks/NaturalLanguage.framework"
MAX_HYPOTHESES = 5
MAX_DRAFT_CHARS = 1200


class LanguageUnavailable(RuntimeError):
    """The optional local framework cannot provide recognition."""


def language_recognizer():
    """Create a recognizer, or raise LanguageUnavailable if loading fails.

    Fall back to the system bundle when the dedicated PyObjC wrapper is absent.
    The caller owns the returned instance and serializes its use.
    """
    try:
        import objc
        try:
            from NaturalLanguage import NLLanguageRecognizer
        except ImportError:
            from Foundation import NSBundle
            bundle = NSBundle.bundleWithPath_(NATURAL_LANGUAGE)
            loaded = bundle.loadAndReturnError_(None) if bundle is not None else False
            if not (loaded[0] if isinstance(loaded, tuple) else loaded):
                raise LanguageUnavailable("Apple NaturalLanguage could not be loaded.")
            NLLanguageRecognizer = objc.lookUpClass("NLLanguageRecognizer")
        recognizer = NLLanguageRecognizer.alloc().init()
        if recognizer is None:
            raise LanguageUnavailable("Apple NaturalLanguage is unavailable.")
        return recognizer
    except (ImportError, AttributeError) as exc:
        raise LanguageUnavailable(f"Apple NaturalLanguage is unavailable: {exc}") from exc
    except Exception as exc:
        if isinstance(exc, LanguageUnavailable):
            raise
        raise LanguageUnavailable(f"Apple NaturalLanguage is unavailable: {exc}") from exc


def identify(recognizer, text: str) -> dict[str, float]:
    """Return up to MAX_HYPOTHESES language scores for this text alone.

    Reset the supplied recognizer before processing and in the cleanup path;
    native recognition errors propagate to the caller.
    """
    recognizer.reset()
    try:
        if not text.strip():
            return {}
        recognizer.processString_(text)
        hypotheses = recognizer.languageHypothesesWithMaximum_(MAX_HYPOTHESES) or {}
        return {str(tag): float(value) for tag, value in hypotheses.items()}
    finally:
        recognizer.reset()


def _confident(hypotheses: dict[str, float]) -> str | None:
    """Return a valid language tag only for a finite, strongly separated winner."""
    ranked = sorted(((tag, value) for tag, value in hypotheses.items()
                     if math.isfinite(value)), key=lambda item: item[1], reverse=True)
    if not ranked:
        return None
    tag, score = ranked[0]
    runner_up = ranked[1][1] if len(ranked) > 1 else 0.0
    if score < 0.90 or score - runner_up < 0.80:
        return None
    return tag if re.fullmatch(r"[A-Za-z]{2,8}(?:-[A-Za-z0-9]{1,8})*", tag) else None


def draft_language_hint(prefix: str) -> str | None:
    """Optional high-confidence evidence from the current recent draft only.

    A fresh recognizer belongs exclusively to this call/thread. Absence is
    explicitly represented by LanguageUnavailable, not a detected language.
    References, suffixes and user locale never participate.

    Returns None for short or ambiguous drafts. Framework and recognition
    failures raise LanguageUnavailable so callers can distinguish missing
    evidence from an unavailable backend.
    """
    if sys.platform != "darwin":
        raise LanguageUnavailable("Apple NaturalLanguage is unavailable on this platform.")
    text = prefix[-MAX_DRAFT_CHARS:]
    # Prefer the sentence being written over an earlier language. A very short
    # tail has insufficient evidence; retain the bounded draft in that case.
    tail = re.split(r"[.!?。！？\n]", text)[-1]
    if sum(char.isalpha() for char in tail) >= 6:
        text = tail
    if sum(char.isalpha() for char in text) < 6:
        return None
    try:
        return _confident(identify(language_recognizer(), text))
    except LanguageUnavailable:
        raise
    except Exception as exc:
        raise LanguageUnavailable(f"Apple NaturalLanguage recognition failed: {exc}") from exc
