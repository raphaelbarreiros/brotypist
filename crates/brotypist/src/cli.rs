//! Public CLI and resident launcher. Launch overrides never silently persist preferences.
use crate::runtime::Worker;
use crate::ui::settings::SettingsDraft;
use crate::{
    config::{Config, load_config, save_config},
    models,
    providers::{GenerationOptions, create_provider},
};
#[cfg(target_os = "linux")]
use anyhow::Context;
use anyhow::{Result, bail, ensure};
use clap::{Parser, Subcommand, ValueEnum};
use std::{
    io::{self, Read, Write},
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

#[derive(Parser, Debug)]
#[command(name="BroTypist", bin_name="brotypist", version=env!("CARGO_PKG_VERSION"), about="Local-first predictive typing")]
struct Cli {
    #[arg(long)]
    model_path: Option<String>,
    #[arg(long)]
    background: bool,
    #[arg(long, hide = true)]
    login_startup: bool,
    #[arg(long)]
    no_tray: bool,
    #[arg(long)]
    wayland_input_method: bool,
    #[arg(long)]
    paused: bool,
    #[arg(long)]
    settings: bool,
    #[arg(long)]
    pane: Option<String>,
    #[arg(long, value_parser=["continuation", "writing"])]
    prompt_style: Option<String>,
    #[arg(long, value_parser=["auto", "cpu"])]
    acceleration: Option<String>,
    #[command(subcommand)]
    command: Option<Commands>,
}
#[derive(Subcommand, Debug)]
enum Commands {
    Doctor,
    Complete,
    Wayland {
        #[arg(value_enum)]
        action: WaylandAction,
    },
    Model {
        #[arg(value_enum)]
        action: ModelAction,
        #[arg(long, default_value=models::DEFAULT_PRESET)]
        preset: String,
        #[arg(long)]
        no_select: bool,
        #[arg(long)]
        accept_model_license: bool,
    },
    Update {
        #[arg(value_enum)]
        action: UpdateAction,
    },
}
#[derive(Clone, Debug, ValueEnum)]
enum WaylandAction {
    Install,
}
#[derive(Clone, Debug, ValueEnum)]
enum ModelAction {
    List,
    Download,
}
#[derive(Clone, Debug, ValueEnum)]
enum UpdateAction {
    Check,
    Download,
}

pub fn entry() -> i32 {
    match run(std::env::args().skip(1).collect()) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("BroTypist: {error}");
            1
        }
    }
}

pub fn run(arguments: Vec<String>) -> Result<i32> {
    // Must precede config, model, toolkit and worker startup. Consumes health environment
    // so inference children cannot inherit authority to acknowledge an updated GUI.
    let _startup_guard = crate::update_install::startup_guard()?;
    if arguments == ["_worker"] {
        return crate::worker::worker_main();
    }
    let args = match Cli::try_parse_from(std::iter::once("brotypist".to_string()).chain(arguments))
    {
        Ok(args) => args,
        Err(error) => {
            let code = error.exit_code();
            error.print()?;
            return Ok(code);
        }
    };
    if let Some(pane) = &args.pane {
        ensure!(valid_pane(pane), "Unknown settings pane: {pane}");
    }
    ensure!(
        !args.wayland_input_method || args.command.is_none(),
        "The Wayland input-method mode is only for KDE application launches on Linux."
    );
    let prepared = prepare_wayland(args.wayland_input_method)?;
    let result = dispatch(args);
    if prepared {
        cleanup_wayland();
    }
    result
}

