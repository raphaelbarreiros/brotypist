//! Real-model KV regression; synthetic tokens only, no profile, capture or credentials.
//! Compact SWA is diagnostic-only; production retains full-size SWA allocation.
//! Allocation is not checkpoint history: cache_policy_smoke separately covers
//! whole-memory snapshots, preserving occupied cells but not already evicted ones.
//!
//! Acceptance compares finite f32 bits exactly for matched retained-prefix and
//! final decode schedules. Cross-schedule decisions use fresh/fresh calibration.
//! Replay subjects first compute the retained prefix with the reference's batch
//! schedule, then extend a long history before removing it. References append
//! the final token without destructive trim. Recovery also invalidates logits.
//! Unmatched historical-prefix and cross-batch comparisons are diagnostic only:
//! a 128-token prefill and a 127+1 prefill are not the same numerical algorithm.
//! Passing this bounded cache regression does not establish batch invariance,
//! generation quality, or correctness for other models/backends.
#[path = "cache_measurement/calibration.rs"]
#[allow(dead_code)]
mod calibration;
use anyhow::{Result, ensure};
use brotypist::inference_native::{
    FinishReason, NativeModel, NativeWork, SamplerOptions, Tokenizer,
};
use clap::Parser;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fmt::Write,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Parser)]
struct Arguments {
    #[arg(long)]
    model: PathBuf,
    #[arg(long, default_value = "auto", value_parser = ["auto", "cpu"])]
    acceleration: String,
}

fn fixture(model: &NativeModel) -> Result<Vec<i32>> {
    let mut text = String::with_capacity(80000);
    for index in 0..1000 {
        write!(
            text,
            "Synthetic record {index}: the review includes measurements and delivery dates. "
        )?;
    }
    let mut tokens = model.tokenize(&text, true, false)?;
    ensure!(
        tokens.len() >= model.context_size() - 64,
        "Insufficient synthetic tokens."
    );
    tokens.truncate(model.context_size() - 64);
    Ok(tokens)
}

