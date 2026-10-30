//! Offline notice material for the production app and independent updater.
//! This records upstream evidence, not a legal certification or a first-party grant.
use crate::common::{LLAMA_REVISION, json_new, linked, real_dir, run, safe_relative, write_new};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};

const ROOT_PACKAGES: [&str; 2] = ["brotypist", "brotypist-updater"];
const MAX_DOCUMENT: u64 = 2 * 1024 * 1024;
const MAX_TOTAL: usize = 64 * 1024 * 1024;
const MAX_ENTRIES: usize = 100_000;

#[derive(Deserialize)]
struct Metadata {
    version: u32,
    workspace_root: PathBuf,
    workspace_members: Vec<String>,
    packages: Vec<Package>,
    resolve: Option<Resolve>,
}
#[derive(Deserialize)]
struct Package {
    id: String,
    name: String,
    version: String,
    source: Option<String>,
    manifest_path: PathBuf,
    license: Option<String>,
    license_file: Option<PathBuf>,
    #[serde(default)]
    dependencies: Vec<Dependency>,
    #[serde(default)]
    features: BTreeMap<String, Vec<String>>,
}
#[derive(Deserialize)]
struct Dependency {
    name: String,
    rename: Option<String>,
    kind: Option<String>,
    target: Option<String>,
    optional: bool,
    uses_default_features: bool,
    features: Vec<String>,
}
#[derive(Deserialize)]
struct Resolve {
    nodes: Vec<Node>,
}
#[derive(Deserialize)]
struct Node {
    id: String,
    deps: Vec<Edge>,
}
#[derive(Deserialize)]
struct Edge {
    name: String,
    pkg: String,
    dep_kinds: Vec<DependencyKind>,
}
#[derive(Deserialize)]
struct DependencyKind {
    kind: Option<String>,
    target: Option<String>,
}
#[derive(Deserialize)]
struct Lock {
    package: Vec<LockedPackage>,
}
#[derive(Deserialize)]
struct LockedPackage {
    name: String,
    version: String,
    source: Option<String>,
    checksum: Option<String>,
}