fn dispatch(args: Cli) -> Result<i32> {
    if args.login_startup
        && args.background
        && args.command.is_none()
        && !args.settings
        && args.pane.is_none()
        && crate::system::wayland_input_method_selected()
    {
        return Ok(0);
    }
    let mut config = load_config()?;
    if let Some(Commands::Update { action }) = &args.command {
        return update(action);
    }
    if let Some(path) = args.model_path {
        let name = Path::new(&path).file_name().and_then(|name| name.to_str());
        let known = models::presets()
            .iter()
            .find(|preset| Some(preset.filename.as_str()) == name);
        config.backend = "gguf".into();
        config.model_path = path;
        config.prompt_style = known
            .map_or("continuation", |p| p.prompt_style.as_str())
            .into();
        config.model_id = known.map_or("", |p| p.id.as_str()).into();
    }
    if let Some(style) = args.prompt_style {
        config.prompt_style = style;
    }
    if args.paused {
        config.global_enabled = false;
    }
    if let Some(acceleration) = args.acceleration {
        config.acceleration = acceleration;
    }
    config.validate()?;
    match args.command {
        Some(Commands::Doctor) => doctor(&config),
        Some(Commands::Complete) => complete(config),
        Some(Commands::Model {
            action,
            preset,
            no_select,
            accept_model_license,
        }) => {
            match action {
                ModelAction::List => {
                    for preset in models::presets() {
                        let state = if models::is_installed(preset) {
                            "installed"
                        } else {
                            "not downloaded"
                        };
                        println!(
                            "{}: {} — {:.0} MB, {}\n  {}\n  License: {} — {}",
                            preset.id,
                            preset.label,
                            preset.size_bytes as f64 / 1_000_000.0,
                            state,
                            preset.description,
                            preset.license_name,
                            preset.license_url
                        );
                    }
                }
                ModelAction::Download => {
                    let preset = models::get_preset(&preset)?;
                    let path = models::download_model(
                        &preset.id,
                        accept_model_license,
                        &models::DownloadCancellation::new(),
                        &mut |_, _| {},
                    )?;
                    if !no_select {
                        let _owner = crate::instance::InstanceLock::acquire()?
                            .ok_or_else(|| anyhow::anyhow!("BroTypist is running. Select the downloaded model in AI Models so its existing inference owner can validate it."))?;
                        let baseline = load_config()?;
                        let mut candidate = baseline.clone();
                        candidate.backend = "gguf".into();
                        candidate.model_path = path.to_string_lossy().into_owned();
                        candidate.prompt_style.clone_from(&preset.prompt_style);
                        candidate.model_id.clone_from(&preset.id);
                        candidate.validate()?;
                        let draft = SettingsDraft::from_configs(&baseline, &candidate)?;
                        let mut worker = crate::process_client::WorkerClient::new()?;
                        let validation = validate_selection(&candidate, &mut worker);
                        let cleanup = worker.stop();
                        validation?;
                        cleanup?;
                        let rebased = draft.apply_to(&load_config()?)?;
                        ensure!(
                            rebased.provider_identity() == candidate.provider_identity()
                                && rebased.prompt_style == candidate.prompt_style
                                && rebased.battery_model == candidate.battery_model,
                            "Model inputs changed during validation; select it again."
                        );
                        save_config(&rebased)?;
                    }
                    println!("Verified model: {}\n{}", preset.label, path.display());
                    println!(
                        "{}",
                        if no_select {
                            "Model downloaded; selection unchanged."
                        } else {
                            "Model structurally validated and selected. This does not certify writing quality."
                        }
                    );
                }
            }
            Ok(0)
        }
        Some(Commands::Wayland {
            action: WaylandAction::Install,
        }) => {
            let path = crate::system::install_wayland_input_method()?;
            println!(
                "Installed {}\nQuit BroTypist, then select BroTypist in KDE System Settings → Keyboard → Virtual Keyboard.\nKDE starts the input-method instance; no desktop setting was changed automatically.\nOrdinary inline completion remains unavailable; this entry provides guarded manual controls.",
                path.display()
            );
            Ok(0)
        }
        Some(Commands::Update { .. }) => unreachable!("update dispatch precedes launch overrides"),
        None => crate::ui::launch(
            config,
            crate::ui::LaunchOptions {
                background: args.background,
                no_tray: args.no_tray,
                pane: args
                    .pane
                    .or_else(|| args.settings.then(|| "general".into())),
                login_startup: args.login_startup,
            },
        ),
    }
}

