//! OS-native shells own their UI loop and presentation; they do not evaluate models.
use anyhow::Result;
use crate::config::Config;
pub mod settings;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "macos")]
pub mod macos_overlay;
#[cfg(target_os = "macos")]
pub mod macos_power;
#[cfg(target_os = "windows")]
pub mod windows;
#[cfg(target_os = "linux")]
pub mod linux;

#[derive(Clone, Debug, Default)]
pub struct LaunchOptions {
    pub background: bool,
    pub no_tray: bool,
    pub pane: Option<String>,
    pub login_startup: bool,
}

pub fn launch(config: Config, options: LaunchOptions) -> Result<i32> {
    let Some(_instance) = crate::instance::InstanceLock::acquire()? else {
        eprintln!("BroTypist is already running. Open Settings from its menu-bar or tray icon.");
        return Ok(0);
    };
    #[cfg(target_os = "macos")]
    { macos::launch(config, options) }
    #[cfg(target_os = "windows")]
    { windows::launch(config, options) }
    #[cfg(target_os = "linux")]
    { linux::launch(config, options) }
}

/// Exercise the real native shell only against an explicitly isolated disposable profile.
#[cfg(feature = "native-smoke")]
pub fn smoke(output: &std::path::Path) -> Result<i32> {
    use anyhow::{Context, ensure};
    ensure!(std::env::var_os("CI").is_some() || std::env::var("BROTYPIST_OWNED_DESKTOP_SMOKE").as_deref() == Ok("1"),
        "Native desktop smoke requires CI or explicit owned-window authorization");
    let override_path = std::env::var_os("INKLOW_HOME").filter(|value| !value.is_empty())
        .context("Native desktop smoke requires an explicit disposable INKLOW_HOME")?;
    let profile = std::path::PathBuf::from(override_path);
    ensure!(profile.is_absolute(), "Native desktop smoke profile must be absolute");
    let normal = crate::paths::platform_config_dir()?;
    ensure!(!profile.starts_with(&normal), "Native desktop smoke cannot use the normal profile tree");
    if let Ok(metadata) = std::fs::symlink_metadata(&profile) {
        ensure!(metadata.is_dir() && !metadata.file_type().is_symlink(), "Smoke profile must be an owned regular directory");
    }
    crate::paths::private_directory(&profile)?;
    let canonical = profile.canonicalize()?;
    let normal = normal.canonicalize().unwrap_or(normal);
    ensure!(!canonical.starts_with(&normal), "Native desktop smoke cannot resolve into the normal profile tree");
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let metadata = std::fs::metadata(&canonical)?;
        ensure!(metadata.uid() == unsafe { libc::geteuid() } && metadata.permissions().mode() & 0o077 == 0,
            "Native desktop smoke profile must be current-user-owned and private");
    }
    ensure!(std::fs::read_dir(&canonical)?.next().is_none(), "Native desktop smoke profile must be empty");
    ensure!(output.is_absolute(), "Native desktop smoke output must be absolute");
    let _instance = crate::instance::InstanceLock::acquire()?.context("Smoke profile already has a running application")?;
    #[cfg(target_os = "linux")]
    let prepared = if std::env::var("BROTYPIST_SMOKE_WAYLAND_INPUT_METHOD").as_deref() == Ok("1") {
        let descriptor = std::env::var("WAYLAND_SOCKET").context("KWin must supply the private smoke input-method socket")?.parse::<i32>()?;
        crate::platform::wayland::prepare_input_method(descriptor)?;
        true
    } else { false };
    #[cfg(target_os = "macos")]
    let result = macos::smoke(output);
    #[cfg(target_os = "windows")]
    let result = windows::smoke(output);
    #[cfg(target_os = "linux")]
    let result = linux::smoke(output);
    #[cfg(target_os = "linux")]
    if prepared { crate::platform::wayland::cleanup_prepared()?; }
    result
}

#[cfg(feature = "native-smoke")]
pub(crate) fn smoke_model_path() -> Result<std::path::PathBuf> {
    use anyhow::ensure;
    let path = match std::env::var_os("BROTYPIST_SMOKE_MODEL").filter(|value| !value.is_empty()) {
        Some(path) => std::path::PathBuf::from(path),
        None => crate::models::model_path(crate::models::get_preset("tiny")?)?,
    };
    ensure!(path.is_file(), "Native preview smoke requires an existing GGUF; download the checksum-verified Tiny preset first");
    Ok(path)
}
