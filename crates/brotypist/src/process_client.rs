//! Latest-only supervisor for an owned native inference child.
//! Pipe I/O and process waits never hold the public state lock.
use crate::config::Config;
use crate::providers::GenerationOptions;
use crate::worker;
pub use crate::worker::WorkerEvent;
#[cfg(test)]
use crate::worker::MAX_MESSAGE_BYTES;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const CANCEL_GRACE: Duration = Duration::from_millis(300);
const PROMPT_EVALUATION_GRACE: Duration = Duration::from_secs(3);
const TERMINATE_GRACE: Duration = Duration::from_millis(150);
const STOP_WAIT: Duration = Duration::from_secs(1);
const EXIT_ERROR: &str = "The inference worker exited. Check local model installation.";
const START_ERROR: &str = "Cannot start the private inference worker. Check the installation.";

struct Pending {
    id: u64,
    identity: Vec<String>,
    frame: Vec<u8>,
    preparing: bool,
}

struct State {
    serial: u64,
    pending: Option<Pending>,
    release_through: Option<u64>,
    release_generation: u64,
    released_generation: u64,
    released_through: u64,
    proceed: Option<(u64, u64)>,
    events: VecDeque<WorkerEvent>,
    active: Option<u64>,
    configured: Option<Vec<String>>,
    idle_timeout: u32,
    worker_pid: Option<u32>,
    model_loaded: bool,
    initializing: bool,
    evaluating: bool,
    awaiting_proceed: bool,
    terminating: bool,
    state: &'static str,
    message: &'static str,
    finished: bool,
}
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    cancel: AtomicBool,
    stopped: AtomicBool,
    epoch: AtomicU64,
}

/// An isolated worker is allocated lazily. A `ready` event is NOT permission to
/// evaluate writing: the caller must recheck native authority and call proceed.
/// ID zero from prepare means no preparation was needed/eligible.
pub struct WorkerClient {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    command: WorkerCommand,
}

#[derive(Clone)]
struct WorkerCommand {
    executable: PathBuf,
    #[cfg(all(test, unix))]
    fixture: Option<Arc<TestCommand>>,
}
impl WorkerCommand {
    fn native() -> Result<Self> {
        Ok(Self { executable: std::env::current_exe().context("Cannot locate the private inference worker")?, #[cfg(all(test, unix))] fixture: None })
    }
    fn spawn(&self) -> Result<(Child, Box<dyn Read + Send>)> {
        let mut command = Command::new(&self.executable);
        command.stdin(Stdio::piped()).stderr(Stdio::null());
        #[cfg(all(test, unix))]
        if let Some(fixture) = &self.fixture { return fixture.spawn(command); }
        command.arg("_worker").stdout(Stdio::piped());
        #[cfg(windows)] {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000); // CREATE_NO_WINDOW, including GUI launchers.
        }
        let mut child = command.spawn().context(START_ERROR)?;
        let stdout = child.stdout.take().expect("worker command always configures piped stdout");
        Ok((child, Box::new(stdout)))
    }
}

impl WorkerClient {
    pub fn new() -> Result<Self> { Self::with_command(WorkerCommand::native()?) }
    /// Explicit application binary for owned smoke/consumer tools. Resolve the
    /// supplied file, rather than allowing a bare filename to search PATH.
    pub fn for_executable(path: impl Into<PathBuf>) -> Result<Self> {
        let executable = path.into();
        ensure!(executable.is_file(), "Private inference executable must be an existing file");
        let executable = executable.canonicalize().context("Cannot resolve the private inference executable")?;
        Self::with_command(WorkerCommand { executable, #[cfg(all(test, unix))] fixture: None })
    }
    fn with_command(command: WorkerCommand) -> Result<Self> {
        Ok(Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State { serial: 0, pending: None, release_through: None, release_generation: 0, released_generation: 0, released_through: 0, proceed: None, events: VecDeque::with_capacity(256), active: None, configured: None, idle_timeout: 300, worker_pid: None, model_loaded: false, initializing: false, evaluating: false, awaiting_proceed: false, terminating: false, state: "idle", message: "", finished: false }),
                changed: Condvar::new(), cancel: AtomicBool::new(false), stopped: AtomicBool::new(false), epoch: AtomicU64::new(0),
            }),
            thread: None, command,
        })
    }
    fn start(&mut self) -> Result<()> {
        if self.thread.is_some() { return Ok(()); }
        let shared = Arc::clone(&self.shared);
        let command = self.command.clone();
        self.thread = Some(thread::Builder::new().name("brotypist-worker-supervisor".into()).spawn(move || Supervisor::new(shared, command).run())?);
        Ok(())
    }
    pub fn prepare(&mut self, config: &Config) -> Result<u64> {
        if config.backend != "gguf" || !expanded_model_path(&config.model_path).is_file() { return Ok(0); }
        ensure!(!self.shared.stopped.load(Ordering::Acquire), "Inference has stopped");
        let mut checked = config.clone();
        checked.validate()?;
        self.start()?;
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let identity = config.provider_identity();
        if state.pending.is_some() || state.active.is_some() || (state.model_loaded && state.configured.as_ref() == Some(&identity) && state.release_through.is_none()) { return Ok(0); }
        let id = next_serial(state.serial)?;
        let frame = worker::encode_frame(&json!({"type":"prepare", "id":id, "config": provider_config(config)}))?;
        state.serial = id;
        state.pending = Some(Pending { id, identity, frame, preparing: true });
        state.idle_timeout = config.idle_release_seconds;
        self.shared.changed.notify_all();
        Ok(id)
    }
    pub fn submit(&mut self, config: &Config, prefix: &str, suffix: &str, options: &GenerationOptions) -> Result<u64> {
        ensure!(!self.shared.stopped.load(Ordering::Acquire), "Inference has stopped");
        let mut checked = config.clone();
        checked.validate()?;
        validate_options(options)?;
        // Bound by Unicode scalar, matching Python slices rather than UTF-8
        // byte indices. Only the text window is trimmed; utility JSON is not.
        let prefix = scalar_tail(prefix, 8000);
        let suffix = scalar_head(suffix, 8000);
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let id = next_serial(state.serial)?;
        let frame = worker::encode_frame(&json!({
            "type":"generate", "id":id, "config":provider_config(config), "prefix":prefix, "suffix":suffix,
            "instructions":options.instructions, "context":options.context, "personalization":options.personalization,
            "utility":options.utility, "anchor":options.anchor, "max_words":options.max_words, "alternatives":options.alternatives,
        }))?;
        state.serial = id;
        state.pending = Some(Pending { id, identity: config.provider_identity(), frame, preparing: false });
        state.idle_timeout = config.idle_release_seconds;
        state.events.clear();
        state.proceed = None;
        self.shared.cancel.store(false, Ordering::Release);
        self.shared.changed.notify_all();
        drop(state);
        self.start()?;
        Ok(id)
    }
    /// Confirm only a ready event from this exact current process/request. The
    /// runtime owns the additional foreground, document and consent authority.
    pub fn proceed(&mut self, id: u64) -> Result<()> {
        ensure!(!self.shared.stopped.load(Ordering::Acquire), "Inference has stopped");
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if id != 0 && state.serial == id && state.active == Some(id) && state.awaiting_proceed && state.pending.is_none() && state.release_through.is_none() && !self.shared.cancel.load(Ordering::Acquire) {
            state.proceed = Some((id, self.shared.epoch.load(Ordering::Acquire)));
            self.shared.changed.notify_all();
        }
        Ok(())
    }
    pub fn poll(&mut self) -> Vec<WorkerEvent> {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if self.shared.cancel.load(Ordering::Acquire) || state.release_through.is_some() || self.shared.stopped.load(Ordering::Acquire) { state.events.clear(); return Vec::new(); }
        let serial = state.serial;
        state.events.drain(..).filter(|event| event.id == serial).collect()
    }
    pub fn cancel(&mut self) -> Result<()> {
        self.shared.cancel.store(true, Ordering::Release);
        self.shared.changed.notify_all();
        Ok(())
    }
    pub fn release(&mut self) -> Result<()> {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let boundary = state.serial;
        state.release_generation = state.release_generation.checked_add(1).context("Worker release sequence exhausted")?;
        state.release_through = Some(state.release_through.map_or(boundary, |old| old.max(boundary)));
        state.proceed = None;
        state.events.clear();
        // No supervisor has ever reserved/spawned work in this case. Honor the
        // release synchronously rather than leaving an unserviceable watermark.
        if self.thread.is_none() && state.active.is_none() && state.worker_pid.is_none() {
            state.pending = None;
            state.configured = None;
            state.model_loaded = false;
            state.initializing = false;
            state.release_through = None;
            state.released_generation = state.release_generation;
            state.released_through = state.released_through.max(boundary);
            state.state = "idle";
            state.message = "Model released.";
        }
        self.shared.changed.notify_all();
        Ok(())
    }
    pub fn set_idle_timeout(&mut self, seconds: u32) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.idle_timeout = seconds;
        self.shared.changed.notify_all();
    }
    pub fn status(&self) -> Value {
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        json!({
            "state":state.state, "message":state.message, "worker_pid":state.worker_pid,
            "model_loaded":state.model_loaded, "lifecycle":if state.terminating { "terminating" } else if self.shared.stopped.load(Ordering::Acquire) { "stopped" } else if state.initializing { "loading" } else if state.awaiting_proceed { "awaiting_proceed" } else if state.evaluating { "evaluating" } else { state.state },
            "loading_detail":if state.initializing { "Preparing the local model; writing evaluation is not authorized." } else { "" },
            "initializing":state.initializing, "evaluating":state.evaluating,
            "awaiting_proceed":state.awaiting_proceed, "terminating":state.terminating,
            "owner_finished":state.finished || (self.thread.is_none() && self.shared.stopped.load(Ordering::Acquire)),
            "release_pending":state.release_generation != state.released_generation || state.release_through.is_some(),
            "release_complete":state.release_generation > 0 && state.release_generation == state.released_generation && state.release_through.is_none() && state.pending.is_none() && state.active.is_none() && state.worker_pid.is_none() && !state.terminating && !state.initializing,
            "release_generation":state.release_generation, "released_generation":state.released_generation, "released_through":state.released_through,
            "active_id":state.active, "latest_id":state.serial, "epoch":self.shared.epoch.load(Ordering::Acquire),
        })
    }
    /// Signal-only shutdown for a GUI owner that retries independent cleanup.
    pub fn request_stop(&mut self) -> Result<()> {
        self.shared.stopped.store(true, Ordering::Release);
        self.shared.cancel.store(true, Ordering::Release);
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.pending = None;
        state.proceed = None;
        state.events.clear();
        if self.thread.is_none() { state.finished = true; state.state = "stopped"; }
        self.shared.changed.notify_all();
        Ok(())
    }
    /// Join only an already-finished owner; never block the native event loop.
    pub fn is_stopped(&mut self) -> bool {
        if !self.shared.stopped.load(Ordering::Acquire) { return false; }
        if self.thread.as_ref().is_some_and(JoinHandle::is_finished) {
            if let Some(thread) = self.thread.take() { let _ = thread.join(); }
        }
        self.thread.is_none() && self.shared.state.lock().unwrap_or_else(|e| e.into_inner()).worker_pid.is_none()
    }
    /// Bounded caller wait. A still-live child remains owned and visible.
    pub fn stop(&mut self) -> Result<()> {
        self.request_stop()?;
        if self.thread.is_some() {
            let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            drop(self.shared.changed.wait_timeout_while(state, STOP_WAIT, |state| !state.finished).unwrap_or_else(|e| e.into_inner()));
        }
        self.is_stopped();
        Ok(())
    }
}
impl Drop for WorkerClient { fn drop(&mut self) { let _ = self.stop(); } }

