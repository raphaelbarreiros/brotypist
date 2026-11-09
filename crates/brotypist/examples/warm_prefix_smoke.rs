//! Real GgufProvider trajectories over synthetic EN/PT bytes; no profile or desktop.
//! Work counts and opt-in paired-seed semantics; no elapsed times are published.
//! Each owner starts without prepare(), so its first request accounts for all
//! cold prompt work. Subsequent requests retain that owner's real native cache.
//! v4 contracts pin executed instrumentation; unused diagnostics remain provenance.
//! Comparison refusals emit a content-free, no-inference receipt before returning.
use anyhow::{Context, Result, ensure};
use brotypist::{
    config::Config,
    features::parse_choices,
    inference_native::{DecodeWork, NativeWork},
    providers::{GenerationOptions, GgufProvider, Provider, RequestMetrics},
};
use clap::Parser;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Parser)]
struct Arguments {
    /// Existing GGUF; its embedded tokenizer is the only tokenizer used.
    #[arg(long)]
    model: PathBuf,
    /// Required pin, checked before opening any provider.
    #[arg(long)]
    expect_model_sha256: String,
    #[arg(long, default_value = "cpu", value_parser = ["auto", "cpu"])]
    acceleration: String,
    #[arg(long, default_value_t = 24)]
    max_tokens: u32,
    #[arg(long, default_value_t = 8000)]
    context_chars: u32,
    /// A previous counts report; require identical model/settings/fixture contract.
    #[arg(long)]
    match_report: Option<PathBuf>,
    /// Explicitly include synthetic input and generated final output, never by default.
    #[arg(long)]
    include_fixture_output: bool,
    /// Add separately identified single-line work trajectories; old fixtures stay unchanged.
    #[arg(long)]
    include_single_line: bool,
    /// Stable per-case first-sampler seeds; alternatives retain native recurrence.
    /// Does not reset KV or change fixture order, cancellation, or model ownership.
    #[arg(long)]
    paired_seed: Option<u32>,
}

#[derive(Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Cancellation {
    None,
    BeforeRequest,
    FirstNonemptyOffer,
}

#[derive(Serialize)]
struct Case {
    id: String,
    owner: String,
    phase: &'static str,
    prefix: String,
    suffix: &'static str,
    options: GenerationOptions,
    cancellation: Cancellation,
}

