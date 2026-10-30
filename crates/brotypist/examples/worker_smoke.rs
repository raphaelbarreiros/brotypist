//! Real native worker/public-CLI QA. Never downloads models or touches the normal profile.
//! Run with an isolated INKLOW_HOME and a separately downloaded `tiny` model.
use anyhow::{Context, Result, bail, ensure};
use brotypist::{config::{Config, save_config_to}, core::{Snapshot, SuggestionState}, models,
    process_client::WorkerClient, providers::{GenerationOptions, MAX_SUGGESTION_CHARS, clean_completion}};
use clap::Parser;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, io::{Read, Write}, path::{Path, PathBuf}, process::{Child, Command, Stdio},
    sync::mpsc, thread::{self, JoinHandle}, time::{Duration, Instant}};

const PREFIX: &str = "The capital of France is";
const HEALING_PREFIX: &str = "The capital of France is Par";
const SUFFIX: &str = ". This city is in Europe.";
const HOLD: Duration = Duration::from_millis(400);
const PIPE_LIMIT: usize = 256 * 1024;

#[derive(Parser)]
struct Arguments {
    /// Actual native BroTypist-cli binary (not a script or this example).
    #[arg(long)] app: PathBuf,
    #[arg(long)] output: PathBuf,
    #[arg(long, default_value = "tiny")] preset: String,
    /// Optional already-cached file; it must match the selected catalogue digest.
    #[arg(long)] model_path: Option<PathBuf>,
    /// Per-stage deadline, including native preparation/evaluation.
    #[arg(long, default_value_t = 180, value_parser = clap::value_parser!(u64).range(5..=1800))]
    timeout_seconds: u64,
}

fn main() {
    let arguments = Arguments::parse();
    let mut receipt = json!({
        "schema": 1, "tool": "worker_smoke", "passed": false,
        "preset": arguments.preset, "observations": [],
        "scope": "real native inference, worker authorization/cancellation/lifecycle, public complete CLI; no desktop permissions or installed application",
    });
    let started = Instant::now();
    let result = run(&arguments, &mut receipt);
    receipt["elapsed_ms"] = json!(started.elapsed().as_millis());
    receipt["passed"] = json!(result.is_ok());
    if let Err(error) = &result {
        // All smoke assertions/errors have fixed wording, never backend text or authored output.
        receipt["failure"] = json!(error.to_string());
    }
    let written = fs::create_dir_all(&arguments.output).and_then(|()| {
        fs::write(arguments.output.join("worker-smoke.json"), serde_json::to_vec_pretty(&receipt).expect("receipt serializes"))
    });
    if written.is_err() { eprintln!("Cannot write worker smoke receipt"); std::process::exit(1); }
    if result.is_err() { eprintln!("Worker smoke failed; see worker-smoke.json"); std::process::exit(1); }
    println!("Worker and public CLI smoke passed; receipt: {}", arguments.output.join("worker-smoke.json").display());
}

fn record(receipt: &mut Value, value: Value) {
    receipt["observations"].as_array_mut().expect("observations array").push(value);
}

