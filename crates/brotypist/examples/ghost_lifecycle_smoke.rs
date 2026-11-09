//! Headless public Core lifecycle proof using synthetic snapshots only.
//! No native bridge, hooks, profile, keychain, model, UI, or successful host edit.
//! Core acceptance is a prediction, never a native commit/provenance receipt.
use anyhow::{Result, ensure};
use brotypist::core::{Snapshot, SuggestionState};
use serde_json::{Value, json};
use std::process::ExitCode;

const PREFIX: &str = "Nous préparons 🖋";
const SUFFIX: &str = "|existing suffix";
const OFFER: &str = " café e\u{301} 👩‍💻 demain";

fn snapshot() -> Snapshot {
    Snapshot {
        app_id: "synthetic.writer".into(),
        field_id: "owned-body".into(),
        text: format!("{PREFIX}{SUFFIX}").into(),
        caret: PREFIX.chars().count(),
        domain: "owned.example".into(),
        window_id: "owned-window".into(),
        window_revision: "owned-document-1".into(),
        window_pid: 42,
        ..Snapshot::default()
    }
}

fn typed(before: &Snapshot, inserted: &str) -> Snapshot {
    let mut current = before.clone();
    current.text = format!("{}{inserted}{}", before.prefix(), before.suffix()).into();
    current.caret += inserted.chars().count();
    current
}

fn offered(before: &Snapshot) -> Result<SuggestionState> {
    let mut state = SuggestionState::new();
    ensure!(state.offer(state.generation, before.clone(), OFFER.into()));
    Ok(state)
}

fn pass(outcomes: &mut Vec<Value>, name: &'static str) {
    outcomes.push(json!({"scenario":name,"outcome":"passed"}));
}

