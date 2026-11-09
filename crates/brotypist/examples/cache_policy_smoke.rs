//! Diagnostic-only whole-memory replay experiment; no profiles or state files.
//! Matched retained-prefix computation and final replay require exact f32 bits.
//! Cross-schedule numerical/greedy decisions use independent fresh/fresh floors.
//! This is a new experiment, not a revision of the negative sequence-state receipt.
#[path = "cache_measurement/calibration.rs"]
#[allow(dead_code)]
mod calibration;
use anyhow::{Result, ensure};
use brotypist::inference_native::{
    DIAGNOSTIC_CHECKPOINT_BYTE_CAP, DIAGNOSTIC_CHECKPOINT_SLOTS, DiagnosticCheckpoint,
    FinishReason, NativeModel, NativeWork, SamplerOptions, Tokenizer,
};
use clap::Parser;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

#[derive(Parser)]
struct Arguments {
    #[arg(long)]
    model: PathBuf,
    #[arg(long, default_value = "auto", value_parser = ["auto", "cpu"])]
    acceleration: String,
    #[arg(long, default_value_t = DIAGNOSTIC_CHECKPOINT_BYTE_CAP)]
    checkpoint_byte_cap: usize,
}

fn micros(start: Instant) -> u64 {
    start.elapsed().as_micros().min(u64::MAX as u128) as u64
}

fn compare(actual: &[f32], expected: &[f32]) -> Result<Value> {
    let mut result = calibration::compare_logits(expected, actual)?;
    result["comparison_class"] = json!({"retained_prefix_computation":"matched",
        "final_decode_shape":"matched_source_derived_physical_chunks",
        "gate":"bit_exact_logits_only_no_unobserved_greedy_claim"});
    Ok(result)
}

const GREEDY_CAP: usize = 24;
const GREEDY_SEED: u32 = 7;
const GREEDY_GRAMMAR: &str = "root ::= [a-zA-Z .,!?\\n]+";

struct Observation {
    logits: Vec<f32>,
    text: String,
    finish: Vec<&'static str>,
    work: NativeWork,
}

fn observe(
    model: &mut NativeModel,
    tokens: &[i32],
    cancel: &AtomicBool,
    label: &str,
    outcomes: &mut Vec<Value>,
) -> Result<Observation> {
    let logits = record_logits(model, label, outcomes)?;
    let mut sampler = SamplerOptions::for_family(None, false);
    sampler.temperature = 0.0;
    let mut text = String::new();
    let mut finish = Vec::new();
    model.diagnostic_set_next_seed(GREEDY_SEED)?;
    let generation = model.generate(
        tokens,
        GREEDY_CAP,
        sampler,
        Some(GREEDY_GRAMMAR),
        &[],
        cancel,
        &mut |delta, reason| {
            text.push_str(delta);
            if let Some(reason) = reason {
                finish.push(match reason {
                    FinishReason::Stop => "stop",
                    FinishReason::Length => "length",
                });
            }
            Ok(true)
        },
    );
    let work = model.work();
    outcomes.push(json!({"scenario":"native_observation","label":label,
        "phase":"grammar_constrained_greedy","cap":GREEDY_CAP,"grammar":GREEDY_GRAMMAR,
        "temperature":0.0,"requested_seed":GREEDY_SEED,
        "actual_seed":model.diagnostic_last_sampler_seed(),"work":work,
        "raw_native_text":text,"finish":finish,
        "error":generation.as_ref().err().map(ToString::to_string)}));
    generation?;
    ensure!(
        finish.len() == 1 && work.sampled_tokens > 0 && work.sampled_tokens <= GREEDY_CAP as u64
    );
    ensure!(
        work.exact_reuses == 1
            && work.reused_tokens == tokens.len() as u64
            && work.prompt.submitted_tokens == 0
            && work.prompt.completed_tokens == 0
    );
    ensure!(
        work.generation.submitted_tokens == work.generation.completed_tokens
            && work.generation.submitted_batches == work.generation.completed_batches
            && work.generation.completed_batches == work.generation.completed_tokens
    );
    ensure!(model.diagnostic_last_sampler_seed() == Some(GREEDY_SEED));
    Ok(Observation {
        logits,
        text,
        finish,
        work,
    })
}

fn compare_observations(cold: &Observation, other: &Observation) -> Result<Value> {
    let mut result = calibration::compare(&cold.logits, &other.logits, &cold.text, &other.text)?;
    let counts_exact = cold.work.sampled_tokens == other.work.sampled_tokens
        && cold.work.generation.submitted_tokens == other.work.generation.submitted_tokens
        && cold.work.generation.completed_tokens == other.work.generation.completed_tokens
        && cold.work.generation.submitted_batches == other.work.generation.submitted_batches
        && cold.work.generation.completed_batches == other.work.generation.completed_batches;
    result["sampler_decode_count_disagreement"] = json!(u8::from(!counts_exact));
    result["finish_reason_disagreement"] = json!(u8::from(cold.finish != other.finish));
    result["matched_hard_gate"] = json!(
        result["bit_exact"] == true
            && result["greedy_text_exact"] == true
            && counts_exact
            && cold.finish == other.finish
    );
    Ok(result)
}

fn record_logits(model: &NativeModel, label: &str, outcomes: &mut Vec<Value>) -> Result<Vec<f32>> {
    let logits = model.diagnostic_logits()?;
    let mut digest = Sha256::new();
    for value in &logits {
        digest.update(value.to_bits().to_le_bytes());
    }
    let finite = !logits.is_empty() && logits.iter().all(|value| value.is_finite());
    outcomes.push(json!({"scenario":"logit_observation","label":label,
        "tokens":model.evaluated_tokens(),"work":model.work(),"logit_count":logits.len(),
        "sha256_le_f32_bits":format!("{:x}",digest.finalize()),"all_finite":finite,
        "top20_token_ids":if finite { Some(calibration::top(&logits)) } else { None }}));
    ensure!(finite, "Invalid native logits");
    Ok(logits)
}

fn clear(model: &mut NativeModel) -> Result<()> {
    model.diagnostic_clear_cache();
    let (_, minimum, maximum) = model.diagnostic_cache_state();
    ensure!(minimum == -1 && maximum == -1 && model.evaluated_tokens() == 0);
    ensure!(model.diagnostic_checkpoint_bytes() == 0 && model.diagnostic_logits().is_err());
    Ok(())
}

