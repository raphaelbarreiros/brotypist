use crate::{build::linux_entries, common::*};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::Command,
};
use unicode_casefold::UnicodeCaseFold;

pub fn verify_binary(path: &Path, os: &str, architecture: &str) -> Result<()> {
    regular(path)?;
    if os == "darwin" {
        let architectures = run(Command::new("/usr/bin/lipo").arg("-archs").arg(path))?;
        ensure!(
            architectures == architecture,
            "binary architecture differs from native marker: {}",
            path.display()
        );
    } else if os == "linux" {
        let mut header = [0u8; 20];
        File::open(path)?.read_exact(&mut header)?;
        ensure!(
            &header[..6] == b"\x7fELF\x02\x01"
                && u16::from_le_bytes([header[18], header[19]]) == 62
                && architecture == "x86_64",
            "expected native amd64 ELF64"
        );
    } else {
        let mut file = File::open(path)?;
        let mut header = [0u8; 64];
        file.read_exact(&mut header)?;
        ensure!(&header[..2] == b"MZ", "missing DOS executable header");
        let offset = u32::from_le_bytes(header[60..64].try_into()?);
        file.seek(SeekFrom::Start(offset.into()))?;
        let mut pe = [0u8; 6];
        file.read_exact(&mut pe)?;
        ensure!(
            &pe == b"PE\0\0\x64\x86" && architecture == "x86_64",
            "expected native x86_64 PE"
        );
    }
    Ok(())
}
pub fn inspect(root: &Path, bundle: &Path, development: bool) -> Result<Value> {
    if !development {
        crate::release::require_clean_source(root)?;
    }
    let os = platform()?;
    let identity = read_identity(bundle, os)?;
    ensure!(
        identity.version == version(root)?
            && identity.source_commit == commit(root)?
            && identity.architecture == arch(),
        "bundle version/source/native architecture differs from checkout"
    );
    let files = inventory(bundle)?;
    let binary = bundle.join(if os == "darwin" {
        "Contents/MacOS/BroTypist"
    } else if os == "win32" {
        "BroTypist.exe"
    } else {
        "BroTypist"
    });
    verify_binary(&binary, os, &identity.architecture)?;
    if os == "win32" {
        verify_binary(&bundle.join("BroTypist-cli.exe"), os, "x86_64")?;
    }
    let resources = bundle.join(if os == "darwin" {
        "Contents/Resources"
    } else if os == "linux" {
        "share"
    } else {
        "assets"
    });
    let provenance_path = resources.join("native-provenance.json");
    regular(&provenance_path)?;
    let provenance: Value = serde_json::from_slice(&fs::read(provenance_path)?)?;
    ensure!(
        provenance["schema"] == 1
            && provenance["runtime"] == "rust"
            && provenance["llama_revision"] == LLAMA_REVISION
            && provenance["source_kind"] == "pinned-source"
            && provenance["target"] == native_target()?,
        "native provenance differs from pinned Rust backend"
    );
    let observed: SourceState = serde_json::from_value(provenance["source"].clone())
        .context("missing exact source-state receipt")?;
    ensure!(
        observed == source_state(root)?,
        "bundle source digest differs from current checkout"
    );
    if !development {
        ensure!(
            provenance["release_eligible"] == true && !observed.dirty,
            "development build cannot be packaged or sealed for release"
        );
    }
    if provenance["release_eligible"] != true {
        regular(&resources.join(".inklow-development.json"))?;
        ensure!(
            development,
            "local development requires explicit --development verification"
        );
    }
    let libraries = provenance["libraries"]
        .as_array()
        .context("missing backend inventory")?;
    ensure!(!libraries.is_empty(), "backend library inventory empty");
    let lib_dir = bundle.join(if os == "darwin" {
        "Contents/Frameworks"
    } else if os == "linux" {
        "lib"
    } else {
        ""
    });
    let mut library_names = BTreeSet::new();
    for name in libraries {
        let name = name.as_str().context("library basename must be string")?;
        ensure!(
            safe_relative(Path::new(name))
                && Path::new(name).components().count() == 1
                && library_names.insert(name),
            "unsafe/duplicate library basename"
        );
        verify_binary(&lib_dir.join(name), os, &identity.architecture)?;
    }
    let mut signature = json!("unsigned");
    if os == "darwin" {
        let plist = plist::Value::from_file(bundle.join("Contents/Info.plist"))?;
        let info = plist.as_dictionary().context("invalid bundle plist")?;
        ensure!(
            info.get("CFBundleIdentifier")
                .and_then(plist::Value::as_string)
                == Some(APP_ID)
                && info
                    .get("CFBundleExecutable")
                    .and_then(plist::Value::as_string)
                    == Some("BroTypist")
                && info
                    .get("CFBundleShortVersionString")
                    .and_then(plist::Value::as_string)
                    == Some(identity.version.as_str()),
            "bundle metadata mismatch"
        );
        ensure!(
            info.get("CFBundleIconName")
                .and_then(plist::Value::as_string)
                == Some("BroTypist")
                && info
                    .get("CFBundleIconFile")
                    .and_then(plist::Value::as_string)
                    == Some("app.icns"),
            "missing native/classic icon metadata"
        );
        regular(&resources.join("Assets.car"))?;
        regular(&resources.join("app.icns"))?;
        ensure!(
            info.get("LSUIElement").and_then(plist::Value::as_boolean) == Some(true)
                && info
                    .get("LSFileQuarantineEnabled")
                    .and_then(plist::Value::as_boolean)
                    == Some(true),
            "resident/quarantine metadata missing"
        );
        run(Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(bundle))?;
        let details = Command::new("/usr/bin/codesign")
            .args(["--display", "--verbose=4"])
            .arg(bundle)
            .output()?;
        ensure!(details.status.success(), "codesign display failed");
        let details = format!(
            "{}{}",
            String::from_utf8_lossy(&details.stdout),
            String::from_utf8_lossy(&details.stderr)
        );
        signature = json!(if details.contains("Signature=adhoc") {
            "ad-hoc"
        } else {
            "identity-signed"
        });
        audit_mac_links(&binary, &library_names)?;
        for name in &library_names {
            audit_mac_links(&lib_dir.join(name), &library_names)?;
        }
    } else if os == "linux" {
        audit_linux_links(&binary)?;
        for name in &library_names {
            audit_linux_links(&lib_dir.join(name))?;
        }
    }
    if os == "win32" {
        audit_windows_crt(&provenance["windows_crt"], &lib_dir, &library_names)?;
        audit_windows_links(&binary, &library_names)?;
        audit_windows_links(&bundle.join("BroTypist-cli.exe"), &library_names)?;
        for name in &library_names {
            audit_windows_links(&lib_dir.join(name), &library_names)?;
        }
    }
    if os != "win32" {
        let helper = bundle.join(if os == "darwin" {
            "Contents/Helpers/BroTypist-updater"
        } else {
            "helpers/BroTypist-updater"
        });
        verify_binary(&helper, os, &identity.architecture)?;
        let links = if os == "darwin" {
            run(Command::new("/usr/bin/otool").arg("-L").arg(&helper))?
        } else {
            run(Command::new("readelf")
                .args(["--dynamic", "--wide"])
                .arg(&helper))?
        };
        ensure!(
            !links.contains("libllama") && !links.contains("libggml"),
            "updater helper must not depend on native model engine"
        );
        if os == "darwin" {
            audit_mac_links(&helper, &BTreeSet::new())?;
        }
    }
    let home = stage(&std::env::temp_dir(), "version-check")?;
    let cli = if os == "win32" {
        bundle.join("BroTypist-cli.exe")
    } else {
        binary.clone()
    };
    let output = run(Command::new(cli)
        .arg("--version")
        .env("INKLOW_HOME", &home)
        .env_remove("LD_LIBRARY_PATH")
        .env_remove("LD_PRELOAD")
        .env_remove("DYLD_LIBRARY_PATH")
        .env_remove("DYLD_INSERT_LIBRARIES")
        .env_remove("PYTHONPATH")
        .env_remove("PYTHONHOME"))?;
    ensure!(
        output == format!("BroTypist {}", identity.version),
        "native executable version differs from metadata"
    );
    fs::remove_dir_all(home)?;
    Ok(
        json!({"application":APP_ID,"version":identity.version,"source_commit":identity.source_commit,"platform":os,"architecture":identity.architecture,"signature":signature,"notarization":"not checked; no notarization claim","runtime":"rust","release_eligible":provenance["release_eligible"],"native_provenance":provenance,"files":files}),
    )
}
fn audit_mac_links(binary: &Path, names: &BTreeSet<&str>) -> Result<()> {
    let output = run(Command::new("/usr/bin/otool").arg("-L").arg(binary))?;
    for line in output.lines().skip(1) {
        let name = line
            .trim()
            .split(" (compatibility version")
            .next()
            .context("bad otool dependency")?;
        ensure!(
            name.starts_with("/System/Library/")
                || name.starts_with("/usr/lib/")
                || name
                    .strip_prefix("@rpath/")
                    .is_some_and(|v| names.contains(v)),
            "nonrelocatable/unbundled dependency {name}"
        );
    }
    let output = run(Command::new("/usr/bin/otool").arg("-l").arg(binary))?;
    let mut rpath = false;
    for line in output.lines() {
        if line.trim() == "cmd LC_RPATH" {
            rpath = true;
        } else if rpath && line.trim().starts_with("path ") {
            let value = line
                .trim()
                .trim_start_matches("path ")
                .split(" (offset")
                .next()
                .unwrap();
            ensure!(
                ["@loader_path", "@executable_path/../Frameworks"].contains(&value),
                "absolute/foreign LC_RPATH refused"
            );
            rpath = false;
        }
    }
    Ok(())
}
fn audit_linux_links(binary: &Path) -> Result<()> {
    let path = run(Command::new("patchelf").arg("--print-rpath").arg(binary))?;
    ensure!(
        path == "$ORIGIN/lib" || path == "$ORIGIN",
        "nonrelocatable ELF rpath"
    );
    let output = run(Command::new("readelf")
        .args(["--dynamic", "--wide"])
        .arg(binary))?;
    for line in output.lines().filter(|v| v.contains("(NEEDED)")) {
        if let Some(dep) = line.split('[').nth(1).and_then(|v| v.split(']').next()) {
            ensure!(!dep.contains('/'), "absolute ELF dependency refused");
        }
    }
    Ok(())
}