fn exercise(outcomes: &mut Vec<Value>, stage: &mut &'static str) -> Result<()> {
    let before = snapshot();
    *stage = "compatible_unicode_typed_through";
    let mut state = offered(&before)?;
    let first_generation = state.generation;
    let current = typed(&before, " café");
    ensure!(state.advance(&current));
    ensure!(state.text == " e\u{301} 👩‍💻 demain");
    ensure!(state.snapshot.as_ref() == Some(&current));
    ensure!(state.snapshot.as_ref().unwrap().suffix() == SUFFIX);
    ensure!(state.generation == first_generation + 1);
    // Late work from the pre-typing generation cannot replace retained text.
    ensure!(!state.offer(first_generation, before.clone(), " stale".into()));
    let second = typed(&current, " e\u{301} 👩‍💻");
    ensure!(state.advance(&second));
    ensure!(state.text == " demain");
    ensure!(state.generation == first_generation + 2);
    pass(outcomes, *stage);

    *stage = "unchanged_observation_is_not_typed_through";
    let generation = state.generation;
    let text = state.text.clone();
    let saved = state.snapshot.clone();
    for _ in 0..4 {
        ensure!(!state.advance(&second));
        ensure!(state.generation == generation && state.text == text && state.snapshot == saved);
    }
    // This is state stability, not a fabricated count of runtime paint events.
    pass(outcomes, *stage);

    *stage = "exact_authority_and_policy_identity";
    let changes: [(&str, fn(&mut Snapshot)); 13] = [
        ("app", |s| s.app_id.push_str(".other")),
        ("field", |s| s.field_id.push_str("-other")),
        ("domain", |s| s.domain = "other.example".into()),
        ("window", |s| s.window_id.push_str("-other")),
        ("document", |s| s.window_revision.push_str("-other")),
        ("process", |s| s.window_pid += 1),
        ("selection", |s| s.selection_length = 1),
        ("protected", |s| s.protected = true),
        ("multiline", |s| s.multiline = !s.multiline),
        ("on_demand", |s| s.on_demand_only = !s.on_demand_only),
        ("replacement", |s| s.can_replace = !s.can_replace),
        ("invalid_caret", |s| s.caret = usize::MAX),
        ("invalid_selection", |s| s.selection_length = usize::MAX),
    ];
    for (_, change) in changes {
        let mut retained = offered(&before)?;
        let mut current = typed(&before, " café");
        change(&mut current);
        ensure!(!retained.advance(&current));
        // Runtime owns invalidation after an incompatible observation. Exercise
        // its public Core operation explicitly, not a fake native notification.
        let retired = retained.generation;
        retained.invalidate();
        ensure!(!retained.offer(retired, before.clone(), OFFER.into()));
        let mut acceptance = offered(&before)?;
        let mut changed = before.clone();
        change(&mut changed);
        ensure!(
            acceptance
                .accept(&changed, true, false, true, Some(OFFER))
                .is_none()
        );
        ensure!(acceptance.snapshot.is_none() && acceptance.text.is_empty());
    }
    pass(outcomes, *stage);

    *stage = "incompatible_edit_and_suffix_refusal";
    let mut incompatible = Vec::new();
    incompatible.push(typed(&before, " thé"));
    let mut changed_prefix = typed(&before, " café");
    changed_prefix.text = format!("Vous préparons 🖋 café{SUFFIX}").into();
    incompatible.push(changed_prefix);
    let mut changed_suffix = typed(&before, " café");
    changed_suffix.text = format!("{PREFIX} café|different suffix").into();
    incompatible.push(changed_suffix);
    let mut caret_back = before.clone();
    caret_back.caret -= 1;
    incompatible.push(caret_back);
    let mut canonically_equivalent = typed(&before, " cafe\u{301}");
    canonically_equivalent.caret = PREFIX.chars().count() + " cafe\u{301}".chars().count();
    incompatible.push(canonically_equivalent);
    for current in &incompatible {
        let mut state = offered(&before)?;
        ensure!(!state.advance(current));
        ensure!(
            state
                .accept(current, true, false, true, Some(OFFER))
                .is_none()
        );
    }
    pass(outcomes, *stage);

    *stage = "explicit_revocation_and_deterministic_late_result_races";
    // These names describe the revocations a runtime must send. Core has no OS
    // notification channel or IME state and makes no claim that one was received.
    for _reason in [
        "focus",
        "scroll",
        "destruction",
        "ime",
        "policy",
        "input_revision",
    ] {
        let mut state = offered(&before)?;
        let stale_generation = state.generation;
        state.invalidate();
        let current_generation = state.generation;
        ensure!(!state.offer(stale_generation, before.clone(), OFFER.into()));
        ensure!(
            state
                .accept(&before, true, false, true, Some(OFFER))
                .is_none()
        );
        ensure!(state.generation > current_generation);
        let fresh_generation = state.generation;
        ensure!(state.offer(fresh_generation, before.clone(), OFFER.into()));
        ensure!(!state.offer(stale_generation, before.clone(), OFFER.into()));
        ensure!(state.text == OFFER);
    }
    pass(outcomes, *stage);

    *stage = "visible_prefix_authority_and_exact_once_proposals";
    for visible in ["", " coffee", "café", " café x"] {
        let mut state = offered(&before)?;
        let generation = state.generation;
        ensure!(
            state
                .accept(&before, true, false, true, Some(visible))
                .is_none()
        );
        ensure!(state.generation == generation && state.text == OFFER);
    }
    let mut state = offered(&before)?;
    ensure!(
        state
            .accept(&before, false, false, true, Some(" ca"))
            .is_none()
    );
    let first = state.accept(&before, false, false, true, Some(" café"));
    ensure!(first.as_deref() == Some(" café"));
    let predicted = state.snapshot.clone().unwrap();
    ensure!(predicted.text.as_ref() == format!("{PREFIX} café{SUFFIX}"));
    ensure!(predicted.suffix() == SUFFIX);
    ensure!(predicted.caret_rect.is_none() && predicted.inline_render.is_none());
    ensure!(!predicted.native_inline && predicted.native_anchor.is_empty());
    let mut replay = state.clone();
    ensure!(
        replay
            .accept(&before, false, false, true, Some(" café"))
            .is_none()
    );
    ensure!(replay.snapshot.is_none());
    // Chained proposals use the predicted updated snapshot; an actual caller
    // still needs independent host commit/readback before adopting this state.
    let remainder = state.text.clone();
    let second = state.accept(&predicted, true, false, true, Some(&remainder));
    ensure!(second.as_deref() == Some(" e\u{301} 👩‍💻 demain"));
    let final_snapshot = state.snapshot.clone().unwrap();
    ensure!(final_snapshot.text.as_ref() == format!("{PREFIX}{OFFER}{SUFFIX}"));
    let final_generation = state.generation;
    ensure!(
        state
            .accept(&final_snapshot, true, false, true, None)
            .is_none()
    );
    ensure!(state.generation == final_generation);
    pass(outcomes, *stage);

    *stage = "full_acceptance_and_full_typed_through_exhaustion";
    let mut full = offered(&before)?;
    ensure!(
        full.accept(&before, true, false, true, Some(OFFER))
            .as_deref()
            == Some(OFFER)
    );
    let current = full.snapshot.clone().unwrap();
    ensure!(
        full.accept(&current, true, false, true, Some(OFFER))
            .is_none()
    );
    let mut typed_all = offered(&before)?;
    let all = typed(&before, OFFER);
    ensure!(typed_all.advance(&all));
    ensure!(typed_all.text.is_empty());
    ensure!(typed_all.accept(&all, true, false, true, None).is_none());
    pass(outcomes, *stage);
    Ok(())
}

fn main() -> ExitCode {
    let mut outcomes = Vec::new();
    let mut stage = "setup";
    let passed = exercise(&mut outcomes, &mut stage).is_ok();
    println!(
        "{}",
        json!({"smoke":"ghost_lifecycle","passed":passed,"scenarios":outcomes,
        "failed_stage":if passed {None} else {Some(stage)},
        "scope":"real_public_Core_state_transitions_and_proposals_only",
        "native_commit_or_readback_proven":false,"runtime_publication_deduplication_proven":false,
        "persisted_acceptance_provenance_proven":false,
        "limits":["explicit_Core_invalidation_is_not_native_notification_delivery",
            "Core_has_no_IME_or_input_revision_channel","prediction_is_not_host_mutation",
            "no_Engine_integration_or_native_publication_receipt_in_this_headless_Core_run"]})
    );
    if passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
