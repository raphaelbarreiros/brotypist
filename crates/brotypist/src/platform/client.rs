//! Target-owned Linux field transport. The fallback retains its private native
//! connection; a cooperating target's refusal never grants fallback access.
mod protocol;
mod server;
#[cfg(test)]
mod tests;
use super::{
    Action, ContextRead, DetachedContextRead, EditOutcome, Foreground, NativeBridge, NativeChoices,
    NativeClientAcceptance, NativeClientOffer, NativeHostDismiss, NativeHostEdit, NativeHostEvent,
    NativeHostPresentation, NativeHostUncertain, NativePreedit, Permission, ShortcutHandler,
};
use crate::{
    config::Config,
    core::{Rect, Snapshot},
};
use anyhow::{Result, ensure};
use protocol::*;
use server::{Message, Peer, Server, State};
use std::{
    collections::{HashMap, VecDeque},
    io::Read,
    os::{fd::AsRawFd, unix::net::UnixStream},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use zbus::blocking::{Connection, Proxy, connection::Builder};

const RPC_DEADLINE: Duration = Duration::from_millis(180);
const POLICY_LEASE: u64 = 6_000_000_000;

pub fn wrap(config: &Config, base: Result<Box<dyn NativeBridge>>) -> Result<Box<dyn NativeBridge>> {
    compose_transports(base, Broker::open(config))
}
fn compose_transports(
    base: Result<Box<dyn NativeBridge>>,
    broker: Result<Broker>,
) -> Result<Box<dyn NativeBridge>> {
    match broker {
        Ok(broker) => {
            let (base, base_error) = match base {
                Ok(base) => (Some(base), None),
                Err(error) => (None, Some(format!("{error:#}"))),
            };
            Ok(Box::new(ClientBridge {
                base,
                base_error,
                broker,
                input_callback: None,
                shortcuts_enabled: false,
                client_owned: false,
                stopped: false,
            }))
        }
        // Keep the exact real fallback object, private FD, or original failure.
        Err(_) => base,
    }
}
fn bus_name() -> Result<String> {
    let name = match std::env::var("BROTYPIST_NATIVE_BUS") {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => {
            let path = crate::paths::config_dir()?;
            let digest = format!(
                "{:x}",
                <sha2::Sha256 as sha2::Digest>::digest(path.as_os_str().as_encoded_bytes())
            );
            format!(
                "org.brotypist.Native1.u{}.p{}",
                unsafe { libc::geteuid() },
                &digest[..24]
            )
        }
        Err(error) => return Err(error.into()),
    };
    ensure!(
        name.len() <= 255
            && !name.starts_with(':')
            && zbus::names::WellKnownName::try_from(name.as_str()).is_ok(),
        "Invalid BROTYPIST_NATIVE_BUS"
    );
    Ok(name)
}
fn consented(config: &Config, peer: &Peer, domain: &str) -> bool {
    super::policy::ObservationPolicy::new(config)
        .is_ok_and(|p| p.allowed(&peer.app, domain, &[&peer.executable, &peer.app], false))
}
fn permitted(config: &Config, peer: &Peer, meta: &Dict) -> bool {
    let Ok(domain) = string(meta, "domain").and_then(domain) else {
        return false;
    };
    matches!(flag(meta, "focus"), Ok(true))
        && matches!(flag(meta, "protected"), Ok(false))
        && matches!(flag(meta, "editable"), Ok(true))
        && string(meta, "purpose").is_ok_and(safe_purpose)
        && consented(config, peer, &domain)
}
fn confirmation_consent(
    config: &Config,
    current_epoch: u64,
    offer_epoch: u64,
    peer: &Peer,
    meta: &Dict,
) -> bool {
    current_epoch == offer_epoch
        && matches!(flag(meta, "protected"), Ok(false))
        && string(meta, "domain")
            .and_then(domain)
            .is_ok_and(|d| consented(config, peer, &d))
}
fn policy_live(current: u64, issued: u64, expires: u64, at: u64) -> bool {
    current == issued && at != 0 && expires > at
}
fn action_allowed(config: &Config, peer: &Peer, meta: &Dict, action: &Action) -> bool {
    if matches!(action, Action::Toggle | Action::AppToggle) {
        return true;
    }
    if pending_edit(meta) {
        return false;
    }
    if !permitted(config, peer, meta) || !matches!(string(meta, "composing"), Ok("idle")) {
        return false;
    }
    let Ok(domain) = string(meta, "domain").and_then(domain) else {
        return false;
    };
    let Ok(p) = super::policy::ObservationPolicy::new(config) else {
        return false;
    };
    match action {
        Action::Force => p.rule_bool(&peer.app, &domain, "enabled", config.enabled),
        Action::Alternatives => p.rule_bool(
            &peer.app,
            &domain,
            "alternatives_enabled",
            config.alternatives_enabled,
        ),
        Action::Synonyms => p.rule_bool(
            &peer.app,
            &domain,
            "synonyms_enabled",
            config.synonyms_enabled,
        ),
        Action::Settings => true,
        _ => false,
    }
}
fn inert_policy(policy: &mut Dict) {
    for key in [
        "enabled",
        "predictions",
        "emoji",
        "synonyms",
        "alternatives",
        "correction",
    ] {
        put(policy, key, false);
    }
    put(policy, "expires_ns", 0u64);
}
fn gate_shortcut_policy(policy: &mut Dict, enabled: bool) {
    if !enabled {
        inert_policy(policy);
    }
}
fn client_actions_live(enabled: bool, current: u64, issued: u64, expires: u64, at: u64) -> bool {
    enabled && policy_live(current, issued, expires, at)
}
fn policy(config: &Config, epoch: u64, peer: &Peer, meta: &Dict, at: u64) -> Result<Dict> {
    let mut d = Dict::new();
    let enabled = permitted(config, peer, meta);
    put(&mut d, "enabled", enabled);
    put(&mut d, "policy_epoch", epoch);
    put(&mut d, "expires_ns", at.saturating_add(POLICY_LEASE));
    text(&mut d, "escape_behavior", &config.escape_behavior);
    put(&mut d, "escape_pause_seconds", config.escape_pause_seconds);
    let domain = domain(string(meta, "domain")?)?;
    let p = super::policy::ObservationPolicy::new(config)?;
    for (key, rule, default) in [
        ("predictions", "enabled", config.enabled),
        ("emoji", "emoji_enabled", config.emoji_enabled),
        ("synonyms", "synonyms_enabled", config.synonyms_enabled),
        (
            "alternatives",
            "alternatives_enabled",
            config.alternatives_enabled,
        ),
        ("correction", "typo_fixes", config.typo_fixes),
    ] {
        put(
            &mut d,
            key,
            enabled && p.rule_bool(&peer.app, &domain, rule, default),
        );
    }
    let mut bindings = HashMap::<String, String>::new();
    for name in [
        "word",
        "all",
        "force",
        "app_toggle",
        "toggle",
        "alternatives",
        "synonyms",
    ] {
        if let Some(value) = config.shortcuts.get(name) {
            let mut binding = crate::shortcuts::normalize_shortcut(value)?;
            if !p.rule_bool(&peer.app, &domain, "tab_enabled", true)
                && binding.rsplit('+').next() == Some("Tab")
            {
                binding.clear();
            }
            bindings.insert(name.into(), binding);
        }
    }
    put(&mut d, "shortcuts", bindings);
    Ok(d)
}
#[derive(Clone)]
struct Target {
    id: u64,
    peer: Peer,
    path: String,
}
struct Frozen {
    target: Target,
    before: Witness,
    snapshot: Snapshot,
    presentation: Option<NativeHostPresentation>,
    offer_id: u64,
    capability: u64,
    issued: u64,
    expires: u64,
    epoch: u64,
    mode: String,
    start: usize,
    length: usize,
    shown_text: String,
    shown_values: Vec<String>,
    word: String,
    all: String,
    armed_at: Option<u64>,
    revoked_at: Option<u64>,
}
fn receipt_live(receipt: &Frozen, enabled: bool, epoch: u64, active: Option<u64>, at: u64) -> bool {
    enabled
        && at != 0
        && receipt.revoked_at.is_none()
        && receipt.epoch == epoch
        && receipt.expires > at
        && active == Some(receipt.target.id)
}
#[derive(Default)]
struct UserInputFence {
    seen: HashMap<u64, u64>,
    revision: u64,
    timestamp: Option<Instant>,
}
impl UserInputFence {
    fn advance(&mut self, at: Instant) {
        self.revision = self.revision.saturating_add(1);
        self.timestamp = Some(at);
    }
    fn seed(&mut self, id: u64, user: u64) {
        self.seen.entry(id).or_insert(user);
    }
    fn observe(&mut self, id: u64, user: u64, at: Instant) -> bool {
        let changed = self.seen.insert(id, user).is_some_and(|old| old != user);
        if changed {
            self.advance(at);
        }
        changed
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct FocusOwner {
    client: u64,
    scope: FocusScope,
}
fn bind_focus_policy(
    policy: &mut Dict,
    client: u64,
    scope: Option<&FocusScope>,
    ready: Option<&FocusOwner>,
) {
    match (scope, ready) {
        (Some(scope), Some(owner)) if owner.client == client && owner.scope == *scope => {
            scope.write(policy)
        }
        _ => inert_policy(policy),
    }
}
struct Broker {
    connection: Connection,
    state: Arc<Mutex<State>>,
    wake: UnixStream,
    bus: String,
    config: Config,
    epoch: u64,
    active: Option<Target>,
    witness: Option<Witness>,
    snapshot: Option<Snapshot>,
    offers: VecDeque<Frozen>,
    events: VecDeque<NativeHostEvent>,
    acceptance: HashMap<u64, u64>,
    user_input: UserInputFence,
    serial: u64,
    revision: u64,
    timestamp: Option<Instant>,
    pending_input: bool,
    shortcuts_enabled: bool,
    last_probe: Instant,
    last_policy: u64,
    connected: bool,
    secure: Option<bool>,
    ambiguous: bool,
    keys_ready: Option<FocusOwner>,
}
impl Broker {
    fn open(config: &Config) -> Result<Self> {
        let bus = bus_name()?;
        let (wake, writer) = UnixStream::pair()?;
        wake.set_nonblocking(true)?;
        writer.set_nonblocking(true)?;
        let state = Arc::new(Mutex::new(State {
            config: config.clone(),
            epoch: 1,
            clients: HashMap::new(),
            queue: VecDeque::new(),
            next: 0,
            stopped: false,
            overflowed: false,
        }));
        let connection = Builder::session()?
            .method_timeout(RPC_DEADLINE)
            .max_queued(MAX_QUEUE)
            .allow_name_replacements(false)
            .replace_existing_names(false)
            .name(bus.as_str())?
            .serve_at(
                "/org/brotypist/Native1",
                Server {
                    state: state.clone(),
                    wake: Arc::new(writer),
                },
            )?
            .build()?;
        Ok(Self {
            connection,
            state,
            wake,
            bus,
            config: config.clone(),
            epoch: 1,
            active: None,
            witness: None,
            snapshot: None,
            offers: VecDeque::new(),
            events: VecDeque::new(),
            acceptance: HashMap::new(),
            user_input: UserInputFence::default(),
            serial: 0,
            revision: 0,
            timestamp: None,
            pending_input: false,
            shortcuts_enabled: false,
            last_probe: Instant::now() - Duration::from_secs(1),
            last_policy: 0,
            connected: true,
            secure: None,
            ambiguous: false,
            keys_ready: None,
        })
    }
    fn proxy<'a>(&'a self, t: &'a Target) -> Result<Proxy<'a>> {
        Ok(Proxy::new(
            &self.connection,
            t.peer.sender.as_str(),
            t.path.as_str(),
            "org.brotypist.Client1",
        )?)
    }
    fn focus_owner(&self) -> Option<FocusOwner> {
        if self.ambiguous {
            return None;
        }
        let client = self.active.as_ref()?.id;
        let state = self.state.lock().ok()?;
        let metadata = &state.clients.get(&client)?.metadata;
        if !matches!(flag(metadata, "focus"), Ok(true)) {
            return None;
        }
        Some(FocusOwner {
            client,
            scope: FocusScope::parse(metadata).ok()?,
        })
    }
    fn targets(&self) -> Vec<Target> {
        self.state
            .lock()
            .map(|s| {
                s.clients
                    .iter()
                    .map(|(id, c)| Target {
                        id: *id,
                        peer: c.peer.clone(),
                        path: c.path.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
    fn authenticate(&self, t: &Target) -> bool {
        let result = (|| -> Result<Peer> {
            let p = Proxy::new(
                &self.connection,
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
            )?;
            let uid: u32 = p.call("GetConnectionUnixUser", &(t.peer.sender.as_str(),))?;
            let pid: u32 = p.call("GetConnectionUnixProcessID", &(t.peer.sender.as_str(),))?;
            Peer::process(t.peer.sender.clone(), pid, uid)
        })();
        result.is_ok_and(|peer| peer == t.peer)
    }
    fn clear(&self, t: &Target, capability: u64) -> Result<()> {
        self.proxy(t)?
            .call("Clear", &(capability,))
            .map_err(Into::into)
    }
    fn cancel(&mut self, t: &Target, capability: u64) -> Result<()> {
        let result = self.clear(t, capability);
        let at = now();
        for o in &mut self.offers {
            if o.target.id == t.id && (capability == 0 || o.capability == capability) {
                let cutoff = if result.is_ok() { at } else { o.expires };
                o.revoked_at = Some(o.revoked_at.map_or(cutoff, |old| old.min(cutoff)));
            }
        }
        result
    }
    fn revoke(&mut self) {
        for t in self.targets() {
            let _ = self.cancel(&t, 0);
        }
        self.witness = None;
        self.snapshot = None;
    }
    fn reconcile_user(&mut self, id: u64, user: u64) -> bool {
        if self.active.as_ref().is_none_or(|t| t.id != id) {
            self.user_input.seen.insert(id, user);
            return false;
        }
        let changed = self.user_input.observe(id, user, Instant::now());
        if changed {
            self.revision = self.revision.saturating_add(1);
            self.timestamp = self.user_input.timestamp;
            self.pending_input = true;
            let at = now();
            for offer in &mut self.offers {
                if offer.target.id == id {
                    offer.revoked_at = Some(offer.revoked_at.map_or(at, |old| old.min(at)));
                }
            }
        }
        changed
    }
    fn set_shortcuts_enabled(&mut self, enabled: bool) {
        if self.shortcuts_enabled == enabled {
            return;
        }
        self.shortcuts_enabled = enabled;
        self.last_policy = 0;
        if !enabled {
            self.keys_ready = None;
            // Suspension is an immediate local withdrawal, not a settings change
            // which would still allow metadata-only toggles. Retain earlier real
            // readbacks, but no acceptance timestamp after this boundary.
            let at = now();
            for offer in &mut self.offers {
                offer.revoked_at = Some(offer.revoked_at.map_or(at, |old| old.min(at)));
            }
            self.events
                .retain(|event| !matches!(event, NativeHostEvent::Action { .. }));
            self.revoke();
            for target in self.targets() {
                let metadata = self.state.lock().ok().and_then(|s| {
                    s.clients
                        .get(&target.id)
                        .and_then(|c| clone_dict(&c.metadata).ok())
                });
                if let Some(metadata) = metadata {
                    let _ = self.send_policy(&target, &metadata);
                }
            }
        }
    }
    fn remove(&mut self, id: u64) {
        if let Ok(mut s) = self.state.lock() {
            s.clients.remove(&id);
        }
        self.offers.retain(|o| o.target.id != id);
        self.acceptance.remove(&id);
        self.user_input.seen.remove(&id);
        if self.active.as_ref().is_some_and(|t| t.id == id) {
            self.active = None;
            self.snapshot = None;
            self.witness = None;
            self.secure = None;
        }
    }
    fn send_policy(&self, t: &Target, meta: &Dict) -> Result<()> {
        let mut p = policy(&self.config, self.epoch, &t.peer, meta, now())?;
        let scope = FocusScope::parse(meta).ok();
        bind_focus_policy(&mut p, t.id, scope.as_ref(), self.keys_ready.as_ref());
        if self.active.as_ref().is_none_or(|a| a.id != t.id)
            || self.ambiguous
            || !matches!(flag(meta, "focus"), Ok(true))
        {
            inert_policy(&mut p);
        }
        gate_shortcut_policy(&mut p, self.shortcuts_enabled);
        let expires = number(&p, "expires_ns")?;
        let metadata = clone_dict(meta)?;
        self.proxy(t)?.call::<_, _, ()>("Configure", &(p,))?;
        if let Ok(mut s) = self.state.lock() {
            if let Some(c) = s.clients.get_mut(&t.id) {
                if c.peer == t.peer {
                    c.policy_epoch = self.epoch;
                    c.policy_expires = expires;
                    c.metadata = metadata;
                }
            }
        }
        Ok(())
    }
    fn metadata(&self, t: &Target) -> Result<Dict> {
        let meta: Dict = self.proxy(t)?.call("Metadata", &())?;
        server::metadata_valid(&meta, &t.peer)?;
        if flag(&meta, "focus")? {
            FocusScope::parse(&meta)?;
        }
        if string(&meta, "backend")? == "qt6-fcitx-client/1" {
            flag(&meta, "pending_edit")?;
        }
        Ok(meta)
    }
    fn probe(&mut self, force: bool) {
        if !force && self.last_probe.elapsed() < Duration::from_millis(100) {
            return;
        }
        self.last_probe = Instant::now();
        let mut focused = Vec::new();
        for t in self.targets() {
            if !self.authenticate(&t) {
                self.remove(t.id);
                continue;
            }
            match self.metadata(&t) {
                Ok(meta) => {
                    if let Ok(mut s) = self.state.lock() {
                        if let Some(c) = s.clients.get_mut(&t.id) {
                            c.metadata = meta;
                        }
                    }
                    let focus = self
                        .state
                        .lock()
                        .ok()
                        .and_then(|s| {
                            s.clients
                                .get(&t.id)
                                .and_then(|c| flag(&c.metadata, "focus").ok())
                        })
                        .unwrap_or(false);
                    if focus {
                        focused.push(t);
                    }
                }
                // A live client's failed/unknown metadata remains a refusal, not
                // an invitation to read that same target through AT-SPI.
                Err(_) => {
                    if self
                        .state
                        .lock()
                        .ok()
                        .and_then(|s| {
                            s.clients
                                .get(&t.id)
                                .and_then(|c| flag(&c.metadata, "focus").ok())
                        })
                        .unwrap_or(true)
                    {
                        focused.push(t);
                    }
                }
            }
        }
        let previous = self.active.as_ref().map(|t| t.id);
        self.ambiguous = focused.len() > 1;
        if focused.len() == 1 {
            self.active = focused.pop();
        } else if focused.is_empty() {
            self.active = None;
        } else {
            // Ambiguous cooperating focus blocks transport until resolved.
            self.active = focused.pop();
            self.secure = Some(true);
        }
        if previous != self.active.as_ref().map(|t| t.id)
            || self
                .keys_ready
                .as_ref()
                .is_some_and(|owner| Some(owner) != self.focus_owner().as_ref())
        {
            self.keys_ready = None;
            self.last_policy = 0;
            self.revoke();
            self.revision = self.revision.saturating_add(1);
            self.user_input.advance(Instant::now());
            self.timestamp = self.user_input.timestamp;
        }
        if now().saturating_sub(self.last_policy) >= POLICY_LEASE / 2 {
            for t in self.targets() {
                let meta = self.state.lock().ok().and_then(|s| {
                    s.clients
                        .get(&t.id)
                        .and_then(|c| clone_dict(&c.metadata).ok())
                });
                if let Some(meta) = meta {
                    if self.send_policy(&t, &meta).is_err() {
                        let _ = self.cancel(&t, 0);
                    }
                }
            }
            self.last_policy = now();
        }
    }
    fn capture(&mut self) -> Result<Option<Snapshot>> {
        self.probe(true);
        let Some(t) = self.active.clone() else {
            return Ok(None);
        };
        if self.ambiguous || !self.shortcuts_enabled {
            self.secure = Some(true);
            return Ok(None);
        }
        let metadata = self.metadata(&t);
        if metadata.as_ref().is_ok_and(pending_edit) {
            let meta = metadata?;
            self.secure = flag(&meta, "protected").ok();
            if let Ok(mut state) = self.state.lock() {
                if let Some(client) = state.clients.get_mut(&t.id) {
                    client.metadata = meta;
                }
            }
            return Ok(None);
        }
        let result = (|| -> Result<(Witness, Snapshot)> {
            let meta = metadata?;
            ensure!(
                permitted(&self.config, &t.peer, &meta),
                "Native policy refused"
            );
            self.send_policy(&t, &meta)?;
            let capture: Dict = self.proxy(&t)?.call("Capture", &(self.epoch,))?;
            let w = Witness::parse(&capture)?;
            ensure!(
                w.unit
                    == if string(&meta, "backend")? == "qt6-fcitx-client/1" {
                        "utf16"
                    } else {
                        "scalar"
                    },
                "Native backend offset mismatch"
            );
            ensure!(
                w.domain == domain(string(&meta, "domain")?)?,
                "Native domain changed"
            );
            ensure!(
                FocusScope::parse(&meta)?.matches(&w),
                "Native focus scope changed during capture"
            );
            let snapshot = w.snapshot(t.id, &t.peer.app, t.peer.pid)?;
            Ok((w, snapshot))
        })();
        match result {
            Ok((w, s)) => {
                self.reconcile_user(t.id, w.user);
                self.secure = Some(false);
                self.witness = Some(w);
                self.snapshot = Some(s.clone());
                Ok(Some(s))
            }
            Err(_) => {
                self.secure = Some(true);
                self.witness = None;
                self.snapshot = None;
                let _ = self.cancel(&t, 0);
                Ok(None)
            }
        }
    }
    fn foreground(&mut self) -> Result<Option<Foreground>> {
        self.probe(true);
        let Some(t) = self.active.as_ref() else {
            return Ok(None);
        };
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("Native broker stopped"))?;
        let client = state
            .clients
            .get(&t.id)
            .ok_or_else(|| anyhow::anyhow!("Native target disconnected"))?;
        Ok(Some(metadata_origin(t, &client.metadata)?))
    }
    fn present(&mut self, offer: &NativeClientOffer<'_>) -> Result<Option<NativeHostPresentation>> {
        ensure!(self.shortcuts_enabled, "Native shortcuts are suspended");
        let Some(snapshot) = self.capture()? else {
            return Ok(None);
        };
        ensure!(
            snapshot.same_key(offer.snapshot)
                && snapshot.same_authority(offer.snapshot)
                && snapshot.native_anchor == offer.snapshot.native_anchor,
            "Stale native offer"
        );
        ensure!(
            matches!(
                offer.mode,
                "completion" | "correction" | "emoji" | "synonyms" | "alternatives"
            ) && bounded_text(offer.text)
                && offer.values.len() <= 256
                && offer.values.iter().all(|s| bounded_text(s)),
            "Invalid native offer"
        );
        ensure!(
            offer
                .snapshot
                .text
                .slice(offer.start, offer.length)
                .is_some(),
            "Invalid native offer range"
        );
        let t = self
            .active
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Native target lost"))?;
        let w = self
            .witness
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Native witness lost"))?;
        let meta = self.metadata(&t)?;
        let p = policy(&self.config, self.epoch, &t.peer, &meta, now())?;
        let feature = match offer.mode {
            "completion" => "predictions",
            "correction" => "correction",
            "emoji" => "emoji",
            "synonyms" => "synonyms",
            _ => "alternatives",
        };
        ensure!(flag(&p, feature)?, "Native feature disabled");
        let issued = now();
        ensure!(issued != 0, "Native clock unavailable");
        let expires = issued.saturating_add(LEASE);
        self.serial = self
            .serial
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("Native offer counter exhausted"))?;
        let offer_id = self.serial;
        let start = native(&w.text, &w.unit, offer.start)?;
        let end = native(&w.text, &w.unit, offer.start + offer.length)?;
        let mut wire = Dict::new();
        w.write_offer(&mut wire);
        put(&mut wire, "offer_id", offer_id);
        put(&mut wire, "policy_epoch", self.epoch);
        put(&mut wire, "expires_ns", expires);
        text(&mut wire, "mode", offer.mode);
        text(&mut wire, "text", offer.text);
        list(&mut wire, "values", offer.values)?;
        put(&mut wire, "start", start as u32);
        put(&mut wire, "length", (end - start) as u32);
        ensure!(bounded(&wire), "Native offer exceeds wire bounds");
        // A pending offer is retained before dispatch. A lost reply never becomes
        // an acknowledged capability and cannot authorize any generated edit.
        let previous = self
            .offers
            .iter()
            .filter(|o| o.target.id == t.id && o.mode == offer.mode && o.revoked_at.is_none())
            .map(|o| o.capability)
            .collect::<Vec<_>>();
        for cap in previous {
            if cap != 0 {
                self.cancel(&t, cap)?;
            }
        }
        let at = now();
        for o in &mut self.offers {
            if o.target.id == t.id && o.mode == offer.mode {
                o.revoked_at.get_or_insert(at);
            }
        }
        ensure!(self.offers.len() < MAX_QUEUE, "Native receipt ledger full");
        self.offers.push_back(Frozen {
            target: t.clone(),
            before: w,
            snapshot: offer.snapshot.clone(),
            presentation: None,
            offer_id,
            capability: 0,
            issued,
            expires,
            epoch: self.epoch,
            mode: offer.mode.into(),
            start: offer.start,
            length: offer.length,
            shown_text: String::new(),
            shown_values: Vec::new(),
            word: String::new(),
            all: String::new(),
            armed_at: None,
            revoked_at: None,
        });
        let receipt: Result<Dict> = self
            .proxy(&t)?
            .call("Present", &(wire,))
            .map_err(Into::into);
        let receipt = match receipt {
            Ok(r) => r,
            Err(_) => {
                let _ = self.cancel(&t, 0);
                return Ok(None);
            }
        };
        let result = (|| -> Result<NativeHostPresentation> {
            ensure!(
                bounded(&receipt)
                    && flag(&receipt, "presented")?
                    && number(&receipt, "offer_id")? == offer_id,
                "Native presentation refused"
            );
            let cap = number(&receipt, "capability_id")?;
            let end = number(&receipt, "expires_ns")?;
            ensure!(
                cap != 0 && end > now() && end <= expires,
                "Invalid native presentation lease"
            );
            let shown = string(&receipt, "shown_text")?.to_owned();
            let values = strings(&receipt, "shown_values")?;
            let chooser = !matches!(offer.mode, "completion" | "correction");
            ensure!(
                if chooser {
                    shown.is_empty() && !values.is_empty() && offer.values.starts_with(&values)
                } else {
                    values.is_empty() && !shown.is_empty() && offer.text.starts_with(&shown)
                },
                "Invalid shown native prefix"
            );
            let at = Instant::now() + Duration::from_nanos(end.saturating_sub(now()));
            let anchor = format!("client:{}:offer:{}:cap:{}", t.id, offer_id, cap);
            let presentation = if chooser {
                NativeHostPresentation::Choices(NativeChoices {
                    anchor,
                    values: values.clone(),
                    revision: offer.user_input_revision,
                    expires_at: at,
                })
            } else {
                NativeHostPresentation::Preedit(NativePreedit {
                    anchor,
                    text: shown.clone(),
                    revision: offer.user_input_revision,
                    expires_at: at,
                })
            };
            let frozen = self
                .offers
                .back_mut()
                .ok_or_else(|| anyhow::anyhow!("Lost native offer"))?;
            frozen.capability = cap;
            frozen.expires = end;
            frozen.shown_text = shown;
            frozen.shown_values = values;
            frozen.presentation = Some(presentation.clone());
            Ok(presentation)
        })();
        match result {
            Ok(r) => Ok(Some(r)),
            Err(_) => {
                if !matches!(flag(&receipt, "presented"), Ok(false)) {
                    let _ = self.cancel(&t, number(&receipt, "capability_id").unwrap_or(0));
                }
                self.offers.retain(|o| o.offer_id != offer_id);
                Ok(None)
            }
        }
    }
    fn arm(&mut self, acceptance: &NativeClientAcceptance<'_>) -> Result<bool> {
        let index = self
            .offers
            .iter()
            .position(|o| {
                o.presentation.as_ref() == Some(acceptance.presentation)
                    && receipt_live(
                        o,
                        self.shortcuts_enabled,
                        self.epoch,
                        self.active.as_ref().map(|t| t.id),
                        now(),
                    )
            })
            .ok_or_else(|| anyhow::anyhow!("Stale or suspended native arm receipt"))?;
        let frozen = &self.offers[index];
        ensure!(
            self.current_consent(&frozen.target, frozen.epoch),
            "Native consent revoked"
        );
        ensure!(frozen.armed_at.is_none(), "Native offer already armed");
        ensure!(
            bounded_text(acceptance.word_text) && bounded_text(acceptance.all_text),
            "Invalid native insertion"
        );
        let mut wire = Dict::new();
        put(&mut wire, "offer_id", frozen.offer_id);
        put(&mut wire, "capability_id", frozen.capability);
        put(&mut wire, "policy_epoch", frozen.epoch);
        put(&mut wire, "expires_ns", frozen.expires);
        text(&mut wire, "word_text", acceptance.word_text);
        text(&mut wire, "all_text", acceptance.all_text);
        text(&mut wire, "shown_text", &frozen.shown_text);
        list(&mut wire, "shown_values", &frozen.shown_values)?;
        ensure!(bounded(&wire), "Native acceptance exceeds wire bounds");
        let target = frozen.target.clone();
        // Freeze the exact core-authorized insertions BEFORE dispatch. A timed-out
        // Arm may have executed; its later real readback remains accountable.
        self.offers[index].word = acceptance.word_text.into();
        self.offers[index].all = acceptance.all_text.into();
        self.offers[index].armed_at = Some(now());
        let reply: Result<bool> = self
            .proxy(&target)?
            .call("Arm", &(wire,))
            .map_err(Into::into);
        match reply {
            Ok(true) => Ok(true),
            Ok(false) => {
                self.offers[index].armed_at = None;
                self.offers[index].revoked_at = Some(now());
                Ok(false)
            }
            Err(_) => {
                let cap = self.offers[index].capability;
                let _ = self.cancel(&target, cap);
                Ok(false)
            }
        }
    }
    fn valid_presentation(&self, p: &NativeHostPresentation) -> bool {
        self.offers.iter().any(|o| {
            receipt_live(
                o,
                self.shortcuts_enabled,
                self.epoch,
                self.active.as_ref().map(|t| t.id),
                now(),
            ) && o.presentation.as_ref() == Some(p)
        })
    }
    fn current_consent(&self, t: &Target, epoch: u64) -> bool {
        self.state
            .lock()
            .ok()
            .and_then(|s| {
                s.clients.get(&t.id).map(|c| {
                    c.peer == t.peer
                        && confirmation_consent(
                            &self.config,
                            self.epoch,
                            epoch,
                            &c.peer,
                            &c.metadata,
                        )
                })
            })
            .unwrap_or(false)
    }
    fn confirmed(&mut self, id: u64, e: &Dict) -> Result<()> {
        take_acceptance(&mut self.acceptance, id, number(e, "acceptance_id")?)?;
        let offer_id = number(e, "offer_id")?;
        let cap = number(e, "capability_id")?;
        let index = self
            .offers
            .iter()
            .position(|o| o.target.id == id && o.offer_id == offer_id && o.capability == cap)
            .ok_or_else(|| anyhow::anyhow!("Unknown native receipt"))?;
        let frozen = self.offers.remove(index).expect("located native offer");
        let current_consent = self.connected && self.current_consent(&frozen.target, frozen.epoch);
        ensure!(self.events.len() < MAX_QUEUE, "Native host queue full");
        match string(e, "outcome")? {
            "applied" if current_consent => {
                let (edit, after) = validate_edit(frozen, e, now())?;
                self.user_input.seed(id, after.user);
                if self.active.as_ref().is_some_and(|t| t.id == id)
                    && self.user_input.seen.get(&id) == Some(&after.user)
                    && self.witness.as_ref().is_none_or(|w| {
                        w.field == after.field
                            && w.document == after.document
                            && w.revision <= after.revision
                    })
                {
                    self.witness = Some(after);
                }
                self.events.push_back(NativeHostEvent::Applied(edit));
            }
            "applied" | "indeterminate" => {
                let uncertain = validate_uncertain(frozen, e, now())?;
                if self
                    .snapshot
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.field_id == uncertain.before.field_id)
                {
                    self.snapshot = None;
                    self.witness = None;
                    self.revision = self.revision.saturating_add(1);
                }
                self.events
                    .push_back(NativeHostEvent::Indeterminate(uncertain));
            }
            "rejected" => {
                validate_frozen_acceptance(&frozen, e, now(), ProofAuthority::ProvenanceOnly)?;
            }
            _ => anyhow::bail!("Invalid native terminal outcome"),
        }
        Ok(())
    }
    fn dismissed(&mut self, id: u64, e: &Dict) -> Result<()> {
        let offer = number(e, "offer_id")?;
        let cap = number(e, "capability_id")?;
        let index = self
            .offers
            .iter()
            .position(|o| o.target.id == id && o.offer_id == offer && o.capability == cap)
            .ok_or_else(|| anyhow::anyhow!("Unknown dismissed native receipt"))?;
        let frozen = self.offers.remove(index).expect("located native offer");
        ensure!(
            frozen.presentation.is_some()
                && self.current_consent(&frozen.target, frozen.epoch)
                && lease_valid(
                    number(e, "accepted_ns")?,
                    frozen.issued,
                    frozen.expires,
                    now()
                )
                && frozen
                    .revoked_at
                    .is_none_or(|at| number(e, "accepted_ns").is_ok_and(|n| n <= at))
                && !flag(e, "bypass")?,
            "Invalid native dismissal"
        );
        ensure!(
            Witness::parse(&nested(e, "before")?)?.matches(&frozen.before),
            "Stale native dismissal"
        );
        if self.events.len() >= MAX_QUEUE {
            anyhow::bail!("Native host queue full");
        }
        self.events
            .push_back(NativeHostEvent::Dismissed(NativeHostDismiss {
                presentation: frozen
                    .presentation
                    .ok_or_else(|| anyhow::anyhow!("Unacknowledged dismissal"))?,
                before: frozen.snapshot,
                mode: frozen.mode,
                bypass: false,
                accepted_at: Instant::now()
                    .checked_sub(Duration::from_nanos(
                        now().saturating_sub(number(e, "accepted_ns")?),
                    ))
                    .unwrap_or_else(Instant::now),
            }));
        Ok(())
    }
    fn pump(&mut self) -> bool {
        let previous_active = self.active.as_ref().map(|t| t.id);
        if !self.connected {
            return false;
        }
        if self.connection.is_closed() {
            self.connected = false;
            let queued = self
                .state
                .lock()
                .map(|mut state| state.queue.drain(..).collect::<Vec<_>>())
                .unwrap_or_default();
            // These calls were already authenticated and acknowledged by the
            // server callback. A later bus loss cannot turn their output into
            // authored text; disconnected receipts are provenance-only.
            for message in queued {
                if let Message::Event(id, event) = message {
                    if matches!(string(&event, "kind"), Ok("accepted")) {
                        let _ = self.confirmed(id, &event);
                    }
                }
            }
            self.active = None;
            self.witness = None;
            self.snapshot = None;
            self.offers.clear();
            fence_revoked_events(&mut self.events, &[], false);
            self.acceptance.clear();
            if let Ok(mut state) = self.state.lock() {
                state.stopped = true;
                state.clients.clear();
                state.queue.clear();
            }
            return true;
        }
        let mut bytes = [0; 256];
        while self.wake.read(&mut bytes).is_ok_and(|n| n > 0) {}
        let (messages, overflow) = match self.state.lock() {
            Ok(mut s) => (
                s.queue.drain(..).collect::<Vec<_>>(),
                std::mem::take(&mut s.overflowed),
            ),
            Err(_) => return false,
        };
        if overflow {
            self.revoke();
            fence_revoked_events(&mut self.events, &[], false);
        }
        let mut ordinary = std::mem::take(&mut self.pending_input);
        for message in messages {
            match message {
                Message::Registered(_id) => {
                    self.last_policy = 0;
                    self.last_probe = Instant::now() - Duration::from_secs(1);
                }
                Message::Event(id, e) => {
                    let target = self.state.lock().ok().and_then(|s| {
                        s.clients.get(&id).map(|c| Target {
                            id,
                            peer: c.peer.clone(),
                            path: c.path.clone(),
                        })
                    });
                    let Some(t) = target else {
                        continue;
                    };
                    if !self.authenticate(&t) {
                        self.remove(id);
                        continue;
                    }
                    match string(&e, "kind").unwrap_or("") {
                        "accepted" => {
                            if self.confirmed(id, &e).is_err() {
                                if let Ok(cap) = number(&e, "capability_id") {
                                    if cap != 0 {
                                        let _ = self.cancel(&t, cap);
                                    }
                                }
                            }
                        }
                        "dismissed" => {
                            let _ = self.dismissed(id, &e);
                        }
                        "action" => {
                            let action = match string(&e, "action").unwrap_or("") {
                                "force" => Some(Action::Force),
                                "alternatives" => Some(Action::Alternatives),
                                "synonyms" => Some(Action::Synonyms),
                                "settings" => Some(Action::Settings),
                                "app_toggle" => Some(Action::AppToggle),
                                "toggle" => Some(Action::Toggle),
                                _ => None,
                            };
                            let owner_ready = self.keys_ready.as_ref().is_some_and(|owner| {
                                owner.client == id && self.focus_owner().as_ref() == Some(owner)
                            });
                            let live = owner_ready
                                && self
                                    .state
                                    .lock()
                                    .ok()
                                    .and_then(|s| {
                                        s.clients.get(&id).map(|c| {
                                            client_actions_live(
                                                self.shortcuts_enabled,
                                                self.epoch,
                                                c.policy_epoch,
                                                c.policy_expires,
                                                now(),
                                            )
                                        })
                                    })
                                    .unwrap_or(false);
                            if live
                                && self.active.as_ref().is_some_and(|a| a.id == id)
                                && self.events.len() < MAX_QUEUE
                            {
                                if let Some(action) = action {
                                    if let Ok(meta) = self.metadata(&t) {
                                        if action_allowed(&self.config, &t.peer, &meta, &action) {
                                            if let Some(owner) = self.keys_ready.as_ref() {
                                                if let Ok(origin) =
                                                    action_origin(&t, &meta, &e, owner, self.epoch)
                                                {
                                                    self.events.push_back(
                                                        NativeHostEvent::Action { action, origin },
                                                    );
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        "changed" => {
                            // Ordering is intentional: confirmed edits have already
                            // been queued, and a later real input cannot erase them.
                            if self.active.as_ref().is_some_and(|t| t.id == id)
                                && self
                                    .witness
                                    .as_ref()
                                    .is_some_and(|w| stale_notification(w, &e))
                            {
                                continue;
                            }
                            let user = number(&e, "user_revision").unwrap_or(u64::MAX);
                            let changed = self.reconcile_user(id, user);
                            let semantic =
                                self.witness.as_ref().is_none_or(|w| changed_revokes(w, &e));
                            let at = now();
                            for o in &mut self.offers {
                                if o.target.id == id && changed_revokes(&o.before, &e) {
                                    let cutoff = o.revoked_at.map_or(at, |old| old.min(at));
                                    o.revoked_at = Some(cutoff);
                                }
                            }
                            if self.active.as_ref().is_some_and(|a| a.id == id) {
                                self.revision = self.revision.saturating_add(1);
                                if changed || semantic {
                                    self.timestamp = Some(Instant::now());
                                    ordinary = true;
                                    self.snapshot = None;
                                    self.witness = None;
                                }
                                self.secure = flag(&e, "protected").ok();
                                if let Ok(mut s) = self.state.lock() {
                                    if let Some(c) = s.clients.get_mut(&id) {
                                        for key in ["focus", "protected", "composing"] {
                                            if let Some(value) =
                                                e.get(key).and_then(|v| v.try_clone().ok())
                                            {
                                                c.metadata.insert(key.into(), value);
                                            }
                                        }
                                    }
                                }
                            }
                            self.last_probe = Instant::now() - Duration::from_secs(1);
                        }
                        _ => {}
                    }
                }
            }
        }
        let at = now();
        self.offers.retain(|o| at.saturating_sub(o.expires) < LEASE);
        self.probe(false);
        ordinary |= previous_active != self.active.as_ref().map(|t| t.id);
        let consented = self
            .targets()
            .into_iter()
            .filter(|t| self.current_consent(t, self.epoch))
            .map(|t| t.id)
            .collect::<Vec<_>>();
        fence_revoked_events(&mut self.events, &consented, true);
        ordinary |= std::mem::take(&mut self.pending_input);
        ordinary
    }
    fn configure(&mut self, config: &Config) -> Result<()> {
        if self.config != *config {
            self.revoke();
            fence_revoked_events(&mut self.events, &[], false);
            self.epoch = self
                .epoch
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("Native policy counter exhausted"))?;
            self.config = config.clone();
            let mut s = self
                .state
                .lock()
                .map_err(|_| anyhow::anyhow!("Native broker stopped"))?;
            s.config = config.clone();
            s.epoch = self.epoch;
            s.queue.retain(|message|matches!(message,Message::Event(_,event) if matches!(string(event,"kind"),Ok("accepted"))));
            drop(s);
            self.last_policy = 0;
            self.probe(true);
        }
        Ok(())
    }
    fn stop(&mut self) {
        if !self.connected {
            return;
        }
        self.revoke();
        if let Ok(mut s) = self.state.lock() {
            s.stopped = true;
            s.queue.clear();
            s.clients.clear();
        }
        self.events.clear();
        self.connected = false;
        self.shortcuts_enabled = false;
        self.active = None;
        self.keys_ready = None;
        self.witness = None;
        self.snapshot = None;
        self.secure = None;
        let _ = self.connection.clone().close();
    }
}
fn stale_notification(witness: &Witness, event: &Dict) -> bool {
    string(event, "field_token").is_ok_and(|s| s == witness.field)
        && string(event, "window_token").is_ok_and(|s| s == witness.window)
        && string(event, "document_token").is_ok_and(|s| s == witness.document)
        && (number(event, "notification_revision").is_ok_and(|n| n < witness.notification)
            || number(event, "revision").is_ok_and(|n| n < witness.revision)
                && number(event, "user_revision").is_ok_and(|n| n <= witness.user))
}
fn changed_revokes(witness: &Witness, event: &Dict) -> bool {
    !(matches!(flag(event, "focus"), Ok(true))
        && matches!(flag(event, "protected"), Ok(false))
        && matches!(string(event, "composing"), Ok("idle"))
        && string(event, "field_token").is_ok_and(|s| s == witness.field)
        && string(event, "window_token").is_ok_and(|s| s == witness.window)
        && string(event, "document_token").is_ok_and(|s| s == witness.document)
        && number(event, "revision").is_ok_and(|n| n == witness.revision)
        && number(event, "user_revision").is_ok_and(|n| n == witness.user))
}
fn event_client(snapshot: &Snapshot) -> Option<u64> {
    snapshot
        .field_id
        .strip_prefix("client:")?
        .split(':')
        .next()?
        .parse()
        .ok()
}
fn take_acceptance(ids: &mut HashMap<u64, u64>, client: u64, id: u64) -> Result<()> {
    ensure!(
        id > ids.get(&client).copied().unwrap_or(0),
        "Duplicate native acceptance"
    );
    // Burn authenticated ids even on rejected/indeterminate/invalid readbacks.
    ids.insert(client, id);
    Ok(())
}
fn metadata_origin(target: &Target, metadata: &Dict) -> Result<Foreground> {
    ensure!(
        flag(metadata, "focus")?,
        "Native action target is not focused"
    );
    let scope = FocusScope::parse(metadata)?;
    Ok(Foreground {
        app_id: target.peer.app.clone(),
        executable: target.peer.executable.clone(),
        domain: domain(string(metadata, "domain")?)?,
        window_id: window_identity(target.id, &scope.window),
        window_pid: target.peer.pid as i32,
        document_id: format!(
            "client:{}:field:{}:{}:document:{}:{}:focus:{}",
            target.id,
            scope.field.len(),
            scope.field,
            scope.document.len(),
            scope.document,
            scope.generation
        ),
    })
}
fn action_origin(
    target: &Target,
    metadata: &Dict,
    event: &Dict,
    owner: &FocusOwner,
    epoch: u64,
) -> Result<Foreground> {
    ensure!(
        number(event, "policy_epoch")? == epoch,
        "Stale native action policy"
    );
    let scope = FocusScope::parse(event)?;
    ensure!(
        owner.client == target.id && owner.scope == scope && FocusScope::parse(metadata)? == scope,
        "Stale native action focus scope"
    );
    metadata_origin(target, metadata)
}
fn uncertain_from_edit(edit: NativeHostEdit) -> NativeHostUncertain {
    NativeHostUncertain {
        presentation: edit.presentation,
        before: edit.before,
        mode: edit.mode,
        accepted_at: edit.accepted_at,
    }
}
fn fence_revoked_events(
    events: &mut VecDeque<NativeHostEvent>,
    consented: &[u64],
    keep_actions: bool,
) {
    for _ in 0..events.len() {
        let Some(event) = events.pop_front() else {
            break;
        };
        match event {
            NativeHostEvent::Applied(edit) => {
                if event_client(&edit.before).is_some_and(|id| consented.contains(&id)) {
                    events.push_back(NativeHostEvent::Applied(edit));
                } else {
                    events.push_back(NativeHostEvent::Indeterminate(uncertain_from_edit(edit)));
                }
            }
            NativeHostEvent::Indeterminate(uncertain) => {
                events.push_back(NativeHostEvent::Indeterminate(uncertain))
            }
            NativeHostEvent::Dismissed(dismiss)
                if event_client(&dismiss.before).is_some_and(|id| consented.contains(&id)) =>
            {
                events.push_back(NativeHostEvent::Dismissed(dismiss))
            }
            NativeHostEvent::Action { action, origin } if keep_actions => {
                events.push_back(NativeHostEvent::Action { action, origin })
            }
            _ => {}
        }
    }
}
#[derive(Clone, Copy)]
enum ProofAuthority {
    Current,
    ProvenanceOnly,
}
struct AcceptedProof {
    before: Witness,
    start: usize,
    action: Action,
    accepted_at: Instant,
}
fn validate_frozen_acceptance(
    frozen: &Frozen,
    e: &Dict,
    received: u64,
    authority: ProofAuthority,
) -> Result<AcceptedProof> {
    ensure!(
        bounded(e)
            && number(e, "offer_id")? == frozen.offer_id
            && number(e, "capability_id")? == frozen.capability,
        "Native receipt mismatch"
    );
    let accepted_ns = number(e, "accepted_ns")?;
    ensure!(
        frozen.presentation.is_some() && frozen.armed_at.is_some_and(|at| accepted_ns >= at),
        "Unarmed native acceptance"
    );
    if matches!(authority, ProofAuthority::Current) {
        ensure!(
            frozen.revoked_at.is_none_or(|at| accepted_ns <= at),
            "Revoked native acceptance"
        );
    }
    ensure!(
        lease_valid(accepted_ns, frozen.issued, frozen.expires, received),
        "Expired native acceptance"
    );
    let before = Witness::parse(&nested(e, "before")?)?;
    ensure!(
        before.matches(&frozen.before),
        "Stale native before witness"
    );
    let kind = string(e, "action")?;
    let accepted = match kind {
        "word" if !frozen.word.is_empty() => frozen.word.as_str(),
        "all" if !frozen.all.is_empty() => frozen.all.as_str(),
        "choice" => frozen
            .shown_values
            .get(offset(e, "choice_index")?)
            .map(String::as_str)
            .ok_or_else(|| anyhow::anyhow!("Hidden native choice"))?,
        _ => anyhow::bail!("Invalid native acceptance action"),
    };
    ensure!(
        string(e, "text")? == accepted,
        "Native accepted text mismatch"
    );
    let start = scalar(&before.text, &before.unit, offset(e, "start")?)?;
    let native_end = offset(e, "start")?
        .checked_add(offset(e, "length")?)
        .ok_or_else(|| anyhow::anyhow!("Invalid native edit range"))?;
    let end = scalar(&before.text, &before.unit, native_end)?;
    ensure!(
        start == frozen.start && end.checked_sub(start) == Some(frozen.length),
        "Native accepted range mismatch"
    );
    let action = match kind {
        "word" => Action::Word,
        "all" => Action::All,
        _ => Action::Choice(offset(e, "choice_index")?),
    };
    let accepted_at = Instant::now()
        .checked_sub(Duration::from_nanos(received.saturating_sub(accepted_ns)))
        .unwrap_or_else(Instant::now);
    Ok(AcceptedProof {
        before,
        start,
        action,
        accepted_at,
    })
}
fn validate_uncertain(frozen: Frozen, e: &Dict, received: u64) -> Result<NativeHostUncertain> {
    ensure!(
        matches!(string(e, "outcome"), Ok("indeterminate" | "applied")),
        "Native edit has no provenance uncertainty"
    );
    let proof = validate_frozen_acceptance(&frozen, e, received, ProofAuthority::ProvenanceOnly)?;
    // Deliberately do not read or claim an after snapshot. This is provenance
    // uncertainty for the core to fence, never an applied edit or a retry.
    Ok(NativeHostUncertain {
        presentation: frozen
            .presentation
            .ok_or_else(|| anyhow::anyhow!("Missing native presentation"))?,
        before: frozen.snapshot,
        mode: frozen.mode,
        accepted_at: proof.accepted_at,
    })
}
fn validate_edit(frozen: Frozen, e: &Dict, received: u64) -> Result<(NativeHostEdit, Witness)> {
    ensure!(
        string(e, "outcome")? == "applied",
        "Native edit not confirmed"
    );
    let proof = validate_frozen_acceptance(&frozen, e, received, ProofAuthority::Current)?;
    let before = proof.before;
    let after = Witness::parse(&nested(e, "after")?)?;
    let accepted = string(e, "text")?;
    let expected = expected_after(&before, proof.start, frozen.length, accepted)?;
    ensure!(
        after.field == before.field
            && after.window == before.window
            && after.document == before.document
            && after.unit == before.unit
            && after.purpose == before.purpose
            && after.domain == before.domain
            && after.text == expected
            && after.user == before.user
            && after.revision >= before.revision
            && after.notification >= before.notification,
        "Native after witness mismatch"
    );
    let expected_caret = proof.start + accepted.chars().count();
    ensure!(
        scalar(&after.text, &after.unit, after.caret)? == expected_caret
            && scalar(&after.text, &after.unit, after.anchor)? == expected_caret,
        "Native caret mismatch"
    );
    let after_snapshot = after.snapshot(
        frozen.target.id,
        &frozen.target.peer.app,
        frozen.target.peer.pid,
    )?;
    Ok((
        NativeHostEdit {
            presentation: frozen
                .presentation
                .ok_or_else(|| anyhow::anyhow!("Missing native presentation"))?,
            before: frozen.snapshot,
            after: after_snapshot,
            start: proof.start,
            length: frozen.length,
            text: accepted.into(),
            action: proof.action,
            mode: frozen.mode,
            accepted_at: proof.accepted_at,
        },
        after,
    ))
}
fn clone_dict(d: &Dict) -> Result<Dict> {
    d.iter()
        .map(|(k, v)| Ok((k.clone(), v.try_clone()?)))
        .collect()
}
fn expected_after(before: &Witness, start: usize, length: usize, text: &str) -> Result<String> {
    let end = start
        .checked_add(length)
        .ok_or_else(|| anyhow::anyhow!("Invalid native replacement range"))?;
    let mut boundaries = before
        .text
        .char_indices()
        .map(|(byte, _)| byte)
        .chain(std::iter::once(before.text.len()))
        .enumerate();
    let first = boundaries
        .find(|(index, _)| *index == start)
        .map(|(_, byte)| byte)
        .ok_or_else(|| anyhow::anyhow!("Invalid native replacement start"))?;
    let last = if length == 0 {
        first
    } else {
        boundaries
            .find(|(index, _)| *index == end)
            .map(|(_, byte)| byte)
            .ok_or_else(|| anyhow::anyhow!("Invalid native replacement end"))?
    };
    let mut expected = String::with_capacity(before.text.len() - (last - first) + text.len());
    expected.push_str(&before.text[..first]);
    expected.push_str(text);
    expected.push_str(&before.text[last..]);
    Ok(expected)
}
struct ClientBridge {
    base: Option<Box<dyn NativeBridge>>,
    base_error: Option<String>,
    broker: Broker,
    input_callback: Option<Arc<dyn Fn() + Send + Sync>>,
    shortcuts_enabled: bool,
    client_owned: bool,
    stopped: bool,
}
impl ClientBridge {
    fn sync_owner(&mut self) -> Result<()> {
        let owned = self.broker.active.is_some();
        if let Some(base) = self.base.as_mut() {
            base.set_shortcuts_enabled(self.shortcuts_enabled && !owned);
            if owned && !self.client_owned {
                base.finish_input(false)?;
            }
        }
        let ready = if self.shortcuts_enabled {
            self.broker.focus_owner()
        } else {
            None
        };
        if self.broker.keys_ready != ready {
            self.broker.keys_ready = ready;
            self.broker.last_policy = 0;
        }
        self.client_owned = owned;
        Ok(())
    }
    fn pump(&mut self) -> Result<()> {
        let fallback = self.base.as_mut().map_or(Ok(()), |base| base.pump_events());
        if self.broker.pump() {
            if let Some(callback) = &self.input_callback {
                callback();
            }
        }
        self.sync_owner()?;
        if self.broker.active.is_some() {
            Ok(())
        } else {
            fallback
        }
    }
    fn owns(&self, snapshot: &Snapshot) -> bool {
        snapshot.field_id.starts_with("client:")
    }
    fn fallback_error(&self) -> anyhow::Error {
        anyhow::anyhow!(
            "{}",
            self.base_error
                .as_deref()
                .unwrap_or("No standalone native transport is available")
        )
    }
}
impl NativeBridge for ClientBridge {
    fn configure(&mut self, config: &Config) -> Result<()> {
        self.broker.configure(config)?;
        if let Some(base) = self.base.as_mut() {
            base.configure(config)?;
        }
        self.sync_owner()
    }
    fn capture(&mut self) -> Result<Option<Snapshot>> {
        self.pump()?;
        let snapshot = self.broker.capture()?;
        self.sync_owner()?;
        if self.broker.active.is_some() {
            Ok(snapshot)
        } else {
            self.base.as_mut().map_or(Ok(None), |base| base.capture())
        }
    }
    fn foreground(&mut self) -> Result<Option<Foreground>> {
        self.pump()?;
        let foreground = self.broker.foreground()?;
        self.sync_owner()?;
        if self.broker.active.is_some() {
            Ok(foreground)
        } else {
            self.base
                .as_mut()
                .map_or(Ok(None), |base| base.foreground())
        }
    }
    fn insert(&mut self, snapshot: &Snapshot, text: &str) -> Result<EditOutcome> {
        if self.owns(snapshot) || self.broker.active.is_some() {
            return Ok(EditOutcome::Rejected);
        }
        self.base
            .as_mut()
            .map_or(Ok(EditOutcome::Rejected), |base| {
                base.insert(snapshot, text)
            })
    }
    fn replace_range(
        &mut self,
        snapshot: &Snapshot,
        start: usize,
        length: usize,
        text: &str,
    ) -> Result<EditOutcome> {
        if self.owns(snapshot) || self.broker.active.is_some() {
            return Ok(EditOutcome::Rejected);
        }
        self.base
            .as_mut()
            .map_or(Ok(EditOutcome::Rejected), |base| {
                base.replace_range(snapshot, start, length, text)
            })
    }
    fn range_rect(
        &mut self,
        snapshot: &Snapshot,
        start: usize,
        length: usize,
    ) -> Result<Option<Rect>> {
        if self.owns(snapshot) || self.broker.active.is_some() {
            return Ok(None);
        }
        self.base
            .as_mut()
            .map_or(Ok(None), |base| base.range_rect(snapshot, start, length))
    }
    fn focused_context(
        &mut self,
        snapshot: &Snapshot,
        authorized: &dyn Fn() -> bool,
        max_chars: usize,
    ) -> Result<ContextRead> {
        if self.owns(snapshot) {
            if !authorized() {
                return Ok(ContextRead::blocked("native-client"));
            }
            let fresh = self.broker.capture()?;
            if authorized()
                && fresh.as_ref().is_some_and(|current| {
                    current.same_key(snapshot) && current.same_authority(snapshot)
                })
            {
                return Ok(ContextRead {
                    text: snapshot.text.chars().take(max_chars.min(16_000)).collect(),
                    status: "ok".into(),
                    source: "native-client".into(),
                });
            }
            return Ok(ContextRead::blocked("native-client"));
        }
        if self.broker.active.is_some() {
            return Ok(ContextRead::blocked("native-client"));
        }
        self.base.as_mut().map_or_else(
            || Ok(ContextRead::blocked("native-unavailable")),
            |base| base.focused_context(snapshot, authorized, max_chars),
        )
    }
    fn context_ticket(
        &mut self,
        snapshot: &Snapshot,
    ) -> Result<Option<Box<dyn DetachedContextRead>>> {
        if self.owns(snapshot) || self.broker.active.is_some() {
            return Ok(None);
        }
        self.base
            .as_mut()
            .map_or(Ok(None), |base| base.context_ticket(snapshot))
    }
    fn start_shortcuts(&mut self, callback: ShortcutHandler) -> Result<()> {
        ensure!(
            self.broker.connected && !self.stopped,
            "Native client action transport is unavailable"
        );
        if let Some(base) = self.base.as_mut() {
            base.start_shortcuts(callback)?;
        }
        self.sync_owner()
    }
    fn set_shortcuts_enabled(&mut self, enabled: bool) {
        self.shortcuts_enabled = enabled;
        self.broker.set_shortcuts_enabled(enabled);
        let _ = self.sync_owner();
    }
    fn set_input_callback(&mut self, callback: Option<Arc<dyn Fn() + Send + Sync>>) {
        self.input_callback = callback.clone();
        if let Some(base) = self.base.as_mut() {
            base.set_input_callback(callback);
        }
    }
    fn finish_input(&mut self, accepted: bool) -> Result<()> {
        self.base.as_mut().map_or(Ok(()), |base| {
            base.finish_input(accepted && self.broker.active.is_none())
        })
    }
    fn input_claim_valid(&self) -> bool {
        self.broker.active.is_none()
            && self
                .base
                .as_ref()
                .is_some_and(|base| base.input_claim_valid())
    }
    fn input_revision(&self) -> u64 {
        if self.broker.active.is_some() {
            self.broker.revision
        } else {
            self.base
                .as_ref()
                .map_or(self.broker.revision, |base| base.input_revision())
        }
    }
    fn user_input_revision(&self) -> u64 {
        if self.broker.active.is_some() {
            self.broker.user_input.revision
        } else {
            self.base
                .as_ref()
                .map_or(self.broker.user_input.revision, |base| {
                    base.user_input_revision()
                })
        }
    }
    fn input_timestamp(&self) -> Option<Instant> {
        if self.broker.active.is_some() {
            self.broker.timestamp
        } else {
            self.base.as_ref().and_then(|base| base.input_timestamp())
        }
    }
    fn secure_input_enabled(&self) -> Option<bool> {
        if self.broker.active.is_some() {
            self.broker.secure
        } else {
            self.base
                .as_ref()
                .and_then(|base| base.secure_input_enabled())
        }
    }
    fn request_permission(&mut self, kind: &str) -> Result<Permission> {
        self.base.as_mut().map_or(Ok(Permission::Unknown), |base| {
            base.request_permission(kind)
        })
    }
    fn status(&self) -> serde_json::Value {
        let mut status = self.base.as_ref().map_or_else(
            || serde_json::json!({"platform":"linux","available":false,"global_capture":false,"message":"Waiting for a supported app"}),
            |base| base.status(),
        );
        if self.broker.active.is_some() {
            status["available"] = serde_json::json!(true);
            if self.base.is_none() {
                status["message"] = serde_json::json!("Native app integration ready");
            }
        }
        if let Some(object) = status.as_object_mut() {
            object.insert("native_client".into(),serde_json::json!({
                "bus":self.broker.bus,"connected":self.broker.connected,
                "state":if !self.broker.connected { "disconnected" } else if self.broker.active.is_some() { "active" } else { "waiting" },
                "active":self.broker.active.is_some(),"clients":self.broker.state.lock().map(|state|state.clients.len()).unwrap_or(0),
                "policy_epoch":self.broker.epoch,"fallback_available":self.base.is_some(),"fallback_error":self.base_error,
            }));
        }
        status
    }
    fn stop(&mut self) -> Result<()> {
        if !self.stopped {
            self.broker.stop();
            self.stopped = true;
            if let Some(base) = self.base.as_mut() {
                base.stop()?;
            }
        }
        Ok(())
    }
    fn atomic_replacement_supported(&self) -> bool {
        self.broker.active.is_some()
            || self
                .base
                .as_ref()
                .is_some_and(|base| base.atomic_replacement_supported())
    }
    fn docs_hint(&mut self) -> Result<Option<serde_json::Value>> {
        if self.broker.active.is_some() {
            return Ok(None);
        }
        self.base.as_mut().map_or(Ok(None), |base| base.docs_hint())
    }
    fn enable_docs_accessibility(&mut self, token: &str) -> Result<serde_json::Value> {
        ensure!(
            self.broker.active.is_none(),
            "Native client owns this target"
        );
        match self.base.as_mut() {
            Some(base) => base.enable_docs_accessibility(token),
            None => Err(self.fallback_error()),
        }
    }
    fn pump_events(&mut self) -> Result<()> {
        self.pump()
    }
    fn native_event_source(&self) -> Option<(i32, bool)> {
        if self.broker.connected {
            Some((self.broker.wake.as_raw_fd(), false))
        } else {
            self.base
                .as_ref()
                .and_then(|base| base.native_event_source())
        }
    }
    fn bind_native_ui(&mut self, callback: Option<Arc<dyn Fn(String) + Send + Sync>>) {
        if let Some(base) = self.base.as_mut() {
            base.bind_native_ui(callback);
        }
    }
    fn client_owns_actions(&self) -> bool {
        self.broker.active.is_some()
    }
    fn native_edit_pending(&self) -> bool {
        if let Some(target) = self.broker.active.as_ref() {
            return self
                .broker
                .state
                .lock()
                .ok()
                .and_then(|state| {
                    state
                        .clients
                        .get(&target.id)
                        .map(|client| pending_edit(&client.metadata))
                })
                .unwrap_or(false);
        }
        self.base
            .as_ref()
            .is_some_and(|base| base.native_edit_pending())
    }
    fn uses_native_presentation(&self) -> bool {
        self.client_owns_actions()
            || self
                .base
                .as_ref()
                .is_some_and(|base| base.uses_native_presentation())
    }
    fn present_client_offer(
        &mut self,
        offer: &NativeClientOffer<'_>,
    ) -> Result<Option<NativeHostPresentation>> {
        self.pump()?;
        if self.broker.active.is_some() || self.owns(offer.snapshot) {
            return self.broker.present(offer);
        }
        self.base
            .as_mut()
            .map_or(Ok(None), |base| base.present_client_offer(offer))
    }
    fn arm_client_offer(&mut self, acceptance: &NativeClientAcceptance<'_>) -> Result<bool> {
        self.pump()?;
        let native = match acceptance.presentation {
            NativeHostPresentation::Preedit(receipt) => receipt.anchor.starts_with("client:"),
            NativeHostPresentation::Choices(receipt) => receipt.anchor.starts_with("client:"),
        };
        if native {
            self.broker.arm(acceptance)
        } else {
            self.base
                .as_mut()
                .map_or(Ok(false), |base| base.arm_client_offer(acceptance))
        }
    }
    fn drain_host_events(&mut self) -> Vec<NativeHostEvent> {
        let _ = self.pump();
        let mut events = self.broker.events.drain(..).collect::<Vec<_>>();
        if let Some(base) = self.base.as_mut() {
            events.extend(base.drain_host_events());
        }
        events
    }
    fn show_native_menu(&mut self, rows: Vec<(String, String, bool)>) -> Result<bool> {
        self.base
            .as_mut()
            .map_or(Ok(false), |base| base.show_native_menu(rows))
    }
    fn context_matches(&mut self, snapshot: &Snapshot) -> Result<bool> {
        if self.owns(snapshot) {
            return Ok(self.broker.capture()?.is_some_and(|current| {
                current.same_key(snapshot) && current.same_authority(snapshot)
            }));
        }
        if self.broker.active.is_some() {
            return Ok(false);
        }
        self.base
            .as_mut()
            .map_or(Ok(false), |base| base.context_matches(snapshot))
    }
    fn present_native(&mut self, anchor: &str, text: &str, paused: bool) -> Result<bool> {
        if self.broker.active.is_some() || anchor.starts_with("client:") {
            return Ok(false);
        }
        self.base
            .as_mut()
            .map_or(Ok(false), |base| base.present_native(anchor, text, paused))
    }
    fn present_preedit(&mut self, anchor: &str, text: &str) -> Result<Option<NativePreedit>> {
        if self.broker.active.is_some() || anchor.starts_with("client:") {
            return Ok(None);
        }
        self.base
            .as_mut()
            .map_or(Ok(None), |base| base.present_preedit(anchor, text))
    }
    fn native_preedit_valid(&self, receipt: &NativePreedit) -> bool {
        if receipt.anchor.starts_with("client:") {
            self.broker
                .valid_presentation(&NativeHostPresentation::Preedit(receipt.clone()))
        } else {
            self.base
                .as_ref()
                .is_some_and(|base| base.native_preedit_valid(receipt))
        }
    }
    fn present_choices(
        &mut self,
        anchor: &str,
        values: &[String],
    ) -> Result<Option<NativeChoices>> {
        if self.broker.active.is_some() || anchor.starts_with("client:") {
            return Ok(None);
        }
        self.base
            .as_mut()
            .map_or(Ok(None), |base| base.present_choices(anchor, values))
    }
    fn native_choices_valid(&self, receipt: &NativeChoices) -> bool {
        if receipt.anchor.starts_with("client:") {
            self.broker
                .valid_presentation(&NativeHostPresentation::Choices(receipt.clone()))
        } else {
            self.base
                .as_ref()
                .is_some_and(|base| base.native_choices_valid(receipt))
        }
    }
    fn set_native_panel_environment(
        &mut self,
        width: i32,
        height: i32,
        scale: i32,
        dark: bool,
    ) -> Result<()> {
        self.base.as_mut().map_or(Ok(()), |base| {
            base.set_native_panel_environment(width, height, scale, dark)
        })
    }
    fn hide_native(&mut self) -> Result<()> {
        self.broker.revoke();
        self.base.as_mut().map_or(Ok(()), |base| base.hide_native())
    }
}
impl Drop for ClientBridge {
    fn drop(&mut self) {
        if !self.stopped {
            self.broker.stop();
            if let Some(base) = self.base.as_mut() {
                let _ = base.stop();
            }
            self.stopped = true;
        }
    }
}
