//! Child-only inference protocol. No authored text is ever written to diagnostics.
use crate::config::Config;
use crate::providers::{GenerationOptions, Provider, create_provider};
use anyhow::{Context, Result, bail, ensure};
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::fmt;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

pub use crate::process_client::WorkerClient;

pub const MAX_MESSAGE_BYTES: usize = 256 * 1024;
pub const MAX_RESULT_CHARS: usize = 4096;
pub(crate) const INFERENCE_ERROR: &str = "Local inference failed. Check the chosen model file, local inference installation, and local server availability.";

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct WorkerEvent {
    pub id: u64,
    #[serde(rename = "type")]
    pub kind: String,
    pub text: String,
    #[serde(rename = "final")]
    pub final_: bool,
    pub model_loaded: bool,
    pub message: String,
}

impl WorkerEvent {
    pub(crate) fn new(kind: &str, id: u64) -> Self {
        Self { kind: kind.into(), id, ..Self::default() }
    }
}

/// Read one complete frame without an unbounded `read_line` allocation. The
/// newline counts toward the limit; EOF in the middle of a frame is invalid.
pub(crate) fn read_frame(reader: &mut impl BufRead, frame: &mut Vec<u8>) -> io::Result<bool> {
    frame.clear();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return if frame.is_empty() { Ok(false) } else { Err(invalid_frame()) };
        }
        let end = available.iter().position(|&b| b == b'\n').map(|n| n + 1);
        let take = end.unwrap_or(available.len());
        if take > MAX_MESSAGE_BYTES.saturating_sub(frame.len()) {
            return Err(invalid_frame());
        }
        frame.extend_from_slice(&available[..take]);
        reader.consume(take);
        if end.is_some() { return Ok(true); }
        if frame.len() == MAX_MESSAGE_BYTES { return Err(invalid_frame()); }
    }
}

fn invalid_frame() -> io::Error { io::Error::new(io::ErrorKind::InvalidData, "Invalid private worker frame") }

/// `Value` ordinarily overwrites duplicate object keys. Reject them at every
/// depth, before configuration deserialization can erase that ambiguity.
struct UniqueValue;
impl<'de> DeserializeSeed<'de> for UniqueValue {
    type Value = Value;
    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> std::result::Result<Value, D::Error> {
        deserializer.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for UniqueValue {
    type Value = Value;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("unambiguous JSON") }
    fn visit_bool<E: de::Error>(self, v: bool) -> std::result::Result<Value, E> { Ok(Value::Bool(v)) }
    fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Value, E> { Ok(v.into()) }
    fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Value, E> { Ok(v.into()) }
    fn visit_f64<E: de::Error>(self, v: f64) -> std::result::Result<Value, E> {
        serde_json::Number::from_f64(v).map(Value::Number).ok_or_else(|| E::custom("Invalid number"))
    }
    fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Value, E> { Ok(v.into()) }
    fn visit_string<E: de::Error>(self, v: String) -> std::result::Result<Value, E> { Ok(Value::String(v)) }
    fn visit_none<E: de::Error>(self) -> std::result::Result<Value, E> { Ok(Value::Null) }
    fn visit_unit<E: de::Error>(self) -> std::result::Result<Value, E> { Ok(Value::Null) }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<Value, A::Error> {
        let mut result = Vec::new();
        while let Some(value) = seq.next_element_seed(UniqueValue)? { result.push(value); }
        Ok(Value::Array(result))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<Value, A::Error> {
        let mut result = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if result.contains_key(&key) { return Err(de::Error::custom("Duplicate protocol field")); }
            result.insert(key, map.next_value_seed(UniqueValue)?);
        }
        Ok(Value::Object(result))
    }
}

pub(crate) fn unique_json(bytes: &[u8]) -> Result<Value> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let value = UniqueValue.deserialize(&mut deserializer).context("Invalid private worker JSON")?;
    deserializer.end()?;
    Ok(value)
}

