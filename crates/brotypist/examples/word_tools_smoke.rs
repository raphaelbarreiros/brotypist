//! Deterministic public-seam checks only: no models, profiles, native edits or UI.
//! Run with `cargo run -p brotypist --example word_tools_smoke`.
use anyhow::{Context, Result, ensure};
use brotypist::{
    config::Config,
    core::{Snapshot, SuggestionState, take_word},
    features::{
        current_word_only, emoji_choices, include_punctuation, match_choice_subsequence, mid_word,
        parse_choices, safe_choice,
    },
};
use serde_json::json;

fn word_boundaries() -> Result<usize> {
    let cases = [
        ("We disc", "uss the plan", "uss"),
        ("We do", "n't know", "n't"),
        ("We don'", "t know", "t"),
        ("We don'", "'t know", ""),
        ("A well-", "-known result", ""),
        ("A well", "-known result", "-known"),
        ("cafe", "\u{301} amanhã", "\u{301}"),
        ("café", "s amanhã", "s"),
        ("你", "好世界 next", "好世界"),
        ("mixЖ", "界e\u{301} next", "界e\u{301}"),
        ("word ", "next word", ""),
        ("Done.", " Next", ""),
        ("❤️", "next word", ""),
        ("👩🏽‍⚕️", "next word", ""),
        ("1️⃣", "next word", ""),
        ("1", "\u{fe0f}\u{20e3} next", ""),
        ("x", "a1️⃣ next", "a"),
        ("\u{301}", "next word", ""),
    ];
    for (index, (prefix, continuation, expected)) in cases.iter().enumerate() {
        ensure!(
            current_word_only(prefix, continuation) == *expected,
            "word boundary fixture {index}"
        );
    }
    ensure!(
        mid_word("cafe\u{301}") && !mid_word("❤️") && !mid_word("word--"),
        "word base classification"
    );
    // These are token-shaping seams, not evidence that the model generated a
    // next-word or fallback alternative. Provider sampling has its own corpus.
    ensure!(
        take_word(" next, later", true, true) == (" next, ".into(), "later".into()),
        "next-word punctuation and spaces"
    );
    ensure!(
        take_word(" next, later", false, false) == (" next".into(), ", later".into()),
        "next-word punctuation exclusion"
    );
    Ok(cases.len() + 3)
}

fn emoji_catalogue() -> Result<usize> {
    let mut config = Config::default();
    let aliases = [
        ("Hello :heart:", ":heart:", "❤️"),
        ("Hello :wave", ":wave", "👋"),
        ("Hello :+1:", ":+1:", "👍"),
        ("Hello :-1:", ":-1:", "👎"),
        ("Hello :-)", ":-)", "🙂"),
        ("Olá :coração", ":coração", "❤️"),
        ("Hello :", ":", "😀"),
    ];
    for (index, (prefix, expected_typed, expected_first)) in aliases.iter().enumerate() {
        let (typed, choices) =
            emoji_choices(prefix, &config).context("catalogue trigger missing")?;
        ensure!(
            typed == *expected_typed
                && choices.first().map(String::as_str) == Some(*expected_first),
            "emoji alias fixture {index}"
        );
        ensure!(
            prefix.ends_with(&typed) && choices.len() <= 5,
            "emoji exact suffix and bound"
        );
    }
    let absent = [
        "https://example.com",
        "10:30",
        "email:wave",
        "word<3",
        "foo:)",
        ":wave ",
        ":-",
        ":___",
        ":--:",
        ":\u{301}",
        "happy",
        "👩🏽‍💻",
    ];
    for (index, prefix) in absent.iter().enumerate() {
        ensure!(
            emoji_choices(prefix, &config).is_none(),
            "emoji non-trigger fixture {index}"
        );
    }
    config.emoji_gender = "female".into();
    config.emoji_skin_tone = 3;
    config.emoji_include_neutral = true;
    let (_, choices) =
        emoji_choices(":technologist", &config).context("profession trigger missing")?;
    ensure!(
        choices.iter().take(2).map(String::as_str).eq(["👩🏽‍💻", "👩‍💻"]),
        "licensed ZWJ/tone variants"
    );
    config.emoji_include_neutral = false;
    let (_, choices) =
        emoji_choices(":technologist", &config).context("toned profession missing")?;
    ensure!(
        choices.first().map(String::as_str) == Some("👩🏽‍💻")
            && !choices.iter().any(|value| value == "👩‍💻"),
        "neutral exclusion"
    );
    config.emoticons_enabled = false;
    ensure!(
        emoji_choices("Hello :-)", &config).is_none(),
        "disabled emoticon"
    );
    config.emoji_enabled = false;
    ensure!(emoji_choices(":wave", &config).is_none(), "disabled emoji");
    Ok(aliases.len() + absent.len() + 4)
}

