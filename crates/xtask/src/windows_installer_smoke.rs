//! Disposable Windows CI installer evidence, deliberately distinct from a full release gate.
use crate::{common::*, package};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};

const RECEIPT: &str = "package-receipt-win32-x86_64.json";

pub fn run(bundle: &Path, assets: &Path, output: &Path) -> Result<()> {
    fs::create_dir_all(output)?;
    real_dir(output)?;
    let output = output.canonicalize()?;
    let report = output.join("windows-installer-smoke.json");
    ensure!(
        !report.try_exists()?,
        "refusing to replace installer evidence"
    );
    let mut receipt = json!({
        "schema": 1, "tool": "windows-core-installer-smoke", "runtime": "rust",
        "passed": false, "full_installer_matrix_passed": false,
        "scope": "fresh per-user MSI install, repair, relocated portable ZIP, installed CLI/doctor, visible native Quit, uninstall and synthetic-data continuity; not a complete production installer matrix",
        "signing": "unsigned", "publication": "held; Windows publication and OTA remain disabled",
        "interactive_typing_verified": false,
        "required_gates": {
            "upgrade": {"status":"not_exercised", "reason":"no genuine prior canonical package supplied"},
            "rollback": {"status":"not_exercised", "reason":"no genuine rollback-failure fixture supplied"}
        }
    });
    let result = execute(bundle, assets, &output, &mut receipt);
    receipt["passed"] = json!(result.is_ok());
    if result.is_err() {
        // Child diagnostics are fixed/bounded/redacted by the owned script. Never
        // serialize arbitrary subprocess output, settings or model text here.
        receipt["failure"] =
            json!("Windows installer smoke failed; inspect observations and redacted diagnostics");
    }
    json_new(&report, &receipt)?;
    result
}