pub(crate) struct Request {
    pub id: u64,
    ticket: u64,
    pub config: Config,
    pub writing: Option<Writing>,
}
pub(crate) struct Writing {
    pub prefix: String,
    pub suffix: String,
    pub options: GenerationOptions,
}
enum Input { Request(Request), Proceed(u64), Cancel, Stop }

fn positive_id(value: &Value) -> Result<u64> {
    let id = value.as_u64().context("Invalid request identifier")?;
    ensure!(id > 0, "Invalid request identifier");
    Ok(id)
}
fn exact_fields(data: &Map<String, Value>, required: &[&str], optional: &[&str]) -> Result<()> {
    ensure!(required.iter().all(|key| data.contains_key(*key)) && data.keys().all(|key| required.contains(&key.as_str()) || optional.contains(&key.as_str())), "Invalid protocol fields");
    Ok(())
}
fn request_text(data: &Map<String, Value>, key: &str, limit: usize) -> Result<String> {
    let value = match data.get(key) { None => "", Some(value) => value.as_str().context("Invalid request text")? };
    ensure!(value.chars().take(limit + 1).count() <= limit, "Invalid request text bound");
    Ok(value.to_owned())
}

fn parse_input(bytes: &[u8]) -> Result<Input> {
    let value = unique_json(bytes)?;
    let data = value.as_object().context("Invalid protocol frame")?;
    match data.get("type").and_then(Value::as_str).context("Invalid protocol type")? {
        "prepare" | "generate" => {
            let preparing = data["type"] == "prepare";
            exact_fields(data, &["type", "id", "config"], if preparing { &[] } else { &["prefix", "suffix", "instructions", "context", "personalization", "utility", "anchor", "max_words", "alternatives"] })?;
            let id = positive_id(&data["id"])?;
            let raw = data["config"].as_object().context("Invalid worker configuration")?;
            static CONFIG_FIELDS: std::sync::LazyLock<std::collections::BTreeSet<String>> = std::sync::LazyLock::new(|| {
                let Value::Object(fields) = serde_json::to_value(Config::default()).expect("Config serializes as an object") else { unreachable!("Config object schema") };
                fields.into_iter().map(|(key, _)| key).collect()
            });
            ensure!(raw.keys().all(|key| CONFIG_FIELDS.contains(key)), "Unknown worker configuration field");
            let mut config = Config::deserialize(&data["config"])?;
            config.validate()?;
            let writing = if preparing {
                ensure!(config.backend == "gguf", "Preparation requires a local model");
                None
            } else {
                let words = match data.get("max_words") { None => 8, Some(value) => value.as_u64().context("Invalid completion length")? };
                ensure!((1..=32).contains(&words), "Invalid completion length");
                let alternatives = match data.get("alternatives") { None => false, Some(value) => value.as_bool().context("Invalid alternatives mode")? };
                Some(Writing {
                    prefix: request_text(data, "prefix", 8000)?, suffix: request_text(data, "suffix", 8000)?,
                    options: GenerationOptions {
                        instructions: request_text(data, "instructions", 26000)?,
                        context: request_text(data, "context", 8000)?,
                        personalization: request_text(data, "personalization", 2048)?,
                        utility: request_text(data, "utility", 24000)?,
                        anchor: request_text(data, "anchor", MAX_RESULT_CHARS)?,
                        max_words: words as u32, alternatives,
                    },
                })
            };
            Ok(Input::Request(Request { id, ticket: 0, config, writing }))
        }
        "proceed" => { exact_fields(data, &["type", "id"], &[])?; Ok(Input::Proceed(positive_id(&data["id"])?)) }
        "cancel" => { exact_fields(data, &["type"], &[])?; Ok(Input::Cancel) }
        "stop" => { exact_fields(data, &["type"], &[])?; Ok(Input::Stop) }
        _ => bail!("Unknown protocol message"),
    }
}

