//! Disposable native DMG/ZIP continuity proof. Never installs into /Applications.
use crate::{common::*, package};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use unicode_casefold::UnicodeCaseFold;
use unicode_normalization::UnicodeNormalization;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
const PIPE_LIMIT: usize = 128 * 1024;
const GUI_TIMEOUT: Duration = Duration::from_secs(30);

pub fn run(bundle: &Path, assets: &Path, output: &Path) -> Result<()> {
    ensure!(
        cfg!(target_os = "macos"),
        "macOS installer smoke requires native macOS"
    );
    fs::create_dir_all(output)?;
    real_dir(output)?;
    let output = output.canonicalize()?;
    let receipt_path = output.join("macos-installer-smoke.json");
    ensure!(
        fs::symlink_metadata(&receipt_path)
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound),
        "refusing existing installer receipt"
    );
    // Keep failed payloads and profiles; success removes only this exclusive stage.
    let work = stage(&output, "macos-installer-smoke")?;
    let mut receipt = json!({"schema":1,"tool":"macos-installer-smoke","passed":false,"checks":{},
        "scope":"actual packaged DMG/ZIP payload, disposable copy, LaunchServices background lifetime and OS normal Quit; not physical menu/keyboard or typing proof",
        "normal_profile":"not accessed; unrelated preservation fixture only","notarization":"not assessed",
        "tcc_and_update_identity":"not assessed; ad-hoc signatures do not prove stable TCC/update identity",
        "full_matrix":false,"prior_version_upgrade":{"status":"not_exercised","reason":"no genuine prior-version native fixture supplied"},
        "release_acceptance":"not asserted; passed covers only the explicitly observed core payload checks",
        "registration":"no explicit registration, login item, permission request, quarantine removal or installed-app replacement; LaunchServices performs its normal launch bookkeeping"});
    let mount = work.join("mount");
    let mut attached = false;
    let result = exercise(bundle, assets, &work, &mount, &mut attached, &mut receipt);
    // Even attach can fail after mounting. Detach only the exclusively owned mountpoint.
    let detach_required = attached || mounted(&mount);
    let detach = if detach_required {
        capture(
            Command::new("/usr/bin/hdiutil").arg("detach").arg(&mount),
            COMMAND_TIMEOUT,
        )
        .map(|_| ())
    } else {
        Ok(())
    };
    receipt["checks"]["dmg_detached"] = if detach_required {
        json!({"passed":detach.is_ok() && !mounted(&mount),"owned_mountpoint_only":true})
    } else {
        json!({"status":"not_exercised","reason":"no owned image was attached"})
    };
    if let Err(error) = &detach {
        receipt["detach_failure"] = json!(redact(&format!("{error:#}")));
    }
    let result = result.and(detach).and_then(|()| {
        ensure!(
            !mounted(&mount),
            "owned disk image remained mounted after detach"
        );
        Ok(())
    });
    if result.is_ok() {
        if let Err(error) = fs::remove_dir_all(&work) {
            receipt["failure"] =
                json!(redact(&format!("owned disposable cleanup failed: {error}")));
            receipt["preserved_disposable_directory"] = json!(work);
            json_new(&receipt_path, &receipt)?;
            return Err(error.into());
        }
        receipt["passed"] = json!(true);
    } else {
        receipt["failure"] = json!(redact(&format!("{:#}", result.as_ref().unwrap_err())));
        receipt["preserved_disposable_directory"] = json!(work);
    }
    json_new(&receipt_path, &receipt)?;
    result
}

fn mounted(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(child), Some(parent)) = (fs::metadata(path), path.parent()) {
            return fs::metadata(parent).is_ok_and(|parent| child.dev() != parent.dev());
        }
    }
    false
}

