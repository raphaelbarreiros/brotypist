//! Wire validation and lossless native/scalar offset conversion.
use crate::core::Snapshot;
use anyhow::{Result, bail, ensure};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use zbus::zvariant::{OwnedValue, Value};

pub(super) type Dict = HashMap<String, OwnedValue>;
pub(super) const LEASE: u64 = 2_000_000_000;
pub(super) const MAX_TEXT: usize = 65_536;
pub(super) const MAX_CLIENTS: usize = 32;
pub(super) const MAX_QUEUE: usize = 128;

pub(super) fn now() -> u64 {
    let mut t = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // CLOCK_MONOTONIC is shared with the target-owned native clients.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) } != 0 {
        return 0;
    }
    (t.tv_sec as u64)
        .saturating_mul(1_000_000_000)
        .saturating_add(t.tv_nsec as u64)
}
pub(super) fn bounded_text(text: &str) -> bool {
    text.len() <= MAX_TEXT && text.chars().count() <= 16_000 && !text.contains('\0')
}
pub(super) fn bounded(map: &Dict) -> bool {
    fn value(v: &Value<'_>, depth: usize, remaining: &mut usize) -> bool {
        if depth > 4 || *remaining < 8 {
            return false;
        }
        *remaining -= 8;
        match v {
            Value::Str(s) => {
                let n = s.len();
                if n > MAX_TEXT || n > *remaining {
                    return false;
                }
                *remaining -= n;
                true
            }
            Value::Value(v) => value(v, depth + 1, remaining),
            Value::Array(a) => a.len() <= 256 && a.iter().all(|v| value(v, depth + 1, remaining)),
            Value::Dict(d) => {
                d.iter().count() <= 64
                    && d.iter().all(|(k, v)| {
                        value(k, depth + 1, remaining) && value(v, depth + 1, remaining)
                    })
            }
            Value::Bool(_) | Value::U32(_) | Value::U64(_) => true,
            _ => false,
        }
    }
    let mut remaining = 512 * 1024;
    map.len() <= 64
        && map
            .iter()
            .all(|(k, v)| k.len() <= 64 && value(v, 0, &mut remaining))
}
pub(super) fn string<'a>(d: &'a Dict, key: &str) -> Result<&'a str> {
    d.get(key)
        .and_then(|v| <&str>::try_from(v).ok())
        .ok_or_else(|| anyhow::anyhow!("Invalid native string: {key}"))
}
pub(super) fn number(d: &Dict, key: &str) -> Result<u64> {
    d.get(key)
        .and_then(|v| u64::try_from(v).ok())
        .ok_or_else(|| anyhow::anyhow!("Invalid native counter: {key}"))
}
pub(super) fn offset(d: &Dict, key: &str) -> Result<usize> {
    d.get(key)
        .and_then(|v| u32::try_from(v).ok())
        .map(|n| n as usize)
        .ok_or_else(|| anyhow::anyhow!("Invalid native offset: {key}"))
}
pub(super) fn flag(d: &Dict, key: &str) -> Result<bool> {
    d.get(key)
        .and_then(|v| bool::try_from(v).ok())
        .ok_or_else(|| anyhow::anyhow!("Invalid native flag: {key}"))
}
pub(super) fn nested(d: &Dict, key: &str) -> Result<Dict> {
    Ok(HashMap::try_from(
        d.get(key)
            .ok_or_else(|| anyhow::anyhow!("Missing native witness"))?
            .try_clone()?,
    )?)
}
pub(super) fn strings(d: &Dict, key: &str) -> Result<Vec<String>> {
    Ok(Vec::try_from(
        d.get(key)
            .ok_or_else(|| anyhow::anyhow!("Missing native list"))?
            .try_clone()?,
    )?)
}
pub(super) fn put(d: &mut Dict, key: &str, v: impl Into<OwnedValue>) {
    d.insert(key.into(), v.into());
}
pub(super) fn text(d: &mut Dict, key: &str, v: &str) {
    put(d, key, zbus::zvariant::Str::from(v.to_owned()));
}
pub(super) fn list(d: &mut Dict, key: &str, values: &[String]) -> Result<()> {
    d.insert(
        key.into(),
        OwnedValue::try_from(Value::from(values.to_vec()))?,
    );
    Ok(())
}
pub(super) fn hash(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}
pub(super) fn token(d: &Dict, key: &str) -> Result<String> {
    let s = string(d, key)?;
    ensure!(
        !s.is_empty()
            && s.len() <= 256
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_:./".contains(&b)),
        "Invalid native identity"
    );
    Ok(s.into())
}
pub(super) fn window_identity(client: u64, window: &str) -> String {
    format!("client:{client}:{}:{window}", window.len())
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FocusScope {
    pub field: String,
    pub window: String,
    pub document: String,
    pub generation: u64,
}
impl FocusScope {
    pub fn parse(metadata: &Dict) -> Result<Self> {
        Ok(Self {
            field: token(metadata, "field_token")?,
            window: token(metadata, "window_token")?,
            document: token(metadata, "document_token")?,
            generation: number(metadata, "focus_generation")?,
        })
    }
    pub fn write(&self, policy: &mut Dict) {
        text(policy, "field_token", &self.field);
        text(policy, "window_token", &self.window);
        text(policy, "document_token", &self.document);
        put(policy, "focus_generation", self.generation);
    }
    pub fn matches(&self, witness: &Witness) -> bool {
        self.field == witness.field
            && self.window == witness.window
            && self.document == witness.document
    }
}
pub(super) fn pending_edit(metadata: &Dict) -> bool {
    metadata
        .get("pending_edit")
        .is_some_and(|value| bool::try_from(value).unwrap_or(true))
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Witness {
    pub field: String,
    pub window: String,
    pub document: String,
    pub revision: u64,
    pub user: u64,
    pub notification: u64,
    pub unit: String,
    pub text: String,
    pub caret: usize,
    pub anchor: usize,
    pub purpose: String,
    pub domain: String,
}
impl Witness {
    pub fn parse(d: &Dict) -> Result<Self> {
        ensure!(
            bounded(d) && flag(d, "focus")? && !flag(d, "protected")? && flag(d, "editable")?,
            "Native target refused"
        );
        ensure!(
            string(d, "composing")? == "idle",
            "Native composition is not idle"
        );
        let purpose = string(d, "purpose")?;
        ensure!(safe_purpose(purpose), "Protected native purpose");
        let text = string(d, "text")?;
        ensure!(bounded_text(text), "Native text exceeds bounds");
        let unit = string(d, "offset_unit")?;
        ensure!(matches!(unit, "scalar" | "utf16"), "Unknown native offsets");
        let caret = offset(d, "caret")?;
        let anchor = offset(d, "anchor")?;
        scalar(text, unit, caret)?;
        scalar(text, unit, anchor)?;
        let domain = domain(string(d, "domain")?)?;
        Ok(Self {
            field: token(d, "field_token")?,
            window: token(d, "window_token")?,
            document: token(d, "document_token")?,
            revision: number(d, "revision")?,
            user: number(d, "user_revision")?,
            notification: number(d, "notification_revision")?,
            unit: unit.into(),
            text: text.into(),
            caret,
            anchor,
            purpose: purpose.into(),
            domain,
        })
    }
    pub fn snapshot(&self, id: u64, app: &str, pid: u32) -> Result<Snapshot> {
        let c = scalar(&self.text, &self.unit, self.caret)?;
        let a = scalar(&self.text, &self.unit, self.anchor)?;
        // document_token may denote one native Qt block, not the whole field.
        let field = format!(
            "client:{id}:{}:{}:{}:{}",
            self.field.len(),
            self.field,
            self.document.len(),
            self.document
        );
        let window = window_identity(id, &self.window);
        Ok(Snapshot {
            app_id: app.into(),
            field_id: field.clone(),
            text: self.text.clone().into(),
            caret: c.min(a),
            selection_length: c.abs_diff(a),
            domain: self.domain.clone(),
            native_anchor: format!("{field}:{}:{}", self.revision, self.user),
            native_inline: true,
            window_id: window,
            window_revision: self.document.clone(),
            window_pid: i32::try_from(pid)?,
            can_replace: true,
            ..Default::default()
        })
    }
    pub fn matches(&self, other: &Self) -> bool {
        self.field == other.field
            && self.window == other.window
            && self.document == other.document
            && self.revision == other.revision
            && self.user == other.user
            && self.unit == other.unit
            && self.text == other.text
            && self.caret == other.caret
            && self.anchor == other.anchor
            && self.purpose == other.purpose
            && self.domain == other.domain
    }
    pub fn write_offer(&self, d: &mut Dict) {
        for (k, v) in [
            ("field_token", self.field.as_str()),
            ("window_token", &self.window),
            ("document_token", &self.document),
            ("offset_unit", &self.unit),
            ("text_sha256", &hash(&self.text)),
        ] {
            text(d, k, v);
        }
        put(d, "revision", self.revision);
        put(d, "user_revision", self.user);
        put(d, "caret", self.caret as u32);
        put(d, "anchor", self.anchor as u32);
    }
}
pub(super) fn safe_purpose(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && !matches!(s, "password" | "pin" | "sensitive" | "hidden" | "unknown")
}
pub(super) fn domain(s: &str) -> Result<String> {
    ensure!(s.len() <= 253, "Invalid native domain");
    if s.is_empty() {
        Ok(String::new())
    } else {
        crate::config::normalize_domain(s)
    }
}
pub(super) fn scalar(text: &str, unit: &str, offset: usize) -> Result<usize> {
    ensure!(
        matches!(unit, "scalar" | "utf16"),
        "Invalid native offset unit"
    );
    let mut units = 0;
    let mut scalars = 0;
    for c in text.chars() {
        if units == offset {
            return Ok(scalars);
        }
        units += if unit == "utf16" { c.len_utf16() } else { 1 };
        scalars += 1;
        ensure!(units <= offset, "Split native surrogate");
    }
    if units == offset {
        Ok(scalars)
    } else {
        bail!("Invalid native offset")
    }
}
pub(super) fn native(text: &str, unit: &str, offset: usize) -> Result<usize> {
    ensure!(
        matches!(unit, "scalar" | "utf16"),
        "Invalid native offset unit"
    );
    let mut units = 0;
    let mut scalars = 0;
    for (index, c) in text.chars().enumerate() {
        if index == offset {
            return Ok(units);
        }
        units += if unit == "utf16" { c.len_utf16() } else { 1 };
        scalars += 1;
    }
    ensure!(scalars == offset, "Invalid scalar offset");
    Ok(units)
}
pub(super) fn lease_valid(accepted: u64, issued: u64, expires: u64, received: u64) -> bool {
    accepted >= issued
        && accepted < expires
        && accepted <= received
        && received.saturating_sub(accepted) <= LEASE
}