fn decoded_choices() -> Result<usize> {
    let choices = parse_choices(
        r#"[" fast", " FAST", " rápido", "中文", "cafe\u0301", "café", "bad\nline", "bad\u202eorder"]"#,
        9,
    );
    ensure!(
        choices == [" fast", " rápido", "中文", "cafe\u{301}", "café"],
        "utility filtering must retain original Unicode spelling"
    );
    ensure!(parse_choices("[]", 5).is_empty(), "safe abstention");
    ensure!(
        parse_choices(
            r#"["one two three four five six seven eight nine", "<|im_start|>assistant"]"#,
            5
        )
        .is_empty(),
        "bounded utility payload"
    );
    // Model-produced utility text and licensed local emoji are separate paths;
    // this smoke must not relax format-character guards to admit arbitrary ZWJ.
    ensure!(
        !safe_choice("👩🏽‍💻") && !safe_choice("text\u{200d}text"),
        "utility format-character guard"
    );
    let offered = [
        " short",
        "a deliberately omitted long row",
        "  cafe\u{301}  👩🏽‍💻",
        " café",
        "❤️",
        "❤︎",
        "1️⃣",
    ]
    .map(String::from);
    let map = match_choice_subsequence(
        &offered,
        [" short", "  cafe\u{301}  👩🏽‍💻", " café", "❤️", "❤︎", "1️⃣"],
    )
    .context("exact displayed subsequence refused")?;
    for (displayed, original) in [0, 2, 3, 4, 5, 6].into_iter().enumerate() {
        ensure!(
            map.original_index(displayed) == Some(original),
            "displayed ordinal changed authorization"
        );
    }
    ensure!(
        map.original_index(6).is_none(),
        "out-of-range displayed choice authorized"
    );
    for altered in ["short", "  café  👩🏽‍💻", "  cafe\u{301}  👩🏽💻", "❤", "1", "👩🏻‍💻"]
    {
        ensure!(
            match_choice_subsequence(&offered, [altered]).is_none(),
            "changed visible bytes authorized"
        );
    }
    ensure!(
        match_choice_subsequence(&offered, [" café", " short"]).is_none(),
        "reordered rows authorized"
    );
    Ok(4 + 6 + 1 + 6 + 1)
}

fn snapshot(prefix: &str, suffix: &str) -> Snapshot {
    Snapshot {
        app_id: "word-tools-smoke".into(),
        field_id: "synthetic-only".into(),
        text: format!("{prefix}{suffix}").into(),
        caret: prefix.chars().count(),
        ..Snapshot::default()
    }
}