fn run(args: &Arguments, receipt: &mut Value) -> Result<()> {
    let root = std::env::var_os("INKLOW_HOME").filter(|value| !value.is_empty())
        .map(PathBuf::from).context("Set INKLOW_HOME to an isolated disposable directory")?;
    ensure!(root.is_absolute(), "INKLOW_HOME must be an absolute disposable directory");
    fs::create_dir_all(&root).context("Cannot create isolated smoke root")?;
    let root = root.canonicalize().context("Cannot resolve isolated smoke root")?;
    ensure_isolated(&root)?;
    let profile = tempfile::Builder::new().prefix("worker-smoke-").tempdir_in(&root)
        .context("Cannot create private disposable smoke profile")?;
    let app = args.app.canonicalize().context("Cannot resolve actual native CLI binary")?;
    ensure!(app.is_file(), "The app must be an existing native CLI binary");
    ensure!(app != std::env::current_exe().context("Cannot resolve smoke executable")?, "The app cannot be the smoke example");
    ensure_native_executable(&app)?;
    let preset = models::get_preset(&args.preset).context("Unknown smoke model preset")?;
    let model = args.model_path.clone().map(Ok).unwrap_or_else(|| models::model_path(preset))
        .context("Cannot resolve separately cached model")?;
    let model = model.canonicalize().context("Cached model is missing; download it in a separate step")?;
    verify_model(&model, preset)?;
    let mut config = Config {
        backend: "gguf".into(), model_path: model.to_str().context("Cached model path must be Unicode")?.into(),
        model_id: preset.id.clone(), prompt_style: preset.prompt_style.clone(), acceleration: "cpu".into(),
        max_tokens: 48, context_chars: 512, idle_release_seconds: 0,
        launch_at_login: false, check_updates_automatically: false, clipboard_context: false,
        screen_context: false, screenshot_context: false, record_history: false, record_unaccepted: false,
        statistics_enabled: false, personalization_strength: 0, ..Config::default()
    };
    config.validate().context("Invalid smoke configuration")?;
    save_config_to(&config, &profile.path().join("settings.json")).context("Cannot write disposable CLI configuration")?;
    record(receipt, json!({"check":"isolation_and_verified_cache", "passed":true,
        "model_sha256":preset.sha256, "model_bytes":preset.size_bytes, "acceleration":"cpu",
        "prompt_style":config.prompt_style, "raw_authored_text_persisted":false,
        "sampler":"actual production sampler; deterministic initial seed and seed evolution, not greedy",
        "worker_transport":"canonical WorkerClient: bounded JSON pipes, owned supervisor and child cleanup",
        "worker_stderr":"canonical private worker intentionally suppresses diagnostics; public CLI stderr sanitized separately"}));
    let timeout = Duration::from_secs(args.timeout_seconds);
    let mut client = WorkerClient::for_executable(app.clone()).context("Cannot construct canonical worker owner")?;
    // Even a failed inference assertion cannot skip acknowledged owner shutdown.
    let worker_result = exercise_worker(&mut client, &config, timeout, receipt);
    let shutdown = shutdown(&mut client, timeout);
    record(receipt, json!({"check":"worker_shutdown_acknowledged", "passed":shutdown.is_ok(), "status":client.status(),
        "failure":shutdown.as_ref().err().map(ToString::to_string)}));
    worker_result?;
    shutdown?;
    exercise_cli(&app, profile.path(), &config, timeout, &args.output, receipt)?;
    Ok(())
}

fn ensure_isolated(root: &Path) -> Result<()> {
    // Never accept the normal platform profile, a parent of it, or a profile
    // containing normal history/preferences. The created child profile is owned
    // by TempDir and removed on every ordinary exit.
    if let Some(home) = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) {
        let home = PathBuf::from(home);
        let candidates = [home.join("Library/Application Support/inklow"), home.join(".config/inklow"), home.join("AppData/Local/inklow")];
        for candidate in candidates {
            let normal = candidate.canonicalize().unwrap_or(candidate);
            ensure!(root != normal && !normal.starts_with(root) && !root.starts_with(&normal), "INKLOW_HOME must not overlap the normal profile");
        }
        if let Ok(home) = home.canonicalize() { ensure!(root != home, "INKLOW_HOME cannot be the user home"); }
    }
    for name in ["settings.json", "history.db", "history.sqlite3", "credentials.json"] {
        ensure!(!root.join(name).exists(), "INKLOW_HOME contains existing profile data; use a fresh disposable root");
    }
    Ok(())
}

fn ensure_native_executable(app: &Path) -> Result<()> {
    let mut header = [0u8; 4];
    fs::File::open(app).and_then(|mut file| file.read_exact(&mut header)).context("Cannot read native app header")?;
    ensure!(header == *b"\x7fELF" || header[..2] == *b"MZ" || matches!(header,
        [0xcf,0xfa,0xed,0xfe] | [0xce,0xfa,0xed,0xfe] | [0xfe,0xed,0xfa,0xcf] |
        [0xfe,0xed,0xfa,0xce] | [0xca,0xfe,0xba,0xbe] | [0xbe,0xba,0xfe,0xca]),
        "The app must have an ELF, Mach-O or PE executable header, not a script");
    Ok(())
}