fn audit_windows_crt(receipt: &Value, directory: &Path, libraries: &BTreeSet<&str>) -> Result<()> {
    ensure!(
        receipt["source_kind"] == "visual-studio-redistributable"
            && receipt["architecture"] == "x64"
            && receipt["authenticode"] == "valid-microsoft",
        "missing authenticated app-local Visual Studio CRT provenance"
    );
    let files = receipt["files"]
        .as_array()
        .context("missing VC CRT inventory")?;
    ensure!(!files.is_empty(), "app-local VC CRT inventory is empty");
    let mut runtime_names = BTreeSet::new();
    for file in files {
        let name = file["name"].as_str().context("VC CRT basename missing")?;
        ensure!(
            safe_relative(Path::new(name))
                && Path::new(name).components().count() == 1
                && name.to_ascii_lowercase().ends_with(".dll")
                && runtime_names.insert(name.to_ascii_lowercase())
                && libraries.contains(name),
            "unsafe, duplicate or unbundled VC CRT inventory entry"
        );
        ensure!(
            file["sha256"].as_str() == Some(hash(&directory.join(name))?.as_str()),
            "app-local VC CRT differs from authenticated Visual Studio source: {name}"
        );
    }
    for name in libraries {
        let lower = name.to_ascii_lowercase();
        ensure!(
            lower.starts_with("llama.")
                || lower.starts_with("ggml")
                || runtime_names.contains(&lower),
            "Windows native DLL has no engine/CRT provenance: {name}"
        );
    }
    Ok(())
}