pub(crate) fn decode_event(bytes: &[u8]) -> Result<WorkerEvent> {
    let value = unique_json(bytes)?;
    let data = value.as_object().context("Invalid worker response")?;
    let kind = data.get("type").and_then(Value::as_str).context("Invalid worker response")?;
    match kind {
        "loading" => exact_fields(data, &["type", "id"], &["model_loaded"] )?,
        "prepared" | "ready" | "done" => exact_fields(data, &["type", "id"], &["model_loaded"] )?,
        "result" => exact_fields(data, &["type", "id", "text", "final"], &[] )?,
        "error" => exact_fields(data, &["type", "id", "message"], &[] )?,
        _ => bail!("Invalid worker response kind"),
    }
    positive_id(&data["id"])?;
    let mut event: WorkerEvent = serde_json::from_value(value)?;
    ensure!(event.text.chars().take(MAX_RESULT_CHARS + 1).count() <= MAX_RESULT_CHARS, "Invalid worker response bound");
    // The supervisor never propagates arbitrary backend/child diagnostics.
    if event.kind == "error" { event.message = INFERENCE_ERROR.into(); }
    Ok(event)
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum WireEvent<'a> {
    Loading { id: u64 },
    Prepared { id: u64, model_loaded: bool },
    Ready { id: u64, model_loaded: bool },
    Done { id: u64, model_loaded: bool },
    Result { id: u64, text: &'a str, #[serde(rename = "final")] final_: bool },
    Error { id: u64, message: &'a str },
}
fn encode_event(event: &WorkerEvent) -> Result<Vec<u8>> {
    let id = event.id;
    let model_loaded = event.model_loaded;
    encode_frame(&match event.kind.as_str() {
        "loading" => WireEvent::Loading { id },
        "prepared" => WireEvent::Prepared { id, model_loaded },
        "ready" => WireEvent::Ready { id, model_loaded },
        "done" => WireEvent::Done { id, model_loaded },
        "result" => WireEvent::Result { id, text: &event.text, final_: event.final_ },
        "error" => WireEvent::Error { id, message: INFERENCE_ERROR },
        _ => bail!("Invalid worker response kind"),
    })
}

pub(crate) fn encode_frame(value: &impl Serialize) -> Result<Vec<u8>> {
    // Serialization itself is capped too: highly escaped strings must not cause
    // a temporary allocation beyond the private pipe limit.
    struct Bounded(Vec<u8>);
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > (MAX_MESSAGE_BYTES - 1).saturating_sub(self.0.len()) { return Err(invalid_frame()); }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }
    let mut output = Bounded(Vec::with_capacity(1024));
    serde_json::to_writer(&mut output, value).context("The writing request exceeds the private worker message limit")?;
    output.0.push(b'\n');
    Ok(output.0)
}

struct Output<W>(Mutex<W>);
impl<W: Write> Output<W> {
    fn send(&self, event: WorkerEvent) -> Result<()> {
        let frame = encode_event(&event)?;
        let mut writer = self.0.lock().map_err(|_| anyhow::anyhow!("Worker output unavailable"))?;
        writer.write_all(&frame)?;
        writer.flush()?;
        Ok(())
    }
}

struct InputState {
    pending: Option<Request>,
    cancel: Arc<AtomicBool>,
    active_id: Option<u64>,
    awaiting_proceed: bool,
    allowed: bool,
    input_ready: u64,
    next_ticket: u64,
    stopped: bool,
    invalid: bool,
}
struct InputShared { state: Mutex<InputState>, changed: Condvar }
impl InputShared {
    fn stop(&self, invalid: bool) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.stopped = true;
        state.invalid |= invalid;
        state.pending = None;
        state.cancel.store(true, Ordering::Release);
        self.changed.notify_all();
    }
}

