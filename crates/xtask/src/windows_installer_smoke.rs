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
        .arg(&*external_tool_path(&script))
        .arg("-InputFile")
        .arg(&*external_tool_path(&input))
        .arg("-Work")
        .arg(&*external_tool_path(&work))
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
    receipt["portable_runtime_passed"] = json!(observed["portable_runtime_passed"] == true);
    // Retain the canonical worker proof outside private staging even when the
    // independent MSI registration gate fails afterwards.
    for name in ["portable-tiny-worker", "tiny-worker"] {
        let worker_report = work.join(name).join("worker-smoke.json");
        if worker_report.try_exists()? {
            regular(&worker_report)?;
            ensure!(
                fs::metadata(&worker_report)?.len() <= 256 * 1024,
                "worker evidence exceeds limit"
            );
            let worker_observed: Value = serde_json::from_slice(&fs::read(&worker_report)?)?;
            json_new(&output.join(format!("{name}.json")), &worker_observed)?;
        }
    }
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
        let canonical = artifact.canonicalize()?;
        value["artifacts"][kind]["path"] = json!(&*external_tool_path(&canonical));
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
    #[cfg(target_os = "windows")]
    #[test]
    fn historical_checkout_binding_requires_exact_initial_checkout_sha() {
        let temp = tempfile::tempdir().unwrap();
        let script = temp.path().join("checkout-fixture.ps1");
        fs::write(
            &script,
            r#"
param([string]$Source, [string]$Work)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$tokens = $null; $errors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile($Source, [ref]$tokens, [ref]$errors)
if ($errors.Count -ne 0) { throw 'Adapter syntax errors' }
foreach ($name in @('Require', 'Read-CheckoutIdentity')) {
    $definition = $ast.Find({ param($node) $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -ceq $name }, $true)
    Invoke-Expression $definition.Extent.Text
}
$sha = '3a47450006b9ec09bffab38ca23465441c38e502'
$other = '0000000000000000000000000000000000000000'
$prefix = '2026-10-31T22:17:42.2047551Z '
$start = $prefix + '##[group]Run actions/checkout@v6'
$fetch = $prefix + '[command]"C:\Program Files\Git\bin\git.exe" -c protocol.version=2 fetch --depth=1 origin +' + $sha + ':refs/remotes/pull/1/merge'
$command = $prefix + '[command]"C:\Program Files\Git\bin\git.exe" log -1 --format=%H'
$finish = $prefix + '##[group]Run build'
$valid = @($start, $fetch, $command, ($prefix + $sha), $finish)
$path = Join-Path $Work 'checkout.log'
function Parse-Fixture([string[]]$Lines) {
    [IO.File]::WriteAllText($path, ($Lines -join "`n"))
    Read-CheckoutIdentity $path
}
$identity = Parse-Fixture $valid
if ($identity.commit -cne $sha -or $identity.pull_request -cne '1') { throw 'Historical exact checkout lost' }
# Later build output cannot replace the original checkout identity.
$identity = Parse-Fixture ($valid + @($command, ($prefix + $other)))
if ($identity.commit -cne $sha) { throw 'Later log output replaced checkout identity' }
$rejected = 0
foreach ($fixture in @(
    @{ lines = @($start, $fetch, $command, ($prefix + $other), $finish) },
    @{ lines = @($start, $fetch, $finish) },
    @{ lines = @($fetch, $command, ($prefix + $sha), $finish) },
    @{ lines = @($start, $fetch, $fetch, $command, ($prefix + $sha), $finish) },
    @{ lines = @($start, $fetch, $finish, $command, ($prefix + $sha)) }
)) {
    try { $null = Parse-Fixture $fixture.lines } catch { $rejected++ }
}
if ($rejected -ne 5) { throw 'Checkout missing/mismatched/duplicate/out-of-step evidence accepted' }
"#,
        )
        .unwrap();
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.github/native/windows-installer-reuse.ps1");
        let status = Command::new("pwsh.exe")
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-File"])
            .arg(script)
            .arg(source)
            .arg(temp.path())
            .status()
            .unwrap();
        assert!(status.success());
    }
    #[cfg(target_os = "windows")]
    #[test]
    fn routing_evidence_retains_owned_hives_without_raw_log_content() {
        let temp = tempfile::tempdir().unwrap();
        let script = temp.path().join("scope-fixture.ps1");
        // Load only the parser's function AST: never execute the installer probe.
        // Both native roots must survive; other products, properties and paths must not.
        fs::write(
            &script,
            r#"
param([string]$Source, [string]$Work)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$tokens = $null; $errors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile($Source, [ref]$tokens, [ref]$errors)
if ($errors.Count -ne 0) { throw 'Probe syntax errors' }
$definition = $ast.Find({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -ceq 'Observe-MsiRouting' }, $true)
Invoke-Expression $definition.Extent.Text
$products = @('{ED0E8FF5-4909-5BF5-A22D-D8F924EB7C88}')
$receipt = @{ observations = [Collections.Generic.List[object]]::new() }
$path = Join-Path $Work 'fixture.log'
$fixture = @'
Action start 22:34:44: RegisterProduct.
MSI (s) (04:E0): Executing op: RegOpenKey(Root=-2147483646,Key=Software\Microsoft\Windows\CurrentVersion\Uninstall\{ED0E8FF5-4909-5BF5-A22D-D8F924EB7C88},BinaryType=1,)
MSI (s) (04:E0): Executing op: RegOpenKey(Root=-2147483647,Key=Software\Microsoft\Windows\CurrentVersion\Uninstall\{ED0E8FF5-4909-5BF5-A22D-D8F924EB7C88},BinaryType=1,)
MSI (s) (04:E0): Executing op: RegOpenKey(Root=-2147483646,Key=Software\Microsoft\Windows\CurrentVersion\Uninstall\{00000000-0000-0000-0000-000000000000},BinaryType=1,)
Property(S): ALLUSERS = 1
Property(S): MSIINSTALLPERUSER = 1
Property(S): PRIVATE = never-retain-this-value
MSI (s): C:\Users\never-retain-this-path\payload
'@
[IO.File]::WriteAllText($path, $fixture, [Text.Encoding]::Unicode)
Observe-MsiRouting 'fixture' $path
$facts = @($receipt.observations[0].evidence.facts)
if ($facts.Count -ne 5) { throw 'Owned routing facts missing or unrelated content retained' }
if (@($facts | Where-Object { $_.event -eq 'owned_arp_reg_open' }).Count -ne 2) { throw 'Native hive distinction lost' }
$json = $receipt | ConvertTo-Json -Depth 16
if ($json.Contains('never-retain') -or $json.Contains('00000000-0000')) { throw 'Private or unrelated log content retained' }
[IO.File]::WriteAllText($path, ('Property(S): ALLUSERS = 1' + "`n") * 300)
Observe-MsiRouting 'bounded' $path
if (-not $receipt.observations[1].evidence.truncated -or @($receipt.observations[1].evidence.facts).Count -ne 256) { throw 'Routing fact limit failed' }
if ($null -ne $receipt.observations[1].passed) { throw 'Diagnostic observation was promoted to passing gate' }
"#,
        )
        .unwrap();
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.github/native/windows-installer-smoke.ps1");
        let status = Command::new("pwsh.exe")
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-File"])
            .arg(script)
            .arg(source)
            .arg(temp.path())
            .status()
            .unwrap();
        assert!(status.success());
    }
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