fn fixtures() -> Vec<Case> {
    let mut cases = Vec::new();
    for (language, sentence, stem, suffix, endings, context, instructions) in [
        (
            "en",
            "The synthetic review records measurements, dates and decisions for the next meeting.",
            "Please send your",
            " before Friday.",
            [
                " ",
                " f",
                " fe",
                " feed",
                " feedb",
                " feedback",
                " feedback ",
                " feedback  ",
                " feedback o",
            ],
            "Synthetic reference: the next review is on Friday, not Monday.",
            "Continue in English with a concise professional tone.",
        ),
        (
            "pt",
            "A revisão sintética registra medições, datas e decisões para a próxima reunião.",
            "Por favor, envie sua",
            " antes de sexta-feira.",
            [
                " ",
                " a",
                " av",
                " aval",
                " avali",
                " avaliação",
                " avaliação ",
                " avaliação  ",
                " avaliação s",
            ],
            "Referência sintética: a próxima revisão será na sexta-feira, não na segunda-feira.",
            "Continue em português com um tom profissional e conciso.",
        ),
    ] {
        let mut stable = String::new();
        for index in 0..32 {
            stable.push_str(&format!("{index:02}: {sentence}\n"));
        }
        stable.push_str(stem);
        let owner = format!("{language}.trajectory");
        let mut add =
            |id: &str, ending: &str, phase, changed_context, changed_instructions, cancellation| {
                cases.push(Case {
                    id: format!("{language}.{id}"),
                    owner: owner.clone(),
                    phase,
                    prefix: format!("{stable}{ending}"),
                    suffix,
                    options: GenerationOptions {
                        context: if changed_context {
                            context.into()
                        } else {
                            String::new()
                        },
                        instructions: if changed_instructions {
                            instructions.into()
                        } else {
                            String::new()
                        },
                        ..GenerationOptions::default()
                    },
                    cancellation,
                });
            };
        add(
            "00.seed",
            endings[0],
            "seed_cold_owner",
            false,
            false,
            Cancellation::None,
        );
        for (id, ending) in [
            ("01.partial", endings[1]),
            ("02.partial", endings[2]),
            ("03.partial", endings[3]),
            ("04.partial", endings[4]),
            ("05.word", endings[5]),
            ("06.space", endings[6]),
            ("07.double_space", endings[7]),
            ("08.next_partial", endings[8]),
            ("09.backspace", endings[7]),
            ("10.caret_rewind", endings[2]),
            ("11.caret_return", endings[8]),
        ] {
            add(id, ending, "warm_request", false, false, Cancellation::None);
        }
        add(
            "12.context_changed",
            endings[8],
            "warm_request",
            true,
            false,
            Cancellation::None,
        );
        add(
            "13.instructions_changed",
            endings[8],
            "warm_request",
            true,
            true,
            Cancellation::None,
        );
        add(
            "14.options_restored",
            endings[8],
            "warm_request",
            false,
            false,
            Cancellation::None,
        );
        add(
            "15.precancel",
            endings[8],
            "cancellation_probe",
            false,
            false,
            Cancellation::BeforeRequest,
        );
        add(
            "16.cancel_on_offer",
            endings[8],
            "cancellation_probe",
            false,
            false,
            Cancellation::FirstNonemptyOffer,
        );
        add(
            "17.recovery",
            endings[8],
            "warm_request",
            false,
            false,
            Cancellation::None,
        );

        // Alternatives have their own owner/seed and never pollute trajectory totals.
        for (id, ending, alternatives) in [
            ("alt.00.seed", endings[0], false),
            ("alt.01.partial", endings[4], true),
            ("alt.02.space", endings[6], true),
        ] {
            cases.push(Case {
                id: format!("{language}.{id}"),
                owner: format!("{language}.alternatives"),
                phase: if alternatives {
                    "alternatives_request"
                } else {
                    "seed_cold_owner"
                },
                prefix: format!("{stable}{ending}"),
                suffix,
                options: GenerationOptions {
                    alternatives,
                    ..GenerationOptions::default()
                },
                cancellation: Cancellation::None,
            });
        }
    }
    cases
}

fn single_line_fixtures() -> Vec<Case> {
    let mut cases = Vec::new();
    for (language, sentence, stem, endings, suffix) in [
        (
            "en",
            "The synthetic review records measurements and decisions for the next meeting. ",
            "Please send your",
            [" f", " fe", " feedback", " feedback ", " feedback n"],
            " before Friday.",
        ),
        (
            "pt",
            "A revisão sintética registra medições e decisões para a próxima reunião. ",
            "Por favor, envie sua",
            [" a", " av", " avaliação", " avaliação ", " avaliação n"],
            " antes de sexta-feira.",
        ),
    ] {
        let stable = format!("{}{stem}", sentence.repeat(32));
        for (index, ending) in endings.into_iter().enumerate() {
            cases.push(Case {
                id: format!("{language}.single_line.{index:02}"),
                owner: format!("{language}.single_line"),
                phase: if index == 0 {
                    "seed_cold_owner"
                } else {
                    "warm_request"
                },
                prefix: format!("{stable}{ending}"),
                suffix,
                options: GenerationOptions::default(),
                cancellation: Cancellation::None,
            });
        }
    }
    cases
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Same derivation as benchmark_writing's paired-case-round-v2, with round zero.
/// The reserved random sentinel maps to zero, never a falsely claimed exact seed.
fn case_seed(root: u32, case_id: &str) -> u32 {
    let mut hash = Sha256::new();
    hash.update(b"brotypist-paired-case-round-v2\0");
    hash.update(root.to_le_bytes());
    hash.update(0_u64.to_le_bytes());
    hash.update(case_id.as_bytes());
    let bytes = hash.finalize();
    u32::from_le_bytes(bytes[..4].try_into().expect("SHA-256 word")) % u32::MAX
}

fn model_digest(path: &Path) -> Result<(String, u64)> {
    let mut file = File::open(path).context("Cannot open supplied GGUF")?;
    ensure!(
        file.metadata()?.is_file(),
        "Supplied GGUF is not a regular file"
    );
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut bytes = 0_u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        bytes += count as u64;
    }
    Ok((format!("{:x}", hasher.finalize()), bytes))
}