fn read_input<R: Read, W: Write>(reader: R, shared: &InputShared, output: &Output<W>) -> Result<()> {
    let mut reader = BufReader::with_capacity(8192, reader);
    let mut frame = Vec::with_capacity(4096);
    while read_frame(&mut reader, &mut frame)? {
        match parse_input(&frame)? {
            Input::Request(mut request) => {
                let native = request.config.backend == "gguf";
                let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                state.next_ticket = state.next_ticket.checked_add(1).context("Worker request sequence exhausted")?;
                let ticket = state.next_ticket;
                request.ticket = ticket;
                state.cancel.store(true, Ordering::Release);
                let superseded = state.pending.replace(request).map(|request| request.id);
                shared.changed.notify_all();
                if let Some(id) = superseded { output.send(WorkerEvent::new("done", id))?; }
                // Do not overlap native DLL/CRT preparation with a blocking read
                // from the process standard input (particularly on Windows).
                if native {
                    while state.input_ready != ticket && !state.stopped {
                        state = shared.changed.wait(state).unwrap_or_else(|e| e.into_inner());
                    }
                }
            }
            Input::Proceed(id) => {
                let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                if state.active_id == Some(id) && state.awaiting_proceed && !state.cancel.load(Ordering::Acquire) {
                    state.allowed = true;
                    shared.changed.notify_all();
                }
            }
            Input::Cancel => {
                let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                state.cancel.store(true, Ordering::Release);
                let superseded = state.pending.take().map(|request| request.id);
                shared.changed.notify_all();
                drop(state);
                if let Some(id) = superseded { output.send(WorkerEvent::new("done", id))?; }
            }
            Input::Stop => break,
        }
    }
    Ok(())
}

