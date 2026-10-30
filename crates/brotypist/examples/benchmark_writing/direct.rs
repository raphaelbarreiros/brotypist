//! Direct native diagnostics using the same canonical framing, replay, output
//! filtering and alternatives algorithms as GgufProvider. Only observation and
//! the explicitly separate legacy diagnostic token allowance are added here.
use super::{
    corpus::{self, Case},
    direct_checks, evaluation, native_details,
};
use anyhow::{Context, Result, ensure};
use brotypist::{
    config::Config,
    features::synonym_prompt,
    providers::{GenerationOptions, diagnostics::DiagnosticSession},
    writing,
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

fn ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}
fn options(case: &Case, max_words: u32) -> GenerationOptions {
    GenerationOptions {
        max_words,
        instructions: case.instructions.clone(),
        context: if !case.context.is_empty() {
            case.context.clone()
        } else {
            writing::reference_context(&case.prefix, &case.suffix, &case.reference_sources, 8000)
        },
        alternatives: case.kind == "alternatives",
        anchor: case.anchor.clone(),
        utility: if case.kind == "synonyms" {
            synonym_prompt(&case.prefix, &case.suffix, &case.selection)
        } else {
            String::new()
        },
        ..GenerationOptions::default()
    }
}
fn stream_case(
    direct: &mut DiagnosticSession,
    case: &Case,
    token_cap: u32,
    words: u32,
) -> Result<Value> {
    let options = options(case, words);
    direct.reset_observations();
    let cancel = AtomicBool::new(false);
    let started = Instant::now();
    let mut text = String::new();
    let mut first = None;
    let mut updates = 0;
    let execution = direct.stream(
        &case.prefix,
        &case.suffix,
        &options,
        token_cap,
        &cancel,
        &mut |value| {
            text.clear();
            text.push_str(value);
            updates += 1;
            if first.is_none() && evaluation::displayable(case, value) {
                first = Some(ms(started));
            }
            Ok(())
        },
    );
    let elapsed = ms(started);
    let error = execution.as_ref().err().map(ToString::to_string);
    Ok(
        json!({"case":case.id, "kind":case.kind, "generated_text":text,
        "assembled_draft":if case.kind == "completion" { Some(format!("{}{}{}", case.prefix, text, case.suffix)) } else { None },
        "reference_context_used":options.context, "reference_context_truncated_for_worker":false,
        "requested_limits":{"max_tokens":token_cap, "max_words":words},
        "time_to_first_displayable_text_ms":first, "total_stream_ms":if error.is_none() { Some(elapsed) } else { None },
        "stream_observed_ms":elapsed, "stream_updates":updates, "error":error,
        "protocol_completed":execution.is_ok(), "measurement_path":"direct_native_provider",
        "native_calls":direct.observations(), "mechanical":direct_checks::mechanical(case, &text, direct.observations(), direct.context_size(), token_cap, error.is_some()),
        "semantic_evaluation":evaluation::semantic(case, &text)?, "semantic_evaluation_error":null,
        "process_lifetime_peak_rss_bytes_after_case":native_details::peak_rss_bytes()?}),
    )
}
fn cancellation(direct: &mut DiagnosticSession, token_cap: u32, pre_cancelled: bool) -> Value {
    let cancel = AtomicBool::new(pre_cancelled);
    direct.reset_observations();
    direct.cancel_on_first_chunk(!pre_cancelled);
    let started = Instant::now();
    let mut updates = 0;
    let mut after_cancel = 0;
    let execution = direct.stream(
        "The next step in the project is to",
        "",
        &GenerationOptions {
            max_words: 8,
            ..GenerationOptions::default()
        },
        token_cap,
        &cancel,
        &mut |_| {
            updates += 1;
            if cancel.load(Ordering::Acquire) {
                after_cancel += 1;
            }
            Ok(())
        },
    );
    let finished = Instant::now();
    direct.cancel_on_first_chunk(false);
    let requested = if pre_cancelled {
        Some(started)
    } else {
        direct.cancellation_time()
    };
    let status = if execution.is_err() || after_cancel > 0 {
        "fail"
    } else if pre_cancelled && !direct.observations().is_empty() {
        "fail_native_generation_started_despite_pre_cancel"
    } else if requested.is_none() {
        "inconclusive_no_native_chunk_to_cancel"
    } else {
        "pass"
    };
    json!({"kind":if pre_cancelled { "pre_cancelled" } else { "after_first_native_chunk" }, "status":status,
        "cancel_requested":requested.is_some(), "cancel_request_ms":requested.map(|time| time.duration_since(started).as_secs_f64() * 1000.0),
        "return_after_cancel_ms":requested.map(|time| finished.duration_since(time).as_secs_f64() * 1000.0),
        "total_ms":finished.duration_since(started).as_secs_f64() * 1000.0, "updates":updates, "outputs_after_cancel":after_cancel,
        "error":execution.err().map(|error| error.to_string()), "native_generation_started":!direct.observations().is_empty(), "native_calls":direct.observations(),
        "limitation":"Cancellation is synchronously set at the first real NativeModel UTF-8 callback before canonical replay/display filtering. Actual native sampler cleanup is observed on return. Does not prove interruption during prefill or desktop-worker freshness."})
}

