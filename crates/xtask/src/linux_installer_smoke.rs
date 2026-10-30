//! Destructive package QA is restricted to an explicitly opted-in disposable hosted CI root.
//! The observer never asks an application to expose a test-only Quit path.
use anyhow::Result;
use std::path::Path;

pub fn run(bundle: &Path, assets: &Path, output: &Path) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        native::run(bundle, assets, output)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (bundle, assets, output);
        anyhow::bail!("Linux installer smoke requires native Linux");
    }
}

#[cfg(target_os = "linux")]
mod native {
    use crate::common::{self, APP_ID};
    use anyhow::{Context, Result, bail, ensure};
    use serde_json::{Value, json};
    use std::{
        collections::{BTreeMap, BTreeSet},
        fs::{self, File},
        io::Read,
        os::unix::process::CommandExt,
        path::{Component, Path, PathBuf},
        process::{Child, Command, ExitStatus, Stdio},
        thread,
        time::{Duration, Instant},
    };
    use zbus::{
        blocking::{Connection, Proxy, connection::Builder},
        zvariant::OwnedObjectPath,
    };

    const GUARD: &str = "BROTYPIST_INSTALLER_SMOKE_ALLOW_SYSTEM_PACKAGE";
    const ROOT: &str = "/opt/brotypist";
    const LAUNCHER: &str = "/usr/bin/brotypist";
    const LIMIT: usize = 256 * 1024;
    const WAIT: Duration = Duration::from_millis(100);
    type Accessible = (String, OwnedObjectPath);

    pub fn run(bundle: &Path, assets: &Path, output: &Path) -> Result<()> {
        fs::create_dir_all(output)?;
        common::real_dir(output)?;
        let output = output.canonicalize()?;
        let receipt_path = output.join("installer-smoke-linux.json");
        ensure!(
            fs::symlink_metadata(&receipt_path)
                .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
            "refusing existing installer receipt"
        );
        let mut receipt = json!({"schema":1,"platform":"linux","runtime":"rust","passed":false,
            "scope":"actual DEB install, same-version replacement, purge and relocated portable CLI; owned X11/AT-SPI visible Quit",
            "interactive_keyboard_compatibility":"NOT TESTED; installer smoke is not physical typing or external-host proof",
            "required_gates":{"previous_release_upgrade":"not_exercised","full_installer_matrix_passed":false},
            "assertions":[],"diagnostics":[],"real_worker":"not requested"});
        let work = common::stage(&output, "linux-installer-smoke")?;
        let mut attempted = false;
        let result = exercise(bundle, assets, &work, &mut attempted, &mut receipt);
        // An unsuccessful unpack can leave dpkg state behind, so cleanup follows the attempt.
        let cleanup = if attempted {
            checked(
                Command::new("dpkg").args(["--purge", "brotypist"]),
                &work,
                "cleanup-purge",
                120,
                &mut receipt,
            )
            .and_then(|_| removed())
        } else {
            Ok(())
        };
        receipt["cleanup"] = json!({"package_purge_attempted":attempted,"passed":cleanup.is_ok()});
        if let Err(error) = &result {
            receipt["failure"] = json!(format!("{error:#}"));
        }
        if let Err(error) = &cleanup {
            receipt["cleanup_failure"] = json!(format!("{error:#}"));
        }
        // Private command output is never retained verbatim. Only bounded digests/categories survive.
        let remove = fs::remove_dir_all(&work);
        if remove.is_err() {
            receipt["temporary_cleanup_failure"] =
                json!("could not remove owned temporary directory");
        }
        receipt["passed"] = json!(result.is_ok() && cleanup.is_ok() && remove.is_ok());
        common::json_new(&receipt_path, &receipt)?;
        result?;
        cleanup?;
        remove?;
        Ok(())
    }

    fn guard() -> Result<()> {
        ensure!(
            std::env::consts::ARCH == "x86_64",
            "native Linux amd64 required"
        );
        ensure!(
            std::env::var(GUARD).as_deref() == Ok("isolated-ci-root")
                && std::env::var("CI").as_deref() == Ok("true")
                && std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true")
                && std::env::var("RUNNER_ENVIRONMENT").as_deref() == Ok("github-hosted"),
            "system package mutation requires explicit isolated hosted-CI root opt-in"
        );
        let status = fs::read_to_string("/proc/self/status")?;
        ensure!(
            status
                .lines()
                .find(|v| v.starts_with("Uid:"))
                .is_some_and(|v| v.split_whitespace().skip(1).all(|n| n == "0")),
            "installer smoke runs only as root inside the disposable runner; sudo is never used"
        );
        ensure!(
            Path::new("/var/lib/dpkg/status").is_file(),
            "Debian dpkg host required"
        );
        Ok(())
    }