fn acceptance() -> Result<usize> {
    let cases = [
        (" café, tomorrow", false, true, "always", " café, "),
        (" café, tomorrow", false, false, "never", " café"),
        (" café, tomorrow", false, false, "full", " café"),
        (
            " café, tomorrow!  ",
            true,
            false,
            "full",
            " café, tomorrow!",
        ),
        (
            " café, tomorrow!  ",
            true,
            false,
            "never",
            " café, tomorrow",
        ),
        (" cafe\u{301} 中文", false, false, "always", " cafe\u{301}"),
        (" 👩🏽‍💻 next", false, false, "always", " 👩🏽‍💻"),
        (" ❤️ ❤︎ 1️⃣", true, false, "always", " ❤️ ❤︎ 1️⃣"),
        (" Ж界 café", true, true, "always", " Ж界 café"),
    ];
    for (index, (offer, full, space, punctuation, expected)) in cases.iter().enumerate() {
        let before = snapshot("Draft 🖋 e\u{301}", "|unchanged suffix");
        let mut state = SuggestionState::new();
        ensure!(
            state.offer(state.generation, before.clone(), (*offer).into()),
            "offer fixture {index}"
        );
        let accepted = state
            .accept(
                &before,
                *full,
                *space,
                include_punctuation(punctuation, *full),
                Some(offer),
            )
            .context("supported acceptance refused")?;
        ensure!(accepted == *expected, "acceptance fixture {index}");
        let predicted = state
            .snapshot
            .as_ref()
            .context("predicted snapshot missing")?;
        ensure!(
            predicted.prefix() == format!("{}{}", before.prefix(), expected)
                && predicted.suffix() == before.suffix(),
            "prefix/suffix preservation"
        );
        ensure!(
            predicted.caret == before.caret + expected.chars().count(),
            "scalar caret treated as bytes or UTF-16"
        );
    }
    let before = snapshot("Draft", "");
    let mut state = SuggestionState::new();
    ensure!(
        state.offer(0, before.clone(), " next word".into()),
        "visible-prefix fixture"
    );
    ensure!(
        state
            .accept(&before, false, false, true, Some(" ne"))
            .is_none(),
        "partial word authorized beyond visible text"
    );
    ensure!(
        state
            .accept(&before, true, false, true, Some(" forged"))
            .is_none(),
        "forged visible text accepted"
    );
    ensure!(
        state
            .accept(&before, true, false, true, Some(" next"))
            .as_deref()
            == Some(" next"),
        "exact full visible prefix refused"
    );
    for mode in 0..3 {
        let mut state = SuggestionState::new();
        ensure!(
            state.offer(0, before.clone(), " next".into()),
            "stale fixture"
        );
        let mut changed = before.clone();
        match mode {
            0 => changed.text = "Changed draft".into(),
            1 => changed.protected = true,
            _ => changed.field_id = "other-field".into(),
        }
        ensure!(
            state.accept(&changed, true, false, true, None).is_none(),
            "stale/protected authority accepted"
        );
    }
    // A selected synonym/typo target is not an inline insertion capability.
    // Its real native replacement must still go through the runtime's existing
    // selection/readback fences; this fixture never edits a host document.
    let mut selected = snapshot("Before ", "cafe\u{301} 👩🏽‍💻 after");
    selected.selection_length = "cafe\u{301} 👩🏽‍💻".chars().count();
    ensure!(
        selected.selected_text() == "cafe\u{301} 👩🏽‍💻" && selected.selection_suffix() == " after",
        "selected scalar range changed"
    );
    let mut state = SuggestionState::new();
    ensure!(
        !state.offer(0, selected.clone(), "replacement".into()),
        "selection acquired inline rewrite authority"
    );
    ensure!(
        state.accept(&selected, true, false, true, None).is_none(),
        "selection accepted without supported rewrite owner"
    );
    Ok(cases.len() + 3 + 3 + 3)
}

fn main() -> Result<()> {
    let word_checks = word_boundaries()?;
    let emoji_checks = emoji_catalogue()?;
    let choice_checks = decoded_choices()?;
    let acceptance_checks = acceptance()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "schema": "brotypist-word-tools-smoke-v1",
            "passed": true,
            "checks": {"word_boundaries": word_checks, "emoji_catalogue": emoji_checks, "decoded_choices_and_ordinals": choice_checks, "pure_acceptance": acceptance_checks},
            "scope": "deterministic public feature/core APIs with synthetic data; no model, profile, UI or native edits",
            "not_proven": ["model synonym semantics", "model next-word/fallback quality", "native insertion/readback", "physical keyboard/IME", "general grapheme segmentation"],
            "trigger_customization": "not adopted: existing explicit aliases/catalogue/emoticons cover these fixtures; no measured need or collision/consent evidence for a new preference"
        }))?
    );
    Ok(())
}