fn execute(bundle: &Path, assets: &Path, output: &Path, receipt: &mut Value) -> Result<()> {
    receipt["stage"] = json!("disposable_native_ci_guard");
    ensure!(
        cfg!(target_os = "windows") && arch() == "x86_64",
        "native Windows x86_64 required"
    );
    ensure!(
        std::env::var("BROTYPIST_INSTALLER_SMOKE_DISPOSABLE").as_deref() == Ok("1")
            && std::env::var("CI").as_deref() == Ok("true")
            && std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true")
            && std::env::var("RUNNER_ENVIRONMENT").as_deref() == Ok("github-hosted")
            && std::env::var("RUNNER_OS").as_deref() == Ok("Windows"),
        "explicit disposable GitHub-hosted Windows CI guard required"
    );
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let bundle = bundle.canonicalize()?;
    real_dir(assets)?;
    let assets = assets.canonicalize()?;
    ensure!(
        !output.starts_with(&bundle) && !output.starts_with(&assets),
        "evidence output must be separate from payload and artifacts"
    );
    receipt["stage"] = json!("canonical_clean_package_and_native_dependency_audit");
    let inspected = package::inspect(&root, &bundle, false)?;
    let package = verified_package(&assets)?;
    for field in [
        "application",
        "version",
        "source_commit",
        "architecture",
        "files",
        "native_provenance",
    ] {
        ensure!(
            package[field] == inspected[field],
            "canonical package and inspected bundle differ: {field}"
        );
    }
    ensure!(
        package["product_code"]
            == package::stable_guid(&format!("product/{}", package["version"].as_str().unwrap()))
            && package["upgrade_code"] == package::stable_guid("upgrade"),
        "Windows installer family identity differs"
    );
    receipt["version"] = package["version"].clone();
    receipt["source_commit"] = package["source_commit"].clone();
    receipt["package_sha256"] = json!(hash(&assets.join(RECEIPT))?);
    receipt["pe_native_dll_closure"] = json!({
        "passed":true, "method":"canonical bundle x86_64 PE and dumpbin dependency audit plus exact portable ZIP content; installed MSI payload is asserted separately by the native lifecycle probe",
        "development_bundle":false, "portable_zip_exact_content":true
    });
    receipt["stage"] = json!("optional_genuine_fixture_and_worker_preflight");
    let previous_dir = assets.join("upgrade-from");
    let previous = if previous_dir.try_exists()? {
        let previous = verified_package(&previous_dir)?;
        let current_version = validate_version(package["version"].as_str().unwrap())?;
        let prior_version = validate_version(
            previous["version"]
                .as_str()
                .context("prior version missing")?,
        )?;
        ensure!(
            prior_version < current_version
                && previous["upgrade_code"] == package["upgrade_code"]
                && previous["product_code"] != package["product_code"],
            "upgrade fixture must be a genuine older package in the same installer family"
        );
        Some(previous)
    } else {
        None
    };
    let worker = std::env::var_os("BROTYPIST_INSTALLER_SMOKE_WORKER_EXE").map(PathBuf::from);
    let model = std::env::var_os("BROTYPIST_INSTALLER_SMOKE_MODEL_PATH").map(PathBuf::from);
    ensure!(
        worker.is_some() == model.is_some(),
        "Tiny worker smoke executable and cached model must be supplied together"
    );
    let worker = worker.map(|path| path.canonicalize()).transpose()?;
    let model = model.map(|path| path.canonicalize()).transpose()?;
    if let (Some(worker), Some(model)) = (&worker, &model) {
        package::verify_binary(worker, "win32", "x86_64")?;
        regular(model)?;
    }
    // No rollback-failure MSI exists in the canonical package contract. Do not
    // synthesize failure properties or silently ignore a supplied fixture.
    ensure!(
        !assets.join("rollback-fixture.json").try_exists()?,
        "rollback fixture needs an explicitly reviewed native fixture contract"
    );
    let work = stage(output, "windows-installer-smoke")?;
    let input = work.join("input.json");
    json_new(
        &input,
        &json!({"package":package, "previous":previous,
        "worker":worker, "model":model}),
    )?;
    let script = root.join(".github/native/windows-installer-smoke.ps1");
    regular(&script)?;
    receipt["stage"] = json!("native_per_user_installer_lifecycle");
    let status = Command::new("pwsh.exe")
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-File"])
        .arg(&script)
        .arg("-InputFile")
        .arg(&input)
        .arg("-Work")
        .arg(&work)
        .stdin(std::process::Stdio::null())
        .status()
        .context("cannot invoke owned native installer probe")?;
    let observed_path = work.join("observations.json");
    regular(&observed_path).context("native installer probe did not produce evidence")?;
    ensure!(
        fs::metadata(&observed_path)?.len() <= 256 * 1024,
        "native installer evidence exceeds limit"
    );
    let observed: Value = serde_json::from_slice(&fs::read(observed_path)?)?;
    if let Some(gates) = observed.get("required_gates") {
        receipt["required_gates"] = gates.clone();
    }
    receipt["observations"] = observed.clone();
    ensure!(
        status.success() && observed["passed"] == true,
        "native installer assertions failed"
    );
    receipt["stage"] = json!("completed_core_scope_only");
    Ok(())
}

