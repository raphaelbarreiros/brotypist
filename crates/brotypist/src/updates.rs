//! Credential-free signed releases, cancellable transport and explicitly requested updates.
use std::{collections::HashSet, fs::{self, File}, io::Write, path::{Path, PathBuf}, sync::{Arc, Mutex}, thread::{self, JoinHandle}, time::{Duration, Instant, SystemTime, UNIX_EPOCH}};
use anyhow::{anyhow, bail, ensure, Context, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize, de::{self, MapAccess, SeqAccess, Visitor}};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use crate::{update_install::{self, Handoff, Installation}, update_keys::{APP_ID, PUBLISHER_PUBLIC_KEY, REPOSITORY}};

pub const MAX_MANIFEST_BYTES: usize = 256 * 1024;
pub const MAX_SIGNATURE_BYTES: usize = 128;
pub const MAX_PACKAGE_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_UNPACKED_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const MAX_ARCHIVE_ENTRIES: usize = 20_000;
const CHECK_INTERVAL: Duration = Duration::from_secs(86400);
const ALLOWED_HOSTS: &[&str] = &["github.com", "release-assets.githubusercontent.com", "objects.githubusercontent.com", "github-releases.githubusercontent.com"];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UpdateAsset { pub name: String, pub target: String, pub format: String, pub size: u64, pub sha256: String }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpdateRelease { pub version: String, pub source_commit: String, pub assets: Vec<UpdateAsset> }
impl UpdateRelease {
    pub fn release_url(&self) -> Result<String> {
        version(&self.version)?;
        Ok(format!("https://github.com/{REPOSITORY}/releases/tag/v{}", self.version))
    }
}

pub(crate) fn version(text: &str) -> Result<[u32; 3]> {
    let parts: Vec<_> = text.split('.').collect();
    ensure!(parts.len() == 3, "The update version is not a supported stable version.");
    let mut result = [0; 3];
    for (i, part) in parts.into_iter().enumerate() {
        ensure!(!part.is_empty() && part.len() <= 9 && part.bytes().all(|c| c.is_ascii_digit()) && (part == "0" || !part.starts_with('0')), "The update version is not a supported stable version.");
        result[i] = part.parse()?;
    }
    Ok(result)
}
pub(crate) fn lower_hex(text: &str, count: usize) -> bool { text.len() == count && text.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) }

// serde_json::Value alone accepts last-key-wins objects. This visitor checks every
// object, including those nested inside arrays, before any schema deserialization.
struct StrictValue(Value);
impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct StrictVisitor;
        impl<'de> Visitor<'de> for StrictVisitor {
            type Value = StrictValue;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { f.write_str("JSON without duplicate fields") }
            fn visit_bool<E: de::Error>(self, v: bool) -> std::result::Result<Self::Value,E> { Ok(StrictValue(v.into())) }
            fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Self::Value,E> { Ok(StrictValue(v.into())) }
            fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Self::Value,E> { Ok(StrictValue(v.into())) }
            fn visit_f64<E: de::Error>(self, v: f64) -> std::result::Result<Self::Value,E> { serde_json::Number::from_f64(v).map(|n| StrictValue(Value::Number(n))).ok_or_else(|| E::custom("nonfinite JSON number")) }
            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Self::Value,E> { Ok(StrictValue(v.into())) }
            fn visit_string<E: de::Error>(self, v: String) -> std::result::Result<Self::Value,E> { Ok(StrictValue(v.into())) }
            fn visit_unit<E: de::Error>(self) -> std::result::Result<Self::Value,E> { Ok(StrictValue(Value::Null)) }
            fn visit_none<E: de::Error>(self) -> std::result::Result<Self::Value,E> { Ok(StrictValue(Value::Null)) }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> std::result::Result<Self::Value,A::Error> {
                let mut out = Vec::new(); while let Some(StrictValue(v)) = a.next_element()? { out.push(v); } Ok(StrictValue(Value::Array(out)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> std::result::Result<Self::Value,A::Error> {
                let mut out = serde_json::Map::new();
                while let Some((k, StrictValue(v))) = a.next_entry::<String,StrictValue>()? {
                    if out.insert(k,v).is_some() { return Err(de::Error::custom("duplicate metadata field")); }
                }
                Ok(StrictValue(Value::Object(out)))
            }
        }
        d.deserialize_any(StrictVisitor)
    }
}
pub(crate) fn strict_json(data: &[u8]) -> Result<Value> { Ok(serde_json::from_slice::<StrictValue>(data)?.0) }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest { schema: u32, application: String, version: String, source_commit: String, assets: Vec<UpdateAsset> }
fn asset_suffix(target: &str, format: &str) -> Option<&'static str> {
    match (target,format) {
        ("macos-arm64","zip") => Some("macos-arm64.zip"),
        ("macos-arm64","dmg") => Some("macos-arm64.dmg"),
        ("linux-x86_64","tar.gz") => Some("linux-amd64.tar.gz"),
        ("linux-x86_64","deb") => Some("linux-amd64.deb"), _ => None,
    }
}
fn validate_asset(asset: &UpdateAsset, release: &str) -> Result<()> {
    let suffix = asset_suffix(&asset.target,&asset.format).context("The update platform is unsupported.")?;
    ensure!(asset.name == format!("BroTypist-{release}-{suffix}"), "The update asset does not match its platform and version.");
    ensure!(asset.size > 0 && asset.size <= MAX_PACKAGE_BYTES && lower_hex(&asset.sha256,64), "Invalid update package size or digest."); Ok(())
}
pub fn verify_manifest(data: &[u8], signature: &[u8]) -> Result<UpdateRelease> {
    let key: [u8;32] = STANDARD.decode(PUBLISHER_PUBLIC_KEY)?.try_into().map_err(|_| anyhow!("Invalid publisher identity."))?;
    verify_with_key(data,signature,&key)
}
fn verify_with_key(data: &[u8], signature: &[u8], key: &[u8;32]) -> Result<UpdateRelease> {
    ensure!(!data.is_empty() && data.len() <= MAX_MANIFEST_BYTES && !signature.is_empty() && signature.len() <= MAX_SIGNATURE_BYTES, "Update metadata exceeds its allowed bounds.");
    let first = signature.iter().position(|c| *c != b'\r' && *c != b'\n').unwrap_or(signature.len());
    let last = signature.iter().rposition(|c| *c != b'\r' && *c != b'\n').map_or(first,|i| i+1);
    let encoded = &signature[first..last];
    let raw = STANDARD.decode(encoded).context("The update publisher signature could not be verified.")?;
    let sig = Signature::from_slice(&raw).context("Invalid update signature length.")?;
    VerifyingKey::from_bytes(key)?.verify_strict(data,&sig).context("The update publisher signature could not be verified.")?;
    let manifest: Manifest = serde_json::from_value(strict_json(data)?)?;
    ensure!(manifest.schema == 1 && manifest.application == APP_ID, "The update is not for this application or schema.");
    version(&manifest.version)?;
    ensure!(lower_hex(&manifest.source_commit,40) && (1..=4).contains(&manifest.assets.len()), "Invalid release source or asset inventory.");
    let mut names = HashSet::new();
    for asset in &manifest.assets { validate_asset(asset,&manifest.version)?; ensure!(names.insert(&asset.name), "The signed update contains duplicate packages."); }
    Ok(UpdateRelease { version: manifest.version, source_commit: manifest.source_commit, assets: manifest.assets })
}