fn schedule(
    model: &mut NativeModel,
    tokens: &[i32],
    ends: &[usize],
    cancel: &AtomicBool,
) -> Result<NativeWork> {
    let mut work = NativeWork::default();
    for &end in ends {
        ensure!(model.evaluate(&tokens[..end], cancel)?);
        work += model.work();
    }
    Ok(work)
}

fn control(
    model: &mut NativeModel,
    tokens: &[i32],
    ends: &[usize],
    cancel: &AtomicBool,
) -> Result<Vec<f32>> {
    clear(model)?;
    schedule(model, tokens, ends, cancel)?;
    model.diagnostic_logits()
}

fn sample_once(model: &mut NativeModel, tokens: &[i32], cancel: &AtomicBool) -> Result<()> {
    model.generate(
        tokens,
        1,
        SamplerOptions::for_family(None, false),
        None,
        &[],
        cancel,
        &mut |_, _| Ok(false),
    )
}

fn seed_boundaries(model: &mut NativeModel, tokens: &[i32], cancel: &AtomicBool) -> Result<Value> {
    for seed in [0, u32::MAX - 1] {
        model.diagnostic_set_next_seed(seed)?;
        sample_once(model, tokens, cancel)?;
        ensure!(model.diagnostic_last_sampler_seed() == Some(seed));
    }
    model.diagnostic_set_next_seed(0)?;
    sample_once(model, tokens, cancel)?;
    sample_once(model, tokens, cancel)?;
    let ordinary = model.diagnostic_sampler_seed();
    let ordinary_known = model.diagnostic_last_sampler_seed();
    for failure in [
        "sentinel",
        "cancelled",
        "invalid_request",
        "invalid_grammar",
    ] {
        model.diagnostic_set_next_seed(0)?;
        sample_once(model, tokens, cancel)?;
        model.diagnostic_set_next_seed(u32::MAX - 1)?;
        match failure {
            "sentinel" => ensure!(model.diagnostic_set_next_seed(u32::MAX).is_err()),
            "cancelled" => {
                cancel.store(true, Ordering::Release);
                sample_once(model, tokens, cancel)?;
                ensure!(model.diagnostic_last_sampler_seed().is_none());
                cancel.store(false, Ordering::Release);
            }
            _ => {
                let (maximum, grammar) = if failure == "invalid_request" {
                    (0, None)
                } else {
                    (1, Some("\0"))
                };
                ensure!(
                    model
                        .generate(
                            tokens,
                            maximum,
                            SamplerOptions::for_family(None, false),
                            grammar,
                            &[],
                            cancel,
                            &mut |_, _| Ok(false)
                        )
                        .is_err()
                );
                ensure!(model.diagnostic_last_sampler_seed().is_none());
            }
        }
        sample_once(model, tokens, cancel)?;
        ensure!(
            model.diagnostic_sampler_seed() == ordinary
                && model.diagnostic_last_sampler_seed() == ordinary_known,
            "Rejected/cancelled override leaked into a later generation."
        );
    }
    Ok(json!({"scenario":"exact_seed_boundaries","passed":true,
        "accepted_seeds":[0,u32::MAX - 1],"random_sentinel_rejected":true,
        "pending_override_retired_after":["rejection","cancellation","invalid_request","invalid_grammar"],
        "ordinary_recurrence_unchanged":true}))
}