fn exercise(
    bundle: &Path,
    assets: &Path,
    work: &Path,
    mount: &Path,
    attached: &mut bool,
    receipt: &mut Value,
) -> Result<()> {
    real_dir(bundle)?;
    real_dir(assets)?;
    let bundle = bundle.canonicalize()?;
    let assets = assets.canonicalize()?;
    ensure!(
        !work.starts_with(&bundle) && !bundle.starts_with(work),
        "probe output overlaps source payload"
    );
    let identity = read_identity(&bundle, "darwin")?;
    ensure!(
        identity.architecture == arch(),
        "installer smoke must run on the packaged native architecture"
    );
    let receipt_file = assets.join(format!("package-receipt-darwin-{}.json", arch()));
    regular(&receipt_file)?;
    let package_receipt: Value = serde_json::from_slice(&fs::read(&receipt_file)?)?;
    ensure!(
        package_receipt["application"] == APP_ID
            && package_receipt["platform"] == "darwin"
            && package_receipt["runtime"] == "rust"
            && package_receipt["release_eligible"] == true
            && package_receipt["version"] == identity.version
            && package_receipt["source_commit"] == identity.source_commit
            && package_receipt["architecture"] == identity.architecture,
        "canonical release package receipt identity differs"
    );
    ensure!(
        package_receipt["native_provenance"]["release_eligible"] == true
            && package_receipt["native_provenance"]["source"]["dirty"] == false
            && package_receipt["native_provenance"]["runtime"] == "rust"
            && package_receipt["native_provenance"]["llama_revision"] == LLAMA_REVISION,
        "development or foreign backend payload refused"
    );
    let rows: Vec<Row> = serde_json::from_value(package_receipt["files"].clone())?;
    ensure!(
        !rows.is_empty() && inventory(&bundle)? == rows,
        "source payload differs from canonical package receipt"
    );
    let artifacts = package_receipt["artifacts"]
        .as_object()
        .context("missing packaged artifact set")?;
    ensure!(
        artifacts.len() == 2 && artifacts.contains_key("dmg") && artifacts.contains_key("zip"),
        "macOS package must own exactly DMG and ZIP artifacts"
    );
    let stem = format!(
        "BroTypist-{}-macos-{}",
        identity.version, identity.architecture
    );
    let mut paths = BTreeMap::new();
    for extension in ["dmg", "zip"] {
        let path = assets.join(format!("{stem}.{extension}"));
        regular(&path)?;
        let sealed = &artifacts[extension];
        ensure!(
            Path::new(sealed["path"].as_str().context("artifact path missing")?).canonicalize()?
                == path.canonicalize()?
                && sealed["sha256"].as_str() == Some(hash(&path)?.as_str())
                && sealed["size"].as_u64() == Some(fs::metadata(&path)?.len()),
            "owned artifact path/hash/size differs from canonical receipt"
        );
        paths.insert(extension, path);
    }
    let signature = validate_bundle(&bundle, &identity, &rows, &package_receipt)?;
    receipt["identity"] = serde_json::to_value(&identity)?;
    receipt["signature"] = signature.clone();
    receipt["checks"]["owned_assets_and_canonical_payload"] = json!(true);
    let unrelated = work.join("unrelated-profile");
    fs::create_dir(&unrelated)?;
    let sentinel = unrelated.join("user-data-preservation.txt");
    let fixture_settings = unrelated.join("settings.json");
    write_new(
        &sentinel,
        b"preserve unrelated user data on disposable uninstall\n",
    )?;
    write_new(
        &fixture_settings,
        b"{\"fixture\":\"unrelated profile, never opened by the application\"}\n",
    )?;
    let sentinel_before = fs::read(&sentinel)?;
    let settings_before = fs::read(&fixture_settings)?;
    let fixture_before = tree_snapshot(&unrelated)?;

    fs::create_dir(mount)?;
    capture(
        Command::new("/usr/bin/hdiutil")
            .arg("verify")
            .arg(&paths["dmg"]),
        COMMAND_TIMEOUT,
    )?;
    capture(
        Command::new("/usr/bin/hdiutil")
            .args([
                "attach",
                "-readonly",
                "-nobrowse",
                "-noautoopen",
                "-mountpoint",
            ])
            .arg(mount)
            .arg(&paths["dmg"]),
        COMMAND_TIMEOUT,
    )?;
    *attached = true;
    ensure!(mounted(mount), "attach did not mount the owned image");
    validate_volume(mount)?;
    #[cfg(target_os = "macos")]
    native::verify_read_only_mount(mount)?;
    let mounted_app = mount.join("BroTypist.app");
    ensure!(
        validate_bundle(&mounted_app, &identity, &rows, &package_receipt)? == signature,
        "mounted signing identity changed"
    );
    receipt["checks"]["read_only_dmg_layout_and_payload"] = json!(true);
    let applications = work.join("DisposableApplications");
    fs::create_dir(&applications)?;
    let installed = applications.join("BroTypist.app");
    capture(
        Command::new("/usr/bin/ditto")
            .arg(&mounted_app)
            .arg(&installed),
        COMMAND_TIMEOUT,
    )?;
    ensure!(
        validate_bundle(&installed, &identity, &rows, &package_receipt)? == signature,
        "copied signing identity changed"
    );
    receipt["checks"]["disposable_drag_copy"] = json!(true);

    let profile = work.join("profile");
    fs::create_dir(&profile)?;
    let home = work.join("home");
    fs::create_dir(&home)?;
    let temporary = work.join("tmp");
    fs::create_dir(&temporary)?;
    let model = PathBuf::from(std::env::var_os("BROTYPIST_INSTALLER_SMOKE_MODEL_PATH")
        .context("Nonactivating GUI smoke requires an explicit cached Tiny model to avoid first-run Welcome")?);
    ensure!(
        model.is_absolute(),
        "The installer GUI smoke model path must be absolute"
    );
    regular(&model)?;
    let model = model.canonicalize()?;
    let model_path = model
        .to_str()
        .context("The installer GUI smoke model path must be Unicode")?;
    let settings = profile.join("settings.json");
    json_new(
        &settings,
        &json!({"setup_completed":true,"global_enabled":false,"launch_at_login":false,
        "check_updates_automatically":false,"clipboard_context":false,"screen_context":false,"screenshot_context":false,
        "record_history":false,"record_unaccepted":false,"statistics_enabled":false,"show_accessory":false,
        "model_path":model_path,"model_id":"","prompt_style":"continuation"}),
    )?;
    let environment = isolated_environment(&profile, &home, &temporary);
    let executable = installed.join("Contents/MacOS/BroTypist");
    let version = capture(
        command(&executable, &environment).arg("--version"),
        COMMAND_TIMEOUT,
    )?;
    ensure!(
        version.trim() == format!("BroTypist {}", identity.version),
        "copied executable version differs"
    );
    let doctor = capture(
        command(&executable, &environment).args(["--paused", "doctor"]),
        COMMAND_TIMEOUT,
    )?;
    let doctor: Value = serde_json::from_str(&doctor).context("doctor did not emit JSON")?;
    ensure!(
        doctor["runtime"] == "Rust"
            && doctor["platform"] == "macos"
            && doctor["version"] == identity.version
            && doctor["global_enabled"] == false
            && doctor["settings"].as_str() == settings.to_str(),
        "doctor did not observe the copied runtime and isolated paused profile"
    );
    receipt["checks"]["actual_bundled_cli_version_and_doctor"] = json!({"passed":true,"global_enabled":false,"settings":"owned disposable profile","shell":doctor["shell"]});
    worker_probe(&executable, work, &environment, receipt)?;
    #[cfg(target_os = "macos")]
    {
        native::gui(
            &installed,
            &environment,
            &mut receipt["checks"]["launchservices_gui_and_os_quit"],
        )?;
    }
    #[cfg(not(target_os = "macos"))]
    bail!("LaunchServices GUI requires native macOS");
    // A shutdown must not rewrite an intentionally disabled startup preference.
    let current: Value = serde_json::from_slice(&fs::read(&settings)?)?;
    ensure!(
        current["launch_at_login"] == false
            && current["global_enabled"] == false
            && current["check_updates_automatically"] == false,
        "isolated GUI changed disabled startup/typing/update preferences"
    );
    let profile_before_uninstall = tree_snapshot(&profile)?;
    fs::remove_dir_all(&installed)?;
    ensure!(
        !installed.exists() && tree_snapshot(&profile)? == profile_before_uninstall,
        "disposable app removal changed its external profile"
    );
    ensure!(
        fs::read(&sentinel)? == sentinel_before
            && fs::read(&fixture_settings)? == settings_before
            && tree_snapshot(&unrelated)? == fixture_before,
        "unrelated profile fixture changed"
    );
    receipt["checks"]["uninstall_preserves_owned_and_unrelated_profile_fixture"] = json!(true);

    let zip = &paths["zip"];
    validate_zip(zip, &rows)?;
    traversal_refusal(work, &rows)?;
    let extracted = work.join("zip-extraction");
    fs::create_dir(&extracted)?;
    capture(
        Command::new("/usr/bin/ditto")
            .args(["-x", "-k"])
            .arg(zip)
            .arg(&extracted),
        COMMAND_TIMEOUT,
    )?;
    let top = fs::read_dir(&extracted)?.collect::<std::io::Result<Vec<_>>>()?;
    ensure!(
        top.len() == 1 && top[0].file_name() == "BroTypist.app",
        "ZIP extraction produced unrelated entries"
    );
    ensure!(
        validate_bundle(
            &extracted.join("BroTypist.app"),
            &identity,
            &rows,
            &package_receipt
        )? == signature,
        "ZIP payload signing identity differs"
    );
    ensure!(
        tree_snapshot(&unrelated)? == fixture_before,
        "ZIP extraction changed unrelated preservation fixture"
    );
    receipt["checks"]["zip_preflight_real_extraction_identity_and_traversal_refusal"] = json!(true);
    // Recheck immutable package inputs after all native readers have finished.
    ensure!(
        inventory(&bundle)? == rows,
        "source bundle changed during installer smoke"
    );
    for (extension, path) in &paths {
        ensure!(
            artifacts[*extension]["sha256"].as_str() == Some(hash(path)?.as_str()),
            "sealed asset changed during installer smoke"
        );
    }
    Ok(())
}

