from dataclasses import replace

import pytest

from inklow.core import Snapshot, SuggestionState, take_word


def test_outdated_generation_cannot_replace_new_suggestion():
    state = SuggestionState()
    original = Snapshot("writer", "body", "Let us meet", 11)
    old_generation = state.generation
    generation = state.invalidate()
    revised = replace(original, text="Let us meet tomorrow", caret=20)
    assert state.offer(generation, revised, " at noon")
    assert not state.offer(old_generation, original, " yesterday")
    assert state.accept(revised, full=True) == " at noon"


@pytest.mark.parametrize("changes", [
    {"app_id": "other"}, {"field_id": "subject"}, {"caret": 3},
    {"text": "Let us eat!"}, {"selection_length": 1}, {"protected": True},
])
def test_acceptance_rejects_changed_or_unsafe_field(changes):
    snapshot = Snapshot("writer", "body", "Let us meet", 11)
    state = SuggestionState()
    state.offer(state.generation, snapshot, " tomorrow")
    assert state.accept(replace(snapshot, **changes)) == ""
    assert state.text == ""


def test_repeated_acceptance_preserves_unicode_and_suffix():
    snapshot = Snapshot("writer", "body", "Olá! 🖋|", 6)
    state = SuggestionState()
    state.offer(state.generation, snapshot, " café amanhã")
    assert state.accept(snapshot) == " café "
    current = replace(snapshot, text="Olá! 🖋 café |", caret=12)
    assert state.accept(current, full=True) == "amanhã"
    assert state.snapshot.text == "Olá! 🖋 café amanhã|"
    assert state.text == ""


@pytest.mark.parametrize("snapshot", [
    Snapshot("a", "f", "password", 8, protected=True),
    Snapshot("a", "f", "selected", 8, selection_length=2),
    Snapshot("a", "f", "short", -1),
    Snapshot("a", "f", "short", 99),
])
def test_unsafe_snapshot_is_ineligible(snapshot):
    assert not snapshot.eligible(1)


def test_word_acceptance_preserves_spacing_and_punctuation():
    assert take_word("  café,\t tomorrow") == ("  café,\t ", "tomorrow")