fn verified_package(directory: &Path) -> Result<Value> {
    real_dir(directory)?;
    let path = directory.join(RECEIPT);
    regular(&path)?;
    ensure!(
        fs::metadata(&path)?.len() < 8 * 1024 * 1024,
        "package receipt exceeds limit"
    );
    let mut value: Value = serde_json::from_slice(&fs::read(&path)?)?;
    ensure!(
        value["application"] == APP_ID
            && value["platform"] == "win32"
            && value["runtime"] == "rust"
            && value["architecture"] == "x86_64"
            && value["scope"] == "perUser"
            && value["signing"] == "unsigned"
            && value["release_eligible"] == true
            && value["native_provenance"]["release_eligible"] == true
            && value["native_provenance"]["source"]["dirty"] == false,
        "expected clean canonical unsigned per-user Rust package"
    );
    validate_version(value["version"].as_str().context("version missing")?)?;
    validate_commit(value["source_commit"].as_str().context("source missing")?)?;
    ensure!(
        value["product_code"]
            == package::stable_guid(&format!("product/{}", value["version"].as_str().unwrap()))
            && value["upgrade_code"] == package::stable_guid("upgrade"),
        "foreign MSI installer family"
    );
    for kind in ["msi", "zip"] {
        let recorded = PathBuf::from(
            value["artifacts"][kind]["path"]
                .as_str()
                .context("artifact path missing")?,
        );
        // Receipts remain usable after CI artifact download/relocation, but may
        // only resolve the exact named artifact alongside this receipt.
        let name = recorded.file_name().context("artifact basename missing")?;
        let artifact = directory.join(name);
        regular(&artifact)?;
        ensure!(
            value["artifacts"][kind]["sha256"] == hash(&artifact)?
                && value["artifacts"][kind]["size"] == fs::metadata(&artifact)?.len(),
            "artifact bytes differ from canonical receipt"
        );
        value["artifacts"][kind]["path"] = json!(artifact.canonicalize()?);
    }
    let rows: Vec<Row> = serde_json::from_value(value["files"].clone())?;
    verify_zip(
        Path::new(value["artifacts"]["zip"]["path"].as_str().unwrap()),
        &rows,
    )?;
    Ok(value)
}

fn verify_zip(path: &Path, rows: &[Row]) -> Result<()> {
    let mut expected = BTreeMap::new();
    for row in rows {
        ensure!(
            safe_relative(Path::new(&row.path)) && !row.path.contains('\\'),
            "unsafe package inventory"
        );
        ensure!(
            expected
                .insert(format!("BroTypist/{}", row.path), row)
                .is_none(),
            "duplicate package file"
        );
    }
    let mut archive = zip::ZipArchive::new(File::open(path)?)?;
    ensure!(
        archive.len() == expected.len() && !expected.is_empty(),
        "portable ZIP file count differs"
    );
    let mut buffer = [0u8; 65536];
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        ensure!(
            !entry.is_dir()
                && entry.enclosed_name().is_some()
                && !entry.name().contains('\\')
                && safe_relative(Path::new(entry.name())),
            "unsafe portable ZIP path"
        );
        ensure!(
            entry
                .unix_mode()
                .is_none_or(|mode| mode & 0o170000 == 0 || mode & 0o170000 == 0o100000),
            "portable ZIP contains a link/special entry"
        );
        let row = expected
            .remove(entry.name())
            .context("unexpected or duplicate ZIP member")?;
        ensure!(
            entry.size() == row.size,
            "portable ZIP entry length differs"
        );
        let mut digest = Sha256::new();
        let mut read = 0u64;
        loop {
            let count = entry.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            read += count as u64;
            ensure!(
                read <= row.size,
                "portable ZIP expands beyond declared length"
            );
            digest.update(&buffer[..count]);
        }
        ensure!(
            read == row.size && format!("{:x}", digest.finalize()) == row.sha256,
            "portable ZIP content differs"
        );
    }
    ensure!(expected.is_empty(), "portable ZIP omitted payload");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn portable_zip_rejects_extra_and_traversal_members() {
        let temp = tempfile::tempdir().unwrap();
        let zip_path = temp.path().join("payload.zip");
        let row = Row {
            path: "asset".into(),
            size: 3,
            sha256: format!("{:x}", Sha256::digest(b"abc")),
        };
        for name in [
            "BroTypist/asset",
            "BroTypist/../asset",
            "BroTypist/asset:stream",
            "elsewhere/asset",
        ] {
            let mut archive = zip::ZipWriter::new(File::create(&zip_path).unwrap());
            archive
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            std::io::Write::write_all(&mut archive, b"abc").unwrap();
            archive.finish().unwrap();
            assert_eq!(
                verify_zip(&zip_path, std::slice::from_ref(&row)).is_ok(),
                name == "BroTypist/asset"
            );
        }
    }
}