fn validate_volume(mount: &Path) -> Result<()> {
    let mut app = false;
    let mut applications = false;
    for entry in fs::read_dir(mount)? {
        let entry = entry?;
        match entry.file_name().to_str().context("non-UTF-8 DMG entry")? {
            "BroTypist.app" => {
                real_dir(&entry.path())?;
                app = true;
            }
            "Applications" => {
                ensure!(
                    fs::symlink_metadata(entry.path())?.file_type().is_symlink()
                        && fs::read_link(entry.path())? == Path::new("/Applications"),
                    "DMG Applications link differs"
                );
                applications = true;
            }
            // hdiutil/HFS+ can create these volume-owned bookkeeping directories;
            // never traverse them or follow any of their contents.
            ".Trashes" | ".fseventsd" | ".Spotlight-V100" | ".HFS+ Private Directory Data\r" => {
                real_dir(&entry.path())?;
            }
            _ => bail!("DMG root contains unexpected payload"),
        }
    }
    ensure!(app && applications, "DMG drag-install layout is incomplete");
    Ok(())
}

fn validate_bundle(
    bundle: &Path,
    identity: &Identity,
    rows: &[Row],
    receipt: &Value,
) -> Result<Value> {
    ensure!(
        read_identity(bundle, "darwin")? == *identity && inventory(bundle)? == rows,
        "bundle bytes or marker differ from sealed payload"
    );
    let provenance: Value = serde_json::from_slice(&fs::read(
        bundle.join("Contents/Resources/native-provenance.json"),
    )?)?;
    ensure!(
        provenance == receipt["native_provenance"],
        "bundle native provenance differs"
    );
    let plist = plist::Value::from_file(bundle.join("Contents/Info.plist"))?;
    let plist = plist
        .as_dictionary()
        .context("bundle plist is not a dictionary")?;
    for (key, expected) in [
        ("CFBundleIdentifier", APP_ID),
        ("CFBundleExecutable", "BroTypist"),
        ("CFBundleShortVersionString", identity.version.as_str()),
    ] {
        ensure!(
            plist.get(key).and_then(plist::Value::as_string) == Some(expected),
            "bundle plist identity differs"
        );
    }
    ensure!(
        plist
            .get("LSFileQuarantineEnabled")
            .and_then(plist::Value::as_boolean)
            == Some(true),
        "bundle quarantine policy disabled"
    );
    let mut binaries = vec![
        bundle.join("Contents/MacOS/BroTypist"),
        bundle.join("Contents/Helpers/BroTypist-updater"),
    ];
    for row in rows
        .iter()
        .filter(|row| row.path.starts_with("Contents/Frameworks/"))
    {
        ensure!(
            row.path.ends_with(".dylib"),
            "unexpected native framework payload"
        );
        binaries.push(bundle.join(&row.path));
    }
    let mut signatures = BTreeMap::new();
    for binary in binaries {
        package::verify_binary(&binary, "darwin", &identity.architecture)?;
        capture(
            Command::new("/usr/bin/codesign")
                .args(["--verify", "--strict"])
                .arg(&binary),
            COMMAND_TIMEOUT,
        )?;
        signatures.insert(
            binary.strip_prefix(bundle)?.to_string_lossy().into_owned(),
            signing_identity(&binary)?,
        );
    }
    capture(
        Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(bundle),
        COMMAND_TIMEOUT,
    )?;
    let signature = signing_identity(bundle)?;
    let kind = signature["kind"]
        .as_str()
        .context("missing signature kind")?;
    ensure!(
        receipt["signature"].as_str() == Some(kind),
        "signature kind differs from package receipt"
    );
    for binary in signatures.values() {
        ensure!(
            binary["kind"] == signature["kind"]
                && binary["team"] == signature["team"]
                && binary["authorities"] == signature["authorities"],
            "payload native code uses a different signing identity"
        );
    }
    Ok(json!({"bundle":signature,"native_code":signatures}))
}