    fn exercise(
        bundle: &Path,
        assets: &Path,
        work: &Path,
        attempted: &mut bool,
        receipt: &mut Value,
    ) -> Result<()> {
        guard()?;
        common::real_dir(bundle)?;
        common::real_dir(assets)?;
        let bundle = bundle.canonicalize()?;
        let identity = common::read_identity(&bundle, "linux")?;
        let package_receipt = assets.join("package-receipt-linux-x86_64.json");
        common::regular(&package_receipt)?;
        let package: Value = serde_json::from_slice(&fs::read(package_receipt)?)?;
        ensure!(
            package["application"] == APP_ID
                && package["platform"] == "linux"
                && package["architecture"] == "x86_64"
                && package["runtime"] == "rust"
                && package["release_eligible"] == true
                && package["native_provenance"]["release_eligible"] == true
                && package["native_provenance"]["source"]["dirty"] == false
                && package["version"] == identity.version
                && package["source_commit"] == identity.source_commit
                && package["files"] == serde_json::to_value(common::inventory(&bundle)?)?,
            "canonical release-eligible package/bundle identity mismatch"
        );
        let stem = format!("BroTypist-{}-linux-amd64", identity.version);
        let deb = artifact(assets, &package, "deb", &format!("{stem}.deb"))?;
        let portable = artifact(assets, &package, "tar.gz", &format!("{stem}.tar.gz"))?;
        receipt["version"] = json!(identity.version);
        receipt["source_commit"] = json!(identity.source_commit);
        receipt["artifacts"] = package["artifacts"].clone();
        ensure!(
            checked(
                Command::new("dpkg").arg("--print-architecture"),
                work,
                "architecture",
                10,
                receipt
            )? == "amd64",
            "dpkg architecture mismatch"
        );
        let (status, installed) = observed(
            Command::new("dpkg-query").args(["-W", "-f=${db:Status-Status}", "brotypist"]),
            work,
            "preexisting-package",
            10,
            receipt,
        )?;
        ensure!(
            !status.success() || installed == "not-installed",
            "refusing any existing BroTypist dpkg state"
        );
        removed()?;
        let legacy = Command::new("dpkg-query")
            .args(["-W", "-f=${db:Status-Status}", "inklow"])
            .output()?;
        ensure!(
            !legacy.status.success()
                || String::from_utf8_lossy(&legacy.stdout).trim() == "not-installed",
            "refusing existing legacy Inklow package"
        );
        let metadata = checked(
            Command::new("dpkg-deb").args(["--field"]).arg(&deb).args([
                "Package",
                "Version",
                "Architecture",
            ]),
            work,
            "deb-metadata",
            30,
            receipt,
        )?;
        let fields: BTreeMap<_, _> = metadata
            .lines()
            .filter_map(|line| line.split_once(": "))
            .collect();
        ensure!(
            fields.len() == 3
                && fields.get("Package") == Some(&"brotypist")
                && fields.get("Version") == Some(&identity.version.as_str())
                && fields.get("Architecture") == Some(&"amd64"),
            "Debian metadata differs from canonical identity"
        );
        let payload_tar = work.join("deb-payload.tar");
        export_tar(&deb, "--fsys-tarfile", &payload_tar, work, receipt)?;
        let paths = validate_tar(File::open(&payload_tar)?, false)?;
        let control_tar = work.join("deb-control.tar");
        export_tar(&deb, "--ctrl-tarfile", &control_tar, work, receipt)?;
        validate_control(File::open(control_tar)?)?;
        for path in &paths {
            ensure!(
                fs::symlink_metadata(Path::new("/").join(path))
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
                "refusing preexisting package-owned file"
            );
        }
        let home = work.join("private-home");
        let state = seed(&home)?;
        let mut session = Session::start(work, &home, receipt)?;
        let transaction = (|| -> Result<()> {
            for phase in ["install", "same-version-upgrade"] {
                *attempted = true;
                checked(
                    Command::new("dpkg").arg("--unpack").arg(&deb),
                    work,
                    &format!("{phase}-unpack"),
                    180,
                    receipt,
                )?;
                checked(
                    Command::new("dpkg").args(["--configure", "brotypist"]),
                    work,
                    &format!("{phase}-configure"),
                    180,
                    receipt,
                )?;
                preserved(&state)?;
                validate_installed(&bundle, &paths, &session, work, receipt)?;
                cli(
                    Path::new(LAUNCHER),
                    &identity.version,
                    &session,
                    work,
                    phase,
                    receipt,
                )?;
                let gui = gui(Path::new(LAUNCHER), &session, work, phase, receipt)?;
                receipt[phase] = gui;
                preserved(&state)?;
                ensure!(
                    !session.home.join(".config/autostart").try_exists()?
                        && !session.home.join(".config/kwinrc").try_exists()?,
                    "native GUI launch changed autostart or explicit KDE selection"
                );
                assertion(
                    receipt,
                    &format!(
                        "{phase}: actual configured dpkg payload, owned desktop integration and visible native Quit"
                    ),
                );
            }
            worker(&session, work, receipt)?;
            checked(
                Command::new("dpkg").args(["--purge", "brotypist"]),
                work,
                "uninstall-purge",
                120,
                receipt,
            )?;
            removed()?;
            for path in &paths {
                ensure!(
                    !Path::new("/").join(path).try_exists()?,
                    "purge left an owned payload file"
                );
            }
            preserved(&state)?;
            assertion(
                receipt,
                "purge removes every package-owned file and preserves exact external settings/history/model hashes",
            );
            receipt["external_state_sha256"] = json!(
                state
                    .iter()
                    .map(|(p, h)| (
                        p.strip_prefix(&home)
                            .unwrap()
                            .to_string_lossy()
                            .into_owned(),
                        h
                    ))
                    .collect::<BTreeMap<_, _>>()
            );
            let extracted = work.join("portable-original");
            fs::create_dir(&extracted)?;
            // Validate the entire stream before extraction: no symlinks, hardlinks, devices or traversal.
            validate_tar(flate2::read::GzDecoder::new(File::open(&portable)?), true)?;
            let mut archive =
                tar::Archive::new(flate2::read::GzDecoder::new(File::open(&portable)?));
            archive.unpack(&extracted)?;
            let relocated = work.join("relocated portable with spaces");
            fs::rename(&extracted, &relocated)?;
            let app = relocated.join("BroTypist");
            ensure!(
                common::inventory(&app)? == expected_payload(&bundle)?,
                "portable payload differs after relocation"
            );
            cli(
                &app.join("launch-brotypist"),
                &identity.version,
                &session,
                work,
                "portable-relocated",
                receipt,
            )?;
            preserved(&state)?;
            assertion(
                receipt,
                "portable archive safely extracted and relocated; exact CLI version and native doctor passed",
            );
            Ok(())
        })();
        let stopped = session.stop();
        receipt["private_session_cleanup"] = json!(stopped.is_ok());
        transaction?;
        stopped?;
        Ok(())
    }

