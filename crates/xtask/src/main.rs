mod build;
mod common;
#[cfg(target_os = "linux")]
mod linux_installer_smoke;
#[cfg(target_os = "macos")]
mod macos_installer_smoke;
mod notices;
mod package;
mod release;
#[path = "../../brotypist/src/unicode_compat.rs"]
mod unicode_compat;
#[path = "../../brotypist/src/update_keys.rs"]
mod update_keys;
#[cfg(target_os = "windows")]
mod windows_installer_smoke;

use anyhow::{Result, ensure};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "Native Rust BroTypist builder and offline release tooling (never publishes)")]
struct Cli {
    #[arg(long, global = true)]
    root: Option<PathBuf>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Compile and assemble on the native host. Failed staging and previous builds are retained.
    Build {
        #[arg(long)]
        output: Option<PathBuf>,
        #[arg(long)]
        replace: bool,
        #[arg(long)]
        development: bool,
    },
    /// Inspect native binary identity, provenance, dependencies and macOS signature.
    Verify {
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long)]
        development: bool,
    },
    /// Build native DMG/ZIP, Debian/portable, or unsigned held Windows MSI/ZIP.
    Package {
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long, default_value = "release")]
        output: PathBuf,
    },
    /// Exercise actual native installers only within the probe's explicit ownership/safety guards.
    SmokeInstaller {
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long)]
        assets: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    /// Print/check the Rust application version, or change all three native Cargo package versions.
    Version {
        #[arg(long)]
        tag: Option<String>,
        #[arg(long, conflicts_with = "tag")]
        set: Option<String>,
    },
    /// Seal an exact native artifact set with a caller-provided passed installer receipt.
    Manifest {
        #[arg(long)]
        assets: PathBuf,
        #[arg(long)]
        target: String,
        #[arg(long)]
        commit: String,
        #[arg(long)]
        receipt: PathBuf,
    },
    /// Emit exact canonical publisher bytes for external authorized signing; contains no secrets.
    ReleaseSigningData {
        #[arg(long)]
        inputs: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        commit: String,
        #[arg(long, required = true, num_args = 1..)]
        targets: Vec<String>,
    },
    /// Offline release preparation; requires a valid existing-publisher detached signature.
    ReleaseManifest {
        #[arg(long)]
        inputs: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        commit: String,
        #[arg(long)]
        tag: String,
        #[arg(long)]
        signature: PathBuf,
        #[arg(long, required = true, num_args = 1..)]
        targets: Vec<String>,
    },
}

fn main() {
    if let Err(error) = execute(Cli::parse()) {
        eprintln!("BroTypist xtask: {error:#}");
        std::process::exit(1);
    }
}

fn execute(cli: Cli) -> Result<()> {
    let root = cli
        .root
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let root = root.canonicalize()?;
    let version = common::version(&root)?;
    match cli.command {
        Commands::Build {
            output,
            replace,
            development,
        } => {
            let output = output.unwrap_or_else(|| {
                root.join("dist").join(if cfg!(target_os = "macos") {
                    "BroTypist.app"
                } else {
                    "BroTypist"
                })
            });
            println!(
                "{}",
                build::build(&root, &output, replace, development)?.display()
            );
        }
        Commands::Verify {
            bundle,
            development,
        } => println!(
            "{}",
            serde_json::to_string_pretty(&package::inspect(&root, &bundle, development)?)?
        ),
        Commands::Package { bundle, output } => println!(
            "{}",
            serde_json::to_string_pretty(&package::package(&root, &bundle, &output)?)?
        ),
        Commands::SmokeInstaller {
            bundle,
            assets,
            output,
        } => {
            #[cfg(target_os = "macos")]
            macos_installer_smoke::run(&bundle, &assets, &output)?;
            #[cfg(target_os = "linux")]
            linux_installer_smoke::run(&bundle, &assets, &output)?;
            #[cfg(target_os = "windows")]
            windows_installer_smoke::run(&bundle, &assets, &output)?;
            #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
            anyhow::bail!(
                "Installer smoke requires a supported native macOS, Linux or Windows host"
            );
        }
        Commands::Version { tag, set } => {
            if let Some(value) = set {
                common::set_version(&root, &value)?;
                println!("{value}");
            } else {
                if let Some(tag) = &tag {
                    ensure!(
                        tag == &format!("v{version}"),
                        "tag differs from application version"
                    );
                }
                if let Some(output) = std::env::var_os("GITHUB_OUTPUT") {
                    use std::io::Write;
                    let mut file = std::fs::OpenOptions::new().append(true).open(output)?;
                    writeln!(
                        file,
                        "version={version}\ntag={}",
                        tag.as_deref().unwrap_or("")
                    )?;
                }
                println!("{version}");
            }
        }
        Commands::Manifest {
            assets,
            target,
            commit,
            receipt,
        } => release::manifest(&root, &assets, &target, &commit, &receipt)?,
        Commands::ReleaseSigningData {
            inputs,
            output,
            commit,
            targets,
        } => {
            let native = release::validate_inputs(&root, &inputs, &commit, &targets)?;
            common::write_new(
                &output,
                &release::publisher_bytes(&version, &commit, &native)?,
            )?;
        }
        Commands::ReleaseManifest {
            inputs,
            output,
            commit,
            tag,
            signature,
            targets,
        } => {
            ensure!(
                tag == format!("v{version}"),
                "tag differs from application version"
            );
            release::prepare(&root, &inputs, &output, &commit, &tag, &signature, &targets)?;
        }
    }
    Ok(())
}