fn decode_counts(work: DecodeWork) -> Value {
    json!({
        "submitted_tokens": work.submitted_tokens,
        "completed_tokens": work.completed_tokens,
        "submitted_batches": work.submitted_batches,
        "completed_batches": work.completed_batches,
    })
}

fn work_counts(work: NativeWork) -> Value {
    json!({
        "evaluations": work.evaluations,
        "input_tokens": work.input_tokens,
        "reused_tokens": work.reused_tokens,
        "prompt": decode_counts(work.prompt),
        "generation": decode_counts(work.generation),
        "sampled_tokens": work.sampled_tokens,
        "cold_evaluations": work.cold_evaluations,
        "exact_reuses": work.exact_reuses,
        "prefix_reuses": work.prefix_reuses,
        "prefix_misses": work.prefix_misses,
        "trim_failures": work.trim_failures,
        "cache_resets": work.cache_resets,
    })
}

fn reconcile(metrics: RequestMetrics) -> Result<()> {
    let work = metrics.native;
    for decode in [work.prompt, work.generation] {
        ensure!(decode.completed_tokens <= decode.submitted_tokens);
        ensure!(decode.completed_batches <= decode.submitted_batches);
        ensure!(decode.submitted_batches <= decode.submitted_tokens);
        ensure!(decode.completed_batches <= decode.completed_tokens);
        ensure!(decode.submitted_tokens <= decode.submitted_batches * 64);
        ensure!(decode.completed_tokens <= decode.completed_batches * 64);
        if !metrics.failed {
            ensure!(decode.completed_tokens == decode.submitted_tokens);
            ensure!(decode.completed_batches == decode.submitted_batches);
        }
    }
    ensure!(work.generation.submitted_tokens == work.generation.submitted_batches);
    ensure!(work.generation.completed_tokens == work.generation.completed_batches);
    ensure!(work.generation.submitted_tokens <= work.sampled_tokens);
    ensure!(work.reused_tokens + work.prompt.completed_tokens <= work.input_tokens);
    let classified = work.cold_evaluations
        + work.exact_reuses
        + work.prefix_reuses
        + work.prefix_misses
        + work.trim_failures;
    ensure!(classified <= work.evaluations);
    if !metrics.cancelled && !metrics.failed {
        ensure!(classified == work.evaluations);
        ensure!(work.reused_tokens + work.prompt.completed_tokens == work.input_tokens);
        // Every completed generate call leaves its terminal/EOG/consumer-stop
        // sample undecoded. This holds across all internal alternatives samples.
        ensure!(work.sampled_tokens == work.generation.completed_tokens + work.evaluations);
    }
    Ok(())
}

#[derive(Default)]
struct Totals {
    requests: u64,
    no_offer: u64,
    cancellation_not_reached: u64,
    native: NativeWork,
}

