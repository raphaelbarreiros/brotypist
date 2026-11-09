//! Pinned model inventory and explicit, cancellable acquisition. Never selects a model.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::LazyLock,
    time::Duration,
};
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

pub fn presets() -> &'static [ModelPreset] {
    CATALOG.as_slice()
}

pub fn get_preset(id: &str) -> Result<&'static ModelPreset> {
    presets()
        .iter()
        .find(|p| p.id == id)
        .with_context(|| format!("Unknown model preset: {id}"))
}

static CAPACITY: LazyLock<Option<u64>> = LazyLock::new(measure_physical_memory);
pub fn physical_memory_bytes() -> Option<u64> {
    *CAPACITY
}

struct RamLabel<'a> {
    bytes: Option<u64>,
    unavailable: &'a str,
}

impl std::fmt::Display for RamLabel<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.bytes {
            Some(bytes) => write!(f, "{:.2} GiB", bytes as f64 / 1_073_741_824.0),
            None => f.write_str(self.unavailable),
        }
    }
}

/// Formats physical RAM in binary GiB without allocating an intermediate string.
/// Callers omit or filter unspecified zero catalogue minima and supply their
/// existing unavailable label; detected memory remains distinct from a minimum.
pub(crate) fn ram_label(bytes: Option<u64>, unavailable: &str) -> impl std::fmt::Display + '_ {
    RamLabel { bytes, unavailable }
}