#[cfg(test)]
pub(crate) fn verify_test_manifest(data: &[u8], signature: &[u8], key: &[u8;32]) -> Result<UpdateRelease> {
    verify_with_key(data,signature,key)
}

#[derive(Clone, Default)]
pub struct DownloadCancellation(CancellationToken);
impl DownloadCancellation {
    pub fn new() -> Self { Self::default() }
    pub fn cancel(&self) { self.0.cancel(); }
    pub fn is_cancelled(&self) -> bool { self.0.is_cancelled() }
    fn check(&self) -> Result<()> { ensure!(!self.is_cancelled(), "The update operation was cancelled."); Ok(()) }
}
pub fn trusted_url(text: &str) -> Result<url::Url> {
    // WHATWG parsing erases empty userinfo (https://@github.com), so inspect
    // the original authority before normalizing it into a Url.
    ensure!(!text.bytes().any(|b| b.is_ascii_control()) && !text.contains('\\'),"The update transport refused an untrusted redirect.");
    let authority = text.split_once("://").map(|(_,tail)| tail.split(['/', '?', '#']).next().unwrap_or("")).context("The update transport refused an untrusted redirect.")?;
    ensure!(!authority.is_empty() && !authority.contains('@'),"The update transport refused an untrusted redirect.");
    let url = url::Url::parse(text)?;
    ensure!(url.scheme() == "https" && ALLOWED_HOSTS.contains(&url.host_str().unwrap_or("")) && url.port_or_known_default() == Some(443) && url.username().is_empty() && url.password().is_none(), "The update transport refused an untrusted redirect."); Ok(url)
}
fn redirect_url(base: &url::Url, location: &str) -> Result<url::Url> {
    ensure!(!location.bytes().any(|b| b.is_ascii_control()) && !location.contains('\\'),"The update transport refused an untrusted redirect.");
    let location = location.trim();
    if location.starts_with("//") { return trusted_url(&format!("https:{location}")) }
    if url::Url::parse(location).is_ok() { return trusted_url(location) }
    trusted_url(base.join(location)?.as_str())
}
fn transport() -> Result<reqwest::Client> {
    // Inspect raw Location values ourselves: reqwest's redirect callback only
    // exposes an already-normalized URL and cannot detect erased userinfo.
    Ok(reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).connect_timeout(Duration::from_secs(20)).read_timeout(Duration::from_secs(20)).user_agent("BroTypist-updater/1").build()?)
}
async fn stream(url: &str, limit: u64, expected: Option<u64>, cancel: &DownloadCancellation, mut consume: impl FnMut(&[u8]) -> Result<()>) -> Result<()> {
    let mut destination = trusted_url(url)?; cancel.check()?;
    let client = transport()?; let mut redirects = 0;
    let mut response = loop {
        let response = tokio::select! { biased; _ = cancel.0.cancelled() => bail!("The update operation was cancelled."), r = client.get(destination.clone()).header(reqwest::header::ACCEPT_ENCODING,"identity").send() => r? };
        if matches!(response.status().as_u16(),301|302|303|307|308) {
            ensure!(redirects < 10,"The update transport refused excessive redirects.");
            let location = response.headers().get(reqwest::header::LOCATION).context("The update redirect has no destination.")?.to_str()?;
            destination = redirect_url(&destination,location)?; redirects += 1; cancel.check()?;
        } else { break response }
    };
    ensure!(response.status() == reqwest::StatusCode::OK, "The update server returned an unexpected response.");
    let headers = response.headers();
    ensure!(headers.get(reqwest::header::CONTENT_ENCODING).map(|v| v.to_str().map(|s| s.eq_ignore_ascii_case("identity")).unwrap_or(false)).unwrap_or(true), "The update server returned an unsupported encoding.");
    if let Some(length) = headers.get(reqwest::header::CONTENT_LENGTH) {
        let count: u64 = length.to_str()?.parse()?;
        ensure!(count <= limit && expected.is_none_or(|e| e == count), "The update response size does not match its allowed bounds.");
    }
    let mut received = 0u64;
    loop {
        let chunk = tokio::select! { biased; _ = cancel.0.cancelled() => bail!("The update operation was cancelled."), r = response.chunk() => r? };
        cancel.check()?;
        let Some(chunk) = chunk else { break };
        received = received.checked_add(chunk.len() as u64).context("Update size overflow.")?;
        ensure!(received <= limit, "The update response exceeds its allowed size."); consume(&chunk)?;
    }
    cancel.check()?;
    ensure!(expected.is_none_or(|e| e == received), "The update response was truncated."); Ok(())
}
fn runtime() -> Result<tokio::runtime::Runtime> { Ok(tokio::runtime::Builder::new_current_thread().enable_all().build()?) }
pub fn fetch_release(cancel: &DownloadCancellation) -> Result<(UpdateRelease,Vec<u8>,Vec<u8>)> {
    runtime()?.block_on(async {
        let feed = format!("https://github.com/{REPOSITORY}/releases/latest/download/update-manifest");
        let mut data = Vec::new(); let mut sig = Vec::new();
        stream(&format!("{feed}.json"),MAX_MANIFEST_BYTES as u64,None,cancel,|chunk| { data.extend_from_slice(chunk); Ok(()) }).await?;
        stream(&format!("{feed}.sig"),MAX_SIGNATURE_BYTES as u64,None,cancel,|chunk| { sig.extend_from_slice(chunk); Ok(()) }).await?;
        cancel.check()?; Ok((verify_manifest(&data,&sig)?,data,sig))
    })
}
pub(crate) fn private_directory(path: &Path) -> Result<()> {
    update_install::no_symlink_components(path)?;
    #[cfg(unix)] { use std::os::unix::fs::DirBuilderExt; fs::DirBuilder::new().recursive(true).mode(0o700).create(path)?; }
    #[cfg(not(unix))] fs::create_dir_all(path)?;
    update_install::require_private_directory(path)
}
pub(crate) fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    let parent = path.parent().context("Missing private staging parent.")?;
    update_install::no_symlink_components(path)?;
    let mut temp = tempfile::Builder::new().prefix(".update-record-").tempfile_in(parent)?;
    temp.write_all(data)?; temp.as_file().sync_all()?; temp.persist(path).map_err(|e| e.error)?; File::open(parent)?.sync_all()?; Ok(())
}
struct DownloadSink {
    temporary: tempfile::NamedTempFile,
    destination: PathBuf,
    asset: UpdateAsset,
    received: u64,
    digest: Sha256,
}
impl DownloadSink {
    fn new(destination: &Path, asset: &UpdateAsset) -> Result<Self> {
        let parent = destination.parent().context("Missing download directory.")?;
        private_directory(parent)?; update_install::no_symlink_components(destination)?;
        Ok(Self { temporary:tempfile::Builder::new().prefix(".update-download-").tempfile_in(parent)?,destination:destination.into(),asset:asset.clone(),received:0,digest:Sha256::new() })
    }
    fn write(&mut self, chunk: &[u8], cancel: &DownloadCancellation) -> Result<()> {
        cancel.check()?;
        self.received = self.received.checked_add(chunk.len() as u64).context("Update size overflow.")?;
        ensure!(self.received <= self.asset.size,"The update response exceeds its allowed size.");
        self.temporary.write_all(chunk)?; self.digest.update(chunk); Ok(())
    }
    fn finish(mut self, source: &str, origin: &str, cancel: &DownloadCancellation) -> Result<PathBuf> {
        cancel.check()?;
        ensure!(self.received == self.asset.size && format!("{:x}",self.digest.finalize()) == self.asset.sha256,"The update package failed integrity verification; the previous file was preserved.");
        self.temporary.as_file_mut().flush()?; self.temporary.as_file().sync_all()?;
        update_install::mark_web_download(self.temporary.path(),source,origin)?;
        cancel.check()?; update_install::no_symlink_components(&self.destination)?;
        self.temporary.persist(&self.destination).map_err(|e| e.error)?;
        File::open(self.destination.parent().unwrap())?.sync_all()?; Ok(self.destination)
    }
}
pub fn download_asset(release: &UpdateRelease, asset: &UpdateAsset, destination: &Path, cancel: &DownloadCancellation, mut progress: impl FnMut(u64,u64)) -> Result<PathBuf> {
    version(&release.version)?; ensure!(release.assets.contains(asset), "Selected package is not part of the signed release."); validate_asset(asset,&release.version)?; cancel.check()?;
    let mut sink = DownloadSink::new(destination,asset)?;
    let url = format!("https://github.com/{REPOSITORY}/releases/download/v{}/{}",release.version,asset.name);
    progress(0,asset.size);
    runtime()?.block_on(stream(&url,asset.size,Some(asset.size),cancel,|chunk| {
        sink.write(chunk,cancel)?; progress(sink.received,asset.size); Ok(())
    }))?;
    sink.finish(&url,&release.release_url()?,cancel)
}
fn now() -> f64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs_f64() }
fn recent_check(root: &Path, time: f64) -> bool {
    (|| -> Result<bool> {
        let value = strict_json(&update_install::read_file(&root.join("check.json"),1024,false)?)?;
        let t = value["checked_at"].as_f64().context("Invalid timestamp")?;
        Ok(t.is_finite() && time >= t && time-t < CHECK_INTERVAL.as_secs_f64())
    })().unwrap_or(false)
}
pub fn target() -> Option<&'static str> {
    match (std::env::consts::OS,std::env::consts::ARCH) { ("macos","aarch64") => Some("macos-arm64"), ("linux","x86_64") => Some("linux-x86_64"), _ => None }
}
fn select_asset<'a>(release: &'a UpdateRelease, installation: &Installation) -> Result<&'a UpdateAsset> {
    ensure!(installation.installable || installation.kind == "linux_debian", "This installation cannot be updated in place.");
    let format = match installation.kind.as_str() { "macos_bundle" => "zip", "linux_portable" => "tar.gz", "linux_debian" => "deb", _ => bail!("Updates are not supported for this platform.") };
    let target = target().context("Updates are not supported for this platform.")?;
    release.assets.iter().find(|a| a.target == target && a.format == format).context("This signed release does not contain a compatible update package.")
}
struct State {
    status: Value, closed: bool, next_automatic: Option<Instant>, release: Option<UpdateRelease>, manifest: Vec<u8>, signature: Vec<u8>,
    asset: Option<UpdateAsset>, archive_path: Option<PathBuf>, manifest_path: Option<PathBuf>, signature_path: Option<PathBuf>,
    cancel: DownloadCancellation, worker: Option<JoinHandle<()>>, handoff: Option<Arc<Mutex<Handoff>>>,
}
pub struct UpdateService { state: Arc<Mutex<State>>, root: PathBuf, installation: Installation }
impl UpdateService {
    pub fn new() -> Result<Self> { Self::at(crate::paths::update_dir()?, update_install::detect_installation(),env!("CARGO_PKG_VERSION")) }
    fn at(root: PathBuf, installation: Installation, current: &str) -> Result<Self> {
        version(current)?;
        let status = json!({"state":"idle","current_version":current,"version":"","message":"Updates have not been checked.","received":0,"total":0,"install_kind":installation.kind,"installable":installation.installable,"install_reason":installation.reason,"release_url":""});
        Ok(Self { root, installation, state: Arc::new(Mutex::new(State { status, closed:false, next_automatic:None, release:None, manifest:Vec::new(), signature:Vec::new(), asset:None,archive_path:None,manifest_path:None,signature_path:None,cancel:DownloadCancellation::new(),worker:None,handoff:None })) })
    }
    pub fn snapshot(&self) -> Value { self.state.lock().unwrap().status.clone() }
    fn launch(state: &mut State, shared: Arc<Mutex<State>>, operation: impl FnOnce(DownloadCancellation) -> Result<()> + Send + 'static) -> Result<()> {
        let cancel = DownloadCancellation::new(); state.cancel = cancel.clone();
        state.worker = Some(thread::Builder::new().name("brotypist-update".into()).spawn(move || {
            if operation(cancel.clone()).is_err() {
                let mut s = shared.lock().unwrap();
                if !s.closed { s.status["state"] = if cancel.is_cancelled() { "cancelled" } else { "error" }.into(); s.status["message"] = if cancel.is_cancelled() { "The update operation was cancelled." } else { "The update could not be completed securely. Your installed app was not changed." }.into(); }
            }
        })?); Ok(())
    }
    pub fn start_check(&self) -> Result<()> { self.start_check_with_policy(false) }
    pub fn start_automatic_check(&self) -> Result<()> { self.start_check_with_policy(true) }
    pub fn start_check_with_policy(&self, automatic: bool) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        if automatic && (s.closed || s.next_automatic.is_some_and(|t| Instant::now() < t) || s.status["state"] == "ready") { return Ok(()) }
        ensure!(!s.closed,"The update service has closed.");
        if s.worker.as_ref().is_some_and(|w| !w.is_finished()) || s.status["state"] == "installing" { return Ok(()) }
        if automatic { s.next_automatic = Some(Instant::now()+CHECK_INTERVAL); }
        s.status["state"] = "checking".into(); s.status["message"] = "Checking for signed updates.".into(); s.status["received"] = 0.into(); s.status["total"] = 0.into();
        let root = self.root.clone(); let shared = self.state.clone(); let worker_shared = shared.clone();
        Self::launch(&mut s,shared,move |cancel| {
            cancel.check()?; private_directory(&root)?; let time = now();
            if automatic && recent_check(&root,time) {
                let mut s = worker_shared.lock().unwrap(); cancel.check()?;
                if !s.closed { let state = if let Some(r) = &s.release { if version(&r.version)? > version(s.status["current_version"].as_str().unwrap())? { "available" } else { "current" } } else { "idle" }; s.status["state"] = state.into(); s.status["message"] = "Automatic updates are checked once per day. Use Check for Updates to check now.".into(); } return Ok(())
            }
            cancel.check()?; write_private(&root.join("check.json"),&serde_json::to_vec(&json!({"checked_at":time}))?)?;
            let (release,data,signature) = fetch_release(&cancel)?;
            let mut s = worker_shared.lock().unwrap(); cancel.check()?; if s.closed { return Ok(()) }
            let newer = version(&release.version)? > version(s.status["current_version"].as_str().unwrap())?;
            s.status["state"] = if newer { "available" } else { "current" }.into(); s.status["version"] = release.version.clone().into(); s.status["release_url"] = release.release_url()?.into(); s.status["message"] = if newer { "A signed update is available." } else { "You are running the latest available version." }.into();
            s.release = Some(release); s.manifest = data; s.signature = signature; s.asset = None; s.archive_path = None; Ok(())
        })
    }
    pub fn start_download(&self) -> Result<()> {
        let mut s = self.state.lock().unwrap(); ensure!(!s.closed,"The update service has closed.");
        if s.worker.as_ref().is_some_and(|w| !w.is_finished()) { return Ok(()) }
        ensure!(["available","error","cancelled"].iter().any(|v| s.status["state"] == *v),"Check for a newer signed release before downloading.");
        let release = s.release.clone().context("Check for a newer signed release before downloading.")?;
        ensure!(version(&release.version)? > version(s.status["current_version"].as_str().unwrap())?,"Updates must advance the installed version.");
        let asset = select_asset(&release,&self.installation)?.clone(); let data = s.manifest.clone(); let signature = s.signature.clone();
        ensure!(verify_manifest(&data,&signature)? == release,"The signed release no longer matches the selected update.");
        s.status["state"] = "downloading".into(); s.status["message"] = "Downloading and verifying the update.".into(); s.status["received"] = 0.into(); s.status["total"] = asset.size.into();
        let root = self.root.clone(); let shared = self.state.clone(); let worker_shared = shared.clone();
        Self::launch(&mut s,shared,move |cancel| {
            private_directory(&root)?;
            let mut builder = tempfile::Builder::new(); builder.prefix("release-");
            #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; builder.permissions(fs::Permissions::from_mode(0o700)); }
            let stage = builder.tempdir_in(root)?;
            let manifest_path = stage.path().join("update-manifest.json"); let signature_path = stage.path().join("update-manifest.sig"); let archive_path = stage.path().join(&asset.name);
            write_private(&manifest_path,&data)?; write_private(&signature_path,&signature)?;
            download_asset(&release,&asset,&archive_path,&cancel,|received,total| { let mut s = worker_shared.lock().unwrap(); if !s.closed && !cancel.is_cancelled() { s.status["received"] = received.into(); s.status["total"] = total.into(); } })?;
            let mut s = worker_shared.lock().unwrap(); cancel.check()?; ensure!(!s.closed,"The update operation was cancelled.");
            s.status["state"] = "ready".into(); s.status["message"] = if asset.format == "deb" { "The verified package is ready. Open the system installer to upgrade and restart BroTypist." } else { "The verified update is ready to install and relaunch." }.into();
            s.archive_path = Some(stage.keep().join(&asset.name)); s.asset = Some(asset); s.manifest_path = Some(manifest_path); s.signature_path = Some(signature_path); Ok(())
        })
    }
    pub fn cancel(&self) {
        let mut s = self.state.lock().unwrap();
        if let Some(h) = &s.handoff { if h.lock().unwrap().committed() { return } }
        s.cancel.cancel();
        if s.status["state"] == "installing" { if let Some(h) = s.handoff.take() { h.lock().unwrap().abort(); } s.status["state"] = "ready".into(); s.status["message"] = "Installation was cancelled. The verified update is still ready.".into(); }
    }
    fn settle_worker(&self) -> Result<()> {
        let deadline = Instant::now()+Duration::from_millis(250);
        loop {
            let mut s = self.state.lock().unwrap();
            if s.worker.as_ref().is_none_or(|w| w.is_finished()) { if let Some(w) = s.worker.take() { w.join().map_err(|_| anyhow!("Update worker terminated unexpectedly."))?; } return Ok(()) }
            drop(s); ensure!(Instant::now() < deadline,"Finishing cancellation of the update operation."); thread::sleep(Duration::from_millis(5));
        }
    }
    pub fn prepare_install(&self) -> Result<Arc<Mutex<Handoff>>> {
        self.settle_worker()?; let mut s = self.state.lock().unwrap(); ensure!(!s.closed,"The update service has closed.");
        ensure!(s.status["state"] == "ready" && s.asset.as_ref().is_some_and(|a| a.format != "deb"),"A verified self-installable update must be ready first.");
        let current = update_install::detect_installation(); ensure!(current == self.installation && current.installable,"The installation identity or update policy changed. Check for updates again.");
        let h = Arc::new(Mutex::new(update_install::spawn_installer(s.manifest_path.as_ref().context("Missing manifest")?,s.signature_path.as_ref().context("Missing signature")?,s.archive_path.as_ref().context("Missing archive")?,&current)?));
        s.handoff = Some(h.clone()); s.status["state"] = "installing".into(); s.status["message"] = "Waiting for approved app shutdown before installing.".into(); Ok(h)
    }
    pub fn open_package_installer(&self) -> Result<()> {
        let mut s = self.state.lock().unwrap(); ensure!(!s.closed && s.status["state"] == "ready" && self.installation.kind == "linux_debian","A verified Debian package must be ready first.");
        let release = verify_manifest(&update_install::read_file(s.manifest_path.as_ref().context("Missing manifest")?,MAX_MANIFEST_BYTES,true)?,&update_install::read_file(s.signature_path.as_ref().context("Missing signature")?,MAX_SIGNATURE_BYTES,true)?)?;
        let asset = s.asset.as_ref().context("Missing asset")?; ensure!(asset.format == "deb" && s.release.as_ref() == Some(&release) && release.assets.contains(asset),"The package identity changed.");
        let archive = s.archive_path.as_ref().context("Missing package")?; update_install::verify_package(archive,asset,true)?;
        std::process::Command::new("xdg-open").arg(archive).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn()?;
        s.status["message"] = "Opening the verified package was requested. Complete the upgrade in your system installer, then restart BroTypist.".into(); Ok(())
    }
    pub fn close(&self) -> Result<()> { { let mut s = self.state.lock().unwrap(); s.closed = true; s.cancel.cancel(); if let Some(h) = &s.handoff { h.lock().unwrap().abort(); } } self.settle_worker() }
}
impl Drop for UpdateService { fn drop(&mut self) { let _ = self.close(); } }

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    fn manifest() -> Value { json!({"schema":1,"application":APP_ID,"version":"99.2.0","source_commit":"a".repeat(40),"assets":[{"name":"BroTypist-99.2.0-macos-arm64.zip","target":"macos-arm64","format":"zip","size":7,"sha256":"b".repeat(64)}]}) }
    fn signed(data: &[u8]) -> (Vec<u8>,[u8;32]) { let key = SigningKey::from_bytes(&[31;32]); (STANDARD.encode(key.sign(data).to_bytes()).into_bytes(),key.verifying_key().to_bytes()) }
    fn verify(value: &Value) -> Result<UpdateRelease> { let data = serde_json::to_vec(value)?; let (sig,key) = signed(&data); verify_with_key(&data,&sig,&key) }
    #[test] fn exact_signature_bytes_and_pinned_key() {
        let data = serde_json::to_vec(&manifest()).unwrap(); let (sig,key) = signed(&data);
        assert_eq!(verify_with_key(&data,&sig,&key).unwrap().version,"99.2.0");
        let mut changed = data.clone(); changed.push(b' '); assert!(verify_with_key(&changed,&sig,&key).is_err()); assert!(verify_manifest(&data,&sig).is_err());
        let padded = [b"\r\n".as_slice(),sig.as_slice(),b"\n".as_slice()].concat(); assert!(verify_with_key(&data,&padded,&key).is_ok());
        let internal = [sig[..20].to_vec(),b" ".to_vec(),sig[20..].to_vec()].concat(); assert!(verify_with_key(&data,&internal,&key).is_err());
    }
    #[test] fn duplicate_fields_and_packages_rejected_recursively() {
        for text in [r#"{"a":{"b":1,"b":2}}"#,r#"[{"a":1,"a":2}]"#] { assert!(strict_json(text.as_bytes()).is_err()); }
        let mut value = manifest(); let asset = value["assets"][0].clone(); value["assets"].as_array_mut().unwrap().push(asset); assert!(verify(&value).is_err());
        let text = serde_json::to_string(&manifest()).unwrap().replace("\"schema\":1","\"schema\":1,\"schema\":1"); let (sig,key) = signed(text.as_bytes()); assert!(verify_with_key(text.as_bytes(),&sig,&key).is_err());
    }
    #[test] fn signed_schema_and_asset_bounds_are_exact() {
        for (field,value) in [("schema",json!(true)),("schema",json!(1.0)),("application",json!("other")),("version",json!("01.2.3")),("version",json!("1.2.3-beta")),("source_commit",json!("A".repeat(40))),("extra",json!(1)),("assets",json!([]))] { let mut m = manifest(); m[field] = value; assert!(verify(&m).is_err(),"{field}"); }
        for (field,value) in [("size",json!(0)),("size",json!(true)),("size",json!(MAX_PACKAGE_BYTES+1)),("sha256",json!("A".repeat(64))),("target",json!("windows-x86_64")),("name",json!("different.zip")),("extra",json!(1))] { let mut m = manifest(); m["assets"][0][field] = value; assert!(verify(&m).is_err(),"{field}"); }
        let data = vec![b'x';MAX_MANIFEST_BYTES+1]; assert!(verify_manifest(&data,b"x").is_err());
    }
    #[test] fn stable_versions_compare_numerically() { assert!(version("99.10.0").unwrap() > version("99.9.0").unwrap()); for s in ["1.2","-1.2.3","1.2.3.4","1.2.03","1000000000.1.1"] { assert!(version(s).is_err()); } }
    #[test] fn transport_allowlist_is_https_credential_free() {
        for url in ["http://github.com/a","https://evil.test/a","https://github.com.evil.test/a","https://user@github.com/a","https://@github.com/a","https://:@github.com/a","https:////@github.com/a","https://github.com:444/a","file:///a"] { assert!(trusted_url(url).is_err(),"{url}"); }
        for url in ["https://github.com/a","https://release-assets.githubusercontent.com/a","https://objects.githubusercontent.com/a","https://github-releases.githubusercontent.com:443/a"] { assert!(trusted_url(url).is_ok()); }
    }
    #[test] fn raw_redirect_authorities_are_checked_before_url_normalization() {
        let base=trusted_url("https://github.com/releases/file").unwrap();
        for location in ["https://@github.com/file","//@github.com/file","https://user@github.com/file","//evil.test/file","http://github.com/file","https:\\\\github.com/file","\t//@github.com/file"] { assert!(redirect_url(&base,location).is_err(),"{location}"); }
        assert_eq!(redirect_url(&base,"../next").unwrap().as_str(),"https://github.com/next");
        assert_eq!(redirect_url(&base,"//objects.githubusercontent.com/file").unwrap().as_str(),"https://objects.githubusercontent.com/file");
        assert_eq!(redirect_url(&base,"/file?note=@host").unwrap().as_str(),"https://github.com/file?note=@host");
    }
    #[cfg(unix)]
    #[test] fn existing_nonprivate_staging_is_refused_without_changing_permissions() {
        let temp=tempfile::tempdir().unwrap(); let root=temp.path().canonicalize().unwrap();
        fs::set_permissions(&root,fs::Permissions::from_mode(0o755)).unwrap();
        assert!(private_directory(&root).is_err());
        assert_eq!(fs::metadata(&root).unwrap().permissions().mode() & 0o777,0o755);
    }
    #[test] fn constructor_snapshot_and_close_do_not_touch_profile() {
        let temp = tempfile::tempdir().unwrap(); let root = temp.path().join("absent");
        let service = UpdateService::at(root.clone(),Installation { path:None,kind:"held".into(),installable:false,reason:"Held".into() },"0.4.13").unwrap();
        let mut snapshot = service.snapshot(); snapshot["state"] = "wrong".into(); assert_eq!(service.snapshot()["state"],"idle"); service.close().unwrap(); assert!(!root.exists()); assert!(service.start_check().is_err());
    }
    #[cfg(unix)]
    #[test] fn finite_daily_record_bounds() {
        let root = tempfile::tempdir().unwrap(); let path = root.path().canonicalize().unwrap(); for t in [1000.,999.,1001.,-90000.] { write_private(&path.join("check.json"),&serde_json::to_vec(&json!({"checked_at":t})).unwrap()).unwrap(); assert_eq!(recent_check(&path,1000.),t == 1000. || t == 999.); }
    }
    #[test] fn source_and_held_installations_never_select_assets() { let r = verify(&manifest()).unwrap(); for kind in ["source","held","read_only"] { let i = Installation { path:None,kind:kind.into(),installable:false,reason:String::new() }; assert!(select_asset(&r,&i).is_err()); } }
    #[cfg(unix)]
    #[test] fn verified_stream_sink_promotes_only_complete_hash_matched_uncancelled_bytes() {
        let temp = tempfile::Builder::new().permissions(fs::Permissions::from_mode(0o700)).tempdir().unwrap(); let root = temp.path().canonicalize().unwrap();
        let path = root.join("download"); let payload = b"verified archive";
        let asset = UpdateAsset { name:String::new(),target:String::new(),format:String::new(),size:payload.len() as u64,sha256:format!("{:x}",Sha256::digest(payload)) };
        fs::write(&path,b"previous").unwrap(); let cancel=DownloadCancellation::new();
        for data in [b"tampered archive".as_slice(),b"short".as_slice()] {
            let mut sink=DownloadSink::new(&path,&asset).unwrap(); sink.write(data,&cancel).unwrap();
            assert!(sink.finish("https://github.com/release","https://github.com/tag",&cancel).is_err()); assert_eq!(fs::read(&path).unwrap(),b"previous");
        }
        let mut sink=DownloadSink::new(&path,&asset).unwrap(); assert!(sink.write(b"much larger than the signed package",&cancel).is_err()); drop(sink); assert_eq!(fs::read(&path).unwrap(),b"previous");
        let mut sink=DownloadSink::new(&path,&asset).unwrap(); sink.write(payload,&cancel).unwrap(); cancel.cancel();
        assert!(sink.finish("https://github.com/release","https://github.com/tag",&cancel).is_err()); assert_eq!(fs::read(&path).unwrap(),b"previous");
        let cancel=DownloadCancellation::new(); let mut sink=DownloadSink::new(&path,&asset).unwrap(); sink.write(&payload[..4],&cancel).unwrap(); sink.write(&payload[4..],&cancel).unwrap();
        sink.finish("https://github.com/release","https://github.com/tag",&cancel).unwrap(); assert_eq!(fs::read(&path).unwrap(),payload);
        let linked=root.join("linked"); std::os::unix::fs::symlink(&path,&linked).unwrap(); assert!(DownloadSink::new(&linked,&asset).is_err()); assert_eq!(fs::read(&path).unwrap(),payload);
        let mut sink=DownloadSink::new(&root.join("late-link"),&asset).unwrap(); sink.write(payload,&cancel).unwrap(); std::os::unix::fs::symlink(&path,root.join("late-link")).unwrap();
        assert!(sink.finish("https://github.com/release","https://github.com/tag",&cancel).is_err()); assert_eq!(fs::read(&path).unwrap(),payload);
    }
    #[cfg(unix)]
    #[test] fn persisted_daily_attempt_skips_transport_and_readies_no_download() {
        let temp=tempfile::Builder::new().permissions(fs::Permissions::from_mode(0o700)).tempdir().unwrap(); let root=temp.path().canonicalize().unwrap();
        write_private(&root.join("check.json"),&serde_json::to_vec(&json!({"checked_at":now()})).unwrap()).unwrap();
        let service=UpdateService::at(root,Installation { path:None,kind:"source".into(),installable:false,reason:"Source".into() },"0.4.13").unwrap();
        service.start_automatic_check().unwrap();
        let deadline=Instant::now()+Duration::from_secs(2); while service.snapshot()["state"] == "checking" && Instant::now()<deadline { thread::sleep(Duration::from_millis(5)); }
        assert_eq!(service.snapshot()["state"],"idle"); assert!(service.snapshot()["message"].as_str().unwrap().contains("once per day")); service.close().unwrap();
    }
    #[test] fn service_single_flight_and_cancel_own_one_worker() {
        let temp=tempfile::tempdir().unwrap();
        let service=UpdateService::at(temp.path().join("unused"),Installation { path:None,kind:"source".into(),installable:false,reason:String::new() },"0.4.13").unwrap();
        let shared=service.state.clone();
        let (entered,ready)=std::sync::mpsc::channel();
        {
            let mut state=service.state.lock().unwrap(); state.status["state"]="checking".into();
            UpdateService::launch(&mut state,shared,move |cancel| { entered.send(()).unwrap(); while !cancel.is_cancelled() { thread::sleep(Duration::from_millis(2)); } cancel.check() }).unwrap();
        }
        ready.recv_timeout(Duration::from_secs(1)).unwrap(); service.start_check().unwrap(); service.start_download().unwrap(); service.cancel();
        service.settle_worker().unwrap(); assert_eq!(service.snapshot()["state"],"cancelled"); service.close().unwrap(); assert!(!temp.path().join("unused").exists());
    }
}