fn verify_model(path: &Path, preset: &models::ModelPreset) -> Result<()> {
    let mut file = fs::File::open(path).context("Cannot open separately cached model")?;
    ensure!(file.metadata().context("Cannot inspect cached model")?.len() == preset.size_bytes, "Cached model size differs from pinned catalogue");
    let mut hash = Sha256::new();
    let mut bytes = [0u8; 64 * 1024];
    loop {
        let length = file.read(&mut bytes).context("Cannot read cached model")?;
        if length == 0 { break; }
        hash.update(&bytes[..length]);
    }
    ensure!(format!("{:x}", hash.finalize()) == preset.sha256, "Cached model digest differs from pinned catalogue");
    Ok(())
}

fn wait_status(client: &mut WorkerClient, timeout: Duration, predicate: impl Fn(&Value) -> bool, failure: &'static str) -> Result<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let status = client.status();
        if predicate(&status) { return Ok(status); }
        ensure!(status["state"] != "error", "Canonical worker reported an inference error");
        ensure!(Instant::now() < deadline, "{failure}");
        thread::sleep(Duration::from_millis(5));
    }
}

fn ready(client: &mut WorkerClient, id: u64, timeout: Duration, require_loading: bool) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let mut loading = false;
    loop {
        for event in client.poll() {
            ensure!(event.id == id, "Worker returned an event for a different request");
            match event.kind.as_str() {
                "loading" => { ensure!(!loading, "Worker repeated model loading"); loading = true; }
                "ready" => {
                    ensure!(!require_loading || loading, "Worker omitted the loading-to-ready barrier");
                    ensure!(event.model_loaded, "Worker became ready without a loaded model");
                    return Ok(());
                }
                "result" | "done" => bail!("Worker evaluated writing before explicit proceed"),
                "error" => bail!("Worker reported an inference error before ready"),
                _ => bail!("Worker emitted an unexpected pre-ready event"),
            }
        }
        ensure!(Instant::now() < deadline, "Worker did not reach ready before deadline");
        thread::sleep(Duration::from_millis(5));
    }
}

