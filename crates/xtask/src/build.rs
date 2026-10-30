use crate::common::*;
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Deserialize)]
struct NativeLibraries {
    schema: u32,
    llama_revision: String,
    target: String,
    target_os: String,
    source_kind: String,
    library_dir: PathBuf,
    libraries: Vec<PathBuf>,
}

pub fn build(root: &Path, output: &Path, replace: bool, development: bool) -> Result<PathBuf> {
    let os = platform()?;
    ensure!(
        arch() == "arm64" && os == "darwin"
            || arch() == "x86_64" && ["darwin", "linux", "win32"].contains(&os),
        "unsupported native architecture"
    );
    if !development {
        crate::release::require_clean_source(root)?;
    }
    let output = destination(output, root, replace, os)?;
    let lock_path = output.parent().unwrap().join(format!(
        ".{}.build.lock",
        output.file_name().unwrap().to_string_lossy()
    ));
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)?;
    ensure!(
        !linked(&fs::symlink_metadata(&lock_path)?),
        "linked lockfile refused"
    );
    lock.try_lock_exclusive()
        .context("another builder owns this destination")?;
    let work = stage(output.parent().unwrap(), "build")?;
    let result = assemble(root, &work, &output, replace, os, development);
    match result {
        Ok(()) => {
            fs::remove_dir_all(&work)?;
            Ok(output)
        }
        Err(error) => Err(error.context(format!(
            "staging retained at {}; previous backup, if any, is its sibling",
            work.display()
        ))),
    }
}

