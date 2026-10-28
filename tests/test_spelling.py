import contextlib
import sys
import threading
import time
import unicodedata
from dataclasses import replace

import pytest

from inklow.core import Snapshot
from inklow.spelling import SpellingService


def _units(text):
    return len(text.encode("utf-16-le")) // 2


class Dictionary:
    """Tiny local word lists standing in for the native checker."""
    words = {
        "en": {"we", "will", "receive", "send", "feedback", "the", "well", "known", "it's"},
        "pt_BR": {"até", "amanhã", "café", "você"},
    }
    fixes = {"recieve": "receive", "amánha": "amanhã", "knwon": "known"}

    def __init__(self, hypotheses):
        self.hypotheses = hypotheses
        self.gate = threading.Event()
        self.gate.set()
        self.entered = threading.Event()
        self.checked = []

    def work_scope(self):
        return contextlib.nullcontext()

    def languages(self):
        return ("en", "pt_BR"), ("en",)

    def identify(self, text):
        return dict(self.hypotheses)

    def misspelled(self, word, language):
        self.entered.set()
        self.gate.wait(5)
        self.checked.append((word, language))
        assert unicodedata.is_normalized("NFC", word)
        ranges, offset = [], 0
        for part in word.split("-"):
            if part.casefold() not in self.words[language]:
                ranges.append((offset, _units(part)))
            offset += _units(part) + 1
        return ranges

    def has_completions(self, partial, language):
        folded = partial.casefold()
        return any(w.startswith(folded) and w != folded for w in self.words[language])

    def suggestions(self, text, start, length, language):
        flagged = text.encode("utf-16-le")[2 * start:2 * (start + length)].decode("utf-16-le")
        return self.fixes.get(flagged, ""), ()

    def close(self):
        pass


def settle(service, snapshot, woke, timeout=5.0):
    deadline = time.monotonic() + timeout
    while True:
        result = service.assess(snapshot)
        if result is not None:
            return result
        assert time.monotonic() < deadline, "spelling result never arrived"
        woke.wait(0.05)
        woke.clear()


def service_for(hypotheses):
    woke = threading.Event()
    backend = Dictionary(hypotheses)
    return SpellingService(woke.set, backend=backend), backend, woke


@pytest.mark.parametrize("tail", ["", ". "])
def test_typo_fix_uses_python_indexes_after_astral_text(tail):
    service, _, woke = service_for({"en": 0.95})
    text = "🖋🙂 we will recieve" + tail
    snapshot = Snapshot("mail", "body", text, len(text))
    result = settle(service, snapshot, woke)
    assert result.suspected and result.reason == "typo"
    assert (result.start, result.length) == (text.index("recieve"), 7)
    assert text[result.start:result.start + result.length] == result.word == "recieve"
    assert result.replacement == "receive" and result.language == "en"
    assert result.finished is bool(tail)
    service.close()


def test_valid_unfinished_prefix_is_not_a_typo_until_finished():
    service, _, woke = service_for({"en": 0.95})
    typing = Snapshot("mail", "body", "send the feedb", 14)
    result = settle(service, typing, woke)
    assert result.reason == "partial" and not result.suspected
    finished = replace(typing, text="send the feedb ", caret=15)
    assert settle(service, finished, woke).suspected
    service.close()


@pytest.mark.parametrize("words", [
    ("amánha", "ama\u0301nha"),
    ("ama\u0301nha", "amánha"),
])
def test_cached_verdicts_preserve_each_original_cluster_range(words):
    service, _, woke = service_for({"pt": 0.9})
    try:
        for word in words:
            prefix, suffix = "🙂 Até ", " na terça."
            text = prefix + word + suffix
            result = settle(service, Snapshot("notes", "body", text, len(prefix + word)), woke)
            assert result.suspected and result.language == "pt_BR"
            assert (result.start, result.length) == (len(prefix), len(word))
            replaced = text[:result.start] + result.replacement + text[result.start + result.length:]
            assert replaced == "🙂 Até amanhã na terça."
        accepted = Snapshot("notes", "body", "Até cafe\u0301", 9)
        assert settle(service, accepted, woke).reason == "ok"
    finally:
        service.close()


def test_flagged_part_of_a_compound_maps_back_to_its_own_range():
    service, _, woke = service_for({"en": 0.95})
    text = "it is well-knwon "
    result = settle(service, Snapshot("mail", "body", text, len(text)), woke)
    assert result.word == "knwon" and result.start == text.index("knwon")
    assert result.replacement == "known"
    assert settle(service, Snapshot("mail", "body", "it's", 4), woke).reason == "ok"
    service.close()


def test_unsupported_language_is_distinguished_from_a_typo():
    text = "ninakupenda sana rafikii"
    foreign, backend, woke = service_for({"sw": 0.92, "en": 0.03})
    result = settle(foreign, Snapshot("chat", "msg", text, len(text)), woke)
    assert result.reason == "unsupported-language" and not result.suspected
    assert result.language == "sw" and not backend.checked  # never checked as English
    foreign.close()
    english, _, woke = service_for({"en": 0.9})
    assert settle(english, Snapshot("chat", "msg", text, len(text)), woke).suspected
    english.close()


def test_typo_requires_rejection_by_every_plausible_language():
    service, backend, woke = service_for({"en": 0.55, "pt": 0.4})
    text = "see you amanhã"
    result = settle(service, Snapshot("chat", "msg", text, len(text)), woke)
    assert result.reason == "ok" and result.language == "pt_BR"
    assert {language for _, language in backend.checked} == {"en", "pt_BR"}
    service.close()


def test_stale_result_never_attaches_to_another_field():
    service, backend, woke = service_for({"en": 0.95})
    backend.gate.clear()
    first = Snapshot("mail", "subject", "we will recieve", 15)
    other = replace(first, field_id="body")
    try:
        assert service.assess(first) is None
        assert backend.entered.wait(2), "First spelling request never entered the blocked backend"
        assert service.assess(other) is None
        backend.gate.set()
        result = settle(service, other, woke)
        assert result.key == other.key
        assert service.assess(first) is None  # superseded check was discarded, not reused
    finally:
        backend.gate.set()
        service.close()


@pytest.mark.parametrize("changes", [{"protected": True}, {"selection_length": 2, "caret": 3},
                                     {"caret": 4}])
def test_unsafe_or_mid_word_snapshots_answer_immediately_without_checking(changes):
    service, backend, _ = service_for({"en": 0.95})
    snapshot = replace(Snapshot("mail", "body", "we recieve", 10), **changes)
    result = service.assess(snapshot)
    assert result is not None and not result.suspected and result.key == snapshot.key
    assert not backend.checked
    service.close()


@pytest.mark.skipif(sys.platform != "darwin", reason="macOS spelling service")
def test_macos_spelling_offers_receive():
    woke = threading.Event()
    service = SpellingService(woke.set)
    assert not service.diagnostic
    text = "We will recieve the parcel tomorrow and we will recieve "
    typo = Snapshot("test", "field", text, len(text))
    result = settle(service, typo, woke, timeout=15)
    assert result.suspected and result.replacement == "receive", result
    assert typo.text[result.start:result.start + result.length] == "recieve"
    service.close()