fn barrier(client: &mut WorkerClient, id: u64) -> Result<()> {
    let deadline = Instant::now() + HOLD;
    while Instant::now() < deadline {
        ensure!(client.poll().is_empty(), "Worker emitted output while explicit proceed was withheld");
        let state = client.status();
        ensure!(state["awaiting_proceed"] == true && state["evaluating"] == false && state["active_id"] == id,
            "Worker did not preserve its ready authorization barrier");
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

fn exercise_worker(client: &mut WorkerClient, config: &Config, timeout: Duration, receipt: &mut Value) -> Result<()> {
    let options = GenerationOptions { max_words: 8, ..GenerationOptions::default() };
    let guided_healing = config.prompt_style == "writing";
    let suffix_prefix = if guided_healing { HEALING_PREFIX } else { PREFIX };
    for (index, (prefix, suffix, healing)) in [(PREFIX, "", false), (suffix_prefix, SUFFIX, guided_healing)].into_iter().enumerate() {
        let id = client.submit(config, prefix, suffix, &options).context("Cannot submit actual worker writing")?;
        ready(client, id, timeout, index == 0)?;
        barrier(client, id)?;
        record(receipt, json!({"check":"explicit_proceed_barrier", "request":id, "passed":true, "withheld_ms":HOLD.as_millis(), "loading_observed":index == 0}));
        client.proceed(id).context("Cannot authorize worker writing")?;
        let deadline = Instant::now() + timeout;
        let mut previous = String::new();
        let mut final_seen = false;
        let mut results = 0usize;
        let mut done = false;
        while !done {
            for event in client.poll() {
                ensure!(event.id == id, "Worker response request identity changed");
                match event.kind.as_str() {
                    "result" => {
                        ensure!(!final_seen, "Worker emitted a result after final acknowledgement");
                        validate_text(&event.text, prefix, suffix, true)?;
                        ensure!(event.text.starts_with(&previous), "Worker cumulative updates rewrote an already visible UTF-8 prefix");
                        previous = event.text;
                        results += 1;
                        final_seen = event.final_;
                    }
                    "done" => { ensure!(final_seen && results > 0 && event.model_loaded, "Worker done lacked valid final writing/model acknowledgement"); done = true; }
                    "error" => bail!("Worker reported an inference error during writing"),
                    _ => bail!("Worker emitted an unexpected post-proceed event"),
                }
            }
            ensure!(Instant::now() < deadline, "Worker writing did not finish before deadline");
            if !done { thread::sleep(Duration::from_millis(5)); }
        }
        validate_boundary(&previous, healing)?;
        validate_consumer(prefix, suffix, &previous)?;
        record(receipt, json!({"check":if healing {"worker_partial_word_and_suffix"} else if suffix.is_empty() {"worker_writing"} else {"worker_suffix"},
            "passed":true, "request":id, "result_frames":results, "final_observed":final_seen,
            "done_observed":done, "unicode_scalars":previous.chars().count(), "utf8_bytes":previous.len(),
            "semantic_contract":"exact visible acceptance preserves typed prefix/suffix and scalar caret; stale-document acceptance refused",
            "factual_quality_claim":false, "consumer_acceptance":true,
            "guided_healing_exercised":healing && config.prompt_style == "writing", "suffix_preserved":!suffix.is_empty()}));
    }
    let id = client.submit(config, PREFIX, "", &options).context("Cannot submit cancellable writing")?;
    ready(client, id, timeout, false)?;
    barrier(client, id)?;
    client.cancel().context("Cannot cancel actual worker request")?;
    let cancelled = wait_status(client, timeout, |s| s["latest_id"].as_u64().is_some_and(|latest| latest > id)
        && s["active_id"].is_null() && s["initializing"] == false && s["evaluating"] == false && s["awaiting_proceed"] == false,
        "Worker cancellation was not acknowledged before deadline")?;
    ensure!(client.poll().is_empty(), "Cancelled writing escaped consumer revocation");
    record(receipt, json!({"check":"cancellation_acknowledged", "passed":true, "cancelled_request":id, "status":cancelled,
        "boundary":"ready-before-proceed; cancellation revokes authorization, not a claim of mid-token interruption"}));
    client.release().context("Cannot request actual model release")?;
    let released = wait_status(client, timeout, |s| s["release_complete"] == true && s["release_pending"] == false
        && s["worker_pid"].is_null() && s["terminating"] == false
        && s["release_generation"] == s["released_generation"] && s["released_through"].as_u64().is_some_and(|through| through >= id),
        "Worker model release was not acknowledged before deadline")?;
    record(receipt, json!({"check":"release_acknowledged", "passed":true, "status":released}));
    // Shutdown must also retire a genuinely loaded child, not merely an idle owner.
    let prepared = client.prepare(config).context("Cannot prepare worker for loaded shutdown")?;
    ensure!(prepared > 0, "Worker did not reserve model preparation after release");
    let deadline = Instant::now() + timeout;
    let mut loaded = false;
    loop {
        for event in client.poll() {
            match event.kind.as_str() {
                "loading" => loaded = true,
                "prepared" => {
                    ensure!(event.id == prepared && loaded && event.model_loaded, "Reload did not acknowledge actual model preparation");
                    record(receipt, json!({"check":"reload_before_shutdown", "passed":true, "request":prepared}));
                    return Ok(());
                }
                "done" => {},
                _ => bail!("Worker model reload returned an unexpected event"),
            }
        }
        ensure!(Instant::now() < deadline, "Worker reload did not finish before deadline");
        thread::sleep(Duration::from_millis(5));
    }
}

fn shutdown(client: &mut WorkerClient, timeout: Duration) -> Result<()> {
    client.request_stop().context("Cannot request worker shutdown")?;
    let deadline = Instant::now() + timeout;
    while !client.is_stopped() {
        ensure!(Instant::now() < deadline, "Canonical worker owner did not acknowledge shutdown before deadline");
        thread::sleep(Duration::from_millis(5));
    }
    let status = client.status();
    ensure!(status["owner_finished"] == true && status["worker_pid"].is_null() && status["terminating"] == false,
        "Worker shutdown returned without owner/child cleanup acknowledgement");
    Ok(())
}

fn validate_text(text: &str, prefix: &str, suffix: &str, short: bool) -> Result<()> {
    ensure!(!text.trim().is_empty() && text.chars().any(char::is_alphanumeric), "Completion was empty, whitespace-only or nonlexical");
    ensure!(text.chars().count() <= MAX_SUGGESTION_CHARS, "Completion exceeded consumer scalar bound");
    ensure!(!text.contains('\u{fffd}') && text.chars().all(|c| c == ' ' || (!c.is_control() && !c.is_whitespace()
        && !matches!(unicode_general_category::get_general_category(c), unicode_general_category::GeneralCategory::Format))),
        "Completion contained control, replacement or non-inline whitespace characters");
    for marker in ["<|", "<turn|>", "<channel|>", "<bos>", "<eos>", "<start_of_turn>", "<end_of_turn>", "</s>", "[INST]", "<think", "</think", "<tool", "</tool", "```", "<", ">"] {
        ensure!(!text.contains(marker), "Completion leaked markup or model framing");
    }
    ensure!(clean_completion(text, suffix, false) == text, "Completion violated canonical suffix/sanitation fences");
    ensure!(!text.contains(prefix), "Completion repeated the already typed document prefix");
    if short { ensure!(text.split_whitespace().count() <= 8, "Writing completion exceeded its eight-word consumer fence"); }
    Ok(())
}

fn validate_boundary(text: &str, healing: bool) -> Result<()> {
    // The pinned 135M Tiny baseline repeats \"the capital of France\" for this
    // prompt. Factual knowledge and a particular generated word are not the
    // typing consumer contract. Check insertion boundaries, not model phrases.
    if healing {
        ensure!(text.chars().next().is_some_and(char::is_alphanumeric),
            "Partial-word continuation did not extend the typed lexical boundary");
    } else {
        ensure!(text.starts_with(' '), "Continuation did not preserve the typed word boundary");
    }
    Ok(())
}

fn validate_consumer(prefix: &str, suffix: &str, text: &str) -> Result<()> {
    let snapshot = Snapshot { app_id:"worker-smoke".into(), field_id:"disposable-fixture".into(),
        text:format!("{prefix}{suffix}").into(), caret:prefix.chars().count(), ..Snapshot::default() };
    let mut state = SuggestionState::new();
    ensure!(state.offer(state.generation, snapshot.clone(), text.into()), "Canonical consumer rejected writing offer");
    let accepted = state.accept(&snapshot, true, true, true, Some(text)).context("Canonical consumer refused full acceptance")?;
    ensure!(accepted == text, "Canonical consumer changed a fully visible completion");
    let predicted = state.snapshot.as_ref().context("Canonical consumer omitted predicted document")?;
    ensure!(predicted.prefix() == format!("{prefix}{text}") && predicted.suffix() == suffix
        && predicted.caret == prefix.chars().count() + text.chars().count(), "Acceptance changed the typed prefix/suffix or scalar caret");
    // A stale document must never authorize inserting the observed suggestion.
    let mut stale = SuggestionState::new();
    ensure!(stale.offer(stale.generation, snapshot.clone(), text.into()), "Canonical consumer rejected stale-check offer");
    let changed = Snapshot { text:format!("{prefix}!{suffix}").into(), ..snapshot };
    ensure!(stale.accept(&changed, true, true, true, Some(text)).is_none(), "Consumer accepted writing against a stale document");
    Ok(())
}

struct Capture { bytes: Vec<u8>, total: usize }
fn drain(mut pipe: impl Read) -> std::io::Result<Capture> {
    let mut captured = Capture { bytes:Vec::new(), total:0 };
    let mut bytes = [0u8; 8192];
    loop {
        let length = pipe.read(&mut bytes)?;
        if length == 0 { return Ok(captured); }
        captured.total = captured.total.saturating_add(length);
        let remaining = PIPE_LIMIT.saturating_sub(captured.bytes.len());
        captured.bytes.extend_from_slice(&bytes[..length.min(remaining)]);
    }
}

struct CliChild { child: Child, readers: Vec<JoinHandle<()>> }
impl Drop for CliChild {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() { let _ = self.child.kill(); }
        let _ = self.child.wait();
        for reader in self.readers.drain(..) { let _ = reader.join(); }
    }
}

fn exercise_cli(app: &Path, profile: &Path, config: &Config, timeout: Duration, output: &Path, receipt: &mut Value) -> Result<()> {
    let mut command = Command::new(app);
    command.arg("--acceleration").arg("cpu").arg("complete").env("INKLOW_HOME", profile)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let child = command.spawn().context("Cannot spawn actual public CLI complete")?;
    let mut owned = CliChild { child, readers:Vec::new() };
    let stdout = owned.child.stdout.take().context("Public CLI stdout pipe missing")?;
    let stderr = owned.child.stderr.take().context("Public CLI stderr pipe missing")?;
    let (out_tx, out_rx) = mpsc::sync_channel(1);
    let (err_tx, err_rx) = mpsc::sync_channel(1);
    owned.readers.push(thread::spawn(move || { let _ = out_tx.send(drain(stdout)); }));
    owned.readers.push(thread::spawn(move || { let _ = err_tx.send(drain(stderr)); }));
    let mut input = owned.child.stdin.take().context("Public CLI stdin pipe missing")?;
    input.write_all(PREFIX.as_bytes()).context("Cannot write disposable public CLI prompt")?;
    drop(input);
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = owned.child.try_wait().context("Cannot observe public CLI process")? { break status; }
        if Instant::now() >= deadline {
            let _ = owned.child.kill();
            let _ = owned.child.wait();
            break owned.child.try_wait().context("Cannot reap timed-out public CLI")?.context("Timed-out public CLI was not reaped")?;
        }
        thread::sleep(Duration::from_millis(5));
    };
    let remaining = deadline.saturating_duration_since(Instant::now()).max(Duration::from_secs(1));
    let stdout = out_rx.recv_timeout(remaining).context("Public CLI stdout reader did not finish")?
        .context("Cannot read public CLI stdout")?;
    let stderr = err_rx.recv_timeout(remaining).context("Public CLI stderr reader did not finish")?
        .context("Cannot read public CLI stderr")?;
    fs::create_dir_all(output).context("Cannot create smoke report directory")?;
    // Native diagnostics are untrusted and may contain authored text. Preserve
    // bounded line digests/categories, never a raw fragment in durable artifacts.
    fs::write(output.join("worker-smoke-stderr.json"), serde_json::to_vec_pretty(&sanitized_diagnostics(&stderr))
        .expect("diagnostics serialize")).context("Cannot persist sanitized stderr diagnostics")?;
    ensure!(Instant::now() <= deadline, "Public CLI exceeded its owned process deadline");
    ensure!(status.success(), "Public CLI complete exited unsuccessfully; inspect sanitized stderr receipt");
    ensure!(stdout.total <= PIPE_LIMIT && stderr.total <= PIPE_LIMIT, "Public CLI exceeded bounded pipe capture");
    let text = String::from_utf8(stdout.bytes).context("Public CLI returned invalid UTF-8")?;
    validate_text(&text, PREFIX, "", config.prompt_style == "writing")?;
    validate_boundary(&text, false)?;
    validate_consumer(PREFIX, "", &text)?;
    record(receipt, json!({"check":"public_cli_complete", "passed":true, "exit_code":status.code(),
        "unicode_scalars":text.chars().count(), "utf8_bytes":text.len(), "stderr_bytes":stderr.total,
        "same_consumer_fences":true, "factual_quality_claim":false,
        "semantic_contract":"exact visible acceptance preserves typed prefix/suffix and scalar caret; stale-document acceptance refused"}));
    Ok(())
}

fn sanitized_diagnostics(capture: &Capture) -> Value {
    let lines: Vec<Value> = capture.bytes.split(|&byte| byte == b'\n').filter(|line| !line.is_empty()).map(|line| {
        let lower = String::from_utf8_lossy(line).to_ascii_lowercase();
        let category = if lower.contains("memory") || lower.contains("alloc") { "memory" }
            else if lower.contains("error") || lower.contains("fail") { "native_error" }
            else if lower.contains("model") || lower.contains("gguf") { "model_diagnostic" }
            else { "redacted_diagnostic" };
        json!({"category":category,"bytes":line.len(),"sha256":format!("{:x}",Sha256::digest(line)),"content":"redacted: may contain authored data"})
    }).collect();
    json!({"total_bytes":capture.total,"captured_bytes":capture.bytes.len(),"truncated":capture.total > capture.bytes.len(),"lines":lines})
}