// Reviewed source-specific supplements, never a fallback selected by SPDX alone.
// Original bytes and provenance are compiled into xtask: collection stays offline.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewedMaterial {
    schema: u32,
    legal_certification: bool,
    limits: String,
    documents: Vec<ReviewedDocument>,
    packages: Vec<ReviewedPackage>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewedDocument {
    file: String,
    sha256: String,
    source: String,
    role: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReviewedPackage {
    name: String,
    version: String,
    source: String,
    checksum: String,
    declaration: String,
    chosen_license: String,
    upstream_source: String,
    upstream_package_path: Option<String>,
    declaration_source: String,
    source_evidence: Vec<SourceEvidence>,
    documents: Vec<String>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SourceEvidence {
    path: String,
    sha256: String,
}
#[derive(Serialize)]
struct ReviewedProvenance {
    legal_certification: bool,
    limits: String,
    binding: ReviewedPackage,
}

#[derive(Serialize)]
struct Document {
    upstream_path: String,
    path: String,
    size: usize,
    sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    upstream_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    excerpt: Option<String>,
}
#[derive(Serialize)]
struct ThirdParty {
    name: String,
    version: String,
    /// The upstream declaration is retained verbatim; it is not reinterpreted as a grant.
    spdx: Option<String>,
    source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    checksum: Option<String>,
    checksum_kind: String,
    documents: Vec<Document>,
    #[serde(skip_serializing_if = "Option::is_none")]
    chosen_license: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reviewed_material: Option<ReviewedProvenance>,
}
#[derive(Serialize)]
struct FirstParty {
    name: String,
    version: String,
    manifest: String,
    declared_license: Option<String>,
    status: String,
}
#[derive(Serialize)]
struct Inventory {
    schema: u32,
    target: String,
    cargo_lock_sha256: String,
    scope: &'static str,
    legal_status: &'static str,
    legal_certification: bool,
    first_party: Vec<FirstParty>,
    third_party: Vec<ThirdParty>,
}
struct Payload {
    path: PathBuf,
    bytes: Vec<u8>,
}
#[derive(Default)]
struct Material {
    payloads: Vec<Payload>,
    total: usize,
}

/// Create a new, exclusively owned licenses directory. The caller must invoke
/// this before signing and before generating the bundle's payload inventory.
/// Cargo and its locked source cache must already be available; no fetch occurs.
pub fn collect(root: &Path, target_triple: &str, output: &Path) -> Result<()> {
    ensure!(
        !target_triple.is_empty()
            && target_triple
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
        "invalid native target triple"
    );
    let root = root.canonicalize()?;
    real_dir(&root)?;
    let mut command = Command::new("cargo");
    command.current_dir(&root).args([
        "metadata",
        "--locked",
        "--offline",
        "--format-version",
        "1",
        "--filter-platform",
        target_triple,
    ]);
    // Match the packaging build's default feature selection, not a developer's
    // CARGO_BUILD_TARGET or command-line --all-features invocation.
    command.env_remove("CARGO_BUILD_TARGET");
    let text = run(&mut command).context("offline notice metadata unavailable; pre-populate the locked Cargo source cache before packaging")?;
    let metadata: Metadata = serde_json::from_str(&text)?;
    let native = root.join("crates/brotypist/vendor/llama.cpp");
    checked_path(&root, Path::new("crates/brotypist/vendor/llama.cpp"), true)?;
    let revision = run(Command::new("git")
        .arg("-C")
        .arg(&native)
        .args(["rev-parse", "HEAD"]))?;
    ensure!(
        revision == LLAMA_REVISION,
        "native notice prerequisite: llama.cpp checkout must match pinned revision {LLAMA_REVISION}"
    );
    let lock = read_bounded(
        &checked_path(&root, Path::new("Cargo.lock"), false)?,
        MAX_DOCUMENT,
    )?;
    collect_metadata(&root, target_triple, output, &metadata, &lock, &native)
}

fn collect_metadata(
    root: &Path,
    target: &str,
    output: &Path,
    metadata: &Metadata,
    lock_bytes: &[u8],
    native: &Path,
) -> Result<()> {
    ensure!(metadata.version == 1, "unsupported Cargo metadata schema");
    ensure!(
        metadata.workspace_root.canonicalize()? == root.canonicalize()?,
        "notice metadata belongs to a different workspace"
    );
    let selected = production_packages(metadata)?;
    let lock: Lock = toml::from_str(std::str::from_utf8(lock_bytes)?)?;
    let members: BTreeSet<_> = metadata
        .workspace_members
        .iter()
        .map(String::as_str)
        .collect();
    let mut material = Material::default();
    let reviewed: ReviewedMaterial =
        serde_json::from_str(include_str!("../legal-material/manifest.json"))?;
    ensure!(
        reviewed.schema == 1 && !reviewed.legal_certification,
        "invalid reviewed legal-material manifest"
    );
    let mut third_party = Vec::new();
    let mut first_party = Vec::new();
    let mut missing = Vec::new();
    for package in selected {
        ensure!(
            safe_relative(Path::new(&package.name)) && safe_relative(Path::new(&package.version)),
            "unsafe package identity"
        );
        if members.contains(package.id.as_str()) && package.source.is_none() {
            let manifest = package
                .manifest_path
                .strip_prefix(root)
                .context("workspace manifest outside source root")?;
            checked_path(root, manifest, false)?;
            first_party.push(FirstParty {
                name: package.name.clone(),
                version: package.version.clone(),
                manifest: spelling(manifest)?,
                declared_license: package.license.clone(),
                status: if package.license.is_none() && package.license_file.is_none() {
                    "not-declared-no-license-granted"
                } else {
                    "upstream-declaration-only-no-license-granted-by-collector"
                }
                .into(),
            });
            continue;
        }
        let source = package.source.as_deref().with_context(|| format!("notice prerequisite: non-workspace path dependency {} {} needs a portable, pinned upstream source identity", package.name, package.version))?;
        validate_source(source)?;
        let locked = lock
            .package
            .iter()
            .filter(|p| {
                p.name == package.name
                    && p.version == package.version
                    && p.source.as_deref() == Some(source)
            })
            .collect::<Vec<_>>();
        ensure!(
            locked.len() == 1,
            "notice prerequisite: {} {} has no unique locked source",
            package.name,
            package.version
        );
        let checksum = locked[0].checksum.clone();
        let checksum_kind = if source.starts_with("git+") {
            let revision = source.rsplit_once('#').map(|(_, v)| v).unwrap_or("");
            ensure!(
                revision.len() == 40 && revision.bytes().all(|v| v.is_ascii_hexdigit()),
                "unresolved git dependency revision"
            );
            "git-revision-in-source"
        } else {
            ensure!(
                checksum.as_deref().is_some_and(valid_sha256),
                "notice prerequisite: {} {} lacks a locked archive SHA-256",
                package.name,
                package.version
            );
            "cargo-archive-sha256"
        };
        let directory = package
            .manifest_path
            .parent()
            .context("dependency manifest parent missing")?;
        real_dir(directory)?;
        checked_path(directory, Path::new("Cargo.toml"), false)?;
        let mut files = BTreeSet::new();
        let mut visited = 0;
        find_documents(directory, directory, false, 0, &mut visited, &mut files)?;
        if let Some(file) = &package.license_file {
            let relative = if file.is_absolute() {
                file.strip_prefix(directory)
                    .context("license_file outside dependency root")?
                    .to_path_buf()
            } else {
                file.clone()
            };
            checked_path(directory, &relative, false).with_context(|| {
                format!(
                    "invalid license_file for {} {}",
                    package.name, package.version
                )
            })?;
            files.insert(relative);
        }
        let needs_supplement = files.is_empty()
            || package.license_file.is_none()
                && !files.iter().any(|file| {
                    file.components()
                        .any(|part| license_name(&part.as_os_str().to_string_lossy()))
                });
        // Include source identity in the directory key: same-name/version crates
        // from different registries must neither overwrite nor alias one another.
        let key = format!(
            "cargo/{}-{}-{}",
            package.name,
            package.version,
            digest(source.as_bytes())
        );
        let supplement = if needs_supplement {
            reviewed_documents(
                &reviewed,
                package,
                checksum.as_deref(),
                directory,
                &key,
                &mut material,
            )?
        } else {
            None
        };
        if needs_supplement && supplement.is_none() {
            missing.push(format!("{} {} ({source}): no bundled license/copying text or license_file and no exact reviewed source supplement (an SPDX string or NOTICE alone is insufficient); obtain authentic upstream notice material", package.name, package.version));
            continue;
        }
        let (mut documents, provenance) = match supplement {
            Some((documents, binding)) => (
                documents,
                Some(ReviewedProvenance {
                    legal_certification: false,
                    limits: reviewed.limits.clone(),
                    binding,
                }),
            ),
            None => (Vec::new(), None),
        };
        for file in files {
            let bytes = read_bounded(&checked_path(directory, &file, false)?, MAX_DOCUMENT)?;
            documents.push(material.add(&key, &file, bytes, None)?);
        }
        third_party.push(ThirdParty {
            name: package.name.clone(),
            version: package.version.clone(),
            spdx: package.license.clone(),
            source: source.into(),
            checksum,
            checksum_kind: checksum_kind.into(),
            chosen_license: provenance
                .as_ref()
                .map(|p| p.binding.chosen_license.clone()),
            reviewed_material: provenance,
            documents,
        });
    }
    ensure!(
        missing.is_empty(),
        "notice prerequisites unresolved:\n{}",
        missing.join("\n")
    );
    third_party.push(native_documents(native, target, &mut material)?);
    third_party
        .sort_by(|a, b| (&a.name, &a.version, &a.source).cmp(&(&b.name, &b.version, &b.source)));
    first_party.sort_by(|a, b| (&a.name, &a.version).cmp(&(&b.name, &b.version)));
    let inventory = Inventory {
        schema: 1,
        target: target.into(),
        cargo_lock_sha256: digest(lock_bytes),
        scope: "normal production dependency reachability from brotypist and brotypist-updater, plus pinned llama.cpp CPU/Metal native notices; excludes build/dev-only dependencies and disabled llama.cpp tools",
        legal_status: "upstream notice evidence only; not legal certification, a first-party license grant, or evidence that system/toolchain/model license obligations are satisfied",
        legal_certification: false,
        first_party,
        third_party,
    };
    // All source validation and missing-material failures happen before output
    // creation. Never append to, replace, or follow a pre-existing destination.
    let parent = output.parent().context("licenses output needs a parent")?;
    real_dir(parent)?;
    fs::create_dir(output).context("licenses output must be a new owned directory")?;
    for payload in material.payloads {
        let destination = output.join(payload.path);
        fs::create_dir_all(destination.parent().unwrap())?;
        write_new(&destination, &payload.bytes)?;
    }
    json_new(&output.join("inventory.json"), &inventory)
}

// Metadata resolves the whole workspace, including xtask/dev feature requests.
// Re-evaluate the default production feature closure over Cargo's already
// target-filtered, locked edges so those requests do not enable optional runtime
// crates accidentally. Version/target resolution itself remains Cargo's job.
fn production_packages(metadata: &Metadata) -> Result<Vec<&Package>> {
    let packages: BTreeMap<_, _> = metadata
        .packages
        .iter()
        .map(|p| (p.id.as_str(), p))
        .collect();
    ensure!(
        packages.len() == metadata.packages.len(),
        "duplicate Cargo package IDs"
    );
    let resolve = metadata
        .resolve
        .as_ref()
        .context("Cargo metadata has no resolved dependency graph")?;
    let nodes: BTreeMap<_, _> = resolve.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    ensure!(
        nodes.len() == resolve.nodes.len(),
        "duplicate Cargo resolve nodes"
    );
    let mut active: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for name in ROOT_PACKAGES {
        let roots = metadata
            .packages
            .iter()
            .filter(|p| p.name == name && metadata.workspace_members.contains(&p.id))
            .collect::<Vec<_>>();
        ensure!(
            roots.len() == 1,
            "expected one workspace production root {name}"
        );
        active
            .entry(&roots[0].id)
            .or_default()
            .insert("default".into());
    }
    loop {
        let before = active.clone();
        for (&id, requests) in &before {
            let package = packages
                .get(id)
                .context("resolved package missing from metadata")?;
            let node = nodes
                .get(id)
                .context("production package has no resolve node")?;
            let (features, enabled, dependency_features) = expand_features(package, requests)?;
            active.get_mut(id).unwrap().extend(features);
            for edge in &node.deps {
                for dependency in package.dependencies.iter().filter(|d| {
                    d.kind.is_none()
                        && normalized(d.rename.as_deref().unwrap_or(&d.name))
                            == normalized(&edge.name)
                }) {
                    if !edge
                        .dep_kinds
                        .iter()
                        .any(|k| k.kind.is_none() && k.target == dependency.target)
                    {
                        continue;
                    }
                    let alias = dependency.rename.as_deref().unwrap_or(&dependency.name);
                    if dependency.optional && !enabled.contains(alias) {
                        continue;
                    }
                    ensure!(
                        packages.contains_key(edge.pkg.as_str()),
                        "normal dependency missing from metadata"
                    );
                    let entry = active.entry(edge.pkg.as_str()).or_default();
                    entry.extend(dependency.features.iter().cloned());
                    if dependency.uses_default_features {
                        entry.insert("default".into());
                    }
                    if let Some(values) = dependency_features.get(alias) {
                        entry.extend(values.iter().cloned());
                    }
                }
            }
        }
        if active == before {
            break;
        }
    }
    let mut result = active.keys().map(|id| packages[id]).collect::<Vec<_>>();
    result.sort_by(|a, b| (&a.name, &a.version, &a.source).cmp(&(&b.name, &b.version, &b.source)));
    Ok(result)
}
fn normalized(value: &str) -> String {
    value.replace('-', "_")
}
type FeatureClosure = (
    BTreeSet<String>,
    BTreeSet<String>,
    BTreeMap<String, BTreeSet<String>>,
);
fn expand_features(package: &Package, requests: &BTreeSet<String>) -> Result<FeatureClosure> {
    let mut features = requests.clone();
    let mut enabled = BTreeSet::new();
    let mut forwarded: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    loop {
        let before = (features.clone(), enabled.clone(), forwarded.clone());
        for feature in features.clone() {
            // A missing default feature is legal and behaves as an empty set.
            let Some(values) = package.features.get(&feature) else {
                ensure!(
                    feature == "default",
                    "unknown production feature {} in {}",
                    feature,
                    package.name
                );
                continue;
            };
            for value in values {
                if let Some(dependency) = value.strip_prefix("dep:") {
                    enabled.insert(dependency.to_owned());
                } else if let Some((dependency, feature)) = value.split_once('/') {
                    let weak = dependency.ends_with('?');
                    let dependency = dependency.trim_end_matches('?');
                    if !weak {
                        enabled.insert(dependency.into());
                    }
                    if !weak || enabled.contains(dependency) {
                        forwarded
                            .entry(dependency.into())
                            .or_default()
                            .insert(feature.into());
                    }
                } else {
                    features.insert(value.clone());
                }
            }
        }
        if before == (features.clone(), enabled.clone(), forwarded.clone()) {
            break;
        }
    }
    Ok((features, enabled, forwarded))
}

fn reviewed_documents(
    reviewed: &ReviewedMaterial,
    package: &Package,
    checksum: Option<&str>,
    directory: &Path,
    key: &str,
    material: &mut Material,
) -> Result<Option<(Vec<Document>, ReviewedPackage)>> {
    let matches = reviewed
        .packages
        .iter()
        .filter(|binding| {
            binding.name == package.name
                && binding.version == package.version
                && Some(binding.source.as_str()) == package.source.as_deref()
        })
        .collect::<Vec<_>>();
    ensure!(matches.len() <= 1, "duplicate reviewed source binding");
    let Some(binding) = matches.first() else {
        return Ok(None);
    };
    ensure!(
        valid_sha256(&binding.checksum)
            && checksum == Some(binding.checksum.as_str())
            && package.license.as_deref() == Some(binding.declaration.as_str()),
        "reviewed legal material does not match locked checksum/declaration for {} {}",
        package.name,
        package.version
    );
    let terms = match binding.chosen_license.as_str() {
        "MIT" => "MIT-standard.txt",
        "Apache-2.0" => "Apache-2.0.txt",
        _ => anyhow::bail!("unsupported reviewed license selection"),
    };
    ensure!(
        binding
            .declaration
            .split([' ', '/'])
            .any(|part| part == binding.chosen_license)
            && binding.documents.len() >= 2
            && binding
                .documents
                .iter()
                .filter(|file| matches!(
                    file.as_str(),
                    "objc2-declaration.txt" | "unicode-casefold-Cargo.toml"
                ))
                .count()
                == 1
            && binding.documents.iter().any(|file| file == terms)
            && !binding.source_evidence.is_empty(),
        "reviewed source must supply an explicit declaration, selected standard terms and archive evidence"
    );
    // These evidence hashes were independently matched to the checksum-bound
    // published archives. A changed cache, release, registry or declaration
    // must receive a new source-specific review; it cannot reuse these terms.
    for evidence in &binding.source_evidence {
        let bytes = read_bounded(
            &checked_path(directory, Path::new(&evidence.path), false)?,
            MAX_DOCUMENT,
        )?;
        ensure!(
            valid_sha256(&evidence.sha256) && digest(&bytes) == evidence.sha256,
            "reviewed archive evidence changed for {} {}: {}",
            package.name,
            package.version,
            evidence.path
        );
    }
    let mut seen = BTreeSet::new();
    let mut documents = Vec::new();
    for file in &binding.documents {
        ensure!(seen.insert(file), "duplicate reviewed document");
        let records = reviewed
            .documents
            .iter()
            .filter(|d| d.file == *file)
            .collect::<Vec<_>>();
        ensure!(
            records.len() == 1,
            "missing or duplicate reviewed document provenance"
        );
        let record = records[0];
        let bytes: &[u8] = match file.as_str() {
            "Apache-2.0.txt" => include_bytes!("../legal-material/Apache-2.0.txt"),
            "MIT-standard.txt" => include_bytes!("../legal-material/MIT-standard.txt"),
            "objc2-declaration.txt" => include_bytes!("../legal-material/objc2-declaration.txt"),
            "unicode-casefold-Cargo.toml" => {
                include_bytes!("../legal-material/unicode-casefold-Cargo.toml")
            }
            _ => anyhow::bail!("unreviewed legal document refused"),
        };
        ensure!(
            valid_sha256(&record.sha256) && digest(bytes) == record.sha256,
            "reviewed legal document hash mismatch: {file}"
        );
        let mut document = material.add(
            key,
            &Path::new("reviewed").join(file),
            bytes.to_vec(),
            Some(record.role.clone()),
        )?;
        // The shared objc2 declaration was fetched at every package's exact
        // revision. Do not replace that binding with a mutable repository URL.
        document.upstream_url = Some(if file == "objc2-declaration.txt" {
            binding.declaration_source.clone()
        } else {
            record.source.clone()
        });
        documents.push(document);
    }
    Ok(Some((documents, (*binding).clone())))
}

impl Material {
    fn add(
        &mut self,
        key: &str,
        relative: &Path,
        bytes: Vec<u8>,
        excerpt: Option<String>,
    ) -> Result<Document> {
        ensure!(!bytes.is_empty(), "empty upstream notice refused");
        ensure!(
            bytes.len() as u64 <= MAX_DOCUMENT,
            "upstream notice exceeds size limit"
        );
        self.total = self
            .total
            .checked_add(bytes.len())
            .context("notice size overflow")?;
        ensure!(
            self.total <= MAX_TOTAL,
            "total upstream notice material exceeds size limit"
        );
        let path = Path::new(key).join(relative);
        ensure!(safe_relative(&path), "unsafe notice output path");
        let document = Document {
            upstream_path: spelling(relative)?,
            path: spelling(&path)?,
            size: bytes.len(),
            sha256: digest(&bytes),
            excerpt,
            upstream_url: None,
        };
        self.payloads.push(Payload { path, bytes });
        Ok(document)
    }
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn spelling(path: &Path) -> Result<String> {
    ensure!(safe_relative(path), "unsafe relative notice path");
    Ok(path
        .to_str()
        .context("non-UTF-8 notice path")?
        .replace('\\', "/"))
}
fn validate_source(source: &str) -> Result<()> {
    let url = source
        .strip_prefix("registry+")
        .or_else(|| source.strip_prefix("sparse+"))
        .or_else(|| source.strip_prefix("git+"))
        .context("unsupported dependency source identity")?;
    let address = url.strip_prefix("https://").context(
        "notice source identity requires portable HTTPS without local paths or credentials",
    )?;
    let authority = address.split(['/', '?', '#']).next().unwrap_or("");
    ensure!(
        !authority.is_empty()
            && !authority.contains('@')
            && !source.chars().any(char::is_control)
            && !source.contains('\\'),
        "unsafe dependency source identity"
    );
    if let Some((_, query)) = address.split_once('?') {
        for parameter in query.split('#').next().unwrap_or("").split('&') {
            let key = parameter.split('=').next().unwrap_or("");
            ensure!(
                ["branch", "tag", "rev"].contains(&key),
                "unexpected dependency source query; refusing potential credential disclosure"
            );
        }
    }
    Ok(())
}

fn checked_path(root: &Path, relative: &Path, directory: bool) -> Result<PathBuf> {
    ensure!(
        safe_relative(relative),
        "notice path must stay inside its source root"
    );
    real_dir(root)?;
    let canonical_root = root.canonicalize()?;
    let mut path = root.to_path_buf();
    for component in relative.components() {
        path.push(component.as_os_str());
        let metadata = fs::symlink_metadata(&path)?;
        ensure!(!linked(&metadata), "linked upstream notice path refused");
        if path != root.join(relative) || directory {
            ensure!(metadata.is_dir(), "notice path parent is not a directory");
        } else {
            ensure!(metadata.is_file(), "upstream notice is not a regular file");
        }
    }
    ensure!(
        path.canonicalize()?.starts_with(canonical_root),
        "upstream notice escapes source root"
    );
    Ok(path)
}
fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        !linked(&metadata) && metadata.is_file() && metadata.len() <= limit,
        "upstream document is linked, special, or exceeds size limit"
    );
    let mut bytes = Vec::new();
    File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "upstream document grew beyond size limit"
    );
    Ok(bytes)
}
fn license_name(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    // Objective-C's NSCopying implementation is source code, not legal text.
    // In particular objc2-foundation/src/copying.rs must not satisfy this gate.
    if name.ends_with(".RS") {
        return false;
    }
    ["LICENSE", "LICENCE", "COPYING"].iter().any(|prefix| {
        name == *prefix
            || name == format!("{prefix}S")
            || name
                .strip_prefix(prefix)
                .is_some_and(|suffix| suffix.starts_with(['.', '-', '_']))
    })
}
fn legal_name(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    license_name(&name)
        || ["NOTICE", "COPYRIGHT"].iter().any(|prefix| {
            name == *prefix
                || name == format!("{prefix}S")
                || name
                    .strip_prefix(prefix)
                    .is_some_and(|suffix| suffix.starts_with(['.', '-', '_']))
        })
}
fn find_documents(
    root: &Path,
    directory: &Path,
    legal_directory: bool,
    depth: usize,
    visited: &mut usize,
    result: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    ensure!(
        depth <= 32,
        "upstream notice directory nesting exceeds limit"
    );
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        *visited += 1;
        ensure!(
            *visited <= MAX_ENTRIES,
            "upstream notice scan exceeds entry limit"
        );
        let name = entry.file_name();
        let name = name.to_str().context("non-UTF-8 dependency entry")?;
        if [".git", "target"].contains(&name) {
            continue;
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        let is_legal = legal_name(name);
        // Even an innocuously named linked directory could hide notices.
        // Refuse links rather than silently claiming that its scan is complete.
        ensure!(
            !linked(&metadata),
            "linked upstream source entry refused during notice scan"
        );
        if metadata.is_dir() {
            let in_legal = legal_directory
                || is_legal
                || ["licenses", "licences", "notices"]
                    .contains(&name.to_ascii_lowercase().as_str());
            find_documents(root, &path, in_legal, depth + 1, visited, result)?;
        } else if is_legal || legal_directory {
            ensure!(metadata.is_file(), "special upstream notice refused");
            let relative = path.strip_prefix(root)?.to_path_buf();
            checked_path(root, &relative, false)?;
            result.insert(relative);
        }
    }
    Ok(())
}