// Whole-memory serialization must preserve the history occupied BEFORE saving,
// including full-SWA cells outside the attention window. Compact eviction is
// irreversible. Live controls, not restored extrema or a copied trim formula,
// determine reusable history. Fresh append-only logits independently catch an
// equally wrong rewind in both live and restored paths.
fn long_checkpoints(
    model: &mut NativeModel,
    tokens: &[i32],
    cap: usize,
    cancel: &AtomicBool,
    outcomes: &mut Vec<Value>,
) -> Result<bool> {
    let (window, _, _) = model.diagnostic_cache_state();
    let mut cases = vec![
        (128, 127),
        (128, 128),
        (128, 129),
        (256, 128),
        (256, 256),
        (256, 257),
        (1024, 1000),
        (1024, 1023),
        (1024, 1024),
        (1024, 1025),
        (tokens.len() - 64, 128),
        (tokens.len() - 64, tokens.len() - 64),
        (tokens.len() - 64, tokens.len()),
    ];
    if window > 1 && (window as usize + 2) < tokens.len() {
        let boundary = window as usize;
        cases.extend([
            (boundary - 1, boundary - 1),
            (boundary, boundary - 1),
            (boundary, boundary),
            (boundary, boundary + 1),
            (boundary + 1, boundary),
            (boundary + 1, boundary + 1),
            (boundary + 1, boundary + 2),
        ]);
    }
    let mut passed = true;
    for (saved_len, target) in cases {
        outcomes.push(json!({"scenario":"checkpoint_case_start",
            "saved_tokens":saved_len,"target_tokens":target}));
        let retained = if target <= saved_len {
            target - 1
        } else {
            saved_len
        };
        let save_schedule = if retained < saved_len {
            vec![retained, saved_len]
        } else {
            vec![saved_len]
        };

        // Unrestored control begins with the identical decode partition/history.
        clear(model)?;
        let live_save_work = schedule(model, tokens, &save_schedule, cancel)?;
        let original_history = model.diagnostic_cache_state();
        ensure!(model.evaluate(&tokens[..target], cancel)?);
        let direct_live_work = model.work();
        let direct_live = observe(
            model,
            &tokens[..target],
            cancel,
            "fresh_direct_live",
            outcomes,
        )?;
        let (live_work, live, replay_setup_work) = if target == saved_len {
            // The live exact hit has valid logits and does not decode. A second
            // live control changes ONLY the final saved token, forcing genuine
            // replay of the original final token. Its causal prefix, occupied
            // positions and decode partition match; no validity flag is forged.
            let mut changed = tokens[..saved_len].to_vec();
            changed[saved_len - 1] = *tokens
                .iter()
                .find(|&&token| token != tokens[saved_len - 1])
                .ok_or_else(|| anyhow::anyhow!("Fixture needs two distinct tokens."))?;
            clear(model)?;
            let setup_work = schedule(model, &changed, &save_schedule, cancel)?;
            ensure!(model.diagnostic_cache_state() == original_history);
            ensure!(model.evaluate(&tokens[..target], cancel)?);
            (
                model.work(),
                observe(
                    model,
                    &tokens[..target],
                    cancel,
                    "fresh_live_replay",
                    outcomes,
                )?,
                setup_work,
            )
        } else {
            clear(model)?;
            let setup_work = schedule(model, tokens, &save_schedule, cancel)?;
            ensure!(model.diagnostic_cache_state() == original_history);
            ensure!(model.evaluate(&tokens[..target], cancel)?);
            (
                model.work(),
                observe(
                    model,
                    &tokens[..target],
                    cancel,
                    "fresh_live_replay",
                    outcomes,
                )?,
                setup_work,
            )
        };
        let expected_reuse = usize::try_from(live_work.reused_tokens)?;
        ensure!(expected_reuse < target);
        // This schedule comes from the independently executed live control.
        // It never uses a restored observation to excuse missing history.
        let reference_schedule = if expected_reuse == 0 {
            vec![target]
        } else {
            vec![expected_reuse, target]
        };
        clear(model)?;
        let reference_work = schedule(model, tokens, &reference_schedule, cancel)?;
        let reference_final_work = model.work();
        let reference = observe(
            model,
            &tokens[..target],
            cancel,
            "fresh_append_only",
            outcomes,
        )?;
        let live_comparison = compare_observations(&reference, &live)?;
        let direct_floor = compare_observations(&direct_live, &live)?;
        let reference_prefix_matches = expected_reuse == 0 || save_schedule[0] == expected_reuse;
        let live_reference_schedule_matches =
            reference_prefix_matches && same_decode_schedule(live_work, reference_final_work);
        let live_reference_exact_passed =
            !live_reference_schedule_matches || live_comparison["matched_hard_gate"] == true;
        passed &= live_reference_exact_passed;
        outcomes.push(json!({"scenario":"fresh_schedule_calibration",
            "saved_tokens":saved_len,"target_tokens":target,
            "reference_schedule":reference_schedule,"save_schedule":save_schedule,
            "retained_prefix_computation_matches_reference":reference_prefix_matches,
            "exact_safety_passed":live_reference_exact_passed,
            "reference_gate":if live_reference_schedule_matches {
                "bit_exact_and_actual_greedy_exact"
            } else { "independent_fresh_reference_vs_live_replay_floor" },
            "direct_final_decode_shape":decode_shape(direct_live_work.reused_tokens, direct_live_work.prompt.completed_tokens),
            "live_replay_final_decode_shape":decode_shape(live_work.reused_tokens, live_work.prompt.completed_tokens),
            "reference_final_decode_shape":decode_shape(reference_final_work.reused_tokens, reference_final_work.prompt.completed_tokens),
            "shape_source":"source_chunks_64_microbatch_32_and_actual_work_not_runtime_kernel_attestation",
            "reference_vs_live_replay_floor":live_comparison,
            "direct_live_vs_live_replay_floor":direct_floor,
            "floor_source":"independently_cleared_controls_before_any_restore",
            "retained_prefix_computation":"live_and_restored_use_identical_save_schedule",
            "final_decode_shape":if target == saved_len {
                "direct_live_no_decode_vs_live_and_restored_single_token_replay"
            } else { "direct_live_and_restored_use_same_observed_replay" }}));

        clear(model)?;
        let save_work = schedule(model, tokens, &save_schedule, cancel)?;
        let before_save_history = model.diagnostic_cache_state();
        let native_bytes = model.diagnostic_checkpoint_size();
        let started = Instant::now();
        let Some(saved) = model.diagnostic_save_checkpoint(0, cap)? else {
            passed = false;
            outcomes.push(json!({"scenario":"whole_memory_checkpoint",
                "saved_tokens":saved_len,"target_tokens":target,"passed":false,
                "reason":"checkpoint_cap_refused","required_native_bytes":native_bytes,
                "memory_refusal_status":"intended_cap_refusal_required_replay_not_exercised",
                "exact_safety_status":"incomplete","numerical_status":"incomplete","decision_status":"incomplete",
                "live_work":live_work,"live_reference_comparison":live_comparison}));
            continue;
        };
        let save_us = micros(started);
        let stored_bytes = model.diagnostic_checkpoint_bytes();
        outcomes.push(
            json!({"scenario":"checkpoint_saved","saved_tokens":saved_len,"target_tokens":target,
            "history":before_save_history,"work":save_work,"save_us":save_us,
            "native_whole_memory_bytes":native_bytes,"stored_total_bytes":stored_bytes}),
        );
        let extended_len = (saved_len + 64).min(tokens.len());
        ensure!(model.evaluate(&tokens[..extended_len], cancel)?);
        let extension_work = model.work();
        outcomes.push(
            json!({"scenario":"checkpoint_extended","saved_tokens":saved_len,"target_tokens":target,
            "work":extension_work,"history":model.diagnostic_cache_state()}),
        );
        let seed_before = model.diagnostic_sampler_seed();
        let started = Instant::now();
        ensure!(model.diagnostic_restore_checkpoint(&saved, &tokens[..saved_len])?);
        let restore_us = micros(started);
        let restored_history = model.diagnostic_cache_state();
        outcomes.push(json!({"scenario":"checkpoint_restored","saved_tokens":saved_len,"target_tokens":target,
            "history":restored_history,"restore_us":restore_us,"work":model.work(),
            "logits_invalid_observed":model.diagnostic_logits().is_err(),
            "sampler_seed_before":seed_before,"sampler_seed_after":model.diagnostic_sampler_seed()}));
        ensure!(model.diagnostic_logits().is_err());
        ensure!(model.work() == extension_work);
        ensure!(model.diagnostic_sampler_seed() == seed_before);
        ensure!(model.evaluate(&tokens[..target], cancel)?);
        let work = model.work();
        let actual = observe(
            model,
            &tokens[..target],
            cancel,
            "restored_replay",
            outcomes,
        )?;
        let live_replay_comparison = compare_observations(&live, &actual)?;
        let direct_live_comparison = compare_observations(&direct_live, &actual)?;
        let reference_comparison = compare_observations(&reference, &actual)?;
        // A matched schedule has no numerical allowance: a shared live/restored
        // disagreement with the independent append-only reference is a failure,
        // not a fresh/fresh floor. Actual replay work must match as well as the
        // independently computed retained prefix.
        let reference_final_shape_matches = same_decode_schedule(work, reference_final_work);
        let reference_schedule_matches = reference_prefix_matches
            && work.reused_tokens == live_work.reused_tokens
            && reference_final_shape_matches;
        let reference_excess = if reference_schedule_matches {
            calibration::METRICS
                .into_iter()
                .filter(|metric| reference_comparison[*metric].as_f64().unwrap() > 0.0)
                .collect()
        } else {
            calibration::excess(&reference_comparison, &live_comparison)
        };
        let direct_excess = calibration::excess(&direct_live_comparison, &direct_floor);
        let numerical_passed = reference_excess
            .iter()
            .chain(&direct_excess)
            .all(|metric| !calibration::METRICS[..4].contains(metric));
        let decision_passed = reference_excess
            .iter()
            .chain(&direct_excess)
            .all(|metric| !calibration::METRICS[4..].contains(metric));
        let exact_safety_passed = original_history == before_save_history
            && restored_history == before_save_history
            && work.reused_tokens == live_work.reused_tokens
            && work.prompt.submitted_tokens == live_work.prompt.submitted_tokens
            && work.prompt.completed_tokens == live_work.prompt.completed_tokens
            && work.prompt.submitted_batches == live_work.prompt.submitted_batches
            && work.prompt.completed_batches == live_work.prompt.completed_batches
            && work.exact_reuses == 0
            && work.trim_failures == live_work.trim_failures
            && work.cache_resets == live_work.cache_resets
            && live_replay_comparison["matched_hard_gate"] == true
            && live_reference_exact_passed
            && (!reference_schedule_matches || reference_comparison["matched_hard_gate"] == true)
            && (target == saved_len || direct_live_comparison["matched_hard_gate"] == true);
        let success = exact_safety_passed && numerical_passed && decision_passed;
        passed &= success;
        outcomes.push(json!({"scenario":"whole_memory_checkpoint","passed":success,
            "exact_safety_passed":exact_safety_passed,
            "calibrated_numerical_passed":numerical_passed,"calibrated_decisions_passed":decision_passed,
            "memory_refusal_status":"checkpoint_admitted_and_replayed",
            "reference_floor_excess":reference_excess,"direct_floor_excess":direct_excess,
            "comparison_classes":{
                "restored_live_replay":{"retained_prefix_computation":"matched",
                    "final_decode_shape":"matched","gate":"bit_exact_and_actual_greedy_exact"},
                "restored_direct_live":{"retained_prefix_computation":"matched",
                    "final_decode_shape":if target == saved_len { "no_decode_vs_replay" } else { "matched" },
                    "gate":if target == saved_len { "independent_direct_live_vs_live_replay_floor" }
                        else { "bit_exact_and_actual_greedy_exact_plus_reported_fresh_floor" }},
                "restored_reference":{"retained_prefix_computation_matches":reference_prefix_matches
                        && work.reused_tokens == live_work.reused_tokens,
                    "final_decode_shape_matches":reference_final_shape_matches,
                    "gate":if reference_schedule_matches { "bit_exact_and_actual_greedy_exact" }
                        else { "independent_fresh_reference_vs_live_replay_floor" },
                    "metric_baseline":if reference_schedule_matches { "exact_zero_not_measured_floor" }
                        else { "independently_cleared_reference_vs_live_replay" }}},
            "saved_tokens":saved_len,"target_tokens":target,"attention_window":window,
            "original_live_history":original_history,"before_save_history":before_save_history,
            "restored_history":restored_history,"expected_reuse_from_live_control":expected_reuse,
            "extended_before_restore_tokens":extended_len,"save_schedule":save_schedule,
            "cleared_reference_schedule":reference_schedule,
            "native_whole_memory_bytes":native_bytes,"stored_total_bytes":stored_bytes,
            "save_us":save_us,"restore_us":restore_us,"save_work":save_work,
            "live_save_work":live_save_work,"extension_work":extension_work,
            "direct_live_work":direct_live_work,"live_replay_work":live_work,
            "live_replay_setup_work":replay_setup_work,
            "cleared_reference_total_work":reference_work,"restored_work":work,
            "cleared_reference_final_work":reference_final_work,
            "logits_invalid_after_restore":true,"restore_preserves_seed_and_work":true,
            "exact_live_replay_control":if target == saved_len {
                "alternate_final_token_then_genuine_original_token_evaluation"
            } else { "independently_cleared_identical_save_schedule_then_live_replay" },
            "direct_live_reference_floor":direct_floor,
            "live_reference_comparison":live_comparison,
            "restored_live_replay_comparison":live_replay_comparison,
            "restored_direct_live_comparison":direct_live_comparison,
            "restored_reference_comparison":reference_comparison}));
    }
    Ok(passed)
}

