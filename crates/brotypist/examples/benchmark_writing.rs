//! Synthetic writing benchmark against direct native inference and its worker.
//! No downloads, saved settings, normal profile, desktop/context capture or keys.
//! Exit 0: observed mechanical checks passed, NOT semantic acceptance or an SLA.
//! Exit 1: mechanical/runtime failure or inconclusive cancellation; 2: invalid CLI.
#[path = "benchmark_writing/corpus.rs"]
mod corpus;
#[path = "benchmark_writing/direct.rs"]
mod direct;
#[path = "benchmark_writing/direct_checks.rs"]
mod direct_checks;
#[path = "benchmark_writing/direct_process.rs"]
mod direct_process;
#[path = "benchmark_writing/evaluation.rs"]
mod evaluation;
#[path = "benchmark_writing/native_details.rs"]
mod native_details;
#[path = "benchmark_writing/runner.rs"]
mod runner;
#[path = "../src/unicode_compat.rs"]
mod unicode_compat;

use anyhow::{Context, Result, ensure};
use brotypist::{config::Config, features::length_profile};
use clap::Parser;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

#[derive(Parser)]
#[command(
    about = "Evaluate authored synthetic writing with real direct GGUF inference and optional canonical worker measurements (macOS). Exit 0 means mechanical success, never semantic acceptance."
)]
struct Arguments {
    /// Existing candidate GGUF; no download or automatic model selection.
    #[arg(long)]
    model: PathBuf,
    /// Optional existing GGUF; identical settings/cases, alternating model order.
    #[arg(long)]
    baseline_model: Option<PathBuf>,
    /// New private JSON artifact; parent must exist; refuses existing files/symlinks.
    #[arg(long)]
    output: PathBuf,
    /// Actual native BroTypist-cli executable, NOT this example or a script.
    #[arg(long)]
    app: PathBuf,
    /// Fresh owned native-worker rounds per model (not cleared OS/Metal caches).
    #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u8).range(1..=10))]
    repetitions: u8,
    #[arg(long, default_value = "medium", value_parser = ["word", "short", "medium", "long"])]
    completion_length: String,
    /// Ordinary diagnostic cap override (8–256); utilities always request 128.
    /// Worker-only/both execution retains canonical Config's <=128 bound.
    #[arg(long, value_parser = clap::value_parser!(u32).range(8..=256))]
    max_tokens: Option<u32>,
    #[arg(long, default_value = "auto", value_parser = ["auto", "cpu"])]
    acceleration: String,
    /// Original benchmark uses writing. Explicit continuation supports legacy Tiny
    /// transport exercises; it does not make Tiny support guided utility prompts.
    #[arg(long, default_value = "writing", value_parser = ["writing", "continuation"])]
    prompt_style: String,
    /// Optional exact case IDs, repeatable. Omit for the complete authored corpus.
    /// A subset is reported explicitly and is never full-suite acceptance.
    #[arg(long = "case")]
    cases: Vec<String>,
    #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u64).range(5..=1800))]
    prepare_timeout_seconds: u64,
    #[arg(long, default_value_t = 90, value_parser = clap::value_parser!(u64).range(5..=1800))]
    case_timeout_seconds: u64,
    /// Direct preserves native-call diagnostics and per-process RSS. Worker
    /// measures canonical IPC lifecycle separately; both never pools timings.
    #[arg(long, default_value = "direct", value_parser = ["direct", "worker", "both"])]
    execution: String,
}