fn audit_windows_links(binary: &Path, libraries: &BTreeSet<&str>) -> Result<()> {
    let output = run(Command::new("dumpbin").arg("/DEPENDENTS").arg(binary))?;
    let system =
        PathBuf::from(std::env::var_os("SystemRoot").context("Windows SystemRoot unavailable")?)
            .join("System32");
    let bundled: BTreeSet<_> = libraries.iter().map(|v| v.to_ascii_lowercase()).collect();
    for line in output.lines() {
        let dependency = line.trim().to_ascii_lowercase();
        if !dependency.ends_with(".dll") || dependency.chars().any(char::is_whitespace) {
            continue;
        }
        ensure!(
            !dependency.starts_with("libomp") && !dependency.starts_with("vcomp"),
            "undeclared OpenMP dependency {dependency}"
        );
        let vc_runtime = dependency.starts_with("vcruntime")
            || dependency.starts_with("msvcp")
            || dependency.starts_with("concrt")
            || dependency.starts_with("vccorlib");
        // A hosted runner's globally installed CRT must never hide a missing
        // portable/MSI payload. All native and CRT DLLs are audited recursively.
        ensure!(
            !vc_runtime || bundled.contains(&dependency),
            "VC runtime must be bundled app-locally: {dependency}"
        );
        ensure!(
            bundled.contains(&dependency)
                || dependency.starts_with("api-ms-win-")
                || dependency.starts_with("ext-ms-win-")
                || system.join(&dependency).is_file(),
            "unbundled Windows dependency {dependency}"
        );
    }
    Ok(())
}

