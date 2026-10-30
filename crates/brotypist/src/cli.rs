//! Public CLI and resident launcher. Launch overrides never silently persist preferences.
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
                        config.backend = "gguf".into();
                        config.model_path = path.to_string_lossy().into_owned();
                        config.prompt_style.clone_from(&preset.prompt_style);
                        config.model_id.clone_from(&preset.id);
                        save_config(&config)?;
                    }
                    println!("Verified model: {}\n{}", preset.label, path.display());
                    println!(
                        "{}",
                        if no_select {
                            "Model downloaded; selection unchanged."
                        } else {
                            "Model selected. Restart BroTypist or select it in AI Models to reload."
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
        crate::platform::wayland::prepare_input_method(descriptor)?;
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
