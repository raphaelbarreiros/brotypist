//! Pinned model inventory and explicit, cancellable acquisition. Never selects a model.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs::File, io::{Read, Write}, path::{Path, PathBuf}, sync::LazyLock, time::Duration};
use tokio_util::sync::CancellationToken;

pub const DEFAULT_PRESET: &str = "gemma4-e2b";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ModelPreset {
    pub id: String,
    pub label: String,
    pub description: String,
    pub repo: String,
    pub revision: String,
    pub filename: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub prompt_style: String,
    pub license_name: String,
    pub license_url: String,
    pub requires_license_acceptance: bool,
    pub minimum_memory_bytes: u64,
}

static CATALOG: LazyLock<Vec<ModelPreset>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../assets/models.json"))
        .expect("embedded model catalogue must be valid")
});

pub fn presets() -> &'static [ModelPreset] { CATALOG.as_slice() }

pub fn get_preset(id: &str) -> Result<&'static ModelPreset> {
    presets().iter().find(|p| p.id == id).with_context(|| format!("Unknown model preset: {id}"))
}

static CAPACITY: LazyLock<Option<u64>> = LazyLock::new(measure_physical_memory);
pub fn physical_memory_bytes() -> Option<u64> { *CAPACITY }

#[cfg(target_os = "macos")]
fn measure_physical_memory() -> Option<u64> {
    let mut size = std::mem::size_of::<u64>();
    let mut value = 0_u64;
    // SAFETY: hw.memsize is a fixed read-only sysctl; both output pointers match its ABI.
    let status = unsafe { libc::sysctlbyname(c"hw.memsize".as_ptr(), (&mut value as *mut u64).cast(),
        &mut size, std::ptr::null_mut(), 0) };
    (status == 0 && size == std::mem::size_of::<u64>() && value > 0).then_some(value)
}
#[cfg(all(unix, not(target_os = "macos")))]
fn measure_physical_memory() -> Option<u64> {
    // SAFETY: sysconf accepts these constant selectors and returns no borrowed memory.
    let pages = unsafe { libc::sysconf(libc::_SC_PHYS_PAGES) };
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if pages <= 0 || page_size <= 0 { None } else { (pages as u64).checked_mul(page_size as u64) }
}
#[cfg(target_os = "windows")]
fn measure_physical_memory() -> Option<u64> {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default() };
    // SAFETY: initialized documented length and writable structure; no retained pointer.
    unsafe { GlobalMemoryStatusEx(&mut status).ok()?; }
    (status.ullTotalPhys > 0).then_some(status.ullTotalPhys)
}
#[cfg(not(any(unix, target_os = "windows")))]
fn measure_physical_memory() -> Option<u64> { None }

pub fn recommended_preset(memory: Option<u64>) -> &'static ModelPreset {
    get_preset(if memory.is_some_and(|bytes| bytes < 8 * 1024_u64.pow(3)) {
        "balanced"
    } else { DEFAULT_PRESET }).expect("embedded recommendation must exist")
}
pub fn model_availability(preset: &ModelPreset, memory: Option<u64>) -> Option<String> {
    memory.filter(|bytes| *bytes < preset.minimum_memory_bytes)
        .map(|_| format!("Requires at least {} GB of physical memory.", preset.minimum_memory_bytes / 1024_u64.pow(3)))
}
pub fn model_path(preset: &ModelPreset) -> Result<PathBuf> {
    Ok(crate::paths::model_dir()?.join(&preset.filename))
}
pub fn is_installed(preset: &ModelPreset) -> bool {
    model_path(preset).ok().and_then(|p| p.metadata().ok())
        .is_some_and(|m| m.is_file() && m.len() == preset.size_bytes)
}

/// Cancellation drops an owned asynchronous body read immediately, including stalled transfers.
#[derive(Clone, Debug, Default)]
pub struct DownloadCancellation(CancellationToken);
impl DownloadCancellation {
    pub fn new() -> Self { Self::default() }
    pub fn set(&self) { self.0.cancel(); }
    pub fn is_set(&self) -> bool { self.0.is_cancelled() }
    pub async fn cancelled(&self) { self.0.cancelled().await; }
    pub fn check(&self) -> Result<()> {
        ensure!(!self.is_set(), "Model download cancelled.");
        Ok(())
    }
}

fn digest_file(path: &Path, cancel: &DownloadCancellation) -> Result<String> {
    let mut source = File::open(path)?;
    let mut digest = Sha256::new();
    let mut chunk = vec![0_u8; 1024 * 1024];
    loop {
        cancel.check()?;
        let count = source.read(&mut chunk)?;
        if count == 0 { break; }
        digest.update(&chunk[..count]);
    }
    cancel.check()?;
    Ok(format!("{:x}", digest.finalize()))
}

