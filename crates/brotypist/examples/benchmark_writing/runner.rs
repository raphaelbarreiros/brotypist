//! Owned canonical worker sessions; no desktop, saved settings, network or keys.
use super::{Arguments, corpus::Case, evaluation};
use anyhow::{Context, Result, bail, ensure};
use brotypist::{
    config::Config,
    diagnostics::TerminalReason,
    features::synonym_prompt,
    process_client::{WorkerClient, WorkerEvent},
    providers::GenerationOptions,
    writing::reference_context,
};
use serde_json::{Value, json};
use std::{
    path::Path,
    thread,
    time::{Duration, Instant},
};

const POLL: Duration = Duration::from_millis(2);
const CLOSE: Duration = Duration::from_secs(30);
fn ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}
fn check_deadline(deadline: Instant) -> Result<()> {
    ensure!(
        Instant::now() < deadline,
        "Native worker phase exceeded its deadline"
    );
    Ok(())
}
fn events(client: &mut WorkerClient, id: u64) -> Result<Vec<WorkerEvent>> {
    let events = client.poll();
    ensure!(
        events.iter().all(|event| event.id == id),
        "Worker returned an event for another request"
    );
    ensure!(
        !events.iter().any(|event| event.kind == "error"),
        "Native worker reported an inference error"
    );
    ensure!(
        client.status()["state"] != "error",
        "Native worker owner reported an error"
    );
    Ok(events)
}
fn prepare(client: &mut WorkerClient, config: &Config, timeout: Duration) -> Result<Value> {
    let started = Instant::now();
    let ticket = client.release()?;
    let deadline = started + timeout;
    while !client.release_complete(ticket) {
        check_deadline(deadline)?;
        thread::sleep(POLL);
    }
    let id = client.prepare_for_validation(config, ticket)?.get();
    let mut loading = false;
    let mut prepared = false;
    loop {
        for event in events(client, id)? {
            match event.kind.as_str() {
                "loading" => {
                    ensure!(!loading, "Worker repeated model loading");
                    loading = true;
                }
                "prepared" => {
                    ensure!(
                        loading && event.model_loaded && !prepared,
                        "Worker preparation lacked a loaded model"
                    );
                    prepared = true;
                }
                "done" => {
                    ensure!(
                        prepared && event.model_loaded,
                        "Worker preparation ended without a prepared acknowledgement"
                    );
                    return Ok(
                        json!({"elapsed_ms":ms(started), "loading_observed":loading, "prepared_acknowledged":true, "done_acknowledged":true, "worker_pid":client.status()["worker_pid"]}),
                    );
                }
                _ => bail!("Worker emitted writing during preparation"),
            }
        }
        check_deadline(deadline)?;
        thread::sleep(POLL);
    }
}
fn ready(client: &mut WorkerClient, id: u64, deadline: Instant) -> Result<()> {
    loop {
        let batch = events(client, id)?;
        let mut seen = false;
        for event in batch {
            match event.kind.as_str() {
                "ready" => {
                    ensure!(
                        !seen && event.model_loaded,
                        "Worker ready did not identify a loaded model"
                    );
                    seen = true;
                }
                "loading" => {}
                _ => bail!("Worker evaluated writing before explicit authorization"),
            }
        }
        if seen {
            return Ok(());
        }
        check_deadline(deadline)?;
        thread::sleep(POLL);
    }
}
fn options(case: &Case, max_words: u32) -> GenerationOptions {
    // The worker transport accepts 8000 context scalars. The oversized direct
    // reference fixture is intentionally bounded here, not rejected or faked.
    let context = if !case.context.is_empty() {
        case.context.chars().take(8000).collect()
    } else {
        reference_context(&case.prefix, &case.suffix, &case.reference_sources, 8000)
    };
    GenerationOptions {
        max_words,
        context,
        instructions: case.instructions.clone(),
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
    client: &mut WorkerClient,
    config: &Config,
    case: &Case,
    max_words: u32,
    timeout: Duration,
) -> Value {
    let options = options(case, max_words);
    let request_start = Instant::now();
    let deadline = request_start + timeout;
    let mut text = String::new();
    let mut updates = 0;
    let mut final_seen = false;
    let mut done = false;
    let mut authorized = None;
    let mut ready_ms = None;
    let mut first_chunk_ms = None;
    let mut first_displayable_ms = None;
    let mut total_ms = None;
    let result = (|| -> Result<()> {
        let id = client.submit(config, &case.prefix, &case.suffix, &options)?;
        ready(client, id, deadline)?;
        ready_ms = Some(ms(request_start));
        let started = Instant::now();
        authorized = Some(started);
        client.proceed(id)?;
        while !done {
            for event in events(client, id)? {
                match event.kind.as_str() {
                    "result" => {
                        ensure!(
                            !final_seen,
                            "Worker returned output after its final acknowledgement"
                        );
                        if first_chunk_ms.is_none() {
                            first_chunk_ms = Some(ms(started));
                        }
                        if case.kind == "completion" {
                            ensure!(
                                event.text.starts_with(&text),
                                "Worker rewrote an already displayed cumulative insertion"
                            );
                        }
                        text = event.text;
                        if first_displayable_ms.is_none() && evaluation::displayable(case, &text) {
                            first_displayable_ms = Some(ms(started));
                        }
                        updates += 1;
                        final_seen = event.final_;
                    }
                    "done" => {
                        ensure!(
                            final_seen && event.model_loaded,
                            "Worker ended without final result and loaded model acknowledgements"
                        );
                        done = true;
                        total_ms = Some(ms(started));
                    }
                    _ => bail!("Worker emitted an unexpected generation event"),
                }
            }
            if !done {
                check_deadline(deadline)?;
                thread::sleep(POLL);
            }
        }
        Ok(())
    })();
    let error = result.as_ref().err().map(ToString::to_string);
    let mechanical = evaluation::mechanical(case, &text, done && final_seen, error.is_some());
    let semantic = evaluation::semantic(case, &text);
    // Embedded regexes are validated before any worker is created; failures
    // still surface explicitly rather than supplying invented review evidence.
    let semantic_error = semantic.as_ref().err().map(ToString::to_string);
    json!({"case":case.id, "kind":case.kind, "generated_text":text,
        "assembled_draft":if case.kind == "completion" { Some(format!("{}{}{}", case.prefix, text, case.suffix)) } else { None },
        "reference_context_used":options.context,
        "reference_context_truncated_for_worker":case.context.chars().count() > 8000,
        "requested_limits":{"max_tokens":config.max_tokens, "max_words":max_words},
        "submit_to_ready_ms":ready_ms, "time_to_first_worker_result_ms":first_chunk_ms,
        "time_to_first_displayable_text_ms":first_displayable_ms, "total_stream_ms":total_ms,
        "request_observed_ms":ms(request_start), "authorized_stream_observed_ms":authorized.map(ms),
        "stream_updates":updates, "protocol_completed":done && final_seen, "error":error,
        "native_calls":null, "mechanical":mechanical,
        "semantic_evaluation":semantic.ok(), "semantic_evaluation_error":semantic_error,
        "process_lifetime_peak_rss_bytes_after_case":null})
}
fn wait_inactive(client: &mut WorkerClient, id: u64, deadline: Instant) -> Result<Value> {
    loop {
        check_deadline(deadline)?;
        let status = client.status();
        if status["latest_id"]
            .as_u64()
            .is_some_and(|latest| latest > id)
            && status["active_id"].is_null()
            && !status["initializing"].as_bool().unwrap_or(true)
            && !status["evaluating"].as_bool().unwrap_or(true)
            && !status["awaiting_proceed"].as_bool().unwrap_or(true)
            && !status["terminating"].as_bool().unwrap_or(true)
        {
            return Ok(status);
        }
        thread::sleep(POLL);
    }
}
// One fixed ordinary-writing workload, not a quality fixture or a retry pool.
// Only this probe's per-request token/word limits differ from ordinary cases.
const CANCELLATION_PREFIX: &str = "Our neighborhood garden team is preparing a handover note for new volunteers. The seedlings are ready, the tools are in the shed, and the planting beds still need attention. During Saturday's planting session, the volunteers should";
const CANCELLATION_INSTRUCTIONS: &str = "Continue this practical handover note with a detailed sentence of about thirty words explaining how to prepare the beds, plant the seedlings, water them, and leave the shared tools ready for the next team. Use plain prose, not a list or a summary.";

fn counter_delta(before: &Value, after: &Value) -> Option<u64> {
    after.as_u64()?.checked_sub(before.as_u64()?)
}

fn cancellation(
    client: &mut WorkerClient,
    config: &Config,
    before_proceed: bool,
    timeout: Duration,
) -> Value {
    let started = Instant::now();
    let deadline = started + timeout;
    let mut effective = config.clone();
    if !before_proceed {
        effective.max_tokens = 128;
    }
    let prefix = if before_proceed {
        "The next step in the project is to"
    } else {
        CANCELLATION_PREFIX
    };
    let options = GenerationOptions {
        max_words: if before_proceed { 8 } else { 32 },
        instructions: if before_proceed {
            String::new()
        } else {
            CANCELLATION_INSTRUCTIONS.into()
        },
        ..GenerationOptions::default()
    };
    // Separate prior requests from this probe without consulting last_request,
    // which may belong to a different request or be absent after retirement.
    let prior_report_ids: Vec<_> = std::iter::from_fn(|| client.take_terminal_report())
        .map(|report| report.request_id)
        .collect();
    let accounting_before = client.status();
    let mut validated = false;
    let mut request_id = None;
    let mut ready_ms = None;
    let mut proceed_requested_ms = None;
    let mut cancelled = None;
    let mut observed = 0;
    let mut nonfinal_updates = 0;
    let mut nonempty_nonfinal_updates = 0;
    let mut first_nonfinal_ms = None;
    let mut final_seen = false;
    let mut done_seen = false;
    let mut last_text = String::new();
    let mut window_observed = false;
    let mut window_missed = None;
    let mut quiescent = None;
    let mut quiescent_ms = None;
    let mut outputs_after_cancel = None;
    let mut status_at_cancel = Value::Null;
    let observation = (|| -> Result<()> {
        effective.validate()?;
        validated = true;
        let id = client.submit(&effective, prefix, "", &options)?;
        request_id = Some(id);
        ready(client, id, deadline)?;
        ready_ms = Some(ms(started));
        if !before_proceed {
            proceed_requested_ms = Some(ms(started));
            client.proceed(id)?;
            loop {
                // Inspect the entire batch: a non-final result followed by
                // final/done in the same poll is already a missed window.
                for event in events(client, id)? {
                    match event.kind.as_str() {
                        "result" => {
                            ensure!(
                                !final_seen && !done_seen,
                                "Worker returned output after its final acknowledgement"
                            );
                            observed += 1;
                            if !event.final_ {
                                nonfinal_updates += 1;
                                first_nonfinal_ms.get_or_insert_with(|| ms(started));
                                if !event.text.trim().is_empty() {
                                    nonempty_nonfinal_updates += 1;
                                }
                            }
                            final_seen = event.final_;
                            last_text = event.text;
                        }
                        "done" => done_seen = true,
                        _ => bail!("Worker cancellation probe returned an unexpected event"),
                    }
                }
                if final_seen || done_seen {
                    window_missed = Some(if final_seen && last_text.trim().is_empty() {
                        "final_empty_result_not_a_streaming_window"
                    } else {
                        "final_or_done_observed_before_cancel"
                    });
                    break;
                }
                if client.status()["active_id"] != id {
                    window_missed = Some("request_inactive_before_cancel");
                    break;
                }
                if Instant::now() >= deadline {
                    window_missed = Some("no_active_nonempty_nonfinal_result_before_deadline");
                    break;
                }
                if nonempty_nonfinal_updates > 0 {
                    window_observed = true;
                    break;
                }
                thread::sleep(POLL);
            }
        }
        Ok(())
    })();
    // Cleanup is attempted even after observation/protocol failure. It retains
    // the original request deadline; session close still runs independently.
    let cleanup = (|| -> Result<()> {
        let Some(id) = request_id else {
            return Ok(());
        };
        status_at_cancel = client.status();
        if window_observed && status_at_cancel["active_id"] != id {
            window_observed = false;
            window_missed = Some("request_became_inactive_between_observation_and_cancel");
        }
        cancelled = Some(Instant::now());
        client.cancel()?;
        quiescent = Some(wait_inactive(client, id, deadline)?);
        quiescent_ms = Some(ms(started));
        let remaining = client.poll();
        outputs_after_cancel = Some(remaining.len());
        ensure!(
            remaining.is_empty(),
            "Cancelled request retained consumer-visible results"
        );
        Ok(())
    })();
    let reports: Vec<_> = std::iter::from_fn(|| client.take_terminal_report()).collect();
    let mut matching = reports
        .iter()
        .filter(|report| Some(report.request_id) == request_id);
    let terminal = matching.next();
    let matching_count = usize::from(terminal.is_some()) + matching.count();
    let terminal = terminal.filter(|_| matching_count == 1);
    let accounting_after = client.status();
    let submitted_delta = counter_delta(
        &accounting_before["inference"]["submitted_requests"],
        &accounting_after["inference"]["submitted_requests"],
    );
    let measured_delta = counter_delta(
        &accounting_before["inference"]["measured_requests"],
        &accounting_after["inference"]["measured_requests"],
    );
    let finished_delta = counter_delta(
        &accounting_before["inference"]["finished_requests"],
        &accounting_after["inference"]["finished_requests"],
    );
    let unreported_delta = counter_delta(
        &accounting_before["inference"]["unreported_requests"],
        &accounting_after["inference"]["unreported_requests"],
    );
    let lost_delta = counter_delta(
        &accounting_before["lost_terminal_reports"],
        &accounting_after["lost_terminal_reports"],
    );
    let checks = (|| -> Result<()> {
        ensure!(
            matching_count <= 1,
            "Worker repeated a request terminal report"
        );
        ensure!(
            accounting_after["state"] != "error",
            "Worker owner reported an error during cancellation"
        );
        if let Some(report) = terminal {
            ensure!(!report.failed, "Worker cancellation request failed");
            if before_proceed {
                ensure!(
                    report.proceed.is_none() && report.first_result.is_none(),
                    "Before-proceed cancellation unexpectedly dispatched writing"
                );
            }
        }
        Ok(())
    })();
    let error = observation
        .as_ref()
        .err()
        .or_else(|| cleanup.as_ref().err())
        .or_else(|| checks.as_ref().err())
        .map(ToString::to_string);
    let terminal_preceded_cancel = terminal
        .zip(cancelled)
        .map(|(report, cancel)| report.terminal <= cancel);
    let status = if error.is_some() {
        "fail"
    } else if !before_proceed && (!window_observed || final_seen || done_seen) {
        "inconclusive_no_active_streaming_window"
    } else if terminal_preceded_cancel == Some(true)
        || terminal.is_some_and(|report| report.reason == TerminalReason::Completed)
    {
        "inconclusive_request_already_completed"
    } else if !before_proceed
        && terminal.is_some_and(|report| report.metrics.is_some_and(|metrics| metrics.no_offer))
    {
        "inconclusive_no_offer"
    } else if terminal.is_none()
        || submitted_delta != Some(1)
        || measured_delta != Some(u64::from(!before_proceed))
        || finished_delta != Some(1)
        || unreported_delta != Some(0)
        || lost_delta != Some(0)
        || terminal.is_some_and(|report| {
            report.reason != TerminalReason::Cancelled
                || !report.cancelled
                || report.terminal > deadline
                || (!before_proceed && (report.first_result.is_none() || report.metrics.is_none()))
        })
    {
        "inconclusive_terminal_evidence_unavailable"
    } else {
        "pass"
    };
    // TerminalSnapshot retains all native prompt/generation submitted/completed,
    // sample and reuse counters. Missing metrics stay null, never zero work.
    let mut report = json!({
        "kind":if before_proceed { "before_worker_proceed" } else { "after_first_observed_worker_result" },
        "probe_version":"canonical_cancellation_v2", "status":status, "request_id":request_id,
        "workload_version":if before_proceed { "before_proceed_v1" } else { "ordinary_garden_handover_v1" },
        "runner_package_version":env!("CARGO_PKG_VERSION"), "worker_build_version":null, "seed":null,
        "config":effective, "config_validated":validated,
        "input":{"prefix":prefix, "suffix":"", "max_words":options.max_words,
            "instructions":options.instructions, "context":options.context, "personalization":options.personalization,
            "utility":options.utility, "anchor":options.anchor, "alternatives":options.alternatives},
        "submit_to_ready_ms":ready_ms, "proceed_requested_ms":proceed_requested_ms,
        "first_nonfinal_result_observed_ms":first_nonfinal_ms,
        "active_nonempty_nonfinal_window_observed":window_observed, "missed_window_reason":window_missed,
        "updates_before_cancel":observed, "nonfinal_updates_before_cancel":nonfinal_updates,
        "nonempty_nonfinal_updates_before_cancel":nonempty_nonfinal_updates,
        "final_result_observed_before_cancel":final_seen, "done_observed_before_cancel":done_seen,
        "last_text_before_cancel":last_text,
    });
    report["cancel_requested"] = json!(cancelled.is_some());
    report["cancel_purpose"] = json!(if before_proceed || window_observed {
        "probe_trigger"
    } else {
        "cleanup_only"
    });
    report["cancel_request_ms"] =
        json!(cancelled.map(|time| time.duration_since(started).as_secs_f64() * 1000.0));
    report["quiescence_acknowledged_ms"] = json!(quiescent_ms);
    report["return_after_cancel_ms"] = json!(cancelled.map(ms));
    report["total_ms"] = json!(ms(started));
    report["outputs_after_cancel"] = json!(outputs_after_cancel);
    report["status_at_cancel"] = status_at_cancel;
    report["quiescent_status"] = json!(quiescent);
    report["error"] = json!(error);
    report["observation_error"] = json!(observation.err().map(|error| error.to_string()));
    report["cleanup_error"] = json!(cleanup.err().map(|error| error.to_string()));
    report["contract_error"] = json!(checks.err().map(|error| error.to_string()));
    report["terminal_report"] = json!(terminal.map(|report| report.snapshot()));
    report["terminal_report_count"] = json!(matching_count);
    report["terminal_submitted_ms"] = json!(
        terminal
            .and_then(|report| report.submitted.checked_duration_since(started))
            .map(|duration| duration.as_secs_f64() * 1000.0)
    );
    report["terminal_preceded_cancel"] = json!(terminal_preceded_cancel);
    report["prior_terminal_report_ids_drained"] = json!(prior_report_ids);
    report["unmatched_terminal_reports"] = json!(
        reports
            .iter()
            .filter(|report| Some(report.request_id) != request_id)
            .map(|report| report.snapshot())
            .collect::<Vec<_>>()
    );
    report["accounting_before"] = accounting_before;
    report["accounting_after"] = accounting_after;
    report["submitted_requests_delta"] = json!(submitted_delta);
    report["measured_requests_delta"] = json!(measured_delta);
    report["finished_requests_delta"] = json!(finished_delta);
    report["unreported_requests_delta"] = json!(unreported_delta);
    report["lost_terminal_reports_delta"] = json!(lost_delta);
    report["native_cancellation_acknowledged"] = Value::Null;
    report["native_interruption_proven"] = Value::Null;
    report["pass_contract"] = json!(
        "Observed trigger, supervisor revocation, matching nonfailed terminal report, worker terminal accounting, acknowledged quiescence, and no consumer-visible events after cancellation. Session release/shutdown is reported separately in close and remains required by passes()."
    );
    report["limitation"] = json!(
        "One fixed attempt per session on the same sequential WorkerClient/model owner; no retry selection. Seed and worker build identity are not exposed here. Model/process allocation identity is not proven by a PID. A non-final IPC result with matching active_id is a supervisor observation, not proof the native producer is still executing. A completion racing after that observation may be filtered: the native race is unobservable. poll() suppresses revoked results, so silence alone is not native cancellation acknowledgement. TerminalReport and metrics.cancelled merge supervisor cancellation; native acknowledgement remains unknown. evaluating/proceed are supervisor dispatch, not native prefill. Terminal offsets are supervisor observations relative to submission. Native work counters, including under-cap counts, do not identify an interrupted kernel or phase. Missing reports/metrics remain unknown; before-proceed never authorizes writing. No desktop freshness claim."
    );
    report
}
fn close(client: &mut WorkerClient) -> Value {
    let started = Instant::now();
    let release = (|| -> Result<Value> {
        let ticket = client.release()?;
        let deadline = Instant::now() + CLOSE;
        loop {
            let status = client.status();
            if client.release_complete(ticket)
                && status["worker_pid"].is_null()
                && status["model_loaded"] == false
            {
                return Ok(status);
            }
            check_deadline(deadline)?;
            thread::sleep(POLL);
        }
    })();
    let release_ms = ms(started);
    // Attempt shutdown even if release did not acknowledge; never abandon an
    // owned supervisor because a request or cleanup assertion failed.
    let stopping = Instant::now();
    let shutdown = (|| -> Result<Value> {
        client.request_stop()?;
        let deadline = Instant::now() + CLOSE;
        while !client.is_stopped() {
            check_deadline(deadline)?;
            thread::sleep(POLL);
        }
        let status = client.status();
        ensure!(
            status["owner_finished"] == true && status["worker_pid"].is_null(),
            "Worker owner did not finish shutdown"
        );
        Ok(status)
    })();
    json!({"status":if release.is_ok() && shutdown.is_ok() { "pass" } else { "fail" }, "elapsed_ms":ms(started),
        "release_ms":release_ms, "release_status":release.as_ref().ok(), "release_error":release.as_ref().err().map(ToString::to_string),
        "shutdown_ms":ms(stopping), "shutdown_status":shutdown.as_ref().ok(), "shutdown_error":shutdown.as_ref().err().map(ToString::to_string)})
}

pub fn session(
    args: &Arguments,
    app: &Path,
    model: &Path,
    round: usize,
    cases: &[Case],
    tokens: u32,
    words: u32,
) -> Value {
    let mut result = json!({"round":round + 1, "model_path":model, "cases":[], "cancellation":[], "close":{"status":"not_started"},
        "process_lifetime_peak_rss_bytes":null,
        "process":{"exitcode":null, "terminated_by_watchdog":null, "exitcode_observation":"WorkerClient acknowledges owned release and shutdown, not raw child wait status; see close."}});
    let mut client = match WorkerClient::for_executable(app.to_path_buf()) {
        Ok(client) => client,
        Err(error) => {
            result["error"] = json!(error.to_string());
            return result;
        }
    };
    let execution = (|| -> Result<()> {
        let mut config = Config {
            backend: "gguf".into(),
            model_path: model.to_str().context("Model path must be Unicode")?.into(),
            prompt_style: args.prompt_style.clone(),
            acceleration: args.acceleration.clone(),
            max_tokens: tokens,
            completion_length: args.completion_length.clone(),
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
        result["config"] = json!({"backend":config.backend, "prompt_style":config.prompt_style, "max_tokens":tokens,
            "context_chars":config.context_chars, "acceleration":config.acceleration,
            "record_history":false, "record_unaccepted":false, "clipboard_context":false, "screen_context":false,
            "screenshot_context":false, "statistics_enabled":false, "personalization_strength":0, "idle_release_seconds":0});
        let prepared = prepare(
            &mut client,
            &config,
            Duration::from_secs(args.prepare_timeout_seconds),
        )?;
        result["cold_prepare_ms"] = prepared["elapsed_ms"].clone();
        result["preparation"] = prepared;
        let offset = round % cases.len();
        for case in cases[offset..].iter().chain(cases[..offset].iter()) {
            let mut effective = config.clone();
            if case.kind != "completion" {
                effective.max_tokens = 128;
            }
            let row = stream_case(
                &mut client,
                &effective,
                case,
                words,
                Duration::from_secs(args.case_timeout_seconds),
            );
            let failed = !row["error"].is_null() || !row["semantic_evaluation_error"].is_null();
            result["cases"]
                .as_array_mut()
                .expect("case array")
                .push(row);
            ensure!(
                !failed,
                "Benchmark case failed; partial observations retained"
            );
        }
        for before_proceed in [true, false] {
            let probe = cancellation(
                &mut client,
                &config,
                before_proceed,
                Duration::from_secs(args.case_timeout_seconds),
            );
            let failed = !probe["error"].is_null();
            result["cancellation"]
                .as_array_mut()
                .expect("cancellation array")
                .push(probe);
            ensure!(!failed, "Native cancellation probe failed");
        }
        Ok(())
    })();
    if let Err(error) = execution {
        result["error"] = json!(error.to_string());
    }
    result["close"] = close(&mut client);
    // Cancellation may time out before a terminal report arrives. Preserve late
    // reports after release/shutdown, without upgrading an earlier failed or
    // inconclusive probe or conflating shutdown with the cancellation deadline.
    result["terminal_reports_after_close"] = json!(
        std::iter::from_fn(|| client.take_terminal_report())
            .map(|report| report.snapshot())
            .collect::<Vec<_>>()
    );
    result["status_after_close"] = client.status();
    result
}

pub fn passes(session: &Value, count: usize) -> bool {
    session["error"].is_null()
        && session["close"]["status"] == "pass"
        && session["cases"].as_array().is_some_and(|rows| {
            rows.len() == count && rows.iter().all(|row| row["mechanical"]["pass"] == true)
        })
        && session["cancellation"].as_array().is_some_and(|probes| {
            probes.len() == 2 && probes.iter().all(|probe| probe["status"] == "pass")
        })
}
