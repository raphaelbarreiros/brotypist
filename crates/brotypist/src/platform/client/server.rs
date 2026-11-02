//! D-Bus callbacks retain bounded plain data only; native handles stay on the owner.
use super::protocol::*;
use super::{inert_policy, policy};
use crate::config::Config;
use anyhow::{Result, ensure};
use std::{
    collections::{HashMap, VecDeque},
    io::Write,
    os::unix::net::UnixStream,
    sync::{Arc, Mutex},
};
use zbus::{message::Header, zvariant::OwnedObjectPath};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Peer {
    pub sender: String,
    pub pid: u32,
    pub uid: u32,
    pub executable: String,
    pub app: String,
}
impl Peer {
    pub fn process(sender: String, pid: u32, uid: u32) -> Result<Self> {
        ensure!(
            uid == unsafe { libc::geteuid() }
                && pid != 0
                && pid != std::process::id()
                && pid <= i32::MAX as u32,
            "Invalid native peer"
        );
        let exe = std::fs::read_link(format!("/proc/{pid}/exe"))?;
        let executable = exe
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Non-UTF8 native executable"))?
            .to_owned();
        ensure!(
            executable.len() <= 4096 && exe.is_absolute(),
            "Invalid native executable"
        );
        let app = exe
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow::anyhow!("Invalid native executable"))?
            .to_owned();
        Ok(Self {
            sender,
            pid,
            uid,
            executable,
            app,
        })
    }
}
pub(super) struct Client {
    pub peer: Peer,
    pub path: String,
    pub metadata: Dict,
    pub policy_epoch: u64,
    pub policy_expires: u64,
    pub sent_policy: Option<Dict>,
}
pub(super) enum Message {
    Registered(u64),
    Event(u64, Dict),
}
pub(super) struct State {
    pub config: Config,
    pub epoch: u64,
    pub clients: HashMap<u64, Client>,
    pub queue: VecDeque<Message>,
    pub next: u64,
    pub stopped: bool,
    pub overflowed: bool,
}
pub(super) struct Server {
    pub state: Arc<Mutex<State>>,
    pub wake: Arc<UnixStream>,
}
fn denied() -> zbus::fdo::Error {
    zbus::fdo::Error::AccessDenied("Native peer refused".into())
}
async fn authenticate(
    connection: &zbus::Connection,
    header: &Header<'_>,
) -> zbus::fdo::Result<Peer> {
    let sender = header.sender().ok_or_else(denied)?.as_str().to_owned();
    let proxy = zbus::Proxy::new(
        connection,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )
    .await
    .map_err(|_| denied())?;
    let uid: u32 = proxy
        .call("GetConnectionUnixUser", &(sender.as_str(),))
        .await
        .map_err(|_| denied())?;
    let pid: u32 = proxy
        .call("GetConnectionUnixProcessID", &(sender.as_str(),))
        .await
        .map_err(|_| denied())?;
    Peer::process(sender, pid, uid).map_err(|_| denied())
}
pub(super) fn metadata_valid(d: &Dict, peer: &Peer) -> Result<()> {
    ensure!(
        d.keys().all(|k| matches!(
            k.as_str(),
            "protocol"
                | "backend"
                | "pid"
                | "app_id"
                | "library_sha256"
                | "focus"
                | "protected"
                | "editable"
                | "purpose"
                | "domain"
                | "composing"
                | "field_token"
                | "window_token"
                | "document_token"
                | "focus_generation"
                | "pending_edit"
        )),
        "Native metadata contains content"
    );
    ensure!(
        bounded(d) && offset(d, "protocol")? == 1 && offset(d, "pid")? == peer.pid as usize,
        "Invalid native registration"
    );
    ensure!(
        matches!(
            string(d, "backend")?,
            "gtk3-fcitx-client/1" | "qt6-fcitx-client/1"
        ),
        "Unknown native backend"
    );
    ensure!(
        string(d, "app_id")? == peer.executable,
        "Native executable mismatch"
    );
    if d.contains_key("library_sha256") {
        let digest = string(d, "library_sha256")?;
        ensure!(
            digest.is_empty()
                || digest.len() == 64 && digest.bytes().all(|c| c.is_ascii_hexdigit()),
            "Invalid native library digest"
        );
    }
    if d.contains_key("composing") {
        ensure!(
            matches!(string(d, "composing")?, "idle" | "active" | "unknown"),
            "Invalid native composition metadata"
        );
    }
    flag(d, "focus")?;
    flag(d, "protected")?;
    flag(d, "editable")?;
    if d.contains_key("pending_edit") {
        flag(d, "pending_edit")?;
    }
    if [
        "field_token",
        "window_token",
        "document_token",
        "focus_generation",
    ]
    .iter()
    .any(|key| d.contains_key(*key))
    {
        number(d, "focus_generation")?;
        for key in ["field_token", "window_token", "document_token"] {
            if flag(d, "focus")? || !string(d, key)?.is_empty() {
                token(d, key)?;
            }
        }
    }
    ensure!(string(d, "purpose")?.len() <= 64, "Invalid native purpose");
    domain(string(d, "domain")?)?;
    Ok(())
}
pub(super) fn registered_peer(clients: &HashMap<u64, Client>, id: u64, peer: &Peer) -> bool {
    clients.get(&id).is_some_and(|client| client.peer == *peer)
}
impl Server {
    fn wake(&self) {
        let _ = (&*self.wake).write(&[1]);
    }
}
// Sequential dispatch preserves accepted-before-changed transport order even while
// bus credentials are awaited. No callback observes fields or executes inference.
#[zbus::interface(name = "org.brotypist.Native1", spawn = false)]
impl Server {
    async fn register(
        &self,
        client_path: OwnedObjectPath,
        metadata: Dict,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> zbus::fdo::Result<(u64, Dict)> {
        let peer = authenticate(connection, &header).await?;
        metadata_valid(&metadata, &peer).map_err(|_| denied())?;
        let path = client_path.as_str();
        if path.len() > 256 || !path.starts_with("/org/brotypist/Client1/") {
            return Err(denied());
        }
        let mut s = self.state.lock().map_err(|_| denied())?;
        if s.stopped
            || s.clients.len() >= MAX_CLIENTS
            || s.queue.len() >= MAX_QUEUE
            || s.clients.values().any(|c| c.peer.sender == peer.sender)
        {
            return Err(denied());
        }
        let id = s.next.checked_add(1).ok_or_else(denied)?;
        s.next = id;
        let mut reply =
            policy(&s.config, s.epoch, &peer, &metadata, now()).map_err(|_| denied())?;
        // Registration does not prove the core owner has released fallback keys.
        inert_policy(&mut reply);
        let policy_epoch = s.epoch;
        let policy_expires = number(&reply, "expires_ns").map_err(|_| denied())?;
        s.clients.insert(
            id,
            Client {
                peer,
                path: path.into(),
                metadata,
                policy_epoch,
                policy_expires,
                sent_policy: None,
            },
        );
        s.queue.push_back(Message::Registered(id));
        drop(s);
        self.wake();
        Ok((id, reply))
    }
    async fn event(
        &self,
        client_id: u64,
        event: Dict,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        if !bounded(&event) {
            return Err(denied());
        }
        let peer = authenticate(connection, &header).await?;
        let mut s = self.state.lock().map_err(|_| denied())?;
        if s.stopped || !registered_peer(&s.clients, client_id, &peer) {
            return Err(denied());
        }
        if s.queue.len() >= MAX_QUEUE {
            s.overflowed = true;
            drop(s);
            self.wake();
            return Err(zbus::fdo::Error::LimitsExceeded("Native queue full".into()));
        }
        if !matches!(
            string(&event, "kind").map_err(|_| denied())?,
            "changed" | "accepted" | "dismissed" | "action"
        ) {
            return Err(denied());
        }
        s.queue.push_back(Message::Event(client_id, event));
        drop(s);
        self.wake();
        Ok(())
    }
}
