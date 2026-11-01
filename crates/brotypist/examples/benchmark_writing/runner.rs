//! Owned canonical worker sessions; no desktop, saved settings, network or keys.
use super::{Arguments, corpus::Case, evaluation};
use anyhow::{Context, Result, bail, ensure};
use brotypist::{
    config::Config,
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
    let id = client.prepare(config)?;
    ensure!(id != 0, "Fresh worker did not accept model preparation");
    let deadline = started + timeout;
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
fn wait_inactive(client: &mut WorkerClient, timeout: Duration) -> Result<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let status = client.status();
        if status["active_id"].is_null()
            && !status["initializing"].as_bool().unwrap_or(true)
            && !status["evaluating"].as_bool().unwrap_or(true)
            && !status["awaiting_proceed"].as_bool().unwrap_or(true)
            && !status["terminating"].as_bool().unwrap_or(true)
        {
            return Ok(status);
        }
        check_deadline(deadline)?;
        thread::sleep(POLL);
    }
}
fn cancellation(
    client: &mut WorkerClient,
    config: &Config,
    before_proceed: bool,
    timeout: Duration,
) -> Value {
    let started = Instant::now();
    let deadline = started + timeout;
    let mut cancelled = None;
    let mut observed = 0;
    let mut completed_before_cancel = false;
    let mut quiescent = None;
    let mut status_at_cancel = Value::Null;
    let result = (|| -> Result<()> {
        let id = client.submit(
            config,
            "The next step in the project is to",
            "",
            &GenerationOptions {
                max_words: 8,
                ..GenerationOptions::default()
            },
        )?;
        ready(client, id, deadline)?;
        if !before_proceed {
            client.proceed(id)?;
            'observe: loop {
                let batch = events(client, id)?;
                let mut first_result = false;
                let mut done = false;
                for event in batch {
                    match event.kind.as_str() {
                        "result" => {
                            observed += 1;
                            first_result = true;
                        }
                        "done" => done = true,
                        _ => bail!("Worker cancellation probe returned an unexpected event"),
                    }
                }
                if first_result {
                    completed_before_cancel = done || client.status()["active_id"] != id;
                    break 'observe;
                }
                if done {
                    completed_before_cancel = true;
                    break 'observe;
                }
                check_deadline(deadline)?;
                thread::sleep(POLL);
            }
        }
        status_at_cancel = client.status();
        cancelled = Some(Instant::now());
        client.cancel()?;
        quiescent = Some(wait_inactive(
            client,
            deadline.saturating_duration_since(Instant::now()),
        )?);
        ensure!(
            client.poll().is_empty(),
            "Cancelled request retained consumer-visible results"
        );
        Ok(())
    })();
    json!({"kind":if before_proceed { "before_worker_proceed" } else { "after_first_observed_worker_result" },
        "status":if result.is_err() { "fail" } else if completed_before_cancel { "inconclusive_request_already_completed" } else { "pass" },
        "cancel_requested":cancelled.is_some(), "cancel_request_ms":cancelled.map(|time| time.duration_since(started).as_secs_f64() * 1000.0),
        "return_after_cancel_ms":cancelled.map(ms), "total_ms":ms(started), "updates_before_cancel":observed,
        "outputs_after_cancel":if result.is_ok() { Some(0) } else { None },
        "error":result.err().map(|error| error.to_string()), "status_at_cancel":status_at_cancel, "quiescent_status":quiescent,
        "limitation":"Real canonical WorkerClient cancellation. Before-proceed never authorizes generation; after-result observes IPC, not a synchronous raw native chunk. Producer may finish before observation (explicitly inconclusive). Does not prove interruption during prefill or desktop freshness."})
}
fn close(client: &mut WorkerClient) -> Value {
    let started = Instant::now();
    let release = (|| -> Result<Value> {
        client.release()?;
        let deadline = Instant::now() + CLOSE;
        loop {
            let status = client.status();
            if status["release_complete"] == true
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
