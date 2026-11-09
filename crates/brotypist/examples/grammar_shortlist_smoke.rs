//! Real loaded-vocabulary comparison; never reads a saved profile or credentials.
//! cargo run -p brotypist --features native-smoke --example grammar_shortlist_smoke --
//!   --model PATH --acceleration auto --output NEW.json
//! Finite exact-output/count/finish gates, not distribution, quality or latency proof.
#[path = "benchmark_writing/corpus.rs"]
mod corpus;

use anyhow::{Context, Result, ensure};
use brotypist::{
    config::Config,
    features::synonym_prompt,
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
    sync::atomic::AtomicBool,
};

// Fixed before any model observation. No CLI seed, search, retry or prompt edits.
const SEEDS: [(&str, u32); 2] = [("greedy", 7), ("normal_family", 19_220_024)];
const RUNS: [(&str, bool); 4] = [
    ("full_vocabulary_1", true),
    ("shortlist_1", false),
    ("full_vocabulary_2", true),
    ("shortlist_2", false),
];

#[derive(Parser)]
struct Arguments {
    #[arg(long)]
    model: PathBuf,
    #[arg(long, default_value = "auto", value_parser = ["auto", "cpu"])]
    acceleration: String,
    /// Must not already exist; use a private parent directory.
    #[arg(long)]
    output: PathBuf,
}

