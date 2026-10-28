"""Constrain emoji replacement spans and safe, whitespace-preserving choices."""

from dataclasses import replace

from inklow.config import Config
from inklow.features import current_word_only, emoji_choices, mid_word, parse_choices


def test_colon_emoji_preserves_the_word_separator_and_explicit_alias():
    prefix = "Olá :sweat_smile:"
    suffix, choices = emoji_choices(prefix, Config())
    assert suffix == ":sweat_smile:"
    assert prefix[:-len(suffix)] + choices[0] == "Olá 😅"


def test_nonemoji_colons_and_disabled_search_do_not_offer_conversion():
    assert emoji_choices("https://example.com", Config()) is None
    assert emoji_choices("Horário 10:30", Config()) is None
    assert emoji_choices("a:wave", Config()) is None
    assert emoji_choices(":wave", replace(Config(), emoji_enabled=False)) is None


def test_skin_and_gender_preferences_choose_valid_complete_profession_sequences():
    config = replace(Config(), emoji_gender="female", emoji_skin_tone=3,
                     emoji_include_neutral=True)
    suffix, choices = emoji_choices("Equipe :technologist", config)
    assert suffix == ":technologist"
    assert choices[:2] == ["👩🏽‍💻", "👩‍💻"]
    _, toned_only = emoji_choices(":technologist", replace(config, emoji_include_neutral=False))
    assert "👩‍💻" not in toned_only


def test_emoticon_conversion_does_not_consume_preceding_text():
    suffix, choices = emoji_choices("Obrigado <3", Config())
    assert "Obrigado <3"[:-len(suffix)] + choices[0] == "Obrigado ❤️"
    assert emoji_choices("code<3", Config()) is None


def test_choice_parser_preserves_insertion_spacing_and_rejects_unsafe_values():
    payload = '[" resposta", " resposta", " ótimo", "\\nunsafe", "' + "long " * 9 + '", "...", 3, {"x":1}]'
    assert parse_choices(payload) == [" resposta", " ótimo"]
    assert parse_choices('{"choices":["yes"]}') == []
    assert parse_choices("not structured output") == []


def test_current_word_only_finishes_the_word_and_never_starts_the_next():
    assert current_word_only("I will recie", "ve the parcel") == "ve"
    assert current_word_only("I do", "n't know") == "n't"
    assert current_word_only("Olá, coraç", "ão aberto") == "ão"
    assert current_word_only("I will ", "receive it") == ""
    assert current_word_only("Done.", " Next") == ""
    assert not mid_word("word ") and mid_word("don'")
    assert current_word_only("don'", "t go") == "t"