/// Run the actual protocol against owned pipes. The calling thread alone owns
/// native providers, including close on EOF, malformed input and stop. Only the
/// reader can outlive a broken output pipe; process dispatch exits immediately.
pub(crate) fn run_worker<R, W, F>(reader: R, writer: W, mut factory: F) -> Result<i32>
where R: Read + Send + 'static, W: Write + Send + 'static, F: FnMut(Config) -> Result<Box<dyn Provider>> {
    let shared = Arc::new(InputShared {
        state: Mutex::new(InputState { pending: None, cancel: Arc::new(AtomicBool::new(false)), active_id: None, awaiting_proceed: false, allowed: false, input_ready: 0, next_ticket: 0, stopped: false, invalid: false }),
        changed: Condvar::new(),
    });
    let output = Arc::new(Output(Mutex::new(writer)));
    let input_shared = Arc::clone(&shared);
    let input_output = Arc::clone(&output);
    let input_thread = thread::Builder::new().name("brotypist-worker-input".into()).spawn(move || {
        let result = read_input(reader, &input_shared, &input_output);
        input_shared.stop(result.is_err());
    })?;
    let mut provider: Option<Box<dyn Provider>> = None;
    let mut configured = None;
    let mut provider_ready = false;
    let result = (|| -> Result<i32> {
        loop {
            let (request, cancel) = {
                let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                while state.pending.is_none() && !state.stopped {
                    state = shared.changed.wait(state).unwrap_or_else(|e| e.into_inner());
                }
                if state.stopped { return Ok(if state.invalid { 2 } else { 0 }); }
                let request = state.pending.take().expect("pending request checked");
                state.active_id = Some(request.id);
                state.awaiting_proceed = false;
                state.allowed = false;
                state.cancel = Arc::new(AtomicBool::new(false));
                (request, Arc::clone(&state.cancel))
            };
            let id = request.id;
            let generated = (|| -> Result<()> {
                let identity = request.config.provider_identity();
                if configured.as_ref() != Some(&identity) {
                    provider_ready = false;
                    configured = None;
                    if let Some(mut previous) = provider.take() { previous.close()?; }
                    provider = Some(factory(request.config.clone())?);
                    configured = Some(identity);
                }
                let provider = provider.as_mut().context("Provider unavailable")?;
                provider.reconfigure(request.config.clone())?;
                if request.config.backend == "gguf" && !provider_ready {
                    output.send(WorkerEvent::new("loading", id))?;
                    provider.prepare()?;
                    provider_ready = true;
                }
                if request.writing.is_none() {
                    if !cancel.load(Ordering::Acquire) {
                        let mut event = WorkerEvent::new("prepared", id);
                        event.model_loaded = provider_ready;
                        output.send(event)?;
                    }
                    return Ok(());
                }
                if cancel.load(Ordering::Acquire) { return Ok(()); }
                {
                    let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                    let mut event = WorkerEvent::new("ready", id);
                    event.model_loaded = provider_ready;
                    output.send(event)?;
                    state.awaiting_proceed = true;
                    state.input_ready = request.ticket;
                    shared.changed.notify_all();
                    while !state.allowed && !cancel.load(Ordering::Acquire) && !state.stopped {
                        state = shared.changed.wait(state).unwrap_or_else(|e| e.into_inner());
                    }
                    state.awaiting_proceed = false;
                    if state.stopped || cancel.load(Ordering::Acquire) { return Ok(()); }
                }
                let writing = request.writing.as_ref().expect("writing checked");
                let mut text = String::new();
                provider.stream(&writing.prefix, &writing.suffix, &writing.options, &cancel, &mut |replacement| {
                    if cancel.load(Ordering::Acquire) { return Ok(()); }
                    ensure!(replacement.chars().take(MAX_RESULT_CHARS + 1).count() <= MAX_RESULT_CHARS, "Invalid model response");
                    text.clear();
                    text.push_str(replacement);
                    let mut event = WorkerEvent::new("result", id);
                    event.text.push_str(&text);
                    output.send(event)
                })?;
                if !cancel.load(Ordering::Acquire) {
                    let mut event = WorkerEvent::new("result", id);
                    event.text = text;
                    event.final_ = true;
                    output.send(event)?;
                }
                Ok(())
            })();
            if generated.is_err() && !cancel.load(Ordering::Acquire) { output.send(WorkerEvent::new("error", id))?; }
            {
                let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                state.input_ready = request.ticket;
                state.active_id = None;
                state.awaiting_proceed = false;
                shared.changed.notify_all();
            }
            let mut event = WorkerEvent::new("done", id);
            event.model_loaded = provider_ready;
            output.send(event)?;
        }
    })();
    shared.stop(false);
    // No native model owner is detached, even on malformed input or EOF.
    let close = provider.as_mut().map(|provider| provider.close()).transpose();
    if input_thread.is_finished() { let _ = input_thread.join(); }
    close?;
    result
}

/// Duplicate protocol stdout before silencing the process-wide diagnostic
/// handles. Native runtimes cannot accidentally put authored content on IPC or
/// block forever on an undrained stderr pipe.
#[cfg(unix)]
pub(crate) fn private_stdout() -> Result<std::fs::File> {
    use std::os::fd::{FromRawFd, AsRawFd};
    let null = std::fs::OpenOptions::new().write(true).open("/dev/null")?;
    let fd = unsafe { libc::dup(libc::STDOUT_FILENO) };
    ensure!(fd >= 0, "Cannot duplicate worker protocol output");
    let protocol = unsafe { std::fs::File::from_raw_fd(fd) };
    ensure!(unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } >= 0, "Cannot isolate worker protocol descriptor");
    ensure!(unsafe { libc::dup2(null.as_raw_fd(), libc::STDOUT_FILENO) } >= 0 && unsafe { libc::dup2(null.as_raw_fd(), libc::STDERR_FILENO) } >= 0, "Cannot isolate worker diagnostics");
    Ok(protocol)
}

