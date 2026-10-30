//! Explicit desktop actions and ephemeral, consent-fenced native context.
use crate::{core::Snapshot, platform::{ContextRead, NativeBridge}};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{ffi::{OsStr, OsString}, io::{Read, Write}, path::{Path, PathBuf}, process::{Command, Stdio}, sync::{Arc, LazyLock, Mutex, Weak, atomic::{AtomicBool, Ordering}, mpsc}, thread::{self, ThreadId}, time::{Duration, Instant}};

const AUTOSTART_ID: &str = "com.inklow.Inklow";
const INPUT_METHOD_ID: &str = "com.inklow.Inklow.InputMethod";
const LIMIT: usize = 4000;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PowerStatus {
    pub available: bool,
    pub has_battery: bool,
    pub on_battery: bool,
    pub charging: Option<bool>,
    pub percent: Option<u32>,
}
/// Never performs native sampling on the caller. Hold power::PowerSampler for app lifetime.
pub fn power_status() -> PowerStatus { crate::power::cached_status() }
pub fn on_battery() -> bool { power_status().on_battery }

fn isolated() -> bool { std::env::var_os("INKLOW_HOME").is_some_and(|p| !p.is_empty()) }
fn require_normal_profile(isolated: bool) -> Result<()> {
    if isolated { bail!("Desktop integration cannot be changed from an isolated profile. Open the normal installed BroTypist app."); }
    Ok(())
}
fn launch_arguments() -> Result<Vec<OsString>> { Ok(vec![std::env::current_exe()?.into_os_string(), "--background".into()]) }
fn desktop_command(arguments: &[OsString]) -> Result<String> {
    arguments.iter().map(|argument| {
        let argument = argument.to_str().context("Desktop launcher path must be UTF-8")?;
        if argument.contains(['\n', '\r', '\0']) { bail!("Desktop launcher arguments cannot contain line breaks or NULs."); }
        Ok(format!("\"{}\"", argument.replace('\\', "\\\\").replace('"', "\\\"").replace('`', "\\`").replace('$', "\\$").replace('%', "%%")))
    }).collect::<Result<Vec<_>>>().map(|parts| parts.join(" "))
}
fn xdg(name: &str, fallback: &str) -> Result<PathBuf> {
    Ok(std::env::var_os(name).filter(|p| !p.is_empty()).map(PathBuf::from).unwrap_or(crate::paths::home_dir()?.join(fallback)))
}
fn autostart_path() -> Result<PathBuf> {
    if cfg!(target_os = "macos") { Ok(crate::paths::home_dir()?.join("Library/LaunchAgents").join(format!("{AUTOSTART_ID}.plist"))) }
    else { Ok(xdg("XDG_CONFIG_HOME", ".config")?.join("autostart").join(format!("{AUTOSTART_ID}.desktop"))) }
}
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("Integration file has no directory")?;
    crate::paths::private_directory(parent)?;
    let mut temporary = tempfile::Builder::new().prefix(".inklow-").tempfile_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}
fn owned_desktop(content: &str) -> bool {
    let mut in_desktop = false;
    content.lines().any(|line| {
        let line = line.trim();
        if line.starts_with('[') { in_desktop = line == "[Desktop Entry]"; }
        in_desktop && line == "X-Inklow-Owned=true"
    })
}
fn file_login_state(path: &Path, mac: bool) -> Result<bool> {
    if !path.is_file() { return Ok(false); }
    if mac {
        let Ok(value) = plist::Value::from_file(path) else { return Ok(false); };
        let Some(data) = value.as_dictionary() else { return Ok(false); };
        Ok(data.get("Label").and_then(plist::Value::as_string) == Some(AUTOSTART_ID) && data.get("RunAtLoad").and_then(plist::Value::as_boolean) == Some(true))
    } else { Ok(owned_desktop(&std::fs::read_to_string(path)?)) }
}
pub fn launch_at_login_state() -> Result<bool> {
    if isolated() { return Ok(false); }
    #[cfg(windows)] { return windows::login_state(); }
    #[cfg(not(windows))] { file_login_state(&autostart_path()?, cfg!(target_os = "macos")) }
}
pub fn set_launch_at_login(enabled: bool) -> Result<bool> {
    require_normal_profile(isolated())?;
    #[cfg(windows)] { return windows::set_login(enabled, &launch_arguments()?); }
    #[cfg(not(windows))] {
        let arguments = launch_arguments()?;
        #[cfg(target_os = "linux")]
        let arguments = {
            let mut arguments = arguments;
            arguments.push("--login-startup".into());
            arguments
        };
        write_login(&autostart_path()?, cfg!(target_os = "macos"), enabled, &arguments)?;
        Ok(true)
    }
}
fn write_login(path: &Path, mac: bool, enabled: bool, arguments: &[OsString]) -> Result<()> {
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() || !file_login_state(path, mac)? { bail!("A different or invalid startup entry already exists at {}. It was not overwritten.", path.display()); }
    }
    if !enabled {
        match std::fs::remove_file(path) { Ok(()) => (), Err(error) if error.kind() == std::io::ErrorKind::NotFound => (), Err(error) => return Err(error.into()) }
        return Ok(());
    }
    if mac {
        let mut data = plist::Dictionary::new();
        data.insert("Label".into(), AUTOSTART_ID.into());
        data.insert("ProgramArguments".into(), plist::Value::Array(arguments.iter().map(|arg| arg.to_str().map(|s| plist::Value::String(s.into())).context("Launch path must be UTF-8")).collect::<Result<_>>()?));
        data.insert("RunAtLoad".into(), true.into());
        data.insert("KeepAlive".into(), false.into());
        data.insert("ProcessType".into(), "Interactive".into());
        data.insert("LimitLoadToSessionType".into(), "Aqua".into());
        let mut bytes = Vec::new();
        plist::Value::Dictionary(data).to_writer_xml(&mut bytes)?;
        atomic_write(path, &bytes)
    } else {
        atomic_write(path, format!("[Desktop Entry]\nType=Application\nName=BroTypist\nExec={}\nTerminal=false\nX-Inklow-Owned=true\n", desktop_command(arguments)?).as_bytes())
    }
}