// Elapsed time is not schedule identity. Native evaluation uses fixed physical
// chunks, so positions plus submitted/completed token and batch counts identify
// the final decode partition without claiming runtime kernel attestation.
fn same_decode_schedule(left: NativeWork, right: NativeWork) -> bool {
    left.reused_tokens == right.reused_tokens
        && left.prompt.submitted_tokens == right.prompt.submitted_tokens
        && left.prompt.completed_tokens == right.prompt.completed_tokens
        && left.prompt.submitted_batches == right.prompt.submitted_batches
        && left.prompt.completed_batches == right.prompt.completed_batches
}

fn decode_shape(retained: u64, decoded: u64) -> Vec<Value> {
    (0..decoded)
        .step_by(64)
        .map(|offset| {
            json!({
                "position":retained + offset,"tokens":(decoded - offset).min(64)
            })
        })
        .collect()
}

fn exercise(
    arguments: &Arguments,
    full_swa: bool,
    flash: bool,
    foreign: Option<&DiagnosticCheckpoint>,
    stage: &mut &'static str,
    outcomes: &mut Vec<Value>,
) -> Result<(Value, Option<DiagnosticCheckpoint>)> {
    *stage = "policy_open";
    let opened = Instant::now();
    let mut model = NativeModel::diagnostic_open_policy(
        &arguments.model,
        &arguments.acceleration,
        full_swa,
        flash,
    )?;
    let open_us = micros(opened);
    let text = "Synthetic instruction: preserve exact Unicode. Context: owned experiment. Draft: we discuss the next project review and measurements. Nós discutimos a próxima reunião. 😀 e\u{301} ".repeat(1200);
    let mut tokens = model.tokenize(&text, true, false)?;
    let count = model.context_size().saturating_sub(64);
    ensure!(count > 1025 && tokens.len() >= count);
    tokens.truncate(count);
    let cancel = AtomicBool::new(false);
    let mut passed = true;

    *stage = "exact_seed_boundaries";
    outcomes.push(seed_boundaries(&mut model, &tokens[..64], &cancel)?);
    clear(&mut model)?;

    *stage = "long_saved_state_boundaries";
    passed &= long_checkpoints(
        &mut model,
        &tokens,
        arguments.checkpoint_byte_cap,
        &cancel,
        outcomes,
    )?;
    clear(&mut model)?;

    *stage = "bounded_bias_validation";
    ensure!(
        model
            .diagnostic_set_logit_bias(&[(tokens[0], f32::NAN)])
            .is_err()
    );
    ensure!(
        model
            .diagnostic_set_logit_bias(&[(tokens[0], f32::INFINITY)])
            .is_err()
    );
    ensure!(
        model
            .diagnostic_set_logit_bias(&[(tokens[0], 2.01)])
            .is_err()
    );
    ensure!(model.diagnostic_set_logit_bias(&[(i32::MAX, 0.5)]).is_err());
    ensure!(model.diagnostic_set_logit_bias(&[(-1, 0.5)]).is_err());
    ensure!(
        model
            .diagnostic_set_logit_bias(&[(tokens[0], 0.5); 65])
            .is_err()
    );
    ensure!(
        model
            .diagnostic_set_logit_bias(&[(tokens[0], 0.5); 2])
            .is_err()
    );
    model.diagnostic_set_logit_bias(&[(tokens[0], 0.5)])?;
    model.diagnostic_set_logit_bias(&[])?;
    outcomes.push(
        json!({"scenario":"bounded_bias_validation","refused_cases":7,"clear_accepted":true,
        "sampling_effect_proof":"separate_learning_bias_smoke"}),
    );

    *stage = "bounded_checkpoint_admission";
    schedule(&mut model, &tokens, &[127, 128], &cancel)?;
    let required_state_bytes = model.diagnostic_checkpoint_size();
    ensure!(required_state_bytes > 0);
    ensure!(model.diagnostic_save_checkpoint(0, 0)?.is_none());
    ensure!(model.diagnostic_checkpoint_bytes() == 0);
    let required_total_bytes = required_state_bytes
        .checked_add(128 * std::mem::size_of::<i32>())
        .ok_or_else(|| anyhow::anyhow!("Checkpoint byte accounting overflow."))?;
    let insufficient_cap = required_total_bytes
        .saturating_sub(1)
        .min(arguments.checkpoint_byte_cap);
    ensure!(
        model
            .diagnostic_save_checkpoint(0, insufficient_cap)?
            .is_none()
    );
    ensure!(model.diagnostic_checkpoint_bytes() == 0);
    ensure!(
        model
            .diagnostic_save_checkpoint(DIAGNOSTIC_CHECKPOINT_SLOTS, arguments.checkpoint_byte_cap)
            .is_err()
    );
    ensure!(
        model
            .diagnostic_save_checkpoint(0, DIAGNOSTIC_CHECKPOINT_BYTE_CAP + 1)
            .is_err()
    );
    let started = Instant::now();
    let short = model.diagnostic_save_checkpoint(0, arguments.checkpoint_byte_cap)?;
    let save_short_us = micros(started);
    let short_bytes = model.diagnostic_checkpoint_bytes();
    outcomes.push(json!({"scenario":"bounded_checkpoint_admission", "required_native_bytes":required_state_bytes,
        "required_payload_and_token_bytes":required_total_bytes,"insufficient_cap_refused":insufficient_cap,
        "stored_total_bytes":short_bytes,"save_us":save_short_us,"zero_budget_refused":true,
        "reason":if short.is_none() { Some("byte_cap") } else { None },
        "saved":short.is_some()}));
    passed &= short.is_some();

    if let Some(short) = short {
        *stage = "backspace_and_caret_return";
        schedule(&mut model, &tokens, &[255, 256], &cancel)?;
        let started = Instant::now();
        let long = model.diagnostic_save_checkpoint(1, arguments.checkpoint_byte_cap)?;
        let save_long_us = micros(started);
        let total_bytes = model.diagnostic_checkpoint_bytes();
        ensure!(total_bytes <= arguments.checkpoint_byte_cap);
        // Rejected replacement must not evict either existing checkpoint.
        ensure!(model.diagnostic_save_checkpoint(1, 1)?.is_none());
        ensure!(model.diagnostic_checkpoint_bytes() == total_bytes);
        schedule(&mut model, &tokens, &[tokens.len()], &cancel)?;
        let (_, minimum, maximum) = model.diagnostic_cache_state();
        let started = Instant::now();
        ensure!(model.diagnostic_restore_checkpoint(&short, &tokens[..128])?);
        let restore_short_us = micros(started);
        ensure!(model.diagnostic_logits().is_err());
        ensure!(model.evaluate(&tokens[..128], &cancel)?);
        let rewind_work = model.work();
        let short_logits = record_logits(&model, "backspace", outcomes)?;
        let mut long_subject = None;
        if let Some(long) = long {
            let started = Instant::now();
            ensure!(model.diagnostic_restore_checkpoint(&long, &tokens[..256])?);
            let restore_us = micros(started);
            ensure!(model.diagnostic_logits().is_err());
            ensure!(model.evaluate(&tokens[..256], &cancel)?);
            long_subject = Some((
                record_logits(&model, "caret_return", outcomes)?,
                model.work(),
                restore_us,
            ));
        }
        let short_comparison = compare(
            &short_logits,
            &control(&mut model, &tokens, &[127, 128], &cancel)?,
        )?;
        let short_passed = rewind_work.reused_tokens == 127
            && rewind_work.prompt.completed_tokens == 1
            && rewind_work.exact_reuses == 0
            && short_comparison["bit_exact"] == true;
        passed &= short_passed;
        outcomes.push(
            json!({"scenario":"backspace", "passed":short_passed,"save_us":save_short_us,
            "restore_us":restore_short_us,"stored_total_bytes":total_bytes,
            "extended_cache_minimum":minimum,"extended_cache_maximum":maximum,
            "work":rewind_work,"matched_schedule_comparison":short_comparison}),
        );
        if let Some((logits, work, restore_us)) = long_subject {
            let comparison = compare(
                &logits,
                &control(&mut model, &tokens, &[127, 128, 255, 256], &cancel)?,
            )?;
            let success = work.reused_tokens == 255
                && work.prompt.completed_tokens == 1
                && work.exact_reuses == 0
                && comparison["bit_exact"] == true;
            passed &= success;
            outcomes.push(
                json!({"scenario":"caret_return","passed":success,"save_us":save_long_us,
                "restore_us":restore_us,"work":work,"matched_schedule_comparison":comparison}),
            );
        } else {
            passed = false;
            outcomes.push(json!({"scenario":"caret_return","passed":false,"reason":"total_byte_cap_refused_second_state"}));
        }
    } else {
        outcomes.push(
            json!({"scenario":"backspace_and_caret_return","passed":false,
            "reason":"byte_cap","status":"required_saved_state_unavailable_not_refusal_test_pass"}),
        );
    }

    *stage = "exact_identity_and_retirement";
    for (name, changed) in [
        (
            "changed_context",
            "Synthetic instruction: preserve exact Unicode. Context: different evidence. Draft: review.",
        ),
        (
            "changed_instructions",
            "Synthetic instruction: use another style. Context: owned experiment. Draft: review.",
        ),
    ] {
        clear(&mut model)?;
        schedule(&mut model, &tokens, &[127, 128], &cancel)?;
        if let Some(saved) = model.diagnostic_save_checkpoint(0, arguments.checkpoint_byte_cap)? {
            let changed = model.tokenize(changed, true, false)?;
            ensure!(!model.diagnostic_restore_checkpoint(&saved, &changed)?);
            ensure!(model.evaluated_tokens() == 0 && model.diagnostic_logits().is_err());
            ensure!(model.diagnostic_checkpoint_bytes() == 0);
            outcomes.push(json!({"scenario":name,"mismatch_refused":true,"fully_reset":true}));
        } else {
            passed = false;
            outcomes.push(json!({"scenario":name,"unavailable":"byte_cap"}));
        }
    }
    clear(&mut model)?;
    schedule(&mut model, &tokens, &[127, 128], &cancel)?;
    if let Some(stale) = model.diagnostic_save_checkpoint(0, arguments.checkpoint_byte_cap)? {
        let current = model.diagnostic_save_checkpoint(0, arguments.checkpoint_byte_cap)?;
        ensure!(current.is_some());
        ensure!(!model.diagnostic_restore_checkpoint(&stale, &tokens[..128])?);
        ensure!(model.evaluated_tokens() == 0 && model.diagnostic_checkpoint_bytes() == 0);
        outcomes.push(json!({"scenario":"replaced_slot_handle","mismatch_refused":true}));
    } else {
        passed = false;
        outcomes
            .push(json!({"scenario":"replaced_slot_handle","passed":false,"reason":"byte_cap"}));
    }

    *stage = "checkpoint_corruption_clear_and_seed_retirement";
    for retirement in ["corrupt_payload", "explicit_clear", "seed_and_work"] {
        clear(&mut model)?;
        schedule(&mut model, &tokens, &[127, 128], &cancel)?;
        model.diagnostic_set_next_seed(7)?;
        sample_once(&mut model, &tokens[..128], &cancel)?;
        let Some(saved) = model.diagnostic_save_checkpoint(0, arguments.checkpoint_byte_cap)?
        else {
            passed = false;
            outcomes.push(json!({"scenario":retirement,"passed":false,"reason":"byte_cap"}));
            continue;
        };
        model.diagnostic_set_next_seed(19)?;
        sample_once(&mut model, &tokens[..128], &cancel)?;
        let work_before = model.work();
        ensure!(model.diagnostic_sampler_seed() == 19);
        match retirement {
            "corrupt_payload" => {
                model.diagnostic_corrupt_checkpoint(&saved)?;
                ensure!(!model.diagnostic_restore_checkpoint(&saved, &tokens[..128])?);
            }
            "explicit_clear" => {
                clear(&mut model)?;
                ensure!(!model.diagnostic_restore_checkpoint(&saved, &tokens[..128])?);
            }
            _ => {
                ensure!(model.diagnostic_restore_checkpoint(&saved, &tokens[..128])?);
                ensure!(model.work() == work_before);
                ensure!(model.evaluated_tokens() == 128);
            }
        }
        ensure!(model.diagnostic_sampler_seed() == 19);
        ensure!(model.diagnostic_last_sampler_seed() == Some(19));
        ensure!(model.diagnostic_logits().is_err());
        if retirement != "seed_and_work" {
            let (_, minimum, maximum) = model.diagnostic_cache_state();
            ensure!(minimum == -1 && maximum == -1 && model.evaluated_tokens() == 0);
            ensure!(model.diagnostic_checkpoint_bytes() == 0);
            ensure!(model.work().prompt == work_before.prompt);
            ensure!(model.work().sampled_tokens == work_before.sampled_tokens);
        }
        outcomes.push(json!({"scenario":retirement,"passed":true,
            "logits_invalid":true,"sampler_seed_not_rolled_back":true,
            "work_not_rolled_back":true,"stored_total_bytes":model.diagnostic_checkpoint_bytes()}));
    }

    *stage = "static_prefix_prewarm";
    clear(&mut model)?;
    let started = Instant::now();
    ensure!(model.evaluate(&tokens[..64], &cancel)?);
    let prewarm_us = micros(started);
    let prewarm_work = model.work();
    let prewarm_drain_us = model.diagnostic_drain();
    let started = Instant::now();
    ensure!(model.evaluate(&tokens[..128], &cancel)?);
    let completion_us = micros(started);
    let completion_work = model.work();
    let actual = record_logits(&model, "static_prefix_prewarm", outcomes)?;
    // A single cold evaluation decodes the same 64-token chunks without the
    // subject's intermediate evaluate/trim call.
    let comparison = compare(&actual, &control(&mut model, &tokens, &[128], &cancel)?)?;
    let success = completion_work.reused_tokens == 64
        && completion_work.prompt.completed_tokens == 64
        && comparison["bit_exact"] == true;
    passed &= success;
    outcomes.push(json!({"scenario":"static_prefix_prewarm","passed":success,
        "prewarm_us":prewarm_us,"prewarm_work":prewarm_work,"context_drain_us":prewarm_drain_us,
        "completion_us":completion_us,"completion_work":completion_work,"matched_schedule_comparison":comparison}));

    *stage = "cancel_completed_batch_and_drain";
    clear(&mut model)?;
    model.diagnostic_cancel_after_decode_batches(1)?;
    let started = Instant::now();
    ensure!(!model.evaluate(&tokens[..128], &cancel)?);
    let cancelled_us = micros(started);
    let cancelled_work = model.work();
    let cancellation_passed =
        cancel.load(Ordering::Acquire) && cancelled_work.prompt.completed_tokens == 64;
    let drain_us = model.diagnostic_drain();
    outcomes.push(json!({"scenario":"cancel_completed_batch_and_drain",
        "phase":"cancelled_and_drained","passed":cancellation_passed,
        "cancelled_us":cancelled_us,"cancelled_work":cancelled_work,"context_drain_us":drain_us,
        "system_gpu_idle_proven":false,"power_measurement":null}));
    ensure!(cancellation_passed);
    *stage = "cancel_completed_batch_recovery";
    cancel.store(false, Ordering::Release);
    ensure!(model.evaluate(&tokens[..128], &cancel)?);
    let recovery_work = model.work();
    let actual = record_logits(&model, "cancel_completed_batch_and_drain", outcomes)?;
    let comparison = compare(&actual, &control(&mut model, &tokens, &[64, 128], &cancel)?)?;
    let success = recovery_work.reused_tokens == 64
        && recovery_work.prompt.completed_tokens == 64
        && comparison["bit_exact"] == true;
    passed &= success;
    outcomes.push(
        json!({"scenario":"cancel_completed_batch_and_drain","phase":"recovery","passed":success,
        "recovery_work":recovery_work,"matched_schedule_comparison":comparison,
        "system_gpu_idle_proven":false,"power_measurement":null}),
    );

    *stage = "cancel_after_restore_trim";
    clear(&mut model)?;
    schedule(&mut model, &tokens, &[127, 128], &cancel)?;
    if let Some(saved) = model.diagnostic_save_checkpoint(0, arguments.checkpoint_byte_cap)? {
        ensure!(model.diagnostic_restore_checkpoint(&saved, &tokens[..128])?);
        model.diagnostic_cancel_after_trim();
        ensure!(!model.evaluate(&tokens[..128], &cancel)?);
        let cancelled_work = model.work();
        let logits_invalid = model.diagnostic_logits().is_err();
        let drain_us = model.diagnostic_drain();
        outcomes.push(json!({"scenario":"cancel_after_restore_trim",
            "phase":"cancelled_and_drained","passed":logits_invalid,
            "cancelled_work":cancelled_work,"context_drain_us":drain_us,
            "logits_invalid":logits_invalid,
            "system_gpu_idle_proven":false,"power_measurement":null}));
        ensure!(logits_invalid);
        *stage = "cancel_after_restore_trim_recovery";
        cancel.store(false, Ordering::Release);
        ensure!(model.evaluate(&tokens[..128], &cancel)?);
        let work = model.work();
        let actual = record_logits(&model, "cancel_after_restore_trim", outcomes)?;
        let comparison = compare(
            &actual,
            &control(&mut model, &tokens, &[127, 128], &cancel)?,
        )?;
        let success = work.exact_reuses == 0
            && work.prompt.completed_tokens == 1
            && comparison["bit_exact"] == true;
        passed &= success;
        outcomes.push(
            json!({"scenario":"cancel_after_restore_trim","phase":"recovery","passed":success,
            "work":work,"matched_schedule_comparison":comparison}),
        );
    } else {
        passed = false;
        outcomes.push(
            json!({"scenario":"cancel_after_restore_trim","passed":false,"reason":"byte_cap"}),
        );
    }

    *stage = "policy_memory_observation";
    clear(&mut model)?;
    ensure!(model.evaluate(&tokens, &cancel)?);
    let full_work = model.work();
    let (window, minimum, maximum) = model.diagnostic_cache_state();
    let state_bytes = model.diagnostic_checkpoint_size();
    outcomes.push(json!({"scenario":"policy_memory_observation","evaluated_tokens":tokens.len(),
        "attention_window":window,"retained_minimum":minimum,"retained_maximum":maximum,
        "native_whole_memory_serialized_bytes":state_bytes,"device_allocated_bytes":null,"work":full_work}));
    if !full_swa && minimum > 0 {
        *stage = "evicted_trim_full_reset";
        ensure!(model.evaluate(&tokens[..128], &cancel)?);
        let work = model.work();
        let actual = record_logits(&model, "evicted_trim_full_reset", outcomes)?;
        let comparison = compare(&actual, &control(&mut model, &tokens, &[128], &cancel)?)?;
        let success = work.trim_failures == 1
            && work.cache_resets == 1
            && work.reused_tokens == 0
            && work.prompt.completed_tokens == 128
            && comparison["bit_exact"] == true;
        passed &= success;
        outcomes.push(
            json!({"scenario":"evicted_trim_full_reset","passed":success,
            "work":work,"matched_schedule_comparison":comparison}),
        );
    } else {
        outcomes.push(json!({"scenario":"evicted_trim_full_reset",
            "status":if full_swa { "not_applicable_full_history" } else { "unsupported_no_observed_eviction" },
            "intended_refusal_exercised":false}));
        passed &= full_swa;
    }

    *stage = "model_owner_mismatch";
    clear(&mut model)?;
    schedule(&mut model, &tokens, &[127, 128], &cancel)?;
    let mut returned = model.diagnostic_save_checkpoint(0, arguments.checkpoint_byte_cap)?;
    if let Some(foreign) = foreign {
        ensure!(!model.diagnostic_restore_checkpoint(foreign, &tokens[..128])?);
        ensure!(model.evaluated_tokens() == 0 && model.diagnostic_checkpoint_bytes() == 0);
        outcomes.push(json!({"scenario":"model_owner_mismatch","mismatch_refused":true}));
        schedule(&mut model, &tokens, &[127, 128], &cancel)?;
        returned = model.diagnostic_save_checkpoint(0, arguments.checkpoint_byte_cap)?;
    } else if !(full_swa && !flash) {
        passed = false;
        outcomes.push(json!({"scenario":"model_owner_mismatch","passed":false,
            "status":"required_foreign_handle_unavailable_not_refusal_test_pass"}));
    } else {
        outcomes.push(json!({"scenario":"model_owner_mismatch",
            "status":"first_owner_exports_handle_for_next_policy"}));
    }
    if returned.is_none() {
        passed = false;
        outcomes.push(json!({"scenario":"owner_handle_export","passed":false,"reason":"byte_cap"}));
    }
    let final_drain_us = model.diagnostic_drain();
    let gpu_layers_requested = model.gpu_layers_requested();
    *stage = "owned_model_shutdown";
    // The returned capability deliberately outlives this owner. The next policy
    // must reject it; neither native pointers nor payloads escape through it.
    let shutdown_started = Instant::now();
    drop(model);
    let shutdown_us = micros(shutdown_started);
    outcomes.push(json!({"scenario":"owned_model_shutdown","passed":true,
        "shutdown_returned":true,"shutdown_us":shutdown_us,
        "payload_zeroization":"Zeroizing_drop_not_memory_inspected"}));
    Ok((
        json!({"passed":passed,"full_swa_requested":full_swa,"flash_attention_requested":flash,
        "acceptance_statuses":acceptance_statuses(outcomes, true),
        "open_us":open_us,"gpu_layers_requested":gpu_layers_requested,
        "final_context_drain_us":final_drain_us,"scenarios":outcomes}),
        returned,
    ))
}