fn assemble(
    root: &Path,
    work: &Path,
    output: &Path,
    replace: bool,
    os: &str,
    development: bool,
) -> Result<()> {
    let source = source_state(root)?;
    ensure!(
        development || !source.dirty,
        "dirty source cannot produce a release build"
    );
    let target = native_target()?;
    let target_dir = work.join("cargo-target");
    let mut cargo = Command::new("cargo");
    cargo
        .current_dir(root)
        .args([
            "build",
            "--locked",
            "--release",
            "-p",
            "brotypist",
            "-p",
            "brotypist-updater",
            "--bins",
            "--message-format=json-render-diagnostics",
            "--target",
        ])
        .arg(&target)
        .arg("--target-dir")
        .arg(&target_dir);
    cargo
        .env_remove("BROTYPIST_LLAMA_LIB_DIR")
        .env_remove("CARGO_BUILD_TARGET")
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS");
    if os == "darwin" {
        cargo.env("MACOSX_DEPLOYMENT_TARGET", "15.0");
    }
    if os == "win32" {
        cargo.env("RUSTFLAGS", "-C target-feature=+crt-static");
    }
    let messages = run(&mut cargo)?;
    let mut executables = BTreeMap::new();
    let mut manifests = BTreeSet::new();
    for line in messages.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let package_id = value["package_id"].as_str().unwrap_or("");
        let is_app = package_id.contains("#brotypist@") || package_id.contains("/brotypist#");
        let is_helper = package_id.contains("#brotypist-updater@")
            || package_id.contains("/brotypist-updater#");
        if !is_app && !is_helper {
            continue;
        }
        match value["reason"].as_str() {
            Some("compiler-artifact") => {
                if let (Some(name), Some(path)) = (
                    value["target"]["name"].as_str(),
                    value["executable"].as_str(),
                ) {
                    executables.insert(name.to_owned(), PathBuf::from(path));
                }
            }
            Some("build-script-executed") if is_app => {
                if let Some(dir) = value["out_dir"].as_str() {
                    manifests.insert(PathBuf::from(dir).join("native-libraries.json"));
                }
            }
            _ => (),
        }
    }
    ensure!(
        manifests.len() == 1,
        "expected one fresh BroTypist native library manifest, got {}",
        manifests.len()
    );
    let manifest_path = manifests.iter().next().unwrap();
    regular(manifest_path)?;
    ensure!(
        manifest_path
            .canonicalize()?
            .starts_with(target_dir.canonicalize()?),
        "native manifest outside fresh target directory"
    );
    let manifest: NativeLibraries = serde_json::from_slice(&fs::read(manifest_path)?)?;
    ensure!(
        manifest.schema == 1
            && manifest.llama_revision == LLAMA_REVISION
            && manifest.target == target
            && manifest.target_os == std::env::consts::OS
            && manifest.source_kind == "pinned-source",
        "native backend provenance/target mismatch"
    );
    ensure!(
        manifest
            .library_dir
            .canonicalize()?
            .starts_with(target_dir.canonicalize()?),
        "backend outside fresh build directory"
    );
    let bundle = work.join(if os == "darwin" {
        "BroTypist.app"
    } else {
        "BroTypist"
    });
    let (bin_dir, lib_dir, resources) = if os == "darwin" {
        (
            bundle.join("Contents/MacOS"),
            bundle.join("Contents/Frameworks"),
            bundle.join("Contents/Resources"),
        )
    } else if os == "linux" {
        (bundle.clone(), bundle.join("lib"), bundle.join("share"))
    } else {
        (bundle.clone(), bundle.clone(), bundle.join("assets"))
    };
    for path in [&bin_dir, &lib_dir, &resources] {
        fs::create_dir_all(path)?;
    }
    let mut binaries = Vec::new();
    for name in if os == "win32" {
        vec!["BroTypist", "BroTypist-cli"]
    } else {
        vec!["BroTypist"]
    } {
        let source = executables
            .get(name)
            .with_context(|| format!("Cargo did not emit required native binary {name}"))?;
        regular(source)?;
        ensure!(
            source
                .canonicalize()?
                .starts_with(target_dir.canonicalize()?),
            "binary outside fresh target directory"
        );
        let destination =
            bin_dir.join(format!("{name}{}", if os == "win32" { ".exe" } else { "" }));
        fs::copy(source, &destination)?;
        executable(&destination)?;
        binaries.push(destination);
    }
    if os != "win32" {
        let source = executables
            .get("BroTypist-updater")
            .context("Cargo did not emit standalone BroTypist-updater")?;
        regular(source)?;
        ensure!(
            source
                .canonicalize()?
                .starts_with(target_dir.canonicalize()?),
            "helper outside fresh target directory"
        );
        let helpers = bundle.join(if os == "darwin" {
            "Contents/Helpers"
        } else {
            "helpers"
        });
        fs::create_dir(&helpers)?;
        let helper = helpers.join("BroTypist-updater");
        fs::copy(source, &helper)?;
        executable(&helper)?;
        binaries.push(helper);
    }
    let mut libraries = Vec::new();
    let mut names = BTreeSet::new();
    ensure!(
        !manifest.libraries.is_empty(),
        "missing standalone native libraries"
    );
    for source in &manifest.libraries {
        let canonical = source.canonicalize()?;
        ensure!(
            canonical.starts_with(target_dir.canonicalize()?) && canonical.is_file(),
            "backend source outside fresh Cargo output"
        );
        let name = source.file_name().context("library basename missing")?;
        ensure!(
            names.insert(name.to_string_lossy().to_lowercase()),
            "colliding library names"
        );
        let destination = lib_dir.join(name);
        fs::copy(source, &destination)?;
        libraries.push(destination);
    }
    let version = version(root)?;
    let identity = Identity {
        application: APP_ID.into(),
        platform: os.into(),
        version: version.clone(),
        source_commit: source.base_commit.clone(),
        architecture: arch().into(),
    };
    let mut provenance = json!({"schema":1,"runtime":"rust","target":target,"llama_revision":manifest.llama_revision,"source_kind":manifest.source_kind,"libraries":libraries.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect::<Vec<_>>(),"source":source,"release_eligible":!development && !source.dirty});
    if os == "darwin" {
        compile_mac_icons(root, &resources, &mut provenance)?;
        write_mac_info(&bundle, &version)?;
        rebase_macos(&binaries, &libraries, &lib_dir)?;
    } else if os == "linux" {
        for binary in binaries.iter().chain(libraries.iter()) {
            if binary.file_name().and_then(|p| p.to_str()) == Some("BroTypist-updater") {
                continue;
            }
            let rpath = if binary.parent() == Some(lib_dir.as_path()) {
                "$ORIGIN"
            } else {
                "$ORIGIN/lib"
            };
            run(Command::new("patchelf")
                .arg("--set-rpath")
                .arg(rpath)
                .arg(binary))?;
        }
        prepare_linux(root, &bundle, output)?;
    } else {
        fs::copy(
            root.join("crates/brotypist/assets/app.ico"),
            resources.join("app.ico"),
        )?;
    }
    let licenses = if os == "darwin" {
        resources.join("licenses")
    } else {
        bundle.join("licenses")
    };
    crate::notices::collect(root, &target, &licenses)?;
    json_new(&marker(&bundle, os), &identity)?;
    json_new(&resources.join("native-provenance.json"), &provenance)?;
    inventory(&bundle)?;
    ensure!(
        source_state(root)? == source,
        "source changed while compiling/assembling; candidate remains nonpromotable in staging"
    );
    if development {
        json_new(
            &resources.join(".inklow-development.json"),
            &json!({"kind":"local-development-not-release","base_commit":source.base_commit,"dirty_source_digest":source.source_digest,"release_eligible":false}),
        )?;
    }
    if os == "darwin" {
        sign_macos(root, &bundle, &binaries, &libraries)?;
    }
    crate::package::inspect(root, &bundle, development)?;
    promote(&bundle, output, replace, os)
}