fn compare(actual: &[f32], expected: &[f32]) -> Result<Value> {
    calibration::compare_logits(expected, actual)
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

// A cold reference shares model weights, but no KV cells or reusable logits
// with the operation under test. Validate clearing rather than assuming it.
fn clear(model: &mut NativeModel) -> Result<()> {
    model.diagnostic_clear_cache();
    let (_, minimum, maximum) = model.diagnostic_cache_state();
    ensure!(model.evaluated_tokens() == 0 && minimum == -1 && maximum == -1);
    ensure!(model.diagnostic_logits().is_err());
    Ok(())
}

fn cold_evaluate(model: &mut NativeModel, tokens: &[i32], cancel: &AtomicBool) -> Result<()> {
    clear(model)?;
    ensure!(model.evaluate(tokens, cancel)?);
    let work = model.work();
    ensure!(
        work.cold_evaluations == 1
            && work.reused_tokens == 0
            && work.exact_reuses == 0
            && work.prefix_reuses == 0
            && work.prompt.submitted_tokens == tokens.len() as u64
            && work.prompt.completed_tokens == tokens.len() as u64
            && work.prompt.submitted_batches == tokens.len().div_ceil(64) as u64
            && work.prompt.completed_batches == tokens.len().div_ceil(64) as u64
    );
    Ok(())
}

fn cold_logits(model: &mut NativeModel, tokens: &[i32], cancel: &AtomicBool) -> Result<Vec<f32>> {
    cold_evaluate(model, tokens, cancel)?;
    model.diagnostic_logits()
}

fn require_replay(model: &NativeModel, retained: usize, decoded: usize) -> Result<()> {
    let work = model.work();
    ensure!(
        work.reused_tokens == retained as u64
            && work.exact_reuses == 0
            && work.prefix_reuses == 1
            && work.trim_failures == 0
            && work.cache_resets == 0
            && work.prompt.submitted_tokens == decoded as u64
            && work.prompt.completed_tokens == decoded as u64
            && work.prompt.submitted_batches == decoded.div_ceil(64) as u64
            && work.prompt.completed_batches == decoded.div_ceil(64) as u64
            && model.evaluated_tokens() == retained + decoded
    );
    let (_, minimum, maximum) = model.diagnostic_cache_state();
    ensure!(minimum == 0 && maximum == (retained + decoded) as i32 - 1);
    Ok(())
}

fn append_only_reference(
    model: &mut NativeModel,
    tokens: &[i32],
    cancel: &AtomicBool,
) -> Result<Vec<f32>> {
    if tokens.len() == 1 {
        return cold_logits(model, tokens, cancel);
    }
    // This path never removes a populated suffix. It cannot reproduce a
    // destructive-trim bug merely by exercising the same destructive operation.
    cold_evaluate(model, &tokens[..tokens.len() - 1], cancel)?;
    ensure!(model.evaluate(tokens, cancel)?);
    require_replay(model, tokens.len() - 1, 1)?;
    model.diagnostic_logits()
}

// Establish the retained KV with exactly the cold reference's computation
// schedule, then populate a suffix that only the subject will remove. Starting
// with the full history instead changes the retained prefix's last batch shape.
fn populate_replay_history(
    model: &mut NativeModel,
    tokens: &[i32],
    retained: usize,
    cancel: &AtomicBool,
) -> Result<()> {
    ensure!(retained < tokens.len());
    if retained == 0 {
        cold_evaluate(model, tokens, cancel)?;
    } else {
        cold_evaluate(model, &tokens[..retained], cancel)?;
        ensure!(model.evaluate(tokens, cancel)?);
        require_replay(model, retained, tokens.len() - retained)?;
    }
    let (_, minimum, maximum) = model.diagnostic_cache_state();
    ensure!(
        model.evaluated_tokens() == tokens.len()
            && minimum == 0
            && maximum == tokens.len() as i32 - 1
    );
    Ok(())
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

fn calibrated_case(
    name: &str,
    cold: &Observation,
    reference: &Observation,
    actual: &Observation,
    outcomes: &mut Vec<Value>,
) -> Result<bool> {
    let matched = compare_observations(reference, actual)?;
    let floor = compare_observations(cold, reference)?;
    let subject = compare_observations(cold, actual)?;
    let excess = calibration::excess(&subject, &floor);
    let numerical = excess
        .iter()
        .all(|metric| !calibration::METRICS[..4].contains(metric));
    let decisions = excess
        .iter()
        .all(|metric| !calibration::METRICS[4..].contains(metric));
    let exact = matched["matched_hard_gate"] == true;
    outcomes.push(json!({"scenario":name,"exact_safety_passed":exact,
        "calibrated_numerical_passed":numerical,"calibrated_decisions_passed":decisions,
        "matched":matched,"fresh_fresh_floor":floor,"cross_schedule_subject":subject,
        "excess_metrics":excess,
        "matched_retained_prefix_computation":"independent_cold_prefix_length_minus_one",
        "matched_final_decode_shape":"single_token_replay",
        "cross_schedule_retained_prefix_computation":"cold_batched_vs_split_before_final_token",
        "cross_schedule_final_decode_shape":"physical_final_batch_vs_single_token",
        "batch_shape_source":"source_derived_64_microbatch_32_not_runtime_kernel_attestation"}));
    Ok(exact && numerical && decisions)
}

fn production_cache(arguments: &Arguments, outcomes: &mut Vec<Value>) -> Result<Value> {
    let mut model = NativeModel::open(&arguments.model, &arguments.acceleration)?;
    let tokens = fixture(&model)?;
    let prefix = &tokens[..tokens.len() - 64];
    let short = &tokens[..128];
    let cancel = AtomicBool::new(false);
    ensure!(prefix.len().is_multiple_of(64));
    cold_evaluate(&mut model, prefix, &cancel)?;
    ensure!(model.evaluate(prefix, &cancel)?);
    let exact_work = model.work();
    let exact_logits = record_logits(&model, "exact_hit", outcomes)?;
    ensure!(
        exact_work.exact_reuses == 1
            && exact_work.reused_tokens == prefix.len() as u64
            && exact_work.cache_resets == 0
            && exact_work.prompt.submitted_tokens == 0
            && exact_work.prompt.completed_tokens == 0
            && exact_work.prompt.submitted_batches == 0
            && exact_work.prompt.completed_batches == 0
    );
    ensure!(model.evaluate(&tokens, &cancel)?);
    let extension_work = model.work();
    let extension = record_logits(&model, "extension", outcomes)?;
    let retained = model.diagnostic_cache_state();
    ensure!(retained.1 == 0 && retained.2 == tokens.len() as i32 - 1);
    require_replay(&model, prefix.len(), 64)?;
    ensure!(model.evaluate(short, &cancel)?);
    let rewind_work = model.work();
    let rewind = record_logits(&model, "unmatched_historical_rewind", outcomes)?;
    require_replay(&model, short.len() - 1, 1)?;
    let exact = compare(&exact_logits, &cold_logits(&mut model, prefix, &cancel)?)?;
    outcomes.push(json!({"scenario":"exact_hit","exact_safety_passed":exact["bit_exact"],
        "retained_prefix_computation":"matched_cold_batched","final_decode_shape":"no_decode_valid_cached_logits",
        "comparison":exact,"work":exact_work}));
    // The extension uses the same 64-token batching as this cold reference.
    let extension_comparison = compare(&extension, &cold_logits(&mut model, &tokens, &cancel)?)?;
    outcomes.push(json!({"scenario":"extension","exact_safety_passed":extension_comparison["bit_exact"],
        "retained_prefix_computation":"matched_64_token_physical_chunks","final_decode_shape":"matched_64_token_chunk",
        "comparison":extension_comparison,"work":extension_work}));
    let rewind_cold = cold_logits(&mut model, short, &cancel)?;
    let rewind_comparison = compare(&rewind, &rewind_cold)?;
    outcomes.push(json!({"scenario":"historical_rewind_diagnostic",
        "classification":"unmatched_retained_prefix_and_final_decode_not_cache_corruption_acceptance",
        "comparison":rewind_comparison,"work":rewind_work}));

    // Deterministically cancel after native trimming but before any new decode.
    model.diagnostic_cancel_after_trim();
    ensure!(!model.evaluate(&tokens[..64], &cancel)?);
    ensure!(model.evaluated_tokens() == 63 && model.work().prompt.submitted_tokens == 0);
    let cancelled_trim_work = model.work();
    require_replay(&model, 63, 0)?;
    ensure!(
        model.diagnostic_logits().is_err(),
        "Trimmed context exposed stale logits."
    );
    cancel.store(false, Ordering::Release);
    ensure!(model.evaluate(&tokens[..63], &cancel)?);
    let recovery_work = model.work();
    require_replay(&model, 62, 1)?;
    let recovered = record_logits(&model, "unmatched_historical_recovery", outcomes)?;
    let cancellation_cold = cold_logits(&mut model, &tokens[..63], &cancel)?;
    let cancellation_comparison = compare(&recovered, &cancellation_cold)?;
    outcomes.push(json!({"scenario":"historical_recovery_diagnostic",
        "classification":"unmatched_retained_prefix_and_final_decode_not_cache_corruption_acceptance",
        "comparison":cancellation_comparison,"work":recovery_work,"cancelled_trim_work":cancelled_trim_work}));
    // The earlier rewind/recovery retain KV computed in full historical
    // batches. Keep those measurements as diagnostics, not acceptance subjects.
    // Each acceptance subject starts afresh with the reference's retained-prefix
    // batches before extending to a long history and destructively rewinding.
    populate_replay_history(&mut model, &tokens, short.len() - 1, &cancel)?;
    ensure!(model.evaluate(short, &cancel)?);
    require_replay(&model, short.len() - 1, 1)?;
    let matched_rewind_work = model.work();
    let matched_rewind = observe(&mut model, short, &cancel, "matched_rewind", outcomes)?;
    append_only_reference(&mut model, short, &cancel)?;
    let rewind_reference = observe(
        &mut model,
        short,
        &cancel,
        "fresh_rewind_append_only",
        outcomes,
    )?;
    let rewind_matched = compare_observations(&rewind_reference, &matched_rewind)?;
    let rewind_unmatched_prefix = compare(&rewind, &rewind_reference.logits)?;
    outcomes.push(json!({"scenario":"rewind_exact_comparison",
        "exact_safety_passed":rewind_matched["matched_hard_gate"],"matched_comparison":rewind_matched,
        "unmatched_historical_prefix_diagnostic":rewind_unmatched_prefix}));
    cold_evaluate(&mut model, short, &cancel)?;
    let rewind_batched = observe(&mut model, short, &cancel, "fresh_rewind_batched", outcomes)?;
    let rewind_calibrated = calibrated_case(
        "rewind_calibrated",
        &rewind_batched,
        &rewind_reference,
        &matched_rewind,
        outcomes,
    )?;

    populate_replay_history(&mut model, &tokens, 62, &cancel)?;
    model.diagnostic_cancel_after_trim();
    ensure!(!model.evaluate(&tokens[..64], &cancel)?);
    require_replay(&model, 63, 0)?;
    let matched_cancelled_trim_work = model.work();
    ensure!(
        model.diagnostic_logits().is_err(),
        "Trimmed context exposed stale logits."
    );
    cancel.store(false, Ordering::Release);
    ensure!(model.evaluate(&tokens[..63], &cancel)?);
    require_replay(&model, 62, 1)?;
    let matched_recovery_work = model.work();
    let matched_recovered = observe(
        &mut model,
        &tokens[..63],
        &cancel,
        "matched_recovery",
        outcomes,
    )?;
    append_only_reference(&mut model, &tokens[..63], &cancel)?;
    let cancellation_reference = observe(
        &mut model,
        &tokens[..63],
        &cancel,
        "fresh_recovery_append_only",
        outcomes,
    )?;
    let cancellation_matched = compare_observations(&cancellation_reference, &matched_recovered)?;
    let cancellation_unmatched_prefix = compare(&recovered, &cancellation_reference.logits)?;
    outcomes.push(json!({"scenario":"recovery_exact_comparison",
        "exact_safety_passed":cancellation_matched["matched_hard_gate"],
        "matched_comparison":cancellation_matched,
        "unmatched_historical_prefix_diagnostic":cancellation_unmatched_prefix}));
    cold_evaluate(&mut model, &tokens[..63], &cancel)?;
    let recovery_batched = observe(
        &mut model,
        &tokens[..63],
        &cancel,
        "fresh_recovery_batched",
        outcomes,
    )?;
    let recovery_calibrated = calibrated_case(
        "recovery_calibrated",
        &recovery_batched,
        &cancellation_reference,
        &matched_recovered,
        outcomes,
    )?;
    // These independent no-trim controls expose batch-shape variability without
    // attributing a failed reused-vs-control comparison to numerical noise.
    let rewind_control_cross_batch = compare(&rewind_reference.logits, &rewind_cold)?;
    let cancellation_control_cross_batch =
        compare(&cancellation_reference.logits, &cancellation_cold)?;
    let batch_invariance_passed = [
        &exact,
        &extension_comparison,
        &rewind_comparison,
        &cancellation_comparison,
    ]
    .iter()
    .all(|comparison| comparison["bit_exact"] == true);
    let cache_safety_passed = [
        &exact,
        &extension_comparison,
        &rewind_matched,
        &cancellation_matched,
    ]
    .iter()
    .all(|comparison| comparison["bit_exact"] == true);
    outcomes.push(
        json!({"scenario":"production_logit_summary","exact_safety_passed":cache_safety_passed,
        "rewind_unmatched_prefix_vs_append_only":rewind_unmatched_prefix,
        "cancellation_unmatched_prefix_vs_append_only":cancellation_unmatched_prefix,
        "rewind_append_only_vs_batched_cold":rewind_control_cross_batch,
        "cancellation_append_only_vs_batched_cold":cancellation_control_cross_batch,
        "batch_invariance_diagnostic_only":batch_invariance_passed}),
    );
    let boundaries = replay_boundaries(&mut model, &tokens, retained.0, &cancel, outcomes)?;
    let passed = cache_safety_passed
        && rewind_calibrated
        && recovery_calibrated
        && boundaries["passed"] == true;
    Ok(
        json!({"passed":passed, "cache_policy":"production_full", "cached_tokens":tokens.len(),
        "sliding_window":retained.0, "retained_minimum":retained.1, "retained_maximum":retained.2,
        "batch_invariance_passed":batch_invariance_passed, "cache_safety_passed":cache_safety_passed,
        "acceptance":"independently_cleared_matching_retained_prefix_and_final_decode_schedules_with_verified_cache_work",
        "cross_batch_equality_required":false,
        "comparison_classes":{
            "exact":{"retained_prefix_computation":"matched_original_cold_prefill",
                "final_logit_producing_decode_shape":"matched","current_evaluation":"no_decode_valid_logits_hit_vs_cold_prefill",
                "gate":"bit_exact_cached_logits"},
            "extension":{"retained_prefix_computation":"matched_64_token_chunks",
                "final_decode_shape":"matched_64_token_chunk","gate":"bit_exact"},
            "historical_rewind_and_recovery":{"retained_prefix_computation":"unmatched_full_historical_batches_vs_short_cold",
                "final_decode_shape":"unmatched_single_token_vs_short_cold_batch","gate":"diagnostic_only"},
            "historical_vs_append_only":{"retained_prefix_computation":"unmatched_historical_vs_split_prefix",
                "final_decode_shape":"matched_single_token","gate":"diagnostic_only"},
            "matched_rewind_and_recovery":{"retained_prefix_computation":"matched_independent_prefix",
                "final_decode_shape":"matched_single_token","gate":"bit_exact_and_actual_greedy_exact"},
            "append_only_vs_batched_cold":{"retained_prefix_computation":"different_prefill_partitions",
                "final_decode_shape":"single_token_vs_physical_final_batch","gate":"fresh_fresh_calibration_reported_separately"}},
        "exact_work":exact_work, "exact":exact, "extension_work":extension_work,
        "extension":extension_comparison, "rewind_work":rewind_work, "rewind":rewind_comparison,
        "cancellation_recovery_work":recovery_work, "cancellation_recovery":cancellation_comparison,
        "cancelled_trim_work":cancelled_trim_work,
        "diagnostic_scope":"rewind_and_cancellation_recovery_fields_use_unmatched_historical_prefix_schedules_and_are_not_acceptance",
        "rewind_unmatched_prefix_vs_append_only":rewind_unmatched_prefix,
        "cancellation_unmatched_prefix_vs_append_only":cancellation_unmatched_prefix,
        "matched_schedule":{
            "decode_batch_tokens":64,
            "subject_history_tokens":tokens.len(),
            "rewind_retained_prefix_tokens":short.len() - 1,
            "cancellation_retained_prefix_tokens":62,
            "cancellation_trimmed_tokens":63,
            "rewind_work":matched_rewind_work,
            "cancelled_trim_work":matched_cancelled_trim_work,
            "cancellation_recovery_work":matched_recovery_work
        },
        "rewind_append_only_vs_batched_cold":rewind_control_cross_batch,
        "cancellation_append_only_vs_batched_cold":cancellation_control_cross_batch,
        "replay_boundaries":boundaries,
        "rewind_matched_schedule":rewind_matched, "cancellation_matched_schedule":cancellation_matched}),
    )
}

// Exercise the real public evaluation seam: empty input, zero/one retained
// token, decode-batch boundaries, and replay positions at the history window.
fn replay_boundaries(
    model: &mut NativeModel,
    tokens: &[i32],
    window: i32,
    cancel: &AtomicBool,
    outcomes: &mut Vec<Value>,
) -> Result<Value> {
    clear(model)?;
    ensure!(model.evaluate(&[], cancel).is_err());
    ensure!(model.work().prompt.submitted_tokens == 0 && model.diagnostic_logits().is_err());
    let mut lengths = vec![1, 2, 63, 64, 65, 999, 1000, 1023, 1024, 1025];
    if window > 0 {
        lengths.extend([window as usize, window as usize + 1, window as usize + 2]);
    }
    lengths.sort_unstable();
    lengths.dedup();
    let history_length = lengths.last().copied().unwrap() + 64;
    ensure!(
        history_length <= tokens.len(),
        "Boundary fixture exceeds context."
    );
    let mut cases = Vec::new();
    let mut passed = true;
    for length in lengths {
        outcomes.push(json!({"scenario":"boundary_start","tokens":length}));
        populate_replay_history(model, &tokens[..history_length], length - 1, cancel)?;
        ensure!(model.evaluate(&tokens[..length], cancel)?);
        let work = model.work();
        outcomes.push(
            json!({"scenario":"boundary_replay_work","tokens":length,"work":work,
            "history":model.diagnostic_cache_state(),"evaluated_tokens":model.evaluated_tokens()}),
        );
        if length == 1 {
            ensure!(
                work.reused_tokens == 0
                    && work.prefix_misses == 1
                    && work.cache_resets == 1
                    && work.trim_failures == 0
                    && work.exact_reuses == 0
                    && work.prefix_reuses == 0
                    && work.prompt.submitted_tokens == 1
                    && work.prompt.completed_tokens == 1
                    && work.prompt.submitted_batches == 1
                    && work.prompt.completed_batches == 1
                    && model.evaluated_tokens() == 1
            );
            let (_, minimum, maximum) = model.diagnostic_cache_state();
            ensure!(minimum == 0 && maximum == 0);
        } else {
            require_replay(model, length - 1, 1)?;
        }
        let actual = observe(model, &tokens[..length], cancel, "matched_rewind", outcomes)?;
        append_only_reference(model, &tokens[..length], cancel)?;
        let reference = observe(
            model,
            &tokens[..length],
            cancel,
            "fresh_append_only",
            outcomes,
        )?;
        let comparison = compare_observations(&reference, &actual)?;
        outcomes.push(
            json!({"scenario":"boundary_exact_comparison","tokens":length,
            "exact_safety_passed":comparison["matched_hard_gate"],"comparison":comparison}),
        );
        cold_evaluate(model, &tokens[..length], cancel)?;
        let batched = observe(model, &tokens[..length], cancel, "fresh_batched", outcomes)?;
        let floor = compare_observations(&batched, &reference)?;
        let cross_schedule = compare_observations(&batched, &actual)?;
        let excess = calibration::excess(&cross_schedule, &floor);
        let numerical_passed = excess
            .iter()
            .all(|metric| !calibration::METRICS[..4].contains(metric));
        let decision_passed = excess
            .iter()
            .all(|metric| !calibration::METRICS[4..].contains(metric));
        let exact_passed = comparison["matched_hard_gate"] == true;
        passed &= exact_passed && numerical_passed && decision_passed;
        let case = json!({"scenario":"replay_boundary","tokens":length, "replay_position":length - 1,
            "work":work,"exact_safety_passed":exact_passed,
            "calibrated_numerical_passed":numerical_passed,"calibrated_decisions_passed":decision_passed,
            "cold_append_only_comparison":comparison,"cross_schedule_comparison":cross_schedule,
            "fresh_fresh_floor":floor,"excess_metrics":excess,
            "matched_retained_prefix_computation":"independent_cold_prefix_length_minus_one",
            "matched_final_decode_shape":"one_token_at_length_minus_one",
            "cross_schedule_retained_prefix_computation":"cold_batched_vs_split_before_final_token",
            "cross_schedule_final_decode_shape":"final_physical_batch_vs_single_token",
            "batch_shape_source":"source_derived_not_runtime_kernel_attestation"});
        outcomes.push(case.clone());
        cases.push(case);
    }
    Ok(json!({"passed":passed, "empty_prompt_rejected":true,
        "acceptance":"independent_matching_retained_prefix_batches_then_single_token_replay",
        "decode_batch_tokens":64, "subject_history_tokens":history_length,
        "cases":cases}))
}

fn compact_cache(arguments: &Arguments, outcomes: &mut Vec<Value>) -> Result<Value> {
    let mut model =
        NativeModel::diagnostic_open_compact_cache(&arguments.model, &arguments.acceleration)?;
    let (window, _, _) = model.diagnostic_cache_state();
    outcomes.push(json!({"scenario":"compact_window_support","window":window,
        "context_size":model.context_size(),"unsupported_is_not_intended_refusal":true}));
    ensure!(
        window > 0 && (window as usize) < model.context_size() - 256,
        "Eviction smoke requires a sliding window smaller than the model context."
    );
    let tokens = fixture(&model)?;
    let short = &tokens[..128];
    let cancel = AtomicBool::new(false);
    cold_evaluate(&mut model, &tokens, &cancel)?;
    let (_, minimum, maximum) = model.diagnostic_cache_state();
    outcomes.push(json!({"scenario":"compact_history","work":model.work(),
        "retained_minimum":minimum,"retained_maximum":maximum}));
    ensure!(
        minimum > 0 && maximum == tokens.len() as i32 - 1,
        "Compact fixture did not evict sliding-window history."
    );
    ensure!(model.evaluate(short, &cancel)?);
    let work = model.work();
    let actual = record_logits(&model, "compact_refusal_recovery", outcomes)?;
    let (_, replay_minimum, replay_maximum) = model.diagnostic_cache_state();
    ensure!(
        model.evaluated_tokens() == short.len()
            && replay_minimum == 0
            && replay_maximum == short.len() as i32 - 1
    );
    // A refused reuse and a fresh evaluation submit exactly the same batches.
    let comparison = compare(&actual, &cold_logits(&mut model, short, &cancel)?)?;
    let passed = work.reused_tokens == 0
        && work.cache_resets == 1
        && work.trim_failures == 1
        && work.prompt.completed_tokens == short.len() as u64
        && work.prompt.submitted_tokens == short.len() as u64
        && work.prompt.completed_batches == short.len().div_ceil(64) as u64
        && work.prompt.submitted_batches == short.len().div_ceil(64) as u64
        && work.exact_reuses == 0
        && work.prefix_reuses == 0
        && comparison["bit_exact"] == true;
    outcomes.push(
        json!({"scenario":"compact_refusal","exact_safety_passed":passed,
        "memory_refusal_status":"evicted_history_reuse_intentionally_refused_and_cold_recomputed",
        "work":work,"fresh_comparison":comparison}),
    );
    Ok(
        json!({"passed":passed, "cache_policy":"diagnostic_compact", "sliding_window":window,
        "cached_tokens":tokens.len(), "retained_minimum":minimum, "retained_maximum":maximum,
        "return_tokens":short.len(), "work":work, "fresh_comparison":comparison}),
    )
}

fn acceptance_statuses(outcomes: &[Value], completed: bool) -> Value {
    let status = |key: &str| {
        let values: Vec<_> = outcomes
            .iter()
            .filter_map(|row| row[key].as_bool())
            .collect();
        if values.contains(&false) {
            "failed"
        } else if !completed {
            "incomplete"
        } else if values.is_empty() {
            "not_applicable"
        } else {
            "passed"
        }
    };
    json!({"exact_safety":status("exact_safety_passed"),
        "calibrated_numerical":status("calibrated_numerical_passed"),
        "calibrated_decisions":status("calibrated_decisions_passed"),
        "memory_refusal":if outcomes.iter().any(|row| row["scenario"] == "compact_refusal") {
            "observed_see_exact_safety"
        } else if completed { "not_applicable_full_history" } else { "incomplete_not_a_refusal_pass" },
        "adoption":"not_adopted_no_production_setting_change"})
}

fn main() -> Result<()> {
    let arguments = Arguments::parse();
    let mut production_observations = Vec::new();
    let production =
        production_cache(&arguments, &mut production_observations).unwrap_or_else(|error| {
            json!({"passed":false,"status":"incomplete",
            "error":error.to_string(),"failure_is_not_automatic_cache_corruption":true})
        });
    let mut compact_observations = Vec::new();
    let compact = compact_cache(&arguments, &mut compact_observations).unwrap_or_else(|error| {
        json!({"passed":false,"status":"incomplete_or_unsupported",
            "error":error.to_string(),"intended_refusal_passed":false})
    });
    let passed = production["passed"] == true && compact["passed"] == true;
    println!(
        "{}",
        json!({"passed":passed, "production":production, "compact":compact,
        "production_observations":production_observations,"compact_observations":compact_observations,
        "acceptance_statuses":{
            "production":acceptance_statuses(&production_observations, production.get("status").is_none()),
            "compact":acceptance_statuses(&compact_observations, compact.get("status").is_none()),
            "adoption":"not_adopted_no_production_setting_change"},
        "numeric_policy":"bit_exact_matched_and_paired_fresh_fresh_dominance_no_epsilon",
        "calibrated_metric_groups":{"numerical":calibration::METRICS[..4],
            "decisions":calibration::METRICS[4..]},
        "greedy_scope":"actual_native_sampler_cap24_synthetic_grammar_not_semantic_quality_or_UI_acceptance",
        "flash_policy":"production_request_unchanged_not_runtime_kernel_attestation",
        "scope":"synthetic_token_cache_regression_for_this_model_and_backend_not_universal_batch_invariance_or_model_quality",
        "historical_diagnostic_evidence":[
            {"source":"evidence/rust-migration/request-accounting-cache.json",
             "record":"final_cache_smoke",
             "scope":"preserved_failed_cross_batch_observations_not_current_run_results_or_passes"},
            {"source":"evidence/rust-migration/cache-replay-boundary-observation.json",
             "scope":"preserved_failed_unmatched_historical_prefix_boundary_observations_not_current_run_results_or_passes"}
        ]})
    );
    ensure!(
        passed,
        "Cache reuse exact safety, calibrated numerical/decision, or supported-refusal acceptance incomplete."
    );
    Ok(())
}