#[cfg(target_os = "macos")]
fn measure_physical_memory() -> Option<u64> {
    let mut size = std::mem::size_of::<u64>();
    let mut value = 0_u64;
    // SAFETY: hw.memsize is a fixed read-only sysctl; both output pointers match its ABI.
    let status = unsafe {
        libc::sysctlbyname(
            c"hw.memsize".as_ptr(),
            (&mut value as *mut u64).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (status == 0 && size == std::mem::size_of::<u64>() && value > 0).then_some(value)
}
#[cfg(all(unix, not(target_os = "macos")))]
fn measure_physical_memory() -> Option<u64> {
    // SAFETY: sysconf accepts these constant selectors and returns no borrowed memory.
    let pages = unsafe { libc::sysconf(libc::_SC_PHYS_PAGES) };
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if pages <= 0 || page_size <= 0 {
        None
    } else {
        (pages as u64).checked_mul(page_size as u64)
    }
}
#[cfg(target_os = "windows")]
fn measure_physical_memory() -> Option<u64> {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: initialized documented length and writable structure; no retained pointer.
    unsafe {
        GlobalMemoryStatusEx(&mut status).ok()?;
    }
    (status.ullTotalPhys > 0).then_some(status.ullTotalPhys)
}
#[cfg(not(any(unix, target_os = "windows")))]
fn measure_physical_memory() -> Option<u64> {
    None
}

pub fn recommended_preset(memory: Option<u64>) -> &'static ModelPreset {
    get_preset(if memory.is_some_and(|bytes| bytes < 8 * 1024_u64.pow(3)) {
        "balanced"
    } else {
        DEFAULT_PRESET
    })
    .expect("embedded recommendation must exist")
}
pub fn model_availability(preset: &ModelPreset, memory: Option<u64>) -> Option<String> {
    memory
        .filter(|bytes| *bytes < preset.minimum_memory_bytes)
        .map(|_| {
            format!(
                "Requires at least {} of physical memory.",
                ram_label(Some(preset.minimum_memory_bytes), "")
            )
        })
}
pub fn model_path(preset: &ModelPreset) -> Result<PathBuf> {
    Ok(crate::paths::model_dir()?.join(&preset.filename))
}
pub fn is_installed(preset: &ModelPreset) -> bool {
    model_path(preset)
        .ok()
        .and_then(|p| p.metadata().ok())
        .is_some_and(|m| m.is_file() && m.len() == preset.size_bytes)
}

/// Retain only a pinned legacy default, without replacing or deleting either copy.
/// Returns false when neither location has the model. Call on a background owner:
/// the one-time migration verifies all weight bytes before publishing the copy.
pub(crate) fn retain_legacy_model(
    preset: &ModelPreset,
    legacy_directory: &Path,
    model_directory: &Path,
    cancel: &DownloadCancellation,
) -> Result<bool> {
    retain_legacy_model_with_link(
        preset,
        legacy_directory,
        model_directory,
        cancel,
        |source, target| std::fs::hard_link(source, target),
    )
}

fn retain_legacy_model_with_link(
    preset: &ModelPreset,
    legacy_directory: &Path,
    model_directory: &Path,
    cancel: &DownloadCancellation,
    link: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> Result<bool> {
    cancel.check()?;
    ensure!(
        Path::new(&preset.filename).file_name() == Some(std::ffi::OsStr::new(&preset.filename)),
        "Model retention requires a plain catalogue filename"
    );
    let source = legacy_directory.join(&preset.filename);
    let target = model_directory.join(&preset.filename);
    check_retention_directory(model_directory)?;
    if let Some(mut existing) = retained_file(&target)? {
        verify_retained_file(preset, &mut existing, cancel)?;
        return Ok(true);
    }
    let Some(mut input) = retained_file(&source)? else {
        return Ok(false);
    };
    cancel.check()?;
    crate::paths::private_directory(model_directory)?;
    check_retention_directory(model_directory)?;
    check_retention_directory(legacy_directory)?;
    let staging_directory = tempfile::Builder::new()
        .prefix(".retain-")
        .tempdir_in(model_directory)?;
    let staging_path = staging_directory.path().join(&preset.filename);
    match link(&source, &staging_path) {
        Ok(()) => {
            let mut staged = retained_file(&staging_path)?.context("Missing retained model")?;
            verify_retained_file(preset, &mut staged, cancel)?;
            // Linking does not write weight bytes. A read-only verification
            // handle cannot be flushed with Windows FlushFileBuffers.
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::CrossesDevices
                    | std::io::ErrorKind::Unsupported
                    | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut staged = options.open(&staging_path)?;
            let (size, hash) = copy_retained_file(preset, &mut input, &mut staged, cancel)?;
            verify_download(preset, size, &hash)?;
            staged.flush()?;
            staged.sync_all()?;
        }
        Err(error) => return Err(error).context("Cannot stage retained model"),
    }
    let staging = tempfile::TempPath::try_from_path(staging_path)?;
    #[cfg(unix)]
    File::open(model_directory)?.sync_all()?;
    // Publication is the commit point: cancellation observed before it leaves
    // both model paths unchanged; cancellation after it cannot undo the commit.
    cancel.check()?;
    match staging.persist_noclobber(&target) {
        Ok(_) => (),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            // A concurrent migration may have won. It must contain the same pinned bytes.
            let mut existing = retained_file(&target)?.context("Retained model disappeared")?;
            verify_retained_file(preset, &mut existing, cancel)?;
        }
        Err(error) => return Err(error.error).context("Cannot retain verified model"),
    }
    Ok(true)
}

fn check_retention_directory(path: &Path) -> Result<()> {
    let directory = match std::fs::symlink_metadata(path) {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        directory.is_dir() && !directory.file_type().is_symlink(),
        "Model storage must be a real directory"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            directory.uid() == unsafe { libc::geteuid() } && directory.mode() & 0o022 == 0,
            "Model storage must be owned by the current user and not writable by others"
        );
    }
    Ok(())
}

fn retained_file(path: &Path) -> Result<Option<File>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "Model retention refuses non-regular files: {}",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file(),
        "Model retention requires a regular file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            metadata.uid() == unsafe { libc::geteuid() } && metadata.mode() & 0o022 == 0,
            "Model retention requires a user-owned file not writable by others"
        );
    }
    Ok(Some(file))
}

