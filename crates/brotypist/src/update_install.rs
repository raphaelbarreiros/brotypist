//! Offline, unprivileged native update transaction. No generic replace fallback.
//!
//! The application must call `startup_guard` before other initialization and keep
//! its returned guard until exit. Only its first real GUI-loop turn may call ACK.
//! A cancelled transaction makes the replacement terminate *itself*, so rollback
//! never guesses that a LaunchServices launcher PID is the application PID.
use crate::{
    update_keys::APP_ID,
    updates::{
        self, MAX_ARCHIVE_ENTRIES, MAX_MANIFEST_BYTES, MAX_PACKAGE_BYTES, MAX_SIGNATURE_BYTES,
        MAX_UNPACKED_BYTES, UpdateAsset, UpdateRelease,
    },
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use fs2::FileExt;
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::{
    fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd},
    unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        process::CommandExt,
    },
};
use std::{
    collections::HashSet,
    ffi::CString,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, LazyLock, Mutex},
    thread,
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;

pub const MARKER: &str = ".inklow-build.json";
pub const HEALTH_PATH: &str = "INKLOW_UPDATE_HEALTH_RECEIPT";
pub const HEALTH_NONCE: &str = "INKLOW_UPDATE_HEALTH_NONCE";
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(300);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);
const STOP_TIMEOUT: Duration = Duration::from_secs(20);
const FORBIDDEN_NAMES: &[&str] = &[
    "settings.json",
    "config.json",
    "secrets.yaml",
    ".env",
    "history.enc",
    ".git",
];
const FORBIDDEN_SUFFIXES: &[&str] = &[
    "gguf",
    "ggml",
    "safetensors",
    "pt",
    "pth",
    "db",
    "sqlite",
    "sqlite3",
];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Installation {
    pub path: Option<PathBuf>,
    pub kind: String,
    pub installable: bool,
    pub reason: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct BuildIdentity {
    application: String,
    platform: String,
    version: String,
    source_commit: String,
    architecture: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    schema: u32,
    destination: PathBuf,
    platform: String,
    identity: BuildIdentity,
    device: u64,
    inode: u64,
    uid: u32,
    parent_pid: u32,
    isolated_home: Option<PathBuf>,
    nonce: String,
    version: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ProcessIdentity {
    pid: u32,
    start: String,
    uid: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ParentOwnership {
    schema: u32,
    nonce: String,
    process: ProcessIdentity,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TransactionReceipt {
    schema: u32,
    application: String,
    version: String,
    status: String,
    previous_version: String,
    notarization: String,
    #[serde(default)]
    rollback: Option<PathBuf>,
    #[serde(default)]
    message: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StartupLease {
    nonce: String,
    version: String,
    process: ProcessIdentity,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StartupAck {
    nonce: String,
    version: String,
    pid: u32,
}

fn native() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin"
    } else {
        std::env::consts::OS
    }
}
fn architecture() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        a => a,
    }
}
fn nonce_matches(left: &str, right: &str) -> bool {
    bool::from(left.as_bytes().ct_eq(right.as_bytes()))
}
fn random_hex(bytes: usize) -> String {
    let mut data = vec![0; bytes];
    OsRng.fill_bytes(&mut data);
    data.iter().map(|v| format!("{v:02x}")).collect()
}
fn executable(bundle: &Path, platform: &str) -> PathBuf {
    bundle.join(if platform == "darwin" {
        "Contents/MacOS/BroTypist"
    } else {
        "BroTypist"
    })
}

pub(crate) fn no_symlink_components(path: &Path) -> Result<()> {
    ensure!(
        path.is_absolute()
            && !path
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir)),
        "Application path must be canonical."
    );
    for p in path.ancestors() {
        match fs::symlink_metadata(p) {
            Ok(m) => ensure!(
                !m.file_type().is_symlink(),
                "Linked application paths cannot be updated."
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
pub(crate) fn require_private_directory(path: &Path) -> Result<()> {
    no_symlink_components(path)?;
    let m = fs::symlink_metadata(path)?;
    ensure!(
        m.is_dir() && !m.file_type().is_symlink(),
        "Invalid private update directory."
    );
    #[cfg(unix)]
    ensure!(
        m.uid() == unsafe { libc::geteuid() } && m.mode() & 0o077 == 0,
        "The update directory must be accessible only to its owner."
    );
    #[cfg(not(unix))]
    bail!("Native OTA is held for this platform.");
    Ok(())
}
fn open_regular(path: &Path, private: bool) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    let f = options.open(path)?;
    let m = f.metadata()?;
    ensure!(m.is_file(), "Unsafe installer input.");
    #[cfg(unix)]
    {
        ensure!(
            m.nlink() == 1,
            "Hardlinked installer input is not accepted."
        );
        if private {
            ensure!(
                m.uid() == unsafe { libc::geteuid() } && m.mode() & 0o077 == 0,
                "Installer input must be private."
            );
        }
    }
    #[cfg(not(unix))]
    {
        let _ = private;
        ensure!(
            !fs::symlink_metadata(path)?.file_type().is_symlink(),
            "Linked input refused."
        );
    }
    Ok(f)
}
pub(crate) fn read_file(path: &Path, maximum: usize, private: bool) -> Result<Vec<u8>> {
    let mut f = open_regular(path, private)?;
    ensure!(
        f.metadata()?.len() <= maximum as u64,
        "Installer input exceeds its limit."
    );
    let mut data = Vec::new();
    (&mut f).take(maximum as u64 + 1).read_to_end(&mut data)?;
    ensure!(
        data.len() <= maximum,
        "Installer input grew beyond its limit."
    );
    Ok(data)
}
fn write_exclusive(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
fn copy_private(source: &Path, destination: &Path, maximum: u64) -> Result<()> {
    let mut input = open_regular(source, false)?;
    let size = input.metadata()?.len();
    ensure!(size > 0 && size <= maximum, "Invalid update input size.");
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let mut output = options.open(destination)?;
    let count = std::io::copy(&mut (&mut input).take(maximum + 1), &mut output)?;
    ensure!(
        count == size && count <= maximum,
        "Update input changed while copying."
    );
    output.sync_all()?;
    preserve_quarantine(source, destination)?;
    Ok(())
}
fn marker(bundle: &Path, platform: &str) -> Result<BuildIdentity> {
    let resources = bundle.join(match platform {
        "darwin" => "Contents/Resources",
        "linux" => "share",
        _ => "assets",
    });
    let development = resources.join(".inklow-development.json");
    no_symlink_components(&development)?;
    match fs::symlink_metadata(&development) {
        Ok(_) => {
            bail!("Local development applications are not eligible for signed OTA installation.")
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let path = bundle.join(if platform == "darwin" {
        "Contents/Resources/.inklow-build.json"
    } else {
        MARKER
    });
    no_symlink_components(&path)?;
    let identity: BuildIdentity =
        serde_json::from_value(updates::strict_json(&read_file(&path, 8192, false)?)?)?;
    ensure!(
        identity.application == APP_ID
            && identity.platform == platform
            && identity.architecture == architecture()
            && updates::lower_hex(&identity.source_commit, 40),
        "Application build identity does not match this computer."
    );
    updates::version(&identity.version)?;
    Ok(identity)
}
fn run_output(command: &mut Command, timeout: Duration) -> Result<Vec<u8>> {
    // Files avoid pipe deadlocks; both output streams are bounded while the owned
    // child is running. Killing an unreaped Child never targets a recycled PID.
    let stdout = tempfile::tempfile()?;
    let stderr = tempfile::tempfile()?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?)
        .spawn()?;
    let deadline = Instant::now() + timeout;
    loop {
        if stdout.metadata()?.len() + stderr.metadata()?.len() > 128 * 1024
            || Instant::now() >= deadline
        {
            let _ = child.kill();
            let _ = child.wait();
            bail!("Native verification exceeded its time or output limit.");
        }
        if let Some(status) = child.try_wait()? {
            ensure!(status.success(), "Native verification failed.");
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    use std::io::{Seek, SeekFrom};
    let mut result = Vec::new();
    let mut out = stdout;
    let mut err = stderr;
    out.seek(SeekFrom::Start(0))?;
    err.seek(SeekFrom::Start(0))?;
    out.take(128 * 1024 + 1).read_to_end(&mut result)?;
    err.take(128 * 1024 + 1).read_to_end(&mut result)?;
    ensure!(
        result.len() <= 128 * 1024,
        "Native verification output exceeded its limit."
    );
    Ok(result)
}
fn managed_debian(bundle: &Path) -> Result<bool> {
    if bundle.starts_with("/opt/brotypist") {
        return Ok(true);
    }
    if Path::new("/usr/bin/dpkg-query").is_file() {
        let out = tempfile::tempfile()?;
        let mut child = Command::new("/usr/bin/dpkg-query")
            .args(["-S"])
            .arg(bundle.join("BroTypist"))
            .stdin(Stdio::null())
            .stdout(out.try_clone()?)
            .stderr(Stdio::null())
            .spawn()?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(status.success());
            }
            if Instant::now() >= deadline || out.metadata()?.len() > 64 * 1024 {
                let _ = child.kill();
                let _ = child.wait();
                bail!("Package ownership could not be established safely.");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    Ok(false)
}
fn binary_architecture(path: &Path, platform: &str) -> Result<()> {
    let mut f = open_regular(path, false)?;
    let mut header = [0u8; 64];
    let count = f.read(&mut header)?;
    let expected = architecture();
    let actual = if platform == "darwin" {
        ensure!(
            count >= 8 && header[..4] == [0xcf, 0xfa, 0xed, 0xfe],
            "Unsupported native executable header."
        );
        match u32::from_le_bytes(header[4..8].try_into()?) {
            0x100000c => "arm64",
            0x1000007 => "x86_64",
            _ => "unknown",
        }
    } else {
        ensure!(
            count >= 20 && header[..6] == [0x7f, b'E', b'L', b'F', 2, 1],
            "Unsupported native executable header."
        );
        match u16::from_le_bytes(header[18..20].try_into()?) {
            183 => "arm64",
            62 => "x86_64",
            _ => "unknown",
        }
    };
    ensure!(
        actual == expected,
        "Native executable architecture does not match this computer."
    );
    Ok(())
}
fn target_policy(bundle: &Path, platform: &str, isolated: Option<&Path>) -> Result<BuildIdentity> {
    ensure!(
        platform == "darwin" || platform == "linux",
        "Native OTA is held for this platform."
    );
    no_symlink_components(bundle)?;
    let info = fs::symlink_metadata(bundle)?;
    let parent_path = bundle.parent().context("Missing application parent.")?;
    let parent = fs::metadata(parent_path)?;
    ensure!(info.is_dir(), "Invalid native application directory.");
    #[cfg(unix)]
    {
        let uid = unsafe { libc::geteuid() };
        let mut groups =
            vec![0; unsafe { libc::getgroups(0, std::ptr::null_mut()) }.max(0) as usize];
        if !groups.is_empty() {
            ensure!(
                unsafe { libc::getgroups(groups.len() as i32, groups.as_mut_ptr()) } >= 0,
                "Could not inspect application ownership."
            );
        }
        let parent_c = CString::new(parent_path.as_os_str().as_bytes())?;
        ensure!(
            info.uid() == uid
                && info.mode() & 0o022 == 0
                && parent.mode() & 0o002 == 0
                && (parent.mode() & 0o020 == 0 || groups.contains(&parent.gid()))
                && [0, uid].contains(&parent.uid())
                && unsafe { libc::access(parent_c.as_ptr(), libc::W_OK | libc::X_OK) } == 0,
            "Application directory is not privately writable."
        );
        ensure!(
            info.dev() == parent.dev(),
            "Application and staging parent must share a filesystem."
        );
    }
    for p in bundle.ancestors() {
        ensure!(
            !p.join(".git").exists(),
            "Source checkouts cannot replace themselves."
        );
    }
    if platform == "linux" {
        ensure!(
            !managed_debian(bundle)?,
            "Use the system package installer for this installation."
        );
    }
    if let Some(home) = isolated {
        let home = home.canonicalize()?;
        ensure!(
            bundle != home && bundle.starts_with(&home),
            "An isolated profile can update only its own QA application copy."
        );
    }
    let identity = marker(bundle, platform)?;
    let exe = executable(bundle, platform);
    no_symlink_components(&exe)?;
    let file = open_regular(&exe, false)?;
    #[cfg(unix)]
    ensure!(
        file.metadata()?.mode() & 0o111 != 0,
        "Native application executable is not executable."
    );
    binary_architecture(&exe, platform)?;
    Ok(identity)
}
pub fn detect_installation() -> Installation {
    let held = |path, kind: &str, reason: &str| Installation {
        path,
        kind: kind.into(),
        installable: false,
        reason: reason.into(),
    };
    if !cfg!(any(target_os = "macos", target_os = "linux")) {
        return held(
            None,
            "held",
            "Updates are not available for this platform. Windows OTA remains held.",
        );
    }
    let Ok(exe) = std::env::current_exe() else {
        return held(
            None,
            "read_only",
            "The executable identity could not be established safely.",
        );
    };
    if exe.file_name().and_then(|s| s.to_str()) != Some("BroTypist") {
        return held(
            None,
            "source",
            "Source checkouts must be updated from source.",
        );
    }
    let (bundle, kind) = if cfg!(target_os = "macos") {
        if exe
            .parent()
            .and_then(Path::file_name)
            .and_then(|p| p.to_str())
            != Some("MacOS")
            || exe
                .parent()
                .and_then(Path::parent)
                .and_then(Path::file_name)
                .and_then(|p| p.to_str())
                != Some("Contents")
        {
            return held(None, "held", "Unrecognized native application layout.");
        }
        (
            exe.ancestors().nth(3).unwrap().to_path_buf(),
            "macos_bundle",
        )
    } else {
        let bundle = exe.parent().unwrap().to_path_buf();
        match managed_debian(&bundle) {
            Ok(true) => {
                return held(
                    Some(bundle),
                    "linux_debian",
                    "Open the downloaded Debian package in your system installer; restart afterwards.",
                );
            }
            Err(_) => {
                return held(
                    Some(bundle),
                    "read_only",
                    "Package ownership could not be established safely.",
                );
            }
            _ => {}
        }
        (bundle, "linux_portable")
    };
    if bundle.ancestors().any(|p| p.join(".git").exists()) {
        return held(
            None,
            "source",
            "Source checkouts must be updated from source.",
        );
    }
    let isolated = std::env::var_os("INKLOW_HOME").map(PathBuf::from);
    if target_policy(&bundle, native(), isolated.as_deref()).is_err() {
        return held(
            Some(bundle),
            "read_only",
            "This app cannot replace itself here. Use a writable native copy; isolated profiles can update only their own QA copy.",
        );
    }
    Installation {
        path: Some(bundle),
        kind: kind.into(),
        installable: true,
        reason: "Install and relaunch this native application.".into(),
    }
}

pub struct Handoff {
    #[cfg(unix)]
    writer: Option<OwnedFd>,
    process: Child,
    pub request_root: PathBuf,
    committed: bool,
}
impl Handoff {
    pub fn committed(&self) -> bool {
        self.committed
    }
    pub fn helper_pid(&self) -> u32 {
        self.process.id()
    }
    pub fn commit(&mut self) -> Result<()> {
        if self.committed {
            return Ok(());
        }
        ensure!(
            self.process.try_wait()?.is_none(),
            "Update helper is no longer available."
        );
        #[cfg(unix)]
        {
            let writer = self
                .writer
                .as_ref()
                .context("The update handoff was aborted.")?;
            let mut file = File::from(writer.try_clone()?);
            file.write_all(b"1")?;
            // Retain a noninheritable writer until actual process exit. Dropping
            // the service/UI handle cannot turn approval into false parent exit.
            let fd = self.writer.take().unwrap().into_raw_fd();
            COMMITTED_WRITERS.lock().unwrap().push(fd);
            self.committed = true;
            Ok(())
        }
        #[cfg(not(unix))]
        bail!("Native OTA is held for this platform.")
    }
    pub fn abort(&mut self) {
        if !self.committed {
            #[cfg(unix)]
            {
                self.writer.take();
            }
        }
    }
}
impl Drop for Handoff {
    fn drop(&mut self) {
        self.abort();
    }
}
#[cfg(unix)]
static COMMITTED_WRITERS: LazyLock<Mutex<Vec<RawFd>>> = LazyLock::new(|| Mutex::new(Vec::new()));
fn clean_command(exe: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(exe);
    command.env_clear();
    for (key, value) in std::env::vars_os() {
        let s = key.to_string_lossy();
        if !["TMPDIR", "TEMP", "TMP"].contains(&s.as_ref())
            && ![
                "PYTHON",
                "DYLD_",
                "LD_",
                "_PYI_",
                "_MEI",
                "INKLOW_UPDATE_HEALTH_",
            ]
            .iter()
            .any(|p| s.starts_with(p))
        {
            command.env(key, value);
        }
    }
    command
}
fn bundled_helper(bundle: &Path, platform: &str) -> Result<PathBuf> {
    let relative = match platform {
        "darwin" => "Contents/Helpers/BroTypist-updater",
        "linux" => "helpers/BroTypist-updater",
        _ => bail!("Native OTA is held for this platform."),
    };
    let helper = bundle.join(relative);
    no_symlink_components(&helper)?;
    let metadata = open_regular(&helper, false)?.metadata()?;
    ensure!(
        metadata.len() > 0 && metadata.len() <= 128 * 1024 * 1024,
        "Standalone update helper has an invalid size."
    );
    #[cfg(unix)]
    ensure!(
        [0, unsafe { libc::geteuid() }].contains(&metadata.uid())
            && metadata.mode() & 0o022 == 0
            && metadata.mode() & 0o111 != 0,
        "Standalone update helper ownership or permissions are unsafe."
    );
    binary_architecture(&helper, platform)?;
    Ok(helper)
}
pub fn spawn_installer(
    manifest: &Path,
    signature: &Path,
    archive: &Path,
    installation: &Installation,
) -> Result<Handoff> {
    #[cfg(not(unix))]
    {
        let _ = (manifest, signature, archive, installation);
        bail!("Native OTA is held for this platform.");
    }
    #[cfg(unix)]
    {
        ensure!(
            installation.installable && *installation == detect_installation(),
            "Application installation changed before updating."
        );
        let bundle = installation
            .path
            .as_ref()
            .context("Missing application destination.")?;
        let isolated = std::env::var_os("INKLOW_HOME").map(PathBuf::from);
        let identity = target_policy(bundle, native(), isolated.as_deref())?;
        let source_helper = bundled_helper(bundle, native())?;
        // Authenticate the installed bundle's nested helper before executing
        // its private copy; the standalone binary has no llama runtime closure.
        if native() == "darwin" {
            signing_identity(bundle)?;
        }
        let root = tempfile::Builder::new()
            .prefix(".brotypist-update-")
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(bundle.parent().unwrap())?;
        require_private_directory(root.path())?;
        copy_private(
            manifest,
            &root.path().join("manifest.json"),
            MAX_MANIFEST_BYTES as u64,
        )?;
        copy_private(
            signature,
            &root.path().join("manifest.sig"),
            MAX_SIGNATURE_BYTES as u64,
        )?;
        copy_private(archive, &root.path().join("package"), MAX_PACKAGE_BYTES)?;
        let helper = root.path().join("BroTypist-updater");
        copy_private(&source_helper, &helper, 128 * 1024 * 1024)?;
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700))?;
        binary_architecture(&helper, native())?;
        if native() == "darwin" {
            run_output(
                Command::new("/usr/bin/codesign")
                    .args(["--verify", "--strict"])
                    .arg(&helper),
                Duration::from_secs(60),
            )?;
        }
        let release = updates::verify_manifest(
            &read_file(&root.path().join("manifest.json"), MAX_MANIFEST_BYTES, true)?,
            &read_file(&root.path().join("manifest.sig"), MAX_SIGNATURE_BYTES, true)?,
        )?;
        ensure!(
            updates::version(&release.version)? > updates::version(&identity.version)?,
            "An update must be newer than the installed version."
        );
        let metadata = fs::metadata(bundle)?;
        let parent = process_identity(std::process::id())?
            .context("Cannot establish parent process identity.")?;
        let request = Request {
            schema: 1,
            destination: bundle.clone(),
            platform: native().into(),
            identity,
            device: metadata.dev(),
            inode: metadata.ino(),
            uid: unsafe { libc::geteuid() },
            parent_pid: parent.pid,
            isolated_home: isolated,
            nonce: random_hex(32),
            version: release.version,
        };
        write_exclusive(
            &root.path().join("request.json"),
            &serde_json::to_vec(&request)?,
        )?;
        let ownership = ParentOwnership {
            schema: 1,
            nonce: request.nonce.clone(),
            process: parent,
        };
        write_exclusive(
            &root.path().join("parent.json"),
            &serde_json::to_vec(&ownership)?,
        )?;
        write_exclusive(&root.path().join("startup.lock"), b"")?;
        let mut pipe = [-1; 2];
        ensure!(
            unsafe { libc::pipe(pipe.as_mut_ptr()) } == 0,
            "Could not create update approval pipe."
        );
        let reader = unsafe { OwnedFd::from_raw_fd(pipe[0]) };
        let writer = unsafe { OwnedFd::from_raw_fd(pipe[1]) };
        for fd in [&reader, &writer] {
            ensure!(
                unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } == 0,
                "Could not secure approval pipe."
            );
        }
        let read_fd = reader.as_raw_fd();
        let mut command = clean_command(helper.as_os_str());
        command
            .arg("--request")
            .arg(root.path().join("request.json"))
            .arg("--approval-fd")
            .arg(read_fd.to_string())
            .env("TMPDIR", root.path())
            .current_dir(root.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() < 0 || libc::fcntl(read_fd, libc::F_SETFD, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let process = command.spawn()?;
        drop(reader);
        Ok(Handoff {
            writer: Some(writer),
            process,
            request_root: root.keep(),
            committed: false,
        })
    }
}
fn request(path: &Path) -> Result<Request> {
    no_symlink_components(path)?;
    ensure!(
        path.file_name().and_then(|s| s.to_str()) == Some("request.json"),
        "Invalid update request basename."
    );
    let root = path.parent().context("Missing update transaction.")?;
    require_private_directory(root)?;
    ensure!(
        root.file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.starts_with(".brotypist-update-")),
        "Invalid private update transaction."
    );
    let r: Request = serde_json::from_value(updates::strict_json(&read_file(path, 16384, true)?)?)?;
    #[cfg(unix)]
    ensure!(
        r.uid == unsafe { libc::geteuid() },
        "Invalid update transaction owner."
    );
    ensure!(
        r.schema == 1
            && r.platform == native()
            && updates::lower_hex(&r.nonce, 64)
            && r.device > 0
            && r.inode > 0
            && r.parent_pid > 0,
        "Invalid update transaction identity."
    );
    ensure!(
        r.destination.parent() == root.parent(),
        "Update transaction is not beside its application."
    );
    no_symlink_components(&r.destination)?;
    updates::version(&r.version)?;
    ensure!(
        r.identity.application == APP_ID
            && r.identity.platform == r.platform
            && r.identity.architecture == architecture()
            && updates::lower_hex(&r.identity.source_commit, 40),
        "Invalid previous application identity."
    );
    updates::version(&r.identity.version)?;
    ensure!(
        updates::version(&r.version)? > updates::version(&r.identity.version)?,
        "Update would not advance the installed version."
    );
    Ok(r)
}
fn parent_ownership(root: &Path, r: &Request) -> Result<Option<ProcessIdentity>> {
    let path = root.join("parent.json");
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
        Ok(_) => {}
    }
    let owner: ParentOwnership =
        serde_json::from_value(updates::strict_json(&read_file(&path, 2048, true)?)?)?;
    ensure!(
        owner.schema == 1
            && nonce_matches(&owner.nonce, &r.nonce)
            && owner.process.pid == r.parent_pid
            && owner.process.uid == r.uid
            && !owner.process.start.is_empty(),
        "Invalid original-parent process ownership."
    );
    Ok(Some(owner.process))
}
fn ensure_startup_lock(root: &Path, r: &Request) -> Result<()> {
    if parent_ownership(root, r)?.is_none() {
        // Published schema-1 helpers predate this private lock. Creating it
        // exclusively does not change their request, receipt or ACK contract.
        match write_exclusive(&root.join("startup.lock"), b"") {
            Ok(()) => {}
            Err(e)
                if e.downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::AlreadyExists) => {}
            Err(e) => return Err(e),
        }
    }
    let _ = open_regular(&root.join("startup.lock"), true)?;
    Ok(())
}
fn check_destination(r: &Request) -> Result<PathBuf> {
    let identity = target_policy(&r.destination, &r.platform, r.isolated_home.as_deref())?;
    let metadata = fs::metadata(&r.destination)?;
    ensure!(
        identity == r.identity,
        "The installed application changed during the update."
    );
    #[cfg(unix)]
    ensure!(
        metadata.dev() == r.device && metadata.ino() == r.inode,
        "The installed application inode changed during the update."
    );
    Ok(r.destination.clone())
}
pub(crate) fn verify_package(path: &Path, asset: &UpdateAsset, private: bool) -> Result<()> {
    no_symlink_components(path)?;
    let mut file = open_regular(path, private)?;
    ensure!(
        file.metadata()?.len() == asset.size && asset.size <= MAX_PACKAGE_BYTES,
        "Update package size does not match its signed manifest."
    );
    let mut digest = Sha256::new();
    let mut received = 0u64;
    let mut block = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut block)?;
        if n == 0 {
            break;
        }
        received += n as u64;
        ensure!(
            received <= asset.size,
            "Update package grew beyond its signed size."
        );
        digest.update(&block[..n]);
    }
    ensure!(
        received == asset.size && format!("{:x}", digest.finalize()) == asset.sha256,
        "Update package hash does not match its signed manifest."
    );
    Ok(())
}
fn verified_asset(root: &Path, r: &Request) -> Result<(UpdateRelease, UpdateAsset)> {
    let release = updates::verify_manifest(
        &read_file(&root.join("manifest.json"), MAX_MANIFEST_BYTES, true)?,
        &read_file(&root.join("manifest.sig"), MAX_SIGNATURE_BYTES, true)?,
    )?;
    ensure!(
        release.version == r.version
            && updates::version(&release.version)? > updates::version(&r.identity.version)?,
        "Update does not advance the installed version."
    );
    let target = format!(
        "{}-{}",
        if r.platform == "darwin" {
            "macos"
        } else {
            "linux"
        },
        architecture()
    );
    let format = if r.platform == "darwin" {
        "zip"
    } else {
        "tar.gz"
    };
    let mut matches = release
        .assets
        .iter()
        .filter(|a| a.target == target && a.format == format);
    let asset = matches
        .next()
        .context("Release has no native update package.")?
        .clone();
    ensure!(
        matches.next().is_none(),
        "Release has no unique native update package."
    );
    verify_package(&root.join("package"), &asset, true)?;
    Ok((release, asset))
}

#[derive(Clone, Debug)]
struct Member {
    path: PathBuf,
    link: Option<String>,
    directory: bool,
    size: u64,
    mode: u32,
}
fn archive_path(name: &str, expected_root: &str) -> Result<PathBuf> {
    ensure!(
        !name.is_empty() && !name.contains(['\0', '\\']) && !name.starts_with('/'),
        "Update archive contains an unsafe path."
    );
    let trimmed = name.trim_end_matches('/');
    let parts: Vec<_> = trimmed.split('/').collect();
    ensure!(
        parts.first().copied() == Some(expected_root)
            && parts
                .iter()
                .all(|p| !p.is_empty() && *p != "." && *p != ".."),
        "Update archive contains an unsafe path."
    );
    ensure!(
        !parts
            .iter()
            .any(|p| FORBIDDEN_NAMES.contains(&p.to_lowercase().as_str()))
            && !FORBIDDEN_SUFFIXES.contains(
                &Path::new(trimmed)
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_lowercase()
                    .as_str()
            ),
        "Update archives cannot contain models or private user data."
    );
    Ok(PathBuf::from(trimmed))
}
fn link_target(path: &Path, link: &str, root: &str) -> Result<PathBuf> {
    ensure!(
        !link.is_empty() && !link.starts_with('/') && !link.contains(['\0', '\\']),
        "Update archive contains an unsafe link."
    );
    let mut parts: Vec<_> = path
        .parent()
        .context("Invalid link path.")?
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    for p in link.split('/') {
        match p {
            ".." => {
                ensure!(
                    parts.len() > 1,
                    "Update archive link escapes its application."
                );
                parts.pop();
            }
            "" | "." => {}
            p => parts.push(p.into()),
        }
    }
    ensure!(
        parts.first().map(String::as_str) == Some(root),
        "Update archive link escapes its application."
    );
    Ok(parts.iter().collect())
}
fn fold_path(path: &Path) -> String {
    use crate::unicode_compat::UnicodeCaseFold;
    use unicode_normalization::UnicodeNormalization;
    path.to_string_lossy().nfc().case_fold().collect()
}
fn validate_members(members: &[Member]) -> Result<()> {
    let mut names = HashSet::new();
    let mut non_directories = HashSet::new();
    for member in members {
        let folded = fold_path(&member.path);
        ensure!(
            names.insert(folded.clone()),
            "Update archive has duplicate paths."
        );
        if member.link.is_some() || !member.directory {
            non_directories.insert(folded);
        }
    }
    for member in members {
        ensure!(
            !member
                .path
                .ancestors()
                .skip(1)
                .any(|p| non_directories.contains(&fold_path(p))),
            "Update archive traverses a link or non-directory."
        );
    }
    Ok(())
}
fn check_limit(total: &mut u64, size: u64, count: usize) -> Result<()> {
    *total = total.checked_add(size).context("Archive size overflow.")?;
    ensure!(
        count <= MAX_ARCHIVE_ENTRIES && *total <= MAX_UNPACKED_BYTES,
        "Update archive exceeds its entry or unpacked size limit."
    );
    Ok(())
}
fn zip_members(archive: &Path, root: &str) -> Result<Vec<Member>> {
    let mut zip = zip::ZipArchive::new(open_regular(archive, false)?)?;
    ensure!(
        zip.len() <= MAX_ARCHIVE_ENTRIES,
        "Update archive has too many entries."
    );
    let mut members = Vec::new();
    let mut total = 0;
    let mut raw_names = HashSet::new();
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index)?;
        let raw = entry.name().to_owned();
        ensure!(
            raw_names.insert(fold_path(Path::new(&raw))),
            "Duplicate native archive metadata."
        );
        check_limit(&mut total, entry.size(), index + 1)?;
        ensure!(
            !entry.encrypted(),
            "Encrypted update archives are not supported."
        );
        let metadata = raw.starts_with("__MACOSX/");
        let name = raw.strip_prefix("__MACOSX/").unwrap_or(&raw);
        if metadata && (name.is_empty() || name == "BroTypist.app/") {
            ensure!(
                entry.is_dir() && entry.size() == 0,
                "Invalid archive metadata directory."
            );
            continue;
        }
        let path = archive_path(name, root)?;
        let mode = entry.unix_mode().unwrap_or(0);
        let kind = mode & libc_mode_type_mask();
        ensure!(
            [0, 0o100000, 0o040000, 0o120000].contains(&kind) && mode & 0o6000 == 0,
            "Unsupported update archive entry."
        );
        let directory = entry.is_dir();
        let size = entry.size();
        let link = if kind == 0o120000 {
            ensure!(!metadata && size <= 4096, "Invalid archive link.");
            let mut bytes = Vec::new();
            (&mut entry).take(4097).read_to_end(&mut bytes)?;
            ensure!(bytes.len() as u64 == size, "Invalid archive link size.");
            let link = String::from_utf8(bytes)?;
            link_target(&path, &link, root)?;
            Some(link)
        } else {
            let mut count = 0;
            let mut buffer = [0u8; 64 * 1024];
            loop {
                let n = entry.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                count += n as u64;
                ensure!(count <= size, "Invalid archive entry size.");
            }
            ensure!(count == size, "Invalid archive entry size.");
            None
        };
        if !metadata {
            members.push(Member {
                path,
                link,
                directory,
                size,
                mode,
            });
        }
    }
    validate_members(&members)?;
    Ok(members)
}
const fn libc_mode_type_mask() -> u32 {
    0o170000
}
fn preflight_tar_stream(archive: &Path) -> Result<()> {
    let decoder = flate2::read::GzDecoder::new(open_regular(archive, false)?);
    let mut tar = tar::Archive::new(decoder);
    let mut total = 0;
    for (index, row) in tar.entries()?.raw(true).enumerate() {
        let mut entry = row?;
        let kind = entry.header().entry_type().as_byte();
        ensure!(
            [0, b'0', b'5', b'2', b'L', b'K', b'x', b'g'].contains(&kind),
            "Unsupported tar entry, sparse payload or hardlink."
        );
        let size = entry.header().size()?;
        check_limit(&mut total, size, index + 1)?;
        if [b'L', b'K', b'x', b'g'].contains(&kind) {
            ensure!(
                size <= 64 * 1024,
                "Archive extension metadata exceeds its limit."
            );
            let mut metadata = Vec::new();
            (&mut entry)
                .take(64 * 1024 + 1)
                .read_to_end(&mut metadata)?;
            ensure!(
                metadata.len() as u64 == size,
                "Truncated archive extension metadata."
            );
            if [b'x', b'g'].contains(&kind) {
                ensure!(
                    !metadata
                        .windows(b"GNU.sparse".len())
                        .any(|w| w == b"GNU.sparse")
                        && !metadata
                            .windows(b"SCHILY.realsize".len())
                            .any(|w| w == b"SCHILY.realsize"),
                    "Sparse archive payloads are not supported."
                );
            }
        } else {
            ensure!(
                ![b'5', b'2'].contains(&kind) || size == 0,
                "Directory or link archive entry has unexpected payload."
            );
            let count = std::io::copy(&mut (&mut entry).take(size + 1), &mut std::io::sink())?;
            ensure!(count == size, "Truncated archive entry.");
        }
    }
    // tar iteration stops at its first end marker; drain the gzip trailer to
    // verify CRC and refuse a hidden second archive or unbounded trailing data.
    let mut decoder = tar.into_inner();
    let mut tail = Vec::new();
    Read::by_ref(&mut decoder)
        .take(64 * 1024 + 1)
        .read_to_end(&mut tail)?;
    ensure!(
        tail.len() <= 64 * 1024 && tail.iter().all(|b| *b == 0),
        "Unexpected data after the archive end marker."
    );
    Ok(())
}
fn tar_members(archive: &Path, root: &str) -> Result<Vec<Member>> {
    preflight_tar_stream(archive)?;
    let decoder = flate2::read::GzDecoder::new(open_regular(archive, false)?);
    let mut tar = tar::Archive::new(decoder);
    let mut members = Vec::new();
    let mut total = 0;
    for row in tar.entries()? {
        let mut entry = row?;
        let raw = entry.path_bytes();
        let path = archive_path(std::str::from_utf8(&raw)?, root)?;
        let header = entry.header();
        let kind = header.entry_type();
        let mode = header.mode()?;
        let size = header.size()?;
        ensure!(
            (kind.is_file() || kind.is_dir() || kind.is_symlink()) && mode & 0o6000 == 0,
            "Unsupported update archive entry (including hardlinks and sparse files)."
        );
        check_limit(&mut total, size, members.len() + 1)?;
        let link = if kind.is_symlink() {
            let target = entry
                .link_name()?
                .context("Missing link target.")?
                .to_str()
                .context("Invalid link encoding.")?
                .to_owned();
            ensure!(target.len() <= 4096, "Archive link is too long.");
            link_target(&path, &target, root)?;
            Some(target)
        } else {
            None
        };
        // Reading preflight verifies the gzip CRC and declared byte counts before
        // extraction can create even the first payload file.
        let count = std::io::copy(&mut (&mut entry).take(size + 1), &mut std::io::sink())?;
        ensure!(count == size, "Invalid archive entry size.");
        members.push(Member {
            path,
            link,
            directory: kind.is_dir(),
            size,
            mode,
        });
    }
    validate_members(&members)?;
    Ok(members)
}
fn extract_package(archive: &Path, stage: &Path, platform: &str) -> Result<PathBuf> {
    require_private_directory(stage)?;
    let root = if platform == "darwin" {
        "BroTypist.app"
    } else {
        "BroTypist"
    };
    if platform == "darwin" {
        zip_members(archive, root)?;
        #[cfg(target_os = "macos")]
        {
            run_output(
                Command::new("/usr/bin/ditto")
                    .args(["-x", "-k"])
                    .arg(archive)
                    .arg(stage),
                Duration::from_secs(180),
            )?;
        }
        #[cfg(not(target_os = "macos"))]
        bail!("Native macOS extraction requires macOS.");
    } else {
        let members = tar_members(archive, root)?;
        let decoder = flate2::read::GzDecoder::new(open_regular(archive, false)?);
        let mut tar = tar::Archive::new(decoder);
        for (index, row) in tar.entries()?.enumerate() {
            let mut entry = row?;
            let m = members.get(index).context("Archive inventory changed.")?;
            ensure!(
                archive_path(std::str::from_utf8(&entry.path_bytes())?, root)? == m.path
                    && entry.header().size()? == m.size
                    && entry.header().mode()? == m.mode,
                "Archive inventory changed."
            );
            if m.link.is_some() {
                continue;
            }
            let path = stage.join(&m.path);
            let parent = path.parent().context("Invalid archive member parent.")?;
            create_payload_directories(parent)?;
            if m.directory {
                create_payload_directories(&path)?;
            } else {
                let mut options = OpenOptions::new();
                options.create_new(true).write(true);
                #[cfg(unix)]
                options
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
                let mut output = options.open(&path)?;
                let n = std::io::copy(&mut (&mut entry).take(m.size + 1), &mut output)?;
                ensure!(n == m.size, "Archive payload changed.");
                output.sync_all()?;
            }
            #[cfg(unix)]
            fs::set_permissions(
                path,
                fs::Permissions::from_mode(
                    (m.mode & 0o777 & !0o022) | if m.directory { 0o700 } else { 0o600 },
                ),
            )?;
        }
        // Creating links only after all files prevents links from becoming an
        // extraction destination, even if the archive's order is adversarial.
        for m in members.iter().filter(|m| m.link.is_some()) {
            let path = stage.join(&m.path);
            create_payload_directories(path.parent().unwrap())?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(m.link.as_ref().unwrap(), path)?;
            #[cfg(not(unix))]
            bail!("Native OTA is held for this platform.");
        }
    }
    let bundle = stage.join(root);
    let info = fs::symlink_metadata(&bundle)?;
    ensure!(
        info.is_dir() && !info.file_type().is_symlink(),
        "Update archive has no native application."
    );
    let mut count = 0;
    let mut total = 0;
    inspect_payload(stage, &bundle, root, &mut count, &mut total)?;
    Ok(bundle)
}
fn create_payload_directories(path: &Path) -> Result<()> {
    no_symlink_components(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
    }
    #[cfg(not(unix))]
    bail!("Native OTA is held for this platform.");
    Ok(())
}
fn inspect_payload(
    stage: &Path,
    path: &Path,
    root: &str,
    count: &mut usize,
    total: &mut u64,
) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    archive_path(
        path.strip_prefix(stage)?
            .to_str()
            .context("Invalid extracted path.")?,
        root,
    )?;
    #[cfg(unix)]
    if !metadata.file_type().is_symlink() {
        ensure!(
            metadata.uid() == unsafe { libc::geteuid() } && metadata.mode() & 0o6022 == 0,
            "Extracted application permissions are unsafe."
        );
    }
    if metadata.file_type().is_symlink() {
        let link = fs::read_link(path)?;
        link_target(
            path.strip_prefix(stage)?,
            link.to_str().context("Invalid extracted link.")?,
            root,
        )?;
        ensure!(
            path.canonicalize()?
                .starts_with(stage.join(root).canonicalize()?),
            "Extracted link escapes the application."
        );
    } else if metadata.is_dir() {
        for item in fs::read_dir(path)? {
            *count += 1;
            ensure!(*count <= MAX_ARCHIVE_ENTRIES, "Too many extracted entries.");
            inspect_payload(stage, &item?.path(), root, count, total)?;
        }
    } else if metadata.is_file() {
        #[cfg(unix)]
        ensure!(
            metadata.nlink() == 1,
            "Extracted hardlinks are not permitted."
        );
        check_limit(total, metadata.len(), *count)?;
    } else {
        bail!("Invalid extracted application payload.")
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct SigningIdentity {
    identifier: String,
    team: String,
    authorities: Vec<String>,
}
fn bundle_plist(bundle: &Path) -> Result<plist::Dictionary> {
    let path = bundle.join("Contents/Info.plist");
    no_symlink_components(&path)?;
    let bytes = read_file(&path, 64 * 1024, false)?;
    plist::Value::from_reader(std::io::Cursor::new(bytes))?
        .into_dictionary()
        .context("Invalid native bundle metadata.")
}
fn signing_identity(bundle: &Path) -> Result<SigningIdentity> {
    ensure!(
        native() == "darwin",
        "Code-signing identity requires macOS."
    );
    run_output(
        Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(bundle),
        Duration::from_secs(60),
    )?;
    let bytes = run_output(
        Command::new("/usr/bin/codesign")
            .args(["--display", "--verbose=4"])
            .arg(bundle),
        Duration::from_secs(30),
    )?;
    let text = std::str::from_utf8(&bytes)?;
    let fields = |prefix: &str| {
        text.lines()
            .filter_map(|l| l.strip_prefix(prefix))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let identifiers = fields("Identifier=");
    let teams = fields("TeamIdentifier=");
    let authorities = fields("Authority=");
    ensure!(
        identifiers.len() == 1
            && identifiers[0] == APP_ID
            && teams.len() == 1
            && !teams[0].is_empty()
            && teams[0] != "not set"
            && !authorities.is_empty(),
        "Updates require the existing identity-signed application."
    );
    let info = bundle_plist(bundle)?;
    ensure!(
        info.get("CFBundleIdentifier")
            .and_then(plist::Value::as_string)
            == Some(APP_ID)
            && info
                .get("CFBundleExecutable")
                .and_then(plist::Value::as_string)
                == Some("BroTypist"),
        "Unexpected native bundle identity."
    );
    Ok(SigningIdentity {
        identifier: identifiers[0].clone(),
        team: teams[0].clone(),
        authorities,
    })
}
fn health_version(bundle: &Path, version: &str, platform: &str) -> Result<()> {
    if platform == "darwin" {
        let info = bundle_plist(bundle)?;
        ensure!(
            info.get("CFBundleShortVersionString")
                .and_then(plist::Value::as_string)
                == Some(version)
                && info
                    .get("CFBundleVersion")
                    .and_then(plist::Value::as_string)
                    == Some(version),
            "Sealed bundle version does not match the signed release."
        );
    } else {
        let bytes = run_output(
            clean_command(executable(bundle, platform).as_os_str())
                .arg("--version")
                .current_dir(bundle.parent().unwrap()),
            Duration::from_secs(30),
        )?;
        ensure!(
            std::str::from_utf8(&bytes)?.trim() == format!("BroTypist {version}"),
            "Updated executable did not report the signed version."
        );
    }
    Ok(())
}
fn atomic_exchange(left: &Path, right: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        no_symlink_components(left)?;
        no_symlink_components(right)?;
        ensure!(
            fs::metadata(left)?.dev() == fs::metadata(right)?.dev(),
            "Atomic exchange requires one filesystem."
        );
        let left = CString::new(left.as_os_str().as_bytes())?;
        let right = CString::new(right.as_os_str().as_bytes())?;
        #[cfg(target_os = "macos")]
        let result = unsafe {
            libc::renameatx_np(
                libc::AT_FDCWD,
                left.as_ptr(),
                libc::AT_FDCWD,
                right.as_ptr(),
                2,
            )
        };
        #[cfg(target_os = "linux")]
        let result = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                libc::AT_FDCWD,
                left.as_ptr(),
                libc::AT_FDCWD,
                right.as_ptr(),
                2,
            ) as i32
        };
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let result = -1;
        ensure!(
            result == 0,
            "Atomic application exchange failed; no non-atomic fallback was used."
        );
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (left, right);
        bail!("Native OTA is held for this platform.")
    }
}
fn receipt(
    root: &Path,
    r: &Request,
    status: &str,
    rollback: Option<&Path>,
    message: Option<&str>,
) -> Result<()> {
    let mut value = json!({"schema":1,"application":APP_ID,"version":r.version,"status":status,"previous_version":r.identity.version,"notarization":"not claimed"});
    if let Some(path) = rollback {
        value["rollback"] = json!(path);
    }
    if let Some(message) = message {
        value["message"] = message.chars().take(512).collect::<String>().into();
    }
    updates::write_private(&root.join("transaction.json"), &serde_json::to_vec(&value)?)
}
fn transaction_lock(root: &Path) -> Result<File> {
    let file = open_regular(&root.join("startup.lock"), true)?;
    file.lock_exclusive()?;
    Ok(file)
}
fn transaction_status(root: &Path, r: &Request) -> Result<String> {
    let v: TransactionReceipt = serde_json::from_value(updates::strict_json(&read_file(
        &root.join("transaction.json"),
        4096,
        true,
    )?)?)?;
    ensure!(
        v.schema == 1
            && v.application == APP_ID
            && v.version == r.version
            && v.previous_version == r.identity.version
            && v.notarization == "not claimed",
        "Invalid transaction receipt identity."
    );
    if let Some(path) = &v.rollback {
        ensure!(
            path.is_absolute()
                && path.starts_with(root)
                && !path.components().any(|c| matches!(c, Component::ParentDir)),
            "Invalid rollback receipt path."
        );
    }
    ensure!(
        v.message.as_ref().is_none_or(|m| m.chars().count() <= 512),
        "Invalid transaction receipt message."
    );
    Ok(v.status)
}