pub fn package(root: &Path, bundle: &Path, output: &Path) -> Result<Value> {
    crate::release::require_clean_source(root)?;
    real_dir(bundle)?;
    let bundle = bundle.canonicalize()?;
    fs::create_dir_all(output)?;
    real_dir(output)?;
    let output = output.canonicalize()?;
    ensure!(
        !output.starts_with(&bundle),
        "output must be outside application"
    );
    let metadata = inspect(root, &bundle, false)?;
    let version = metadata["version"].as_str().unwrap();
    let platform = platform()?;
    let stem = match platform {
        "darwin" => format!("BroTypist-{version}-macos-{}", arch()),
        "linux" => format!("BroTypist-{version}-linux-amd64"),
        "win32" => format!("BroTypist-{version}-windows-x86_64"),
        _ => unreachable!(),
    };
    let extensions = match platform {
        "darwin" => ["dmg", "zip"],
        "linux" => ["deb", "tar.gz"],
        "win32" => ["msi", "zip"],
        _ => unreachable!(),
    };
    for name in [
        format!("{stem}.{}", extensions[0]),
        format!("{stem}.{}", extensions[1]),
        format!("package-receipt-{platform}-{}.json", arch()),
    ] {
        ensure!(
            fs::symlink_metadata(output.join(&name))
                .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
            "refusing existing/unsafe package output {name}"
        );
    }
    let work = stage(&output, "package")?;
    let artifacts = match platform {
        "darwin" => mac_package(&bundle, &work, version)?,
        "linux" => linux_package(root, &bundle, &work, version)?,
        "win32" => windows_package(root, &bundle, &work, version, &metadata)?,
        _ => unreachable!(),
    };
    // Rehash every byte after native packagers read input. A mutated source can never be sealed.
    ensure!(
        metadata["files"] == serde_json::to_value(inventory(&bundle)?)?,
        "payload changed during packaging"
    );
    ensure!(
        metadata["native_provenance"]["source"] == serde_json::to_value(source_state(root)?)?,
        "source changed during packaging"
    );
    crate::release::require_clean_source(root)?;
    let mut published = BTreeMap::new();
    for artifact in artifacts {
        let destination = output.join(artifact.file_name().unwrap());
        publish_file(&artifact, &destination)?;
        let name = destination.file_name().unwrap().to_string_lossy();
        let kind = if name.ends_with(".tar.gz") {
            "tar.gz"
        } else if name.ends_with(".msi") {
            "msi"
        } else if name.ends_with(".deb") {
            "deb"
        } else if name.ends_with(".dmg") {
            "dmg"
        } else {
            "zip"
        };
        published.insert(kind, json!({"path":destination,"sha256":hash(&destination)?,"size":fs::metadata(&destination)?.len()}));
    }
    let mut receipt = json!({"application":APP_ID,"version":version,"source_commit":metadata["source_commit"],"platform":platform,"architecture":arch(),"signature":metadata["signature"],"signing":metadata["signature"],"notarization":"not checked; no notarization claim","publication":if platform == "win32" {"held; unsigned Windows packages are not OTA or published artifacts"} else {"not published"},"scope":if platform=="win32" {"perUser"} else {"native"},"artifacts":published,"files":metadata["files"],"native_provenance":metadata["native_provenance"],"runtime":"rust","release_eligible":true});
    if platform == "win32" {
        receipt["wix_version"] = json!("5.0.2");
        receipt["product_code"] = json!(stable_guid(&format!("product/{version}")));
        receipt["upgrade_code"] = json!(stable_guid("upgrade"));
        receipt["install_directory"] = json!("%LOCALAPPDATA%\\Programs\\BroTypist");
        receipt["start_menu_shortcut"] =
            json!("%APPDATA%\\Microsoft\\Windows\\Start Menu\\Programs\\BroTypist.lnk");
    }
    json_new(
        &output.join(format!("package-receipt-{platform}-{}.json", arch())),
        &receipt,
    )?;
    fs::remove_dir_all(work)?;
    Ok(receipt)
}
fn mac_package(bundle: &Path, work: &Path, version: &str) -> Result<Vec<PathBuf>> {
    ensure!(cfg!(target_os = "macos"), "DMG requires native macOS");
    ensure!(
        bundle.file_name().and_then(|v| v.to_str()) == Some("BroTypist.app"),
        "macOS package bundle must be named BroTypist.app"
    );
    let stem = format!("BroTypist-{version}-macos-{}", arch());
    let volume = work.join("volume");
    fs::create_dir(&volume)?;
    run(Command::new("/usr/bin/ditto")
        .arg(bundle)
        .arg(volume.join("BroTypist.app")))?;
    #[cfg(unix)]
    std::os::unix::fs::symlink("/Applications", volume.join("Applications"))?;
    let dmg = work.join(format!("{stem}.dmg"));
    let zip = work.join(format!("{stem}.zip"));
    run(Command::new("/usr/bin/hdiutil")
        .args(["create", "-volname", "BroTypist", "-srcfolder"])
        .arg(&volume)
        .args(["-format", "UDZO", "-fs", "HFS+"])
        .arg(&dmg))?;
    run(Command::new("/usr/bin/ditto")
        .args(["-c", "-k", "--sequesterRsrc", "--keepParent"])
        .arg(bundle)
        .arg(&zip))?;
    run(Command::new("/usr/bin/hdiutil").arg("verify").arg(&dmg))?;
    // ditto preserves signed resources, Finder metadata and quarantine; no xattr deletion or notarization substitute.
    Ok(vec![dmg, zip])
}
fn copy_linux(bundle: &Path, target: &Path) -> Result<()> {
    copy_tree(bundle, target)?;
    for path in [
        target.join("BroTypist.desktop"),
        target.join(format!("share/applications/{APP_ID}.desktop")),
        target.join(format!("share/applications/{APP_ID}.InputMethod.desktop")),
    ] {
        if path.exists() {
            fs::remove_file(path)?;
        }
    }
    normalize_modes(target)?;
    Ok(())
}
fn normalize_modes(root: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root, fs::Permissions::from_mode(0o755))?;
        for entry in fs::read_dir(root)? {
            let path = entry?.path();
            let m = fs::symlink_metadata(&path)?;
            ensure!(!linked(&m), "linked mode target");
            if m.is_dir() {
                normalize_modes(&path)?;
            } else {
                let mode = if m.permissions().mode() & 0o111 != 0 {
                    0o755
                } else {
                    0o644
                };
                fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
            }
        }
    }
    let _ = root;
    Ok(())
}
fn linux_package(root: &Path, bundle: &Path, work: &Path, version: &str) -> Result<Vec<PathBuf>> {
    ensure!(
        cfg!(target_os = "linux")
            && arch() == "x86_64"
            && run(Command::new("dpkg").arg("--print-architecture"))? == "amd64",
        "native Debian/Ubuntu amd64 required"
    );
    let deb_root = work.join("deb-root");
    let app = deb_root.join("opt/brotypist");
    fs::create_dir_all(app.parent().unwrap())?;
    copy_linux(bundle, &app)?;
    let binary = deb_root.join("usr/bin/brotypist");
    fs::create_dir_all(binary.parent().unwrap())?;
    fs::write(
        &binary,
        "#!/bin/sh\nexec /opt/brotypist/launch-brotypist \"$@\"\n",
    )?;
    executable(&binary)?;
    linux_entries(
        root,
        &deb_root.join("usr/share/applications"),
        "/usr/bin/brotypist",
        APP_ID,
    )?;
    fs::create_dir_all(deb_root.join("usr/share/icons"))?;
    copy_tree(
        &app.join("share/icons/hicolor"),
        &deb_root.join("usr/share/icons/hicolor"),
    )?;
    let dependencies = linux_dependencies(&app, work)?;
    let control = deb_root.join("DEBIAN");
    fs::create_dir(&control)?;
    let size = inventory(&deb_root)?
        .iter()
        .map(|v| v.size)
        .sum::<u64>()
        .div_ceil(1024);
    fs::write(
        control.join("control"),
        format!(
            "Package: brotypist\nVersion: {version}\nSection: utils\nPriority: optional\nArchitecture: amd64\nMaintainer: BroTypist contributors <noreply@localhost>\nInstalled-Size: {size}\nDepends: {dependencies}\nConflicts: inklow\nReplaces: inklow\nDescription: Local-first predictive typing for supported desktop text inputs\n Native Rust writing assistance with AT-SPI and OS-native UI.\n Supports Ubuntu 24.04+ and Debian 13+ amd64 X11; KDE Wayland requires\n explicit input-method selection. Other compositors are unsupported.\n Requires AVX2, FMA and F16C. Models download only on request.\n Settings, history and model caches are never package-owned.\n"
        ),
    )?;
    normalize_modes(&deb_root)?;
    let stem = format!("BroTypist-{version}-linux-amd64");
    let deb = work.join(format!("{stem}.deb"));
    run(Command::new("dpkg-deb")
        .args(["--build", "--root-owner-group"])
        .arg(&deb_root)
        .arg(&deb))?;
    let portable_root = work.join("portable");
    fs::create_dir(&portable_root)?;
    let portable = portable_root.join("BroTypist");
    copy_linux(bundle, &portable)?;
    let archive = work.join(format!("{stem}.tar.gz"));
    write_tar(&portable, &archive)?;
    Ok(vec![deb, archive])
}
fn linux_dependencies(app: &Path, work: &Path) -> Result<String> {
    let debian = work.join("debian");
    fs::create_dir(&debian)?;
    fs::write(
        debian.join("control"),
        "Source: brotypist\nSection: utils\nPriority: optional\nMaintainer: BroTypist contributors <noreply@localhost>\n\nPackage: brotypist\nArchitecture: amd64\nDescription: Local-first predictive typing\n",
    )?;
    let mut elfs = Vec::new();
    let mut directories = BTreeSet::new();
    let mut sonames = BTreeSet::new();
    for row in inventory(app)? {
        let path = app.join(row.path);
        let mut header = [0u8; 18];
        if File::open(&path)?.read_exact(&mut header).is_ok()
            && &header[..4] == b"\x7fELF"
            && [2, 3].contains(&u16::from_le_bytes([header[16], header[17]]))
        {
            directories.insert(path.parent().unwrap().to_owned());
            let dynamic = run(Command::new("readelf")
                .args(["--dynamic", "--wide"])
                .arg(&path))?;
            for line in dynamic.lines().filter(|v| v.contains("(SONAME)")) {
                if let Some(name) = line.split('[').nth(1).and_then(|v| v.split(']').next()) {
                    sonames.insert(name.to_owned());
                }
            }
            elfs.push(path);
        }
    }
    ensure!(!elfs.is_empty(), "no dynamic ELF input");
    let mut local = String::new();
    for soname in sonames {
        if let Some((name, suffix)) = soname.split_once(".so.") {
            local.push_str(&format!("{name} {suffix} brotypist\n"));
        } else if let Some(base) = soname.strip_suffix(".so") {
            if let Some((name, suffix)) = base.rsplit_once('-') {
                if suffix.starts_with(|c: char| c.is_ascii_digit()) {
                    local.push_str(&format!("{name} {suffix} brotypist\n"));
                }
            }
        }
    }
    fs::write(debian.join("shlibs.local"), local)?;
    let mut command = Command::new("dpkg-shlibdeps");
    command
        .current_dir(work)
        .env("LC_ALL", "C")
        .args(["-xbrotypist", "-O"]);
    for dir in directories {
        command.arg(format!("-l{}", dir.display()));
    }
    for elf in elfs {
        command.arg(format!("-e{}", elf.display()));
    }
    let output = run(&mut command)?;
    let dependencies = output
        .lines()
        .find_map(|v| v.strip_prefix("shlibs:Depends="))
        .context("dpkg-shlibdeps did not emit dependencies")?;
    ensure!(
        !dependencies.contains("${"),
        "unresolved Debian dependencies"
    );
    // Session bus and dlopen services cannot all be derived from DT_NEEDED.
    Ok(format!(
        "{dependencies}, libc6 (>= 2.39), at-spi2-core (>= 2.52), dbus-user-session, libsecret-1-0, libgtk-3-0 (>= 3.24), librsvg2-common, shared-mime-info, libwayland-client0, libxkbcommon0"
    ))
}
pub fn write_tar(root: &Path, output: &Path) -> Result<()> {
    let files = inventory(root)?;
    let stream =
        flate2::write::GzEncoder::new(File::create(output)?, flate2::Compression::default());
    let mut archive = tar::Builder::new(stream);
    for row in files {
        let path = root.join(&row.path);
        let mut header = tar::Header::new_gnu();
        header.set_size(row.size);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            header.set_mode(if fs::metadata(&path)?.permissions().mode() & 0o111 != 0 {
                0o755
            } else {
                0o644
            });
        }
        #[cfg(not(unix))]
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        archive.append_data(
            &mut header,
            Path::new("BroTypist").join(&row.path),
            File::open(path)?,
        )?;
    }
    archive.into_inner()?.finish()?.sync_all()?;
    Ok(())
}
pub fn write_zip(root: &Path, output: &Path) -> Result<()> {
    let files = inventory(root)?;
    let mut archive = zip::ZipWriter::new(File::create(output)?);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);
    for row in files {
        archive.start_file(format!("BroTypist/{}", row.path), options)?;
        std::io::copy(&mut File::open(root.join(row.path))?, &mut archive)?;
    }
    archive.finish()?.sync_all()?;
    Ok(())
}
fn installer_namespace() -> uuid::Uuid {
    uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_URL,
        b"https://inklow.com/installer/windows/x86_64",
    )
}
pub fn stable_guid(value: &str) -> String {
    let folded: String = value.case_fold().collect();
    uuid::Uuid::new_v5(&installer_namespace(), folded.as_bytes())
        .to_string()
        .to_uppercase()
}
fn identifier(prefix: &str, relative: &str) -> String {
    let folded: String = relative.case_fold().collect();
    let hash = format!("{:x}", Sha256::digest(folded.as_bytes()));
    format!("{prefix}{}", &hash[..32])
}
fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
pub fn payload_source(bundle: &Path, files: &[Row]) -> Result<String> {
    let mut directories = BTreeMap::new();
    directories.insert(String::new(), String::from("INSTALLFOLDER"));
    for row in files {
        let path = Path::new(&row.path);
        ensure!(safe_relative(path), "unsafe installer path");
        let mut current = String::new();
        if let Some(parent) = path.parent() {
            for part in parent.components() {
                let part = part
                    .as_os_str()
                    .to_str()
                    .context("non-UTF8 installer path")?;
                if !current.is_empty() {
                    current.push('/');
                }
                current.push_str(part);
                directories.insert(current.clone(), identifier("dir_", &current));
            }
        }
    }
    let mut output = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<Wix xmlns=\"http://wixtoolset.org/schemas/v4/wxs\"><Fragment>",
    );
    // DirectoryRef fragments avoid nesting-order assumptions while preserving stable directory IDs.
    for (relative, id) in directories.iter().filter(|(p, _)| !p.is_empty()) {
        let path = Path::new(relative);
        let parent = path.parent().unwrap().to_str().unwrap();
        output.push_str(&format!(
            "<DirectoryRef Id=\"{}\"><Directory Id=\"{id}\" Name=\"{}\" /></DirectoryRef>",
            directories[parent],
            xml(path.file_name().unwrap().to_str().unwrap())
        ));
    }
    output.push_str("<ComponentGroup Id=\"InklowPayload\">");
    for (relative, directory) in &directories {
        output.push_str(&format!("<Component Id=\"{}\" Directory=\"{directory}\" Guid=\"{}\" Bitness=\"always64\"><RegistryValue Root=\"HKCU\" Key=\"Software\\Inklow\\Installer\\Directories\" Name=\"{}\" Value=\"1\" Type=\"integer\" KeyPath=\"yes\"/><RemoveFolder Id=\"{}\" On=\"uninstall\"/></Component>", identifier("cleanup_",relative),stable_guid(&format!("directory/{relative}")),identifier("d_",relative),identifier("remove_",relative)));
    }
    for row in files {
        let path = Path::new(&row.path);
        let parent = path.parent().unwrap().to_str().unwrap();
        output.push_str(&format!("<Component Id=\"{}\" Directory=\"{}\" Guid=\"{}\" Bitness=\"always64\"><File Id=\"{}\" Source=\"{}\" Name=\"{}\"/><RegistryValue Root=\"HKCU\" Key=\"Software\\Inklow\\Installer\\Files\" Name=\"{}\" Value=\"1\" Type=\"integer\" KeyPath=\"yes\"/></Component>",identifier("cmp_",&row.path),directories[parent],stable_guid(&format!("file/{}",row.path)),identifier("file_",&row.path),xml(&bundle.join(path).to_string_lossy()),xml(path.file_name().unwrap().to_str().unwrap()),identifier("f_",&row.path)));
    }
    output.push_str("</ComponentGroup></Fragment></Wix>\n");
    Ok(output)
}
fn windows_package(
    root: &Path,
    bundle: &Path,
    work: &Path,
    version: &str,
    metadata: &Value,
) -> Result<Vec<PathBuf>> {
    ensure!(
        cfg!(target_os = "windows") && arch() == "x86_64",
        "native Windows x86_64 required; publication remains held"
    );
    let [major, minor, patch] = validate_version(version)?;
    ensure!(
        major <= 255 && minor <= 255 && patch <= 65535,
        "MSI version exceeds 255.255.65535"
    );
    let wix = run(Command::new("wix").arg("--version"))?;
    ensure!(
        wix.split('+').next() == Some("5.0.2"),
        "WiX5.0.2 required (build-only dotnet tool)"
    );
    let product = work.join("Product.wxs");
    fs::copy(root.join("packaging/windows/Product.wxs.in"), &product)?;
    let payload = work.join("Payload.wxs");
    let files: Vec<Row> = serde_json::from_value(metadata["files"].clone())?;
    fs::write(&payload, payload_source(bundle, &files)?)?;
    let stem = format!("BroTypist-{version}-windows-x86_64");
    let msi = work.join(format!("{stem}.msi"));
    let zip = work.join(format!("{stem}.zip"));
    let mut command = Command::new("wix");
    command
        .current_dir(work)
        .arg("build")
        .arg(product)
        .arg(payload)
        .args(["-arch", "x64", "-o"])
        .arg(&msi);
    for define in [
        format!("Version={version}"),
        format!("ProductCode={}", stable_guid(&format!("product/{version}"))),
        format!("UpgradeCode={}", stable_guid("upgrade")),
        format!(
            "SourceCommit={}",
            metadata["source_commit"].as_str().unwrap()
        ),
        format!("RegistrationGuid={}", stable_guid("registration")),
        format!("BundlePath={}", bundle.display()),
    ] {
        command.arg("-d").arg(define);
    }
    run(&mut command)?;
    write_zip(bundle, &zip)?;
    Ok(vec![msi, zip])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn zip_and_tar_have_only_owned_regular_paths() {
        let t = tempfile::tempdir().unwrap();
        let app = t.path().join("app");
        fs::create_dir(&app).unwrap();
        fs::create_dir(app.join("assets")).unwrap();
        fs::write(app.join("assets/icon"), b"real bytes").unwrap();
        let z = t.path().join("a.zip");
        write_zip(&app, &z).unwrap();
        let mut zip = zip::ZipArchive::new(File::open(z).unwrap()).unwrap();
        assert_eq!(zip.len(), 1);
        assert_eq!(zip.by_index(0).unwrap().name(), "BroTypist/assets/icon");
        let tar = t.path().join("a.tar.gz");
        write_tar(&app, &tar).unwrap();
        let decoder = flate2::read::GzDecoder::new(File::open(tar).unwrap());
        let mut archive = tar::Archive::new(decoder);
        let mut entries = archive.entries().unwrap();
        let entry = entries.next().unwrap().unwrap();
        assert_eq!(entry.path().unwrap(), Path::new("BroTypist/assets/icon"));
        assert_eq!(entry.header().uid().unwrap(), 0);
        assert_eq!(entry.header().mtime().unwrap(), 0);
    }
    #[test]
    fn stable_family_and_safe_msi_xml() {
        assert_eq!(stable_guid("upgrade"), stable_guid("UPGRADE"));
        assert_ne!(stable_guid("product/0.4.13"), stable_guid("product/0.4.14"));
        let files = vec![Row {
            path: "assets/A&B.png".into(),
            size: 1,
            sha256: "a".repeat(64),
        }];
        let text = payload_source(Path::new("C:/Build & Signed"), &files).unwrap();
        assert!(text.contains("A&amp;B.png"));
        assert!(text.contains("Root=\"HKCU\""));
        assert!(!text.contains("RemoveFile"));
        assert!(!text.contains("AppDataFolder"));
        assert!(!text.contains("CustomAction"));
    }
    #[test]
    fn installer_casefold_matches_python_identity() {
        assert_eq!(
            stable_guid("file/straße.png"),
            stable_guid("FILE/STRASSE.PNG")
        );
        assert_eq!(
            identifier("cmp_", "straße.png"),
            identifier("cmp_", "STRASSE.PNG")
        );
    }
    #[test]
    fn pe_architecture_rejects_truncation_and_arm() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("app.exe");
        let mut bytes = vec![0u8; 70];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[60..64].copy_from_slice(&64u32.to_le_bytes());
        bytes[64..70].copy_from_slice(b"PE\0\0\x64\x86");
        fs::write(&path, &bytes).unwrap();
        assert!(verify_binary(&path, "win32", "x86_64").is_ok());
        bytes[68] = 0xaa;
        fs::write(&path, bytes).unwrap();
        assert!(verify_binary(&path, "win32", "x86_64").is_err());
    }
}
