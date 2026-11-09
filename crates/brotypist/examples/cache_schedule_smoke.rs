//! Disposable native-smoke calibration over the immutable synthetic writing corpus.
//! Run: cargo run -p brotypist --features native-smoke --example cache_schedule_smoke --
//!   --model PATH --acceleration auto --output NEW.json
//!
//! One weight owner, independently destructive-cleared trajectories. A uses the
//! production 64-token batching. B computes n-1 tokens, then appends one token.
//! Subjects compute the complete retained prefix with B's exact schedule, extend
//! native history, and rewind; one subject cancels after trim before recovering.
//! No checkpoint save/restore, backend changes, semantic-quality claim or fixed
//! float tolerance. Receipt writes precede assertions; failed gates exit nonzero.
//! Inputs and outputs are synthetic; no saved profile, capture or credentials are read.
//! Receipts are create-new and owner-only on Unix; use a private output directory.
#[path = "cache_measurement/calibration.rs"]
mod calibration;
#[path = "benchmark_writing/corpus.rs"]
mod corpus;

use anyhow::{Context, Result, ensure};
use brotypist::{
    config::Config,
    inference_native::{
        FinishReason, LLAMA_REVISION, NativeModel, NativeWork, SamplerOptions, Tokenizer,
    },
    providers::{
        GenerationOptions,
        diagnostics::{PromptProjection, prompt_projection},
    },
    writing,
};
use clap::Parser;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

const SEED: u32 = 19_220_024;
const RUNS: [&str; 6] = [
    "cold_a1",
    "cold_a2",
    "cold_b1",
    "cold_b2",
    "rewind",
    "cancelled_trim_recovery",
];

#[derive(Parser)]
struct Arguments {
    #[arg(long)]
    model: PathBuf,
    #[arg(long, default_value = "auto", value_parser = ["auto", "cpu"])]
    acceleration: String,
    /// Required fresh file; an existing file is never overwritten.
    #[arg(long)]
    output: PathBuf,
}

struct Receipt {
    file: File,
    value: Value,
}
impl Receipt {
    fn save(&mut self) -> Result<()> {
        // The file is exclusively created once. Flush every observation so an
        // ordinary later native/API/gate failure does not discard earlier work.
        let bytes = serde_json::to_vec_pretty(&self.value)?;
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&bytes)?;
        self.file.set_len(bytes.len() as u64)?;
        self.file.sync_data()?;
        Ok(())
    }
    fn event(&mut self, row: usize, run: &str, event: Value) -> Result<()> {
        self.value["rows"][row]["runs"][run]["events"]
            .as_array_mut()
            .context("Missing event array")?
            .push(event);
        self.save()
    }
}

fn digest_tokens(tokens: &[i32]) -> String {
    let mut digest = Sha256::new();
    for token in tokens {
        digest.update(token.to_le_bytes());
    }
    format!("{:x}", digest.finalize())
}
fn digest_logits(logits: &[f32]) -> String {
    let mut digest = Sha256::new();
    for value in logits {
        digest.update(value.to_bits().to_le_bytes());
    }
    format!("{:x}", digest.finalize())
}