struct Receipt {
    file: File,
    value: Value,
}
impl Receipt {
    fn save(&mut self) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(&self.value)?;
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&bytes)?;
        self.file.set_len(bytes.len() as u64)?;
        self.file.sync_data()?;
        Ok(())
    }
    fn event(&mut self, row: usize, run: &str, value: Value) -> Result<()> {
        self.value["rows"][row]["runs"][run]["events"]
            .as_array_mut()
            .context("Missing native observation array")?
            .push(value);
        self.save()
    }
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn hash_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
fn hash_logits(logits: &[f32]) -> String {
    let mut digest = Sha256::new();
    for value in logits {
        digest.update(value.to_bits().to_le_bytes());
    }
    format!("{:x}", digest.finalize())
}
fn count_identity(mut work: NativeWork) -> NativeWork {
    // Elapsed boundary observations are retained, never equated with parity.
    work.prompt.elapsed_us = 0;
    work.generation.elapsed_us = 0;
    work
}
struct Observation {
    text: String,
    finish: Vec<&'static str>,
    work: NativeWork,
    prefill: NativeWork,
    logits_hash: String,
    vocabulary: usize,
}
fn trajectory(
    model: &mut NativeModel,
    projection: &PromptProjection,
    cap: usize,
    sampler: SamplerOptions,
    seed: u32,
    full_vocabulary: bool,
    receipt: &mut Receipt,
    row: usize,
    run: &str,
) -> Result<Observation> {
    model.diagnostic_set_full_vocabulary_grammar(full_vocabulary);
    let cache = model.diagnostic_cache_state();
    let invalid = model.diagnostic_logits().is_err();
    receipt.event(
        row,
        run,
        json!({"phase":"independent_clear_and_sampler_close",
        "full_vocabulary_grammar":full_vocabulary, "evaluated_tokens":model.evaluated_tokens(),
        "cache_state":[cache.0,cache.1,cache.2], "logits_invalid":invalid,
        "last_sampler_seed":model.diagnostic_last_sampler_seed()}),
    )?;
    ensure!(
        model.evaluated_tokens() == 0 && cache.1 == -1 && cache.2 == -1 && invalid,
        "Independent clear failed"
    );
    let cancel = AtomicBool::new(false);
    let evaluation = model.evaluate(&projection.tokens, &cancel);
    let prefill = model.work();
    receipt.event(
        row,
        run,
        json!({"phase":"production_full_prefill",
        "native_work":prefill, "completed":evaluation.as_ref().ok(),
        "error":evaluation.as_ref().err().map(ToString::to_string)}),
    )?;
    ensure!(evaluation?, "Prefill cancelled");
    ensure!(
        prefill.cold_evaluations == 1
            && prefill.reused_tokens == 0
            && prefill.prompt.submitted_tokens == projection.tokens.len() as u64
            && prefill.prompt.completed_tokens == projection.tokens.len() as u64
            && prefill.prompt.submitted_batches == projection.tokens.len().div_ceil(64) as u64
            && prefill.prompt.completed_batches == prefill.prompt.submitted_batches,
        "Prefill schedule mismatch"
    );
    let logits = model.diagnostic_logits()?;
    let vocabulary = logits.len();
    let logits_hash = hash_logits(&logits);
    let finite = logits.iter().all(|value| value.is_finite());
    receipt.event(
        row,
        run,
        json!({"phase":"loaded_vocabulary_prompt_logits",
        "loaded_vocabulary_size":vocabulary, "sha256_le_f32_bits":logits_hash,
        "all_finite":finite}),
    )?;
    ensure!(
        finite && vocabulary > 128,
        "Invalid or insufficient loaded vocabulary"
    );
    drop(logits);
    let stops: Vec<&str> = projection.stops.iter().map(String::as_str).collect();
    let mut text = String::new();
    let mut finish = Vec::new();
    model.diagnostic_set_next_seed(seed)?;
    let generation = model.generate(
        &projection.tokens,
        cap,
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
            Ok(true)
        },
    );
    let work = model.work();
    receipt.event(row, run, json!({"phase":"generation", "raw_utf8_after_native_stop_fence":text,
        "utf8_sha256":hash(text.as_bytes()), "utf8_bytes":text.len(), "finish":finish,
        "sampled_tokens":work.sampled_tokens, "generated_decoded_tokens":work.generation.completed_tokens,
        "native_work":work, "requested_seed":seed, "actual_seed":model.diagnostic_last_sampler_seed(),
        "replay_complete":text.starts_with(&projection.replay),
        "error":generation.as_ref().err().map(ToString::to_string)}))?;
    generation?;
    ensure!(
        model.diagnostic_last_sampler_seed() == Some(seed),
        "Explicit seed not honored"
    );
    ensure!(
        finish.len() == 1 && work.sampled_tokens > 0 && work.sampled_tokens <= cap as u64,
        "Missing terminal observation or invalid sampled count"
    );
    ensure!(
        work.exact_reuses == 1
            && work.reused_tokens == projection.tokens.len() as u64
            && work.prompt.submitted_tokens == 0
            && work.prompt.completed_tokens == 0,
        "Generation changed the prefill schedule"
    );
    ensure!(
        work.generation.submitted_tokens == work.generation.completed_tokens
            && work.generation.submitted_batches == work.generation.completed_batches
            && work.generation.completed_batches == work.generation.completed_tokens,
        "Incomplete or non-single-token generation decode"
    );
    Ok(Observation {
        text,
        finish,
        work,
        prefill,
        logits_hash,
        vocabulary,
    })
}
fn comparison(a: &Observation, b: &Observation) -> Value {
    let text = a.text == b.text;
    let finish = a.finish == b.finish;
    let counts = count_identity(a.work) == count_identity(b.work)
        && count_identity(a.prefill) == count_identity(b.prefill);
    let prompt = a.logits_hash == b.logits_hash && a.vocabulary == b.vocabulary;
    json!({"raw_utf8_exact":text, "finish_exact":finish, "all_native_counts_exact":counts,
        "prompt_logit_bits_sha256_exact":prompt, "passed":text && finish && counts && prompt})
}
fn measure(
    model: &mut NativeModel,
    config: &Config,
    case: &corpus::Case,
    mode: &str,
    seed: u32,
    receipt: &mut Receipt,
    row: usize,
) -> Result<()> {
    let utility = case.kind == "synonyms";
    let options = GenerationOptions {
        instructions: case.instructions.clone(),
        context: if case.context.is_empty() {
            writing::reference_context(&case.prefix, &case.suffix, &case.reference_sources, 8000)
        } else {
            case.context.clone()
        },
        utility: if utility {
            synonym_prompt(&case.prefix, &case.suffix, &case.selection)
        } else {
            String::new()
        },
        ..GenerationOptions::default()
    };
    let mut budget = config.clone();
    budget.max_tokens = if utility { 128 } else { 64 };
    let projection = prompt_projection(model, &budget, &case.prefix, &case.suffix, &options)?;
    let cap = (budget.max_tokens as usize)
        .checked_add(projection.replay_tokens)
        .context("Replay cap overflow")?;
    let mut sampler = SamplerOptions::for_family(Some(&projection.family), false);
    if mode == "greedy" {
        sampler.temperature = 0.0;
    }
    receipt.value["rows"][row]["projection"] = json!(projection);
    receipt.value["rows"][row]["grammar_sha256"] =
        json!(projection.grammar.as_deref().map(|s| hash(s.as_bytes())));
    receipt.value["rows"][row]["base_cap"] = json!(budget.max_tokens);
    receipt.value["rows"][row]["native_cap_with_exact_replay"] = json!(cap);
    receipt.value["rows"][row]["grammar_path"] = json!(if utility {
        "existing_utility_json"
    } else if projection.grammar.is_some() {
        "ordinary_exact_healing_replay"
    } else {
        "ordinary_no_grammar_control"
    });
    receipt.value["rows"][row]["sampler"] = json!({"mode":mode, "seed":seed,
        "top_k":sampler.top_k,"top_p":sampler.top_p,"min_p":sampler.min_p,
        "temperature":sampler.temperature,"repeat_penalty":sampler.repeat_penalty,
        "typical_p":1.0,"diverse":false});
    receipt.save()?;
    ensure!(
        !projection.tokens.is_empty() && projection.tokens.len() + cap <= model.context_size(),
        "Exact corpus prompt and cap do not fit; no silent reduction"
    );
    let mut observed = Vec::new();
    for (run, full) in RUNS {
        receipt.value["rows"][row]["runs"][run] = json!({"status":"running","events":[]});
        receipt.save()?;
        let outcome = trajectory(
            model,
            &projection,
            cap,
            sampler,
            seed,
            full,
            receipt,
            row,
            run,
        );
        receipt.value["rows"][row]["runs"][run]["status"] = json!(if outcome.is_ok() {
            "complete"
        } else {
            "failed"
        });
        receipt.value["rows"][row]["runs"][run]["error"] =
            json!(outcome.as_ref().err().map(ToString::to_string));
        observed.push(outcome.ok());
        receipt.save()?;
    }
    // Both independent references expose backend repeatability; no mismatch is
    // waived as noise. Attribution is withheld if fresh/fresh prompt bits differ.
    for (label, left, right) in [
        ("pair_1", 0, 1),
        ("pair_2", 2, 3),
        ("reference_repeatability", 0, 2),
        ("wrapper_repeatability", 1, 3),
    ] {
        if let (Some(a), Some(b)) = (&observed[left], &observed[right]) {
            receipt.value["rows"][row]["comparisons"][label] = comparison(a, b);
        }
    }
    receipt.save()?;
    ensure!(
        observed.iter().all(Option::is_some)
            && receipt.value["rows"][row]["comparisons"]
                .as_object()
                .context("Missing comparisons")?
                .values()
                .all(|value| value["passed"] == true),
        "Grammar pair differs or native trajectory failed; observations retained"
    );
    Ok(())
}
fn execute(args: &Arguments, receipt: &mut Receipt) -> Result<()> {
    let (_, cases) = corpus::corpus()?;
    receipt.value["corpus"] = json!({"rows":cases.len(), "serialized_cases_sha256":hash(&serde_json::to_vec(&cases)?),
        "quality_sha256":hash(corpus::QUALITY_BYTES),"contextual_sha256":hash(corpus::CONTEXTUAL_BYTES),
        "excluded":cases.iter().filter(|case| case.kind == "alternatives").map(|case| json!({
            "id":case.id,"reason":"Alternatives multi-sample, conditioned-choice, diverse sampling and deduplication algorithm NOT exercised"})).collect::<Vec<_>>()});
    receipt.save()?;
    ensure!(
        cases.iter().all(|case| matches!(
            case.kind.as_str(),
            "completion" | "synonyms" | "alternatives"
        )),
        "Unrecognized immutable corpus path"
    );
    let model_path = args.model.canonicalize()?;
    let model_hash = hash_file(&model_path)?;
    receipt.value["model"] = json!({"requested_path":args.model,"canonical_path":model_path,
        "sha256":model_hash,"file_bytes":std::fs::metadata(&model_path)?.len()});
    receipt.save()?;
    let mut config = Config {
        prompt_style: "writing".into(),
        max_tokens: 64,
        model_path: model_path
            .to_str()
            .context("Non-Unicode model path")?
            .into(),
        acceleration: args.acceleration.clone(),
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
    let mut model = NativeModel::open(&model_path, &args.acceleration)?;
    receipt.value["native_identity"] = json!({"architecture":model.metadata("general.architecture")?,
        "model_name":model.metadata("general.name")?,"context_size":model.context_size(),
        "context_chars":config.context_chars,"requested_acceleration":args.acceleration,
        "gpu_layers_requested":model.gpu_layers_requested(),"llama_revision":LLAMA_REVISION,
        "batch":64,"microbatch":32,"full_swa":true,"flash_attention":false,
        "threads_requested":std::thread::available_parallelism().map_or(1, |n| n.get()).min(i32::MAX as usize),
        "os":std::env::consts::OS,"arch":std::env::consts::ARCH,"weight_owners":1,
        "backend_identity_scope":"Production policy and requested offload, not runtime-attested physical kernels",
        "schedule":"Independent destructive clear, complete prompt chunks(64), exact prompt reuse, single-token generation"});
    receipt.save()?;
    for case in cases.iter().filter(|case| case.kind != "alternatives") {
        for (mode, seed) in SEEDS {
            let row = receipt.value["rows"]
                .as_array()
                .context("Missing rows")?
                .len();
            receipt.value["rows"].as_array_mut().unwrap().push(json!({"id":case.id,"kind":case.kind,
                "language":case.language,"status":"running","mode":mode,"runs":{},"comparisons":{}}));
            receipt.save()?;
            let result = measure(&mut model, &config, case, mode, seed, receipt, row);
            receipt.value["rows"][row]["status"] =
                json!(if result.is_ok() { "passed" } else { "failed" });
            receipt.value["rows"][row]["error"] =
                json!(result.as_ref().err().map(ToString::to_string));
            receipt.save()?;
        }
    }
    model.diagnostic_set_full_vocabulary_grammar(false);
    let rows = receipt.value["rows"].as_array().unwrap();
    let passed = rows.iter().filter(|row| row["status"] == "passed").count();
    let replay = rows
        .iter()
        .filter(|row| row["grammar_path"] == "ordinary_exact_healing_replay")
        .count();
    let utility = rows
        .iter()
        .filter(|row| row["grammar_path"] == "existing_utility_json")
        .count();
    let all = passed == rows.len() && replay > 0 && utility > 0;
    receipt.value["summary"] = json!({"row_sampler_units":rows.len(),"passed_units":passed,
        "ordinary_replay_units":replay,"utility_units":utility,"passed":all,
        "repeat_count_per_policy":2,"ordinary_wrapper_default_restored":true});
    receipt.save()?;
    ensure!(
        all,
        "Grammar shortlist parity failed or required path missing; see receipt"
    );
    Ok(())
}
fn main() -> Result<()> {
    let args = Arguments::parse();
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options
        .open(&args.output)
        .context("Output must be a fresh file in a private directory")?;
    let mut receipt = Receipt {
        file,
        value: json!({"schema":"grammar-shortlist-paired-v1","status":"running","rows":[],
        "compiled_source_sha256":{
            "llama_bridge.c":hash(include_bytes!("../native/llama_bridge.c")),
            "grammar_sampler.h":hash(include_bytes!("../native/grammar_sampler.h")),
            "inference_native.rs":hash(include_bytes!("../src/inference_native.rs")),
            "providers.rs":hash(include_bytes!("../src/providers.rs")),
            "providers_diagnostics.rs":hash(include_bytes!("../src/providers_diagnostics.rs")),
            "writing.rs":hash(include_bytes!("../src/writing.rs")),
            "corpus.rs":hash(include_bytes!("benchmark_writing/corpus.rs")),
            "grammar_shortlist_smoke.rs":hash(include_bytes!("grammar_shortlist_smoke.rs"))},
        "coverage_limits":["Finite authored corpus and fixed greedy/normal-family seeds on one loaded model/backend only",
            "No universal distribution, semantic quality or latency equivalence claim; not the owned semantic matrix",
            "Raw UTF-8 is after native stop/UTF-8 fencing, before provider healing/display filters; not raw token pieces",
            "Alternatives search/conditioned-choice algorithm and diverse sampling NOT exercised",
            "No instrumentation of shortlist fast-path versus fallback hit counts; wrapper execution alone does not establish fast-path coverage",
            "Prompt logits hashes must agree for attribution; mismatch fails closed, never excused as backend noise"],
        "receipt_durability":"Synced after each phase and before gates; ordinary failures retain observations, rewrites are not power-loss atomic"}),
    };
    receipt.save()?;
    let result = execute(&args, &mut receipt);
    receipt.value["status"] = json!(if result.is_ok() { "passed" } else { "failed" });
    receipt.value["error"] = json!(result.as_ref().err().map(ToString::to_string));
    receipt.save()?;
    result
}