pub fn install_wayland_input_method() -> Result<PathBuf> {
    if !cfg!(target_os = "linux") { bail!("KDE Wayland input-method setup is available on Linux only."); }
    require_normal_profile(isolated())?;
    // The Rust executable owns the native compositor client; no shared Python client is required.
    let executable = std::env::current_exe()?;
    if !executable.is_file() { bail!("The native BroTypist executable is missing."); }
    let path = xdg("XDG_DATA_HOME", ".local/share")?.join("applications").join(format!("{INPUT_METHOD_ID}.desktop"));
    let command = desktop_command(&[executable.into_os_string(), "--background".into(), "--wayland-input-method".into()])?;
    write_wayland_entry(&path, &command)?;
    Ok(path)
}
fn write_wayland_entry(path: &Path, command: &str) -> Result<()> {
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() || !metadata.is_file() || !owned_desktop(&std::fs::read_to_string(path)?) { bail!("A different input-method entry already exists at {}; it was not overwritten.", path.display()); }
    }
    let content = format!("[Desktop Entry]\nType=Application\nName=BroTypist\nGenericName=Predictive typing input method\nComment=Manual controls for supported KDE Wayland text inputs; ordinary inline completion unavailable\nExec={command}\nIcon=com.inklow.Inklow\nTerminal=false\nNoDisplay=true\nX-KDE-Wayland-VirtualKeyboard=true\nX-Inklow-Owned=true\n");
    atomic_write(path, content.as_bytes())
}
pub fn wayland_input_method_selected() -> bool {
    if isolated() || !cfg!(target_os = "linux") || !(std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty()) || std::env::var("XDG_SESSION_TYPE").is_ok_and(|v| v.eq_ignore_ascii_case("wayland"))) { return false; }
    let Ok(output) = bounded_output(Command::new("kreadconfig6").args(["--file", "kwinrc", "--group", "Wayland", "--key", "InputMethod"]), Duration::from_secs(3), 16384) else { return false; };
    let Ok(selected) = std::str::from_utf8(&output) else { return false; };
    let selected = Path::new(selected.trim());
    if selected.file_name() != Some(OsStr::new(&format!("{INPUT_METHOD_ID}.desktop"))) { return false; }
    if selected.is_absolute() { return selected.is_file(); }
    let Ok(data) = xdg("XDG_DATA_HOME", ".local/share") else { return false; };
    std::iter::once(data).chain(std::env::split_paths(&std::env::var_os("XDG_DATA_DIRS").unwrap_or_else(|| "/usr/local/share:/usr/share".into())))
        .filter(|root| !root.as_os_str().is_empty()).any(|root| root.join("applications").join(selected).is_file())
}
pub fn open_wayland_settings() -> Result<()> {
    if !cfg!(target_os = "linux") { bail!("KDE Wayland settings are available on Linux only."); }
    spawn_opener(Command::new("kcmshell6").arg("kcm_virtualkeyboard")).context("Open KDE System Settings → Keyboard → Virtual Keyboard and choose BroTypist.")
}
fn spawn_opener(command: &mut Command) -> Result<()> {
    let child = command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    // Keep ownership recoverable if creating the reaper thread fails.
    let owner = Arc::new(Mutex::new(Some(child)));
    let reaper = owner.clone();
    if let Err(error) = thread::Builder::new().name("brotypist-os-opener".into()).spawn(move || {
        if let Some(mut child) = reaper.lock().unwrap_or_else(|p| p.into_inner()).take() { let _ = child.wait(); }
    }) {
        if let Some(mut child) = owner.lock().unwrap_or_else(|p| p.into_inner()).take() { let _ = child.kill(); let _ = child.wait(); }
        return Err(error.into());
    }
    Ok(())
}
fn open_target(target: &OsStr) -> Result<()> {
    #[cfg(target_os = "macos")] { bounded_output(Command::new("/usr/bin/open").arg(target), Duration::from_secs(15), 16384)?; Ok(()) }
    #[cfg(windows)] { windows::open(target) }
    #[cfg(not(any(target_os = "macos", windows)))] { spawn_opener(Command::new("xdg-open").arg(target)) }
}
pub fn open_permissions(kind: &str) -> Result<()> {
    #[cfg(target_os = "macos")] {
        let target = match kind {
            "accessibility" => "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
            "input_monitoring" => "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent",
            "screen_recording" => "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture",
            "keyboard" => "x-apple.systempreferences:com.apple.preference.keyboard",
            _ => bail!("Unknown permission settings page."),
        };
        open_target(OsStr::new(target))
    }
    #[cfg(windows)] { let _ = kind; open_target(OsStr::new("ms-settings:privacy")) }
    #[cfg(not(any(target_os = "macos", windows)))] { let _ = kind; bail!("Enable desktop accessibility. X11 uses AT-SPI keyboard access; KDE Wayland requires explicitly selecting BroTypist as the native input method. OS access is never granted automatically.") }
}
pub fn reveal_models() -> Result<()> { let path = crate::paths::model_dir()?; crate::paths::private_directory(&path)?; open_target(path.as_os_str()) }

/// Spawn one bounded read process. Own/reap it on every exit, including timeout/oversize.
fn bounded_output(command: &mut Command, timeout: Duration, limit: usize) -> Result<Vec<u8>> {
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(unix)] { use std::os::unix::process::CommandExt; command.process_group(0); }
    let mut child = command.spawn()?;
    let mut stdout = child.stdout.take().context("Native reader stdout unavailable")?;
    let (send, receive) = mpsc::sync_channel(1);
    let reader = thread::Builder::new().name("brotypist-os-read".into()).spawn(move || {
        let mut bytes = Vec::new();
        let result = (&mut stdout).take(limit as u64 + 1).read_to_end(&mut bytes).map(|_| bytes);
        let _ = send.send(result);
    });
    let reader = match reader { Ok(reader) => reader, Err(error) => { let _ = child.kill(); let _ = child.wait(); return Err(error.into()); } };
    let deadline = Instant::now() + timeout;
    let mut output = None;
    let result = loop {
        if output.is_none() { match receive.try_recv() { Ok(result) => output = Some(result), Err(mpsc::TryRecvError::Empty) => (), Err(_) => break Err(anyhow::anyhow!("Native reader failed")) } }
        if output.as_ref().is_some_and(|result| result.as_ref().is_ok_and(|bytes| bytes.len() > limit)) { break Err(anyhow::anyhow!("Native reader exceeded its output limit")); }
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() { break Err(anyhow::anyhow!("Native reader failed")); }
                if let Some(result) = output.take() { break result.map_err(Into::into); }
            }
            Ok(None) => (), Err(error) => break Err(error.into()),
        }
        if Instant::now() >= deadline { break Err(anyhow::anyhow!("Native reader timed out")); }
        thread::sleep(Duration::from_millis(5));
    };
    if result.is_err() {
        #[cfg(unix)] unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL); }
        let _ = child.kill();
    }
    let _ = child.wait();
    let _ = reader.join();
    result
}

fn bounded_text(value: &str, limit: usize) -> String { value.chars().take(limit).collect() }
fn clipboard_read(authorized: &dyn Fn() -> bool, read: impl FnOnce() -> Result<String>) -> Result<String> {
    if !authorized() { return Ok(String::new()); }
    let result = read().map_err(|_| anyhow::anyhow!("Clipboard context could not be read."));
    if !authorized() { return Ok(String::new()); }
    result.map(|text| bounded_text(&text, LIMIT))
}
/// Native APIs do not require a toolkit thread. A GUI-owned reader may instead use ClipboardService.
pub fn clipboard_text(authorized: &dyn Fn() -> bool) -> Result<String> { clipboard_read(authorized, native_clipboard) }
fn native_clipboard() -> Result<String> {
    #[cfg(target_os = "macos")] {
        use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
        objc2::rc::autoreleasepool(|_| Ok(NSPasteboard::generalPasteboard().stringForType(unsafe { NSPasteboardTypeString }).map(|value| value.to_string()).unwrap_or_default()))
    }
    #[cfg(windows)] { windows::clipboard() }
    #[cfg(not(any(target_os = "macos", windows)))] {
        let bytes = if std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty()) {
            bounded_output(Command::new("wl-paste").args(["--no-newline", "--type", "text"]), Duration::from_millis(250), 1024 * 1024)?
        } else { bounded_output(Command::new("xclip").args(["-selection", "clipboard", "-out"]), Duration::from_millis(250), 1024 * 1024)? };
        Ok(String::from_utf8(bytes)?)
    }
}

