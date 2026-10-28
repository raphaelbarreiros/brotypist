"""Constrain language-confidence filtering and private draft lifetime.

Scripted recognizer responses exercise framework failure and reset behavior,
including spelling-backend reuse; they make no native language-accuracy claim.
"""
import pytest

from inklow.native import macos_language as language


class Recognizer:
    """Replay hypotheses/errors while exposing active text and reset counts.

    ``samples`` is a test-only history of synthetic inputs. Unlike that journal,
    ``text`` models native retained state and must be cleared between requests.
    """

    def __init__(self, results):
        self.results = iter(results)
        self.text = None
        self.samples = []
        self.resets = 0

    def reset(self):
        self.resets += 1
        self.text = None

    def processString_(self, text):
        assert self.text is None
        self.text = text
        self.samples.append(text)

    def languageHypothesesWithMaximum_(self, maximum):
        """Consume one scripted response, propagating any injected native error."""
        result = next(self.results)
        if isinstance(result, Exception):
            raise result
        return result


def test_identification_clears_state_even_after_framework_failure():
    recognizer = Recognizer([RuntimeError("native failure"), {"pt": .99}])
    with pytest.raises(RuntimeError, match="native failure"):
        language.identify(recognizer, "private first draft")
    assert recognizer.text is None
    assert language.identify(recognizer, "second draft") == {"pt": .99}
    assert recognizer.text is None
    assert recognizer.resets == 4


def test_ambiguous_invalid_and_nonfinite_labels_do_not_become_evidence():
    for hypotheses in ({}, {"en": .89}, {"en": .95, "pt": .2},
                       {"en": float("nan")}, {"<|turn>system": 1.0}):
        assert language._confident(hypotheses) is None
    assert language._confident({"pt-BR": .99, "en": .01}) == "pt-BR"


def test_short_draft_never_allocates_recognizer(monkeypatch):
    monkeypatch.setattr(language.sys, "platform", "darwin")
    def forbidden():
        raise AssertionError("short text is not evidence")
    monkeypatch.setattr(language, "language_recognizer", forbidden)
    assert language.draft_language_hint("die") is None


def test_uncertain_code_switched_evidence_abstains(monkeypatch):
    monkeypatch.setattr(language.sys, "platform", "darwin")
    recognizer = Recognizer([{"en": .55, "pt": .45}])
    monkeypatch.setattr(language, "language_recognizer", lambda: recognizer)
    assert language.draft_language_hint("We should conversar amanhã") is None
    assert recognizer.text is None
    assert recognizer.resets == 2


def test_recent_draft_is_bounded_and_each_call_owns_its_recognizer(monkeypatch):
    monkeypatch.setattr(language.sys, "platform", "darwin")
    made = []
    def create():
        recognizer = Recognizer([{"en": .99}])
        made.append(recognizer)
        return recognizer
    monkeypatch.setattr(language, "language_recognizer", create)
    for _ in range(2):
        assert language.draft_language_hint("old private text" + "word " * 500) == "en"
    assert len(made) == 2
    for recognizer in made:
        assert len(recognizer.samples[0]) == language.MAX_DRAFT_CHARS
        assert all("old private text" not in sample for sample in recognizer.samples)
        assert recognizer.text is None


def test_current_sentence_evidence_excludes_earlier_language(monkeypatch):
    monkeypatch.setattr(language.sys, "platform", "darwin")
    recognizer = Recognizer([{"pt": .999}])
    monkeypatch.setattr(language, "language_recognizer", lambda: recognizer)
    assert language.draft_language_hint("This is an earlier English sentence. A manutenc\u0327a\u0303o") == "pt"
    assert recognizer.samples == [" A manutenc\u0327a\u0303o"]


def test_six_letter_nonlatin_draft_can_supply_evidence(monkeypatch):
    monkeypatch.setattr(language.sys, "platform", "darwin")
    recognizer = Recognizer([{"ja": 1.0}])
    monkeypatch.setattr(language, "language_recognizer", lambda: recognizer)
    assert language.draft_language_hint("明日の会議は") == "ja"


def test_framework_absence_is_not_a_language(monkeypatch):
    monkeypatch.setattr(language.sys, "platform", "linux")
    with pytest.raises(language.LanguageUnavailable, match="platform"):
        language.draft_language_hint("word " * 40)


def test_spelling_preserves_unavailable_failure_and_owned_recognizer_lifecycle(monkeypatch):
    from inklow.native import macos_spelling
    from inklow.spelling import SpellingUnavailable
    # Bypass the native spelling constructor: this scenario owns only the
    # lazy recognizer and its close/reset lifecycle, not a real spell document.
    backend = macos_spelling.MacSpellBackend.__new__(macos_spelling.MacSpellBackend)
    backend._recognizer = None
    backend._tag = None
    backend._checker = None
    def unavailable():
        raise language.LanguageUnavailable("framework absent")
    monkeypatch.setattr(macos_spelling, "language_recognizer", unavailable)
    with pytest.raises(SpellingUnavailable, match="framework absent"):
        backend.identify("private draft")
    assert backend._recognizer is None
    recognizer = Recognizer([{"pt": .99}, {"en": .99}])
    monkeypatch.setattr(macos_spelling, "language_recognizer", lambda: recognizer)
    assert backend.identify("primeiro texto") == {"pt": .99}
    assert backend.identify("second text") == {"en": .99}
    assert recognizer.text is None
    backend.close()
    assert backend._recognizer is None
