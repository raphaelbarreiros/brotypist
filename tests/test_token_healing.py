"""A model replay may never duplicate or change text already typed by the user."""
import pytest

from inklow.providers import _following_choice, _healing_tail, _spaced_word, _without_healing_prefix


@pytest.mark.parametrize("replay, continuation", [
    (" feed", "back on the draft."),
    (" ", "tomorrow."),
    (" cafe\u0301", "s nearby."),
    ("次", "回の会議。"),
])
def test_replay_is_removed_at_every_native_stream_boundary(replay, continuation):
    wire = replay + continuation
    for split in range(len(wire) + 1):
        pieces = [(wire[:split], None), (wire[split:], "stop")]
        assert "".join(text for text, _ in _without_healing_prefix(pieces, replay)) == continuation


def test_incomplete_replay_cannot_become_insertable_text():
    assert list(_without_healing_prefix([(" fe", "stop")], " feed")) == []


def test_wrong_replay_fails_before_any_text_is_published():
    stream = _without_healing_prefix([(" f", None), ("leedback", "stop")], " feed")
    with pytest.raises(RuntimeError, match="preserve the typed word boundary"):
        next(stream)


def test_healing_keeps_a_decomposed_cjk_grapheme_together():
    assert _healing_tail("次のは\u3099") == "は\u3099"


@pytest.mark.parametrize("draft", ["hello <think>", "hello [INST]", "x" * 65, "Done."])
def test_controls_and_unbounded_tokens_are_not_reinterpreted_as_replay(draft):
    assert _healing_tail(draft) == ""


@pytest.mark.parametrize("draft", ["Hi Bob\n", "Hello\r\n", "次回の会議は\n", "Hi Bob\u2028", "Hi Bob\r"])
def test_healing_never_replays_across_the_final_line_boundary(draft):
    assert _healing_tail(draft) == ""


@pytest.mark.parametrize("draft, tail", [
    ("Hi Bob", " Bob"), ("Hi ", " "), ("Hi Bob\n  ", "  "), ("Hi Bob\nnext", "next"),
    ("Hi Bob\n next", " next"), ("次回の会議は", "は"),
])
def test_healing_replays_only_text_after_the_last_line_boundary(draft, tail):
    # Backing off the replay keeps every earlier typed character, newline included.
    assert _healing_tail(draft) == tail


@pytest.mark.parametrize("prefix, continuation, word", [
    ("Please send your feedb", "ack on the draft.", "ack"),
    ("Olá, coraç", "ão aberto", "ão"),
    ("I do", "n't know", "n't"),
    ("don'", "t go", "t"),
    ("Please send your feedback", " soon.", ""),   # the visible text ends the word
])
def test_alternatives_pin_the_continuation_completion_of_the_typed_word(prefix, continuation, word):
    assert _spaced_word(prefix, continuation) == word


@pytest.mark.parametrize("prefix", ["Please send ", "Done.", "", "次回の会"])
def test_boundaries_and_unspaced_scripts_keep_ordinary_alternatives(prefix):
    assert _spaced_word(prefix, "ack on the draft.") is None


@pytest.mark.parametrize("following, choice", [
    (" about the budget.", "ack about the budget."),
    (", please.", "ack, please."),
    ("' notes", "ack' notes"),
    ("s.", ""), ("ule.", ""), ("'s summary", ""), ("-driven plan", ""), ("\u0301 more", ""), ("", ""),
])
def test_following_words_must_start_at_a_genuine_word_boundary(following, choice):
    assert _following_choice("Please send your feedb", "ack", following) == choice