pub type ClipboardAuthorization = Arc<dyn Fn() -> bool + Send + Sync>;
struct ClipboardRequest { authorized: ClipboardAuthorization, deadline: Instant, cancelled: AtomicBool, result: Mutex<Option<Result<String>>> }
struct ClipboardQueue { owner: ThreadId, pending: Mutex<Option<Arc<ClipboardRequest>>>, stopped: AtomicBool }
static CLIPBOARD: LazyLock<Mutex<Weak<ClipboardQueue>>> = LazyLock::new(|| Mutex::new(Weak::new()));
/// Optional GUI owner for portable shells. One queued operation maximum; expired requests never read late.
pub struct ClipboardService { queue: Arc<ClipboardQueue>, _owner_only: std::marker::PhantomData<std::rc::Rc<()>> }
impl ClipboardService {
    pub fn install() -> Result<Self> {
        let mut slot = CLIPBOARD.lock().unwrap_or_else(|p| p.into_inner());
        if slot.upgrade().is_some() { bail!("Clipboard service already has an owner"); }
        let queue = Arc::new(ClipboardQueue { owner: thread::current().id(), pending: Mutex::new(None), stopped: AtomicBool::new(false) });
        *slot = Arc::downgrade(&queue);
        Ok(Self { queue, _owner_only: std::marker::PhantomData })
    }
    pub fn poll(&self) -> Result<()> { self.poll_with(native_clipboard) }
    fn poll_with(&self, read: impl FnOnce() -> Result<String>) -> Result<()> {
        if self.queue.owner != thread::current().id() { bail!("Clipboard context requires the application thread."); }
        let request = self.queue.pending.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(request) = request {
            let live = || !request.cancelled.load(Ordering::Acquire) && !self.queue.stopped.load(Ordering::Acquire) && Instant::now() < request.deadline && (request.authorized)();
            let result = clipboard_read(&live, read);
            *request.result.lock().unwrap_or_else(|p| p.into_inner()) = Some(result);
        }
        Ok(())
    }
}
impl Drop for ClipboardService {
    fn drop(&mut self) {
        self.queue.stopped.store(true, Ordering::Release);
        if let Some(request) = self.queue.pending.lock().unwrap_or_else(|p| p.into_inner()).take() { request.cancelled.store(true, Ordering::Release); }
    }
}
/// Marshal when a GUI service is installed; otherwise use native thread-independent access.
pub fn clipboard_text_shared(authorized: ClipboardAuthorization) -> Result<String> {
    if !authorized() { return Ok(String::new()); }
    let queue = CLIPBOARD.lock().unwrap_or_else(|p| p.into_inner()).upgrade();
    let Some(queue) = queue else { return clipboard_text(&*authorized); };
    if queue.owner == thread::current().id() { return clipboard_text(&*authorized); }
    clipboard_from_queue(queue, authorized, Duration::from_millis(250))
}
fn clipboard_from_queue(queue: Arc<ClipboardQueue>, authorized: ClipboardAuthorization, timeout: Duration) -> Result<String> {
    let request = Arc::new(ClipboardRequest { authorized: authorized.clone(), deadline: Instant::now() + timeout, cancelled: AtomicBool::new(false), result: Mutex::new(None) });
    {
        let mut pending = queue.pending.lock().unwrap_or_else(|p| p.into_inner());
        if pending.as_ref().is_some_and(|r| !r.cancelled.load(Ordering::Acquire) && r.deadline > Instant::now()) { return Ok(String::new()); }
        *pending = Some(request.clone());
    }
    loop {
        if !authorized() || queue.stopped.load(Ordering::Acquire) || Instant::now() >= request.deadline { request.cancelled.store(true, Ordering::Release); return Ok(String::new()); }
        if let Some(result) = request.result.lock().unwrap_or_else(|p| p.into_inner()).take() { return if authorized() { result } else { Ok(String::new()) }; }
        thread::sleep(Duration::from_millis(2));
    }
}