fn next_serial(serial: u64) -> Result<u64> { serial.checked_add(1).context("Inference request identifiers exhausted") }
fn expanded_model_path(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) { return PathBuf::from(home).join(rest); }
    }
    Path::new(path).to_owned()
}
fn provider_config(config: &Config) -> Value {
    json!({"backend":config.backend, "model_path":config.model_path, "endpoint":config.endpoint, "model":config.model, "acceleration":config.acceleration, "prompt_style":config.prompt_style, "max_tokens":config.max_tokens, "context_chars":config.context_chars})
}
fn validate_options(options: &GenerationOptions) -> Result<()> {
    for (value, limit) in [(&options.instructions, 26000), (&options.context, 8000), (&options.personalization, 2048), (&options.utility, 24000), (&options.anchor, 4096)] {
        ensure!(value.chars().take(limit + 1).count() <= limit, "Invalid request text bound");
    }
    ensure!((1..=32).contains(&options.max_words), "Invalid completion length");
    Ok(())
}
fn scalar_head(text: &str, limit: usize) -> &str { &text[..text.char_indices().nth(limit).map_or(text.len(), |(index, _)| index)] }
fn scalar_tail(text: &str, limit: usize) -> &str { &text[text.char_indices().rev().nth(limit.saturating_sub(1)).map_or(0, |(index, _)| index)..] }

struct Incoming { epoch: u64, event: WorkerEvent }
struct OwnedChild {
    process: Child,
    writer: Option<SyncSender<Vec<u8>>>,
    reader_done: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
    writer_thread: Option<JoinHandle<()>>,
    terminated_at: Option<Instant>,
    killed_at: Option<Instant>,
}
impl OwnedChild {
    fn send(&self, frame: Vec<u8>) -> Result<()> {
        self.writer.as_ref().context("Worker input closed")?.try_send(frame).map_err(|_| anyhow::anyhow!("Worker input unavailable"))
    }
    fn request_termination(&mut self) {
        self.writer.take();
        if self.terminated_at.is_none() {
            #[cfg(unix)] unsafe { libc::kill(self.process.id() as libc::pid_t, libc::SIGTERM); }
            #[cfg(not(unix))] { let _ = self.process.kill(); }
            self.terminated_at = Some(Instant::now());
        }
    }
    fn reap(&mut self) -> bool {
        match self.process.try_wait() {
            Ok(Some(_)) => {
                self.writer.take();
                if self.reader.as_ref().is_some_and(JoinHandle::is_finished) { let _ = self.reader.take().unwrap().join(); }
                if self.writer_thread.as_ref().is_some_and(JoinHandle::is_finished) { let _ = self.writer_thread.take().unwrap().join(); }
                // Native exit and both pipe owners must acknowledge shutdown;
                // do not detach a reader/writer and report completed cleanup.
                self.reader.is_none() && self.writer_thread.is_none()
            }
            _ => {
                if self.terminated_at.is_some_and(|started| started.elapsed() >= TERMINATE_GRACE) && self.killed_at.is_none_or(|killed| killed.elapsed() >= TERMINATE_GRACE) {
                    let _ = self.process.kill();
                    self.killed_at = Some(Instant::now());
                }
                false
            }
        }
    }
}
// This is a last-resort signal, not the ownership protocol: normal retirement
// retains OwnedChild until try_wait acknowledges actual exit.
impl Drop for OwnedChild { fn drop(&mut self) { if self.process.try_wait().ok().flatten().is_none() { let _ = self.process.kill(); } } }

