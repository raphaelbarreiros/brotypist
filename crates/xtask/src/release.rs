use crate::common::*;
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, VerifyingKey};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use crate::update_keys::{PUBLISHER_PUBLIC_KEY as PUBLIC_KEY, REPOSITORY};
const TARGETS: [&str; 3] = ["macos-arm64", "windows-x86_64", "linux-x86_64"];

pub fn endings(target: &str) -> Result<[&'static str; 2]> {
    match target {
        "macos-arm64" => Ok(["macos-arm64.dmg", "macos-arm64.zip"]),
        "linux-x86_64" => Ok(["linux-amd64.deb", "linux-amd64.tar.gz"]),
        "windows-x86_64" => Ok(["windows-x86_64.msi", "windows-x86_64.zip"]),
        _ => bail!("unsupported release target {target}"),
    }
}
fn native_target_name() -> Result<&'static str> {
    match (platform()?, arch()) {
        ("darwin", "arm64") => Ok("macos-arm64"),
        ("linux", "x86_64") => Ok("linux-x86_64"),
        ("win32", "x86_64") => Ok("windows-x86_64"),
        _ => bail!("no publisher target for native host"),
    }
}
fn passed(receipt: &Value) -> bool {
    if !receipt.is_object() {
        return false;
    }
    let mut present = false;
    for field in ["passed", "status", "result"] {
        if let Some(value) = receipt.get(field) {
            present = true;
            if if field == "passed" {
                value.as_bool() != Some(true)
            } else {
                value.as_str() != Some("passed")
            } {
                return false;
            }
        }
    }
    present
}
fn check_receipt(receipt: &Value, version: &str, commit: &str) -> Result<()> {
    ensure!(
        passed(receipt),
        "only passed native installer receipts can seal release assets"
    );
    if let Some(observed) = receipt.get("version") {
        ensure!(observed == version, "installer receipt version mismatch");
    }
    if let Some(observed) = receipt.get("source_commit") {
        ensure!(observed == commit, "installer receipt source mismatch");
    }
    ensure!(
        receipt.get("development").and_then(Value::as_bool) != Some(true),
        "development receipt is not release evidence"
    );
    Ok(())
}
fn asset_row(path: &Path) -> Result<Value> {
    regular(path)?;
    let size = fs::metadata(path)?.len();
    ensure!(size > 0, "empty native asset");
    Ok(
        json!({"name":path.file_name().unwrap().to_str().context("nonUTF8 asset filename")?,"size":size,"sha256":hash(path)?}),
    )
}
pub fn manifest(
    root: &Path,
    assets: &Path,
    target: &str,
    source: &str,
    receipt: &Path,
) -> Result<()> {
    validate_commit(source)?;
    require_clean_source(root)?;
    ensure!(
        source == commit(root)?,
        "manifest source must be actual clean checkout HEAD"
    );
    ensure!(
        native_target_name()? == target,
        "manifest sealing requires actual native target host, not cross-compilation"
    );
    let version = version(root)?;
    regular(receipt)?;
    let smoke: Value = serde_json::from_slice(&fs::read(receipt)?)?;
    check_receipt(&smoke, &version, source)?;
    let receipt_path = assets.join(format!("package-receipt-{}-{}.json", platform()?, arch()));
    regular(&receipt_path)?;
    let package: Value = serde_json::from_slice(&fs::read(receipt_path)?)?;
    ensure!(
        package["version"] == version
            && package["source_commit"] == source
            && package["runtime"] == "rust"
            && package["release_eligible"] == true,
        "package receipt must bind release-eligible Rust artifacts to clean source"
    );
    let mut rows = Vec::new();
    for ending in endings(target)? {
        let path = assets.join(format!("BroTypist-{version}-{ending}"));
        let row = asset_row(&path)?;
        let listed = package["artifacts"]
            .as_object()
            .context("package artifact map missing")?
            .values()
            .any(|v| {
                v["sha256"] == row["sha256"]
                    && v["size"] == row["size"]
                    && v["path"]
                        .as_str()
                        .and_then(|s| Path::new(s).file_name())
                        .and_then(|s| s.to_str())
                        == row["name"].as_str()
            });
        ensure!(listed, "native artifact differs from package receipt");
        rows.push(row);
    }
    json_new(
        &assets.join("build-manifest.json"),
        &json!({"version":version,"source_commit":source,"target":target,"artifacts":rows,"installer_smoke":smoke}),
    )?;
    Ok(())
}
pub fn require_clean_source(root: &Path) -> Result<()> {
    let status = run(std::process::Command::new("git").arg("-C").arg(root).args([
        "status",
        "--porcelain",
        "--untracked-files=normal",
    ]))?;
    ensure!(
        status.is_empty(),
        "release build/package/manifest requires a clean actual revision; use build --development only for local nonrelease UI smoke"
    );
    Ok(())
}
pub fn validate_inputs(
    root: &Path,
    inputs: &Path,
    source: &str,
    requested: &[String],
) -> Result<Vec<Value>> {
    validate_commit(source)?;
    require_clean_source(root)?;
    ensure!(
        commit(root)? == source,
        "release metadata source must be actual checkout HEAD"
    );
    let version = version(root)?;
    let set: BTreeSet<_> = requested.iter().map(String::as_str).collect();
    ensure!(
        !set.is_empty() && set.len() == requested.len(),
        "explicit unique targets required"
    );
    for target in &set {
        endings(target)?;
        ensure!(
            *target != "windows-x86_64",
            "Windows publication/OTA remains held; unsigned MSI may only be built and tested locally"
        );
    }
    real_dir(inputs)?;
    let mut native = Vec::new();
    let mut found = BTreeSet::new();
    let mut children: Vec<PathBuf> = fs::read_dir(inputs)?
        .map(|e| e.map(|v| v.path()))
        .collect::<std::io::Result<_>>()?;
    children.sort();
    for child in children {
        let metadata = fs::symlink_metadata(&child)?;
        ensure!(!linked(&metadata), "linked release input refused");
        if !metadata.is_dir() {
            continue;
        }
        let path = child.join("build-manifest.json");
        if !path.exists() {
            continue;
        }
        regular(&path)?;
        let value: Value = serde_json::from_slice(&fs::read(path)?)?;
        let target = value["target"]
            .as_str()
            .context("missing manifest target")?;
        ensure!(
            set.contains(target) && found.insert(target.to_owned()),
            "duplicate/unexpected native target"
        );
        ensure!(
            value["source_commit"] == source && value["version"] == version,
            "native packages built from different version/revision"
        );
        check_receipt(&value["installer_smoke"], &version, source)?;
        let expected: BTreeSet<_> = endings(target)?
            .into_iter()
            .map(|e| format!("BroTypist-{version}-{e}"))
            .collect();
        let artifacts = value["artifacts"]
            .as_array()
            .context("native artifact list missing")?;
        ensure!(artifacts.len() == 2, "unexpected native artifact count");
        let mut seen = BTreeSet::new();
        for row in artifacts {
            let name = row["name"].as_str().context("missing artifact name")?;
            ensure!(
                safe_relative(Path::new(name))
                    && Path::new(name).components().count() == 1
                    && expected.contains(name)
                    && seen.insert(name),
                "unexpected/unsafe/duplicate artifact name"
            );
            ensure!(
                *row == asset_row(&child.join(name))?,
                "transferred native asset failed size/hash verification"
            );
        }
        native.push(value);
    }
    ensure!(
        found.len() == set.len(),
        "successful native manifest required for every explicitly requested target"
    );
    native.sort_by(|a, b| a["target"].as_str().cmp(&b["target"].as_str()));
    Ok(native)
}
fn canonical(value: &Value, output: &mut Vec<u8>) -> Result<()> {
    match value {
        Value::Object(map) => {
            output.push(b'{');
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort();
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                output.extend(serde_json::to_vec(key)?);
                output.push(b':');
                canonical(&map[*key], output)?;
            }
            output.push(b'}');
        }
        Value::Array(values) => {
            output.push(b'[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                canonical(value, output)?;
            }
            output.push(b']');
        }
        _ => output.extend(serde_json::to_vec(value)?),
    }
    Ok(())
}
pub fn publisher_bytes(version: &str, source: &str, native: &[Value]) -> Result<Vec<u8>> {
    validate_version(version)?;
    validate_commit(source)?;
    let mut assets = Vec::new();
    for target in native {
        ensure!(
            target["target"] != "windows-x86_64",
            "held Windows OTA format"
        );
        for row in target["artifacts"]
            .as_array()
            .context("missing artifacts")?
        {
            let name = row["name"].as_str().context("missing asset name")?;
            let format = ["tar.gz", "zip", "deb", "dmg"]
                .into_iter()
                .find(|suffix| name.ends_with(&format!(".{suffix}")))
                .context("held/unsupported OTA format")?;
            let mut row = row.as_object().context("artifact must be object")?.clone();
            row.insert("target".into(), target["target"].clone());
            row.insert("format".into(), json!(format));
            assets.push(Value::Object(row));
        }
    }
    let value = json!({"schema":1,"application":APP_ID,"version":version,"source_commit":source,"assets":assets});
    let mut data = Vec::new();
    canonical(&value, &mut data)?;
    data.push(b'\n');
    Ok(data)
}
pub fn verify_signature(data: &[u8], signature: &[u8]) -> Result<()> {
    ensure!(signature.len() <= 1024, "oversized signature");
    let public: [u8; 32] = STANDARD
        .decode(PUBLIC_KEY)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid pinned public key"))?;
    let encoded = std::str::from_utf8(signature)?.trim();
    let signature = Signature::from_slice(&STANDARD.decode(encoded)?)?;
    VerifyingKey::from_bytes(&public)?.verify_strict(data,&signature).context("detached signature does not match existing publisher identity; no unsigned metadata produced")
}
pub fn prepare(
    root: &Path,
    inputs: &Path,
    output: &Path,
    source: &str,
    tag: &str,
    signature: &Path,
    targets: &[String],
) -> Result<()> {
    let native = validate_inputs(root, inputs, source, targets)?;
    let version = version(root)?;
    ensure!(
        tag == format!("v{version}"),
        "release tag differs from source version"
    );
    let data = publisher_bytes(&version, source, &native)?;
    regular(signature)?;
    let signature = fs::read(signature)?;
    verify_signature(&data, &signature)?;
    ensure!(
        fs::symlink_metadata(output).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
        "release output already exists"
    );
    let absolute = if output.is_absolute() {
        output.to_owned()
    } else {
        std::env::current_dir()?.join(output)
    };
    fs::create_dir_all(absolute.parent().context("missing release output parent")?)?;
    let work = stage(absolute.parent().unwrap(), "release")?;
    let mut paths = Vec::new();
    for target in &native {
        let target_name = target["target"].as_str().unwrap();
        let mut dir = None;
        for entry in fs::read_dir(inputs)? {
            let child = entry?.path();
            let path = child.join("build-manifest.json");
            if path.is_file() {
                let value: Value = serde_json::from_slice(&fs::read(path)?)?;
                if value["target"] == target_name {
                    dir = Some(child);
                    break;
                }
            }
        }
        let dir = dir.context("validated target disappeared")?;
        for row in target["artifacts"].as_array().unwrap() {
            let name = row["name"].as_str().unwrap();
            let dest = work.join(name);
            regular(&dir.join(name))?;
            fs::copy(dir.join(name), &dest)?;
            ensure!(
                asset_row(&dest)? == *row,
                "artifact changed during release preparation"
            );
            paths.push(dest);
        }
    }
    let unpublished: Vec<_> = TARGETS
        .into_iter()
        .filter(|v| !targets.iter().any(|t| t == v))
        .collect();
    let release = work.join("release-manifest.json");
    json_new(
        &release,
        &json!({"tag":tag,"source_commit":source,"repository":REPOSITORY,"created_at":chrono::Utc::now().to_rfc3339(),"signing":"Recorded in each native installer receipt; no distribution-signing or notarization claim.","targets":native,"unpublished_targets":unpublished}),
    )?;
    paths.push(release);
    let manifest = work.join("update-manifest.json");
    write_new(&manifest, &data)?;
    paths.push(manifest);
    let sig = work.join("update-manifest.sig");
    let mut normalized = std::str::from_utf8(&signature)?.trim().as_bytes().to_vec();
    normalized.push(b'\n');
    write_new(&sig, &normalized)?;
    paths.push(sig);
    paths.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    let mut sums = String::new();
    for path in paths {
        sums.push_str(&format!(
            "{}  {}\n",
            hash(&path)?,
            path.file_name().unwrap().to_str().unwrap()
        ));
    }
    write_new(&work.join("SHA256SUMS"), sums.as_bytes())?;
    // create_dir is exclusive; promote individual already-complete files without ever overwriting existing metadata.
    fs::create_dir(&absolute)?;
    for entry in fs::read_dir(&work)? {
        let path = entry?.path();
        publish_file(&path, &absolute.join(path.file_name().unwrap()))?;
    }
    fs::remove_dir_all(work)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_boolean_installer_receipts_and_conflicting_failures() {
        let source = "a".repeat(40);
        let receipt =
            json!({"passed":true,"version":"0.4.13","source_commit":source,"full_matrix":false});
        check_receipt(&receipt, "0.4.13", &source).unwrap();
        for value in [
            json!({"passed":false,"status":"passed"}),
            json!({"passed":true,"status":"failed"}),
            json!({"passed":"true"}),
        ] {
            assert!(check_receipt(&value, "0.4.13", &source).is_err());
        }
    }
    #[test]
    fn publisher_format_matches_python_sorted_compact_contract() {
        let data=publisher_bytes("0.4.13",&"a".repeat(40),&[json!({"target":"macos-arm64","artifacts":[{"name":"BroTypist-0.4.13-macos-arm64.zip","size":3,"sha256":"b".repeat(64)}]})]).unwrap();
        let expected = format!(
            "{{\"application\":\"com.inklow.Inklow\",\"assets\":[{{\"format\":\"zip\",\"name\":\"BroTypist-0.4.13-macos-arm64.zip\",\"sha256\":\"{}\",\"size\":3,\"target\":\"macos-arm64\"}}],\"schema\":1,\"source_commit\":\"{}\",\"version\":\"0.4.13\"}}\n",
            "b".repeat(64),
            "a".repeat(40)
        );
        assert_eq!(data, expected.as_bytes());
    }
    #[test]
    fn fake_signature_and_held_windows_refused() {
        assert!(verify_signature(b"bytes", STANDARD.encode([0u8; 64]).as_bytes()).is_err());
        assert!(
            publisher_bytes(
                "0.4.13",
                &"a".repeat(40),
                &[json!({"target":"windows-x86_64","artifacts":[]})]
            )
            .is_err()
        );
    }
    #[test]
    fn receipt_result_is_not_inferred() {
        assert!(!passed(&json!({"status":"failed","result":"passed"})));
        assert!(!passed(&json!({"result":"skipped"})));
        assert!(
            check_receipt(
                &json!({"status":"passed","source_commit":"b".repeat(40)}),
                "0.4.13",
                &"a".repeat(40)
            )
            .is_err()
        );
    }
    #[test]
    fn asset_hash_binds_bytes() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("BroTypist.zip");
        fs::write(&p, b"first").unwrap();
        let before = asset_row(&p).unwrap();
        fs::write(&p, b"other").unwrap();
        assert_ne!(before, asset_row(&p).unwrap());
    }
}