/// Blocking public entrypoint. Call on a download owner, never the GUI or keyboard hook.
pub fn download_model(id: &str, accept_license: bool, cancel: &DownloadCancellation,
    progress: &mut dyn FnMut(u64, u64)) -> Result<PathBuf> {
    let preset = get_preset(id)?;
    if let Some(reason) = model_availability(preset, physical_memory_bytes()) { bail!(reason); }
    ensure!(!preset.requires_license_acceptance || accept_license,
        "Read and accept {} before downloading: {}. CLI downloads require --accept-model-license.",
        preset.license_name, preset.license_url);
    let target = model_path(preset)?;
    cancel.check()?;
    std::fs::create_dir_all(target.parent().context("Model has no cache directory")?)?;
    progress(0, preset.size_bytes);
    if is_installed(preset) && digest_file(&target, cancel)? == preset.sha256 {
        cancel.check()?;
        progress(preset.size_bytes, preset.size_bytes);
        cancel.check()?;
        return Ok(target);
    }
    let url = format!("https://huggingface.co/{}/resolve/{}/{}", preset.repo, preset.revision, preset.filename);
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(download_to(preset, &url, &target, cancel, progress))?;
    Ok(target)
}

async fn download_to(preset: &ModelPreset, url: &str, target: &Path,
    cancel: &DownloadCancellation, progress: &mut dyn FnMut(u64,u64)) -> Result<()> {
    ensure!(url::Url::parse(url)?.scheme() == "https", "Model download requires HTTPS.");
    let client = reqwest::Client::builder().connect_timeout(Duration::from_secs(30))
        .user_agent("inklow/0.2")
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.url().scheme() != "https" {
                attempt.error("Refusing a non-HTTPS model download redirect.")
            } else if attempt.previous().len() >= 10 {
                attempt.error("Too many model download redirects.")
            } else { attempt.follow() }
        })).build()?;
    let mut response = tokio::select! {
        _ = cancel.cancelled() => { cancel.check()?; unreachable!() },
        result = client.get(url).send() => result?.error_for_status()?,
    };
    let parent = target.parent().context("Model target has no parent")?;
    let mut staging = tempfile::Builder::new().prefix(".download-").tempfile_in(parent)?;
    let mut digest = Sha256::new();
    let mut count = 0_u64;
    loop {
        cancel.check()?;
        let next = tokio::select! {
            _ = cancel.cancelled() => { cancel.check()?; unreachable!() },
            result = tokio::time::timeout(Duration::from_secs(30), response.chunk()) => {
                result.context("Model download body timed out")??
            }
        };
        let Some(chunk) = next else { break; };
        // reqwest's transport chunks can be larger; process/progress remain bounded to 64 KiB.
        for part in chunk.chunks(64 * 1024) {
            cancel.check()?;
            count = count.checked_add(part.len() as u64).context("Model download size overflow")?;
            ensure!(count <= preset.size_bytes, "Model download exceeds its verified size.");
            digest.update(part);
            staging.write_all(part)?;
            progress(count, preset.size_bytes);
        }
    }
    cancel.check()?;
    verify_download(preset, count, &format!("{:x}", digest.finalize()))?;
    staging.as_file().sync_all()?;
    cancel.check()?;
    staging.persist(target).map_err(|e| e.error).context("Could not atomically install verified model")?;
    Ok(())
}

fn verify_download(preset: &ModelPreset, size: u64, hash: &str) -> Result<()> {
    ensure!(size == preset.size_bytes && hash == preset.sha256,
        "Model integrity verification failed; the existing model was not replaced.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capacity_policy_distinguishes_unknown_and_low_memory() {
        assert_eq!(recommended_preset(None).id, "gemma4-e2b");
        assert_eq!(recommended_preset(Some(4 * 1024_u64.pow(3))).id, "balanced");
        assert!(model_availability(get_preset("gemma3-1b").unwrap(), Some(3 * 1024_u64.pow(3))).is_some());
        assert!(model_availability(get_preset("gemma3-1b").unwrap(), None).is_none());
    }
    #[test]
    fn corrupt_or_truncated_transfer_is_not_installable() {
        let preset = get_preset("tiny").unwrap();
        assert!(verify_download(preset, preset.size_bytes, &"0".repeat(64)).is_err());
        assert!(verify_download(preset, preset.size_bytes - 1, &preset.sha256).is_err());
        assert!(verify_download(preset, preset.size_bytes + 1, &preset.sha256).is_err());
        assert!(verify_download(preset, preset.size_bytes, &preset.sha256).is_ok());
    }
    #[test]
    fn cached_verification_checks_content_and_cancellation() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"old working weights").unwrap();
        let cancel = DownloadCancellation::new();
        let before = digest_file(file.path(), &cancel).unwrap();
        file.as_file_mut().set_len(0).unwrap();
        file.write_all(b"corrupt replacement").unwrap();
        assert_ne!(digest_file(file.path(), &cancel).unwrap(), before);
        cancel.set();
        assert!(digest_file(file.path(), &cancel).is_err());
    }
}