fn copy_retained_file(
    preset: &ModelPreset,
    input: &mut File,
    output: &mut dyn Write,
    cancel: &DownloadCancellation,
) -> Result<(u64, String)> {
    cancel.check()?;
    ensure!(
        input.metadata()?.len() == preset.size_bytes,
        "Legacy model size differs from the pinned catalogue"
    );
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut size = 0_u64;
    loop {
        cancel.check()?;
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        size = size
            .checked_add(count as u64)
            .context("Model size overflow")?;
        ensure!(
            size <= preset.size_bytes,
            "Legacy model grew during retention"
        );
        digest.update(&buffer[..count]);
        output.write_all(&buffer[..count])?;
    }
    cancel.check()?;
    Ok((size, format!("{:x}", digest.finalize())))
}

fn verify_retained_file(
    preset: &ModelPreset,
    file: &mut File,
    cancel: &DownloadCancellation,
) -> Result<()> {
    let (size, hash) = copy_retained_file(preset, file, &mut std::io::sink(), cancel)?;
    verify_download(preset, size, &hash)
}

/// Check a catalogue candidate's existing bytes on a background owner. Opens
/// once, checks regular-file ownership, pinned size and SHA-256, and observes
/// cancellation while hashing. Never downloads, replaces or adopts custom paths.
///
/// This is selection-time integrity, not immutable-file attestation: the caller
/// must still prepare the selected configuration, and later file mutation is not
/// prevented by this check.
pub fn verify_catalogue_file(
    preset: &ModelPreset,
    path: &Path,
    cancel: &DownloadCancellation,
) -> Result<()> {
    cancel.check()?;
    let mut file = retained_file(path)?.context("Catalogue model file is missing")?;
    verify_retained_file(preset, &mut file, cancel)
}

/// Cancellation drops an owned asynchronous body read immediately, including stalled transfers.
#[derive(Clone, Debug, Default)]
pub struct DownloadCancellation(CancellationToken);
impl DownloadCancellation {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn set(&self) {
        self.0.cancel();
    }
    pub fn is_set(&self) -> bool {
        self.0.is_cancelled()
    }
    pub async fn cancelled(&self) {
        self.0.cancelled().await;
    }
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
        if count == 0 {
            break;
        }
        digest.update(&chunk[..count]);
    }
    cancel.check()?;
    Ok(format!("{:x}", digest.finalize()))
}

/// Blocking public entrypoint. Call on a download owner, never the GUI or keyboard hook.
pub fn download_model(
    id: &str,
    accept_license: bool,
    cancel: &DownloadCancellation,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<PathBuf> {
    let preset = get_preset(id)?;
    if let Some(reason) = model_availability(preset, physical_memory_bytes()) {
        bail!(reason);
    }
    ensure!(
        !preset.requires_license_acceptance || accept_license,
        "Read and accept {} before downloading: {}. CLI downloads require --accept-model-license.",
        preset.license_name,
        preset.license_url
    );
    let target = model_path(preset)?;
    cancel.check()?;
    crate::paths::private_directory(target.parent().context("Model has no storage directory")?)?;
    progress(0, preset.size_bytes);
    if is_installed(preset) && digest_file(&target, cancel)? == preset.sha256 {
        cancel.check()?;
        progress(preset.size_bytes, preset.size_bytes);
        cancel.check()?;
        return Ok(target);
    }
    let url = format!(
        "https://huggingface.co/{}/resolve/{}/{}",
        preset.repo, preset.revision, preset.filename
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(download_to(preset, &url, &target, cancel, progress))?;
    Ok(target)
}

async fn download_to(
    preset: &ModelPreset,
    url: &str,
    target: &Path,
    cancel: &DownloadCancellation,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<()> {
    ensure!(
        url::Url::parse(url)?.scheme() == "https",
        "Model download requires HTTPS."
    );
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .user_agent("inklow/0.2")
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.url().scheme() != "https" {
                attempt.error("Refusing a non-HTTPS model download redirect.")
            } else if attempt.previous().len() >= 10 {
                attempt.error("Too many model download redirects.")
            } else {
                attempt.follow()
            }
        }))
        .build()?;
    let mut response = tokio::select! {
        _ = cancel.cancelled() => { cancel.check()?; unreachable!() },
        result = client.get(url).send() => result?.error_for_status()?,
    };
    let parent = target.parent().context("Model target has no parent")?;
    let mut staging = tempfile::Builder::new()
        .prefix(".download-")
        .tempfile_in(parent)?;
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
        let Some(chunk) = next else {
            break;
        };
        // reqwest's transport chunks can be larger; process/progress remain bounded to 64 KiB.
        for part in chunk.chunks(64 * 1024) {
            cancel.check()?;
            count = count
                .checked_add(part.len() as u64)
                .context("Model download size overflow")?;
            ensure!(
                count <= preset.size_bytes,
                "Model download exceeds its verified size."
            );
            digest.update(part);
            staging.write_all(part)?;
            progress(count, preset.size_bytes);
        }
    }
    cancel.check()?;
    verify_download(preset, count, &format!("{:x}", digest.finalize()))?;
    staging.as_file().sync_all()?;
    cancel.check()?;
    staging
        .persist(target)
        .map_err(|e| e.error)
        .context("Could not atomically install verified model")?;
    Ok(())
}

