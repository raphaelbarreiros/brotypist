from dataclasses import replace

import pytest

from inklow.core import Snapshot, SuggestionState


def suggested_state():
    snapshot = Snapshot("writer", "body", "Olá 🖋|existing suffix", 5)
    state = SuggestionState()
    assert state.offer(state.generation, snapshot, " café amanhã")
    return state, snapshot


@pytest.mark.parametrize("full, accepted, remainder", [
    (False, " café ", "amanhã"), (True, " café amanhã", ""),
])
def test_acceptance_rejects_inflight_stream_without_losing_remainder(full, accepted, remainder):
    # R4: old streaming generations cannot revive content the user already accepted.
    state, snapshot = suggested_state()
    generation = state.generation
    assert state.accept(snapshot, full=full) == accepted
    assert state.generation == generation + 1
    expected = replace(snapshot, text=snapshot.prefix + accepted + snapshot.suffix,
                       caret=snapshot.caret + len(accepted))
    assert state.snapshot == expected
    assert not state.offer(generation, snapshot, " café amanhã e ontem")
    assert state.snapshot == expected
    assert state.text == remainder
    if not remainder:
        assert state.accept(expected) == ""
        assert state.generation == generation + 1


def test_typing_through_unicode_suggestion_preserves_suffix_and_rejects_old_stream():
    # R4: advance follows exact insertion, including multi-byte characters and spaces.
    state, snapshot = suggested_state()
    generation = state.generation
    inserted = " café"
    current = replace(snapshot, text=snapshot.prefix + inserted + snapshot.suffix,
                      caret=snapshot.caret + len(inserted), caret_rect=(10, 20, 1, 12))
    assert state.advance(current)
    assert state.snapshot == current
    assert state.text == " amanhã"
    assert state.generation == generation + 1
    assert not state.offer(generation, snapshot, " café errado")
    assert state.accept(current, full=True) == " amanhã"
    assert state.snapshot.text == "Olá 🖋 café amanhã|existing suffix"
    assert state.generation == generation + 2


@pytest.mark.parametrize("changes", [
    {"app_id": "mail"}, {"field_id": "subject"}, {"protected": True},
    {"selection_length": 1}, {"caret": -1}, {"caret": 1000},
    {"caret": 5}, {"text": "Olá 🖋 café|changed suffix"},
    {"text": "Olá café|existing suffix", "caret": 8},
    {"text": "Olá 🖋 chá|existing suffix", "caret": 9},
])
def test_unrelated_or_unsafe_edit_does_not_advance_suggestion(changes):
    state, snapshot = suggested_state()
    current = replace(snapshot, text="Olá 🖋 café|existing suffix", caret=10)
    before = (state.generation, state.snapshot, state.text)
    assert not state.advance(replace(current, **changes))
    assert (state.generation, state.snapshot, state.text) == before


def test_typing_entire_suggestion_empties_it_and_cannot_restart_old_stream():
    state, snapshot = suggested_state()
    generation = state.generation
    current = replace(snapshot, text="Olá 🖋 café amanhã|existing suffix", caret=17)
    assert state.advance(current)
    assert state.text == ""
    assert state.snapshot == current
    assert state.generation == generation + 1
    assert not state.offer(generation, snapshot, " café amanhã duplicado")
    assert not state.advance(current)
    assert state.generation == generation + 1