#[cfg(target_os = "linux")]
fn process_identity(pid: u32) -> Result<Option<ProcessIdentity>> {
    let root = PathBuf::from(format!("/proc/{pid}"));
    let info = match fs::metadata(&root) {
        Ok(info) => info,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let data = match fs::read_to_string(root.join("stat")) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let fields: Vec<_> = data
        .rsplit_once(')')
        .context("Invalid process identity.")?
        .1
        .split_whitespace()
        .collect();
    ensure!(fields.len() > 19, "Incomplete process identity.");
    if fields[0] == "Z" || fields[0] == "X" {
        return Ok(None);
    }
    Ok(Some(ProcessIdentity {
        pid,
        start: fields[19].into(),
        uid: info.uid(),
    }))
}
#[cfg(target_os = "macos")]
fn process_identity(pid: u32) -> Result<Option<ProcessIdentity>> {
    #[repr(C)]
    struct BsdInfo {
        flags: u32,
        status: u32,
        xstatus: u32,
        pid: u32,
        ppid: u32,
        uid: u32,
        gid: u32,
        ruid: u32,
        rgid: u32,
        svuid: u32,
        svgid: u32,
        reserved: u32,
        comm: [u8; 16],
        name: [u8; 32],
        nfiles: u32,
        pgid: u32,
        pjobc: u32,
        e_tdev: u32,
        e_tpgid: u32,
        nice: i32,
        start_sec: u64,
        start_usec: u64,
    }
    #[link(name = "proc")]
    unsafe extern "C" {
        fn proc_pidinfo(
            pid: i32,
            flavor: i32,
            arg: u64,
            buffer: *mut std::ffi::c_void,
            buffersize: i32,
        ) -> i32;
    }
    let mut info: BsdInfo = unsafe { std::mem::zeroed() };
    let n = unsafe {
        proc_pidinfo(
            pid as i32,
            3,
            0,
            (&mut info as *mut BsdInfo).cast(),
            std::mem::size_of::<BsdInfo>() as i32,
        )
    };
    if n == 0 {
        let e = std::io::Error::last_os_error();
        if [Some(libc::ESRCH), Some(libc::ENOENT)].contains(&e.raw_os_error()) {
            return Ok(None);
        }
        return Err(e.into());
    }
    ensure!(
        n as usize == std::mem::size_of::<BsdInfo>() && info.pid == pid,
        "Could not establish native process generation."
    );
    if info.status == 5 {
        return Ok(None);
    }
    Ok(Some(ProcessIdentity {
        pid,
        start: format!("{}:{}", info.start_sec, info.start_usec),
        uid: info.uid,
    }))
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn process_identity(_pid: u32) -> Result<Option<ProcessIdentity>> {
    bail!("Native updater process identity is unsupported on this held platform.")
}
fn same_process(identity: &ProcessIdentity) -> Result<bool> {
    Ok(process_identity(identity.pid)?.as_ref() == Some(identity))
}
#[cfg(unix)]
fn wait_for_exit(
    fd: RawFd,
    r: &Request,
    parent: &ProcessIdentity,
    timeout: Duration,
) -> Result<()> {
    let mut metadata: libc::stat = unsafe { std::mem::zeroed() };
    ensure!(
        unsafe { libc::fstat(fd, &mut metadata) } == 0
            && metadata.st_mode & libc::S_IFMT == libc::S_IFIFO
            && unsafe { libc::fcntl(fd, libc::F_GETFL) } & libc::O_ACCMODE == libc::O_RDONLY,
        "Update approval requires a private read pipe."
    );
    let deadline = Instant::now() + timeout;
    let mut received = Vec::new();
    loop {
        ensure!(
            Instant::now() < deadline,
            "Update approval timed out; the original app was not changed."
        );
        let mut poll = libc::pollfd {
            fd,
            events: libc::POLLIN | libc::POLLHUP,
            revents: 0,
        };
        let n = unsafe {
            libc::poll(
                &mut poll,
                1,
                (deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis()
                    .min(i32::MAX as u128)) as i32,
            )
        };
        if n < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        ensure!(n > 0, "Update approval timed out.");
        let mut bytes = [0u8; 2];
        let n = unsafe { libc::read(fd, bytes.as_mut_ptr().cast(), 2) };
        if n < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        ensure!(n >= 0, "Could not read update approval.");
        if n == 0 {
            break;
        }
        received.extend_from_slice(&bytes[..n as usize]);
        ensure!(received == b"1", "Update was not approved.");
    }
    ensure!(received == b"1", "Update was cancelled.");
    ensure!(
        parent.pid == r.parent_pid && parent.uid == r.uid && !parent.start.is_empty(),
        "Original parent process identity does not match the request."
    );
    while same_process(parent)? {
        ensure!(
            Instant::now() < deadline,
            "The previous application did not exit."
        );
        thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

struct StartupContext {
    root: PathBuf,
    request: Request,
    process: ProcessIdentity,
}
static STARTUP_CONTEXT: LazyLock<Mutex<Option<Arc<StartupContext>>>> =
    LazyLock::new(|| Mutex::new(None));
pub struct StartupGuard {
    context: Arc<StartupContext>,
}
impl StartupGuard {
    /// The guard is process-lifetime ownership, not a reversible install approval.
    /// Dropping it does not disable the cancellation watcher or clear its lease.
    pub fn transaction_root(&self) -> &Path {
        &self.context.root
    }
}
type ManifestVerifier = fn(&[u8], &[u8]) -> Result<UpdateRelease>;
fn enroll_startup(
    root: &Path,
    nonce: &str,
    actual_exe: &Path,
    verify: ManifestVerifier,
) -> Result<Arc<StartupContext>> {
    let r = request(&root.join("request.json"))?;
    ensure!(
        nonce_matches(nonce, &r.nonce),
        "Update startup nonce does not match."
    );
    ensure!(
        actual_exe == executable(&r.destination, &r.platform),
        "Startup executable does not belong to this update."
    );
    let identity = marker(&r.destination, &r.platform)?;
    let release = verify(
        &read_file(&root.join("manifest.json"), MAX_MANIFEST_BYTES, true)?,
        &read_file(&root.join("manifest.sig"), MAX_SIGNATURE_BYTES, true)?,
    )?;
    ensure!(
        identity.version == r.version
            && identity.version == release.version
            && identity.source_commit == release.source_commit,
        "Startup build does not match the signed update."
    );
    // Authenticate before creating the one private lock absent from published
    // clients. Recheck the live build under that lock before process enrollment.
    ensure_startup_lock(root, &r)?;
    let _lock = transaction_lock(root)?;
    ensure!(
        transaction_status(root, &r)? == "promoted",
        "This update startup has been cancelled or is no longer current."
    );
    ensure!(
        marker(&r.destination, &r.platform)? == identity,
        "Startup build changed during enrollment."
    );
    let process = process_identity(std::process::id())?
        .context("Startup process identity is unavailable.")?;
    ensure!(process.uid == r.uid, "Startup process owner changed.");
    let lease = StartupLease {
        nonce: r.nonce.clone(),
        version: r.version.clone(),
        process: process.clone(),
    };
    write_exclusive(
        &root.join(format!("process-{}.json", process.pid)),
        &serde_json::to_vec(&lease)?,
    )?;
    Ok(Arc::new(StartupContext {
        root: root.into(),
        request: r,
        process,
    }))
}
/// FIRST application entrypoint action, before any model/worker/GUI can inherit
/// tokens. Invalid/delayed/cancelled startup is an error: caller must exit.
/// Normal launches return None and do not read a profile or create files.
pub fn startup_guard() -> Result<Option<StartupGuard>> {
    let path = std::env::var_os(HEALTH_PATH);
    let nonce = std::env::var_os(HEALTH_NONCE);
    // Called before other threads exist. Centralized here, never in UI callbacks.
    unsafe {
        std::env::remove_var(HEALTH_PATH);
        std::env::remove_var(HEALTH_NONCE);
    }
    if path.is_none() && nonce.is_none() {
        return Ok(None);
    }
    let path = PathBuf::from(path.context("Missing startup receipt token.")?);
    let nonce = nonce
        .context("Missing startup nonce.")?
        .into_string()
        .map_err(|_| anyhow!("Invalid startup nonce encoding."))?;
    ensure!(
        path.file_name().and_then(|s| s.to_str()) == Some("startup.json"),
        "Invalid startup receipt basename."
    );
    let context = enroll_startup(
        path.parent()
            .context("Missing startup transaction parent.")?,
        &nonce,
        &std::env::current_exe()?,
        updates::verify_manifest,
    )?;
    let mut slot = STARTUP_CONTEXT.lock().unwrap();
    ensure!(slot.is_none(), "Update startup gate was already entered.");
    *slot = Some(context.clone());
    drop(slot);
    start_cancellation_watcher(context.clone())?;
    Ok(Some(StartupGuard { context }))
}
fn start_cancellation_watcher(watcher: Arc<StartupContext>) -> Result<()> {
    thread::Builder::new()
        .name("brotypist-update-startup".into())
        .spawn(move || {
            loop {
                thread::sleep(Duration::from_millis(25));
                // Read failures cannot silently orphan a promoted replacement. A
                // transaction is retained until health or rollback is established.
                let status = transaction_status(&watcher.root, &watcher.request);
                match status.as_deref() {
                    Ok("healthy") => return,
                    Ok("promoted") => {}
                    _ => std::process::exit(78),
                }
            }
        })?;
    Ok(())
}
/// Native GUI calls only after its first actual main-loop iteration. Launcher
/// success and CLI initialization are not health. Publication is exclusive.
pub fn acknowledge_startup() -> Result<bool> {
    let Some(context) = STARTUP_CONTEXT.lock().unwrap().clone() else {
        return Ok(false);
    };
    Ok(acknowledge_context(&context, updates::verify_manifest).unwrap_or(false))
}
fn acknowledge_context(context: &StartupContext, verify: ManifestVerifier) -> Result<bool> {
    let _lock = transaction_lock(&context.root)?;
    ensure!(
        transaction_status(&context.root, &context.request)? == "promoted"
            && same_process(&context.process)?,
        "Startup transaction was cancelled."
    );
    let identity = marker(&context.request.destination, &context.request.platform)?;
    let release = verify(
        &read_file(
            &context.root.join("manifest.json"),
            MAX_MANIFEST_BYTES,
            true,
        )?,
        &read_file(
            &context.root.join("manifest.sig"),
            MAX_SIGNATURE_BYTES,
            true,
        )?,
    )?;
    ensure!(
        identity.version == context.request.version
            && release.version == identity.version
            && release.source_commit == identity.source_commit,
        "Startup identity changed."
    );
    let ack = StartupAck {
        nonce: context.request.nonce.clone(),
        version: context.request.version.clone(),
        pid: context.process.pid,
    };
    let temp = context.root.join(format!(".startup-{}", random_hex(16)));
    write_exclusive(&temp, &serde_json::to_vec(&ack)?)?;
    let published = fs::hard_link(&temp, context.root.join("startup.json"));
    let cleanup = fs::remove_file(&temp);
    cleanup?;
    match published {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e.into()),
    }
}
fn leases(root: &Path, r: &Request) -> Result<Vec<StartupLease>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if !name.starts_with("process-") || !name.ends_with(".json") {
            continue;
        }
        ensure!(out.len() < 128, "Too many startup process leases.");
        let lease: StartupLease =
            serde_json::from_value(updates::strict_json(&read_file(&path, 2048, true)?)?)?;
        ensure!(
            nonce_matches(&lease.nonce, &r.nonce)
                && lease.version == r.version
                && lease.process.uid == r.uid
                && name == format!("process-{}.json", lease.process.pid),
            "Invalid startup process lease."
        );
        out.push(lease);
    }
    Ok(out)
}
fn wait_startup(root: &Path, r: &Request, launcher: &mut Child, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        {
            let _lock = transaction_lock(root)?;
            ensure!(
                transaction_status(root, r)? == "promoted",
                "Startup transaction is no longer promoted."
            );
            if root.join("startup.json").exists() {
                let ack: StartupAck = serde_json::from_value(updates::strict_json(&read_file(
                    &root.join("startup.json"),
                    1024,
                    true,
                )?)?)?;
                ensure!(
                    nonce_matches(&ack.nonce, &r.nonce) && ack.version == r.version && ack.pid > 0,
                    "Invalid native startup acknowledgment."
                );
                let identity = leases(root, r)?
                    .into_iter()
                    .find(|lease| lease.process.pid == ack.pid)
                    .context("Acknowledgment lacks an owned startup process lease.")?
                    .process;
                ensure!(
                    same_process(&identity)?,
                    "Updated application exited during startup."
                );
                return Ok(());
            }
        }
        if let Some(status) = launcher.try_wait()? {
            ensure!(
                r.platform == "darwin" && status.success(),
                "The updated application could not start."
            );
        }
        ensure!(
            Instant::now() < deadline,
            "Native startup was not acknowledged; per-app OS approval may be required."
        );
        thread::sleep(Duration::from_millis(50));
    }
}
fn cancel_and_wait(root: &Path, r: &Request, rollback: &Path, timeout: Duration) -> Result<()> {
    let active = {
        let _lock = transaction_lock(root)?;
        // This invalidates delayed LaunchServices launches before any swap back.
        // Enrollment uses the same lock and requires promoted, so the resulting
        // owned process set is closed. No launcher-derived PID is ever signaled.
        receipt(root, r, "cancelled", Some(rollback), None)?;
        leases(root, r)?
    };
    let deadline = Instant::now() + timeout;
    loop {
        let mut live = false;
        for lease in &active {
            if same_process(&lease.process)? {
                live = true;
            }
        }
        if !live {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "Replacement process exit could not be established; both builds are retained and rollback was not attempted."
        );
        thread::sleep(Duration::from_millis(25));
    }
}
fn launch(bundle: &Path, platform: &str, transaction: Option<(&Path, &Request)>) -> Result<Child> {
    let mut command = if platform == "darwin" {
        let mut command = clean_command("/usr/bin/open");
        command.args(["-n", "-a"]).arg(bundle);
        command
    } else {
        clean_command(executable(bundle, platform).as_os_str())
    };
    if let Some((root, r)) = transaction {
        command
            .env(HEALTH_PATH, root.join("startup.json"))
            .env(HEALTH_NONCE, &r.nonce);
        if platform == "darwin" {
            for (key, value) in [
                (
                    HEALTH_PATH,
                    root.join("startup.json").to_string_lossy().into_owned(),
                ),
                (HEALTH_NONCE, r.nonce.clone()),
            ] {
                command.arg("--env").arg(format!("{key}={value}"));
            }
        }
    }
    if platform == "darwin" {
        if let Some(home) = std::env::var_os("INKLOW_HOME") {
            command
                .arg("--env")
                .arg(format!("INKLOW_HOME={}", home.to_string_lossy()));
        }
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .current_dir(bundle.parent().unwrap());
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(command.spawn()?)
}
#[cfg(unix)]
fn run_installer(path: &Path, approval: OwnedFd) -> Result<i32> {
    let r = request(path)?;
    let root = path.parent().unwrap();
    let stage = root.join("payload");
    let mut candidate = None;
    let mut exchanged = false;
    let parent = parent_ownership(root, &r)?
        .context("Rust-origin installation requires original-parent ownership metadata.")?;
    let outcome = (|| -> Result<()> {
        let destination = check_destination(&r)?;
        verified_asset(root, &r)?;
        let signing = if r.platform == "darwin" {
            Some(signing_identity(&destination)?)
        } else {
            None
        };
        wait_for_exit(approval.as_raw_fd(), &r, &parent, APPROVAL_TIMEOUT)?;
        check_destination(&r)?;
        let (release, _) = verified_asset(root, &r)?;
        fs::create_dir(&stage)?;
        fs::set_permissions(&stage, fs::Permissions::from_mode(0o700))?;
        let payload = extract_package(&root.join("package"), &stage, &r.platform)?;
        let identity = marker(&payload, &r.platform)?;
        let expected = BuildIdentity {
            version: release.version.clone(),
            source_commit: release.source_commit.clone(),
            ..r.identity.clone()
        };
        ensure!(
            identity == expected,
            "Extracted build does not match the signed release."
        );
        binary_architecture(&executable(&payload, &r.platform), &r.platform)?;
        if r.platform == "darwin" {
            ensure!(
                Some(signing_identity(&payload)?) == signing,
                "Application signing identity changed."
            );
            preserve_quarantine(&root.join("package"), &payload)?;
            preserve_quarantine(&destination, &payload)?;
        }
        health_version(&payload, &r.version, &r.platform)?;
        check_destination(&r)?;
        if r.platform == "darwin" {
            ensure!(
                Some(signing_identity(&destination)?) == signing,
                "Installed application signing identity changed."
            );
        }
        atomic_exchange(&payload, &destination)?;
        candidate = Some(payload.clone());
        exchanged = true;
        {
            let _lock = transaction_lock(root)?;
            receipt(root, &r, "promoted", Some(&payload), None)?;
        }
        health_version(&destination, &r.version, &r.platform)?;
        let mut process = launch(&destination, &r.platform, Some((root, &r)))?;
        wait_startup(root, &r, &mut process, STARTUP_TIMEOUT)?;
        receipt(root, &r, "healthy", Some(&payload), None)?;
        Ok(())
    })();
    let result = match outcome {
        Ok(()) => Ok(0),
        Err(_) if exchanged => {
            let previous = candidate.as_ref().unwrap();
            match cancel_and_wait(root, &r, previous, STOP_TIMEOUT) {
                Err(e) => {
                    receipt(
                        root,
                        &r,
                        "rollback_pending",
                        Some(previous),
                        Some(
                            "Replacement exit is unconfirmed. Both complete builds were retained; automatic rollback was not attempted.",
                        ),
                    )?;
                    return Err(e);
                }
                Ok(()) => {}
            }
            if atomic_exchange(previous, &r.destination).is_err() {
                receipt(root, &r, "rollback_failed", Some(previous), None)?;
                bail!(
                    "Automatic rollback failed; the previous build is retained beside the application."
                );
            }
            receipt(
                root,
                &r,
                "rolled_back",
                Some(previous),
                Some(
                    "Native startup or validation failed. The previous application was restored; no user data was changed.",
                ),
            )?;
            launch(&r.destination, &r.platform, None)?;
            Ok(1)
        }
        Err(_) => {
            receipt(
                root,
                &r,
                "not_installed",
                None,
                Some("Update validation failed; the installed application was not changed."),
            )?;
            if stage.exists() {
                fs::remove_dir_all(&stage)?;
            }
            Ok(1)
        }
    };
    // Retain bounded signed provenance with completed promoted transactions.
    // The package itself is unnecessary once startup/rollback has settled.
    let package = root.join("package");
    if package.exists() {
        fs::remove_file(package)?;
    }
    if !exchanged {
        for name in ["manifest.json", "manifest.sig"] {
            let p = root.join(name);
            if p.exists() {
                fs::remove_file(p)?;
            }
        }
    }
    drop(approval);
    result
}
pub fn helper_main(args: &[String]) -> Result<i32> {
    #[cfg(not(unix))]
    {
        let _ = args;
        bail!("Native OTA is held for this platform.");
    }
    #[cfg(unix)]
    {
        let mut path = None;
        let mut fd = None;
        let mut iter = args.iter();
        while let Some(flag) = iter.next() {
            match flag.as_str() {
                "--request" if path.is_none() => {
                    path = Some(PathBuf::from(iter.next().context("Missing request path.")?))
                }
                "--approval-fd" if fd.is_none() => {
                    fd = Some(
                        iter.next()
                            .context("Missing approval descriptor.")?
                            .parse::<i32>()?,
                    )
                }
                _ => bail!("Unsupported offline updater argument."),
            }
        }
        let path = path.context("Missing update request.")?;
        let fd = fd.context("Missing approval descriptor.")?;
        ensure!(
            fd >= 3 && unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0,
            "Invalid approval descriptor."
        );
        let approval = unsafe { OwnedFd::from_raw_fd(fd) };
        Ok(run_installer(&path, approval).unwrap_or(1))
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn preserve_quarantine(source: &Path, destination: &Path) -> Result<()> {
    fn get(path: &Path) -> Result<Option<Vec<u8>>> {
        let p = CString::new(path.as_os_str().as_bytes())?;
        let name = c"com.apple.quarantine";
        let size = unsafe {
            libc::getxattr(
                p.as_ptr(),
                name.as_ptr(),
                std::ptr::null_mut(),
                0,
                0,
                libc::XATTR_NOFOLLOW,
            )
        };
        if size < 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::ENOATTR) {
                return Ok(None);
            }
            return Err(e.into());
        }
        ensure!(size <= 64 * 1024, "Quarantine metadata exceeds its limit.");
        let mut bytes = vec![0; size as usize];
        let n = unsafe {
            libc::getxattr(
                p.as_ptr(),
                name.as_ptr(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
                0,
                libc::XATTR_NOFOLLOW,
            )
        };
        ensure!(n == size, "Quarantine metadata changed during copy.");
        Ok(Some(bytes))
    }
    let Some(value) = get(source)? else {
        return Ok(());
    };
    if get(destination)?.is_some() {
        return Ok(());
    }
    let p = CString::new(destination.as_os_str().as_bytes())?;
    if unsafe {
        libc::setxattr(
            p.as_ptr(),
            c"com.apple.quarantine".as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
            libc::XATTR_NOFOLLOW | libc::XATTR_CREATE,
        )
    } != 0
    {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::EEXIST) {
            return Err(e.into());
        }
    }
    Ok(())
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn preserve_quarantine(_source: &Path, _destination: &Path) -> Result<()> {
    Ok(())
}

#[cfg(target_os = "macos")]
pub(crate) fn mark_web_download(path: &Path, source: &str, origin: &str) -> Result<()> {
    // NSURL creates and maintains native quarantine resources; never invent raw
    // quarantine flags, strip attributes, or replace existing agent/event data.
    use std::ffi::{c_char, c_void};
    type Id = *mut c_void;
    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> Id;
        fn sel_registerName(name: *const c_char) -> Id;
        fn objc_msgSend();
    }
    #[link(name = "Foundation", kind = "framework")]
    unsafe extern "C" {
        static NSURLQuarantinePropertiesKey: Id;
    }
    #[link(name = "CoreServices", kind = "framework")]
    unsafe extern "C" {
        static kLSQuarantineTypeKey: Id;
        static kLSQuarantineTypeWebDownload: Id;
        static kLSQuarantineDataURLKey: Id;
        static kLSQuarantineOriginURLKey: Id;
    }
    unsafe fn send0(receiver: Id, selector: &std::ffi::CStr) -> Id {
        let f: unsafe extern "C" fn(Id, Id) -> Id =
            unsafe { std::mem::transmute(objc_msgSend as *const ()) };
        unsafe { f(receiver, sel_registerName(selector.as_ptr())) }
    }
    unsafe fn send1(receiver: Id, selector: &std::ffi::CStr, arg: Id) -> Id {
        let f: unsafe extern "C" fn(Id, Id, Id) -> Id =
            unsafe { std::mem::transmute(objc_msgSend as *const ()) };
        unsafe { f(receiver, sel_registerName(selector.as_ptr()), arg) }
    }
    unsafe fn nsstring(value: &str) -> Result<Id> {
        let s = CString::new(value)?;
        let cls = unsafe { objc_getClass(c"NSString".as_ptr()) };
        let f: unsafe extern "C" fn(Id, Id, *const c_char) -> Id =
            unsafe { std::mem::transmute(objc_msgSend as *const ()) };
        let out = unsafe {
            f(
                cls,
                sel_registerName(c"stringWithUTF8String:".as_ptr()),
                s.as_ptr(),
            )
        };
        ensure!(
            !out.is_null(),
            "Could not encode native download provenance."
        );
        Ok(out)
    }
    unsafe {
        let pool = send0(
            send0(objc_getClass(c"NSAutoreleasePool".as_ptr()), c"alloc"),
            c"init",
        );
        let result = (|| -> Result<()> {
            let file = send1(
                objc_getClass(c"NSURL".as_ptr()),
                c"fileURLWithPath:",
                nsstring(path.to_str().context("Invalid package path encoding.")?)?,
            );
            let mut existing: Id = std::ptr::null_mut();
            let mut error: Id = std::ptr::null_mut();
            let get: unsafe extern "C" fn(Id, Id, *mut Id, Id, *mut Id) -> bool =
                std::mem::transmute(objc_msgSend as *const ());
            ensure!(
                !file.is_null()
                    && get(
                        file,
                        sel_registerName(c"getResourceValue:forKey:error:".as_ptr()),
                        &mut existing,
                        NSURLQuarantinePropertiesKey,
                        &mut error
                    ),
                "Could not read native download security properties."
            );
            let dictionary = if existing.is_null() {
                send0(objc_getClass(c"NSMutableDictionary".as_ptr()), c"new")
            } else {
                send0(existing, c"mutableCopy")
            };
            ensure!(
                !dictionary.is_null(),
                "Native download security properties are unavailable."
            );
            let set: unsafe extern "C" fn(Id, Id, Id, Id) =
                std::mem::transmute(objc_msgSend as *const ());
            set(
                dictionary,
                sel_registerName(c"setObject:forKey:".as_ptr()),
                kLSQuarantineTypeWebDownload,
                kLSQuarantineTypeKey,
            );
            for (key, value) in [
                (kLSQuarantineDataURLKey, source),
                (kLSQuarantineOriginURLKey, origin),
            ] {
                if send1(dictionary, c"objectForKey:", key).is_null() {
                    set(
                        dictionary,
                        sel_registerName(c"setObject:forKey:".as_ptr()),
                        nsstring(value)?,
                        key,
                    );
                }
            }
            let publish: unsafe extern "C" fn(Id, Id, Id, Id, *mut Id) -> bool =
                std::mem::transmute(objc_msgSend as *const ());
            let success = publish(
                file,
                sel_registerName(c"setResourceValue:forKey:error:".as_ptr()),
                dictionary,
                NSURLQuarantinePropertiesKey,
                &mut error,
            );
            send0(dictionary, c"release");
            ensure!(
                success,
                "Downloaded package could not retain native security metadata."
            );
            Ok(())
        })();
        send0(pool, c"drain");
        result
    }
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn mark_web_download(_path: &Path, _source: &str, _origin: &str) -> Result<()> {
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD};
    use ed25519_dalek::{Signer, SigningKey};

    struct Fixture {
        _temp: tempfile::TempDir,
        root: PathBuf,
        destination: PathBuf,
        request: Request,
    }
    fn private_dir(path: &Path) {
        fs::create_dir(path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fn make_bundle(path: &Path, version: &str, commit: &str) -> BuildIdentity {
        fs::create_dir_all(path).unwrap();
        let identity = BuildIdentity {
            application: APP_ID.into(),
            platform: native().into(),
            version: version.into(),
            source_commit: commit.into(),
            architecture: architecture().into(),
        };
        let marker_path = path.join(if native() == "darwin" {
            "Contents/Resources/.inklow-build.json"
        } else {
            MARKER
        });
        fs::create_dir_all(marker_path.parent().unwrap()).unwrap();
        write_exclusive(&marker_path, &serde_json::to_vec(&identity).unwrap()).unwrap();
        let exe = executable(path, native());
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        let mut header = [0u8; 64];
        if native() == "darwin" {
            header[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
            header[4..8].copy_from_slice(
                &(if architecture() == "arm64" {
                    0x100000cu32
                } else {
                    0x1000007
                })
                .to_le_bytes(),
            );
        } else {
            header[..6].copy_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1]);
            header[18..20].copy_from_slice(
                &(if architecture() == "arm64" {
                    183u16
                } else {
                    62
                })
                .to_le_bytes(),
            );
        }
        write_exclusive(&exe, &header).unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o700)).unwrap();
        write_exclusive(&path.join("payload-version"), version.as_bytes()).unwrap();
        identity
    }
    fn sign_fixture(root: &Path, version: &str, commit: &str, bytes: &[u8]) {
        let key = SigningKey::from_bytes(&[31; 32]);
        let (target, format, suffix) = if native() == "darwin" {
            ("macos-arm64", "zip", "macos-arm64.zip")
        } else {
            ("linux-x86_64", "tar.gz", "linux-amd64.tar.gz")
        };
        let data = serde_json::to_vec(&json!({"schema":1,"application":APP_ID,"version":version,"source_commit":commit,"assets":[{"name":format!("BroTypist-{version}-{suffix}"),"target":target,"format":format,"size":bytes.len(),"sha256":format!("{:x}",Sha256::digest(bytes))}]})).unwrap();
        write_exclusive(&root.join("manifest.json"), &data).unwrap();
        write_exclusive(
            &root.join("manifest.sig"),
            STANDARD.encode(key.sign(&data).to_bytes()).as_bytes(),
        )
        .unwrap();
    }
    fn fixture_verify(data: &[u8], signature: &[u8]) -> Result<UpdateRelease> {
        updates::verify_test_manifest(
            data,
            signature,
            &SigningKey::from_bytes(&[31; 32]).verifying_key().to_bytes(),
        )
    }
    fn fixture() -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let destination = base.join(if native() == "darwin" {
            "BroTypist.app"
        } else {
            "BroTypist"
        });
        let identity = make_bundle(&destination, "1.2.3", &"1".repeat(40));
        let metadata = fs::metadata(&destination).unwrap();
        let root = base.join(".brotypist-update-fixture");
        private_dir(&root);
        let current = process_identity(std::process::id()).unwrap().unwrap();
        let r = Request {
            schema: 1,
            destination: destination.clone(),
            platform: native().into(),
            identity,
            device: metadata.dev(),
            inode: metadata.ino(),
            uid: unsafe { libc::geteuid() },
            parent_pid: current.pid,
            isolated_home: None,
            nonce: "a".repeat(64),
            version: "1.2.4".into(),
        };
        write_exclusive(&root.join("request.json"), &serde_json::to_vec(&r).unwrap()).unwrap();
        write_exclusive(&root.join("startup.lock"), b"").unwrap();
        write_exclusive(
            &root.join("parent.json"),
            &serde_json::to_vec(&ParentOwnership {
                schema: 1,
                nonce: r.nonce.clone(),
                process: current,
            })
            .unwrap(),
        )
        .unwrap();
        sign_fixture(&root, "1.2.4", &"2".repeat(40), b"archive");
        Fixture {
            _temp: temp,
            root,
            destination,
            request: r,
        }
    }
    fn promoted(f: &Fixture) {
        let marker = f.destination.join(if native() == "darwin" {
            "Contents/Resources/.inklow-build.json"
        } else {
            MARKER
        });
        let identity = BuildIdentity {
            version: f.request.version.clone(),
            source_commit: "2".repeat(40),
            ..f.request.identity.clone()
        };
        updates::write_private(&marker, &serde_json::to_vec(&identity).unwrap()).unwrap();
        receipt(&f.root, &f.request, "promoted", None, None).unwrap();
    }
    #[test]
    fn development_receipt_presence_refuses_ota_without_changing_identity_or_receipt() {
        for bytes in [
            b"{\"kind\":\"local-development-not-release\",\"release_eligible\":false}".as_slice(),
            b"malformed development receipt".as_slice(),
        ] {
            let f = fixture();
            assert!(
                marker(&f.destination, native()).is_ok(),
                "Published builds without development receipts remain compatible."
            );
            let marker_path = f.destination.join(if native() == "darwin" {
                "Contents/Resources/.inklow-build.json"
            } else {
                MARKER
            });
            let original_marker = fs::read(&marker_path).unwrap();
            let receipt = f.destination.join(if native() == "darwin" {
                "Contents/Resources/.inklow-development.json"
            } else {
                "share/.inklow-development.json"
            });
            fs::create_dir_all(receipt.parent().unwrap()).unwrap();
            write_exclusive(&receipt, bytes).unwrap();
            assert!(marker(&f.destination, native()).is_err());
            assert!(check_destination(&f.request).is_err());
            assert_eq!(fs::read(&receipt).unwrap(), bytes);
            assert_eq!(fs::read(&marker_path).unwrap(), original_marker);
        }
        let f = fixture();
        let receipt = f.destination.join(if native() == "darwin" {
            "Contents/Resources/.inklow-development.json"
        } else {
            "share/.inklow-development.json"
        });
        fs::create_dir_all(receipt.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(f.root.join("missing-receipt-target"), &receipt).unwrap();
        assert!(
            marker(&f.destination, native()).is_err(),
            "A dangling development receipt link is not absence."
        );
        assert!(
            fs::symlink_metadata(receipt)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
    fn pipe() -> (OwnedFd, OwnedFd) {
        let mut fds = [-1; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        for fd in fds {
            assert_eq!(
                unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) },
                0
            );
        }
        unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
    }
    fn tar_file(path: &Path, rows: &[(&str, tar::EntryType, &[u8], Option<&str>)]) {
        let gzip = flate2::write::GzEncoder::new(
            File::create(path).unwrap(),
            flate2::Compression::default(),
        );
        let mut builder = tar::Builder::new(gzip);
        for (name, kind, bytes, link) in rows {
            let mut h = tar::Header::new_gnu();
            h.set_mode(0o755);
            h.set_entry_type(*kind);
            h.set_size(bytes.len() as u64);
            // Set raw bytes so traversal fixtures test the reader rather than
            // being rejected by the archive writer's own path normalization.
            h.as_mut_bytes()[..100].fill(0);
            h.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
            if let Some(link) = link {
                h.set_link_name(link).unwrap();
            }
            h.set_cksum();
            builder.append(&h, *bytes).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();
    }
    #[test]
    fn private_regular_input_bounds_links_and_ownership() {
        let f = fixture();
        let input = f.root.join("input");
        write_exclusive(&input, b"1234").unwrap();
        assert_eq!(read_file(&input, 4, true).unwrap(), b"1234");
        assert!(read_file(&input, 3, true).is_err());
        let link = f.root.join("link");
        std::os::unix::fs::symlink(&input, &link).unwrap();
        assert!(read_file(&link, 8, false).is_err());
        let hard = f.root.join("hard");
        fs::hard_link(&input, &hard).unwrap();
        assert!(read_file(&input, 8, false).is_err());
        fs::remove_file(hard).unwrap();
        fs::set_permissions(&input, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_file(&input, 8, true).is_err());
        let linked_parent = f.root.join("linked-parent");
        std::os::unix::fs::symlink(&f.root, &linked_parent).unwrap();
        assert!(no_symlink_components(&linked_parent.join("input")).is_err());
        assert!(require_private_directory(&linked_parent).is_err());
    }
    #[test]
    fn copy_is_exclusive_bounded_and_keeps_previous_data() {
        let f = fixture();
        let input = f.root.join("source");
        let out = f.root.join("copy");
        write_exclusive(&input, b"archive").unwrap();
        copy_private(&input, &out, 7).unwrap();
        assert_eq!(read_file(&out, 7, true).unwrap(), b"archive");
        assert!(copy_private(&input, &out, 7).is_err());
        assert!(copy_private(&input, &f.root.join("short"), 6).is_err());
    }
    #[test]
    fn archive_paths_reject_traversal_private_data_and_absolute_links() {
        for path in [
            "",
            "/BroTypist/a",
            "BroTypist/../a",
            "BroTypist/./a",
            "BroTypist//a",
            "Other/a",
            "BroTypist/a\\b",
            "BroTypist/.env",
            "BroTypist/settings.json",
            "BroTypist/sub/model.GGUF",
            "BroTypist/sub/history.sqlite",
        ] {
            assert!(archive_path(path, "BroTypist").is_err(), "{path}");
        }
        for link in ["/outside", "../../outside", "a\\b", ""] {
            assert!(link_target(Path::new("BroTypist/link"), link, "BroTypist").is_err());
        }
        assert_eq!(
            link_target(
                Path::new("BroTypist/F.framework/F"),
                "Versions/Current/F",
                "BroTypist"
            )
            .unwrap(),
            Path::new("BroTypist/F.framework/Versions/Current/F")
        );
    }
    #[test]
    fn casefold_duplicates_link_and_file_parents_are_rejected() {
        let row = |path: &str, link: Option<&str>, directory| Member {
            path: path.into(),
            link: link.map(str::to_owned),
            directory,
            size: 0,
            mode: 0o755,
        };
        for rows in [
            vec![
                row("BroTypist/a", None, false),
                row("BroTypist/A", None, false),
            ],
            vec![
                row("BroTypist/ß", None, false),
                row("BroTypist/ss", None, false),
            ],
            vec![
                row("BroTypist/a", Some("internal"), false),
                row("BroTypist/A/file", None, false),
            ],
            vec![
                row("BroTypist/a", None, false),
                row("BroTypist/A/file", None, false),
            ],
        ] {
            assert!(validate_members(&rows).is_err());
        }
    }
    #[test]
    fn tar_preflight_refuses_hardlinks_devices_escape_and_private_payloads() {
        let f = fixture();
        for (name, kind, link) in [
            ("BroTypist/../../escape", tar::EntryType::Regular, None),
            ("BroTypist/a", tar::EntryType::Link, Some("BroTypist/b")),
            ("BroTypist/a", tar::EntryType::Char, None),
            ("BroTypist/a", tar::EntryType::Symlink, Some("../../escape")),
            ("BroTypist/settings.json", tar::EntryType::Regular, None),
        ] {
            let archive = f.root.join("bad.tar.gz");
            tar_file(&archive, &[(name, kind, b"", link)]);
            assert!(tar_members(&archive, "BroTypist").is_err());
            assert!(!f.root.join("escape").exists());
        }
    }
    #[test]
    fn archive_limits_are_enforced_before_extraction() {
        let mut total = MAX_UNPACKED_BYTES;
        assert!(check_limit(&mut total, 1, 1).is_err());
        let mut total = 0;
        assert!(check_limit(&mut total, 0, MAX_ARCHIVE_ENTRIES + 1).is_err());
        let f = fixture();
        let archive = f.root.join("huge.tar.gz");
        let mut header = tar::Header::new_gnu();
        header.set_path("BroTypist/huge").unwrap();
        header.set_mode(0o644);
        header.set_size(MAX_UNPACKED_BYTES + 1);
        header.set_cksum();
        let mut encoder = flate2::write::GzEncoder::new(
            File::create(&archive).unwrap(),
            flate2::Compression::default(),
        );
        encoder.write_all(header.as_bytes()).unwrap();
        encoder.finish().unwrap();
        assert!(tar_members(&archive, "BroTypist").is_err());
        let many = f.root.join("many.tar.gz");
        let mut encoder = flate2::write::GzEncoder::new(
            File::create(&many).unwrap(),
            flate2::Compression::default(),
        );
        for i in 0..=MAX_ARCHIVE_ENTRIES {
            let mut h = tar::Header::new_gnu();
            h.set_path(format!("BroTypist/a{i}")).unwrap();
            h.set_mode(0o644);
            h.set_size(0);
            h.set_cksum();
            encoder.write_all(h.as_bytes()).unwrap();
        }
        encoder.write_all(&[0u8; 1024]).unwrap();
        encoder.finish().unwrap();
        assert!(tar_members(&many, "BroTypist").is_err());
    }
    #[test]
    fn actual_tar_extraction_retains_safe_framework_links_and_bytes() {
        let f = fixture();
        let archive = f.root.join("safe.tar.gz");
        tar_file(
            &archive,
            &[
                ("BroTypist", tar::EntryType::Directory, b"", None),
                (
                    "BroTypist/F.framework/Versions/A/F",
                    tar::EntryType::Regular,
                    b"x",
                    None,
                ),
                (
                    "BroTypist/F.framework/Versions/Current",
                    tar::EntryType::Symlink,
                    b"",
                    Some("A"),
                ),
                (
                    "BroTypist/F.framework/F",
                    tar::EntryType::Symlink,
                    b"",
                    Some("Versions/Current/F"),
                ),
            ],
        );
        let stage = f.root.join("stage");
        private_dir(&stage);
        let bundle = extract_package(&archive, &stage, "linux").unwrap();
        assert!(
            fs::symlink_metadata(bundle.join("F.framework/F"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(bundle.join("F.framework/F")).unwrap(), b"x");
    }
    #[test]
    fn zip_preflight_rejects_traversal_and_duplicate_case_before_ditto() {
        let f = fixture();
        for names in [
            vec!["BroTypist.app/../../escape"],
            vec!["BroTypist.app/a", "BroTypist.app/A"],
        ] {
            let path = f.root.join("bad.zip");
            let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
            for name in names {
                zip.start_file(
                    name,
                    zip::write::SimpleFileOptions::default().unix_permissions(0o644),
                )
                .unwrap();
                zip.write_all(b"x").unwrap();
            }
            zip.finish().unwrap();
            assert!(zip_members(&path, "BroTypist.app").is_err());
        }
    }
    #[test]
    fn unicode15_archive_collisions_are_refused_after_nfc() {
        let f = fixture();
        for (upper, lower) in [
            ("\u{10570}", "\u{10597}"),
            ("\u{1c90}", "\u{10d0}"),
            ("\u{a7c4}", "\u{a794}"),
            ("é\u{10570}", "e\u{301}\u{10597}"),
        ] {
            let path = f.root.join("collision.zip");
            let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
            for name in [upper, lower] {
                zip.start_file(
                    format!("BroTypist.app/{name}"),
                    zip::write::SimpleFileOptions::default().unix_permissions(0o644),
                )
                .unwrap();
                zip.write_all(b"x").unwrap();
            }
            zip.finish().unwrap();
            assert!(zip_members(&path, "BroTypist.app").is_err());

            let path = f.root.join("collision.tar.gz");
            let upper = format!("BroTypist/{upper}");
            let lower = format!("BroTypist/{lower}");
            tar_file(
                &path,
                &[
                    (&upper, tar::EntryType::Regular, b"x", None),
                    (&lower, tar::EntryType::Regular, b"y", None),
                ],
            );
            assert!(tar_members(&path, "BroTypist").is_err());
        }
    }
    #[test]
    fn actual_atomic_promotion_and_rollback_preserve_complete_inodes() {
        let f = fixture();
        let candidate = f.root.join("candidate");
        make_bundle(&candidate, "1.2.4", &"2".repeat(40));
        let original = fs::metadata(&f.destination).unwrap().ino();
        let replacement = fs::metadata(&candidate).unwrap().ino();
        atomic_exchange(&candidate, &f.destination).unwrap();
        assert_eq!(
            fs::read(f.destination.join("payload-version")).unwrap(),
            b"1.2.4"
        );
        assert_eq!(
            fs::read(candidate.join("payload-version")).unwrap(),
            b"1.2.3"
        );
        assert_eq!(fs::metadata(&candidate).unwrap().ino(), original);
        assert_eq!(fs::metadata(&f.destination).unwrap().ino(), replacement);
        receipt(&f.root, &f.request, "promoted", Some(&candidate), None).unwrap();
        cancel_and_wait(&f.root, &f.request, &candidate, Duration::from_millis(100)).unwrap();
        atomic_exchange(&candidate, &f.destination).unwrap();
        receipt(&f.root, &f.request, "rolled_back", Some(&candidate), None).unwrap();
        assert_eq!(fs::metadata(&f.destination).unwrap().ino(), original);
        assert_eq!(
            fs::read(f.destination.join("payload-version")).unwrap(),
            b"1.2.3"
        );
        assert_eq!(
            transaction_status(&f.root, &f.request).unwrap(),
            "rolled_back"
        );
    }
    #[test]
    fn missing_exchange_operand_does_not_move_original_application() {
        let f = fixture();
        let original = fs::metadata(&f.destination).unwrap().ino();
        assert!(atomic_exchange(&f.root.join("missing"), &f.destination).is_err());
        assert_eq!(fs::metadata(&f.destination).unwrap().ino(), original);
    }
    #[test]
    fn destination_policy_detects_symlink_architecture_provenance_and_inode_changes() {
        let f = fixture();
        assert!(check_destination(&f.request).is_ok());
        let other = f.root.join("other");
        make_bundle(&other, "1.2.3", &"1".repeat(40));
        atomic_exchange(&other, &f.destination).unwrap();
        assert!(check_destination(&f.request).is_err());
        atomic_exchange(&other, &f.destination).unwrap();
        let exe = executable(&f.destination, native());
        fs::write(&exe, b"not native").unwrap();
        assert!(binary_architecture(&exe, native()).is_err());
        let linked = f.root.join("linked");
        std::os::unix::fs::symlink(&f.destination, &linked).unwrap();
        assert!(target_policy(&linked, native(), None).is_err());
        assert!(target_policy(&f.destination, native(), Some(&f.root)).is_err());
        assert!(managed_debian(Path::new("/opt/brotypist")).unwrap());
    }
    #[test]
    fn package_hash_refuses_tampering_truncation_growth_and_links() {
        let f = fixture();
        let bytes = b"archive";
        let asset = UpdateAsset {
            name: String::new(),
            target: String::new(),
            format: String::new(),
            size: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(bytes)),
        };
        let path = f.root.join("package");
        write_exclusive(&path, bytes).unwrap();
        verify_package(&path, &asset, true).unwrap();
        for data in [
            b"tampered".as_slice(),
            b"short".as_slice(),
            b"archive-extra".as_slice(),
        ] {
            fs::write(&path, data).unwrap();
            assert!(verify_package(&path, &asset, true).is_err());
        }
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(f.root.join("manifest.json"), &path).unwrap();
        assert!(verify_package(&path, &asset, true).is_err());
    }
    #[test]
    fn approval_byte_and_eof_are_not_actual_parent_exit() {
        let f = fixture();
        let (reader, writer) = pipe();
        let mut writer = File::from(writer);
        writer.write_all(b"1").unwrap();
        drop(writer);
        assert!(
            wait_for_exit(
                reader.as_raw_fd(),
                &f.request,
                &parent_ownership(&f.root, &f.request).unwrap().unwrap(),
                Duration::from_millis(20)
            )
            .is_err()
        );
        let (reader, writer) = pipe();
        let mut writer = File::from(writer);
        writer.write_all(b"1").unwrap();
        assert!(
            wait_for_exit(
                reader.as_raw_fd(),
                &f.request,
                &parent_ownership(&f.root, &f.request).unwrap().unwrap(),
                Duration::from_millis(20)
            )
            .is_err()
        );
        drop(writer);
        for approval in [b"".as_slice(), b"0".as_slice(), b"11".as_slice()] {
            let (reader, writer) = pipe();
            let mut writer = File::from(writer);
            writer.write_all(approval).unwrap();
            drop(writer);
            assert!(
                wait_for_exit(
                    reader.as_raw_fd(),
                    &f.request,
                    &parent_ownership(&f.root, &f.request).unwrap().unwrap(),
                    Duration::from_millis(20)
                )
                .is_err()
            );
        }
    }
    #[test]
    fn approval_accepts_only_after_real_disposable_parent_exits() {
        let f = fixture();
        let (reader, writer) = pipe();
        let fd = writer.as_raw_fd();
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 0.1; printf 1 >&3"]);
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(fd, 3) < 0 || libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        let identity = process_identity(child.id()).unwrap().unwrap();
        drop(writer);
        child.wait().unwrap();
        let r = Request {
            parent_pid: identity.pid,
            ..f.request.clone()
        };
        wait_for_exit(reader.as_raw_fd(), &r, &identity, Duration::from_secs(1)).unwrap();
    }
    #[test]
    fn dropped_uncommitted_handoff_closes_pipe_but_commit_survives_drop() {
        let f = fixture();
        let (reader, writer) = pipe();
        let process = Command::new("/bin/sleep").arg("1").spawn().unwrap();
        let mut handoff = Handoff {
            writer: Some(writer),
            process,
            request_root: f.root.clone(),
            committed: false,
        };
        handoff.abort();
        let mut bytes = [0u8; 2];
        assert_eq!(
            unsafe { libc::read(reader.as_raw_fd(), bytes.as_mut_ptr().cast(), 2) },
            0
        );
        handoff.process.kill().unwrap();
        handoff.process.wait().unwrap();
        let (reader, writer) = pipe();
        let process = Command::new("/bin/sleep").arg("1").spawn().unwrap();
        let mut handoff = Handoff {
            writer: Some(writer),
            process,
            request_root: f.root.clone(),
            committed: false,
        };
        handoff.commit().unwrap();
        handoff.abort();
        assert!(handoff.committed());
        assert_eq!(
            unsafe { libc::read(reader.as_raw_fd(), bytes.as_mut_ptr().cast(), 2) },
            1
        );
        assert_eq!(bytes[0], b'1');
        let mut poll = libc::pollfd {
            fd: reader.as_raw_fd(),
            events: libc::POLLIN | libc::POLLHUP,
            revents: 0,
        };
        assert_eq!(unsafe { libc::poll(&mut poll, 1, 10) }, 0);
        handoff.process.kill().unwrap();
        handoff.process.wait().unwrap();
        // Test owns this exact retained writer, so it can clean up without
        // changing the production rule that committed UI handles never close it.
        let retained = COMMITTED_WRITERS.lock().unwrap().pop().unwrap();
        unsafe {
            libc::close(retained);
        }
    }
    #[test]
    fn published_schema1_request_and_three_field_ack_interoperate_without_new_sidecars() {
        let f = fixture();
        fs::remove_file(f.root.join("parent.json")).unwrap();
        fs::remove_file(f.root.join("startup.lock")).unwrap();
        // Construct the published client's wire object independently: its exact
        // schema intentionally has neither parent generation nor helper locks.
        let old_request = json!({
            "schema":1,"destination":f.destination,"platform":native(),"identity":f.request.identity,
            "device":f.request.device,"inode":f.request.inode,"uid":f.request.uid,
            "parent_pid":f.request.parent_pid,"isolated_home":null,
            "nonce":f.request.nonce,"version":f.request.version,
        });
        updates::write_private(
            &f.root.join("request.json"),
            &serde_json::to_vec(&old_request).unwrap(),
        )
        .unwrap();
        let parsed = request(&f.root.join("request.json")).unwrap();
        assert!(parent_ownership(&f.root, &parsed).unwrap().is_none());
        assert_eq!(serde_json::to_value(&parsed).unwrap(), old_request);
        promoted(&f);
        let old_receipt = json!({
            "schema":1,"application":APP_ID,"version":f.request.version,"status":"promoted",
            "previous_version":f.request.identity.version,"notarization":"not claimed",
            "rollback":f.root.join("payload").join(f.destination.file_name().unwrap()),
        });
        updates::write_private(
            &f.root.join("transaction.json"),
            &serde_json::to_vec(&old_receipt).unwrap(),
        )
        .unwrap();
        assert!(old_receipt.get("nonce").is_none());
        let context = enroll_startup(
            &f.root,
            &f.request.nonce,
            &executable(&f.destination, native()),
            fixture_verify,
        )
        .unwrap();
        assert!(f.root.join("startup.lock").is_file());
        assert!(
            parent_ownership(&f.root, &parsed).unwrap().is_none(),
            "A legacy startup must not fabricate parent lifetime evidence."
        );
        assert!(acknowledge_context(&context, fixture_verify).unwrap());
        let value =
            updates::strict_json(&read_file(&f.root.join("startup.json"), 1024, true).unwrap())
                .unwrap();
        let keys: HashSet<_> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, HashSet::from(["nonce", "version", "pid"]));
        // These are the published _wait_startup consumer's exact conditions,
        // not a permissive new decoder that silently ignores an extra field.
        assert_eq!(value["nonce"], old_request["nonce"]);
        assert_eq!(value["version"], old_request["version"]);
        let pid = value["pid"].as_u64().unwrap();
        assert!(pid > 0);
        assert!(process_identity(pid as u32).unwrap().is_some());
        let leases = leases(&f.root, &parsed).unwrap();
        assert_eq!(leases.len(), 1);
        assert_eq!(leases[0].process.start, context.process.start);
        // Explicit external-oracle smoke only; neither the normal Rust test
        // suite nor the shipped application references a legacy source tree.
        // The old consumer runs while this Rust producer PID is still alive.
        let python = std::env::var_os("BROTYPIST_REFERENCE_PYTHON");
        let reference_root = std::env::var_os("BROTYPIST_REFERENCE_ROOT");
        if python.is_some() || reference_root.is_some() {
            let python=python.expect("Set both BROTYPIST_REFERENCE_PYTHON and BROTYPIST_REFERENCE_ROOT for external interoperability.");
            let reference=PathBuf::from(reference_root.expect("Set both BROTYPIST_REFERENCE_PYTHON and BROTYPIST_REFERENCE_ROOT for external interoperability.")).join("src").canonicalize().expect("BROTYPIST_REFERENCE_ROOT must identify the external oracle checkout.");
            let script = r#"import json, subprocess, sys
from pathlib import Path
from inklow.update_install import _wait_startup
root = Path(sys.argv[1])
request = json.loads((root / "request.json").read_bytes())
launcher = subprocess.Popen(["/bin/sleep", "10"])
try:
    _wait_startup(root, request, launcher, timeout=1)
finally:
    launcher.terminate()
    launcher.wait(timeout=5)
"#;
            let mut command = clean_command(python);
            command
                .args(["-c", script])
                .arg(&f.root)
                .env("PYTHONPATH", reference);
            run_output(&mut command, Duration::from_secs(10)).unwrap();
        }
    }
    #[test]
    fn malformed_parent_sidecar_cannot_downgrade_to_published_protocol() {
        let f = fixture();
        promoted(&f);
        updates::write_private(&f.root.join("parent.json"), b"{}").unwrap();
        assert!(
            enroll_startup(
                &f.root,
                &f.request.nonce,
                &executable(&f.destination, native()),
                fixture_verify
            )
            .is_err()
        );
        fs::remove_file(f.root.join("parent.json")).unwrap();
        std::os::unix::fs::symlink(f.root.join("request.json"), f.root.join("parent.json"))
            .unwrap();
        assert!(parent_ownership(&f.root, &f.request).is_err());
    }
    #[test]
    fn nonce_signed_build_and_exclusive_ack_are_required() {
        let f = fixture();
        promoted(&f);
        assert!(
            enroll_startup(
                &f.root,
                "b".repeat(64).as_str(),
                &executable(&f.destination, native()),
                fixture_verify
            )
            .is_err()
        );
        assert!(
            enroll_startup(
                &f.root,
                &f.request.nonce,
                &f.root.join("different"),
                fixture_verify
            )
            .is_err()
        );
        assert!(
            enroll_startup(
                &f.root,
                &f.request.nonce,
                &executable(&f.destination, native()),
                updates::verify_manifest
            )
            .is_err()
        );
        let context = enroll_startup(
            &f.root,
            &f.request.nonce,
            &executable(&f.destination, native()),
            fixture_verify,
        )
        .unwrap();
        assert!(acknowledge_context(&context, fixture_verify).unwrap());
        assert!(!acknowledge_context(&context, fixture_verify).unwrap());
        let ack: StartupAck = serde_json::from_value(
            updates::strict_json(&read_file(&f.root.join("startup.json"), 1024, true).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(ack.nonce, f.request.nonce);
        assert_eq!(ack.pid, std::process::id());
        assert_eq!(
            leases(&f.root, &f.request).unwrap()[0].process.start,
            context.process.start
        );
        fs::write(f.root.join("manifest.sig"), b"tampered").unwrap();
        assert!(acknowledge_context(&context, fixture_verify).is_err());
    }
    #[test]
    fn signed_source_provenance_cannot_be_changed_by_the_extracted_marker() {
        let f = fixture();
        promoted(&f);
        let path = f.destination.join(if native() == "darwin" {
            "Contents/Resources/.inklow-build.json"
        } else {
            MARKER
        });
        let mut identity = marker(&f.destination, native()).unwrap();
        identity.source_commit = "3".repeat(40);
        updates::write_private(&path, &serde_json::to_vec(&identity).unwrap()).unwrap();
        assert!(
            enroll_startup(
                &f.root,
                &f.request.nonce,
                &executable(&f.destination, native()),
                fixture_verify
            )
            .is_err()
        );
    }
    #[test]
    fn a_live_unrelated_pid_is_not_a_gui_startup_acknowledgment() {
        let f = fixture();
        promoted(&f);
        let current = process_identity(std::process::id()).unwrap().unwrap();
        let ack = StartupAck {
            nonce: f.request.nonce.clone(),
            version: f.request.version.clone(),
            pid: current.pid,
        };
        write_exclusive(
            &f.root.join("startup.json"),
            &serde_json::to_vec(&ack).unwrap(),
        )
        .unwrap();
        let mut launcher = Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        launcher.wait().unwrap();
        assert!(
            wait_startup(
                &f.root,
                &f.request,
                &mut launcher,
                Duration::from_millis(20)
            )
            .is_err()
        );
        assert!(same_process(&process_identity(std::process::id()).unwrap().unwrap()).unwrap());
    }
    #[test]
    fn standalone_helper_requires_expected_layout_regular_owned_executable_and_native_architecture()
    {
        let f = fixture();
        assert!(bundled_helper(&f.destination, native()).is_err());
        let helper = f.destination.join(if native() == "darwin" {
            "Contents/Helpers/BroTypist-updater"
        } else {
            "helpers/BroTypist-updater"
        });
        fs::create_dir_all(helper.parent().unwrap()).unwrap();
        fs::copy(executable(&f.destination, native()), &helper).unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(bundled_helper(&f.destination, native()).unwrap(), helper);
        let copy = f.root.join("copied-helper");
        copy_private(&helper, &copy, 128 * 1024 * 1024).unwrap();
        assert_eq!(fs::read(&copy).unwrap(), fs::read(&helper).unwrap());
        let original = fs::read(&helper).unwrap();
        let mut wrong_arch = original.clone();
        if native() == "darwin" {
            wrong_arch[4..8].copy_from_slice(
                &(if architecture() == "arm64" {
                    0x1000007u32
                } else {
                    0x100000c
                })
                .to_le_bytes(),
            );
        } else {
            wrong_arch[18..20].copy_from_slice(
                &(if architecture() == "arm64" {
                    62u16
                } else {
                    183
                })
                .to_le_bytes(),
            );
        }
        fs::write(&helper, wrong_arch).unwrap();
        assert!(bundled_helper(&f.destination, native()).is_err());
        fs::write(&helper, original).unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o720)).unwrap();
        assert!(bundled_helper(&f.destination, native()).is_err());
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(bundled_helper(&f.destination, native()).is_err());
        fs::remove_file(&helper).unwrap();
        std::os::unix::fs::symlink(executable(&f.destination, native()), &helper).unwrap();
        assert!(bundled_helper(&f.destination, native()).is_err());
        fs::remove_file(&helper).unwrap();
        write_exclusive(&helper, b"not a native helper").unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(bundled_helper(&f.destination, native()).is_err());
    }
    #[test]
    fn open_launcher_success_is_not_gui_health_and_cancelled_late_start_is_refused() {
        let f = fixture();
        promoted(&f);
        let mut launcher = Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        launcher.wait().unwrap();
        assert!(
            wait_startup(
                &f.root,
                &f.request,
                &mut launcher,
                Duration::from_millis(20)
            )
            .is_err()
        );
        cancel_and_wait(
            &f.root,
            &f.request,
            &f.root.join("previous"),
            Duration::from_millis(20),
        )
        .unwrap();
        assert!(
            enroll_startup(
                &f.root,
                &f.request.nonce,
                &executable(&f.destination, native()),
                fixture_verify
            )
            .is_err()
        );
        assert!(
            !f.root
                .join(format!("process-{}.json", std::process::id()))
                .exists()
        );
    }
    #[test]
    fn unconfirmed_process_lifetime_refuses_rollback_and_keeps_both_builds() {
        let f = fixture();
        promoted(&f);
        let candidate = f.root.join("previous");
        make_bundle(&candidate, "1.2.3", &"1".repeat(40));
        let _context = enroll_startup(
            &f.root,
            &f.request.nonce,
            &executable(&f.destination, native()),
            fixture_verify,
        )
        .unwrap();
        let before = fs::metadata(&f.destination).unwrap().ino();
        assert!(
            cancel_and_wait(&f.root, &f.request, &candidate, Duration::from_millis(20)).is_err()
        );
        assert_eq!(fs::metadata(&f.destination).unwrap().ino(), before);
        assert!(candidate.exists());
        assert_eq!(
            transaction_status(&f.root, &f.request).unwrap(),
            "cancelled"
        );
    }
    #[test]
    fn pid_generation_mismatch_is_not_a_live_owned_process() {
        let current = process_identity(std::process::id()).unwrap().unwrap();
        assert!(same_process(&current).unwrap());
        let stale = ProcessIdentity {
            start: format!("{}-stale", current.start),
            ..current
        };
        assert!(!same_process(&stale).unwrap());
    }
    #[test]
    fn startup_child_dispatch() {
        let Some(root) = std::env::var_os("BROTYPIST_TEST_STARTUP_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        let r = request(&root.join("request.json")).unwrap();
        let context = enroll_startup(
            &root,
            &r.nonce,
            &executable(&r.destination, &r.platform),
            fixture_verify,
        )
        .unwrap();
        start_cancellation_watcher(context).unwrap();
        loop {
            thread::sleep(Duration::from_secs(1));
        }
    }
    #[test]
    fn delayed_owned_replacement_self_exits_before_rollback_without_signaling_unrelated_child() {
        let f = fixture();
        promoted(&f);
        let previous = f.root.join("previous");
        make_bundle(&previous, "1.2.3", &"1".repeat(40));
        let mut unrelated = Command::new("/bin/sleep").arg("10").spawn().unwrap();
        let unrelated_identity = process_identity(unrelated.id()).unwrap().unwrap();
        let mut replacement = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "update_install::tests::startup_child_dispatch",
                "--nocapture",
            ])
            .env("BROTYPIST_TEST_STARTUP_ROOT", &f.root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let lease = f.root.join(format!("process-{}.json", replacement.id()));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !lease.exists()
            && replacement.try_wait().unwrap().is_none()
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(lease.exists(), "Disposable replacement did not enroll.");
        cancel_and_wait(&f.root, &f.request, &previous, Duration::from_secs(5)).unwrap();
        assert_eq!(replacement.wait().unwrap().code(), Some(78));
        assert!(same_process(&unrelated_identity).unwrap());
        let original = fs::metadata(&previous).unwrap().ino();
        atomic_exchange(&previous, &f.destination).unwrap();
        assert_eq!(fs::metadata(&f.destination).unwrap().ino(), original);
        unrelated.kill().unwrap();
        unrelated.wait().unwrap();
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn native_quarantine_copy_preserves_existing_provenance() {
        let f = fixture();
        let source = f.root.join("source");
        let dest = f.root.join("destination");
        write_exclusive(&source, b"archive").unwrap();
        let value = b"0083;00000000;Browser;";
        let p = CString::new(source.as_os_str().as_bytes()).unwrap();
        assert_eq!(
            unsafe {
                libc::setxattr(
                    p.as_ptr(),
                    c"com.apple.quarantine".as_ptr(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                    libc::XATTR_NOFOLLOW,
                )
            },
            0
        );
        copy_private(&source, &dest, 1024).unwrap();
        let query = |path: &Path| {
            let p = CString::new(path.as_os_str().as_bytes()).unwrap();
            let mut bytes = [0u8; 256];
            let n = unsafe {
                libc::getxattr(
                    p.as_ptr(),
                    c"com.apple.quarantine".as_ptr(),
                    bytes.as_mut_ptr().cast(),
                    bytes.len(),
                    0,
                    libc::XATTR_NOFOLLOW,
                )
            };
            assert!(n >= 0);
            bytes[..n as usize].to_vec()
        };
        assert_eq!(query(&dest), value);
        let changed = b"0083;11111111;OtherBrowser;";
        assert_eq!(
            unsafe {
                libc::setxattr(
                    p.as_ptr(),
                    c"com.apple.quarantine".as_ptr(),
                    changed.as_ptr().cast(),
                    changed.len(),
                    0,
                    libc::XATTR_NOFOLLOW,
                )
            },
            0
        );
        preserve_quarantine(&source, &dest).unwrap();
        assert_eq!(query(&dest), value);
    }
}