fn context(status: &str, source: &str) -> ContextRead { ContextRead { text: String::new(), status: status.into(), source: source.into() } }
trait WindowContext {
    fn context_matches(&mut self, snapshot: &Snapshot) -> Result<bool>;
    fn focused_context(&mut self, snapshot: &Snapshot, authorized: &dyn Fn() -> bool, limit: usize) -> Result<ContextRead>;
}
struct BridgeContext<'a>(&'a mut dyn NativeBridge);
impl WindowContext for BridgeContext<'_> {
    fn context_matches(&mut self, snapshot: &Snapshot) -> Result<bool> { self.0.context_matches(snapshot) }
    fn focused_context(&mut self, snapshot: &Snapshot, authorized: &dyn Fn() -> bool, limit: usize) -> Result<ContextRead> { self.0.focused_context(snapshot, authorized, limit) }
}
struct DetachedContext<'a>(&'a dyn crate::platform::DetachedContextRead);
impl WindowContext for DetachedContext<'_> {
    fn context_matches(&mut self, snapshot: &Snapshot) -> Result<bool> { Ok(self.0.matches_snapshot(snapshot) && self.0.matches()?) }
    fn focused_context(&mut self, snapshot: &Snapshot, authorized: &dyn Fn() -> bool, limit: usize) -> Result<ContextRead> {
        if !self.0.matches_snapshot(snapshot) { return Ok(context("stale", "")); }
        self.0.read(authorized, limit)
    }
}
/// Worker-only optional context. Ticket contains plain metadata; native handles are reacquired locally.
pub fn screen_context_detached(snapshot: &Snapshot, ticket: &dyn crate::platform::DetachedContextRead, authorized: &dyn Fn() -> bool, max_chars: usize, accessibility: bool, screenshot: bool) -> Result<ContextRead> {
    screen_context_with(snapshot, &mut DetachedContext(ticket), authorized, max_chars, accessibility, screenshot, &mut |snapshot, authorized, current, limit| {
        #[cfg(target_os = "macos")] { crate::platform::macos::vision::recognize_window_text(snapshot, authorized, current, limit) }
        #[cfg(not(target_os = "macos"))] { let _ = (snapshot, authorized, current, limit); Ok(context("unavailable", "screenshot")) }
    })
}
pub fn screen_context(snapshot: &Snapshot, bridge: &mut dyn NativeBridge, authorized: &dyn Fn() -> bool, max_chars: usize, accessibility: bool, screenshot: bool) -> Result<ContextRead> {
    screen_context_with(snapshot, &mut BridgeContext(bridge), authorized, max_chars, accessibility, screenshot, &mut |snapshot, authorized, current, limit| {
        #[cfg(target_os = "macos")] { crate::platform::macos::vision::recognize_window_text(snapshot, authorized, current, limit) }
        #[cfg(not(target_os = "macos"))] { let _ = (snapshot, authorized, current, limit); Ok(context("unavailable", "screenshot")) }
    })
}
fn screen_context_with(snapshot: &Snapshot, bridge: &mut dyn WindowContext, authorized: &dyn Fn() -> bool, max_chars: usize, accessibility: bool, screenshot: bool,
    ocr: &mut dyn FnMut(&Snapshot, &dyn Fn() -> bool, &mut dyn FnMut() -> Result<bool>, usize) -> Result<ContextRead>) -> Result<ContextRead> {
    let terminal = matches!(snapshot.app_id.as_str(), "com.apple.Terminal" | "com.googlecode.iterm2") && snapshot.field_id.starts_with("terminal:");
    if terminal || !screenshot || !authorized() || snapshot.protected { clear_context_cache(); }
    if terminal || snapshot.protected || !(accessibility || screenshot) || snapshot.window_id.is_empty() || snapshot.window_revision.is_empty() || !authorized() { return Ok(context("blocked", "")); }
    let limit = max_chars.min(LIMIT);
    let mut status = "unavailable".to_owned();
    if !bridge.context_matches(snapshot)? { return Ok(context("stale", "")); }
    if !authorized() { return Ok(context("blocked", "")); }
    if accessibility {
        let result = bridge.focused_context(snapshot, authorized, limit)?;
        if !authorized() { return Ok(context("blocked", "")); }
        // Blocked means privacy refusal; never perform another sensitive read or OCR.
        if result.status == "blocked" { return Ok(context("blocked", "")); }
        if !bridge.context_matches(snapshot)? { return Ok(context("stale", "")); }
        if !authorized() { return Ok(context("blocked", "")); }
        if result.status == "ready" && !result.text.trim().is_empty() {
            return Ok(ContextRead { text: bounded_text(&result.text, limit), status: "ready".into(), source: "accessibility".into() });
        }
        status = match result.status.as_str() { "empty" | "unavailable" | "error" | "stale" => result.status, _ => "empty".into() };
        if status == "stale" { return Ok(context("stale", "")); }
    }
    if !screenshot { return Ok(context(&status, "")); }
    if !authorized() { return Ok(context("blocked", "")); }
    if !bridge.context_matches(snapshot)? { return Ok(context("stale", "")); }
    let result = ocr(snapshot, authorized, &mut || bridge.context_matches(snapshot), limit)?;
    if !authorized() { clear_context_cache(); return Ok(context("blocked", "")); }
    if !bridge.context_matches(snapshot)? { return Ok(context("stale", "")); }
    if !authorized() { clear_context_cache(); return Ok(context("blocked", "")); }
    if result.status == "ready" && !result.text.trim().is_empty() { return Ok(ContextRead { text: bounded_text(&result.text, limit), status: "ready".into(), source: "screenshot".into() }); }
    Ok(context(match result.status.as_str() { "empty" | "blocked" | "stale" | "unavailable" | "error" | "screen_permission" => &result.status, _ => "error" }, ""))
}
fn clear_context_cache() { #[cfg(target_os = "macos")] crate::platform::macos::vision::clear_text_cache(); }
pub fn screen_capture_granted() -> bool {
    #[cfg(target_os = "macos")] { crate::platform::macos::vision::capture_granted() }
    #[cfg(not(target_os = "macos"))] { false }
}
pub fn request_screen_capture_permission() -> Result<bool> {
    #[cfg(target_os = "macos")] { crate::platform::macos::vision::request_capture_permission() }
    #[cfg(not(target_os = "macos"))] { Ok(false) }
}
pub fn macos_inline_prediction_enabled() -> Option<bool> {
    #[cfg(target_os = "macos")] {
        use objc2_foundation::{NSString, NSUserDefaults, NSProcessInfo};
        objc2::rc::autoreleasepool(|_| {
            let defaults = NSUserDefaults::standardUserDefaults();
            let domain = defaults.persistentDomainForName(&NSString::from_str("NSGlobalDomain"));
            if let Some(value) = domain.and_then(|domain| domain.objectForKey(&NSString::from_str("NSAutomaticInlinePredictionEnabled"))) {
                let result: bool = unsafe { objc2::msg_send![&*value, boolValue] };
                return Some(result);
            }
            (NSProcessInfo::processInfo().operatingSystemVersion().majorVersion >= 14).then_some(true)
        })
    }
    #[cfg(not(target_os = "macos"))] { None }
}