fn main() -> Result<()> {
    let arguments = Arguments::parse();
    let model = arguments
        .model
        .canonicalize()
        .context("Supplied model must already exist")?;
    let (model_sha256, model_bytes) = model_digest(&model)?;
    ensure!(
        model_sha256 == arguments.expect_model_sha256.to_ascii_lowercase(),
        "Supplied GGUF does not match the required SHA-256"
    );
    // Only Config fields present in pre-roadmap HEAD are named here. Never load
    // a saved profile, resolve a preset, migrate a model, or start a runtime.
    let mut config = Config {
        backend: "gguf".into(),
        model_path: model.to_str().context("Model path must be Unicode")?.into(),
        acceleration: arguments.acceleration.clone(),
        prompt_style: "writing".into(),
        max_tokens: arguments.max_tokens,
        context_chars: arguments.context_chars,
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
    let mut cases = fixtures();
    if arguments.include_single_line {
        cases.extend(single_line_fixtures());
    }
    ensure!(
        cases
            .iter()
            .all(|case| case.prefix.chars().count() <= config.context_chars as usize),
        "context-chars would truncate a synthetic stable prefix"
    );
    let contract = json!({
        "schema": if arguments.include_single_line { "warm-prefix-v4-seeds-single-line" } else { "warm-prefix-v4-seeds" },
        "model_sha256": model_sha256,
        "model_bytes": model_bytes,
        "tokenizer": "embedded_in_the_pinned_gguf",
        "backend": config.backend,
        "acceleration": config.acceleration,
        "prompt_style": config.prompt_style,
        "max_tokens": config.max_tokens,
        "context_chars": config.context_chars,
        "fixtures_sha256": digest(&serde_json::to_vec(&cases)?),
        "harness_sha256": digest(include_bytes!("warm_prefix_smoke.rs")),
        "native_rust_sha256": digest(include_bytes!("../src/inference_native.rs")),
        "native_bridge_sha256": digest(include_bytes!("../native/llama_bridge.c")),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "native_threads": std::thread::available_parallelism().map_or(1, |n| n.get()).min(i32::MAX as usize),
        "prepare_called": false,
        "sampler_policy": if arguments.paired_seed.is_some() {
            "paired_case_round_v2_first_sampler_then_native_recurrence"
        } else {
            "ordinary_production_family_sampler_and_native_recurrence"
        },
        "paired_seed": arguments.paired_seed,
        "seed_round": 0,
        "seed_derivation": "SHA-256(brotypist-paired-case-round-v2\\0 || root-u32-LE || zero-u64-LE || case-ID-UTF8), first-u32-LE modulo 4294967295",
        "sampler_seed_observation": "NativeModel diagnostic_last_sampler_seed and diagnostic_sampler_seed after each actual generation attempt; at most five attempts per request; null is unknown or no sampler, never an invented seed",
        "comparison_mode": if arguments.paired_seed.is_some() {
            "paired_first_seed_retained_kv_semantic_trajectory"
        } else {
            "ordinary_work_counts_not_controlled_semantic_pairs"
        },
        "alternatives_seed_policy": "only first generation attempt overridden; subsequent observed seeds follow native recurrence; candidate-dependent stopping can change sequence length",
        "kv_policy": "retained per owner; no per-case diagnostic clearing",
        "os_cache_policy": "not cleared or controlled",
    });
    if let Some(path) = &arguments.match_report {
        let previous: Result<Value> = (|| Ok(serde_json::from_reader(File::open(path)?)?))();
        let contract_matched = previous
            .as_ref()
            .map(|report| report["contract"] == contract)
            .unwrap_or(false);
        let refusal = match &previous {
            Err(_) => Some("comparison_report_unreadable"),
            Ok(_) if !contract_matched => Some("execution_contract_mismatch"),
            Ok(report) if report["completed_run"] != true => Some("comparison_report_incomplete"),
            Ok(_) => None,
        };
        if let Some(reason) = refusal {
            // Compare the full contract, including missing and unexpected fields.
            // Only field names are emitted; never copy prior report values or paths.
            let differing_contract_fields = previous.as_ref().ok().map(|report| {
                let actual = contract.as_object().expect("contract is an object");
                let expected = report["contract"].as_object();
                let mut fields: Vec<_> = actual
                    .keys()
                    .chain(expected.into_iter().flat_map(|object| object.keys()))
                    .filter(|key| actual.get(*key) != expected.and_then(|object| object.get(*key)))
                    .collect();
                fields.sort_unstable();
                fields.dedup();
                fields
            });
            let receipt = json!({
                "contract": contract,
                "providers_sha256": digest(include_bytes!("../src/providers.rs")),
                "writing_sha256": digest(include_bytes!("../src/writing.rs")),
                "providers_diagnostics_sha256": digest(include_bytes!("../src/providers_diagnostics.rs")),
                "contract_matched": contract_matched,
                "completed_run": false,
                "failure_stage": "comparison_preflight",
                "failure_reason": reason,
                "differing_contract_fields": differing_contract_fields,
                "provider_constructed": false,
                "inference_performed": false,
                "inference_claim": "none; no requests, sampler observations or work results were produced",
            });
            let mut stdout = std::io::stdout().lock();
            serde_json::to_writer_pretty(&mut stdout, &receipt)?;
            writeln!(stdout)?;
            stdout.flush()?;
            anyhow::bail!("Comparison refused: {reason}; no provider was constructed");
        }
    }
    let mut provider: Option<GgufProvider> = None;
    let mut owner = String::new();
    let mut rows = Vec::with_capacity(cases.len());
    let mut totals: BTreeMap<String, Totals> = BTreeMap::new();
    let mut failed_requests = 0_u64;
    for case in &cases {
        let cold_owner = owner != case.owner;
        if cold_owner {
            if let Some(mut previous) = provider.take() {
                previous.close()?;
            }
            provider = Some(GgufProvider::new(config.clone()));
            owner.clone_from(&case.owner);
        }
        let provider = provider.as_mut().expect("owner created");
        let expected_seed = arguments.paired_seed.map(|root| case_seed(root, &case.id));
        if let Some(seed) = expected_seed {
            provider.diagnostic_set_next_seed(seed)?;
        }
        let cancel = AtomicBool::new(case.cancellation == Cancellation::BeforeRequest);
        let mut callbacks = 0_u64;
        let mut nonempty_callbacks = 0_u64;
        let mut final_bytes = 0;
        let mut final_scalars = 0;
        let mut final_utf16_units = 0;
        let mut final_choices = 0;
        let mut final_offer = false;
        let mut fixture_output = String::new();
        let result = provider.stream(
            &case.prefix,
            case.suffix,
            &case.options,
            &cancel,
            &mut |text| {
                callbacks += 1;
                nonempty_callbacks += u64::from(!text.is_empty());
                final_bytes = text.len();
                final_scalars = text.chars().count();
                final_utf16_units = text.encode_utf16().count();
                final_choices = if case.options.alternatives {
                    parse_choices(text, 5).len()
                } else {
                    0
                };
                final_offer = if case.options.alternatives {
                    text.trim_end().ends_with(']') && final_choices > 0
                } else {
                    !text.is_empty()
                };
                if arguments.include_fixture_output {
                    fixture_output.clear();
                    fixture_output.push_str(text);
                }
                if case.cancellation == Cancellation::FirstNonemptyOffer && !text.is_empty() {
                    cancel.store(true, Ordering::Release);
                }
                Ok(())
            },
        );
        let metrics = provider
            .take_metrics()
            .context("Provider omitted request accounting")?;
        ensure!(
            provider.take_metrics().is_none(),
            "Metrics were not consumed exactly once"
        );
        let sampler_seeds = provider.diagnostic_sampler_seeds();
        ensure!(sampler_seeds.len() <= 5, "Unbounded sampling evidence");
        for (index, sample) in sampler_seeds.iter().enumerate() {
            ensure!(
                sample.requested_override == if index == 0 { expected_seed } else { None },
                "Seed override differs from the per-case policy for {}",
                case.id
            );
            if let Some(expected) = sample.requested_override {
                if sample.observed_seed.is_some() || sample.sampled_tokens > 0 {
                    ensure!(
                        sample.observed_seed == Some(expected),
                        "Observed first sampler seed differs from expected for {}",
                        case.id
                    );
                }
            }
        }
        ensure!(
            metrics.failed == result.is_err(),
            "Failure accounting differs from provider result"
        );
        ensure!(metrics.cancelled == cancel.load(Ordering::Acquire));
        ensure!(metrics.alternatives == case.options.alternatives && !metrics.utility);
        ensure!(
            metrics.no_offer == !final_offer,
            "Final offer accounting differs from callback"
        );
        reconcile(metrics).with_context(|| format!("Unreconciled native work for {}", case.id))?;
        if cold_owner && !metrics.failed {
            ensure!(metrics.native.cold_evaluations == 1 && metrics.native.reused_tokens == 0);
        }
        if case.cancellation == Cancellation::BeforeRequest {
            ensure!(metrics.native == NativeWork::default() && metrics.no_offer && callbacks == 0);
        }
        let cancellation_not_reached =
            case.cancellation == Cancellation::FirstNonemptyOffer && !metrics.cancelled;
        let status = if metrics.failed {
            "failed"
        } else if metrics.cancelled {
            "cancelled"
        } else {
            "completed"
        };
        failed_requests += u64::from(metrics.failed);
        let bucket = totals
            .entry(format!("{}:{status}", case.phase))
            .or_default();
        bucket.requests += 1;
        bucket.no_offer += u64::from(metrics.no_offer);
        bucket.cancellation_not_reached += u64::from(cancellation_not_reached);
        bucket.native += metrics.native;
        let mut row = json!({
            "case_id": case.id,
            "owner_id": case.owner,
            "phase": case.phase,
            "cold_owner": cold_owner,
            "status": status,
            "cancelled": metrics.cancelled,
            "failed": metrics.failed,
            "no_offer": metrics.no_offer,
            "alternatives": metrics.alternatives,
            "cancellation": case.cancellation,
            "cancellation_not_reached": cancellation_not_reached,
            "prefix_bytes": case.prefix.len(),
            "prefix_scalars": case.prefix.chars().count(),
            "prefix_utf16_units": case.prefix.encode_utf16().count(),
            "suffix_bytes": case.suffix.len(),
            "callbacks": callbacks,
            "nonempty_callbacks": nonempty_callbacks,
            "final_output_bytes": final_bytes,
            "final_output_scalars": final_scalars,
            "final_output_utf16_units": final_utf16_units,
            "final_choices": final_choices,
            "native": work_counts(metrics.native),
            "expected_first_sampler_seed": expected_seed,
            "observed_first_sampler_seed": sampler_seeds.first().and_then(|sample| sample.observed_seed),
            "sampler_seed_sequence": sampler_seeds,
            "sampler_seed_sequence_complete": true,
        });
        if arguments.include_fixture_output {
            row["synthetic_fixture_input"] = serde_json::to_value(case)?;
            row["synthetic_fixture_generated_final_output"] = json!(fixture_output);
        }
        rows.push(row);
    }
    if let Some(mut provider) = provider {
        provider.close()?;
    }
    let totals: BTreeMap<_, _> = totals
        .into_iter()
        .map(|(id, total)| {
            (
                id,
                json!({
                    "requests": total.requests,
                    "no_offer": total.no_offer,
                    "cancellation_not_reached": total.cancellation_not_reached,
                    "native": work_counts(total.native),
                }),
            )
        })
        .collect();
    let report = json!({
        "contract": contract,
        // Prompt/provider identities and the unused DiagnosticSession module are
        // provenance, not equality requirements. Controls retain strict fixture,
        // sampler, executed native implementation and trajectory contracts.
        "providers_sha256": digest(include_bytes!("../src/providers.rs")),
        "writing_sha256": digest(include_bytes!("../src/writing.rs")),
        "providers_diagnostics_sha256": digest(include_bytes!("../src/providers_diagnostics.rs")),
        "contract_matched": arguments.match_report.is_some(),
        "completed_run": failed_requests == 0,
        "failed_requests": failed_requests,
        "fixture_output_included": arguments.include_fixture_output,
        "latency_claim": "none; counts only; native/provider timers intentionally omitted",
        "scope": "synthetic provider requests, not desktop insertions, accepted suggestions or quality scores",
        "warm_definition": "same native owner after the seed request, not guaranteed cache reuse",
        "alternatives_accounting": "NativeWork sums every internal generate call, not only the final sample or visible choices",
        "cancellation_scope": "pre-request plus first visible nonempty callback; no timed/native-decode cancellation claim",
        "cases": rows,
        "totals": totals,
    });
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, &report)?;
    writeln!(stdout)?;
    ensure!(
        failed_requests == 0,
        "Provider requests failed; inspect content-free failed rows"
    );
    Ok(())
}