fn verify_download(preset: &ModelPreset, size: u64, hash: &str) -> Result<()> {
    ensure!(
        size == preset.size_bytes && hash == preset.sha256,
        "Model integrity verification failed; the existing model was not replaced."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn retention_fixture() -> (tempfile::TempDir, ModelPreset, PathBuf, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let mut preset = get_preset("tiny").unwrap().clone();
        let bytes = b"synthetic pinned model; no user text";
        preset.filename = "fixture.gguf".into();
        preset.size_bytes = bytes.len() as u64;
        preset.sha256 = format!("{:x}", Sha256::digest(bytes));
        let legacy = root.path().join("cache/models");
        let durable = root.path().join("data/models");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join(&preset.filename), bytes).unwrap();
        (root, preset, legacy, durable)
    }

    #[test]
    fn catalogue_selection_verifies_existing_bytes_without_publication_or_replacement() {
        let (_root, mut preset, legacy, durable) = retention_fixture();
        let source = legacy.join(&preset.filename);
        // Published SHA-256 test vector; independent of the hashing helper.
        preset.size_bytes = 3;
        preset.sha256 = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into();
        let original = b"abc".to_vec();
        std::fs::write(&source, &original).unwrap();
        verify_catalogue_file(&preset, &source, &DownloadCancellation::new()).unwrap();
        assert_eq!(std::fs::read(&source).unwrap(), original);
        assert!(!durable.exists());

        let mut corrupt = original.clone();
        corrupt[0] ^= 1;
        std::fs::write(&source, &corrupt).unwrap();
        assert!(verify_catalogue_file(&preset, &source, &DownloadCancellation::new()).is_err());
        assert_eq!(std::fs::read(&source).unwrap(), corrupt);
        std::fs::write(&source, b"short").unwrap();
        assert!(verify_catalogue_file(&preset, &source, &DownloadCancellation::new()).is_err());
        assert_eq!(std::fs::read(&source).unwrap(), b"short");
        std::fs::remove_file(&source).unwrap();
        assert!(verify_catalogue_file(&preset, &source, &DownloadCancellation::new()).is_err());
        assert!(!source.exists());
        assert!(!durable.exists());
    }

    #[test]
    fn cancelled_catalogue_selection_keeps_the_existing_file() {
        let (_root, preset, legacy, _durable) = retention_fixture();
        let source = legacy.join(&preset.filename);
        let original = std::fs::read(&source).unwrap();
        let cancel = DownloadCancellation::new();
        cancel.set();
        assert!(verify_catalogue_file(&preset, &source, &cancel).is_err());
        assert_eq!(std::fs::read(&source).unwrap(), original);
        assert_eq!(std::fs::read_dir(&legacy).unwrap().count(), 1);
    }

    #[test]
    #[cfg(unix)]
    fn catalogue_selection_refuses_symlinks_and_nonregular_files() {
        let (_root, preset, legacy, _durable) = retention_fixture();
        let source = legacy.join(&preset.filename);
        let link = legacy.join("alias.gguf");
        std::os::unix::fs::symlink(&source, &link).unwrap();
        assert!(verify_catalogue_file(&preset, &link, &DownloadCancellation::new()).is_err());
        assert!(verify_catalogue_file(&preset, &legacy, &DownloadCancellation::new()).is_err());
        verify_catalogue_file(&preset, &source, &DownloadCancellation::new()).unwrap();
    }

    #[test]
    fn verified_retention_preserves_rollback_bytes_and_survives_cache_purge() {
        let (_root, preset, legacy, durable) = retention_fixture();
        let source = legacy.join(&preset.filename);
        let target = durable.join(&preset.filename);
        let bytes = std::fs::read(&source).unwrap();
        assert!(
            retain_legacy_model(&preset, &legacy, &durable, &DownloadCancellation::new()).unwrap()
        );
        assert_eq!(std::fs::read(&source).unwrap(), bytes);
        assert_eq!(std::fs::read(&target).unwrap(), bytes);
        std::fs::remove_dir_all(&legacy).unwrap();
        assert!(
            retain_legacy_model(&preset, &legacy, &durable, &DownloadCancellation::new()).unwrap()
        );
        assert_eq!(std::fs::read(&target).unwrap(), bytes);
        assert_eq!(std::fs::read_dir(&durable).unwrap().count(), 1);
    }

    #[test]
    fn read_only_weights_are_retained_without_requiring_write_access() {
        let (_root, preset, legacy, durable) = retention_fixture();
        let source = legacy.join(&preset.filename);
        let target = durable.join(&preset.filename);
        let original = std::fs::metadata(&source).unwrap().permissions();
        let mut read_only = original.clone();
        read_only.set_readonly(true);
        std::fs::set_permissions(&source, read_only).unwrap();
        let retained =
            retain_legacy_model(&preset, &legacy, &durable, &DownloadCancellation::new());
        std::fs::set_permissions(&source, original).unwrap();
        assert!(retained.unwrap());
        assert_eq!(
            std::fs::read(&source).unwrap(),
            std::fs::read(&target).unwrap()
        );
        assert_eq!(std::fs::metadata(&target).unwrap().len(), preset.size_bytes);
    }

    #[test]
    fn cancellation_before_publication_preserves_source_and_destination() {
        for copying in [true, false] {
            let (_root, preset, legacy, durable) = retention_fixture();
            let source = legacy.join(&preset.filename);
            let bytes = std::fs::read(&source).unwrap();
            let cancel = DownloadCancellation::new();
            let result = retain_legacy_model_with_link(
                &preset,
                &legacy,
                &durable,
                &cancel,
                |source, staging| {
                    if !copying {
                        std::fs::hard_link(source, staging)?;
                    }
                    cancel.set();
                    if copying {
                        Err(std::io::Error::from(std::io::ErrorKind::CrossesDevices))
                    } else {
                        Ok(())
                    }
                },
            );
            assert!(result.is_err());
            assert_eq!(std::fs::read(&source).unwrap(), bytes);
            assert!(!durable.join(&preset.filename).exists());
            assert_eq!(std::fs::read_dir(&durable).unwrap().count(), 0);
        }
    }

    #[test]
    fn copy_cancellation_is_observed_between_chunks() {
        struct CancelAfterWrite {
            cancel: DownloadCancellation,
            written: usize,
        }
        impl Write for CancelAfterWrite {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.written += bytes.len();
                self.cancel.set();
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let (_root, mut preset, legacy, _durable) = retention_fixture();
        let source = legacy.join(&preset.filename);
        let bytes = vec![42_u8; 2 * 1024 * 1024];
        std::fs::write(&source, &bytes).unwrap();
        preset.size_bytes = bytes.len() as u64;
        preset.sha256 = format!("{:x}", Sha256::digest(&bytes));
        let mut input = File::open(&source).unwrap();
        let cancel = DownloadCancellation::new();
        let mut writer = CancelAfterWrite {
            cancel: cancel.clone(),
            written: 0,
        };
        assert!(copy_retained_file(&preset, &mut input, &mut writer, &cancel).is_err());
        assert_eq!(writer.written, 1024 * 1024);
        assert_eq!(std::fs::read(&source).unwrap(), bytes);
    }

    #[test]
    fn unsupported_links_copy_verified_bytes_without_deleting_source() {
        let (_root, preset, legacy, durable) = retention_fixture();
        assert!(
            retain_legacy_model_with_link(
                &preset,
                &legacy,
                &durable,
                &DownloadCancellation::new(),
                |_, _| { Err(std::io::Error::from(std::io::ErrorKind::CrossesDevices)) }
            )
            .unwrap()
        );
        let source = legacy.join(&preset.filename);
        let target = durable.join(&preset.filename);
        assert_eq!(
            std::fs::read(&source).unwrap(),
            std::fs::read(&target).unwrap()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_ne!(
                source.metadata().unwrap().ino(),
                target.metadata().unwrap().ino()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn same_filesystem_retention_links_without_duplicating_weights() {
        use std::os::unix::fs::MetadataExt;
        let (_root, preset, legacy, durable) = retention_fixture();
        assert!(
            retain_legacy_model(&preset, &legacy, &durable, &DownloadCancellation::new()).unwrap()
        );
        let source = legacy.join(&preset.filename).metadata().unwrap();
        let target = durable.join(&preset.filename).metadata().unwrap();
        assert_eq!((source.dev(), source.ino()), (target.dev(), target.ino()));
    }

    #[test]
    fn concurrent_publication_is_verified_and_never_clobbered() {
        for valid in [true, false] {
            let (_root, preset, legacy, durable) = retention_fixture();
            let target = durable.join(&preset.filename);
            let original = std::fs::read(legacy.join(&preset.filename)).unwrap();
            let competing = if valid {
                original.clone()
            } else {
                vec![b'X'; original.len()]
            };
            let result = retain_legacy_model_with_link(
                &preset,
                &legacy,
                &durable,
                &DownloadCancellation::new(),
                |source, staging| {
                    std::fs::hard_link(source, staging)?;
                    std::fs::write(&target, &competing)
                },
            );
            assert_eq!(result.is_ok(), valid);
            assert_eq!(std::fs::read(&target).unwrap(), competing);
            assert_eq!(
                std::fs::read(legacy.join(&preset.filename)).unwrap(),
                original
            );
            assert_eq!(std::fs::read_dir(&durable).unwrap().count(), 1);
        }
    }

    #[test]
    fn retention_never_replaces_conflicting_destination_or_copies_invalid_source() {
        let (_root, preset, legacy, durable) = retention_fixture();
        std::fs::create_dir_all(&durable).unwrap();
        let target = durable.join(&preset.filename);
        let source = legacy.join(&preset.filename);
        let original = std::fs::read(&source).unwrap();
        let conflict = vec![b'X'; original.len()];
        std::fs::write(&target, &conflict).unwrap();
        assert!(
            retain_legacy_model(&preset, &legacy, &durable, &DownloadCancellation::new()).is_err()
        );
        assert_eq!(std::fs::read(&target).unwrap(), conflict);
        assert_eq!(std::fs::read(&source).unwrap(), original);
        std::fs::remove_file(&target).unwrap();
        std::fs::write(&source, &conflict).unwrap();
        assert!(
            retain_legacy_model(&preset, &legacy, &durable, &DownloadCancellation::new()).is_err()
        );
        assert!(!target.exists());
        assert_eq!(std::fs::read(&source).unwrap(), conflict);
        assert_eq!(std::fs::read_dir(&durable).unwrap().count(), 0);
    }

    #[test]
    fn missing_models_are_not_downloaded_and_failed_retention_keeps_source() {
        let (root, preset, legacy, durable) = retention_fixture();
        let absent = root.path().join("absent");
        assert!(
            !retain_legacy_model(&preset, &absent, &durable, &DownloadCancellation::new()).unwrap()
        );
        assert!(!durable.exists());
        let blocked = root.path().join("blocked");
        std::fs::write(&blocked, b"not a directory").unwrap();
        let source = legacy.join(&preset.filename);
        let bytes = std::fs::read(&source).unwrap();
        assert!(
            retain_legacy_model(
                &preset,
                &legacy,
                &blocked.join("models"),
                &DownloadCancellation::new()
            )
            .is_err()
        );
        assert_eq!(std::fs::read(&source).unwrap(), bytes);
    }

    #[cfg(unix)]
    #[test]
    fn retention_refuses_sources_writable_by_other_users() {
        use std::os::unix::fs::PermissionsExt;
        let (_root, preset, legacy, durable) = retention_fixture();
        let source = legacy.join(&preset.filename);
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(
            retain_legacy_model(&preset, &legacy, &durable, &DownloadCancellation::new()).is_err()
        );
        assert!(source.exists());
        assert!(!durable.exists());
    }

    #[cfg(unix)]
    #[test]
    fn retention_refuses_symlink_files_and_directories() {
        use std::os::unix::fs::symlink;
        let (root, preset, legacy, durable) = retention_fixture();
        let source = legacy.join(&preset.filename);
        let other = root.path().join("other.gguf");
        std::fs::rename(&source, &other).unwrap();
        symlink(&other, &source).unwrap();
        assert!(
            retain_legacy_model(&preset, &legacy, &durable, &DownloadCancellation::new()).is_err()
        );
        assert!(!durable.exists());
        std::fs::remove_file(&source).unwrap();
        std::fs::rename(&other, &source).unwrap();
        let redirected = root.path().join("redirected");
        std::fs::create_dir(&redirected).unwrap();
        std::fs::create_dir_all(durable.parent().unwrap()).unwrap();
        symlink(&redirected, &durable).unwrap();
        assert!(
            retain_legacy_model(&preset, &legacy, &durable, &DownloadCancellation::new()).is_err()
        );
        assert_eq!(std::fs::read_dir(&redirected).unwrap().count(), 0);
        assert!(source.is_file());
    }

    #[test]
    fn ram_labels_use_binary_gibibytes_and_preserve_fractions() {
        for (bytes, expected) in [
            (8_589_934_592, "8.00 GiB"),
            (9_126_805_504, "8.50 GiB"),
            (536_870_912, "0.50 GiB"),
            (1_000_000_000, "0.93 GiB"),
        ] {
            assert_eq!(ram_label(Some(bytes), "unavailable").to_string(), expected);
        }
    }

    #[test]
    fn fractional_memory_requirements_keep_byte_exact_eligibility() {
        let mut preset = get_preset("gemma4-e2b").unwrap().clone();
        preset.minimum_memory_bytes = 9_126_805_504;
        let warning = model_availability(&preset, Some(9_126_805_503)).unwrap();
        assert!(warning.contains("8.50 GiB"));
        assert!(model_availability(&preset, Some(9_126_805_504)).is_none());
        assert!(model_availability(&preset, Some(9_126_805_505)).is_none());
        assert!(model_availability(&preset, None).is_none());

        preset.minimum_memory_bytes = 0;
        assert!(model_availability(&preset, Some(0)).is_none());
        assert!(model_availability(&preset, Some(9_126_805_504)).is_none());
        assert!(model_availability(&preset, None).is_none());
    }

    #[test]
    fn capacity_policy_distinguishes_unknown_and_low_memory() {
        assert_eq!(recommended_preset(None).id, "gemma4-e2b");
        assert_eq!(recommended_preset(Some(4 * 1024_u64.pow(3))).id, "balanced");
        assert_eq!(recommended_preset(Some(8_589_934_591)).id, "balanced");
        assert_eq!(recommended_preset(Some(8_589_934_592)).id, "gemma4-e2b");
        assert_eq!(recommended_preset(Some(8_589_934_593)).id, "gemma4-e2b");
        assert!(
            model_availability(get_preset("gemma3-1b").unwrap(), Some(3 * 1024_u64.pow(3)))
                .is_some()
        );
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