#[cfg(windows)]
fn private_stdout() -> Result<std::fs::File> {
    use std::ffi::c_void;
    use std::os::windows::io::FromRawHandle;
    // CRT descriptors and Win32 handles are independent; silence both so C/C++
    // runtimes and Rust/native logging share the same content-free sink.
    unsafe extern "C" {
        fn _dup(fd: i32) -> i32;
        fn _dup2(source: i32, target: i32) -> i32;
        fn _get_osfhandle(fd: i32) -> isize;
        fn _open(path: *const i8, flags: i32, ...) -> i32;
        fn _close(fd: i32) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn DuplicateHandle(source_process: *mut c_void, source: *mut c_void, target_process: *mut c_void, target: *mut *mut c_void, access: u32, inherit: i32, options: u32) -> i32;
        fn SetStdHandle(kind: u32, handle: *mut c_void) -> i32;
    }
    let fd = unsafe { _dup(1) };
    ensure!(fd >= 0, "Cannot duplicate worker protocol output");
    let mut handle = std::ptr::null_mut();
    let process = unsafe { GetCurrentProcess() };
    let duplicated = unsafe { DuplicateHandle(process, _get_osfhandle(fd) as *mut c_void, process, &mut handle, 0, 0, 2) };
    unsafe { _close(fd); }
    ensure!(duplicated != 0, "Cannot isolate worker protocol handle");
    let protocol = unsafe { std::fs::File::from_raw_handle(handle) };
    let null = unsafe { _open(c"NUL".as_ptr(), 0x0001 | 0x8000) };
    ensure!(null >= 0, "Cannot open worker diagnostic sink");
    let redirected = unsafe { _dup2(null, 1) == 0 && _dup2(null, 2) == 0 && SetStdHandle(-11i32 as u32, _get_osfhandle(1) as *mut c_void) != 0 && SetStdHandle(-12i32 as u32, _get_osfhandle(2) as *mut c_void) != 0 };
    unsafe { _close(null); }
    ensure!(redirected, "Cannot isolate worker diagnostics");
    Ok(protocol)
}

