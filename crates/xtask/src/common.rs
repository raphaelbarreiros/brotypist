use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Output},
};
use unicode_casefold::UnicodeCaseFold;
use unicode_normalization::UnicodeNormalization;

pub use crate::update_keys::APP_ID;
pub const MARKER: &str = ".inklow-build.json";
pub const LLAMA_REVISION: &str = "0c1e57098bba43ac29e6e3b677cdceebdd22334f";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub application: String,
    pub platform: String,
    pub version: String,
    pub source_commit: String,
    pub architecture: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Row {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

pub fn platform() -> Result<&'static str> {
    match std::env::consts::OS {
        "macos" => Ok("darwin"),
        "linux" => Ok("linux"),
        "windows" => Ok("win32"),
        os => bail!("unsupported host {os}"),
    }
}
pub fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        value => value,
    }
}
pub fn native_target() -> Result<String> {
    let text = run(Command::new("rustc").arg("-vV"))?;
    let host = text
        .lines()
        .find_map(|v| v.strip_prefix("host: "))
        .context("rustc did not report native host")?;
    Ok(host.to_owned())
}
pub fn run(command: &mut Command) -> Result<String> {
    let Output {
        status,
        stdout,
        stderr,
    } = command
        .output()
        .with_context(|| format!("starting {command:?}"))?;
    ensure!(
        status.success(),
        "{command:?} failed ({status}):\n{}\n{}",
        String::from_utf8_lossy(&stderr),
        String::from_utf8_lossy(&stdout)
    );
    Ok(String::from_utf8(stdout)
        .context("command emitted non-UTF-8 output")?
        .trim()
        .to_owned())
}
pub fn commit(root: &Path) -> Result<String> {
    let value = run(Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"]))?;
    validate_commit(&value)?;
    Ok(value)
}
pub fn validate_commit(value: &str) -> Result<()> {
    ensure!(
        value.len() == 40
            && value
                .bytes()
                .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v)),
        "full lowercase 40-character source commit required"
    );
    Ok(())
}
pub fn validate_version(value: &str) -> Result<[u32; 3]> {
    let parts: Vec<_> = value.split('.').collect();
    ensure!(parts.len() == 3, "numeric major.minor.patch required");
    let mut result = [0; 3];
    for (i, part) in parts.iter().enumerate() {
        ensure!(
            !part.is_empty()
                && part.bytes().all(|v| v.is_ascii_digit())
                && (part.len() == 1 || !part.starts_with('0')),
            "invalid numeric version"
        );
        result[i] = part.parse()?;
    }
    Ok(result)
}
pub fn version(root: &Path) -> Result<String> {
    let document: toml::Value = toml::from_str(&fs::read_to_string(
        root.join("crates/brotypist/Cargo.toml"),
    )?)?;
    let value = document["package"]["version"]
        .as_str()
        .context("missing Cargo application version")?;
    validate_version(value)?;
    Ok(value.to_owned())
}
pub fn set_version(root: &Path, version: &str) -> Result<()> {
    validate_version(version)?;
    let lock_path = root.join("Cargo.lock");
    let lock_original = fs::read_to_string(&lock_path)?;
    let mut expected_lock: toml::Value = toml::from_str(&lock_original)?;
    let names = ["brotypist", "brotypist-updater", "xtask"];
    let mut found = [false; 3];
    for package in expected_lock
        .get_mut("package")
        .and_then(toml::Value::as_array_mut)
        .context("Cargo.lock has no packages")?
    {
        if package.get("source").is_none()
            && let Some(index) = names
                .iter()
                .position(|name| package.get("name").and_then(toml::Value::as_str) == Some(*name))
        {
            ensure!(!found[index], "duplicate workspace package in Cargo.lock");
            found[index] = true;
            package["version"] = version.into();
        }
    }
    ensure!(
        found.into_iter().all(|value| value),
        "Cargo.lock is missing a workspace package"
    );
    let mut originals = vec![(lock_path.clone(), lock_original)];
    // Only exact package version declarations are changed; dependency constraints and source files are untouched.
    let mut changes = Vec::new();
    for relative in [
        "crates/brotypist/Cargo.toml",
        "crates/brotypist-updater/Cargo.toml",
        "crates/xtask/Cargo.toml",
    ] {
        let path = root.join(relative);
        let original = fs::read_to_string(&path)?;
        let mut section = false;
        let mut changed = false;
        let mut output = String::new();
        for line in original.lines() {
            if line.starts_with('[') {
                section = line.trim() == "[package]";
            }
            if section && line.trim_start().starts_with("version =") {
                ensure!(!changed, "multiple package version declarations");
                output.push_str(&format!("version = \"{version}\"\n"));
                changed = true;
            } else {
                output.push_str(line);
                output.push('\n');
            }
        }
        ensure!(
            changed,
            "package has no explicit version: {}",
            path.display()
        );
        let _: toml::Value = toml::from_str(&output)?;
        changes.push((path.clone(), output));
        originals.push((path, original));
    }
    let outcome = (|| -> Result<()> {
        for (path, text) in changes {
            let mut file = OpenOptions::new().write(true).truncate(true).open(path)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
        }
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        run(Command::new(cargo)
            .current_dir(root)
            .args(["update", "--workspace", "--offline"]))?;
        let actual_lock: toml::Value = toml::from_str(&fs::read_to_string(&lock_path)?)?;
        ensure!(
            actual_lock == expected_lock,
            "version update changed unrelated locked dependencies"
        );
        Ok(())
    })();
    if let Err(error) = outcome {
        let mut failures = Vec::new();
        for (path, text) in originals {
            if let Err(restore) = fs::write(&path, text) {
                failures.push(format!("{}: {restore}", path.display()));
            }
        }
        ensure!(
            failures.is_empty(),
            "version update failed: {error:#}; restoration failed: {}",
            failures.join("; ")
        );
        return Err(error.context("Version update failed; original manifests and lock restored"));
    }
    Ok(())
}
pub fn marker(bundle: &Path, platform: &str) -> PathBuf {
    if platform == "darwin" {
        bundle.join("Contents/Resources").join(MARKER)
    } else {
        bundle.join(MARKER)
    }
}
pub fn regular(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && !linked(&metadata),
        "not a regular unlinked file: {}",
        path.display()
    );
    Ok(())
}
pub fn linked(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    false
}
pub fn real_dir(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir() && !linked(&metadata),
        "not a real directory: {}",
        path.display()
    );
    Ok(())
}
pub fn read_identity(bundle: &Path, platform: &str) -> Result<Identity> {
    real_dir(bundle)?;
    if platform == "darwin" {
        real_dir(&bundle.join("Contents"))?;
        real_dir(&bundle.join("Contents/Resources"))?;
    }
    let path = marker(bundle, platform);
    regular(&path)?;
    let identity: Identity = serde_json::from_slice(&fs::read(path)?)?;
    ensure!(
        identity.application == APP_ID && identity.platform == platform,
        "foreign build identity"
    );
    validate_version(&identity.version)?;
    validate_commit(&identity.source_commit)?;
    Ok(identity)
}
pub fn destination(path: &Path, root: &Path, replace: bool, platform: &str) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let name = absolute
        .file_name()
        .context("destination needs a filename")?;
    fs::create_dir_all(absolute.parent().context("destination needs parent")?)?;
    let path = absolute.parent().unwrap().canonicalize()?.join(name);
    ensure!(
        !root.starts_with(&path),
        "destination contains project source"
    );
    if platform == "darwin" {
        ensure!(
            path.extension() == Some(OsStr::new("app")),
            "macOS destination must end in .app"
        );
    }
    ensure_destination(&path, replace, platform)?;
    Ok(path)
}
pub fn ensure_destination(path: &Path, replace: bool, platform: &str) -> Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
        Ok(metadata) => {
            ensure!(!linked(&metadata), "refusing linked destination");
            ensure!(
                replace,
                "destination exists; use --replace only for owned builds"
            );
            read_identity(path, platform)?;
            Ok(())
        }
    }
}
pub fn stage(parent: &Path, label: &str) -> Result<PathBuf> {
    fs::create_dir_all(parent)?;
    let path = parent.join(format!(".inklow-{label}-{}", uuid::Uuid::new_v4()));
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&path)?;
    #[cfg(windows)]
    {
        let user = run(Command::new("whoami").args(["/user", "/fo", "csv", "/nh"]))?;
        let sid = user
            .trim()
            .trim_end_matches('"')
            .rsplit("\",\"")
            .next()
            .context("current user SID unavailable")?;
        ensure!(
            sid.starts_with("S-1-")
                && sid
                    .bytes()
                    .all(|c| c.is_ascii_digit() || c == b'-' || c == b'S'),
            "invalid current user SID"
        );
        run(Command::new("icacls")
            .arg(&path)
            .args(["/inheritance:r", "/grant:r"])
            .arg(format!("*{sid}:(OI)(CI)F")))?;
    }
    Ok(path)
}
pub fn write_new(path: &Path, data: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("exclusive output {}", path.display()))?;
    file.write_all(data)?;
    file.sync_all()?;
    Ok(())
}
pub fn json_new<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    write_new(path, &bytes)
}
pub fn hash(path: &Path) -> Result<String> {
    regular(path)?;
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
pub fn safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path.components().all(|part| {
            let Component::Normal(name) = part else {
                return false;
            };
            let Some(name) = name.to_str() else {
                return false;
            };
            if name.is_empty()
                || name.ends_with(['.', ' '])
                || name.chars().any(|v| {
                    v.is_control() || ['\\', '/', ':', '<', '>', '"', '|', '?', '*'].contains(&v)
                })
            {
                return false;
            }
            let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
            !["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
                && !(stem.len() == 4
                    && (stem.starts_with("COM") || stem.starts_with("LPT"))
                    && matches!(stem.as_bytes()[3], b'1'..=b'9'))
        })
}
pub fn inventory(root: &Path) -> Result<Vec<Row>> {
    real_dir(root)?;
    fn walk(
        root: &Path,
        dir: &Path,
        rows: &mut Vec<Row>,
        seen: &mut BTreeSet<String>,
    ) -> Result<()> {
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            let metadata = fs::symlink_metadata(&path)?;
            ensure!(
                !linked(&metadata),
                "linked payload refused: {}",
                path.display()
            );
            let relative = path.strip_prefix(root)?;
            ensure!(safe_relative(relative), "unsafe archive path");
            let name = path.file_name().unwrap().to_string_lossy().to_lowercase();
            let extension = path
                .extension()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase();
            ensure!(
                ![
                    "gguf",
                    "ggml",
                    "safetensors",
                    "pt",
                    "pth",
                    "db",
                    "sqlite",
                    "sqlite3",
                    "py",
                    "pyc",
                    "pyo",
                    "pyz"
                ]
                .contains(&extension.as_str())
                    && ![
                        "settings.json",
                        "config.json",
                        "secrets.yaml",
                        ".env",
                        "history.enc",
                        "python",
                        "python3",
                        "venv",
                        ".venv",
                        "site-packages"
                    ]
                    .contains(&name.as_str())
                    && !name.starts_with("libpython")
                    && !name.starts_with("python3.")
                    && !name.starts_with("pyside")
                    && !name.starts_with("pyobjc"),
                "private/model/Python payload refused: {}",
                relative.display()
            );
            let spelling = relative
                .to_str()
                .context("non-UTF-8 payload path")?
                .replace('\\', "/");
            ensure!(
                seen.insert(spelling.nfc().case_fold().collect()),
                "case-folded payload collision"
            );
            if metadata.is_dir() {
                walk(root, &path, rows, seen)?;
            } else {
                ensure!(metadata.is_file(), "special payload entry");
                rows.push(Row {
                    path: spelling,
                    size: metadata.len(),
                    sha256: hash(&path)?,
                });
            }
        }
        Ok(())
    }
    let mut rows = Vec::new();
    walk(root, root, &mut rows, &mut BTreeSet::new())?;
    rows.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(rows)
}
pub fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    real_dir(from)?;
    fs::create_dir(to)?;
    for entry in fs::read_dir(from)? {
        let path = entry?.path();
        let target = to.join(path.file_name().unwrap());
        let metadata = fs::symlink_metadata(&path)?;
        ensure!(!linked(&metadata), "linked copy source refused");
        if metadata.is_dir() {
            copy_tree(&path, &target)?;
        } else {
            ensure!(metadata.is_file(), "special copy source refused");
            fs::copy(&path, target)?;
        }
    }
    Ok(())
}
pub fn executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    }
    let _ = path;
    Ok(())
}
pub fn publish_file(from: &Path, to: &Path) -> Result<()> {
    regular(from)?;
    ensure!(fs::metadata(from)?.len() > 0, "empty artifact refused");
    OpenOptions::new().write(true).open(from)?.sync_all()?;
    // Same-filesystem hard-link is exclusive even if another publisher races this invocation.
    fs::hard_link(from, to)
        .with_context(|| format!("publishing exclusive artifact {}", to.display()))?;
    sync_directory(to.parent().context("artifact parent missing")?)?;
    Ok(())
}
pub fn promote(bundle: &Path, to: &Path, replace: bool, platform: &str) -> Result<()> {
    promote_with(bundle, to, replace, platform, &mut |a, b| fs::rename(a, b))
}
fn promote_with(
    bundle: &Path,
    to: &Path,
    replace: bool,
    platform: &str,
    rename: &mut dyn FnMut(&Path, &Path) -> std::io::Result<()>,
) -> Result<()> {
    ensure_destination(to, replace, platform)?;
    sync_tree(bundle)?;
    // Backup is a sibling of the destination, NEVER a child of temporary staging (R4).
    let backup = to
        .parent()
        .context("missing destination parent")?
        .join(format!(".inklow-previous-{}", uuid::Uuid::new_v4()));
    let had_previous = to.exists();
    if had_previous {
        rename(to, &backup)?;
    }
    sync_directory(to.parent().unwrap())?;
    if let Err(error) = rename(bundle, to) {
        if had_previous {
            if let Err(restore) = rename(&backup, to) {
                bail!(
                    "promotion failed: {error}; restore failed: {restore}; previous build retained at {}",
                    backup.display()
                );
            }
        }
        return Err(error.into());
    }
    // Commit point: the new complete app occupies destination. An interrupt before cleanup retains the old sibling.
    sync_directory(to.parent().unwrap())?;
    if had_previous {
        fs::remove_dir_all(&backup).with_context(|| {
            format!(
                "new build committed; previous build retained at {}",
                backup.display()
            )
        })?;
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    let _ = path;
    Ok(())
}
fn sync_tree(root: &Path) -> Result<()> {
    real_dir(root)?;
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        let metadata = fs::symlink_metadata(&path)?;
        ensure!(!linked(&metadata), "linked promotion input refused");
        if metadata.is_dir() {
            sync_tree(&path)?;
        } else {
            regular(&path)?;
            OpenOptions::new().write(true).open(path)?.sync_all()?;
        }
    }
    sync_directory(root)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceState {
    pub base_commit: String,
    pub dirty: bool,
    pub source_digest: String,
}
pub fn source_state(root: &Path) -> Result<SourceState> {
    let base_commit = commit(root)?;
    let dirty = !run(Command::new("git").arg("-C").arg(root).args([
        "status",
        "--porcelain",
        "--untracked-files=normal",
    ]))?
    .is_empty();
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .output()?;
    ensure!(output.status.success(), "cannot inventory checkout source");
    let mut names: Vec<_> = output
        .stdout
        .split(|v| *v == 0)
        .filter(|v| !v.is_empty())
        .collect();
    names.sort();
    names.dedup();
    let mut digest = Sha256::new();
    for name in names {
        let name = std::str::from_utf8(name).context("source path is not UTF8")?;
        ensure!(safe_relative(Path::new(name)), "unsafe source path");
        digest.update((name.len() as u64).to_le_bytes());
        digest.update(name.as_bytes());
        let path = root.join(name);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() && !linked(&metadata) => {
                digest.update(b"file");
                digest.update(metadata.len().to_le_bytes());
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    digest.update((metadata.permissions().mode() & 0o111).to_le_bytes());
                }
                digest.update(hash(&path)?.as_bytes());
            }
            Ok(metadata) if metadata.file_type().is_symlink() => {
                digest.update(b"symlink");
                digest.update(fs::read_link(path)?.to_string_lossy().as_bytes());
            }
            Ok(metadata) if metadata.is_dir() => {
                digest.update(b"gitlink");
                digest.update(commit(&path)?.as_bytes());
                let status = run(Command::new("git")
                    .arg("-C")
                    .arg(&path)
                    .args(["status", "--porcelain"]))?;
                ensure!(status.is_empty(), "dirty source submodule refused");
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => digest.update(b"deleted"),
            _ => bail!("unsupported checkout source entry {}", path.display()),
        }
    }
    Ok(SourceState {
        base_commit,
        dirty,
        source_digest: format!("{:x}", digest.finalize()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn version_change_keeps_locked_workspace_resolution() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = [\"crates/brotypist\", \"crates/brotypist-updater\", \"crates/xtask\"]\nresolver = \"3\"\n").unwrap();
        for name in ["brotypist", "brotypist-updater", "xtask"] {
            let directory = root.join("crates").join(name);
            fs::create_dir_all(directory.join("src")).unwrap();
            fs::write(directory.join("src/lib.rs"), "pub fn versioned() {}\n").unwrap();
            fs::write(
                directory.join("Cargo.toml"),
                format!("[package]\nname = \"{name}\"\nversion = \"0.4.13\"\nedition = \"2024\"\n"),
            )
            .unwrap();
        }
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        run(Command::new(&cargo)
            .current_dir(root)
            .args(["generate-lockfile", "--offline"]))
        .unwrap();
        set_version(root, "0.4.14").unwrap();
        let metadata = run(Command::new(&cargo).current_dir(root).args([
            "metadata",
            "--offline",
            "--locked",
            "--format-version",
            "1",
        ]))
        .unwrap();
        let metadata: serde_json::Value = serde_json::from_str(&metadata).unwrap();
        for package in metadata["packages"].as_array().unwrap() {
            assert_eq!(package["version"], "0.4.14");
        }
    }
    fn identity() -> Identity {
        Identity {
            application: APP_ID.into(),
            platform: "linux".into(),
            version: "0.4.13".into(),
            source_commit: "a".repeat(40),
            architecture: "x86_64".into(),
        }
    }
    #[test]
    fn hash_known_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let p = temp.path().join("bytes");
        fs::write(&p, b"abc").unwrap();
        assert_eq!(
            hash(&p).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
    #[test]
    fn ownership_is_strict() {
        let temp = tempfile::tempdir().unwrap();
        assert!(ensure_destination(temp.path(), true, "linux").is_err());
        json_new(&temp.path().join(MARKER), &identity()).unwrap();
        assert!(ensure_destination(temp.path(), false, "linux").is_err());
        assert!(ensure_destination(temp.path(), true, "linux").is_ok());
        assert!(ensure_destination(temp.path(), true, "win32").is_err());
    }
    #[test]
    fn archive_paths_and_private_payload() {
        for value in [
            "../x",
            "/x",
            "a/../x",
            "C:x",
            "CON.txt",
            "a. ",
            "a/b:stream",
        ] {
            assert!(!safe_relative(Path::new(value)));
        }
        #[cfg(unix)]
        assert!(!safe_relative(Path::new("a\\b")));
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("weights.gguf"), b"x").unwrap();
        assert!(inventory(temp.path()).is_err());
    }
    #[test]
    fn casing_collision_refused() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("App"), b"x").unwrap();
        if fs::write(temp.path().join("app"), b"y").is_ok()
            && fs::read_dir(temp.path()).unwrap().count() == 2
        {
            assert!(inventory(temp.path()).is_err());
        }
    }
    #[test]
    fn replacement_failure_restores_previous() {
        let temp = tempfile::tempdir().unwrap();
        let old = temp.path().join("app");
        fs::create_dir(&old).unwrap();
        json_new(&old.join(MARKER), &identity()).unwrap();
        fs::write(old.join("old"), b"previous").unwrap();
        let stage = temp.path().join("stage");
        fs::create_dir(&stage).unwrap();
        let mut calls = 0;
        let result = promote_with(&stage, &old, true, "linux", &mut |a, b| {
            calls += 1;
            if calls == 2 {
                Err(std::io::Error::other("injected interruption"))
            } else {
                fs::rename(a, b)
            }
        });
        assert!(result.is_err());
        assert_eq!(fs::read(old.join("old")).unwrap(), b"previous");
    }
    #[test]
    fn failed_restore_survives_stage_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        let old = temp.path().join("app");
        fs::create_dir(&old).unwrap();
        json_new(&old.join(MARKER), &identity()).unwrap();
        let stage = temp.path().join("stage");
        fs::create_dir(&stage).unwrap();
        let candidate = stage.join("candidate");
        fs::create_dir(&candidate).unwrap();
        let mut calls = 0;
        assert!(
            promote_with(&candidate, &old, true, "linux", &mut |a, b| {
                calls += 1;
                if calls >= 2 {
                    Err(std::io::Error::other("interrupt"))
                } else {
                    fs::rename(a, b)
                }
            })
            .is_err()
        );
        fs::remove_dir_all(stage).unwrap();
        assert!(fs::read_dir(temp.path()).unwrap().any(|e| {
            e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".inklow-previous-")
        }));
    }
    #[cfg(unix)]
    #[test]
    fn symlink_destination_refused() {
        let temp = tempfile::tempdir().unwrap();
        let link = temp.path().join("link");
        std::os::unix::fs::symlink("absent", &link).unwrap();
        assert!(ensure_destination(&link, true, "linux").is_err());
    }
    #[test]
    fn exclusive_artifact_does_not_overwrite() {
        let temp = tempfile::tempdir().unwrap();
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        fs::write(&a, b"new").unwrap();
        fs::write(&b, b"old").unwrap();
        assert!(publish_file(&a, &b).is_err());
        assert_eq!(fs::read(b).unwrap(), b"old");
    }
    #[test]
    fn panic_between_renames_retains_old_sibling() {
        let temp = tempfile::tempdir().unwrap();
        let old = temp.path().join("app");
        fs::create_dir(&old).unwrap();
        json_new(&old.join(MARKER), &identity()).unwrap();
        let staging = temp.path().join("staging");
        fs::create_dir(&staging).unwrap();
        let candidate = staging.join("app");
        fs::create_dir(&candidate).unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut calls = 0;
            let _ = promote_with(&candidate, &old, true, "linux", &mut |a, b| {
                calls += 1;
                if calls == 2 {
                    panic!("simulated interrupt");
                }
                fs::rename(a, b)
            });
        }));
        assert!(result.is_err());
        fs::remove_dir_all(staging).unwrap();
        let backups: Vec<_> = fs::read_dir(temp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".inklow-previous-")
            })
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(
            read_identity(&backups[0].path(), "linux").unwrap(),
            identity()
        );
    }
    #[test]
    fn version_limits_and_marker_unknown_fields() {
        for value in ["1.2", "01.2.3", "1.2.3-beta", "-1.2.3"] {
            assert!(validate_version(value).is_err());
        }
        assert_eq!(
            validate_version("255.255.65535").unwrap(),
            [255, 255, 65535]
        );
        let mut value = serde_json::to_value(identity()).unwrap();
        value["foreign"] = serde_json::json!(true);
        assert!(serde_json::from_value::<Identity>(value).is_err());
    }
    #[test]
    fn source_digest_identifies_dirty_bytes_but_not_ignored_build_cache() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        run(Command::new("git").arg("-C").arg(root).args(["init", "-q"])).unwrap();
        fs::write(root.join(".gitignore"), "target/\n").unwrap();
        fs::write(root.join("source.rs"), "fn source() {}\n").unwrap();
        run(Command::new("git").arg("-C").arg(root).args(["add", "."])).unwrap();
        run(Command::new("git")
            .arg("-C")
            .arg(root)
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@localhost",
                "-c",
                "commit.gpgsign=false",
                "-c",
            ])
            .arg(format!(
                "core.hooksPath={}",
                root.join(".git/no-hooks").display()
            ))
            .args(["commit", "-q", "-m", "synthetic source"])
            .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z"))
        .unwrap();
        let clean = source_state(root).unwrap();
        assert!(!clean.dirty);
        fs::create_dir(root.join("target")).unwrap();
        fs::write(root.join("target/cache"), b"compiled").unwrap();
        assert_eq!(clean, source_state(root).unwrap());
        fs::write(root.join("source.rs"), "fn different() {}\n").unwrap();
        let dirty = source_state(root).unwrap();
        assert!(dirty.dirty);
        assert_eq!(dirty.base_commit, clean.base_commit);
        assert_ne!(dirty.source_digest, clean.source_digest);
    }
}