fn native_documents(root: &Path, target: &str, material: &mut Material) -> Result<ThirdParty> {
    let key = format!("native/llama.cpp-{LLAMA_REVISION}");
    let mut documents = Vec::new();
    let license = Path::new("LICENSE");
    documents.push(material.add(
        &key,
        license,
        read_bounded(&checked_path(root, license, false)?, MAX_DOCUMENT)?,
        None,
    )?);
    // GGML_LLAMAFILE is ON at this pinned revision. Its MIT notice is inline,
    // not present as a separate LICENSE. Copy the original comment bytes only.
    let sgemm = Path::new("ggml/src/ggml-cpu/llamafile/sgemm.cpp");
    let bytes = read_bounded(&checked_path(root, sgemm, false)?, MAX_DOCUMENT)?;
    let header = leading_comment(&bytes)?;
    ensure!(
        header.starts_with(b"// Copyright 2024 Mozilla Foundation")
            && header
                .windows(b"// SOFTWARE.".len())
                .any(|w| w == b"// SOFTWARE."),
        "pinned llamafile license header changed; review native notice prerequisites"
    );
    let mut document = material.add(
        &key,
        Path::new("llamafile-LICENSE.txt"),
        header.to_vec(),
        Some("leading MIT comment, verbatim".into()),
    )?;
    document.upstream_path = spelling(sgemm)?;
    documents.push(document);
    let mut yarn_sources = vec!["ggml/src/ggml-cpu/ops.cpp"];
    if target.contains("apple-darwin") {
        yarn_sources.push("ggml/src/ggml-metal/kernels/rope.metal");
    }
    for (index, source) in yarn_sources.iter().enumerate() {
        let relative = Path::new(source);
        let bytes = read_bounded(&checked_path(root, relative, false)?, MAX_DOCUMENT)?;
        let notice = yarn_notice(&bytes)?;
        let mut document = material.add(&key, Path::new(&format!("yarn-{index}-NOTICE.txt")), notice, Some("YaRN attribution comment, verbatim; MIT declaration retained, no substituted grant".into()))?;
        document.upstream_path = (*source).into();
        documents.push(document);
    }
    documents.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(ThirdParty {
        name: "llama.cpp (ggml CPU and platform Metal)".into(),
        version: LLAMA_REVISION.into(),
        spdx: Some("MIT".into()),
        source: format!("git+https://github.com/ggml-org/llama.cpp#{LLAMA_REVISION}"),
        checksum: None,
        checksum_kind: "pinned-git-revision-in-source; individual-notice-sha256".into(),
        chosen_license: None,
        reviewed_material: None,
        documents,
    })
}
fn leading_comment(bytes: &[u8]) -> Result<&[u8]> {
    let mut end = 0;
    for line in bytes.split_inclusive(|b| *b == b'\n') {
        if !line.starts_with(b"//") {
            break;
        }
        end += line.len();
    }
    ensure!(end > 0, "missing native inline license");
    Ok(&bytes[..end])
}
fn yarn_notice(bytes: &[u8]) -> Result<Vec<u8>> {
    let lines = bytes.split_inclusive(|b| *b == b'\n').collect::<Vec<_>>();
    let matches = lines
        .windows(2)
        .filter(|pair| {
            pair[0].starts_with(b"// YaRN algorithm based on ")
                && pair[1].starts_with(
                    b"// MIT licensed. Copyright (c) 2023 Jeffrey Quesnelle and Bowen Peng.",
                )
        })
        .collect::<Vec<_>>();
    ensure!(
        matches.len() == 1,
        "pinned YaRN attribution changed; review native notice prerequisites"
    );
    Ok([matches[0][0], matches[0][1]].concat())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use tempfile::{TempDir, tempdir};

    struct Fixture {
        directory: TempDir,
        metadata: Value,
        lock: Vec<u8>,
    }
    impl Fixture {
        fn new() -> Self {
            let directory = tempdir().unwrap();
            let root = directory.path();
            let source = "registry+https://github.com/rust-lang/crates.io-index";
            let mut packages = Vec::new();
            for name in [
                "brotypist",
                "brotypist-updater",
                "xtask",
                "runtime",
                "helper-only",
                "dev-only",
                "build-only",
                "other-platform",
                "optional",
                "leaf",
            ] {
                let folder = root.join(name);
                fs::create_dir(&folder).unwrap();
                fs::write(folder.join("Cargo.toml"), "[package]\n").unwrap();
                if !ROOT_PACKAGES.contains(&name) && name != "xtask" {
                    fs::write(
                        folder.join("LICENSE-MIT"),
                        format!("Authentic fixture notice for {name}\n"),
                    )
                    .unwrap();
                }
                packages.push(json!({"id":name,"name":name,"version":"1.0.0","source":if ROOT_PACKAGES.contains(&name) || name == "xtask" { Value::Null } else { json!(source) },"manifest_path":folder.join("Cargo.toml"),"license":if ROOT_PACKAGES.contains(&name) { Value::Null } else { json!("MIT OR Apache-2.0") },"license_file":null,"features":{},"dependencies":[]}));
            }
            let edge = |name: &str, kind: Value| json!({"name":name,"pkg":name,"dep_kinds":[{"kind":kind,"target":null}]});
            let mut nodes = Vec::new();
            for package in &packages {
                nodes.push(json!({"id":package["id"],"deps":[]}));
            }
            nodes[0]["deps"] = json!([
                edge("runtime", Value::Null),
                edge("dev-only", json!("dev")),
                edge("build-only", json!("build"))
            ]);
            nodes[1]["deps"] = json!([edge("helper-only", Value::Null)]);
            nodes[2]["deps"] = json!([edge("optional", Value::Null)]);
            packages[0]["dependencies"] = json!([
                dependency("runtime", false, None),
                dependency("dev-only", false, Some("dev")),
                dependency("build-only", false, Some("build"))
            ]);
            packages[1]["dependencies"] = json!([dependency("helper-only", false, None)]);
            // Metadata's workspace union includes this normal optional edge,
            // but only xtask/dev features enable it, not the production roots.
            nodes[3]["deps"] = json!([edge("optional", Value::Null)]);
            packages[3]["dependencies"] = json!([dependency("optional", true, None)]);
            packages[3]["features"] = json!({"extras":["dep:optional"]});
            let metadata = json!({"version":1,"workspace_root":root,"workspace_members":["brotypist","brotypist-updater","xtask"],"packages":packages,"resolve":{"nodes":nodes}});
            let mut lock = String::new();
            for package in metadata["packages"].as_array().unwrap() {
                if package["source"].is_string() {
                    lock.push_str(&format!("[[package]]\nname = {}\nversion = \"1.0.0\"\nsource = {:?}\nchecksum = {:?}\n", package["name"], source, "a".repeat(64)));
                }
            }
            let native = root.join("native");
            for path in [
                "ggml/src/ggml-cpu/llamafile",
                "ggml/src/ggml-metal/kernels",
                "tools/server",
            ] {
                fs::create_dir_all(native.join(path)).unwrap();
            }
            fs::write(
                native.join("LICENSE"),
                "Pinned upstream MIT license evidence\n",
            )
            .unwrap();
            fs::write(native.join("ggml/src/ggml-cpu/llamafile/sgemm.cpp"), "// Copyright 2024 Mozilla Foundation\n// Permission is hereby granted, upstream fixture\n// SOFTWARE.\n\nint not_notice;\n").unwrap();
            let yarn = "// YaRN algorithm based on LlamaYaRNScaledRotaryEmbedding.py from https://github.com/jquesnelle/yarn\n// MIT licensed. Copyright (c) 2023 Jeffrey Quesnelle and Bowen Peng.\n";
            for file in [
                "ggml/src/ggml-cpu/ops.cpp",
                "ggml/src/ggml-metal/kernels/rope.metal",
            ] {
                fs::write(native.join(file), format!("{yarn}int not_notice;\n")).unwrap();
            }
            fs::write(
                native.join("tools/server/LICENSE"),
                "Disabled tool notice must not ship",
            )
            .unwrap();
            Self {
                directory,
                metadata,
                lock: lock.into_bytes(),
            }
        }
        fn root(&self) -> &Path {
            self.directory.path()
        }
        fn parsed(&self) -> Metadata {
            serde_json::from_value(self.metadata.clone()).unwrap()
        }
        fn collect(&self, name: &str) -> Result<()> {
            collect_metadata(
                self.root(),
                "aarch64-apple-darwin",
                &self.root().join(name),
                &self.parsed(),
                &self.lock,
                &self.root().join("native"),
            )
        }
        fn inventory(&self, name: &str) -> Value {
            serde_json::from_slice(
                &fs::read(self.root().join(name).join("inventory.json")).unwrap(),
            )
            .unwrap()
        }
    }
    fn dependency(name: &str, optional: bool, kind: Option<&str>) -> Value {
        json!({"name":name,"rename":null,"kind":kind,"target":null,"optional":optional,"uses_default_features":true,"features":[]})
    }

    #[test]
    fn production_closure_excludes_dev_build_inactive_optional_and_unreachable_platforms() {
        let f = Fixture::new();
        let metadata = f.parsed();
        let names = production_packages(&metadata)
            .unwrap()
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            ["brotypist", "brotypist-updater", "helper-only", "runtime"]
        );
    }
    #[test]
    fn production_default_renames_and_weak_dependency_features_reach_fixed_point() {
        let mut f = Fixture::new();
        f.metadata["packages"][0]["dependencies"][0]["rename"] = json!("renamed");
        f.metadata["resolve"]["nodes"][0]["deps"][0]["name"] = json!("renamed");
        f.metadata["packages"][0]["features"] = json!({"default":["renamed/extras"]});
        f.metadata["packages"][3]["features"] =
            json!({"extras":["optional?/with-leaf","dep:optional"]});
        f.metadata["packages"][8]["features"] = json!({"with-leaf":["dep:leaf"]});
        f.metadata["packages"][8]["dependencies"] = json!([dependency("leaf", true, None)]);
        f.metadata["resolve"]["nodes"][8]["deps"] =
            json!([{"name":"leaf","pkg":"leaf","dep_kinds":[{"kind":null,"target":null}]}]);
        let metadata = f.parsed();
        let names = production_packages(&metadata)
            .unwrap()
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "brotypist",
                "brotypist-updater",
                "helper-only",
                "leaf",
                "optional",
                "runtime"
            ]
        );
    }
    #[test]
    fn preserves_exact_bytes_identity_checksums_and_separate_unlicensed_first_party() {
        let f = Fixture::new();
        fs::create_dir(f.root().join("runtime/licenses")).unwrap();
        fs::write(
            f.root().join("runtime/licenses/Apache-2.0.txt"),
            b"upstream bytes\r\n\xff",
        )
        .unwrap();
        fs::write(f.root().join("runtime/NOTICE"), b"attribution\n").unwrap();
        f.collect("notices").unwrap();
        let inventory = f.inventory("notices");
        assert_eq!(
            inventory["first_party"][0]["status"],
            "not-declared-no-license-granted"
        );
        assert!(inventory["first_party"][0]["declared_license"].is_null());
        let package = inventory["third_party"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == "runtime")
            .unwrap();
        assert_eq!(package["spdx"], "MIT OR Apache-2.0");
        assert_eq!(package["checksum"], "a".repeat(64));
        for document in package["documents"].as_array().unwrap() {
            let bytes = fs::read(
                f.root()
                    .join("notices")
                    .join(document["path"].as_str().unwrap()),
            )
            .unwrap();
            let original = fs::read(
                f.root()
                    .join("runtime")
                    .join(document["upstream_path"].as_str().unwrap()),
            )
            .unwrap();
            assert_eq!(bytes, original);
            assert_eq!(document["sha256"], digest(&bytes));
        }
        let text = serde_json::to_string(&inventory).unwrap();
        assert!(!text.contains(f.root().to_str().unwrap()));
        assert!(!text.contains("manifest_path"));
        assert!(!text.contains("Disabled tool"));
    }
    #[test]
    fn custom_license_file_is_preserved_and_escape_rejected_before_output() {
        let mut f = Fixture::new();
        fs::write(
            f.root().join("runtime/terms.txt"),
            b"actual upstream custom terms\n",
        )
        .unwrap();
        f.metadata["packages"][3]["license_file"] = json!("terms.txt");
        f.collect("good").unwrap();
        let inventory = f.inventory("good");
        assert!(
            inventory["third_party"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["name"] == "runtime")
                .unwrap()["documents"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["upstream_path"] == "terms.txt")
        );
        f.metadata["packages"][3]["license_file"] = json!("../helper-only/LICENSE-MIT");
        assert!(f.collect("escape").is_err());
        assert!(!f.root().join("escape").exists());
    }
    #[test]
    fn missing_authentic_material_fails_even_with_spdx_declaration() {
        let f = Fixture::new();
        fs::remove_file(f.root().join("runtime/LICENSE-MIT")).unwrap();
        fs::write(
            f.root().join("runtime/NOTICE"),
            b"Attribution without license text\n",
        )
        .unwrap();
        let error = f.collect("missing").unwrap_err().to_string();
        assert!(error.contains("runtime 1.0.0"));
        assert!(error.contains("authentic upstream"));
        assert!(!f.root().join("missing").exists());
    }
    #[test]
    fn copying_rust_modules_are_not_license_material() {
        let f = Fixture::new();
        fs::remove_file(f.root().join("runtime/LICENSE-MIT")).unwrap();
        fs::create_dir(f.root().join("runtime/src")).unwrap();
        fs::write(
            f.root().join("runtime/src/copying.rs"),
            b"pub trait NSCopying {}\n",
        )
        .unwrap();
        assert!(!license_name("copying.rs"));
        assert!(!legal_name("copying.rs"));
        assert!(license_name("COPYING.txt"));
        assert!(f.collect("missing").is_err());
        assert!(!f.root().join("missing").exists());
    }

    fn reviewed_fixture(f: &Fixture) -> ReviewedMaterial {
        let mut reviewed: ReviewedMaterial =
            serde_json::from_str(include_str!("../legal-material/manifest.json")).unwrap();
        let mut binding = reviewed.packages[0].clone();
        binding.name = "runtime".into();
        binding.version = "1.0.0".into();
        binding.checksum = "a".repeat(64);
        binding.declaration = "MIT OR Apache-2.0".into();
        binding.chosen_license = "Apache-2.0".into();
        binding.documents = vec!["objc2-declaration.txt".into(), "Apache-2.0.txt".into()];
        binding.source_evidence = vec![SourceEvidence {
            path: "Cargo.toml".into(),
            sha256: digest(b"[package]\n"),
        }];
        assert!(f.root().join("runtime/Cargo.toml").is_file());
        reviewed.packages = vec![binding];
        reviewed
    }

    #[test]
    fn reviewed_material_preserves_original_terms_provenance_and_license_selection() {
        let f = Fixture::new();
        let metadata = f.parsed();
        let mut reviewed = reviewed_fixture(&f);
        for (license, terms) in [
            ("Apache-2.0", "Apache-2.0.txt"),
            ("MIT", "MIT-standard.txt"),
        ] {
            reviewed.packages[0].chosen_license = license.into();
            reviewed.packages[0].documents[1] = terms.into();
            let mut material = Material::default();
            let (documents, binding) = reviewed_documents(
                &reviewed,
                &metadata.packages[3],
                Some(&"a".repeat(64)),
                &f.root().join("runtime"),
                "cargo/runtime",
                &mut material,
            )
            .unwrap()
            .unwrap();
            assert_eq!(binding.chosen_license, license);
            assert_eq!(documents.len(), 2);
            assert!(documents.iter().all(|d| d.upstream_url.is_some()));
            assert_eq!(
                documents[0].upstream_url.as_deref(),
                Some(binding.declaration_source.as_str())
            );
            for document in documents {
                let payload = material
                    .payloads
                    .iter()
                    .find(|p| spelling(&p.path).unwrap() == document.path)
                    .unwrap();
                assert_eq!(document.sha256, digest(&payload.bytes));
            }
            if license == "MIT" {
                assert!(
                    material.payloads[1]
                        .bytes
                        .windows(b"<YEAR>, <OWNER>".len())
                        .any(|w| w == b"<YEAR>, <OWNER>")
                );
            }
        }
    }

    #[test]
    fn reviewed_material_refuses_unknown_or_changed_source_and_corrupted_evidence() {
        let f = Fixture::new();
        let mut metadata = f.parsed();
        let package = &mut metadata.packages[3];
        let mut reviewed = reviewed_fixture(&f);
        let invoke = |reviewed: &ReviewedMaterial, package: &Package, checksum: &str| {
            reviewed_documents(
                reviewed,
                package,
                Some(checksum),
                &f.root().join("runtime"),
                "cargo/runtime",
                &mut Material::default(),
            )
        };
        assert!(invoke(&reviewed, package, &"b".repeat(64)).is_err());
        package.license = Some("MIT".into());
        assert!(invoke(&reviewed, package, &"a".repeat(64)).is_err());
        package.license = Some("MIT OR Apache-2.0".into());
        package.source = Some("registry+https://example.com/other-index".into());
        assert!(
            invoke(&reviewed, package, &"a".repeat(64))
                .unwrap()
                .is_none()
        );
        package.source = Some("registry+https://github.com/rust-lang/crates.io-index".into());
        package.version = "1.0.1".into();
        assert!(
            invoke(&reviewed, package, &"a".repeat(64))
                .unwrap()
                .is_none()
        );
        package.version = "1.0.0".into();
        reviewed
            .documents
            .iter_mut()
            .find(|d| d.file == "Apache-2.0.txt")
            .unwrap()
            .sha256 = "0".repeat(64);
        assert!(invoke(&reviewed, package, &"a".repeat(64)).is_err());
        reviewed = reviewed_fixture(&f);
        fs::write(
            f.root().join("runtime/Cargo.toml"),
            b"changed archive evidence\n",
        )
        .unwrap();
        assert!(invoke(&reviewed, package, &"a".repeat(64)).is_err());
    }
    #[test]
    fn oversized_notice_and_existing_output_are_refused() {
        let f = Fixture::new();
        let file = File::create(f.root().join("runtime/NOTICE")).unwrap();
        file.set_len(MAX_DOCUMENT + 1).unwrap();
        assert!(f.collect("too-big").is_err());
        assert!(!f.root().join("too-big").exists());
        fs::remove_file(f.root().join("runtime/NOTICE")).unwrap();
        fs::create_dir(f.root().join("existing")).unwrap();
        fs::write(f.root().join("existing/sentinel"), b"leave intact").unwrap();
        assert!(f.collect("existing").is_err());
        assert_eq!(
            fs::read(f.root().join("existing/sentinel")).unwrap(),
            b"leave intact"
        );
    }
    #[test]
    fn deterministic_across_metadata_order_and_cache_root_locations() {
        let a = Fixture::new();
        let mut b = Fixture::new();
        b.metadata["packages"].as_array_mut().unwrap().reverse();
        b.metadata["resolve"]["nodes"]
            .as_array_mut()
            .unwrap()
            .reverse();
        a.collect("out").unwrap();
        b.collect("out").unwrap();
        assert_eq!(
            fs::read(a.root().join("out/inventory.json")).unwrap(),
            fs::read(b.root().join("out/inventory.json")).unwrap()
        );
        for package in a.inventory("out")["third_party"].as_array().unwrap() {
            for document in package["documents"].as_array().unwrap() {
                let relative = document["path"].as_str().unwrap();
                assert_eq!(
                    fs::read(a.root().join("out").join(relative)).unwrap(),
                    fs::read(b.root().join("out").join(relative)).unwrap()
                );
            }
        }
    }
    #[test]
    fn native_evidence_copies_only_linked_notices_and_platform_specific_attribution() {
        let f = Fixture::new();
        let mut material = Material::default();
        let native = native_documents(
            &f.root().join("native"),
            "x86_64-unknown-linux-gnu",
            &mut material,
        )
        .unwrap();
        assert_eq!(native.documents.len(), 3);
        assert_eq!(material.payloads.len(), 3);
        assert!(material.payloads.iter().all(|p| {
            !p.bytes
                .windows(b"int not_notice".len())
                .any(|w| w == b"int not_notice")
        }));
        let mut material = Material::default();
        let mac = native_documents(
            &f.root().join("native"),
            "aarch64-apple-darwin",
            &mut material,
        )
        .unwrap();
        assert_eq!(mac.documents.len(), 4);
        assert!(
            mac.documents
                .iter()
                .any(|d| d.upstream_path.ends_with("rope.metal"))
        );
        fs::write(
            f.root()
                .join("native/ggml/src/ggml-cpu/llamafile/sgemm.cpp"),
            b"// changed header\n",
        )
        .unwrap();
        assert!(
            native_documents(
                &f.root().join("native"),
                "aarch64-apple-darwin",
                &mut Material::default()
            )
            .is_err()
        );
    }
    #[cfg(unix)]
    #[test]
    fn linked_license_files_directories_and_declared_paths_are_refused() {
        use std::os::unix::fs::symlink;
        let mut f = Fixture::new();
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("LICENSE"), b"outside terms").unwrap();
        symlink(
            outside.path().join("LICENSE"),
            f.root().join("runtime/NOTICE"),
        )
        .unwrap();
        assert!(f.collect("file-link").is_err());
        fs::remove_file(f.root().join("runtime/NOTICE")).unwrap();
        symlink(outside.path(), f.root().join("runtime/licenses")).unwrap();
        // A legal directory link cannot silently hide missing material.
        assert!(f.collect("directory-link").is_err());
        fs::remove_file(f.root().join("runtime/licenses")).unwrap();
        symlink(outside.path(), f.root().join("runtime/indirection")).unwrap();
        f.metadata["packages"][3]["license_file"] = json!("indirection/LICENSE");
        assert!(f.collect("parent-link").is_err());
    }
    #[test]
    fn credential_and_local_source_identities_are_never_published() {
        for value in [
            "path+file:///Users/private/source",
            "git+https://token@example.com/repo#abcd",
            "registry+https://example.com/index?token=secret",
        ] {
            assert!(validate_source(value).is_err());
        }
        assert!(validate_source("registry+https://github.com/rust-lang/crates.io-index").is_ok());
        assert!(validate_source("git+https://github.com/example/repo?rev=abc#0123456789012345678901234567890123456789").is_ok());
    }
}