fn digest_file(path: &Path) -> Result<String> {
    let mut input = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

/// Decode shape follows the pinned native Rust wrapper's chunks(64), not an
/// inferred N-LCP. Tokens/batches/reuse and native cell positions are checked
/// against actual counters after every call. Microbatch subdivision is backend
/// configuration identity, not separately instrumented by this public API.
fn shapes(start: usize, count: usize) -> Vec<Value> {
    (0..count).step_by(64).map(|offset| {
        let size = (count - offset).min(64);
        json!({"position":start + offset, "tokens":size,
            "configured_microbatch_partition":if size > 32 { vec![32, size - 32] } else { vec![size] }})
    }).collect()
}

fn evaluate(
    model: &mut NativeModel,
    tokens: &[i32],
    retained: usize,
    cancel_expected: bool,
    cancel: &AtomicBool,
    receipt: &mut Receipt,
    row: usize,
    run: &str,
    phase: &str,
) -> Result<()> {
    let evaluated_before = model.evaluated_tokens();
    let outcome = model.evaluate(tokens, cancel);
    let work = model.work();
    let cache = model.diagnostic_cache_state();
    let decoded = if cancel_expected {
        0
    } else {
        tokens.len() - retained
    };
    let logits_valid = model.diagnostic_logits().is_ok();
    receipt.event(row, run, json!({
        "phase":phase, "requested_tokens":tokens.len(), "requested_token_sha256_le_i32":digest_tokens(tokens),
        "evaluated_tokens_before":evaluated_before, "evaluated_tokens_after":model.evaluated_tokens(),
        "expected_retained_prefix":retained, "expected_decode_shape":shapes(retained, decoded),
        "shape_source":"Pinned NativeModel::decode chunks(64), verified against actual boundary counters; not a native per-batch trace",
        "native_work":work, "cache_state":{"swa_window":cache.0,"minimum":cache.1,"maximum":cache.2},
        "logits_valid":logits_valid, "cancel_requested":cancel.load(Ordering::Acquire),
        "completed":outcome.as_ref().ok(), "error":outcome.as_ref().err().map(ToString::to_string)
    }))?;
    ensure!(
        outcome? != cancel_expected,
        "Unexpected native evaluate completion in {phase}"
    );
    ensure!(
        work.reused_tokens == retained as u64 && work.trim_failures == 0,
        "Wrong retained prefix in {phase}"
    );
    ensure!(
        work.prompt.submitted_tokens == decoded as u64
            && work.prompt.completed_tokens == decoded as u64
            && work.prompt.submitted_batches == decoded.div_ceil(64) as u64
            && work.prompt.completed_batches == decoded.div_ceil(64) as u64,
        "Wrong complete decode shape in {phase}"
    );
    ensure!(
        work.exact_reuses == 0 && work.sampled_tokens == 0 && work.generation.submitted_tokens == 0,
        "Unexpected exact reuse or generation in {phase}"
    );
    ensure!(
        (retained == 0 && work.cold_evaluations == 1 && work.prefix_reuses == 0)
            || (retained > 0 && work.prefix_reuses == 1 && work.cache_resets == 0),
        "Wrong evaluation schedule in {phase}"
    );
    ensure!(
        model.evaluated_tokens() == retained + decoded
            && cache.1 == 0
            && cache.2 == (retained + decoded) as i32 - 1,
        "Unexpected retained native cells in {phase}"
    );
    ensure!(
        logits_valid != cancel_expected,
        "Cancelled trim exposed stale logits or completed decode lacks logits"
    );
    Ok(())
}

struct Snapshot {
    logits: Vec<f32>,
    text: String,
    finish: Vec<&'static str>,
    work: NativeWork,
}

fn trajectory(
    model: &mut NativeModel,
    projection: &PromptProjection,
    history: &[i32],
    receipt: &mut Receipt,
    row: usize,
    run: &str,
) -> Result<Snapshot> {
    let tokens = &projection.tokens;
    let retained = tokens.len() - 1;
    let cancel = AtomicBool::new(false);
    model.diagnostic_clear_cache();
    let cache = model.diagnostic_cache_state();
    let invalid = model.diagnostic_logits().is_err();
    receipt.event(
        row,
        run,
        json!({"phase":"independent_destructive_clear",
        "evaluated_tokens":model.evaluated_tokens(), "cache_state":[cache.0,cache.1,cache.2],
        "logits_invalid":invalid, "native_work_after_clear":model.work()}),
    )?;
    ensure!(
        model.evaluated_tokens() == 0 && cache.1 == -1 && cache.2 == -1 && invalid,
        "Independent native cache clear failed"
    );
    if run.starts_with("cold_a") {
        evaluate(
            model,
            tokens,
            0,
            false,
            &cancel,
            receipt,
            row,
            run,
            "production_full_prefill",
        )?;
    } else {
        evaluate(
            model,
            &tokens[..retained],
            0,
            false,
            &cancel,
            receipt,
            row,
            run,
            "matched_retained_prefix",
        )?;
        if !run.starts_with("cold_b") {
            evaluate(
                model,
                history,
                retained,
                false,
                &cancel,
                receipt,
                row,
                run,
                "extend_real_native_history",
            )?;
            if run == "cancelled_trim_recovery" {
                model.diagnostic_cancel_after_trim();
                evaluate(
                    model,
                    tokens,
                    retained,
                    true,
                    &cancel,
                    receipt,
                    row,
                    run,
                    "deterministic_cancel_after_destructive_trim",
                )?;
                cancel.store(false, Ordering::Release);
            }
        }
        evaluate(
            model,
            tokens,
            retained,
            false,
            &cancel,
            receipt,
            row,
            run,
            if run.starts_with("cold_b") {
                "append_only_final_token"
            } else {
                "rewind_or_recovery_final_token"
            },
        )?;
    }
    let logits = model.diagnostic_logits()?;
    let finite = logits.iter().all(|v| v.is_finite());
    receipt.event(row, run, json!({"phase":"prompt_logits", "logit_count":logits.len(),
        "sha256_le_f32_bits":digest_logits(&logits), "all_finite":finite,
        "top20_token_ids":if finite && !logits.is_empty() { Some(calibration::top(&logits)) } else { None }}))?;
    ensure!(finite && !logits.is_empty(), "Invalid prompt logits");
    let mut sampler = SamplerOptions::for_family(Some(&projection.family), false);
    sampler.temperature = 0.0;
    let allowance = 24_usize
        .checked_add(projection.replay_tokens)
        .context("Replay allowance overflow")?;
    ensure!(
        tokens.len() + allowance < model.context_size(),
        "Exact cap24 plus replay does not fit; no silent cap reduction"
    );
    let stops: Vec<&str> = projection.stops.iter().map(String::as_str).collect();
    let mut text = String::new();
    let mut finish = Vec::new();
    model.diagnostic_set_next_seed(SEED)?;
    let generation = model.generate(
        tokens,
        allowance,
        sampler,
        projection.grammar.as_deref(),
        &stops,
        &cancel,
        &mut |delta, reason| {
            text.push_str(delta);
            if let Some(reason) = reason {
                finish.push(match reason {
                    FinishReason::Stop => "stop",
                    FinishReason::Length => "length",
                });
            }
            Ok(!cancel.load(Ordering::Acquire))
        },
    );
    let work = model.work();
    receipt.event(row, run, json!({"phase":"native_grammar_constrained_greedy_generation",
        "raw_text_after_native_stop_fence":text, "finish_reasons":finish,
        "text_boundary":"Before provider replay/display filters; no semantic or UI claim",
        "replay_complete":text.starts_with(&projection.replay),
        "base_cap":24, "exact_replay_allowance":projection.replay_tokens, "actual_native_cap":allowance,
        "sampler":{"temperature":sampler.temperature,"top_k":sampler.top_k,"top_p":sampler.top_p,
            "min_p":sampler.min_p,"repeat_penalty":sampler.repeat_penalty,
            "requested_seed":SEED,"actual_seed":model.diagnostic_last_sampler_seed()},
        "grammar":projection.grammar, "stops":projection.stops, "native_work":work,
        "generation_decode_shape":shapes_for_generation(tokens.len(), work.generation.completed_tokens),
        "sampler_cleanup_returned":true, "cancel_requested":cancel.load(Ordering::Acquire),
        "error":generation.as_ref().err().map(ToString::to_string)
    }))?;
    generation?;
    ensure!(
        finish.len() == 1 && work.sampled_tokens > 0 && work.sampled_tokens <= allowance as u64,
        "Native greedy stream did not complete normally"
    );
    ensure!(
        work.exact_reuses == 1
            && work.reused_tokens == tokens.len() as u64
            && work.prompt.submitted_tokens == 0
            && work.prompt.completed_tokens == 0,
        "Greedy generation unexpectedly recomputed the measured prompt"
    );
    ensure!(
        work.generation.submitted_tokens == work.generation.completed_tokens
            && work.generation.submitted_batches == work.generation.completed_batches
            && work.generation.completed_batches == work.generation.completed_tokens,
        "Greedy generation did not use the expected single-token decode schedule"
    );
    ensure!(
        model.diagnostic_last_sampler_seed() == Some(SEED),
        "Unexpected actual sampler seed"
    );
    Ok(Snapshot {
        logits,
        text,
        finish,
        work,
    })
}

fn shapes_for_generation(start: usize, count: u64) -> Vec<Value> {
    (0..count)
        .map(|index| json!({"position":start as u64 + index,"tokens":1}))
        .collect()
}

fn comparison(cold: &Snapshot, other: &Snapshot) -> Result<Value> {
    let mut result = calibration::compare(&cold.logits, &other.logits, &cold.text, &other.text)?;
    let counts_exact = cold.work.sampled_tokens == other.work.sampled_tokens
        && cold.work.generation.submitted_tokens == other.work.generation.submitted_tokens
        && cold.work.generation.completed_tokens == other.work.generation.completed_tokens
        && cold.work.generation.submitted_batches == other.work.generation.submitted_batches
        && cold.work.generation.completed_batches == other.work.generation.completed_batches;
    result["actual_sampler_and_decode_counts_exact"] = json!(counts_exact);
    result["finish_reasons_exact"] = json!(cold.finish == other.finish);
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

fn measure_row(
    model: &mut NativeModel,
    config: &Config,
    case: &corpus::Case,
    receipt: &mut Receipt,
    row: usize,
) -> Result<()> {
    let options = GenerationOptions {
        instructions: case.instructions.clone(),
        context: if case.context.is_empty() {
            writing::reference_context(&case.prefix, &case.suffix, &case.reference_sources, 8000)
        } else {
            case.context.clone()
        },
        ..GenerationOptions::default()
    };
    let projection = prompt_projection(model, config, &case.prefix, &case.suffix, &options)?;
    receipt.value["rows"][row]["tokenizer_projection"] = json!({
        "prompt_tokens":projection.tokens, "prompt_token_sha256_le_i32":digest_tokens(&projection.tokens),
        "family":projection.family,"replay":projection.replay,"exact_replay_tokens":projection.replay_tokens,
        "grammar":projection.grammar,"stops":projection.stops,
        "scope":"Canonical last fitting tokenizer pass only; not measured native decode or latency"
    });
    receipt.save()?;
    ensure!(
        projection.tokens.len() > 1,
        "Prompt too short for prefix-then-tail schedule"
    );
    let extension = model.tokenize(" Synthetic follow-up history: the team reviewed the measurements and agreed to send the report on Thursday. The next review will confirm the delivery schedule. ".repeat(8).as_str(), false, false)?;
    let available = model
        .context_size()
        .saturating_sub(projection.tokens.len() + 1);
    let extension_count = extension.len().min(128).min(available);
    ensure!(
        extension_count > 0,
        "No context available for native history extension"
    );
    let mut history = projection.tokens.clone();
    history.extend_from_slice(&extension[..extension_count]);
    receipt.value["rows"][row]["schedule_identity"] = json!({
        "prompt_tokens":projection.tokens.len(),"retained_prefix_tokens":projection.tokens.len()-1,
        "cold_a_full_decode_shape":shapes(0,projection.tokens.len()),
        "cold_b_and_subject_retained_prefix_shape":shapes(0,projection.tokens.len()-1),
        "cold_b_and_subject_final_decode_shape":shapes(projection.tokens.len()-1,1),
        "history_tokens":history.len(),"history_token_sha256_le_i32":digest_tokens(&history),
        "history_extension_tokens":extension_count,"history_source":"Synthetic authored text, actual native decode; never captured history",
        "context_size":model.context_size(), "attention":"disabled", "full_swa":true,
        "batch":64,"microbatch":32
    });
    let mut snapshots = Vec::with_capacity(RUNS.len());
    for run in RUNS {
        receipt.value["rows"][row]["runs"][run] = json!({"status":"running","events":[]});
        receipt.save()?;
        let result = trajectory(model, &projection, &history, receipt, row, run);
        receipt.value["rows"][row]["runs"][run]["status"] =
            json!(if result.is_ok() { "complete" } else { "failed" });
        receipt.value["rows"][row]["runs"][run]["error"] =
            json!(result.as_ref().err().map(ToString::to_string));
        snapshots.push(result.ok());
        receipt.save()?;
    }
    // Preserve all available comparisons even if one trajectory failed later.
    let pairs = [
        ("fresh_same_a", 0, 1),
        ("fresh_same_b", 2, 3),
        ("fresh_cross_a1_b1", 0, 2),
        ("fresh_cross_a2_b2", 1, 3),
        ("matched_rewind_b1", 2, 4),
        ("matched_rewind_b2", 3, 4),
        ("matched_recovery_b1", 2, 5),
        ("matched_recovery_b2", 3, 5),
        ("cross_a1_rewind", 0, 4),
        ("cross_a2_rewind", 1, 4),
        ("cross_a1_recovery", 0, 5),
        ("cross_a2_recovery", 1, 5),
    ];
    for (name, a, b) in pairs {
        if let (Some(a), Some(b)) = (&snapshots[a], &snapshots[b]) {
            receipt.value["rows"][row]["comparisons"][name] = comparison(a, b)?;
            receipt.save()?;
        }
    }
    let complete = snapshots.iter().all(Option::is_some);
    let matched = [
        "matched_rewind_b1",
        "matched_rewind_b2",
        "matched_recovery_b1",
        "matched_recovery_b2",
    ]
    .iter()
    .all(|name| receipt.value["rows"][row]["comparisons"][*name]["matched_hard_gate"] == true);
    let mut numerical_excess = Vec::new();
    let mut decision_excess = Vec::new();
    for (subject, floor) in [
        ("cross_a1_rewind", "fresh_cross_a1_b1"),
        ("cross_a2_rewind", "fresh_cross_a2_b2"),
        ("cross_a1_recovery", "fresh_cross_a1_b1"),
        ("cross_a2_recovery", "fresh_cross_a2_b2"),
    ] {
        let comparisons = &receipt.value["rows"][row]["comparisons"];
        if comparisons[subject].is_object() && comparisons[floor].is_object() {
            for metric in calibration::excess(&comparisons[subject], &comparisons[floor]) {
                let entry = json!({"subject":subject,"fresh_floor":floor,"metric":metric});
                if calibration::METRICS[..4].contains(&metric) {
                    numerical_excess.push(entry);
                } else {
                    decision_excess.push(entry);
                }
            }
        }
    }
    let passed = complete && matched && numerical_excess.is_empty() && decision_excess.is_empty();
    receipt.value["rows"][row]["gates"] = json!({
        "all_native_trajectories_complete":complete,"matched_schedule_bit_exact":matched,
        "calibrated_numerical_no_observed_excess":complete && numerical_excess.is_empty(),
        "calibrated_decision_no_observed_excess":complete && decision_excess.is_empty(),
        "numerical_excess":numerical_excess,"decision_excess":decision_excess,"passed":passed
    });
    receipt.save()?;
    ensure!(
        passed,
        "Corpus row {} failed its schedule calibration gate",
        case.id
    );
    Ok(())
}

fn summarize(receipt: &mut Receipt) -> Result<()> {
    let rows = receipt.value["rows"].as_array().unwrap();
    let mut fresh_distributions = json!({});
    let mut reused_distributions = json!({});
    for name in [
        "fresh_same_a",
        "fresh_same_b",
        "fresh_cross_a1_b1",
        "fresh_cross_a2_b2",
        "cross_a1_rewind",
        "cross_a2_rewind",
        "cross_a1_recovery",
        "cross_a2_recovery",
    ] {
        let observations: Vec<_> = rows
            .iter()
            .filter_map(|row| row["comparisons"].get(name))
            .collect();
        if name.starts_with("fresh_") {
            fresh_distributions[name] = calibration::distribution(&observations);
        } else {
            reused_distributions[name] = calibration::distribution(&observations);
        }
    }
    let mut aggregate_comparisons = json!({});
    for (subject, floor) in [
        ("cross_a1_rewind", "fresh_cross_a1_b1"),
        ("cross_a2_rewind", "fresh_cross_a2_b2"),
        ("cross_a1_recovery", "fresh_cross_a1_b1"),
        ("cross_a2_recovery", "fresh_cross_a2_b2"),
    ] {
        aggregate_comparisons[subject] = calibration::paired_distribution(rows, subject, floor);
    }
    let mut total_work = NativeWork::default();
    let mut work_by_trajectory = json!({});
    for run in RUNS {
        let mut work = NativeWork::default();
        for row in rows {
            if let Some(events) = row["runs"][run]["events"].as_array() {
                for event in events {
                    if let Some(observed) = event.get("native_work") {
                        work += serde_json::from_value::<NativeWork>(observed.clone())?;
                    }
                }
            }
        }
        total_work += work;
        work_by_trajectory[run] = json!(work);
    }
    let n = rows.len();
    let complete = rows
        .iter()
        .filter(|row| row["gates"]["all_native_trajectories_complete"] == true)
        .count();
    let matched = rows
        .iter()
        .filter(|row| row["gates"]["matched_schedule_bit_exact"] == true)
        .count();
    let numerical = rows
        .iter()
        .filter(|row| row["gates"]["calibrated_numerical_no_observed_excess"] == true)
        .count();
    let decision = rows
        .iter()
        .filter(|row| row["gates"]["calibrated_decision_no_observed_excess"] == true)
        .count();
    receipt.value["summary"] = json!({
        "ordinary_corpus_rows":n,"complete_native_rows":complete,
        "three_gate_classes":{
            "matched_schedule_exact":{"passing_rows":matched,"required_rows":n,"tolerance":"None; f32 to_bits, greedy UTF-8, finish reasons and actual sampler/decode counts exact"},
            "calibrated_cross_schedule_numerical":{"passing_rows":numerical,"required_rows":n,"metrics":calibration::METRICS[..4]},
            "calibrated_cross_schedule_decisions":{"passing_rows":decision,"required_rows":n,"metrics":calibration::METRICS[4..]}
        },
        "fresh_fresh_distributions":fresh_distributions,
        "reused_cold_distributions":reused_distributions,
        "paired_corpus_rate_and_quantile_comparisons":aggregate_comparisons,
        "total_observed_native_work":total_work,"native_work_by_trajectory":work_by_trajectory,
        "work_accounting":"Every evaluate and generate call is counted once, including cancelled/error attempts. Clear snapshots contain prior counters and are excluded. Timings are boundary counters, not latency claims.",
        "paired_unit":"One immutable ordinary corpus prompt; A1/B1 and A2/B2 are paired schedule controls; both subjects share those controls",
        "repeats":"Two independently destructive-cleared cold observations per schedule; correlated repeats are NOT additional corpus units",
        "uncertainty":"Finite authored corpus, one model/backend/context and deterministic seed. No population confidence interval, independence assumption, power claim or nonsignificance acceptance; unseen inputs and run-to-run nondeterminism remain unbounded",
        "acceptance_rule":"All rows complete, exact matched gate, and no observed subject metric excess over its paired fresh A/B floor. Per-row dominance implies no excess in corpus rates or quantiles for the same paired samples; never a fixed epsilon",
        "same_schedule_floors":"Fresh A1/A2 and B1/B2 reported separately, not substituted for fresh cross-schedule A/B calibration",
        "semantic_quality":{"status":"not_run","required_separate_parent_gate":"full64 immutable semantic-quality corpus; cap24 cache agreement is not semantic quality"},
        "checkpoint_adoption":{"status":"not_tested","scope":"Ordinary cache correctness only; no whole-memory checkpoint API invoked"}
    });
    Ok(())
}

fn execute(arguments: &Arguments, receipt: &mut Receipt) -> Result<()> {
    let (_, cases) = corpus::corpus()?;
    let ordinary: Vec<_> = cases
        .iter()
        .filter(|case| case.kind == "completion")
        .collect();
    let excluded: Vec<_> = cases.iter().filter(|case| case.kind != "completion").map(|case| json!({
        "id":case.id,"kind":case.kind,
        "reason":if case.kind == "alternatives" { "Alternative multi-sample/conditioned-choice workflow is not this ordinary-writing logits probe" }
            else { "Non-ordinary utility workflow is not this ordinary-writing logits probe" }
    })).collect();
    receipt.value["corpus"] = json!({"source":"benchmark_writing::corpus::corpus (unchanged writing_quality.json and contextual_quality.json plus authored base rows)",
        "all_rows":cases.len(),"applicable_rows":ordinary.len(),"exclusions":excluded,
        "quality_asset_sha256":format!("{:x}",Sha256::digest(corpus::QUALITY_BYTES)),
        "contextual_asset_sha256":format!("{:x}",Sha256::digest(corpus::CONTEXTUAL_BYTES))});
    receipt.save()?;
    ensure!(!ordinary.is_empty(), "No ordinary corpus rows");
    let mut config = Config {
        prompt_style: "writing".into(),
        max_tokens: 24,
        model_path: arguments
            .model
            .to_str()
            .context("Non-Unicode model path")?
            .into(),
        acceleration: arguments.acceleration.clone(),
        launch_at_login: false,
        check_updates_automatically: false,
        clipboard_context: false,
        screen_context: false,
        screenshot_context: false,
        record_history: false,
        record_unaccepted: false,
        statistics_enabled: false,
        personalization_strength: 0,
        idle_release_seconds: 0,
        ..Config::default()
    };
    config.validate()?;
    let model_sha256 = digest_file(&arguments.model)?;
    receipt.value["model_provenance"] = json!({"path":arguments.model,"sha256":model_sha256,
        "hash_method":"Streaming SHA-256 with one 64KiB buffer before opening the sole native weight owner"});
    receipt.save()?;
    let mut model = NativeModel::open(&arguments.model, &arguments.acceleration)?;
    receipt.value["identity"] = json!({"model_path":arguments.model,"model_file_bytes":std::fs::metadata(&arguments.model)?.len(),
        "model_sha256":model_sha256,
        "architecture":model.metadata("general.architecture")?,"model_name":model.metadata("general.name")?,
        "context_size":model.context_size(),"context_chars":config.context_chars,
        "requested_acceleration":arguments.acceleration,"gpu_layers_requested":model.gpu_layers_requested(),
        "llama_revision":LLAMA_REVISION,"batch":64,"microbatch":32,"attention":"disabled","full_swa":true,
        "host_os":std::env::consts::OS,"host_arch":std::env::consts::ARCH,
        "attention_and_batch_identity_source":"Pinned production NativeModel open/decode configuration, not runtime-attested physical-kernel shape",
        "model_owners":1,"temperature":0,"base_generation_cap":24,"seed":SEED});
    receipt.save()?;
    for case in ordinary {
        let row = receipt.value["rows"].as_array().unwrap().len();
        receipt.value["rows"].as_array_mut().unwrap().push(
            json!({"id":case.id,"kind":case.kind,"language":case.language,
            "runs":{},"comparisons":{},"status":"running"}),
        );
        receipt.save()?;
        let result = measure_row(&mut model, &config, case, receipt, row);
        receipt.value["rows"][row]["status"] =
            json!(if result.is_ok() { "passed" } else { "failed" });
        receipt.value["rows"][row]["error"] = json!(result.as_ref().err().map(ToString::to_string));
        receipt.save()?;
    }
    summarize(receipt)?;
    receipt.save()?;
    ensure!(
        receipt.value["rows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["status"] == "passed"),
        "At least one ordinary corpus row failed; all observations retained in the receipt"
    );
    ensure!(
        receipt.value["summary"]["paired_corpus_rate_and_quantile_comparisons"]
            .as_object()
            .context("Missing aggregate comparisons")?
            .values()
            .all(|comparison| comparison["no_observed_aggregate_excess"] == true),
        "Corpus-level rate or distribution quantile exceeds the measured fresh/fresh floor"
    );
    Ok(())
}

fn main() -> Result<()> {
    let arguments = Arguments::parse();
    let mut output = OpenOptions::new();
    output.write(true).create_new(true);
    #[cfg(unix)]
    output.mode(0o600);
    let file = output.open(&arguments.output).with_context(|| {
        format!(
            "Receipt must use an explicit fresh private path: {}",
            arguments.output.display()
        )
    })?;
    let mut receipt = Receipt {
        file,
        value: json!({"schema":"ordinary-cache-schedule-calibration-v1",
        "status":"running","rows":[],"projection_is_not_decode":true,
        "float_threshold_policy":"No fixed absolute/relative epsilon and no hypothesis-test acceptance",
        "receipt_durability":"Synced after every observed native phase; ordinary errors preserve observations. Abrupt power loss during a rewrite is not claimed atomic",
        "scope":"Synthetic immutable ordinary-writing corpus, canonical prompt projection, real native prefill and cap24+exact-replay greedy generation; not UI, latency, full64 semantics or checkpoint adoption"}),
    };
    receipt.save()?;
    let result = execute(&arguments, &mut receipt);
    receipt.value["status"] = json!(if result.is_ok() { "passed" } else { "failed" });
    receipt.value["error"] = json!(result.as_ref().err().map(ToString::to_string));
    receipt.save()?;
    result
}