fn signing_identity(path: &Path) -> Result<Value> {
    let output = capture_both(
        Command::new("/usr/bin/codesign")
            .args(["--display", "--verbose=4"])
            .arg(path),
        COMMAND_TIMEOUT,
    )?;
    let mut authorities = Vec::new();
    let mut team = None;
    let mut identifier = None;
    let mut cdhash = None;
    let mut adhoc = false;
    for line in output.lines() {
        if let Some(value) = line.strip_prefix("Authority=") {
            authorities.push(value.to_owned());
        }
        if let Some(value) = line.strip_prefix("TeamIdentifier=") {
            if value != "not set" {
                team = Some(value.to_owned());
            }
        }
        if let Some(value) = line.strip_prefix("Identifier=") {
            identifier = Some(value.to_owned());
        }
        if let Some(value) = line.strip_prefix("CDHash=") {
            cdhash = Some(value.to_owned());
        }
        adhoc |= line == "Signature=adhoc";
    }
    ensure!(
        identifier.is_some() && cdhash.is_some(),
        "codesign omitted code identity/hash"
    );
    ensure!(
        adhoc || (!authorities.is_empty() && team.is_some()),
        "native code has no verifiable signing identity"
    );
    Ok(
        json!({"kind":if adhoc {"ad-hoc"} else {"identity-signed"},"team":team,"authorities":authorities,"identifier":identifier,"cdhash":cdhash}),
    )
}

fn tree_snapshot(root: &Path) -> Result<BTreeMap<String, Option<String>>> {
    fn visit(root: &Path, dir: &Path, output: &mut BTreeMap<String, Option<String>>) -> Result<()> {
        real_dir(dir)?;
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            let metadata = fs::symlink_metadata(&path)?;
            ensure!(
                !linked(&metadata),
                "unexpected link in owned preservation profile"
            );
            let key = path
                .strip_prefix(root)?
                .to_str()
                .context("non-UTF-8 profile fixture")?
                .to_owned();
            if metadata.is_dir() {
                output.insert(key, None);
                visit(root, &path, output)?;
            } else {
                regular(&path)?;
                output.insert(key, Some(hash(&path)?));
            }
        }
        Ok(())
    }
    let mut result = BTreeMap::new();
    visit(root, root, &mut result)?;
    Ok(result)
}

fn isolated_environment(profile: &Path, home: &Path, temporary: &Path) -> BTreeMap<String, String> {
    [
        ("INKLOW_HOME", profile.to_string_lossy().into_owned()),
        ("HOME", home.to_string_lossy().into_owned()),
        ("TMPDIR", temporary.to_string_lossy().into_owned()),
        ("PATH", "/usr/bin:/bin:/usr/sbin:/sbin".into()),
        ("LANG", "en_US.UTF-8".into()),
        ("DYLD_LIBRARY_PATH", String::new()),
        ("DYLD_INSERT_LIBRARIES", String::new()),
        ("PYTHONPATH", String::new()),
        ("PYTHONHOME", String::new()),
    ]
    .into_iter()
    .map(|(key, value)| (key.into(), value))
    .collect()
}
fn command(executable: &Path, environment: &BTreeMap<String, String>) -> Command {
    let mut command = Command::new(executable);
    command.env_clear().envs(environment);
    command
}

fn worker_probe(
    app: &Path,
    work: &Path,
    environment: &BTreeMap<String, String>,
    receipt: &mut Value,
) -> Result<()> {
    let Some(model) = std::env::var_os("BROTYPIST_INSTALLER_SMOKE_MODEL_PATH") else {
        receipt["checks"]["real_tiny_worker"] = json!({"status":"not exercised","reason":"no explicit separately cached Tiny model supplied; normal model cache is never searched"});
        return Ok(());
    };
    let model = PathBuf::from(model);
    ensure!(
        model.is_absolute(),
        "cached Tiny model path must be absolute"
    );
    regular(&model)?;
    let helper = PathBuf::from(std::env::var_os("BROTYPIST_INSTALLER_SMOKE_WORKER_EXE").context("cached Tiny requires explicit BROTYPIST_INSTALLER_SMOKE_WORKER_EXE canonical native helper")?);
    ensure!(
        helper.is_absolute(),
        "canonical worker_smoke executable path must be absolute"
    );
    regular(&helper)?;
    package::verify_binary(&helper, "darwin", arch())?;
    ensure!(
        helper.canonicalize()? != app.canonicalize()?,
        "worker helper cannot be the app itself"
    );
    let worker_root = work.join("worker-root");
    fs::create_dir(&worker_root)?;
    let worker_output = work.join("worker-evidence");
    fs::create_dir(&worker_output)?;
    let mut worker_environment = environment.clone();
    worker_environment.insert(
        "INKLOW_HOME".into(),
        worker_root.to_string_lossy().into_owned(),
    );
    let mut invocation = command(&helper, &worker_environment);
    invocation
        .arg("--app")
        .arg(app)
        .args(["--preset", "tiny", "--model-path"])
        .arg(&model)
        .arg("--output")
        .arg(&worker_output)
        .args(["--timeout-seconds", "180"]);
    capture(&mut invocation, Duration::from_secs(1800))?;
    let evidence_path = worker_output.join("worker-smoke.json");
    regular(&evidence_path)?;
    let evidence: Value = serde_json::from_slice(&fs::read(&evidence_path)?)?;
    ensure!(
        evidence["tool"] == "worker_smoke"
            && evidence["passed"] == true
            && evidence["preset"] == "tiny"
            && evidence["observations"]
                .as_array()
                .is_some_and(|rows| !rows.is_empty()),
        "canonical real Tiny worker receipt did not pass"
    );
    receipt["checks"]["real_tiny_worker"] = json!({"passed":true,"canonical_receipt_sha256":hash(&evidence_path)?,"helper_sha256":hash(&helper)?,"model_sha256":hash(&model)?,"app":"exact disposable DMG copy"});
    // Preserve the actual canonical assertions in the create-new outer receipt as well.
    receipt["worker_evidence"] = evidence;
    Ok(())
}