    fn artifact(assets: &Path, receipt: &Value, kind: &str, name: &str) -> Result<PathBuf> {
        let path = assets.join(name);
        common::regular(&path)?;
        let row = &receipt["artifacts"][kind];
        ensure!(
            row["path"]
                .as_str()
                .and_then(|p| Path::new(p).file_name())
                .is_some_and(|p| p == name)
                && row["sha256"] == common::hash(&path)?
                && row["size"].as_u64() == Some(fs::metadata(&path)?.len()),
            "artifact receipt digest/size/name mismatch"
        );
        Ok(path.canonicalize()?)
    }
    fn removed() -> Result<()> {
        for path in [
            ROOT.to_owned(),
            LAUNCHER.to_owned(),
            format!("/usr/share/applications/{APP_ID}.desktop"),
            format!("/usr/share/applications/{APP_ID}.InputMethod.desktop"),
            format!("/usr/share/icons/hicolor/256x256/apps/{APP_ID}.png"),
        ] {
            ensure!(
                fs::symlink_metadata(path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
                "application/install integration path exists"
            );
        }
        Ok(())
    }
    fn assertion(receipt: &mut Value, name: &str) {
        receipt["assertions"]
            .as_array_mut()
            .unwrap()
            .push(json!(name));
    }

    fn normalized(path: &Path) -> Result<PathBuf> {
        let mut result = PathBuf::new();
        for c in path.components() {
            match c {
                Component::CurDir => (),
                Component::Normal(s) => result.push(s),
                _ => bail!("archive traversal/absolute path refused"),
            }
        }
        ensure!(
            result.as_os_str().is_empty() || common::safe_relative(&result),
            "unsafe archive filename"
        );
        Ok(result)
    }
    fn allowed(path: &Path, directory: bool, portable: bool) -> bool {
        if portable {
            return path.starts_with("BroTypist") || directory && path.as_os_str().is_empty();
        }
        let integrations = [
            format!("usr/share/applications/{APP_ID}.desktop"),
            format!("usr/share/applications/{APP_ID}.InputMethod.desktop"),
        ];
        let roots = [
            Path::new("opt/brotypist"),
            Path::new("usr/share/icons/hicolor"),
        ];
        roots
            .iter()
            .any(|root| path.starts_with(root) || directory && root.starts_with(path))
            || integrations.iter().any(|name| {
                Path::new(name) == path || directory && Path::new(name).starts_with(path)
            })
            || path == Path::new("usr/bin/brotypist")
            || directory && Path::new("usr/bin/brotypist").starts_with(path)
    }
    fn validate_tar(reader: impl Read, portable: bool) -> Result<BTreeSet<PathBuf>> {
        let mut archive = tar::Archive::new(reader);
        let mut files = BTreeSet::new();
        let mut all = BTreeSet::new();
        let mut bytes = 0u64;
        for entry in archive.entries()? {
            let entry = entry?;
            let path = normalized(&entry.path()?)?;
            let kind = entry.header().entry_type();
            ensure!(
                kind.is_file() || kind.is_dir(),
                "archive link/special entry refused"
            );
            ensure!(
                allowed(&path, kind.is_dir(), portable) && all.insert(path.clone()),
                "unexpected/duplicate archive path"
            );
            bytes = bytes
                .checked_add(entry.size())
                .context("archive size overflow")?;
            ensure!(
                all.len() <= 100_000 && bytes <= 8 * 1024 * 1024 * 1024,
                "archive exceeds bounded payload limits"
            );
            ensure!(
                entry.header().mode()? & 0o7000 == 0,
                "setuid/setgid/sticky payload refused"
            );
            if kind.is_file() {
                files.insert(path);
            }
        }
        ensure!(!files.is_empty(), "empty payload archive");
        Ok(files)
    }
    fn validate_control(reader: impl Read) -> Result<()> {
        let mut archive = tar::Archive::new(reader);
        let mut control = false;
        for entry in archive.entries()? {
            let entry = entry?;
            let path = normalized(&entry.path()?)?;
            ensure!(
                entry.header().entry_type().is_dir() && path.as_os_str().is_empty()
                    || entry.header().entry_type().is_file()
                        && path == Path::new("control")
                        && !control
                        && entry.size() < LIMIT as u64,
                "unexpected control archive entry or maintainer script refused"
            );
            if path == Path::new("control") {
                control = true;
            }
        }
        ensure!(control, "missing package control metadata");
        Ok(())
    }
    fn export_tar(
        deb: &Path,
        option: &str,
        destination: &Path,
        work: &Path,
        receipt: &mut Value,
    ) -> Result<()> {
        let file = File::options()
            .create_new(true)
            .write(true)
            .open(destination)?;
        let mut command = Command::new("dpkg-deb");
        command.arg(option).arg(deb).stdout(Stdio::from(file));
        let error = work.join(format!(
            "{}.stderr",
            destination.file_name().unwrap().to_string_lossy()
        ));
        let mut process = Owned::spawn(command.stderr(Stdio::from(
            File::options().create_new(true).write(true).open(&error)?,
        )))?;
        let status = process.wait(Duration::from_secs(120))?;
        diagnostic(receipt, option, &error, status)?;
        ensure!(status.success(), "dpkg-deb payload export failed");
        Ok(())
    }
    fn seed(home: &Path) -> Result<BTreeMap<PathBuf, String>> {
        fs::create_dir(home)?;
        let profile = home.join(".config/inklow");
        fs::create_dir_all(&profile)?;
        let model = profile.join("models/preserve.gguf");
        fs::create_dir_all(model.parent().unwrap())?;
        let mut files = BTreeMap::new();
        let settings = json!({"global_enabled":false,"enabled":false,"setup_completed":true,"record_history":false,"record_unaccepted":false,
            "statistics_enabled":false,"check_updates_automatically":false,"launch_at_login":false,"screen_context":false,"clipboard_context":false,
            "screenshot_context":false,"model_id":"","model_path":model,"acceleration":"cpu"});
        for (path, content) in [
            (
                profile.join("settings.json"),
                serde_json::to_vec_pretty(&settings)?,
            ),
            (
                profile.join("history.enc"),
                b"synthetic external encrypted history; not user data\n".to_vec(),
            ),
            (
                model,
                b"synthetic external model sentinel; not model weights\n".to_vec(),
            ),
        ] {
            common::write_new(&path, &content)?;
            files.insert(path.clone(), common::hash(&path)?);
        }
        Ok(files)
    }
    fn preserved(state: &BTreeMap<PathBuf, String>) -> Result<()> {
        for (path, digest) in state {
            ensure!(
                &common::hash(path)? == digest,
                "external synthetic state changed"
            );
        }
        Ok(())
    }
    fn expected_payload(bundle: &Path) -> Result<Vec<common::Row>> {
        // Canonical package::copy_linux removes the build-destination desktop entries.
        let excluded = [
            "BroTypist.desktop".to_owned(),
            format!("share/applications/{APP_ID}.desktop"),
            format!("share/applications/{APP_ID}.InputMethod.desktop"),
        ];
        Ok(common::inventory(bundle)?
            .into_iter()
            .filter(|row| !excluded.contains(&row.path))
            .collect())
    }
    fn validate_installed(
        bundle: &Path,
        paths: &BTreeSet<PathBuf>,
        session: &Session,
        work: &Path,
        receipt: &mut Value,
    ) -> Result<()> {
        ensure!(
            common::inventory(Path::new(ROOT))? == expected_payload(bundle)?,
            "installed payload differs from canonical bundle"
        );
        let list = checked(
            Command::new("dpkg-query").args(["-L", "brotypist"]),
            work,
            "package-ownership",
            15,
            receipt,
        )?;
        let listed: BTreeSet<_> = list
            .lines()
            .filter_map(|s| s.strip_prefix('/'))
            .map(PathBuf::from)
            .collect();
        for path in paths {
            ensure!(
                listed.contains(path),
                "installed file is not owned by dpkg package"
            );
        }
        for name in [
            format!("{APP_ID}.desktop"),
            format!("{APP_ID}.InputMethod.desktop"),
        ] {
            let path = Path::new("/usr/share/applications").join(name);
            checked(
                session.command("desktop-file-validate").arg(&path),
                work,
                "desktop-validate",
                10,
                receipt,
            )?;
        }
        let desktop = fs::read_to_string(format!("/usr/share/applications/{APP_ID}.desktop"))?;
        ensure!(
            desktop.lines().any(|l| l == "Exec=/usr/bin/brotypist")
                && desktop.lines().any(|l| l == format!("Icon={APP_ID}")),
            "installed desktop launcher/icon differs"
        );
        let method = fs::read_to_string(format!(
            "/usr/share/applications/{APP_ID}.InputMethod.desktop"
        ))?;
        for line in [
            "Exec=/usr/bin/brotypist --wayland-input-method --background",
            "NoDisplay=true",
            "X-KDE-Wayland-VirtualKeyboard=true",
            "X-Inklow-Owned=true",
        ] {
            ensure!(
                method.lines().any(|l| l == line),
                "KDE input method must require explicit owned selection"
            );
        }
        ensure!(
            !method.contains("Autostart")
                && !method.contains("X-GNOME-AutoRestart")
                && !paths
                    .iter()
                    .any(|p| p.components().any(|c| c.as_os_str() == "autostart")),
            "package must not install autostart or select KDE automatically"
        );
        ensure!(
            !session.home.join(".config/autostart").try_exists()?
                && !session.home.join(".config/kwinrc").try_exists()?,
            "installed launch modified autostart/KDE selection"
        );
        Ok(())
    }

    fn cli(
        exe: &Path,
        version: &str,
        session: &Session,
        work: &Path,
        label: &str,
        receipt: &mut Value,
    ) -> Result<()> {
        let observed = checked(
            session.command(exe).arg("--version"),
            work,
            &format!("{label}-version"),
            30,
            receipt,
        )?;
        ensure!(
            observed == format!("BroTypist {version}"),
            "exact installed/portable CLI version mismatch"
        );
        let doctor: Value = serde_json::from_str(&checked(
            session.command(exe).arg("doctor"),
            work,
            &format!("{label}-doctor"),
            30,
            receipt,
        )?)?;
        ensure!(
            doctor["platform"] == "linux"
                && doctor["runtime"] == "Rust"
                && doctor["version"] == version
                && doctor["settings"]
                    == session
                        .home
                        .join(".config/inklow/settings.json")
                        .to_string_lossy()
                        .as_ref()
                && doctor["native"]
                    .as_str()
                    .is_some_and(|s| s.starts_with("Linux AT-SPI/X11 ready.")),
            "native doctor identity/readiness/isolation mismatch"
        );
        receipt[format!("{label}-cli")] =
            json!({"exact_version":observed,"doctor_native_ready":true,"private_profile":true});
        Ok(())
    }

    struct Owned {
        child: Child,
        reaped: bool,
        members: BTreeMap<u32, String>,
    }
    impl Owned {
        fn spawn(command: &mut Command) -> Result<Self> {
            command.process_group(0);
            let mut owned = Self {
                child: command
                    .spawn()
                    .context("could not launch owned smoke process")?,
                reaped: false,
                members: BTreeMap::new(),
            };
            owned.observe()?;
            Ok(owned)
        }
        fn observe(&mut self) -> Result<()> {
            if !self.reaped {
                let root = self.child.id();
                if let Some(token) = process_token(root)? {
                    if self
                        .members
                        .get(&root)
                        .is_none_or(|previous| previous == &token)
                    {
                        self.members.entry(root).or_insert(token);
                        for pid in descendants(root)? {
                            if let Some(token) = process_token(pid)? {
                                self.members.entry(pid).or_insert(token);
                            }
                        }
                    }
                }
            }
            Ok(())
        }
        fn wait(&mut self, timeout: Duration) -> Result<ExitStatus> {
            let until = Instant::now() + timeout;
            loop {
                self.observe()?;
                if let Some(status) = self.child.try_wait()? {
                    self.reaped = true;
                    return Ok(status);
                }
                ensure!(
                    Instant::now() < until,
                    "owned smoke process exceeded deadline"
                );
                thread::sleep(WAIT);
            }
        }
        fn stop(&mut self) -> Result<()> {
            self.observe()?;
            let mut failed = false;
            // Compare Linux start-time identities before signaling any individually observed child.
            for (&pid, token) in &self.members {
                if process_token(pid)?.as_ref() == Some(token) {
                    let killed = Command::new("/bin/kill")
                        .args(["-KILL", "--"])
                        .arg(pid.to_string())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status()?
                        .success();
                    failed |= !killed && process_token(pid)?.as_ref() == Some(token);
                }
            }
            if !self.reaped {
                self.child.wait()?;
                self.reaped = true;
            }
            ensure!(!failed, "owned process cleanup failed");
            Ok(())
        }
    }
    impl Drop for Owned {
        fn drop(&mut self) {
            let _ = self.stop();
        }
    }

    struct Session {
        bus: Owned,
        x: Owned,
        address: String,
        display: String,
        home: PathBuf,
        runtime: PathBuf,
    }
    impl Session {
        fn start(work: &Path, home: &Path, receipt: &mut Value) -> Result<Self> {
            let runtime = work.join("xdg-runtime");
            fs::create_dir(&runtime)?;
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700))?;
            let address_file = work.join("session-address");
            let display_file = work.join("display-number");
            let mut bus_command = Command::new("dbus-daemon");
            environment(&mut bus_command, home, &runtime);
            let bus = Owned::spawn(
                bus_command
                    .args(["--session", "--nofork", "--print-address=1"])
                    .stdout(Stdio::from(
                        File::options()
                            .create_new(true)
                            .write(true)
                            .open(&address_file)?,
                    ))
                    .stderr(Stdio::null()),
            )?;
            let address = wait_line(&address_file)?;
            let mut x_command = Command::new("Xvfb");
            let x = Owned::spawn(
                x_command
                    .args([
                        "-displayfd",
                        "1",
                        "-screen",
                        "0",
                        "1280x800x24",
                        "-nolisten",
                        "tcp",
                        "-ac",
                    ])
                    .stdout(Stdio::from(
                        File::options()
                            .create_new(true)
                            .write(true)
                            .open(&display_file)?,
                    ))
                    .stderr(Stdio::null()),
            )?;
            let number = wait_line(&display_file)?;
            ensure!(
                number.bytes().all(|b| b.is_ascii_digit()) && !number.is_empty(),
                "Xvfb returned invalid display number"
            );
            let session = Self {
                bus,
                x,
                address,
                display: format!(":{number}"),
                home: home.to_owned(),
                runtime,
            };
            let connection = Builder::address(session.address.as_str())?
                .method_timeout(Duration::from_secs(3))
                .build()?;
            let daemon = Proxy::new(
                &connection,
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
            )?;
            let activation = BTreeMap::from([
                ("DISPLAY", session.display.as_str()),
                ("XDG_SESSION_TYPE", "x11"),
                ("GDK_BACKEND", "x11"),
            ]);
            daemon.call::<_, _, ()>("UpdateActivationEnvironment", &(activation))?;
            let status = Proxy::new(
                &connection,
                "org.a11y.Bus",
                "/org/a11y/status",
                "org.a11y.Status",
            )?;
            status.set_property("IsEnabled", true)?;
            status.set_property("ScreenReaderEnabled", true)?;
            checked(
                session.command("xwininfo").args(["-root"]),
                work,
                "private-x11-ready",
                15,
                receipt,
            )?;
            Ok(session)
        }
        fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
            let mut command = Command::new(program);
            environment(&mut command, &self.home, &self.runtime);
            command
                .env("DBUS_SESSION_BUS_ADDRESS", &self.address)
                .env("DISPLAY", &self.display);
            command
        }
        fn stop(&mut self) -> Result<()> {
            let x = self.x.stop();
            let bus = self.bus.stop();
            x?;
            bus
        }
    }
    fn environment(command: &mut Command, home: &Path, runtime: &Path) {
        command
            .env_clear()
            .env(
                "PATH",
                "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            )
            .env("HOME", home)
            .env("INKLOW_HOME", home.join(".config/inklow"))
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("XDG_RUNTIME_DIR", runtime)
            .env("XDG_SESSION_TYPE", "x11")
            .env("GDK_BACKEND", "x11")
            .env("NO_AT_BRIDGE", "0")
            .env("GTK_MODULES", "atk-bridge")
            .env("LC_ALL", "C.UTF-8")
            .env("DEBIAN_FRONTEND", "noninteractive");
    }
    fn wait_line(path: &Path) -> Result<String> {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let value = fs::read_to_string(path)?;
            ensure!(value.len() <= 4096, "session startup output exceeded bound");
            if let Some(line) = value.lines().next().filter(|line| !line.is_empty()) {
                return Ok(line.to_owned());
            }
            ensure!(
                Instant::now() < deadline,
                "owned desktop session did not start"
            );
            thread::sleep(WAIT);
        }
    }

    fn gui(
        exe: &Path,
        session: &Session,
        work: &Path,
        label: &str,
        receipt: &mut Value,
    ) -> Result<Value> {
        let out = work.join(format!("{label}-gui.stdout"));
        let err = work.join(format!("{label}-gui.stderr"));
        let mut command = session.command(exe);
        let mut app = Owned::spawn(
            command
                .args(["--settings", "--paused", "--no-tray", "--pane", "general"])
                .stdout(Stdio::from(
                    File::options().create_new(true).write(true).open(&out)?,
                ))
                .stderr(Stdio::from(
                    File::options().create_new(true).write(true).open(&err)?,
                )),
        )?;
        let pid = app.child.id();
        let deadline = Instant::now() + Duration::from_secs(30);
        let result = (|| -> Result<Value> {
            let connection = Builder::address(session.address.as_str())?
                .method_timeout(Duration::from_secs(2))
                .build()?;
            let bus = Proxy::new(&connection, "org.a11y.Bus", "/org/a11y/bus", "org.a11y.Bus")?;
            let address: String = bus.call("GetAddress", &())?;
            let accessibility = Builder::address(address.as_str())?
                .method_timeout(Duration::from_secs(2))
                .build()?;
            let mut seen_workers = BTreeSet::new();
            let (window, button) = loop {
                app.observe()?;
                ensure!(
                    app.child.try_wait()?.is_none(),
                    "GUI exited before visible native Quit"
                );
                seen_workers.extend(descendants(pid)?);
                if let Some(window) = mapped_window(pid, session, work, receipt)? {
                    if let Some(button) = quit_button(&accessibility, pid)? {
                        break (window, button);
                    }
                }
                ensure!(
                    Instant::now() < deadline,
                    "owned mapped Settings did not expose visible AT-SPI Quit button"
                );
                thread::sleep(WAIT);
            };
            let action = Proxy::new(
                &accessibility,
                button.0.as_str(),
                button.1.as_str(),
                "org.a11y.atspi.Action",
            )?;
            let actions: Vec<(String, String, String)> = action.call("GetActions", &())?;
            let index = actions
                .iter()
                .position(|(name, _, _)| {
                    ["click", "press", "activate"].contains(&name.to_ascii_lowercase().as_str())
                })
                .context("visible Quit has no native action")?;
            let accepted: bool = action.call("DoAction", &(index as i32))?;
            ensure!(accepted, "visible native Quit action was rejected");
            let status = app.wait(Duration::from_secs(20))?;
            diagnostic(receipt, &format!("{label}-gui"), &err, status)?;
            ensure!(
                status.success(),
                "native Quit did not produce normal exit zero"
            );
            seen_workers.extend(app.members.keys().copied().filter(|child| *child != pid));
            ensure!(
                seen_workers
                    .iter()
                    .all(|pid| !Path::new("/proc").join(pid.to_string()).exists()),
                "GUI left an observed owned child/model worker alive"
            );
            Ok(
                json!({"pid":pid,"mapped_x11_window":window,"visible_settings_quit":"own-PID AT-SPI native button action",
            "quit_exit_code":0,"observed_child_pids":seen_workers,"observed_children_exited":true,
            "model_worker_note":"paused GUI does not request inference; real worker lifecycle is separately checked when supplied"}),
            )
        })();
        if result.is_err() {
            // Cleanup never converts a failed visible Quit into success.
            let cleanup = app.stop();
            receipt[format!("{label}-gui-failure-cleanup")] =
                json!({"owned_pid":pid,"passed":cleanup.is_ok()});
            if let Ok(Some(status)) = app.child.try_wait() {
                let _ = diagnostic(receipt, &format!("{label}-gui-failure"), &err, status);
            }
        }
        result
    }
    fn process_token(pid: u32) -> Result<Option<String>> {
        match fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => {
                let fields = stat
                    .rsplit_once(") ")
                    .context("malformed owned process stat")?
                    .1;
                let token = fields
                    .split_whitespace()
                    .nth(19)
                    .context("owned process has no start-time")?;
                Ok(Some(token.to_owned()))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    fn descendants(pid: u32) -> Result<BTreeSet<u32>> {
        let mut result = BTreeSet::new();
        let mut queue = vec![pid];
        while let Some(parent) = queue.pop() {
            let children = Path::new("/proc")
                .join(parent.to_string())
                .join(format!("task/{parent}/children"));
            match fs::read_to_string(children) {
                Ok(text) => {
                    for child in text.split_whitespace() {
                        let child: u32 = child.parse()?;
                        if result.insert(child) {
                            queue.push(child);
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
            }
            ensure!(result.len() < 1024, "owned process tree exceeded bound");
        }
        Ok(result)
    }
    fn mapped_window(
        pid: u32,
        session: &Session,
        work: &Path,
        receipt: &mut Value,
    ) -> Result<Option<String>> {
        let tree = checked(
            session.command("xwininfo").args(["-root", "-tree"]),
            work,
            "owned-window-tree",
            5,
            receipt,
        )?;
        for line in tree.lines().take(4096) {
            let Some(id) = line.split_whitespace().next().filter(|id| {
                id.starts_with("0x") && id[2..].bytes().all(|b| b.is_ascii_hexdigit())
            }) else {
                continue;
            };
            let (_, properties) = observed(
                session
                    .command("xprop")
                    .args(["-id", id, "_NET_WM_PID", "_NET_WM_NAME"]),
                work,
                "owned-window-properties",
                3,
                receipt,
            )?;
            if !properties.lines().any(|l| {
                l.starts_with("_NET_WM_PID(CARDINAL)")
                    && l.split('=')
                        .nth(1)
                        .is_some_and(|v| v.trim() == pid.to_string())
            }) {
                continue;
            }
            if !properties.contains("BroTypist Settings") {
                continue;
            }
            let info = checked(
                session.command("xwininfo").args(["-id", id]),
                work,
                "owned-window-map",
                3,
                receipt,
            )?;
            if info.contains("Map State: IsViewable") {
                return Ok(Some(id.to_owned()));
            }
        }
        Ok(None)
    }
    fn quit_button(connection: &Connection, pid: u32) -> Result<Option<Accessible>> {
        let root = Proxy::new(
            connection,
            "org.a11y.atspi.Registry",
            "/org/a11y/atspi/accessible/root",
            "org.a11y.atspi.Accessible",
        )?;
        let apps: Vec<Accessible> = root.call("GetChildren", &())?;
        let dbus = Proxy::new(
            connection,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )?;
        for app in apps.into_iter().take(256) {
            let owner: u32 = dbus.call("GetConnectionUnixProcessID", &(app.0.as_str()))?;
            if owner != pid {
                continue;
            }
            // Never inspect foreign application controls. The unique bus owner is checked before traversal.
            let mut queue = vec![app];
            let mut visited = BTreeSet::new();
            let mut remaining = 512;
            while let Some(object) = queue.pop() {
                ensure!(remaining > 0, "owned accessibility tree exceeded bound");
                remaining -= 1;
                if !visited.insert((object.0.clone(), object.1.to_string())) {
                    continue;
                }
                let owner: u32 = dbus.call("GetConnectionUnixProcessID", &(object.0.as_str()))?;
                ensure!(
                    owner == pid,
                    "owned accessibility tree crossed process boundary"
                );
                let proxy = Proxy::new(
                    connection,
                    object.0.as_str(),
                    object.1.as_str(),
                    "org.a11y.atspi.Accessible",
                )?;
                let name: String = proxy.get_property("Name")?;
                if name == "Quit BroTypist" {
                    let role: u32 = proxy.call("GetRole", &())?;
                    let states: Vec<u32> = proxy.call("GetState", &())?;
                    let has = |bit: usize| {
                        states
                            .get(bit / 32)
                            .is_some_and(|v| v & (1u32 << (bit % 32)) != 0)
                    };
                    // Stable AT-SPI role/state constants: BUTTON=43; ENABLED=8.
                    ensure!(
                        role == 43 && [8, 24, 25, 30].into_iter().all(has),
                        "owned Quit must be a visible/showing/enabled/sensitive native push button"
                    );
                    drop(proxy);
                    return Ok(Some(object));
                }
                let children: Vec<Accessible> = proxy.call("GetChildren", &())?;
                ensure!(
                    children.len() <= 256,
                    "owned accessible node exceeded child bound"
                );
                queue.extend(children);
            }
        }
        Ok(None)
    }
    fn worker(session: &Session, work: &Path, receipt: &mut Value) -> Result<()> {
        let Some(exe) = std::env::var_os("BROTYPIST_INSTALLER_SMOKE_WORKER_EXE") else {
            return Ok(());
        };
        let exe = PathBuf::from(exe).canonicalize()?;
        common::regular(&exe)?;
        let model = PathBuf::from(
            std::env::var_os("BROTYPIST_INSTALLER_SMOKE_MODEL_PATH")
                .context("real worker smoke requires separate verified cached model path")?,
        )
        .canonicalize()?;
        common::regular(&model)?;
        let output = work.join("worker-receipt");
        fs::create_dir(&output)?;
        let profile = work.join("worker-profile");
        fs::create_dir(&profile)?;
        checked(
            session
                .command(exe)
                .arg("--app")
                .arg("/opt/brotypist/BroTypist")
                .arg("--output")
                .arg(&output)
                .arg("--model-path")
                .arg(model)
                .args(["--preset", "tiny", "--timeout-seconds", "180"])
                .env("INKLOW_HOME", &profile),
            work,
            "real-installed-worker",
            900,
            receipt,
        )?;
        let path = output.join("worker-smoke.json");
        common::regular(&path)?;
        ensure!(
            fs::metadata(&path)?.len() <= LIMIT as u64,
            "worker receipt exceeded bound"
        );
        let proof: Value = serde_json::from_slice(&fs::read(path)?)?;
        ensure!(
            proof["passed"] == true
                && proof["tool"] == "worker_smoke"
                && proof["observations"].as_array().is_some_and(|rows| rows
                    .iter()
                    .any(|r| r["check"] == "worker_shutdown_acknowledged" && r["passed"] == true)),
            "real installed worker/shutdown receipt failed"
        );
        receipt["real_worker"] = proof;
        Ok(())
    }

    fn observed(
        command: &mut Command,
        work: &Path,
        label: &str,
        timeout: u64,
        receipt: &mut Value,
    ) -> Result<(ExitStatus, String)> {
        let id = uuid::Uuid::new_v4();
        let out = work.join(format!("{id}.stdout"));
        let err = work.join(format!("{id}.stderr"));
        let mut child = Owned::spawn(
            command
                .stdout(Stdio::from(
                    File::options().create_new(true).write(true).open(&out)?,
                ))
                .stderr(Stdio::from(
                    File::options().create_new(true).write(true).open(&err)?,
                ))
                .stdin(Stdio::null()),
        )?;
        let waited = child.wait(Duration::from_secs(timeout));
        if waited.is_err() {
            let _ = child.stop();
        }
        let status = waited?;
        diagnostic(receipt, label, &err, status)?;
        ensure!(
            fs::metadata(&out)?.len() <= LIMIT as u64,
            "smoke command stdout exceeded bound"
        );
        let stdout = fs::read_to_string(&out).context("smoke command stdout was not UTF-8")?;
        fs::remove_file(out)?;
        fs::remove_file(err)?;
        Ok((status, stdout.trim().to_owned()))
    }
    fn checked(
        command: &mut Command,
        work: &Path,
        label: &str,
        timeout: u64,
        receipt: &mut Value,
    ) -> Result<String> {
        let (status, out) = observed(command, work, label, timeout, receipt)?;
        ensure!(
            status.success(),
            "{label}: command failed (raw output withheld; bounded digest in receipt)"
        );
        Ok(out)
    }
    fn diagnostic(receipt: &mut Value, label: &str, path: &Path, status: ExitStatus) -> Result<()> {
        use sha2::{Digest, Sha256};
        let mut bytes = Vec::new();
        File::open(path)?
            .take(LIMIT as u64)
            .read_to_end(&mut bytes)?;
        let total = fs::metadata(path)?.len();
        let rows = receipt["diagnostics"].as_array_mut().unwrap();
        if rows.len() < 512 {
            rows.push(json!({"stage":label,"exit_code":status.code(),"stderr_bytes":total,
            "stderr_prefix_sha256":format!("{:x}",Sha256::digest(&bytes)),"stderr_truncated":total>LIMIT as u64,"redaction":"raw process output withheld"}));
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn paths_refuse_traversal_and_absolute_targets() {
            for path in [
                "../escape",
                "/etc/passwd",
                "BroTypist/../../escape",
                "BroTypist/link:stream",
            ] {
                assert!(normalized(Path::new(path)).is_err());
            }
            assert!(normalized(Path::new("./BroTypist/lib/a.so")).is_ok());
        }
        #[test]
        fn installation_scope_excludes_autostart_and_foreign_files() {
            assert!(!allowed(
                Path::new("etc/xdg/autostart/a.desktop"),
                false,
                false
            ));
            assert!(!allowed(Path::new("usr/bin/other-app"), false, false));
            assert!(allowed(Path::new("opt/brotypist/BroTypist"), false, false));
            assert!(allowed(
                Path::new("usr/share/applications/com.inklow.Inklow.InputMethod.desktop"),
                false,
                false
            ));
            assert!(!allowed(Path::new("BroTypist-escape/binary"), false, true));
        }
        #[test]
        fn guard_never_accepts_normal_host_without_opt_in() {
            // No environment mutation: a normal local invocation must remain non-destructive.
            if std::env::var(GUARD).as_deref() != Ok("isolated-ci-root") {
                assert!(guard().is_err());
            }
        }
        fn archive_with(path: &str, kind: tar::EntryType) -> Vec<u8> {
            let mut archive = tar::Builder::new(Vec::new());
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o755);
            header.set_size(0);
            header.set_entry_type(kind);
            if kind.is_symlink() || kind.is_hard_link() {
                header.set_link_name("/etc/passwd").unwrap();
            }
            header.set_cksum();
            archive
                .append_data(&mut header, path, std::io::empty())
                .unwrap();
            archive.into_inner().unwrap()
        }
        #[test]
        fn portable_archive_refuses_links_and_devices_before_extraction() {
            for kind in [
                tar::EntryType::Symlink,
                tar::EntryType::Link,
                tar::EntryType::Fifo,
                tar::EntryType::Char,
            ] {
                assert!(
                    validate_tar(archive_with("BroTypist/escape", kind).as_slice(), true).is_err()
                );
            }
            assert!(
                validate_tar(
                    archive_with("BroTypist/BroTypist", tar::EntryType::Regular).as_slice(),
                    true
                )
                .is_ok()
            );
        }
        #[test]
        fn control_archive_refuses_maintainer_scripts() {
            assert!(
                validate_control(archive_with("postinst", tar::EntryType::Regular).as_slice())
                    .is_err()
            );
            assert!(
                validate_control(archive_with("control", tar::EntryType::Regular).as_slice())
                    .is_ok()
            );
        }
    }
}