fn validate_selection(config: &Config, worker: &mut dyn Worker) -> Result<()> {
    let mut targets = Vec::new();
    if config.backend == "gguf" && config.battery_model != "same" {
        let preset = models::get_preset(&config.battery_model)?;
        let mut battery = config.clone();
        battery.model_path = models::model_path(preset)?.to_string_lossy().into_owned();
        battery.model_id = preset.id.clone();
        battery.prompt_style = preset.prompt_style.clone();
        targets.push(battery);
    }
    targets.push(config.clone());
    for target in targets {
        if !target.model_id.is_empty() {
            let preset = models::get_preset(&target.model_id)?.clone();
            let path = crate::paths::expand_user(std::ffi::OsStr::new(&target.model_path))?;
            std::thread::spawn(move || {
                models::verify_catalogue_file(&preset, &path, &models::DownloadCancellation::new())
            })
            .join()
            .map_err(|_| anyhow::anyhow!("Model integrity worker stopped."))??;
        }
        let deadline = Instant::now() + Duration::from_secs(120);
        let ticket = worker.release()?;
        while !worker.release_complete(ticket) {
            ensure!(
                Instant::now() < deadline,
                "Model owner did not release in time."
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let id = worker.prepare_for_validation(&target, ticket)?;
        let mut prepared = false;
        loop {
            let mut done = false;
            for event in worker.poll() {
                if event.id != id.get() {
                    continue;
                }
                match event.kind.as_str() {
                    "prepared" => prepared = true,
                    "error" => bail!("{}", event.message),
                    "done" => done = true,
                    _ => {}
                }
            }
            if done {
                ensure!(prepared, "Model ended without fresh preparation.");
                break;
            }
            ensure!(Instant::now() < deadline, "Model validation timed out.");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    Ok(())
}

fn doctor(config: &Config) -> Result<i32> {
    let (mut bridge, native) = match crate::platform::create_bridge(config) {
        Ok(bridge) => {
            let status = bridge.status();
            (Some(bridge), status)
        }
        Err(error) => (
            None,
            serde_json::json!({"available":false,"readiness":"unavailable","reason":error.to_string()}),
        ),
    };
    let output = serde_json::json!({
        "platform": std::env::consts::OS,
        "runtime": "Rust",
        "version": env!("CARGO_PKG_VERSION"),
        "settings": crate::config::config_path()?,
        "backend": config.backend,
        "acceleration": config.acceleration,
        "shell": if cfg!(target_os="macos") { "AppKit" } else if cfg!(target_os="windows") { "Win32" } else { "GTK native utility" },
        "idle_release_seconds": config.idle_release_seconds,
        "model_file_exists": !config.model_path.is_empty() && expanded_model_path(&config.model_path).is_file(),
        "native": native,
        "global_enabled": config.global_enabled,
    });
    let printed = serde_json::to_writer_pretty(io::stdout().lock(), &output);
    let stopped = bridge.as_mut().map_or(Ok(()), |bridge| bridge.stop());
    printed?;
    println!();
    stopped?;
    Ok(0)
}
fn expanded_model_path(value: &str) -> std::path::PathBuf {
    if let Some(rest) = value.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return Path::new(&home).join(rest);
        }
    }
    value.into()
}
fn complete(config: Config) -> Result<i32> {
    let mut prefix = String::new();
    io::stdin().read_to_string(&mut prefix)?;
    ensure!(
        !prefix.trim().is_empty(),
        "Provide text on stdin, e.g. printf 'The capital of France is' | brotypist complete"
    );
    let options = GenerationOptions {
        instructions: config.resolve_instructions("", "", false)?,
        max_words: if config.prompt_style == "writing" {
            8
        } else {
            0
        },
        ..Default::default()
    };
    let mut provider = create_provider(config)?;
    let mut result = String::new();
    let generated = provider.stream(
        &prefix,
        "",
        &options,
        &AtomicBool::new(false),
        &mut |cumulative| {
            result.clear();
            result.push_str(cumulative);
            Ok(())
        },
    );
    let closed = provider.close();
    generated?;
    closed?;
    io::stdout().lock().write_all(result.as_bytes())?;
    io::stdout().flush()?;
    Ok(0)
}

fn update(action: &UpdateAction) -> Result<i32> {
    let service = crate::updates::UpdateService::new()?;
    let result = (|| -> Result<i32> {
        service.start_check()?;
        wait_update(&service, "checking", Duration::from_secs(120))?;
        let status = service.snapshot();
        if status["state"] == "error" {
            bail!(
                "{}",
                status["message"].as_str().unwrap_or("Update check failed.")
            );
        }
        if matches!(action, UpdateAction::Download) && status["state"] == "available" {
            service.start_download()?;
            wait_update(&service, "downloading", Duration::from_secs(600))?;
            let status = service.snapshot();
            ensure!(
                status["state"] == "ready",
                "{}",
                status["message"]
                    .as_str()
                    .unwrap_or("Update download failed.")
            );
        }
        serde_json::to_writer_pretty(io::stdout().lock(), &service.snapshot())?;
        println!();
        Ok(0)
    })();
    let closed = service.close();
    result.and_then(|code| {
        closed?;
        Ok(code)
    })
}
fn wait_update(
    service: &crate::updates::UpdateService,
    state: &str,
    timeout: Duration,
) -> Result<()> {
    let started = Instant::now();
    while service.snapshot()["state"] == state {
        ensure!(
            started.elapsed() < timeout,
            "The update operation did not finish."
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}
fn valid_pane(pane: &str) -> bool {
    if cfg!(target_os = "macos") {
        [
            "welcome",
            "setup",
            "general",
            "models",
            "context",
            "personalization",
            "emoji",
            "shortcuts",
            "battery",
            "appSettings",
            "labs",
            "statistics",
            "about",
        ]
        .contains(&pane)
    } else {
        [
            "welcome",
            "general",
            "models",
            "autocomplete",
            "languages",
            "privacy",
            "about",
        ]
        .contains(&pane)
    }
}
fn prepare_wayland(enabled: bool) -> Result<bool> {
    if !enabled {
        return Ok(false);
    }
    #[cfg(target_os = "linux")]
    {
        let descriptor = std::env::var("WAYLAND_SOCKET").context("Select BroTypist in KDE Virtual Keyboard settings; KWin must supply its private input-method socket.")?.parse::<i32>()?;
        // This launch owner runs before toolkit initialization or worker threads.
        unsafe {
            crate::platform::wayland::prepare_input_method(descriptor)?;
        }
        Ok(true)
    }
    #[cfg(not(target_os = "linux"))]
    {
        bail!("The Wayland input-method mode is only for KDE application launches on Linux.")
    }
}
fn cleanup_wayland() {
    #[cfg(target_os = "linux")]
    let _ = crate::platform::wayland::cleanup_prepared();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        process_client::{ReleaseTicket, WorkerClient},
        worker::WorkerEvent,
    };
    use std::num::NonZeroU64;

    struct ValidationWorker {
        ticket: Option<ReleaseTicket>,
        events: Vec<WorkerEvent>,
        validation_available: bool,
        ordinary_prepares: usize,
        validations: usize,
    }
    impl Worker for ValidationWorker {
        fn prepare(&mut self, _: &Config) -> Result<u64> {
            self.ordinary_prepares += 1;
            Ok(0)
        }
        fn submit(&mut self, _: &Config, _: &str, _: &str, _: &GenerationOptions) -> Result<u64> {
            panic!("selection must not write")
        }
        fn proceed(&mut self, _: u64) -> Result<()> {
            panic!("config-only preparation must not authorize writing")
        }
        fn poll(&mut self) -> Vec<WorkerEvent> {
            std::mem::take(&mut self.events)
        }
        fn cancel(&mut self) -> Result<()> {
            Ok(())
        }
        fn release(&mut self) -> Result<ReleaseTicket> {
            let ticket = WorkerClient::new()?.release()?;
            self.ticket = Some(ticket);
            Ok(ticket)
        }
        fn release_complete(&self, ticket: ReleaseTicket) -> bool {
            self.ticket == Some(ticket)
        }
        fn prepare_for_validation(
            &mut self,
            _: &Config,
            ticket: ReleaseTicket,
        ) -> Result<NonZeroU64> {
            ensure!(self.ticket.take() == Some(ticket), "exact release required");
            ensure!(self.validation_available, "fresh validation unavailable");
            self.validations += 1;
            Ok(NonZeroU64::new(7).unwrap())
        }
        fn set_idle_timeout(&mut self, _: u32) {}
        fn status(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        fn request_stop(&mut self) -> Result<()> {
            Ok(())
        }
        fn is_stopped(&mut self) -> bool {
            true
        }
    }
    fn worker(events: Vec<WorkerEvent>, available: bool) -> ValidationWorker {
        ValidationWorker {
            ticket: None,
            events,
            validation_available: available,
            ordinary_prepares: 0,
            validations: 0,
        }
    }
    #[test]
    fn cli_selection_requires_fresh_matching_prepared_and_done_without_writing() {
        let config = Config {
            model_id: String::new(),
            battery_model: "same".into(),
            ..Config::default()
        };
        let mut valid = worker(
            vec![WorkerEvent::new("prepared", 7), WorkerEvent::new("done", 7)],
            true,
        );
        validate_selection(&config, &mut valid).unwrap();
        assert_eq!(valid.validations, 1);
        assert_eq!(valid.ordinary_prepares, 0);
        let mut no_prepared = worker(
            vec![WorkerEvent::new("prepared", 6), WorkerEvent::new("done", 7)],
            true,
        );
        assert!(validate_selection(&config, &mut no_prepared).is_err());
        let mut zero_only = worker(Vec::new(), false);
        assert!(validate_selection(&config, &mut zero_only).is_err());
        assert_eq!(zero_only.ordinary_prepares, 0);
    }
    #[test]
    fn cli_selection_refuses_error_even_when_prepared_and_done_share_the_batch() {
        let config = Config {
            model_id: String::new(),
            battery_model: "same".into(),
            ..Config::default()
        };
        let mut failed = worker(
            vec![
                WorkerEvent::new("prepared", 7),
                WorkerEvent::new("done", 7),
                WorkerEvent {
                    message: "validation failed".into(),
                    ..WorkerEvent::new("error", 7)
                },
            ],
            true,
        );
        assert!(validate_selection(&config, &mut failed).is_err());
    }
}