fn acceptance_statuses(outcomes: &[Value], completed: bool) -> Value {
    let checkpoints: Vec<_> = outcomes
        .iter()
        .filter(|row| row["scenario"] == "whole_memory_checkpoint")
        .collect();
    let refused = outcomes
        .iter()
        .filter(|row| {
            row["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("cap"))
                || row["unavailable"] == "byte_cap"
        })
        .count();
    let unsupported = outcomes
        .iter()
        .any(|row| row["status"] == "unsupported_no_observed_eviction");
    let status = |key: &str| {
        if checkpoints.iter().any(|row| row[key] == false) {
            "failed"
        } else if !completed
            || checkpoints.is_empty()
            || checkpoints.iter().any(|row| row[key] != true)
        {
            "incomplete"
        } else {
            "passed"
        }
    };
    let exact_failed = outcomes.iter().any(|row| {
        row["exact_safety_passed"] == false
            || row["matched_schedule_comparison"]["bit_exact"] == false
            || (row["scenario"] != "whole_memory_checkpoint"
                && row["passed"] == false
                && row["reason"].is_null())
    });
    json!({
        "exact_safety":if exact_failed { "failed" }
            else if refused > 0 || unsupported { "incomplete" } else { status("exact_safety_passed") },
        "calibrated_numerical":status("calibrated_numerical_passed"),
        "calibrated_decisions":status("calibrated_decisions_passed"),
        "memory_refusal":{"status":if refused > 0 { "intended_cap_refusals_required_work_incomplete" }
            else if unsupported { "unsupported_eviction_not_a_refusal_pass" }
            else if completed { "completed" } else { "incomplete" }, "refused_cases":refused},
        "adoption":"not_adopted_diagnostic_only"
    })
}

fn main() -> Result<()> {
    let arguments = Arguments::parse();
    ensure!(arguments.checkpoint_byte_cap <= DIAGNOSTIC_CHECKPOINT_BYTE_CAP);
    let mut results = Vec::new();
    let mut previous = None;
    for (full, flash) in [(true, false), (false, false), (true, true), (false, true)] {
        let mut stage = "policy_open";
        // Preserve completed numerical observations even if a later safety
        // assertion fails; no failing comparison disappears behind an arm error.
        let mut outcomes = Vec::new();
        match exercise(
            &arguments,
            full,
            flash,
            previous.as_ref(),
            &mut stage,
            &mut outcomes,
        ) {
            Ok((report, checkpoint)) => {
                previous = checkpoint;
                results.push(report);
            }
            Err(error) => results.push(json!({"passed":false,"full_swa_requested":full,
                "flash_attention_requested":flash,"failed_stage":stage,"error":error.to_string(),
                "acceptance_statuses":acceptance_statuses(&outcomes, false),
                "outcome":"incomplete_no_adoption","scenarios":outcomes})),
        }
    }
    let passed = results.iter().all(|result| result["passed"] == true);
    println!(
        "{}",
        json!({"smoke":"cache_policy","experiment":"whole_memory_replay_calibrated_v2",
        "supersedes_negative_sequence_receipt":false,"passed":passed,"policies":results,
        "checkpoint_slots":DIAGNOSTIC_CHECKPOINT_SLOTS,"checkpoint_total_byte_cap":arguments.checkpoint_byte_cap,
        "state_api":"llama_state_get_size/get_data/set_data","state_sequence":-1,
        "serialized_logits":false,"state_persistence":false,"production_defaults_changed":false,
        "numeric_policy":"Exact matched f32 bits and native greedy; unmatched paired fresh/fresh dominance, no epsilon",
        "calibrated_metric_groups":{"numerical":calibration::METRICS[..4],
            "decisions":calibration::METRICS[4..]},
        "greedy_scope":"actual_native_sampler_cap24_synthetic_grammar_not_semantic_quality_or_UI_acceptance",
        "timing_scope":"raw_elapsed_observations_only_no_performance_claim",
        "adoption_status":"not_adopted_diagnostic_only",
        "batch_shape_source":"source_derived_physical_64_microbatch_32_not_runtime_kernel_attestation",
        "power_measurement":null,"system_gpu_idle_proven":false,
        "limitations":["one_model_backend_and_synthetic_fixture_only","no_cross_policy_numerical_equivalence_claim",
            "synchronize_drains_only_owned_context","no_physical_input_latency_or_energy_measurement",
            "serialized_state_bytes_are_not_device_allocation_bytes","flash_policy_is_requested_not_instrumented_kernel_proof"]})
    );
    ensure!(
        passed,
        "Diagnostic cache policy acceptance remains incomplete; see synthetic-fixture receipt."
    );
    Ok(())
}