fn validate_zip(path: &Path, rows: &[Row]) -> Result<()> {
    let mut archive = zip::ZipArchive::new(File::open(path)?)?;
    let expected: BTreeMap<_, _> = rows
        .iter()
        .map(|row| (format!("BroTypist.app/{}", row.path), row))
        .collect();
    let mut directories = BTreeSet::from(["BroTypist.app".to_owned()]);
    for name in expected.keys() {
        let mut parent = Path::new(name).parent();
        while let Some(path) = parent {
            if path.as_os_str().is_empty() {
                break;
            }
            directories.insert(path.to_string_lossy().into_owned());
            parent = path.parent();
        }
    }
    let mut seen = BTreeSet::new();
    let mut payload = BTreeSet::new();
    let mut metadata_bytes = 0u64;
    ensure!(
        archive.len() <= (expected.len() + directories.len()) * 3 + 16,
        "unexpected ZIP entry count"
    );
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        let name = entry.name().trim_end_matches('/');
        ensure!(
            safe_relative(Path::new(name))
                && !entry.name().contains('\\')
                && entry.enclosed_name().is_some(),
            "unsafe ZIP path refused before extraction"
        );
        ensure!(
            seen.insert(name.nfc().case_fold().collect::<String>()),
            "duplicate/case-folded ZIP path refused"
        );
        let mode = entry
            .unix_mode()
            .unwrap_or(if entry.is_dir() { 0o040755 } else { 0o100644 });
        ensure!(
            matches!(mode & 0o170000, 0 | 0o040000 | 0o100000),
            "ZIP link/special entry refused"
        );
        if entry.is_dir() {
            ensure!((mode & 0o170000) != 0o100000, "ZIP directory mode differs");
            ensure!(
                directories.contains(name)
                    || name == "__MACOSX"
                    || name
                        .strip_prefix("__MACOSX/")
                        .is_some_and(|path| directories.contains(path)),
                "unrelated ZIP directory"
            );
        } else if let Some(row) = expected.get(name) {
            ensure!(
                (mode & 0o170000) != 0o040000 && entry.size() == row.size,
                "ZIP payload size/type differs"
            );
            payload.insert(name.to_owned());
        } else if let Some(resource) = name.strip_prefix("__MACOSX/") {
            let path = Path::new(resource);
            let basename = path
                .file_name()
                .and_then(|value| value.to_str())
                .context("unsafe AppleDouble metadata name")?;
            let target_name = basename
                .strip_prefix("._")
                .context("unrelated ZIP metadata")?;
            let target = path
                .parent()
                .unwrap_or(Path::new(""))
                .join(target_name)
                .to_string_lossy()
                .into_owned();
            ensure!(
                expected.contains_key(&target) || directories.contains(&target),
                "AppleDouble metadata has no owned payload target"
            );
            metadata_bytes = metadata_bytes
                .checked_add(entry.size())
                .context("ZIP metadata overflow")?;
            ensure!(
                entry.size() <= 16 * 1024 * 1024 && metadata_bytes <= 64 * 1024 * 1024,
                "ZIP metadata is unexpectedly large"
            );
        } else {
            bail!("unrelated ZIP payload");
        }
    }
    ensure!(
        payload.len() == expected.len(),
        "ZIP omits canonical payload files"
    );
    Ok(())
}

fn traversal_refusal(work: &Path, rows: &[Row]) -> Result<()> {
    let guard = work.join("traversal-guard");
    fs::create_dir(&guard)?;
    let sentinel = guard.join("sentinel.txt");
    write_new(&sentinel, b"unchanged traversal guard\n")?;
    let archive_path = work.join("hostile-traversal.zip");
    let mut archive = zip::ZipWriter::new(
        File::options()
            .write(true)
            .create_new(true)
            .open(&archive_path)?,
    );
    archive.start_file(
        "../traversal-guard/sentinel.txt",
        zip::write::SimpleFileOptions::default(),
    )?;
    archive.write_all(b"this must never extract\n")?;
    archive.finish()?;
    ensure!(
        validate_zip(&archive_path, rows).is_err(),
        "ZIP traversal preflight accepted an escaping payload"
    );
    ensure!(
        fs::read(sentinel)? == b"unchanged traversal guard\n",
        "ZIP traversal refusal changed an outside fixture"
    );
    Ok(())
}