pub(crate) fn sample_power_status() -> Result<PowerStatus> {
    #[cfg(target_os = "macos")] { Ok(parse_mac_power(&String::from_utf8(bounded_output(Command::new("/usr/bin/pmset").args(["-g", "batt"]), Duration::from_secs(3), 65536)?)?)) }
    #[cfg(windows)] { windows::power() }
    #[cfg(not(any(target_os = "macos", windows)))] { linux_power(Path::new("/sys/class/power_supply")) }
}
pub fn parse_mac_power(output: &str) -> PowerStatus {
    let mut reading = PowerStatus::default();
    let Some((source_index, source)) = output.lines().enumerate().find(|(_, line)| matches!(line.trim_end(), "Now drawing from 'Battery Power'" | "Now drawing from 'AC Power'")) else { return reading; };
    reading.on_battery = source.trim_end() == "Now drawing from 'Battery Power'";
    let battery_pattern = LazyLock::force(&MAC_BATTERY);
    let batteries: Vec<_> = output.lines().filter(|line| battery_pattern.is_match(line)).collect();
    reading.has_battery = !batteries.is_empty() || reading.on_battery;
    if batteries.is_empty() {
        let remainder = output.lines().enumerate().filter(|(i, _)| *i != source_index).map(|(_, line)| line).collect::<Vec<_>>().join("\n");
        reading.available = !reading.on_battery && matches!(remainder.trim(), "" | "No batteries.");
        return reading;
    }
    if batteries.len() != 1 { return reading; }
    let Some(record) = MAC_RECORD.captures(batteries[0]) else { return reading; };
    let payload = &record[1];
    if MAC_ABSENT.is_match(payload) { reading.has_battery = false; reading.available = !reading.on_battery; return reading; }
    let fields: Vec<_> = payload.split(';').map(str::trim).collect();
    if fields.len() < 2 { return reading; }
    reading.percent = fields[0].strip_suffix('%').filter(|s| !s.is_empty() && s.len() <= 3 && s.bytes().all(|c| c.is_ascii_digit())).and_then(|s| s.parse::<u32>().ok()).filter(|n| *n <= 100);
    let states: Vec<_> = fields[1..].iter().map(|s| MAC_PRESENT.replace(s, "").trim().to_owned()).collect();
    if states.iter().any(|state| matches!(state.as_str(), "charging" | "finishing charge")) {
        if reading.on_battery { return reading; }
        reading.charging = Some(true);
    } else if states.iter().any(|state| matches!(state.as_str(), "discharging" | "charged" | "not charging")) { reading.charging = Some(false); }
    reading.available = true;
    reading
}
static MAC_BATTERY: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"^\s*-InternalBattery-\d+\b").unwrap());
static MAC_RECORD: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"^\s*-InternalBattery-\d+\s+\(id=\d+\)\s+(.+)$").unwrap());
static MAC_ABSENT: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"\bpresent:\s*false\b").unwrap());
static MAC_PRESENT: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"\s+present:\s*(?:true|false)\s*$").unwrap());
#[cfg(any(windows, test))]
fn windows_power_fields(ac: u8, flags: u8, percentage: u8) -> PowerStatus {
    let mut reading = PowerStatus { available: matches!(ac, 0 | 1), on_battery: ac == 0, has_battery: flags != 255 && flags & 128 == 0, ..PowerStatus::default() };
    if reading.has_battery { reading.charging = Some(flags & 8 != 0); reading.percent = (percentage <= 100).then_some(percentage as u32); }
    reading
}
#[cfg(any(target_os = "linux", test))]
fn linux_power(root: &Path) -> Result<PowerStatus> {
    let mut reading = PowerStatus::default();
    if !root.exists() { return Ok(reading); }
    let mut batteries = Vec::new();
    let mut incomplete = false;
    for entry in std::fs::read_dir(root)? {
        let path = match entry { Ok(entry) => entry.path(), Err(_) => { incomplete = true; continue; } };
        let kind = match std::fs::read_to_string(path.join("type")) { Ok(kind) => kind, Err(_) => { incomplete = true; continue; } };
        if kind.trim() != "Battery" { continue; }
        reading.has_battery = true;
        match std::fs::read_to_string(path.join("status")) {
            Ok(state) => { let state = state.trim().to_owned(); reading.on_battery |= state == "Discharging"; batteries.push((path, state)); },
            Err(_) => incomplete = true,
        }
    }
    reading.available = !incomplete && batteries.iter().all(|(_, state)| matches!(state.as_str(), "Discharging" | "Charging" | "Full" | "Not charging"));
    if !batteries.is_empty() && reading.available {
        reading.charging = Some(batteries.iter().any(|(_, state)| state == "Charging"));
        if batteries.len() == 1 { reading.percent = std::fs::read_to_string(batteries[0].0.join("capacity")).ok().and_then(|s| s.trim().parse::<u32>().ok()).filter(|n| *n <= 100); }
    }
    Ok(reading)
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::os::windows::ffi::OsStrExt;
    use std::ffi::c_void;
    type Handle = *mut c_void;
    fn wide(value: &OsStr) -> Result<Vec<u16>> { let mut result: Vec<_> = value.encode_wide().collect(); if result.contains(&0) { bail!("OS target contains a NUL"); } result.push(0); Ok(result) }
    #[link(name = "user32")] unsafe extern "system" { fn OpenClipboard(window: Handle) -> i32; fn CloseClipboard() -> i32; fn GetClipboardData(format: u32) -> Handle; fn IsClipboardFormatAvailable(format: u32) -> i32; }
    #[link(name = "kernel32")] unsafe extern "system" { fn GlobalLock(memory: Handle) -> Handle; fn GlobalUnlock(memory: Handle) -> i32; fn GlobalSize(memory: Handle) -> usize; fn GetSystemPowerStatus(status: *mut NativePower) -> i32; }
    #[link(name = "shell32")] unsafe extern "system" { fn ShellExecuteW(window: Handle, operation: *const u16, file: *const u16, parameters: *const u16, directory: *const u16, show: i32) -> isize; }
    #[link(name = "advapi32")] unsafe extern "system" {
        fn RegOpenKeyExW(key: Handle, subkey: *const u16, options: u32, access: u32, result: *mut Handle) -> i32;
        fn RegCreateKeyExW(key: Handle, subkey: *const u16, reserved: u32, class: *mut u16, options: u32, access: u32, attributes: Handle, result: *mut Handle, disposition: *mut u32) -> i32;
        fn RegQueryValueExW(key: Handle, value: *const u16, reserved: *mut u32, kind: *mut u32, data: *mut u8, size: *mut u32) -> i32;
        fn RegSetValueExW(key: Handle, value: *const u16, reserved: u32, kind: u32, data: *const u8, size: u32) -> i32;
        fn RegDeleteValueW(key: Handle, value: *const u16) -> i32;
        fn RegCloseKey(key: Handle) -> i32;
    }
    struct Key(Handle);
    impl Drop for Key { fn drop(&mut self) { unsafe { RegCloseKey(self.0); } } }
    const CURRENT_USER: Handle = 0x80000001u32 as i32 as isize as Handle;
    fn registry(create: bool) -> Result<Option<Key>> {
        let path = wide(OsStr::new(r"Software\Microsoft\Windows\CurrentVersion\Run"))?;
        let mut handle = std::ptr::null_mut();
        let error = unsafe { if create { RegCreateKeyExW(CURRENT_USER, path.as_ptr(), 0, std::ptr::null_mut(), 0, 3, std::ptr::null_mut(), &mut handle, std::ptr::null_mut()) } else { RegOpenKeyExW(CURRENT_USER, path.as_ptr(), 0, 1, &mut handle) } };
        if error == 2 && !create { return Ok(None); }
        if error != 0 { return Err(std::io::Error::from_raw_os_error(error).into()); }
        Ok(Some(Key(handle)))
    }
    pub(super) fn login_state() -> Result<bool> {
        let Some(key) = registry(false)? else { return Ok(false); };
        let name = wide(OsStr::new("Inklow"))?;
        let mut size = 0;
        let mut kind = 0;
        let error = unsafe { RegQueryValueExW(key.0, name.as_ptr(), std::ptr::null_mut(), &mut kind, std::ptr::null_mut(), &mut size) };
        if error == 2 { return Ok(false); }
        if error != 0 { return Err(std::io::Error::from_raw_os_error(error).into()); }
        Ok(matches!(kind, 1 | 2) && size > 2)
    }
    fn quote(argument: &OsStr) -> Result<Vec<u16>> {
        let chars: Vec<u16> = argument.encode_wide().collect();
        if chars.contains(&0) { bail!("Launch argument contains a NUL"); }
        let mut value = vec![34];
        let mut slashes = 0;
        for c in chars { if c == 92 { slashes += 1; continue; } if c == 34 { value.extend(std::iter::repeat_n(92, slashes * 2 + 1)); } else { value.extend(std::iter::repeat_n(92, slashes)); } slashes = 0; value.push(c); }
        value.extend(std::iter::repeat_n(92, slashes * 2)); value.push(34); Ok(value)
    }
    pub(super) fn set_login(enabled: bool, arguments: &[OsString]) -> Result<bool> {
        let key = registry(true)?.context("Cannot create the current user's Run key")?;
        let name = wide(OsStr::new("Inklow"))?;
        let error = if enabled {
            let mut command = Vec::new();
            for (i, argument) in arguments.iter().enumerate() { if i > 0 { command.push(32); } command.extend(quote(argument)?); }
            command.push(0);
            unsafe { RegSetValueExW(key.0, name.as_ptr(), 0, 1, command.as_ptr().cast(), (command.len() * 2) as u32) }
        } else { unsafe { RegDeleteValueW(key.0, name.as_ptr()) } };
        if error != 0 && !(error == 2 && !enabled) { return Err(std::io::Error::from_raw_os_error(error).into()); }
        Ok(true)
    }
    pub(super) fn open(target: &OsStr) -> Result<()> {
        let target = wide(target)?;
        let operation = wide(OsStr::new("open"))?;
        let result = unsafe { ShellExecuteW(std::ptr::null_mut(), operation.as_ptr(), target.as_ptr(), std::ptr::null(), std::ptr::null(), 1) };
        if result <= 32 { bail!("Windows could not open this target (code {result})"); }
        Ok(())
    }
    pub(super) fn clipboard() -> Result<String> {
        struct Clipboard;
        impl Drop for Clipboard { fn drop(&mut self) { unsafe { CloseClipboard(); } } }
        struct Memory(Handle);
        impl Drop for Memory { fn drop(&mut self) { unsafe { GlobalUnlock(self.0); } } }
        let deadline = Instant::now() + Duration::from_millis(200);
        while unsafe { OpenClipboard(std::ptr::null_mut()) } == 0 { if Instant::now() >= deadline { bail!("Clipboard is unavailable"); } thread::sleep(Duration::from_millis(5)); }
        let _clipboard = Clipboard;
        if unsafe { IsClipboardFormatAvailable(13) } == 0 { return Ok(String::new()); }
        let handle = unsafe { GetClipboardData(13) };
        if handle.is_null() { bail!("Clipboard text unavailable"); }
        let count = unsafe { GlobalSize(handle) } / 2;
        let pointer = unsafe { GlobalLock(handle) } as *const u16;
        if pointer.is_null() { bail!("Clipboard text unavailable"); }
        let _memory = Memory(handle);
        let units = unsafe { std::slice::from_raw_parts(pointer, count.min(8192)) };
        let end = units.iter().position(|c| *c == 0).unwrap_or(units.len());
        Ok(String::from_utf16_lossy(&units[..end]))
    }
    #[repr(C)] struct NativePower { ac: u8, flags: u8, percentage: u8, system: u8, remaining: u32, full: u32 }
    pub(super) fn power() -> Result<PowerStatus> {
        let mut status = NativePower { ac: 255, flags: 255, percentage: 255, system: 0, remaining: 0, full: 0 };
        if unsafe { GetSystemPowerStatus(&mut status) } == 0 { return Ok(PowerStatus::default()); }
        Ok(windows_power_fields(status.ac, status.flags, status.percentage))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn reading(percent: Option<u32>) -> PowerStatus {
        PowerStatus { available: true, has_battery: true, on_battery: true, charging: Some(false), percent }
    }
    fn pmset(source: &str, percentage: &str, state: &str) -> String {
        format!("Now drawing from '{source}'\n -InternalBattery-0 (id=1234567)\t{percentage}%; {state}; 2:15 remaining present: true\n")
    }
    #[test]
    fn power_parser_retains_unknowns_and_contradictions() {
        for (percentage, expected) in [("0", Some(0)), ("19", Some(19)), ("20", Some(20)), ("100", Some(100)), ("101", None), ("-1", None), ("unknown", None)] {
            assert_eq!(parse_mac_power(&pmset("Battery Power", percentage, "discharging")), reading(expected));
        }
        assert_eq!(parse_mac_power(""), PowerStatus::default());
        assert_eq!(parse_mac_power("unrecognized"), PowerStatus::default());
        assert_eq!(parse_mac_power("Now drawing from 'Battery Power'\n"), PowerStatus { has_battery: true, on_battery: true, ..PowerStatus::default() });
        assert_eq!(parse_mac_power("Now drawing from 'AC Power'\nNo batteries.\n"), PowerStatus { available: true, ..PowerStatus::default() });
        assert!(!parse_mac_power("Now drawing from 'AC Power'\nunsupported diagnostic\n").available);
        let contradiction = parse_mac_power(&pmset("Battery Power", "19", "charging"));
        assert!(!contradiction.available);
        assert_eq!(contradiction.charging, None);
        assert_eq!(contradiction.percent, Some(19));
        assert_eq!(parse_mac_power(&pmset("Battery Power", "19", "unknown")).charging, None);
        let absent = parse_mac_power(&pmset("AC Power", "19", "charging").replace("present: true", "present: false"));
        assert_eq!(absent, PowerStatus { available: true, ..PowerStatus::default() });
        let multiple = pmset("Battery Power", "19", "discharging") + " -InternalBattery-1 (id=2)\t20%; discharging; present: true\n";
        assert_eq!(parse_mac_power(&multiple), PowerStatus { has_battery: true, on_battery: true, ..PowerStatus::default() });
    }
    #[test]
    fn native_power_states_drive_canonical_discharge_policy() {
        let mut policy = crate::power::LowBatteryHelpPolicy::new();
        let samples = [
            pmset("Battery Power", "21", "discharging"),
            pmset("Battery Power", "20", "discharging"),
            pmset("Battery Power", "19", "discharging"),
            String::new(), pmset("Battery Power", "18", "discharging"),
            pmset("AC Power", "18", "charging"), pmset("Battery Power", "19", "discharging"),
            pmset("AC Power", "100", "charged"), pmset("Battery Power", "20", "discharging"),
        ];
        let asks: Vec<_> = samples.iter().map(|s| policy.should_ask(&parse_mac_power(s))).collect();
        assert_eq!(asks, [false, true, false, false, false, false, true, false, true]);
        let held = parse_mac_power("Now drawing from 'AC Power'\n -InternalBattery-0 (id=1234567)\t80%; AC attached; not charging present: true\n");
        assert_eq!(held.charging, Some(false));
        assert_eq!(held.percent, Some(80));
    }
    #[test]
    fn windows_unknown_battery_does_not_invent_capacity() {
        assert_eq!(windows_power_fields(0, 1, 19), reading(Some(19)));
        assert_eq!(windows_power_fields(1, 128, 255), PowerStatus { available: true, ..PowerStatus::default() });
        assert_eq!(windows_power_fields(255, 255, 255), PowerStatus::default());
        assert_eq!(windows_power_fields(1, 8, 38), PowerStatus { on_battery: false, charging: Some(true), ..reading(Some(38)) });
    }
    #[test]
    fn linux_power_reads_discharge_and_preserves_unknown_capacity() {
        let root = tempfile::tempdir().unwrap();
        let battery = root.path().join("BAT0");
        std::fs::create_dir(&battery).unwrap();
        std::fs::write(battery.join("type"), "Battery\n").unwrap();
        std::fs::write(battery.join("status"), "Discharging\n").unwrap();
        std::fs::write(battery.join("capacity"), "19\n").unwrap();
        assert_eq!(linux_power(root.path()).unwrap(), reading(Some(19)));
        std::fs::write(battery.join("capacity"), "101\n").unwrap();
        assert_eq!(linux_power(root.path()).unwrap(), reading(None));
        std::fs::write(battery.join("status"), "Unknown\n").unwrap();
        assert_eq!(linux_power(root.path()).unwrap(), PowerStatus { has_battery: true, ..PowerStatus::default() });
        std::fs::remove_file(battery.join("type")).unwrap();
        assert!(!linux_power(root.path()).unwrap().available);
    }
    #[test]
    fn clipboard_checks_live_revocation_and_truncates_scalars() {
        let grant = Cell::new(false);
        let reads = Cell::new(0);
        assert_eq!(clipboard_read(&|| grant.get(), || { reads.set(reads.get() + 1); Ok("private".into()) }).unwrap(), "");
        assert_eq!(reads.get(), 0);
        grant.set(true);
        assert_eq!(clipboard_read(&|| grant.get(), || { grant.set(false); Ok("discard".into()) }).unwrap(), "");
        grant.set(true);
        let text = clipboard_read(&|| grant.get(), || Ok("🦀".repeat(5000))).unwrap();
        assert_eq!(text.chars().count(), 4000);
        let error = clipboard_read(&|| true, || bail!("secret native clipboard content")).unwrap_err();
        assert_eq!(error.to_string(), "Clipboard context could not be read.");
    }
    fn local_clipboard_service() -> ClipboardService {
        ClipboardService { queue: Arc::new(ClipboardQueue { owner: thread::current().id(), pending: Mutex::new(None), stopped: AtomicBool::new(false) }), _owner_only: std::marker::PhantomData }
    }
    fn queue_request(service: &ClipboardService, authorized: ClipboardAuthorization, deadline: Instant) -> Arc<ClipboardRequest> {
        let request = Arc::new(ClipboardRequest { authorized, deadline, cancelled: AtomicBool::new(false), result: Mutex::new(None) });
        *service.queue.pending.lock().unwrap() = Some(request.clone());
        request
    }
    #[test]
    fn clipboard_gui_queue_never_reads_expired_cancelled_or_revoked_work() {
        let service = local_clipboard_service();
        for (allowed, cancelled, expired) in [(false, false, false), (true, true, false), (true, false, true)] {
            let request = queue_request(&service, Arc::new(move || allowed), if expired { Instant::now() - Duration::from_secs(1) } else { Instant::now() + Duration::from_secs(1) });
            request.cancelled.store(cancelled, Ordering::Release);
            service.poll_with(|| panic!("Revoked/expired GUI operation read the clipboard")).unwrap();
            assert_eq!(request.result.lock().unwrap().take().unwrap().unwrap(), "");
        }
        let grant = Arc::new(AtomicBool::new(true));
        let check = grant.clone();
        let request = queue_request(&service, Arc::new(move || check.load(Ordering::Acquire)), Instant::now() + Duration::from_secs(1));
        service.poll_with(|| { grant.store(false, Ordering::Release); Ok("private".into()) }).unwrap();
        assert_eq!(request.result.lock().unwrap().take().unwrap().unwrap(), "");
        let request = queue_request(&service, Arc::new(|| true), Instant::now() + Duration::from_secs(1));
        service.poll_with(|| Ok("valid GUI-owned result".into())).unwrap();
        assert_eq!(request.result.lock().unwrap().take().unwrap().unwrap(), "valid GUI-owned result");
    }
    #[test]
    fn desktop_registration_preserves_ownership_and_identity() {
        let root = tempfile::tempdir().unwrap();
        let entry = root.path().join("Inklow.desktop");
        let arguments = [OsString::from("/test/quoted $app%\\name"), "--background".into(), "--login-startup".into()];
        let original = "[Desktop Entry]\nExec=somebody-else\n";
        std::fs::write(&entry, original).unwrap();
        for enabled in [true, false] {
            assert!(write_login(&entry, false, enabled, &arguments).is_err());
            assert_eq!(std::fs::read_to_string(&entry).unwrap(), original);
        }
        std::fs::remove_file(&entry).unwrap();
        write_login(&entry, false, true, &arguments).unwrap();
        let content = std::fs::read_to_string(&entry).unwrap();
        assert!(content.contains("--login-startup"));
        assert!(content.contains("--background"));
        assert!(content.contains("X-Inklow-Owned=true"));
        assert!(content.contains("\\$app%%\\\\name"));
        assert!(file_login_state(&entry, false).unwrap());
        write_login(&entry, false, false, &arguments).unwrap();
        assert!(!entry.exists());
        assert!(require_normal_profile(true).is_err());
        assert!(!owned_desktop("[Other]\nX-Inklow-Owned=true\n"));
        assert!(!owned_desktop("[Desktop Entry]\n# X-Inklow-Owned=true\n"));
        assert!(desktop_command(&[OsString::from("bad\nargument")]).is_err());
    }
    #[test]
    fn mac_launchagent_has_compatible_label_and_explicit_arguments() {
        let root = tempfile::tempdir().unwrap();
        let entry = root.path().join("com.inklow.Inklow.plist");
        let arguments = [OsString::from("/isolated/BroTypist.app/Contents/MacOS/BroTypist"), "--background".into()];
        write_login(&entry, true, true, &arguments).unwrap();
        let value = plist::Value::from_file(&entry).unwrap();
        let data = value.as_dictionary().unwrap();
        assert_eq!(data["Label"].as_string(), Some("com.inklow.Inklow"));
        assert_eq!(data["RunAtLoad"].as_boolean(), Some(true));
        assert_eq!(data["KeepAlive"].as_boolean(), Some(false));
        assert_eq!(data["ProgramArguments"].as_array().unwrap().len(), 2);
        assert!(file_login_state(&entry, true).unwrap());
    }
    #[test]
    fn wayland_install_never_overwrites_unowned_or_selects_kwin() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("applications/com.inklow.Inklow.InputMethod.desktop");
        write_wayland_entry(&path, "\"/isolated/BroTypist\" \"--wayland-input-method\"").unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(owned_desktop(&content));
        assert!(content.contains("NoDisplay=true"));
        assert!(content.contains("X-KDE-Wayland-VirtualKeyboard=true"));
        assert!(!root.path().join("kwinrc").exists());
        std::fs::write(&path, "[Desktop Entry]\nExec=unowned\n").unwrap();
        assert!(write_wayland_entry(&path, "anything").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[Desktop Entry]\nExec=unowned\n");
        #[cfg(unix)] {
            std::fs::remove_file(&path).unwrap();
            let target = root.path().join("unowned");
            std::fs::write(&target, "[Desktop Entry]\nX-Inklow-Owned=true\n").unwrap();
            std::os::unix::fs::symlink(&target, &path).unwrap();
            assert!(write_wayland_entry(&path, "anything").is_err());
        }
    }
    struct WindowFixture { result: ContextRead, reads: usize, checks: usize, stale_after: usize, revoke: Option<Arc<AtomicBool>> }
    impl WindowContext for WindowFixture {
        fn context_matches(&mut self, _snapshot: &Snapshot) -> Result<bool> { self.checks += 1; Ok(self.checks <= self.stale_after) }
        fn focused_context(&mut self, _snapshot: &Snapshot, authorized: &dyn Fn() -> bool, _limit: usize) -> Result<ContextRead> {
            assert!(authorized());
            self.reads += 1;
            if let Some(grant) = &self.revoke { grant.store(false, Ordering::Release); }
            Ok(ContextRead { text: self.result.text.clone(), status: self.result.status.clone(), source: self.result.source.clone() })
        }
    }
    fn window() -> Snapshot { Snapshot { app_id: "writer".into(), window_id: "writer:window".into(), window_revision: "document:revision".into(), ..Snapshot::default() } }
    fn fixture(status: &str, text: &str) -> WindowFixture { WindowFixture { result: ContextRead { text: text.into(), status: status.into(), source: "fixture".into() }, reads: 0, checks: 0, stale_after: usize::MAX, revoke: None } }
    #[test]
    fn blocked_ax_is_not_an_ocr_fallback() {
        let mut bridge = fixture("blocked", "");
        let result = screen_context_with(&window(), &mut bridge, &|| true, 2400, true, true, &mut |_, _, _, _| panic!("Blocked AX must not call OCR")).unwrap();
        assert_eq!(result.status, "blocked");
        assert_eq!(bridge.reads, 1);
        assert_eq!(bridge.checks, 1);
    }
    #[test]
    fn ax_first_then_only_explicit_empty_ocr_fallback() {
        let mut bridge = fixture("ready", "Agenda 🦀");
        let result = screen_context_with(&window(), &mut bridge, &|| true, 8, true, true, &mut |_, _, _, _| panic!("Useful AX must not call OCR")).unwrap();
        assert_eq!(result.text, "Agenda 🦀");
        assert_eq!(result.source, "accessibility");
        let mut bridge = fixture("empty", "");
        let result = screen_context_with(&window(), &mut bridge, &|| true, 4, true, true, &mut |_, authorized, current, limit| {
            assert!(authorized());
            assert!(current()?);
            assert_eq!(limit, 4);
            Ok(ContextRead { text: "Invoice".into(), status: "ready".into(), source: "screenshot".into() })
        }).unwrap();
        assert_eq!(result.text, "Invo");
        assert_eq!(result.source, "screenshot");
        let mut bridge = fixture("empty", "");
        assert_eq!(screen_context_with(&window(), &mut bridge, &|| true, 2400, true, false, &mut |_, _, _, _| panic!("OCR not separately enabled")).unwrap().status, "empty");
    }
    #[test]
    fn context_revocation_and_window_change_discard_native_text() {
        let grant = Arc::new(AtomicBool::new(true));
        let mut bridge = fixture("ready", "private");
        bridge.revoke = Some(grant.clone());
        let result = screen_context_with(&window(), &mut bridge, &|| grant.load(Ordering::Acquire), 2400, true, true, &mut |_, _, _, _| panic!("Revoked AX cannot call OCR")).unwrap();
        assert_eq!(result.status, "blocked");
        assert!(result.text.is_empty());
        let mut bridge = fixture("ready", "wrong window");
        bridge.stale_after = 1;
        let result = screen_context_with(&window(), &mut bridge, &|| true, 2400, true, true, &mut |_, _, _, _| panic!("Stale AX cannot call OCR")).unwrap();
        assert_eq!(result.status, "stale");
        assert!(result.text.is_empty());
    }
    #[test]
    fn terminal_protected_unverified_or_unconsented_never_read() {
        for snapshot in [Snapshot { protected: true, ..window() }, Snapshot::default(), Snapshot { app_id: "com.apple.Terminal".into(), field_id: "terminal:unsent-input".into(), ..window() }] {
            let mut bridge = fixture("ready", "private");
            let result = screen_context_with(&snapshot, &mut bridge, &|| true, 2400, true, true, &mut |_, _, _, _| panic!("Private field triggered OCR")).unwrap();
            assert_eq!(result.status, "blocked");
            assert_eq!(bridge.reads, 0);
            assert_eq!(bridge.checks, 0);
        }
        let mut bridge = fixture("ready", "private");
        assert_eq!(screen_context_with(&window(), &mut bridge, &|| false, 2400, true, true, &mut |_, _, _, _| panic!("No consent")).unwrap().status, "blocked");
        assert_eq!(bridge.reads, 0);
    }
    #[test]
    fn clipboard_worker_timeout_never_executes_late_and_queue_remains_usable() {
        let service = local_clipboard_service();
        let queue = service.queue.clone();
        let worker = thread::spawn(move || clipboard_from_queue(queue, Arc::new(|| true), Duration::from_millis(10)));
        assert_eq!(worker.join().unwrap().unwrap(), "");
        service.poll_with(|| panic!("Timed-out worker read late")).unwrap();
        let queue = service.queue.clone();
        let grant = Arc::new(AtomicBool::new(true));
        let check = grant.clone();
        let worker = thread::spawn(move || clipboard_from_queue(queue, Arc::new(move || check.load(Ordering::Acquire)), Duration::from_secs(1)));
        let deadline = Instant::now() + Duration::from_secs(1);
        while service.queue.pending.lock().unwrap().is_none() {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        grant.store(false, Ordering::Release);
        service.poll_with(|| panic!("Queued revocation read the clipboard")).unwrap();
        assert_eq!(worker.join().unwrap().unwrap(), "");
    }
    #[test]
    fn detached_ticket_is_bound_before_any_native_read() {
        struct Ticket;
        impl crate::platform::DetachedContextRead for Ticket {
            fn read(&self, _: &dyn Fn() -> bool, _: usize) -> Result<ContextRead> { panic!("Unrelated snapshot read ticket"); }
            fn matches(&self) -> Result<bool> { panic!("Unrelated snapshot queried native identity"); }
            fn matches_snapshot(&self, _: &Snapshot) -> bool { false }
        }
        let result = screen_context_detached(&window(), &Ticket, &|| true, 2400, true, true).unwrap();
        assert_eq!(result.status, "stale");
        assert!(result.text.is_empty());
    }
    #[test]
    fn isolated_profile_child() {
        if std::env::var_os("BROTYPist_SYSTEM_TEST_CHILD").is_none() { return; }
        assert!(!launch_at_login_state().unwrap());
        assert!(set_launch_at_login(true).is_err());
        assert!(set_launch_at_login(false).is_err());
        assert!(!wayland_input_method_selected());
        assert!(install_wayland_input_method().is_err());
        let root = PathBuf::from(std::env::var_os("INKLOW_HOME").unwrap());
        assert_eq!(std::fs::read_dir(root).unwrap().count(), 0);
    }
    #[test]
    fn isolated_profile_never_registers_or_inspects_user_desktop() {
        let root = tempfile::tempdir().unwrap();
        let nonexistent = root.path().join("must-not-be-created");
        let result = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "system::tests::isolated_profile_child", "--nocapture"])
            .env("BROTYPist_SYSTEM_TEST_CHILD", "1")
            .env("INKLOW_HOME", root.path())
            .env("HOME", &nonexistent)
            .env("USERPROFILE", &nonexistent)
            .env("XDG_CONFIG_HOME", &nonexistent)
            .env("XDG_DATA_HOME", &nonexistent)
            .env("XDG_SESSION_TYPE", "wayland")
            .status().unwrap();
        assert!(result.success());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
}