fn write_mac_info(bundle: &Path, version: &str) -> Result<()> {
    let partial = bundle.join("Contents/Resources/icon-info.plist");
    let mut dict = if partial.exists() {
        plist::Value::from_file(&partial)?
            .as_dictionary()
            .context("actool metadata must be a dictionary")?
            .clone()
    } else {
        plist::Dictionary::new()
    };
    for (key, value) in [
        ("CFBundleIdentifier", APP_ID),
        ("CFBundleExecutable", "BroTypist"),
        ("CFBundlePackageType", "APPL"),
        ("CFBundleName", "BroTypist"),
        ("CFBundleDisplayName", "BroTypist"),
        ("CFBundleShortVersionString", version),
        ("CFBundleVersion", version),
        ("CFBundleIconName", "BroTypist"),
        ("CFBundleIconFile", "app.icns"),
        ("NSPrincipalClass", "NSApplication"),
        ("LSMinimumSystemVersion", "15.0"),
        (
            "NSAppleEventsUsageDescription",
            "With your permission, BroTypist reads the active Terminal or iTerm session and font metadata to align local suggestions. It does not read shell history or run commands.",
        ),
        (
            "NSHumanReadableCopyright",
            "BroTypist — independent, local-first predictive typing",
        ),
    ] {
        dict.insert(key.into(), plist::Value::String(value.into()));
    }
    for (key, value) in [
        ("NSHighResolutionCapable", true),
        ("NSAppleScriptEnabled", false),
        ("LSUIElement", true),
        ("LSFileQuarantineEnabled", true),
    ] {
        dict.insert(key.into(), plist::Value::Boolean(value));
    }
    let mut url = plist::Dictionary::new();
    url.insert(
        "CFBundleURLName".into(),
        plist::Value::String("BroTypist Settings".into()),
    );
    url.insert(
        "CFBundleURLSchemes".into(),
        plist::Value::Array(vec![plist::Value::String("brotypist".into())]),
    );
    dict.insert(
        "CFBundleURLTypes".into(),
        plist::Value::Array(vec![plist::Value::Dictionary(url)]),
    );
    plist::Value::Dictionary(dict).to_file_xml(bundle.join("Contents/Info.plist"))?;
    Ok(())
}
fn compile_mac_icons(root: &Path, resources: &Path, provenance: &mut Value) -> Result<()> {
    let version = run(Command::new("/usr/bin/xcodebuild").arg("-version"))?;
    ensure!(
        version.lines().next() == Some("Xcode 27.1"),
        "Icon Composer requires pinned Xcode 27.1; older hosted actool only supported macOS15.5 and failed deployment26 compilation; select compatible DEVELOPER_DIR"
    );
    let partial = resources.join("icon-info.plist");
    run(Command::new("/usr/bin/xcrun")
        .arg("actool")
        .arg(root.join("design/icons/BroTypist.icon"))
        .arg("--compile")
        .arg(resources)
        .args([
            "--platform",
            "macosx",
            "--target-device",
            "mac",
            "--minimum-deployment-target",
            "26.0",
            "--app-icon",
            "BroTypist",
            "--output-partial-info-plist",
        ])
        .arg(&partial)
        .args([
            "--output-format",
            "human-readable-text",
            "--warnings",
            "--notices",
        ]))?;
    let info = plist::Value::from_file(&partial)?;
    ensure!(
        info.as_dictionary()
            .and_then(|d| d.get("CFBundleIconName"))
            .and_then(plist::Value::as_string)
            == Some("BroTypist"),
        "actool icon metadata mismatch"
    );
    regular(&resources.join("Assets.car"))?;
    fs::copy(
        root.join("crates/brotypist/assets/app.icns"),
        resources.join("app.icns"),
    )?;
    let sources = inventory(&root.join("design/icons/BroTypist.icon"))?;
    provenance["icon_compiler"] = json!({"xcode":version,"asset_minimum":"26.0","runtime_minimum":"15.0","classic_fallback":"app.icns","source_files":sources});
    Ok(())
}
fn mac_dependencies(binary: &Path) -> Result<Vec<String>> {
    let output = run(Command::new("/usr/bin/otool").arg("-L").arg(binary))?;
    Ok(output
        .lines()
        .skip(1)
        .filter_map(|line| line.trim().split(" (compatibility version").next())
        .map(str::to_owned)
        .collect())
}
fn rebase_macos(binaries: &[PathBuf], libraries: &[PathBuf], lib_dir: &Path) -> Result<()> {
    let names: BTreeSet<_> = libraries
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    for binary in binaries.iter().chain(libraries) {
        if binary.file_name().and_then(|p| p.to_str()) == Some("BroTypist-updater") {
            for dependency in mac_dependencies(binary)? {
                ensure!(
                    dependency.starts_with("/usr/lib/")
                        || dependency.starts_with("/System/Library/"),
                    "standalone updater has non-system dependency {dependency}"
                );
            }
            continue;
        }
        if binary.parent() == Some(lib_dir) {
            run(Command::new("/usr/bin/install_name_tool")
                .arg("-id")
                .arg(format!(
                    "@rpath/{}",
                    binary.file_name().unwrap().to_string_lossy()
                ))
                .arg(binary))?;
        }
        for dependency in mac_dependencies(binary)? {
            let name = Path::new(&dependency)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy();
            if names.contains(name.as_ref()) {
                let rebased = format!("@rpath/{name}");
                if dependency != rebased {
                    run(Command::new("/usr/bin/install_name_tool")
                        .arg("-change")
                        .arg(&dependency)
                        .arg(&rebased)
                        .arg(binary))?;
                }
            } else {
                ensure!(
                    dependency.starts_with("/usr/lib/")
                        || dependency.starts_with("/System/Library/"),
                    "unbundled non-system dependency {dependency}"
                );
            }
        }
        let loads = run(Command::new("/usr/bin/otool").arg("-l").arg(binary))?;
        let mut next_path = false;
        let mut rpaths = BTreeSet::new();
        for line in loads.lines() {
            if line.trim() == "cmd LC_RPATH" {
                next_path = true;
            } else if next_path && line.trim().starts_with("path ") {
                rpaths.insert(
                    line.trim()
                        .trim_start_matches("path ")
                        .split(" (offset")
                        .next()
                        .unwrap()
                        .to_owned(),
                );
                next_path = false;
            }
        }
        let desired = if binary.parent() == Some(lib_dir) {
            "@loader_path"
        } else {
            "@executable_path/../Frameworks"
        };
        for path in &rpaths {
            if path != desired {
                run(Command::new("/usr/bin/install_name_tool")
                    .arg("-delete_rpath")
                    .arg(path)
                    .arg(binary))?;
            }
        }
        if !rpaths.contains(desired) {
            run(Command::new("/usr/bin/install_name_tool")
                .arg("-add_rpath")
                .arg(desired)
                .arg(binary))?;
        }
    }
    Ok(())
}
fn sign_macos(
    root: &Path,
    bundle: &Path,
    binaries: &[PathBuf],
    libraries: &[PathBuf],
) -> Result<()> {
    let identity = std::env::var("INKLOW_CODESIGN_IDENTITY").context("INKLOW_CODESIGN_IDENTITY must explicitly select the existing authorized signing identity (or '-' for a deliberate local ad-hoc build)")?;
    ensure!(!identity.trim().is_empty(), "empty code signing identity");
    for library in libraries {
        run(Command::new("/usr/bin/codesign")
            .args(["--force", "--sign"])
            .arg(&identity)
            .arg(library))?;
    }
    for binary in binaries {
        let mut command = Command::new("/usr/bin/codesign");
        command
            .args(["--force", "--sign"])
            .arg(&identity)
            .args(["--options", "runtime"]);
        if binary.file_name().and_then(|p| p.to_str()) != Some("BroTypist-updater") {
            command
                .arg("--entitlements")
                .arg(root.join("packaging/macos/entitlements.plist"));
        }
        run(command.arg(binary))?;
    }
    run(Command::new("/usr/bin/codesign")
        .args(["--force", "--sign"])
        .arg(&identity)
        .args([
            "--identifier",
            APP_ID,
            "--options",
            "runtime",
            "--entitlements",
        ])
        .arg(root.join("packaging/macos/entitlements.plist"))
        .arg(bundle))?;
    run(Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(bundle))?;
    Ok(())
}
pub fn desktop_value(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}
pub fn desktop_exec(path: &Path) -> String {
    let mut value = path.to_string_lossy().replace('%', "%%");
    for c in ['\\', '"', '`', '$'] {
        value = value.replace(c, &format!("\\{c}"));
    }
    desktop_value(&format!("\"{value}\""))
}
pub fn linux_entries(root: &Path, applications: &Path, exec: &str, icon: &str) -> Result<()> {
    fs::create_dir_all(applications)?;
    for name in [
        "com.inklow.Inklow.desktop",
        "com.inklow.Inklow.InputMethod.desktop",
    ] {
        let template = fs::read_to_string(root.join("packaging/linux").join(format!("{name}.in")))?;
        fs::write(
            applications.join(name),
            template.replace("@EXEC@", exec).replace("@ICON@", icon),
        )?;
    }
    Ok(())
}
fn prepare_linux(root: &Path, bundle: &Path, destination: &Path) -> Result<()> {
    fs::copy(
        root.join("packaging/linux/launch-brotypist"),
        bundle.join("launch-brotypist"),
    )?;
    executable(&bundle.join("launch-brotypist"))?;
    for size in [16, 24, 32, 48, 64, 128, 256, 512, 1024] {
        let target = bundle.join(format!(
            "share/icons/hicolor/{size}x{size}/apps/{APP_ID}.png"
        ));
        fs::create_dir_all(target.parent().unwrap())?;
        fs::copy(
            root.join(format!("crates/brotypist/assets/icons/app-{size}.png")),
            target,
        )?;
    }
    linux_entries(
        root,
        &bundle.join("share/applications"),
        &desktop_exec(&destination.join("launch-brotypist")),
        &desktop_value(
            &destination
                .join(format!("share/icons/hicolor/256x256/apps/{APP_ID}.png"))
                .to_string_lossy(),
        ),
    )?;
    fs::copy(
        bundle.join(format!("share/applications/{APP_ID}.desktop")),
        bundle.join("BroTypist.desktop"),
    )?;
    executable(&bundle.join("BroTypist.desktop"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn desktop_exec_is_literal() {
        assert_eq!(
            desktop_exec(Path::new("/tmp/A %f $b\"")),
            "\"/tmp/A %%f \\\\$b\\\\\"\""
        );
    }
    #[test]
    fn desktop_method_selection_is_explicit_and_not_autostart() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let t = tempfile::tempdir().unwrap();
        linux_entries(&root, t.path(), "/usr/bin/brotypist", APP_ID).unwrap();
        let method =
            fs::read_to_string(t.path().join(format!("{APP_ID}.InputMethod.desktop"))).unwrap();
        assert!(method.contains("--wayland-input-method --background"));
        assert!(method.contains("X-Inklow-Owned=true"));
        assert!(method.contains("NoDisplay=true"));
        assert!(!method.contains("Autostart"));
    }
    #[test]
    fn mac_identity_has_security_and_navigation_metadata() {
        let t = tempfile::tempdir().unwrap();
        fs::create_dir_all(t.path().join("Contents")).unwrap();
        write_mac_info(t.path(), "0.4.13").unwrap();
        let p = plist::Value::from_file(t.path().join("Contents/Info.plist")).unwrap();
        let d = p.as_dictionary().unwrap();
        assert_eq!(d["CFBundleIdentifier"].as_string(), Some(APP_ID));
        assert_eq!(d["LSUIElement"].as_boolean(), Some(true));
        assert_eq!(d["LSFileQuarantineEnabled"].as_boolean(), Some(true));
        assert_eq!(d["LSMinimumSystemVersion"].as_string(), Some("15.0"));
        assert!(
            d["NSAppleEventsUsageDescription"]
                .as_string()
                .unwrap()
                .contains("does not read shell history")
        );
    }
    #[test]
    fn actool_partial_metadata_is_preserved_but_classic_icon_is_authoritative() {
        let t = tempfile::tempdir().unwrap();
        fs::create_dir_all(t.path().join("Contents/Resources")).unwrap();
        let mut generated = plist::Dictionary::new();
        generated.insert(
            "NativeCompilerMetadata".into(),
            plist::Value::String("preserved".into()),
        );
        generated.insert(
            "CFBundleIconFile".into(),
            plist::Value::String("synthesized.icns".into()),
        );
        plist::Value::Dictionary(generated)
            .to_file_xml(t.path().join("Contents/Resources/icon-info.plist"))
            .unwrap();
        write_mac_info(t.path(), "0.4.13").unwrap();
        let value = plist::Value::from_file(t.path().join("Contents/Info.plist")).unwrap();
        let dict = value.as_dictionary().unwrap();
        assert_eq!(
            dict["NativeCompilerMetadata"].as_string(),
            Some("preserved")
        );
        assert_eq!(dict["CFBundleIconFile"].as_string(), Some("app.icns"));
        let urls = dict["CFBundleURLTypes"].as_array().unwrap();
        let schemes = urls[0].as_dictionary().unwrap()["CFBundleURLSchemes"]
            .as_array()
            .unwrap();
        assert_eq!(schemes[0].as_string(), Some("brotypist"));
    }
}