fn capture(command: &mut Command, timeout: Duration) -> Result<String> {
    capture_output(command, timeout, false)
}
fn capture_both(command: &mut Command, timeout: Duration) -> Result<String> {
    capture_output(command, timeout, true)
}
fn capture_output(command: &mut Command, timeout: Duration, both: bool) -> Result<String> {
    fn drain(mut reader: impl Read) -> std::io::Result<(Vec<u8>, bool)> {
        let mut result = Vec::new();
        let mut buffer = [0u8; 8192];
        let mut truncated = false;
        loop {
            let n = reader.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            let keep = n.min(PIPE_LIMIT.saturating_sub(result.len()));
            result.extend_from_slice(&buffer[..keep]);
            truncated |= keep != n;
        }
        Ok((result, truncated))
    }
    let program = command.get_program().to_string_lossy().into_owned();
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    fn stop(child: &mut std::process::Child) {
        #[cfg(unix)]
        {
            unsafe extern "C" {
                fn kill(pid: i32, signal: i32) -> i32;
            }
            // Every capture starts a fresh group; never signal an inherited group.
            if let Ok(pid) = i32::try_from(child.id()) {
                unsafe {
                    kill(-pid, 9);
                }
            }
        }
        let _ = child.kill();
        let _ = child.wait();
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("cannot start installer proof command")?;
    let stdout = child.stdout.take().context("missing command stdout")?;
    let stderr = child.stderr.take().context("missing command stderr")?;
    let (out_send, out_receive) = std::sync::mpsc::sync_channel(1);
    let (err_send, err_receive) = std::sync::mpsc::sync_channel(1);
    thread::spawn(move || {
        let _ = out_send.send(drain(stdout));
    });
    thread::spawn(move || {
        let _ = err_send.send(drain(stderr));
    });
    let started = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => (),
            Err(error) => {
                stop(&mut child);
                return Err(error).context("cannot observe owned command exit");
            }
        }
        if started.elapsed() >= timeout {
            timed_out = true;
            stop(&mut child);
            break child.wait()?;
        }
        thread::sleep(Duration::from_millis(25));
    };
    let out = out_receive.recv_timeout(
        timeout
            .saturating_sub(started.elapsed())
            .max(Duration::from_secs(1)),
    );
    let err = err_receive.recv_timeout(
        timeout
            .saturating_sub(started.elapsed())
            .max(Duration::from_secs(1)),
    );
    if out.is_err() || err.is_err() {
        stop(&mut child);
        bail!("owned command pipes exceeded the evidence deadline");
    }
    let (stdout, out_truncated) = out.context("command stdout collector failed")??;
    let (stderr, err_truncated) = err.context("command stderr collector failed")??;
    ensure!(
        !timed_out && status.success(),
        "{} {}: {}",
        Path::new(&program)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy(),
        if timed_out {
            "exceeded its owned command deadline"
        } else {
            "failed"
        },
        redact(&String::from_utf8_lossy(&stderr))
    );
    ensure!(
        !out_truncated && !err_truncated,
        "command output exceeded evidence bound"
    );
    let mut output = String::from_utf8(stdout).context("command stdout is not UTF-8")?;
    if both {
        output.push_str(&String::from_utf8(stderr).context("command stderr is not UTF-8")?);
    }
    Ok(output)
}
fn redact(text: &str) -> String {
    let mut result = String::new();
    for line in text.lines().take(24) {
        let lower = line.to_ascii_lowercase();
        if [
            "token",
            "secret",
            "password",
            "authorization",
            "api_key",
            "api-key",
            "bearer",
        ]
        .iter()
        .any(|key| lower.contains(key))
        {
            result.push_str("[redacted sensitive diagnostic]\n");
        } else {
            result.extend(line.chars().filter(|value| !value.is_control()).take(160));
            result.push('\n');
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        result = result.replace(home.to_string_lossy().as_ref(), "$HOME");
    }
    result.truncate(result.floor_char_boundary(4096));
    result
}

#[cfg(target_os = "macos")]
mod native {
    use super::*;
    use std::{
        ffi::{CStr, CString, c_char, c_int, c_void},
        mem::transmute,
        ptr,
    };
    type Id = *mut c_void;
    type Sel = *mut c_void;
    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> Id;
        fn sel_registerName(name: *const c_char) -> Sel;
        fn objc_msgSend();
    }
    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {
        static NSWorkspaceLaunchConfigurationEnvironment: Id;
        static NSWorkspaceLaunchConfigurationArguments: Id;
    }
    #[link(name = "Foundation", kind = "framework")]
    unsafe extern "C" {}
    #[repr(C)]
    struct Event {
        ident: usize,
        filter: i16,
        flags: u16,
        fflags: u32,
        data: isize,
        udata: *mut c_void,
    }
    #[repr(C)]
    struct Timespec {
        seconds: i64,
        nanos: i64,
    }
    unsafe extern "C" {
        fn kqueue() -> c_int;
        fn kevent(
            queue: c_int,
            changes: *const Event,
            nchanges: c_int,
            events: *mut Event,
            nevents: c_int,
            timeout: *const Timespec,
        ) -> c_int;
        fn close(fd: c_int) -> c_int;
        fn kill(pid: c_int, signal: c_int) -> c_int;
        fn getuid() -> u32;
        #[cfg_attr(target_arch = "x86_64", link_name = "statfs$INODE64")]
        fn statfs(path: *const c_char, buffer: *mut StatFs) -> c_int;
    }
    #[link(name = "proc")]
    unsafe extern "C" {
        fn proc_pidpath(pid: c_int, buffer: *mut c_void, size: u32) -> c_int;
    }
    // Darwin statfs64 layout (arm64/x86_64); MNT_RDONLY is the low flags bit.
    #[repr(C)]
    struct StatFs {
        bsize: u32,
        iosize: i32,
        blocks: u64,
        bfree: u64,
        bavail: u64,
        files: u64,
        ffree: u64,
        fsid: [i32; 2],
        owner: u32,
        kind: u32,
        flags: u32,
        subtype: u32,
        fstypename: [c_char; 16],
        mountpoint: [c_char; 1024],
        source: [c_char; 1024],
        extended_flags: u32,
        reserved: [u32; 7],
    }
    macro_rules! msg {
        ($receiver:expr,$selector:literal => $result:ty $(,$value:expr => $argument:ty)*) => {{
            let function:unsafe extern "C" fn(Id,Sel $(,$argument)*)->$result = unsafe { transmute(objc_msgSend as unsafe extern "C" fn()) };
            unsafe { function($receiver,sel_registerName(concat!($selector,"\0").as_ptr().cast()) $(,$value)*) }
        }};
    }
    fn class(name: &'static [u8]) -> Id {
        unsafe { objc_getClass(name.as_ptr().cast()) }
    }
    fn string(value: &str) -> Result<Id> {
        let value = CString::new(value)?;
        let object = msg!(class(b"NSString\0"),"stringWithUTF8String:" => Id,value.as_ptr() => *const c_char);
        ensure!(!object.is_null(), "cannot construct native launch string");
        Ok(object)
    }
    fn text(value: Id) -> Result<String> {
        ensure!(!value.is_null(), "native launch identity missing");
        let pointer = msg!(value,"UTF8String" => *const c_char);
        ensure!(!pointer.is_null(), "native launch identity is not Unicode");
        Ok(unsafe { CStr::from_ptr(pointer) }.to_str()?.to_owned())
    }
    fn pump() {
        let date = msg!(class(b"NSDate\0"),"dateWithTimeIntervalSinceNow:" => Id,0.05 => f64);
        let run_loop = msg!(class(b"NSRunLoop\0"),"currentRunLoop" => Id);
        msg!(run_loop,"runUntilDate:" => (),date => Id);
    }
    pub fn verify_read_only_mount(path: &Path) -> Result<()> {
        let path = CString::new(path.as_os_str().as_encoded_bytes())?;
        let mut stat: StatFs = unsafe { std::mem::zeroed() };
        ensure!(
            unsafe { statfs(path.as_ptr(), &mut stat) } == 0 && stat.flags & 1 != 0,
            "owned DMG mount is not read-only"
        );
        Ok(())
    }
    struct Pool(Id);
    impl Drop for Pool {
        fn drop(&mut self) {
            msg!(self.0,"drain" => ());
        }
    }
    struct Queue(c_int);
    impl Drop for Queue {
        fn drop(&mut self) {
            unsafe {
                close(self.0);
            }
        }
    }
    struct OwnedApp {
        application: Id,
        pid: c_int,
        executable: PathBuf,
        queue: Queue,
        exited: bool,
    }
    impl OwnedApp {
        fn exact_process(&self) -> bool {
            let mut path = [0u8; 4096];
            let size =
                unsafe { proc_pidpath(self.pid, path.as_mut_ptr().cast(), path.len() as u32) };
            size > 0
                && CStr::from_bytes_until_nul(&path)
                    .ok()
                    .and_then(|path| {
                        Path::new(std::ffi::OsStr::new(path.to_str().ok()?))
                            .canonicalize()
                            .ok()
                    })
                    .is_some_and(|path| path == self.executable)
        }
        fn exit(&mut self) -> Result<Option<i32>> {
            let mut event: Event = unsafe { std::mem::zeroed() };
            let timeout = Timespec {
                seconds: 0,
                nanos: 0,
            };
            let result = unsafe { kevent(self.queue.0, ptr::null(), 0, &mut event, 1, &timeout) };
            ensure!(result >= 0, "cannot observe owned GUI exit event");
            if result == 0 {
                return Ok(None);
            }
            ensure!(
                event.ident == self.pid as usize
                    && event.filter == -5
                    && event.flags & 0x4000 == 0
                    && event.fflags & 0x84000000 == 0x84000000,
                "owned GUI exit event omitted the requested exit status"
            );
            self.exited = true;
            Ok(Some(event.data as i32))
        }
        fn request_quit(&self) -> bool {
            self.exact_process() && msg!(self.application,"terminate" => bool)
        }
    }
    impl Drop for OwnedApp {
        fn drop(&mut self) {
            if self.exited {
                return;
            }
            // Cleanup is not acceptance: a failed normal Quit stays a failure.
            let _ = self.request_quit();
            let started = Instant::now();
            while started.elapsed() < Duration::from_secs(3) {
                if self.exit().ok().flatten().is_some() {
                    return;
                }
                pump();
            }
            if self.exact_process() {
                unsafe {
                    kill(self.pid, 15);
                }
            }
            let started = Instant::now();
            while started.elapsed() < Duration::from_secs(2) {
                if self.exit().ok().flatten().is_some() {
                    return;
                }
                pump();
            }
            if self.exact_process() {
                unsafe {
                    kill(self.pid, 9);
                }
            }
        }
    }
    pub fn gui(
        bundle: &Path,
        environment: &BTreeMap<String, String>,
        evidence: &mut Value,
    ) -> Result<()> {
        *evidence = json!({"passed":false,"launch":"NSWorkspace exact bundle URL, new instance, without activation",
            "arguments":["--background","--paused"],"normal_quit":"NSRunningApplication.terminate (OS Quit AppleEvent)",
            "exit_status_source":"owned PID kqueue NOTE_EXIT | NOTE_EXITSTATUS","physical_menu_or_key":"not exercised",
            "visible_window":"not asserted for background launch","typing":"not exercised",
            "failure_cleanup_policy":"attempt normal Quit, then only exact disposable executable PID TERM/KILL; payload preserved on failure"});
        use std::os::unix::fs::MetadataExt;
        ensure!(
            unsafe { getuid() } != 0 && fs::metadata("/dev/console")?.uid() == unsafe { getuid() },
            "LaunchServices probe requires the current user's Aqua console session"
        );
        let pool = msg!(class(b"NSAutoreleasePool\0"),"new" => Id);
        ensure!(!pool.is_null(), "cannot create native autorelease pool");
        let _pool = Pool(pool);
        let workspace = msg!(class(b"NSWorkspace\0"),"sharedWorkspace" => Id);
        ensure!(
            !workspace.is_null(),
            "LaunchServices workspace is unavailable"
        );
        let foreground = msg!(workspace,"frontmostApplication" => Id);
        ensure!(!foreground.is_null(), "Aqua desktop is unavailable");
        let foreground_id = msg!(foreground,"bundleIdentifier" => Id);
        ensure!(
            text(foreground_id)? != "com.apple.loginwindow",
            "Aqua console is locked"
        );
        let bundle = bundle.canonicalize()?;
        let executable = bundle.join("Contents/MacOS/BroTypist").canonicalize()?;
        let running = msg!(workspace,"runningApplications" => Id);
        let count = msg!(running,"count" => usize);
        for index in 0..count {
            let app = msg!(running,"objectAtIndex:" => Id,index => usize);
            let url = msg!(app,"bundleURL" => Id);
            if !url.is_null() {
                let path = msg!(url,"path" => Id);
                if let Ok(path) = text(path) {
                    ensure!(
                        Path::new(&path).canonicalize().ok().as_ref() != Some(&bundle),
                        "private bundle already has a running process"
                    );
                }
            }
        }
        let env = msg!(class(b"NSMutableDictionary\0"),"dictionary" => Id);
        for (key, value) in environment {
            let key = string(key)?;
            let value = string(value)?;
            msg!(env,"setObject:forKey:" => (),value => Id,key => Id);
        }
        let arguments = msg!(class(b"NSMutableArray\0"),"array" => Id);
        for argument in ["--background", "--paused"] {
            let value = string(argument)?;
            msg!(arguments,"addObject:" => (),value => Id);
        }
        let configuration = msg!(class(b"NSMutableDictionary\0"),"dictionary" => Id);
        msg!(configuration,"setObject:forKey:" => (),env => Id,NSWorkspaceLaunchConfigurationEnvironment => Id);
        msg!(configuration,"setObject:forKey:" => (),arguments => Id,NSWorkspaceLaunchConfigurationArguments => Id);
        let url = msg!(class(b"NSURL\0"),"fileURLWithPath:" => Id,string(bundle.to_str().context("bundle path is not Unicode")?)? => Id);
        let queue = unsafe { kqueue() };
        ensure!(queue >= 0, "cannot create owned GUI exit observer");
        let queue = Queue(queue);
        let mut error: Id = ptr::null_mut();
        // NSWorkspaceLaunchNewInstance | NSWorkspaceLaunchWithoutActivation.
        // No bundle-ID launch, activation, AX automation or explicit registration.
        let application = msg!(workspace,"launchApplicationAtURL:options:configuration:error:" => Id,url => Id,0x00080200usize => usize,configuration => Id,&mut error => *mut Id);
        ensure!(
            !application.is_null(),
            "LaunchServices could not launch the disposable bundle (native error detail intentionally omitted)"
        );
        let pid = msg!(application,"processIdentifier" => c_int);
        ensure!(pid > 0, "LaunchServices returned no owned PID");
        let mut owned = OwnedApp {
            application,
            pid,
            executable,
            queue,
            exited: false,
        };
        ensure!(
            owned.exact_process(),
            "LaunchServices PID does not execute the exact disposable app"
        );
        let launched_url = msg!(application,"bundleURL" => Id);
        let launched_path = msg!(launched_url,"path" => Id);
        ensure!(
            Path::new(&text(launched_path)?).canonicalize()? == bundle,
            "LaunchServices returned a foreign bundle identity; not targeted"
        );
        evidence["pid"] = json!(pid);
        evidence["exact_owned_bundle_pid"] = json!(true);
        let registration = Event {
            ident: pid as usize,
            filter: -5,
            flags: 0x0001 | 0x0010,
            fflags: 0x80000000 | 0x04000000,
            data: 0,
            udata: ptr::null_mut(),
        };
        ensure!(
            unsafe {
                kevent(
                    owned.queue.0,
                    &registration,
                    1,
                    ptr::null_mut(),
                    0,
                    ptr::null(),
                )
            } == 0,
            "cannot register owned GUI exit-status observer"
        );
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(3) {
            ensure!(
                owned.exit()?.is_none() && owned.exact_process(),
                "owned background GUI exited before lifetime observation"
            );
            pump();
        }
        evidence["observed_background_lifetime_ms"] = json!(started.elapsed().as_millis());
        let requested = owned.request_quit();
        evidence["normal_quit_requested"] = json!(requested);
        ensure!(
            requested,
            "owned GUI refused NSRunningApplication normal Quit"
        );
        let quit_started = Instant::now();
        let status = loop {
            if let Some(status) = owned.exit()? {
                break status;
            }
            ensure!(
                quit_started.elapsed() < GUI_TIMEOUT,
                "owned GUI normal Quit exceeded deadline"
            );
            pump();
        };
        evidence["raw_wait_status"] = json!(status);
        evidence["normal_quit_elapsed_ms"] = json!(quit_started.elapsed().as_millis());
        ensure!(status == 0, "owned GUI did not exit cleanly after OS Quit");
        evidence["passed"] = json!(true);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rows() -> Vec<Row> {
        vec![Row {
            path: "Contents/MacOS/BroTypist".into(),
            size: 1,
            sha256: "not-used-by-preflight".into(),
        }]
    }
    #[test]
    fn zip_preflight_rejects_traversal_and_unrelated_entries() {
        let root = tempfile::tempdir().unwrap();
        for (index, name) in [
            "../escape",
            "/escape",
            "BroTypist.app/../../escape",
            "BroTypist.app\\escape",
            "foreign.txt",
        ]
        .into_iter()
        .enumerate()
        {
            let path = root.path().join(format!("{index}.zip"));
            let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"x").unwrap();
            zip.finish().unwrap();
            assert!(validate_zip(&path, &rows()).is_err(), "{name}");
        }
    }
    #[test]
    fn zip_preflight_accepts_only_complete_owned_files() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("owned.zip");
        let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
        zip.start_file(
            "BroTypist.app/Contents/MacOS/BroTypist",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.write_all(b"x").unwrap();
        zip.finish().unwrap();
        assert!(validate_zip(&path, &rows()).is_ok());
        assert!(
            validate_zip(
                &path,
                &[Row {
                    path: "missing".into(),
                    size: 1,
                    sha256: String::new()
                }]
            )
            .is_err()
        );
    }
    #[test]
    fn diagnostics_are_bounded_and_sensitive_lines_are_removed() {
        let value = redact(&format!(
            "Authorization: Bearer private\n{}",
            "x".repeat(20_000)
        ));
        assert!(!value.contains("private"));
        assert!(value.len() <= 4096);
    }
}