pub fn worker_main() -> Result<i32> {
    let protocol = private_stdout()?;
    run_worker(io::stdin(), protocol, create_provider)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn generate() -> Value { serde_json::json!({"type":"generate", "id":1, "config":{}}) }
    fn valid(value: &Value) -> bool { parse_input(&encode_frame(value).unwrap()).is_ok() }

    #[test]
    fn framing_counts_newline_and_rejects_incomplete_or_unbounded_input() {
        let mut frame = Vec::new();
        let mut exact = vec![b' '; MAX_MESSAGE_BYTES - 1];
        exact.push(b'\n');
        assert!(read_frame(&mut Cursor::new(&exact), &mut frame).unwrap());
        assert_eq!(frame.len(), MAX_MESSAGE_BYTES);
        assert!(frame.capacity() <= MAX_MESSAGE_BYTES);
        exact.insert(0, b' ');
        assert!(read_frame(&mut Cursor::new(exact), &mut frame).is_err());
        assert!(read_frame(&mut Cursor::new(b"{}"), &mut frame).is_err());
        assert!(!read_frame(&mut Cursor::new(b""), &mut frame).unwrap());
        let mut two = Cursor::new(b"{}\n[]\n");
        assert!(read_frame(&mut two, &mut frame).unwrap());
        assert_eq!(frame, b"{}\n");
        assert!(read_frame(&mut two, &mut frame).unwrap());
        assert_eq!(frame, b"[]\n");
    }

    #[test]
    fn every_scalar_bound_and_exact_field_type_is_enforced() {
        for (key, limit) in [("prefix",8000), ("suffix",8000), ("instructions",26000), ("context",8000), ("personalization",2048), ("utility",24000), ("anchor",4096)] {
            let mut value = generate();
            value[key] = "🦀".repeat(limit).into();
            assert!(valid(&value), "boundary rejected: {key}");
            value[key] = "🦀".repeat(limit + 1).into();
            assert!(!valid(&value), "overflow accepted: {key}");
            for wrong in [Value::Null, Value::Bool(false), Value::from(1)] {
                value[key] = wrong;
                assert!(!valid(&value), "nontext accepted: {key}");
            }
        }
        for id in [Value::from(0), Value::from(-1), Value::from(1.0), Value::Bool(true), Value::String("1".into()), Value::Null] {
            let mut value = generate(); value["id"] = id;
            assert!(!valid(&value));
        }
        for words in [Value::from(0), Value::from(33), Value::from(8.0), Value::Bool(true), Value::String("8".into())] {
            let mut value = generate(); value["max_words"] = words;
            assert!(!valid(&value));
        }
        for alternatives in [Value::from(0), Value::String("false".into()), Value::Null] {
            let mut value = generate(); value["alternatives"] = alternatives;
            assert!(!valid(&value));
        }
        assert!(valid(&generate()));
        for unknown in [
            serde_json::json!({"type":"generate","id":1,"config":{},"extra":true}),
            serde_json::json!({"type":"generate","id":1,"config":{"unknown":true}}),
            serde_json::json!({"type":"generate","id":1,"config":{"max_tokens":true}}),
            serde_json::json!({"type":"generate","id":1,"config":{"enabled":1}}),
            serde_json::json!({"type":"proceed","id":1,"extra":false}),
            serde_json::json!({"type":"cancel","id":1}),
            serde_json::json!({"type":"stop","extra":null}),
        ] { assert!(!valid(&unknown)); }
    }

    #[test]
    fn preparation_is_configuration_only_and_json_has_no_duplicate_keys() {
        assert!(valid(&serde_json::json!({"type":"prepare","id":1,"config":{}})));
        assert!(!valid(&serde_json::json!({"type":"prepare","id":1,"config":{},"prefix":""})));
        assert!(!valid(&serde_json::json!({"type":"prepare","id":1,"config":{"backend":"openai"}})));
        for bytes in [
            &b"{\"type\":\"generate\",\"id\":1,\"id\":2,\"config\":{}}\n"[..],
            &b"{\"type\":\"generate\",\"id\":1,\"config\":{\"max_tokens\":1,\"max_tokens\":2}}\n"[..],
            &b"[]\n"[..], &b"null\n"[..], &b"{\"type\":\"stop\"}{}\n"[..],
        ] { assert!(parse_input(bytes).is_err()); }
    }

    #[test]
    fn response_schema_rejects_malformed_types_and_bounds_and_redacts_errors() {
        for value in [
            serde_json::json!({"type":"result","id":1,"text":"🦀".repeat(4097),"final":true}),
            serde_json::json!({"type":"result","id":true,"text":"","final":true}),
            serde_json::json!({"type":"result","id":1,"text":"","final":0}),
            serde_json::json!({"type":"ready","id":1,"model_loaded":1}),
            serde_json::json!({"type":"exit","id":1}),
            serde_json::json!({"type":"error","id":1,"message":42}),
            serde_json::json!({"type":"done","id":1,"text":"private"}),
        ] { assert!(decode_event(&encode_frame(&value).unwrap()).is_err()); }
        let error = decode_event(b"{\"type\":\"error\",\"id\":1,\"message\":\"private authored text\"}\n").unwrap();
        assert_eq!(error.message, INFERENCE_ERROR);
        assert_eq!(decode_event(&encode_frame(&serde_json::json!({"type":"result","id":1,"text":"🦀".repeat(4096),"final":true})).unwrap()).unwrap().text.chars().count(), 4096);
    }

    #[test]
    fn encoded_bound_includes_escaping_and_does_not_allocate_the_entire_payload() {
        let exact = "x".repeat(MAX_MESSAGE_BYTES - 3);
        assert_eq!(encode_frame(&exact).unwrap().len(), MAX_MESSAGE_BYTES);
        assert!(encode_frame(&format!("{exact}x")).is_err());
        assert!(encode_frame(&"\0".repeat(MAX_MESSAGE_BYTES / 6 + 1)).is_err());
    }
}