pub fn run(request: &Value, emit: &mut dyn FnMut(Value) -> Result<()>) -> Result<Value> {
    let model = PathBuf::from(request["model"].as_str().context("Missing direct model")?);
    let round = request["round_index"]
        .as_u64()
        .context("Missing direct round")? as usize;
    let token_cap = u32::try_from(
        request["max_tokens"]
            .as_u64()
            .context("Missing direct token cap")?,
    )?;
    let words = u32::try_from(
        request["max_words"]
            .as_u64()
            .context("Missing direct word cap")?,
    )?;
    ensure!(
        (8..=256).contains(&token_cap) && (1..=32).contains(&words),
        "Invalid direct benchmark limits"
    );
    let mut config = Config {
        backend: "gguf".into(),
        model_path: model
            .to_str()
            .context("Non-Unicode direct model path")?
            .into(),
        prompt_style: request["prompt_style"]
            .as_str()
            .context("Missing prompt style")?
            .into(),
        acceleration: request["acceleration"]
            .as_str()
            .context("Missing acceleration")?
            .into(),
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
    let (_, all_cases) = corpus::corpus()?;
    let requested: Vec<String> = serde_json::from_value(request["cases"].clone())?;
    ensure!(
        !requested.is_empty()
            && requested
                .iter()
                .all(|id| all_cases.iter().any(|case| &case.id == id)),
        "Invalid direct case selection"
    );
    let cases: Vec<_> = all_cases
        .into_iter()
        .filter(|case| requested.contains(&case.id))
        .collect();
    let mut report = json!({"round":round + 1, "model_path":model, "cases":[], "cancellation":[], "close":{"status":"not_started"},
        "measurement_path":"direct_native_provider", "config":{"backend":"gguf", "prompt_style":config.prompt_style,
            "max_tokens":token_cap, "diagnostic_token_cap":token_cap, "production_validated_max_tokens":config.max_tokens, "context_chars":config.context_chars,
            "acceleration":config.acceleration, "record_history":false, "record_unaccepted":false,
            "clipboard_context":false, "screen_context":false, "screenshot_context":false, "statistics_enabled":false}});
    emit(json!({"phase":"prepare"}))?;
    let started = Instant::now();
    let mut direct =
        match DiagnosticSession::open(&model, &config.prompt_style, &config.acceleration) {
            Ok(direct) => direct,
            Err(error) => {
                report["error"] = json!(error.to_string());
                report["close"] = json!({"status":"not_applicable_provider_not_constructed"});
                report["process_lifetime_peak_rss_bytes"] =
                    json!(native_details::peak_rss_bytes()?);
                return Ok(report);
            }
        };
    report["cold_prepare_ms"] = json!(ms(started));
    let execution = (|| -> Result<()> {
        report["model"] = native_details::model_details(&model, &direct)?;
        emit(
            json!({"prepared":{"model":report["model"], "config":report["config"], "cold_prepare_ms":report["cold_prepare_ms"]}}),
        )?;
        let offset = round % cases.len();
        for case in cases[offset..].iter().chain(cases[..offset].iter()) {
            emit(json!({"phase":"case", "case":case.id}))?;
            let row = stream_case(
                &mut direct,
                case,
                if case.kind == "completion" {
                    token_cap
                } else {
                    128
                },
                words,
            )?;
            let failed = !row["error"].is_null();
            emit(json!({"case_result":row}))?;
            report["cases"].as_array_mut().expect("cases").push(row);
            ensure!(
                !failed,
                "Direct native case failed; partial observations retained"
            );
        }
        for pre_cancelled in [true, false] {
            emit(json!({"phase":"cancellation"}))?;
            let probe = cancellation(&mut direct, token_cap, pre_cancelled);
            emit(json!({"cancellation_result":probe}))?;
            report["cancellation"]
                .as_array_mut()
                .expect("probes")
                .push(probe);
        }
        Ok(())
    })();
    if let Err(error) = execution {
        report["error"] = json!(error.to_string());
    }
    emit(json!({"phase":"close"}))?;
    let close = Instant::now();
    drop(direct);
    report["close"] = json!({"status":"pass", "elapsed_ms":ms(close), "native_owner_dropped":true});
    report["process_lifetime_peak_rss_bytes"] = json!(native_details::peak_rss_bytes()?);
    Ok(report)
}