struct Supervisor {
    shared: Arc<Shared>,
    command: WorkerCommand,
    process: Option<OwnedChild>,
    terminating: Option<OwnedChild>,
    release_ack: Option<(u64, u64)>,
    incoming_tx: SyncSender<Incoming>,
    incoming: Receiver<Incoming>,
    active: Option<u64>,
    prepare_id: Option<u64>,
    configured: Option<Vec<String>>,
    cancel_since: Option<Instant>,
    cancel_sent: bool,
    initializing: bool,
    evaluating: bool,
    awaiting_proceed: bool,
    last_activity: Instant,
}
impl Supervisor {
    fn new(shared: Arc<Shared>, command: WorkerCommand) -> Self {
        let (incoming_tx, incoming) = mpsc::sync_channel(64);
        Self { shared, command, process: None, terminating: None, release_ack: None, incoming_tx, incoming, active: None, prepare_id: None, configured: None, cancel_since: None, cancel_sent: false, initializing: false, evaluating: false, awaiting_proceed: false, last_activity: Instant::now() }
    }
    fn spawn(&mut self) -> Result<()> {
        ensure!(self.process.is_none() && self.terminating.is_none(), "Previous worker still owns native resources");
        let (mut child, output) = self.command.spawn()?;
        let stdin = child.stdin.take().expect("worker command always configures piped stdin");
        let epoch = self.shared.epoch.fetch_add(1, Ordering::AcqRel) + 1;
        let (writer_tx, writer_rx) = mpsc::sync_channel::<Vec<u8>>(4);
        let mut owned = OwnedChild { process: child, writer: Some(writer_tx), reader_done: Arc::new(AtomicBool::new(false)), reader: None, writer_thread: None, terminated_at: None, killed_at: None };
        // Attach ownership before any fallible thread creation. Even an OS
        // thread-creation failure cannot lose the just-spawned native process.
        {
            let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            state.worker_pid = Some(owned.process.id());
            state.state = "loading";
            state.message = "Loading the local model…";
            state.initializing = true;
        }
        let writer_done = Arc::clone(&owned.reader_done);
        owned.writer_thread = match thread::Builder::new().name("brotypist-worker-writer".into()).spawn(move || {
            let mut stdin = stdin;
            while let Ok(frame) = writer_rx.recv() {
                if stdin.write_all(&frame).and_then(|_| stdin.flush()).is_err() { writer_done.store(true, Ordering::Release); break; }
            }
        }) {
            Ok(thread) => Some(thread),
            Err(error) => { owned.request_termination(); self.terminating = Some(owned); return Err(error.into()); }
        };
        let shared = Arc::clone(&self.shared);
        let tx = self.incoming_tx.clone();
        let reader_done = Arc::clone(&owned.reader_done);
        owned.reader = match thread::Builder::new().name("brotypist-worker-reader".into()).spawn(move || {
            let mut reader = BufReader::with_capacity(8192, output);
            let mut frame = Vec::with_capacity(4096);
            while let Ok(true) = worker::read_frame(&mut reader, &mut frame) {
                let event = match worker::decode_event(&frame) { Ok(event) => event, Err(_) => break };
                let mut item = Incoming { epoch, event };
                loop {
                    match tx.try_send(item) {
                        Ok(()) => { shared.changed.notify_all(); break; }
                        Err(TrySendError::Full(back)) => {
                            if shared.stopped.load(Ordering::Acquire) || epoch != shared.epoch.load(Ordering::Acquire) { reader_done.store(true, Ordering::Release); return; }
                            item = back;
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(TrySendError::Disconnected(_)) => { reader_done.store(true, Ordering::Release); return; }
                    }
                }
            }
            reader_done.store(true, Ordering::Release);
            shared.changed.notify_all();
        }) {
            Ok(thread) => Some(thread),
            Err(error) => { owned.request_termination(); self.terminating = Some(owned); return Err(error.into()); }
        };
        self.process = Some(owned);
        self.initializing = true;
        Ok(())
    }
    fn retire(&mut self) {
        self.shared.epoch.fetch_add(1, Ordering::AcqRel);
        if let Some(mut process) = self.process.take() {
            process.request_termination();
            self.terminating = Some(process);
        }
        if let Some(process) = self.terminating.as_mut() { process.request_termination(); }
        self.active = None;
        self.prepare_id = None;
        self.configured = None;
        self.cancel_since = None;
        self.cancel_sent = false;
        self.initializing = false;
        self.evaluating = false;
        self.awaiting_proceed = false;
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.active = None;
        state.configured = None;
        state.model_loaded = false;
        state.initializing = false;
        state.evaluating = false;
        state.awaiting_proceed = false;
        state.proceed = None;
        state.terminating = self.terminating.is_some();
    }
    fn reap(&mut self) {
        if self.terminating.as_mut().is_some_and(OwnedChild::reap) {
            self.terminating = None;
            let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            state.worker_pid = None;
            state.terminating = false;
            self.shared.changed.notify_all();
        }
        self.acknowledge_release();
    }
    fn acknowledge_release(&mut self) {
        if self.process.is_none() && self.terminating.is_none() {
            if let Some((generation, through)) = self.release_ack.take() {
                let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
                state.released_generation = state.released_generation.max(generation);
                state.released_through = state.released_through.max(through);
                self.shared.changed.notify_all();
            }
        }
    }
    fn fail(&mut self, message: &'static str) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.state = "error";
        state.message = message;
        if state.serial > 0 && !self.shared.stopped.load(Ordering::Acquire) && state.release_through.is_none() && !self.shared.cancel.load(Ordering::Acquire) {
            let mut event = WorkerEvent::new("error", state.serial);
            event.message = message.into();
            push_event(&mut state, event);
        }
    }
    fn run(mut self) {
        loop {
            #[cfg(all(test, unix))]
            if let Some(fixture) = &self.command.fixture { fixture.gate.before_step(); }
            self.reap();
            if self.shared.stopped.load(Ordering::Acquire) {
                if self.process.is_some() { self.retire(); }
                self.reap();
                if self.terminating.is_none() { break; }
                thread::sleep(Duration::from_millis(5));
                continue;
            }
            if self.step().is_err() {
                self.retire();
                self.fail(START_ERROR);
            }
            let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            drop(self.shared.changed.wait_timeout(state, Duration::from_millis(20)).unwrap_or_else(|e| e.into_inner()));
        }
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.finished = true;
        state.state = "stopped";
        state.message = "Inference has stopped.";
        state.worker_pid = None;
        state.model_loaded = false;
        state.release_through = None;
        state.released_generation = state.release_generation;
        state.released_through = state.released_through.max(state.serial);
        self.shared.changed.notify_all();
    }
    fn step(&mut self) -> Result<()> {
        let now = Instant::now();
        let release = {
            let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            let release = state.release_through.take().map(|boundary| (boundary, state.release_generation));
            if let Some((boundary, _)) = release {
                if state.serial <= boundary { state.serial = next_serial(boundary)?; }
                if state.pending.as_ref().is_some_and(|pending| pending.id <= boundary) { state.pending = None; }
                state.events.clear();
                state.proceed = None;
            }
            release
        };
        if let Some((boundary, generation)) = release {
            self.release_ack = Some((generation, boundary));
            self.retire();
            self.acknowledge_release();
            let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            state.state = "idle";
            state.message = "Model released.";
        }
        {
            let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            // Sample under the same lock as submit's serial update/clear: a
            // previously observed cancellation cannot revoke a later submit.
            if self.shared.cancel.swap(false, Ordering::AcqRel) {
                state.serial = next_serial(state.serial)?;
                if state.pending.as_ref().is_some_and(|pending| !pending.preparing) { state.pending = None; }
                state.events.clear();
                state.proceed = None;
                if self.active.is_some() && self.active != self.prepare_id && self.cancel_since.is_none() { self.cancel_since = Some(now); }
            }
        }
        loop {
            match self.incoming.try_recv() {
                Ok(incoming) if incoming.epoch == self.shared.epoch.load(Ordering::Acquire) => self.handle(incoming.event, now)?,
                Ok(_) => {}
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
        if self.process.as_ref().is_some_and(|process| process.reader_done.load(Ordering::Acquire)) {
            let current = { let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner()); self.active == Some(state.serial) };
            if current { self.fail(EXIT_ERROR); }
            self.retire();
        }
        let identity_changed = {
            let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            state.pending.as_ref().is_some_and(|pending| self.configured.as_ref().is_some_and(|configured| configured != &pending.identity))
        };
        if identity_changed { self.retire(); }
        let superseded = {
            let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            self.active.is_some() && self.active != self.prepare_id && (state.pending.is_some() || self.active != Some(state.serial))
        };
        if superseded && !self.cancel_sent {
            if let Some(process) = &self.process { process.send(b"{\"type\":\"cancel\"}\n".to_vec())?; }
            self.cancel_sent = true;
            if self.cancel_since.is_none() { self.cancel_since = Some(now); }
        }
        if !self.initializing && self.cancel_since.is_some_and(|started| started.elapsed() >= if self.evaluating { PROMPT_EVALUATION_GRACE } else { CANCEL_GRACE }) {
            self.retire();
            let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            state.state = "idle";
            state.message = "Suggestion cancelled.";
        }
        self.confirm_proceed()?;
        if self.active.is_none() && self.terminating.is_none() {
            let pending = {
                let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
                let pending = state.pending.take();
                // Reserve ownership before OS spawn releases this lock, so a
                // concurrent configuration-only prepare cannot supersede it.
                if let Some(request) = &pending { state.active = Some(request.id); }
                pending
            };
            if let Some(pending) = pending {
                if self.process.is_none() { self.spawn()?; }
                let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
                if self.shared.stopped.load(Ordering::Acquire) || state.release_through.is_some() || (!pending.preparing && (self.shared.cancel.load(Ordering::Acquire) || pending.id != state.serial)) {
                    // A preparation is not superseded by ordinary typing. A
                    // newer pending request must never be overwritten here.
                    if !self.shared.stopped.load(Ordering::Acquire) && state.pending.is_none() && (pending.preparing || pending.id == state.serial) { state.pending = Some(pending); }
                    state.active = None;
                    return Ok(());
                }
                self.active = Some(pending.id);
                self.prepare_id = pending.preparing.then_some(pending.id);
                self.configured = Some(pending.identity);
                self.cancel_sent = false;
                self.cancel_since = None;
                self.last_activity = now;
                state.active = self.active;
                state.configured = self.configured.clone();
                state.awaiting_proceed = false;
                drop(state);
                self.process.as_ref().context("Worker unavailable")?.send(pending.frame)?;
            } else if self.process.is_some() {
                let timeout = self.shared.state.lock().unwrap_or_else(|e| e.into_inner()).idle_timeout;
                if timeout > 0 && self.last_activity.elapsed() >= Duration::from_secs(timeout.into()) {
                    self.retire();
                    let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
                    state.state = "idle";
                    state.message = "Model released after inactivity.";
                }
            }
        }
        Ok(())
    }
    fn confirm_proceed(&mut self) -> Result<()> {
        let authorized = {
            let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            match state.proceed.take() {
                Some((id, epoch)) if self.awaiting_proceed && self.active == Some(id) && state.serial == id && state.pending.is_none() && state.release_through.is_none() && !self.shared.cancel.load(Ordering::Acquire) && epoch == self.shared.epoch.load(Ordering::Acquire) && !self.shared.stopped.load(Ordering::Acquire) => Some(id),
                _ => None,
            }
        };
        if let Some(id) = authorized {
            self.process.as_ref().context("Worker unavailable")?.send(worker::encode_frame(&json!({"type":"proceed", "id":id}))?)?;
            self.awaiting_proceed = false;
            self.evaluating = true;
            let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            state.awaiting_proceed = false;
            state.evaluating = true;
            state.state = "generating";
            state.message = "Generating a local suggestion…";
        }
        Ok(())
    }
    fn handle(&mut self, event: WorkerEvent, now: Instant) -> Result<()> {
        if self.active != Some(event.id) { return Ok(()); }
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        match event.kind.as_str() {
            "done" => {
                self.initializing = false;
                self.evaluating = false;
                self.awaiting_proceed = false;
                state.model_loaded = event.model_loaded;
                self.active = None;
                self.cancel_since = None;
                self.cancel_sent = false;
                self.last_activity = now;
                if self.prepare_id == Some(event.id) { self.prepare_id = None; }
                state.active = None;
                state.proceed = None;
                if state.state != "error" { state.state = "ready"; state.message = "Ready."; }
            }
            "result" => { self.evaluating = false; }
            "loading" => { self.initializing = true; }
            "prepared" => {
                self.initializing = false;
                state.model_loaded = event.model_loaded;
                state.state = "ready";
                state.message = "Local model prepared; waiting for writing.";
            }
            "ready" => {
                self.initializing = false;
                self.awaiting_proceed = true;
                state.model_loaded = event.model_loaded;
                if self.cancel_since.is_some() { self.cancel_since = Some(now); }
            }
            _ => {}
        }
        state.initializing = self.initializing;
        state.evaluating = self.evaluating;
        state.awaiting_proceed = self.awaiting_proceed;
        if event.id != state.serial || state.release_through.is_some() || self.shared.cancel.load(Ordering::Acquire) { return Ok(()); }
        match event.kind.as_str() {
            "ready" => { state.state = "ready"; state.message = "Local model ready; waiting for writing authorization."; }
            "loading" => { state.state = "loading"; state.message = "Loading the local model…"; }
            "error" => { state.state = "error"; state.message = worker::INFERENCE_ERROR; }
            _ => {}
        }
        push_event(&mut state, event);
        self.shared.changed.notify_all();
        Ok(())
    }
}
fn push_event(state: &mut State, event: WorkerEvent) {
    if state.events.len() == 256 { state.events.pop_front(); }
    state.events.push_back(event);
}

// Test children execute this very protocol/owner and supervisor. A dedicated
// inherited descriptor keeps libtest's own banner off the private JSON pipe.
#[cfg(all(test, unix))]
struct TestCommand {
    control: std::os::unix::net::UnixStream,
    mode: &'static str,
    gate: Arc<TestStepGate>,
}
#[cfg(all(test, unix))]
impl TestCommand {
    fn spawn(&self, mut command: Command) -> Result<(Child, Box<dyn Read + Send>)> {
        use std::os::fd::AsRawFd;
        use std::os::unix::process::CommandExt;
        let (parent, child) = std::os::unix::net::UnixStream::pair()?;
        parent.set_read_timeout(Some(Duration::from_secs(8)))?;
        let output_fd = child.as_raw_fd();
        let control_fd = self.control.as_raw_fd();
        command.args(["--exact", "process_client::tests::fixture_child", "--nocapture"])
            .stdout(Stdio::null())
            .env("BROTYPIST_TEST_WORKER_MODE", self.mode)
            .env("BROTYPIST_TEST_PROTOCOL_FD", output_fd.to_string())
            .env("BROTYPIST_TEST_CONTROL_FD", control_fd.to_string());
        unsafe {
            command.pre_exec(move || {
                if libc::fcntl(output_fd, libc::F_SETFD, 0) < 0 || libc::fcntl(control_fd, libc::F_SETFD, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let process = command.spawn()?;
        Ok((process, Box::new(parent)))
    }
}

#[cfg(all(test, unix))]
#[derive(Default)]
struct TestStepGate { state: Mutex<(bool, bool)>, changed: Condvar }
#[cfg(all(test, unix))]
impl TestStepGate {
    fn before_step(&self) {
        let mut state = self.state.lock().unwrap();
        while state.0 {
            state.1 = true;
            self.changed.notify_all();
            state = self.changed.wait(state).unwrap();
        }
        state.1 = false;
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::providers::Provider;
    use std::cell::{Cell, RefCell};
    use std::io::BufRead;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::net::UnixStream;
    use std::rc::Rc;

    struct Control { reader: BufReader<UnixStream>, writer: UnixStream }
    impl Control {
        fn new(stream: UnixStream) -> Self {
            stream.set_read_timeout(Some(Duration::from_secs(8))).unwrap();
            Self { writer: stream.try_clone().unwrap(), reader: BufReader::new(stream) }
        }
        fn send(&mut self, value: Value) {
            self.writer.write_all(&worker::encode_frame(&value).unwrap()).unwrap();
            self.writer.flush().unwrap();
        }
        fn next(&mut self) -> Value {
            let mut line = String::new();
            assert!(self.reader.read_line(&mut line).unwrap() > 0, "fixture control closed");
            serde_json::from_str(&line).unwrap()
        }
        fn stage(&mut self, expected: &str) -> Value {
            let event = self.next();
            assert_eq!(event["stage"], expected, "{event}");
            event
        }
        fn go(&mut self) { self.send(json!({"go":true})); }
    }

    struct FixtureProvider {
        config: Config,
        allocation: usize,
        attempts: usize,
        control: Rc<RefCell<Control>>,
        closed: bool,
    }
    impl Provider for FixtureProvider {
        fn prepare(&mut self) -> Result<()> {
            self.attempts += 1;
            self.control.borrow_mut().send(json!({"stage":"prepare","allocation":self.allocation,"attempt":self.attempts}));
            // Exercise both CRT/native diagnostic handles while real protocol
            // output uses the duplicate established by private_stdout().
            let diagnostic = b"private authored native diagnostic\n";
            unsafe { libc::write(1, diagnostic.as_ptr().cast(), diagnostic.len()); libc::write(2, diagnostic.as_ptr().cast(), diagnostic.len()); }
            if self.config.model_path.contains("cold-gate") { assert_eq!(self.control.borrow_mut().next()["go"], true); }
            if self.config.model_path.contains("fail-once") && self.attempts == 1 { anyhow::bail!("private authored model failure"); }
            Ok(())
        }
        fn stream(&mut self, prefix: &str, suffix: &str, _options: &GenerationOptions, cancel: &AtomicBool, emit: &mut dyn FnMut(&str) -> Result<()>) -> Result<()> {
            self.control.borrow_mut().send(json!({"stage":"stream","prefix":prefix,"suffix":suffix,"allocation":self.allocation,"style":self.config.prompt_style,"context_chars":self.config.context_chars}));
            if prefix == "private failure" { anyhow::bail!("private authored backend response"); }
            if prefix == "blocked prompt" { assert_eq!(self.control.borrow_mut().next()["go"], true); }
            if cancel.load(Ordering::Acquire) { return Ok(()); }
            emit(" first")?;
            if prefix == "blocked stream" { assert_eq!(self.control.borrow_mut().next()["go"], true); }
            if !cancel.load(Ordering::Acquire) { emit(&format!(" completed {}", scalar_head(prefix, 128)))?; }
            Ok(())
        }
        fn reconfigure(&mut self, config: Config) -> Result<()> { self.config = config; Ok(()) }
        fn close(&mut self) -> Result<()> {
            if !self.closed {
                self.control.borrow_mut().send(json!({"stage":"close","allocation":self.allocation}));
                self.closed = true;
            }
            Ok(())
        }
    }

    /// Launched only in an explicitly marked disposable subprocess. The normal
    /// test invocation performs no process-wide descriptor changes.
    #[test]
    fn fixture_child() {
        let Ok(mode) = std::env::var("BROTYPIST_TEST_WORKER_MODE") else { return; };
        let protocol_fd: i32 = std::env::var("BROTYPIST_TEST_PROTOCOL_FD").unwrap().parse().unwrap();
        let control_fd: i32 = std::env::var("BROTYPIST_TEST_CONTROL_FD").unwrap().parse().unwrap();
        let inherited = unsafe { std::fs::File::from_raw_fd(protocol_fd) };
        assert!(unsafe { libc::dup2(inherited.as_raw_fd(), 1) } >= 0);
        drop(inherited);
        let mut protocol = worker::private_stdout().unwrap();
        if mode == "oversized-output" {
            protocol.write_all(&vec![b'x'; MAX_MESSAGE_BYTES + 1]).unwrap();
            protocol.flush().unwrap();
            // The supervisor, not a fixture self-exit, must retire this child.
            loop { thread::park(); }
        }
        if mode == "malformed-output" {
            protocol.write_all(b"{\"type\":\"ready\",\"id\":true}\n").unwrap();
            protocol.flush().unwrap();
            loop { thread::park(); }
        }
        if mode == "ignore-term" { unsafe { libc::signal(libc::SIGTERM, libc::SIG_IGN); } }
        let control = Rc::new(RefCell::new(Control::new(unsafe { UnixStream::from_raw_fd(control_fd) })));
        let allocations = Rc::new(Cell::new(0usize));
        let result = worker::run_worker(std::io::stdin(), protocol, move |config| {
            let allocation = allocations.get() + 1;
            allocations.set(allocation);
            control.borrow_mut().send(json!({"stage":"create","allocation":allocation,"model":config.model_path}));
            Ok(Box::new(FixtureProvider { config, allocation, attempts: 0, control: Rc::clone(&control), closed: false }) as Box<dyn Provider>)
        });
        std::process::exit(result.unwrap_or(2));
    }

    struct Harness { command: WorkerCommand, control: Control }
    impl Harness {
        fn new(mode: &'static str) -> Self {
            let (parent, child) = UnixStream::pair().unwrap();
            Self {
                command: WorkerCommand { executable: std::env::current_exe().unwrap(), fixture: Some(Arc::new(TestCommand { control: child, mode, gate: Arc::new(TestStepGate::default()) })) },
                control: Control::new(parent),
            }
        }
        fn client(&self) -> WorkerClient { WorkerClient::with_command(self.command.clone()).unwrap() }
        fn raw(&self) -> RawWorker {
            let (mut child, output) = self.command.spawn().unwrap();
            RawWorker { stdin: child.stdin.take(), child, reader: BufReader::new(output) }
        }
        fn pause(&self, client: &WorkerClient) -> PauseGuard {
            let gate = Arc::clone(&self.command.fixture.as_ref().unwrap().gate);
            {
                let mut state = gate.state.lock().unwrap();
                state.0 = true;
                client.shared.changed.notify_all();
                let (state, timeout) = gate.changed.wait_timeout_while(state, Duration::from_secs(5), |state| !state.1).unwrap();
                assert!(!timeout.timed_out() && state.1, "supervisor did not reach deterministic gate");
            }
            PauseGuard(gate)
        }
    }
    struct PauseGuard(Arc<TestStepGate>);
    impl Drop for PauseGuard {
        fn drop(&mut self) { self.0.state.lock().unwrap().0 = false; self.0.changed.notify_all(); }
    }
    struct RawWorker { child: Child, stdin: Option<std::process::ChildStdin>, reader: BufReader<Box<dyn Read + Send>> }
    impl RawWorker {
        fn send(&mut self, value: Value) { self.stdin.as_mut().unwrap().write_all(&worker::encode_frame(&value).unwrap()).unwrap(); }
        fn next(&mut self) -> WorkerEvent {
            let mut bytes = Vec::new();
            assert!(worker::read_frame(&mut self.reader, &mut bytes).unwrap(), "protocol ended unexpectedly");
            worker::decode_event(&bytes).unwrap()
        }
        fn through(&mut self, kind: &str, id: u64) -> Vec<WorkerEvent> {
            let mut events = Vec::new();
            loop {
                let event = self.next();
                let finished = event.kind == kind && event.id == id;
                events.push(event);
                if finished { return events; }
            }
        }
        fn exit(&mut self) -> i32 {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(status) = self.child.try_wait().unwrap() { return status.code().unwrap_or(-1); }
                assert!(Instant::now() < deadline, "child did not acknowledge exit");
                thread::sleep(Duration::from_millis(2));
            }
        }
    }
    impl Drop for RawWorker { fn drop(&mut self) { let _ = self.child.kill(); let _ = self.child.wait(); } }
    fn request(id: u64, prefix: &str, config: &Config) -> Value { json!({"type":"generate","id":id,"config":provider_config(config),"prefix":prefix}) }
    fn local(model: &str) -> Config { Config { model_path: model.into(), idle_release_seconds: 0, ..Config::default() } }
    fn server() -> Config { Config { backend:"openai".into(), idle_release_seconds:0, ..Config::default() } }
    fn wait_status(client: &WorkerClient, check: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(6);
        loop {
            let status = client.status();
            if check(&status) { return status; }
            assert!(Instant::now() < deadline, "state transition missing: {status}");
            let state = client.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            drop(client.shared.changed.wait_timeout(state, Duration::from_millis(10)).unwrap_or_else(|e| e.into_inner()));
        }
    }
    fn wait_event(client: &mut WorkerClient, id: u64, kind: &str) -> Vec<WorkerEvent> {
        let deadline = Instant::now() + Duration::from_secs(6);
        let mut events = Vec::new();
        loop {
            for event in client.poll() {
                assert_eq!(event.id, id, "stale event surfaced");
                assert_ne!(event.kind, "error", "{}", event.message);
                let found = event.kind == kind;
                events.push(event);
                if found { return events; }
            }
            assert!(Instant::now() < deadline, "event {kind} missing: {}", client.status());
            let state = client.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            drop(client.shared.changed.wait_timeout(state, Duration::from_millis(10)).unwrap_or_else(|e| e.into_inner()));
        }
    }
    fn complete(client: &mut WorkerClient, id: u64) -> Vec<WorkerEvent> {
        let mut events = wait_event(client, id, "ready");
        client.proceed(id).unwrap();
        events.extend(wait_event(client, id, "done"));
        assert!(events.iter().any(|event| event.kind == "result" && event.final_));
        events
    }
    fn assert_dead(pid: u32) {
        assert_eq!(unsafe { libc::kill(pid as libc::pid_t, 0) }, -1, "child is still alive");
        assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
    }

    #[test]
    fn real_child_has_ready_proceed_barrier_and_silences_native_diagnostics() {
        let mut harness = Harness::new("normal");
        let mut raw = harness.raw();
        raw.send(request(1, "must not evaluate", &local("synthetic.gguf")));
        assert_eq!(harness.control.stage("create")["allocation"], 1);
        harness.control.stage("prepare");
        let before = raw.through("ready", 1);
        assert_eq!(before.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(), ["loading", "ready"]);
        // A mismatched confirmation cannot start evaluation. Cancel + done is a
        // deterministic acknowledgment that the ready wait ended without it.
        raw.send(json!({"type":"proceed","id":2}));
        raw.send(json!({"type":"cancel"}));
        assert!(raw.through("done", 1).iter().all(|e| e.kind != "result"));
        raw.send(json!({"type":"stop"}));
        assert_eq!(harness.control.stage("close")["allocation"], 1);
        assert_eq!(raw.exit(), 0);
    }

    #[test]
    fn real_child_emits_cumulative_replacements_and_one_final_result() {
        let mut harness = Harness::new("normal");
        let mut raw = harness.raw();
        raw.send(request(7, "fixture", &server()));
        harness.control.stage("create");
        assert_eq!(raw.next().kind, "ready");
        raw.send(json!({"type":"proceed","id":7}));
        assert_eq!(harness.control.stage("stream")["prefix"], "fixture");
        let events = raw.through("done", 7);
        assert_eq!(events.iter().filter(|e| e.kind == "result").map(|e| (e.text.as_str(), e.final_)).collect::<Vec<_>>(), [(" first",false), (" completed fixture",false), (" completed fixture",true)]);
        raw.send(json!({"type":"stop"}));
        harness.control.stage("close");
        assert_eq!(raw.exit(), 0);
    }

    #[test]
    fn real_child_keeps_one_replaceable_pending_request() {
        let mut harness = Harness::new("normal");
        let mut raw = harness.raw();
        let config = server();
        raw.send(request(1, "blocked stream", &config));
        harness.control.stage("create");
        raw.through("ready", 1);
        raw.send(json!({"type":"proceed","id":1}));
        harness.control.stage("stream");
        raw.through("result", 1);
        raw.send(request(2, "obsolete", &config));
        raw.send(request(3, "latest", &config));
        // done(2) proves request 3 replaced pending work before the owner is
        // released, independent of process/thread scheduling.
        raw.through("done", 2);
        harness.control.go();
        let after = raw.through("ready", 3);
        assert!(!after.iter().any(|e| e.id == 2 && e.kind == "ready"));
        raw.send(json!({"type":"proceed","id":3}));
        assert_eq!(harness.control.stage("stream")["prefix"], "latest");
        raw.through("done", 3);
        raw.send(json!({"type":"stop"}));
        harness.control.stage("close");
        assert_eq!(raw.exit(), 0);
    }

    #[test]
    fn real_child_rejects_bad_input_and_closes_owned_provider_on_eof_or_malformed_frame() {
        let invalid = [
            b"[]\n".to_vec(), b"{\"type\":\"generate\",\"id\":true,\"config\":{}}\n".to_vec(),
            b"{\"type\":\"stop\",\"unknown\":true}\n".to_vec(),
            b"{\"type\":\"generate\",\"id\":1,\"config\":{\"unknown\":true}}\n".to_vec(),
            b"{\"type\":\"generate\",\"id\":1,\"config\":{},\"alternatives\":0}\n".to_vec(),
            b"{\"type\":\"generate\",\"id\":1,\"id\":2,\"config\":{}}\n".to_vec(),
            vec![b'x'; MAX_MESSAGE_BYTES + 1], b"{\"type\":\"stop\"}".to_vec(),
        ];
        for bytes in invalid {
            let harness = Harness::new("normal");
            let mut raw = harness.raw();
            // Oversized input may be rejected before its final byte is written.
            let _ = raw.stdin.as_mut().unwrap().write_all(&bytes);
            raw.stdin.take();
            assert_eq!(raw.exit(), 2);
        }
        for malformed in [false, true] {
            let mut harness = Harness::new("normal");
            let mut raw = harness.raw();
            raw.send(request(1, "owned allocation", &local("synthetic.gguf")));
            harness.control.stage("create");
            harness.control.stage("prepare");
            raw.through("ready", 1);
            if malformed { raw.stdin.as_mut().unwrap().write_all(b"null\n").unwrap(); }
            raw.stdin.take();
            assert_eq!(harness.control.stage("close")["allocation"], 1);
            assert_eq!(raw.exit(), if malformed { 2 } else { 0 });
        }
    }

    #[test]
    fn client_requires_explicit_fresh_confirmation_and_release_invalidates_it() {
        let mut harness = Harness::new("normal");
        let mut client = harness.client();
        assert!(client.status()["worker_pid"].is_null());
        let id = client.submit(&server(), "unconfirmed", "", &GenerationOptions::default()).unwrap();
        harness.control.stage("create");
        wait_event(&mut client, id, "ready");
        let pid = client.status()["worker_pid"].as_u64().unwrap() as u32;
        assert_eq!(client.status()["lifecycle"], "awaiting_proceed");
        client.release().unwrap();
        client.proceed(id).unwrap();
        wait_status(&client, |s| s["worker_pid"].is_null());
        assert_dead(pid);
        assert!(client.poll().is_empty());
        client.stop().unwrap();
    }

    #[test]
    fn client_preserves_native_allocation_across_request_only_reconfiguration() {
        let mut harness = Harness::new("normal");
        let mut client = harness.client();
        let config = local("synthetic.gguf");
        let first = client.submit(&config, "first", "", &GenerationOptions::default()).unwrap();
        harness.control.stage("create");
        harness.control.stage("prepare");
        complete(&mut client, first);
        harness.control.stage("stream");
        let pid = client.status()["worker_pid"].clone();
        let changed = Config { prompt_style:"writing".into(), context_chars:4000, max_tokens:64, model:"inactive-server-setting".into(), ..config };
        let next = client.submit(&changed, "next", "", &GenerationOptions::default()).unwrap();
        complete(&mut client, next);
        let stream = harness.control.stage("stream");
        assert_eq!(stream["allocation"], 1);
        assert_eq!(stream["style"], "writing");
        assert_eq!(stream["context_chars"], 4000);
        assert_eq!(client.status()["worker_pid"], pid);
        client.stop().unwrap();
        assert!(client.status()["worker_pid"].is_null());
    }

    #[test]
    fn client_cold_supersession_preserves_one_loader_and_only_latest_is_evaluated() {
        let mut harness = Harness::new("normal");
        let mut client = harness.client();
        let config = local("cold-gate.gguf");
        client.submit(&config, "old", "", &GenerationOptions::default()).unwrap();
        harness.control.stage("create");
        harness.control.stage("prepare");
        let pid = wait_status(&client, |s| s["initializing"] == true)["worker_pid"].clone();
        client.cancel().unwrap();
        client.submit(&config, "middle one", "", &GenerationOptions::default()).unwrap();
        client.submit(&config, "middle two", "", &GenerationOptions::default()).unwrap();
        let latest = client.submit(&config, "latest", "", &GenerationOptions::default()).unwrap();
        harness.control.go();
        let events = complete(&mut client, latest);
        assert!(events.iter().filter(|e| e.kind == "result").all(|e| !e.text.contains("old") && !e.text.contains("middle")));
        let stream = harness.control.stage("stream");
        assert_eq!(stream["prefix"], "latest");
        assert_eq!(stream["allocation"], 1);
        assert_eq!(client.status()["worker_pid"], pid);
        client.stop().unwrap();
    }

    #[test]
    fn client_preparation_never_sends_writing_and_survives_typing_cancel() {
        let directory = tempfile::tempdir().unwrap();
        let model = directory.path().join("cold-gate.gguf");
        std::fs::write(&model, b"").unwrap();
        let config = local(model.to_str().unwrap());
        let mut harness = Harness::new("normal");
        let mut client = harness.client();
        let preparing = client.prepare(&config).unwrap();
        assert!(preparing > 0);
        harness.control.stage("create");
        harness.control.stage("prepare");
        let pid = client.status()["worker_pid"].clone();
        client.cancel().unwrap();
        let id = client.submit(&config, "fresh writing", "", &GenerationOptions::default()).unwrap();
        harness.control.go();
        let events = complete(&mut client, id);
        assert!(!events.iter().any(|e| e.id == preparing));
        assert_eq!(harness.control.stage("stream")["prefix"], "fresh writing");
        assert_eq!(client.status()["worker_pid"], pid);
        assert_eq!(client.prepare(&config).unwrap(), 0);
        client.stop().unwrap();
    }

    #[test]
    fn preparation_acknowledges_without_ready_or_any_stream() {
        let model = tempfile::NamedTempFile::new().unwrap();
        let mut harness = Harness::new("normal");
        let mut client = harness.client();
        let id = client.prepare(&local(model.path().to_str().unwrap())).unwrap();
        harness.control.stage("create");
        harness.control.stage("prepare");
        let events = wait_event(&mut client, id, "done");
        assert!(events.iter().any(|e| e.kind == "prepared" && e.model_loaded));
        assert!(events.iter().all(|e| !matches!(e.kind.as_str(), "result" | "ready")));
        assert_eq!(client.status()["model_loaded"], true);
        client.stop().unwrap();
    }

    #[test]
    fn client_release_watermarks_preserve_only_work_authorized_after_the_boundary() {
        let mut harness = Harness::new("normal");
        let mut client = harness.client();
        let config = server();
        let first = client.submit(&config, "first", "", &GenerationOptions::default()).unwrap();
        harness.control.stage("create");
        complete(&mut client, first);
        harness.control.stage("stream");
        let original = client.status()["worker_pid"].as_u64().unwrap() as u32;
        // Suspend only the supervisor; queue ordering through the real public
        // consumer API, then resume the real process lifecycle implementation.
        let gate = harness.pause(&client);
        client.release().unwrap();
        let obsolete = client.submit(&config, "revoked", "", &GenerationOptions::default()).unwrap();
        client.release().unwrap();
        let latest = client.submit(&config, "newly authorized", "", &GenerationOptions::default()).unwrap();
        assert!(latest > obsolete);
        drop(gate);
        let events = complete(&mut client, latest);
        // A killed worker need not run cooperative close. The only additional
        // control events are a new allocation and the authorized stream.
        harness.control.stage("create");
        assert_eq!(harness.control.stage("stream")["prefix"], "newly authorized");
        assert!(events.iter().all(|event| event.id == latest));
        assert_ne!(client.status()["worker_pid"].as_u64().unwrap(), original as u64);
        assert_dead(original);
        client.stop().unwrap();
    }

    #[test]
    fn model_switch_and_stop_interrupt_cold_native_ownership() {
        for stop in [false, true] {
            let mut harness = Harness::new("ignore-term");
            let mut client = harness.client();
            client.submit(&local("cold-gate.gguf"), "blocked", "", &GenerationOptions::default()).unwrap();
            harness.control.stage("create");
            harness.control.stage("prepare");
            let pid = client.status()["worker_pid"].as_u64().unwrap() as u32;
            if stop {
                let began = Instant::now();
                client.stop().unwrap();
                assert!(began.elapsed() < Duration::from_millis(1200));
                wait_status(&client, |s| s["worker_pid"].is_null());
                assert!(client.submit(&server(), "after stop", "", &GenerationOptions::default()).is_err());
            } else {
                let id = client.submit(&local("another.gguf"), "switched", "", &GenerationOptions::default()).unwrap();
                harness.control.stage("create");
                harness.control.stage("prepare");
                complete(&mut client, id);
                assert_eq!(harness.control.stage("stream")["prefix"], "switched");
                assert_ne!(client.status()["worker_pid"].as_u64().unwrap(), pid as u64);
                client.stop().unwrap();
            }
            assert_dead(pid);
        }
    }

    #[test]
    fn prompt_cancellation_has_longer_grace_and_keeps_warm_native_allocation() {
        let mut harness = Harness::new("normal");
        let mut client = harness.client();
        let config = local("synthetic.gguf");
        let id = client.submit(&config, "blocked prompt", "", &GenerationOptions::default()).unwrap();
        harness.control.stage("create");
        harness.control.stage("prepare");
        wait_event(&mut client, id, "ready");
        client.proceed(id).unwrap();
        harness.control.stage("stream");
        let pid = client.status()["worker_pid"].clone();
        client.cancel().unwrap();
        wait_status(&client, |s| s["latest_id"].as_u64().unwrap() > id);
        // Deliberately exceed token-stream grace while the test provider is
        // deterministically inside prompt evaluation with no emitted token.
        thread::sleep(CANCEL_GRACE + Duration::from_millis(120));
        assert_eq!(client.status()["worker_pid"], pid);
        let next = client.submit(&config, "after prompt", "", &GenerationOptions::default()).unwrap();
        harness.control.go();
        complete(&mut client, next);
        assert_eq!(harness.control.stage("stream")["prefix"], "after prompt");
        assert_eq!(client.status()["worker_pid"], pid);
        client.stop().unwrap();
    }

    #[test]
    fn streaming_cancellation_kills_unresponsive_child_and_replaces_it() {
        let mut harness = Harness::new("normal");
        let mut client = harness.client();
        let config = server();
        let id = client.submit(&config, "blocked stream", "", &GenerationOptions::default()).unwrap();
        harness.control.stage("create");
        wait_event(&mut client, id, "ready");
        client.proceed(id).unwrap();
        harness.control.stage("stream");
        wait_event(&mut client, id, "result");
        let old = client.status()["worker_pid"].as_u64().unwrap() as u32;
        let next = client.submit(&config, "latest", "", &GenerationOptions::default()).unwrap();
        harness.control.stage("create");
        let events = complete(&mut client, next);
        assert_eq!(harness.control.stage("stream")["prefix"], "latest");
        assert!(events.iter().all(|event| event.id == next));
        assert_dead(old);
        client.stop().unwrap();
    }

    #[test]
    fn supervisor_retires_children_with_malformed_or_oversized_stdout() {
        for mode in ["malformed-output", "oversized-output"] {
            let harness = Harness::new(mode);
            let mut client = harness.client();
            client.submit(&server(), "private draft", "", &GenerationOptions::default()).unwrap();
            wait_status(&client, |s| s["state"] == "error" && s["worker_pid"].is_null());
            let events = client.poll();
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].kind, "error");
            assert!(!events[0].message.contains("private draft"));
            client.stop().unwrap();
        }
    }

    #[test]
    fn worker_errors_never_echo_provider_content_and_failed_native_prepare_can_retry() {
        let mut harness = Harness::new("normal");
        let mut client = harness.client();
        let config = local("fail-once.gguf");
        let first = client.submit(&config, "first", "", &GenerationOptions::default()).unwrap();
        harness.control.stage("create");
        harness.control.stage("prepare");
        wait_status(&client, |s| s["state"] == "error" && s["active_id"].is_null());
        let errors = client.poll();
        assert!(errors.iter().any(|event| event.id == first && event.kind == "error" && event.message == worker::INFERENCE_ERROR));
        let retry = client.submit(&config, "retry", "", &GenerationOptions::default()).unwrap();
        assert_eq!(harness.control.stage("prepare")["attempt"], 2);
        complete(&mut client, retry);
        assert_eq!(harness.control.stage("stream")["prefix"], "retry");
        client.stop().unwrap();
    }

    #[test]
    fn client_idle_zero_retains_worker_and_positive_timeout_reaps_it() {
        let mut harness = Harness::new("normal");
        let mut client = harness.client();
        let id = client.submit(&server(), "resident", "", &GenerationOptions::default()).unwrap();
        harness.control.stage("create");
        complete(&mut client, id);
        harness.control.stage("stream");
        let pid = client.status()["worker_pid"].as_u64().unwrap() as u32;
        assert_eq!(client.shared.state.lock().unwrap().idle_timeout, 0);
        client.set_idle_timeout(1);
        wait_status(&client, |s| s["worker_pid"].is_null());
        assert_dead(pid);
        assert_eq!(client.status()["message"], "Model released after inactivity.");
        client.stop().unwrap();
    }

    #[test]
    fn oversized_submission_does_not_spawn_or_invalidate_existing_authority() {
        let mut harness = Harness::new("normal");
        let mut client = harness.client();
        let mut options = GenerationOptions::default();
        options.instructions = "\0".repeat(26000);
        options.utility = "\0".repeat(24000);
        assert!(client.submit(&server(), "", "", &options).is_err());
        assert_eq!(client.status()["latest_id"], 0);
        assert!(client.status()["worker_pid"].is_null());
        assert!(client.thread.is_none());
        assert_eq!(scalar_tail("a🦀bc", 3), "🦀bc");
        assert_eq!(scalar_head("a🦀bc", 2), "a🦀");
        assert_eq!(scalar_tail("short", 8000), "short");
        assert_eq!(scalar_head("short", 8000), "short");
        let id = client.submit(&server(), "still authorized", "", &GenerationOptions::default()).unwrap();
        harness.control.stage("create");
        wait_event(&mut client, id, "ready");
        assert!(client.submit(&server(), "rejected", "", &options).is_err());
        assert_eq!(client.status()["latest_id"], id);
        assert_eq!(client.status()["awaiting_proceed"], true);
        client.proceed(id).unwrap();
        wait_event(&mut client, id, "done");
        assert_eq!(harness.control.stage("stream")["prefix"], "still authorized");
        client.stop().unwrap();
    }

    #[test]
    fn cancelled_ready_cannot_be_confirmed_and_following_request_reuses_worker() {
        let mut harness = Harness::new("normal");
        let mut client = harness.client();
        let config = server();
        let obsolete = client.submit(&config, "revoked before proceed", "", &GenerationOptions::default()).unwrap();
        harness.control.stage("create");
        wait_event(&mut client, obsolete, "ready");
        let pid = client.status()["worker_pid"].clone();
        let gate = harness.pause(&client);
        client.cancel().unwrap();
        client.proceed(obsolete).unwrap();
        assert!(client.poll().is_empty());
        let latest = client.submit(&config, "fresh authority", "", &GenerationOptions::default()).unwrap();
        client.proceed(obsolete).unwrap();
        drop(gate);
        complete(&mut client, latest);
        assert_eq!(harness.control.stage("stream")["prefix"], "fresh authority");
        assert_eq!(client.status()["worker_pid"], pid);
        client.request_stop().unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !client.is_stopped() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        assert!(client.status()["worker_pid"].is_null());
        assert_eq!(client.status()["owner_finished"], true);
    }

    #[test]
    fn bounded_reader_consumes_real_fragmented_pipe_frames() {
        let (mut sender, receiver) = UnixStream::pair().unwrap();
        let owner = thread::spawn(move || {
            for fragment in [&b"{\"type\":\"res"[..], &b"ult\",\"id\":1,\"text\":\""[..], "🦀".as_bytes(), &b"\",\"final\":true}\n"[..]] { sender.write_all(fragment).unwrap(); }
        });
        let mut reader = BufReader::with_capacity(3, receiver);
        let mut frame = Vec::new();
        assert!(worker::read_frame(&mut reader, &mut frame).unwrap());
        assert_eq!(worker::decode_event(&frame).unwrap().text, "🦀");
        assert!(!worker::read_frame(&mut reader, &mut frame).unwrap());
        owner.join().unwrap();
    }

    #[test]
    fn consumer_pipe_preserves_unicode_scalar_windows_not_utf8_byte_windows() {
        let mut harness = Harness::new("normal");
        let mut client = harness.client();
        let prefix = format!("{}{}", "drop".repeat(1000), "🦀".repeat(8000));
        let suffix = format!("{}{}", "尾".repeat(8000), "drop".repeat(1000));
        let id = client.submit(&server(), &prefix, &suffix, &GenerationOptions::default()).unwrap();
        harness.control.stage("create");
        wait_event(&mut client, id, "ready");
        client.proceed(id).unwrap();
        let stream = harness.control.stage("stream");
        wait_event(&mut client, id, "done");
        assert_eq!(stream["prefix"], "🦀".repeat(8000));
        assert_eq!(stream["suffix"], "尾".repeat(8000));
        client.stop().unwrap();
    }

    #[test]
    fn never_started_release_is_synchronous_and_queued_startup_cannot_fake_release_completion() {
        let harness = Harness::new("normal");
        let mut untouched = harness.client();
        assert_eq!(untouched.status()["release_complete"], false);
        untouched.release().unwrap();
        assert_eq!(untouched.status()["release_complete"], true);
        assert_eq!(untouched.status()["release_pending"], false);
        assert!(untouched.thread.is_none());
        let mut client = harness.client();
        harness.command.fixture.as_ref().unwrap().gate.state.lock().unwrap().0 = true;
        client.submit(&server(), "revoked before PID publication", "", &GenerationOptions::default()).unwrap();
        let gate = harness.pause(&client);
        assert!(client.status()["worker_pid"].is_null());
        client.release().unwrap();
        assert_eq!(client.status()["release_pending"], true);
        assert_eq!(client.status()["release_complete"], false);
        drop(gate);
        let released = wait_status(&client, |status| status["release_complete"] == true);
        assert!(released["worker_pid"].is_null());
        assert_eq!(released["release_pending"], false);
        assert_eq!(released["release_generation"], released["released_generation"]);
        assert!(released["active_id"].is_null());
        assert_eq!(released["initializing"], false);
        client.stop().unwrap();
    }

    #[test]
    fn release_completion_waits_for_actual_native_child_and_pipe_owner_exit() {
        let mut harness = Harness::new("ignore-term");
        let mut client = harness.client();
        let id = client.submit(&local("cold-gate.gguf"), "initializing", "", &GenerationOptions::default()).unwrap();
        harness.control.stage("create");
        harness.control.stage("prepare");
        let pid = client.status()["worker_pid"].as_u64().unwrap() as u32;
        client.release().unwrap();
        assert_eq!(client.status()["release_complete"], false);
        let terminating = wait_status(&client, |status| status["terminating"] == true);
        assert_eq!(terminating["release_complete"], false);
        assert!(!terminating["worker_pid"].is_null());
        let released = wait_status(&client, |status| status["release_complete"] == true);
        assert!(released["worker_pid"].is_null());
        assert_eq!(released["terminating"], false);
        assert!(released["released_through"].as_u64().unwrap() >= id);
        assert_dead(pid);
        client.stop().unwrap();
    }

    #[test]
    fn explicit_executable_constructor_validates_and_remains_lazy() {
        let directory = tempfile::tempdir().unwrap();
        assert!(WorkerClient::for_executable(directory.path()).is_err());
        assert!(WorkerClient::for_executable(directory.path().join("missing-worker")).is_err());
        let executable = std::env::current_exe().unwrap();
        let client = WorkerClient::for_executable(&executable).unwrap();
        assert_eq!(client.command.executable, executable.canonicalize().unwrap());
        assert!(client.status()["worker_pid"].is_null());
        assert!(client.thread.is_none());
    }
}