struct Inputs {
    app: PathBuf,
    models: Vec<(String, PathBuf)>,
    output: PathBuf,
    artifact: File,
    quality: Value,
    cases: Vec<corpus::Case>,
    full_case_count: usize,
    tokens: u32,
    words: u32,
    profile: PathBuf,
}
fn expanded(path: &Path) -> PathBuf {
    if let Ok(rest) = path.strip_prefix("~") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    path.to_owned()
}
fn normal_profiles() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        let home = PathBuf::from(home);
        paths.extend([
            home.join("Library/Application Support/inklow"),
            home.join(".config/inklow"),
            home.join("AppData/Local/inklow"),
        ]);
    }
    if let Some(config) = std::env::var_os("XDG_CONFIG_HOME") {
        paths.push(PathBuf::from(config).join("inklow"));
    }
    paths
}
fn no_normal_profile_overlap(path: &Path) -> Result<()> {
    for normal in normal_profiles() {
        let normal = normal.canonicalize().unwrap_or(normal);
        ensure!(
            path != normal && !normal.starts_with(path) && !path.starts_with(&normal),
            "Benchmark location must not overlap the normal profile"
        );
    }
    Ok(())
}
fn isolated_profile() -> Result<PathBuf> {
    let root = std::env::var_os("INKLOW_HOME").filter(|value| !value.is_empty()).map(PathBuf::from)
        .context("Set INKLOW_HOME explicitly to a fresh empty disposable directory (for example mktemp -d)")?;
    ensure!(root.is_absolute(), "INKLOW_HOME must be absolute");
    fs::create_dir_all(&root).context("Cannot create isolated benchmark directory")?;
    let root = root.canonicalize().context("Cannot resolve INKLOW_HOME")?;
    no_normal_profile_overlap(&root)?;
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home)
            .canonicalize()
            .context("Cannot resolve HOME")?;
        ensure!(root != home, "INKLOW_HOME cannot be HOME");
    }
    ensure!(
        fs::read_dir(&root)?.next().is_none(),
        "INKLOW_HOME must be fresh and empty, never an existing app profile"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    }
    // Claim this root once; a later run must deliberately use a fresh root.
    private_file(&root.join("benchmark-owner"))?
        .write_all(b"Synthetic native benchmark only. No app settings are loaded.\n")?;
    Ok(root)
}
fn private_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options
        .open(path)
        .context("Cannot exclusively create private benchmark output")
}
fn native_executable(path: &Path) -> Result<PathBuf> {
    let path = expanded(path)
        .canonicalize()
        .context("Cannot resolve --app native executable")?;
    ensure!(
        path.is_file() && path != std::env::current_exe()?.canonicalize()?,
        "--app must be the actual native BroTypist-cli executable, not this example"
    );
    let mut header = [0; 4];
    File::open(&path)?.read_exact(&mut header)?;
    ensure!(
        header == *b"\x7fELF"
            || header[..2] == *b"MZ"
            || matches!(
                header,
                [0xcf, 0xfa, 0xed, 0xfe]
                    | [0xce, 0xfa, 0xed, 0xfe]
                    | [0xfe, 0xed, 0xfa, 0xcf]
                    | [0xfe, 0xed, 0xfa, 0xce]
                    | [0xca, 0xfe, 0xba, 0xbe]
                    | [0xbe, 0xba, 0xfe, 0xca]
            ),
        "--app must be a native executable, never a script"
    );
    Ok(path)
}
fn inputs(args: &Arguments) -> Result<Inputs> {
    // Preserve the old CLI's supported inference environment. No cross-platform
    // RSS conversion or native UI/runtime setup is silently inferred.
    ensure!(
        cfg!(target_os = "macos"),
        "This writing benchmark preserves its macOS-only inference environment"
    );
    let app = native_executable(&args.app)?;
    let mut models = Vec::new();
    for (label, path) in [
        ("candidate", Some(&args.model)),
        ("baseline", args.baseline_model.as_ref()),
    ] {
        let Some(path) = path else {
            continue;
        };
        let path = expanded(path)
            .canonicalize()
            .context("Model must be an existing GGUF; no weights are downloaded")?;
        ensure!(
            path.is_file()
                && path.extension().is_some_and(|extension| extension
                    .to_string_lossy()
                    .eq_ignore_ascii_case("gguf")),
            "Model must be an existing .gguf file"
        );
        let mut magic = [0; 4];
        File::open(&path)?.read_exact(&mut magic)?;
        ensure!(magic == *b"GGUF", "Model file does not have a GGUF header");
        models.push((label.into(), path));
    }
    let (quality, all_cases) = corpus::corpus()?;
    let full_case_count = all_cases.len();
    for requested in &args.cases {
        ensure!(
            all_cases.iter().any(|case| &case.id == requested),
            "Unknown --case ID: {requested}"
        );
    }
    let cases: Vec<_> = all_cases
        .into_iter()
        .filter(|case| args.cases.is_empty() || args.cases.contains(&case.id))
        .collect();
    ensure!(
        !cases.is_empty(),
        "At least one synthetic case must be selected"
    );
    // Check the embedded literal regexes up front, before allocating a model.
    for case in &cases {
        evaluation::semantic(case, "")?;
    }
    let (length_tokens, words) = length_profile(&args.completion_length);
    let tokens = args
        .max_tokens
        .unwrap_or(Config::default().max_tokens.min(length_tokens));
    ensure!(
        args.execution == "direct" || tokens <= 128,
        "Canonical WorkerClient Config supports max-tokens <=128; use --execution direct for legacy diagnostic caps 129–256"
    );
    let output = expanded(&args.output);
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = parent
        .canonicalize()
        .context("Output parent must already exist")?;
    ensure!(parent.is_dir(), "Output parent must be a directory");
    no_normal_profile_overlap(&parent)?;
    let output = parent.join(output.file_name().context("Output must name a new file")?);
    let profile = isolated_profile()?;
    let artifact = private_file(&output)?;
    Ok(Inputs {
        app,
        models,
        output,
        artifact,
        quality,
        cases,
        full_case_count,
        tokens,
        words,
        profile,
    })
}
fn source_hashes() -> Value {
    let mut hashes = Map::new();
    for (name, bytes) in [
        (
            "crates/brotypist/src/providers.rs",
            include_bytes!("../src/providers.rs").as_slice(),
        ),
        (
            "crates/brotypist/src/writing.rs",
            include_bytes!("../src/writing.rs").as_slice(),
        ),
        (
            "crates/brotypist/src/features.rs",
            include_bytes!("../src/features.rs").as_slice(),
        ),
        (
            "crates/brotypist/src/process_client.rs",
            include_bytes!("../src/process_client.rs").as_slice(),
        ),
        (
            "crates/brotypist/src/worker.rs",
            include_bytes!("../src/worker.rs").as_slice(),
        ),
        (
            "crates/brotypist/src/inference_native.rs",
            include_bytes!("../src/inference_native.rs").as_slice(),
        ),
        (
            "crates/brotypist/examples/benchmark_writing.rs",
            include_bytes!("benchmark_writing.rs").as_slice(),
        ),
        (
            "crates/brotypist/examples/benchmark_writing/corpus.rs",
            include_bytes!("benchmark_writing/corpus.rs").as_slice(),
        ),
        (
            "crates/brotypist/examples/benchmark_writing/evaluation.rs",
            include_bytes!("benchmark_writing/evaluation.rs").as_slice(),
        ),
        (
            "crates/brotypist/examples/benchmark_writing/runner.rs",
            include_bytes!("benchmark_writing/runner.rs").as_slice(),
        ),
        (
            "crates/brotypist/examples/benchmark_writing/direct.rs",
            include_bytes!("benchmark_writing/direct.rs").as_slice(),
        ),
        (
            "crates/brotypist/examples/benchmark_writing/direct_checks.rs",
            include_bytes!("benchmark_writing/direct_checks.rs").as_slice(),
        ),
        (
            "crates/brotypist/examples/benchmark_writing/direct_process.rs",
            include_bytes!("benchmark_writing/direct_process.rs").as_slice(),
        ),
        (
            "crates/brotypist/examples/benchmark_writing/native_details.rs",
            include_bytes!("benchmark_writing/native_details.rs").as_slice(),
        ),
        (
            "crates/brotypist/src/providers_diagnostics.rs",
            include_bytes!("../src/providers_diagnostics.rs").as_slice(),
        ),
        (
            "crates/brotypist/native/llama_bridge.c",
            include_bytes!("../native/llama_bridge.c").as_slice(),
        ),
        (
            "crates/brotypist/examples/benchmark_writing/writing_quality.json",
            corpus::QUALITY_BYTES,
        ),
    ] {
        hashes.insert(name.into(), json!(format!("{:x}", Sha256::digest(bytes))));
    }
    Value::Object(hashes)
}
fn report(args: &Arguments, inputs: &Inputs) -> Result<Value> {
    let mut model_files = Map::new();
    for (label, path) in &inputs.models {
        let metadata = path.metadata()?;
        model_files.insert(label.clone(), json!({"path":path, "size_bytes":metadata.len(),
            "mtime_ns":metadata.modified().ok().and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .and_then(|time| u64::try_from(time.as_nanos()).ok())}));
    }
    Ok(
        json!({"schema_version":4, "suite":inputs.quality["suite"], "created_at":chrono::Utc::now().to_rfc3339(),
        "platform":std::env::consts::OS, "implementation":"Rust native diagnostics and canonical WorkerClient", "package_version":env!("CARGO_PKG_VERSION"),
        "repetitions":args.repetitions, "max_tokens":inputs.tokens, "max_words":inputs.words, "completion_length":args.completion_length,
        "acceleration":args.acceleration, "prompt_style":args.prompt_style, "utility_max_tokens":128,
        "execution":args.execution, "sessions":[], "worker_sessions":[], "source_sha256":source_hashes(), "cases":inputs.cases,
        "case_coverage":{"selected":inputs.cases.len(), "full_corpus":inputs.full_case_count, "complete_corpus_selected":inputs.cases.len() == inputs.full_case_count},
        "semantic_rubric":inputs.quality["rubric"], "review_policy":inputs.quality["review_policy"], "model_files":model_files,
        "methodology":{
            "isolation":"Explicit fresh empty INKLOW_HOME, mode-0700 root and exclusive mode-0600 report. Direct diagnostics use a fresh owned self-child and fresh HOME/XDG/INKLOW_HOME per model/round. Worker mode uses a fresh canonical WorkerClient owner and explicit native --app. Only in-memory validated Config; no app settings, Runtime, desktop capture, history, clipboard, screenshots, credentials, downloads or external inference.",
            "profile":inputs.profile,
            "timing":"Direct: Instant around actual NativeModel open plus canonical warmup, and around direct stream to native return plus canonical filtering. Includes tokenization, native inference and display filtering, excludes parent process startup/IPC. Worker: prepare submission through prepared+done includes child startup/IPC/polling; stream clocks begin immediately before explicit proceed and end on final+done. Includes IPC and 2ms consumer polling, excludes desktop/debounce/rendering. Fresh process is not cleared OS/filesystem/Metal caches. Failed/partial requests are not completed latency samples. Direct and worker summaries are separate; never pooled or paired across paths. No physical typing/key-to-screen claim.",
            "displayable":"First nonblank cumulative canonical insertion, or first parse_choices-compatible utility array, measured in that path's consumer. Worker time_to_first_worker_result_ms can include empty final output; neither is necessarily raw native first-token latency.",
            "alternatives":"Canonical alternatives algorithm: zero to three distinct strict insertable caret continuations, excluding anchor. Empty output or [] is safe absence only if mechanical checks pass; actual counts/empty results remain separately reported. Availability is not semantic quality.",
            "memory":"Direct self-child Darwin getrusage(RUSAGE_SELF).ru_maxrss in bytes: process-lifetime high-water RSS, including Rust/native/shared mappings. Per-case values are cumulative, not incremental or GPU/Metal/whole-machine memory. Measured in the same direct sessions as inference. Worker-only RSS is null because the explicit native child does not expose its own rusage over WorkerClient; never substitute parent/all-children RSS. No cross-path memory comparison.",
            "fairness":"Same model paths, production sampler, cases, length, diagnostic token caps and per-round rotated case order within each measurement path. Candidate/baseline execution order alternates each round; direct process is joined and worker release/shutdown acknowledged before the next scenario. No seed override/cache clearing. Descriptive small samples only; no quality/factual guarantee or SLA. Subsets identify incomplete corpus coverage.",
            "sampler":"Direct calls observe actual NativeModel prompt token counts/BOS occurrences, selected production SamplerOptions, native allowance, grammar/stops, raw UTF-8 before canonical replay/display filtering, finish reasons and sampler-return cleanup. NativeModel's byte-level stop fence precedes UTF-8 callbacks, as native stop handling precedes old Python chunks. No instrumentation changes production sampling. Current public ABI does not expose resolved batch/thread/mmap/mlock getters; those are null with reasons, not guessed defaults.",
            "reference_budget":"Direct retains the complete oversized authored context and canonical tokenizer-based budget clipping. Authored reference_sources use canonical reference_context's 8000-byte budget. Only worker transport bounds oversized direct context to 8000 scalars and identifies truncation. No fake reference sources.",
            "cancellation":"Direct preserves pre-cancel and synchronous cancellation before display at the first real native UTF-8 callback, observing real sampler return/cleanup. Worker separately probes before proceed and after first observed IPC result with bounded quiescence; already completed producer is inconclusive, not fabricated as active native interruption.",
            "limits":{"prepare_seconds":args.prepare_timeout_seconds, "each_case_seconds":args.case_timeout_seconds, "release_seconds":30, "shutdown_seconds":30}
        },
        "unsupported_measurements":["Resolved native batch/thread/mmap/mlock getters absent from current public ABI", "Worker-only per-child peak RSS absent from its protocol", "Incremental allocations and total GPU/Metal memory", "Physical typing/key-to-screen latency"],
        "not_tested":["Actual desktop same-window capture", "Focus/caret freshness and insertion", "Keyboard/idle release/UI", "Actual clipboard/screenshots/history", "Calibrated semantic quality, grammar, factuality or language grading", "Total GPU memory or latency SLA"]}),
    )
}
fn run(args: &Arguments, inputs: &Inputs, report: &mut Value) -> Result<bool> {
    let mut passed = true;
    for round in 0..usize::from(args.repetitions) {
        let mut ordered: Vec<_> = inputs.models.iter().collect();
        if round % 2 == 1 {
            ordered.reverse();
        }
        for (label, model) in ordered {
            if args.execution != "worker" {
                let mut session = direct_process::session(
                    args,
                    &inputs.profile,
                    model,
                    round,
                    &inputs.cases,
                    inputs.tokens,
                    inputs.words,
                );
                session["label"] = json!(label);
                session["execution_index"] =
                    json!(report["sessions"].as_array().expect("sessions").len() + 1);
                passed &= runner::passes(&session, inputs.cases.len())
                    && session["process"]["exitcode"] == 0
                    && session["process"]["terminated_by_watchdog"] == false;
                // The supervisor owns and joins/kills the actual child even
                // when a native diagnostic or a case fails.
                report["sessions"]
                    .as_array_mut()
                    .expect("sessions")
                    .push(session);
            }
            if args.execution != "direct" {
                let mut session = runner::session(
                    args,
                    &inputs.app,
                    model,
                    round,
                    &inputs.cases,
                    inputs.tokens,
                    inputs.words,
                );
                session["label"] = json!(label);
                session["measurement_path"] = json!("canonical_worker");
                session["execution_index"] = json!(
                    report["worker_sessions"]
                        .as_array()
                        .expect("worker sessions")
                        .len()
                        + 1
                );
                passed &= runner::passes(&session, inputs.cases.len());
                let closed = session["close"]["status"] == "pass";
                report["worker_sessions"]
                    .as_array_mut()
                    .expect("worker sessions")
                    .push(session);
                ensure!(
                    closed,
                    "Worker cleanup did not acknowledge; refusing to overlap a new model/round"
                );
            }
        }
    }
    Ok(passed)
}
fn main() {
    if std::env::args_os()
        .nth(1)
        .is_some_and(|argument| argument == "--benchmark-direct-child")
    {
        std::process::exit(direct_process::child_main());
    }
    let args = Arguments::parse();
    let mut inputs = match inputs(&args) {
        Ok(inputs) => inputs,
        Err(error) => {
            eprintln!("Invalid benchmark arguments: {error:#}");
            std::process::exit(2);
        }
    };
    let mut report = match report(&args, &inputs) {
        Ok(report) => report,
        Err(error) => {
            json!({"schema_version":4, "harness_error":error.to_string(), "sessions":[], "worker_sessions":[]})
        }
    };
    let result = if report["harness_error"].is_null() {
        run(&args, &inputs, &mut report)
    } else {
        Ok(false)
    };
    let passed = match result {
        Ok(passed) => passed,
        Err(error) => {
            report["harness_error"] = json!(error.to_string());
            false
        }
    };
    report["summary"] = evaluation::summarize(report["sessions"].as_array().expect("sessions"));
    report["worker_summary"] = evaluation::summarize(
        report["worker_sessions"]
            .as_array()
            .expect("worker sessions"),
    );
    report["mechanical_status"] = json!(if passed {
        "pass"
    } else {
        "fail_or_inconclusive"
    });
    let written = (|| -> Result<()> {
        serde_json::to_writer_pretty(&mut inputs.artifact, &report)?;
        inputs.artifact.write_all(b"\n")?;
        inputs.artifact.sync_all()?;
        Ok(())
    })();
    if let Err(error) = written {
        eprintln!("Cannot write benchmark report: {error:#}");
        std::process::exit(1);
    }
    println!("Synthetic evaluation artifact: {}", inputs.output.display());
    if !passed {
        std::process::exit(1);
    }
}
